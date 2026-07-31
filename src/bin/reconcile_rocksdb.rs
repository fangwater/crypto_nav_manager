use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, TimeZone, Utc};
use clap::Parser;
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SUPPORTED: [&str; 5] = [
    "binance_mm_alpha",
    "binance-intra-arb01",
    "bybit_mm_alpha",
    "bybit-intra-arb01",
    "bybit-intra-arb02",
];

#[derive(Debug, Parser)]
#[command(about = "Reconcile PostgreSQL trades with persisted RocksDB fills")]
struct Args {
    /// Strategy slug. May be repeated. Defaults to all supported strategies.
    #[arg(long)]
    strategy: Vec<String>,

    /// Overrides CRYPTO_NAV_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,

    #[arg(long, default_value_t = 10)]
    settlement_gap_minutes: i64,

    #[arg(long, default_value_t = 5)]
    overlap_minutes: i64,

    #[arg(long, default_value_t = 60)]
    baseline_minutes: i64,

    #[arg(long)]
    end_ms: Option<i64>,

    #[arg(long, default_value_t = 1e-8)]
    qty_epsilon: f64,

    #[arg(long)]
    skip_sync: bool,

    #[arg(long, default_value = "sg")]
    ssh_host: String,

    #[arg(long, default_value = "/home/ubuntu")]
    base_dir: PathBuf,

    #[arg(long, default_value = "/home/ubuntu/mkt_signal")]
    mkt_signal_root: PathBuf,

    #[arg(long, default_value = "http://127.0.0.1:8822")]
    persist_read_url: String,

    #[arg(long)]
    work_dir: Option<PathBuf>,

    #[arg(long)]
    keep_remote: bool,

    #[arg(long)]
    cleanup_on_success: bool,
}

#[derive(Debug)]
struct Checkpoint {
    aligned_from_ms: i64,
    verified_through_ms: i64,
    pg_success_end_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Market {
    Spot,
    Swap,
}

impl Market {
    fn from_sid(value: &str) -> Result<Self> {
        match value {
            "1" => Ok(Self::Spot),
            "0" => Ok(Self::Swap),
            _ => bail!("unsupported PostgreSQL sid {value:?}"),
        }
    }

    fn from_venue(value: &str) -> Result<Self> {
        match value {
            "BinanceMargin" | "BybitMargin" => Ok(Self::Spot),
            "BinanceFutures" | "BybitFutures" => Ok(Self::Swap),
            _ => bail!("unsupported RocksDB trading_venue {value:?}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Swap => "swap",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GroupKey {
    market: Market,
    symbol: String,
    side: String,
}

#[derive(Clone, Debug, Default)]
struct Stat {
    qty: f64,
    events: usize,
    orders: BTreeSet<String>,
}

#[derive(Debug)]
struct Observation {
    timestamp_us: i64,
    cumulative_qty: f64,
}

#[derive(Debug)]
struct UnmatchedSeries {
    group: GroupKey,
    client_ids: BTreeSet<i64>,
    observations: Vec<Observation>,
}

#[derive(Debug)]
struct UnmatchedOrder {
    group: GroupKey,
    client_ids: BTreeSet<i64>,
    qty: f64,
    events: usize,
}

#[derive(Debug, Serialize)]
struct GroupReport {
    market: &'static str,
    symbol: String,
    side: String,
    pg_events: usize,
    pg_orders: usize,
    pg_qty: f64,
    uniform_events: usize,
    uniform_orders: usize,
    uniform_qty: f64,
    unmatched_events: usize,
    unmatched_orders: usize,
    unmatched_qty: f64,
    local_qty: f64,
    qty_diff: f64,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct Summary {
    strategy: String,
    aligned: bool,
    checkpoint_advanced: bool,
    sync_error: Option<String>,
    aligned_from_ms: i64,
    previous_verified_through_ms: i64,
    scan_start_ms: i64,
    candidate_end_ms: i64,
    pg_success_end_ms: i64,
    actual_end_ms: i64,
    settlement_gap_minutes: i64,
    overlap_minutes: i64,
    baseline_minutes: i64,
    group_count: usize,
    mismatched_group_count: usize,
    pg_event_count: usize,
    uniform_event_count: usize,
    unmatched_represented_order_count: usize,
    unmatched_only_order_count: usize,
    pg_qty: f64,
    local_qty: f64,
}

#[derive(Debug, Deserialize)]
struct PgTradeRow {
    sid: String,
    symbol: String,
    id: String,
    #[serde(rename = "orderId")]
    order_id: String,
    side: String,
    qty: f64,
    ts: i64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    validate_args(&args)?;
    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;

    let strategies = if args.strategy.is_empty() {
        SUPPORTED.iter().map(|value| (*value).to_string()).collect()
    } else {
        args.strategy.clone()
    };
    for strategy in &strategies {
        if !SUPPORTED.contains(&strategy.as_str()) {
            bail!("unsupported strategy {strategy:?}");
        }
    }

    let now_ms = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_millis(),
    )
    .context("current timestamp exceeds i64")?;
    let gap_ms = args
        .settlement_gap_minutes
        .checked_mul(60_000)
        .context("settlement gap overflow")?;
    let latest_end_ms = now_ms - gap_ms;
    let candidate_end_ms = args.end_ms.unwrap_or(latest_end_ms).min(latest_end_ms);
    let report_root = prepare_report_root(&args)?;
    println!("report_root={}", report_root.display());
    println!("candidate_end_ms={candidate_end_ms}");

    let mut summaries = Vec::new();
    let mut failed = false;
    for strategy in strategies {
        let run_id = format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
            std::process::id(),
            strategy
        );
        match reconcile(
            &pool,
            &args,
            &strategy,
            candidate_end_ms,
            &report_root,
            &run_id,
        )
        .await
        {
            Ok(summary) => {
                failed |= !summary.aligned || summary.sync_error.is_some();
                summaries.push(serde_json::to_value(summary)?);
            }
            Err(error) => {
                failed = true;
                let message = format!("{error:#}");
                if let Err(status_error) = fail_status(&pool, &strategy, &message).await {
                    eprintln!("{strategy}: status update failed: {status_error:#}");
                }
                let summary = serde_json::json!({
                    "strategy": strategy,
                    "aligned": false,
                    "error": message,
                });
                let strategy_root = report_root.join(&strategy);
                fs::create_dir_all(&strategy_root)?;
                write_json(&strategy_root.join("summary.json"), &summary)?;
                eprintln!("{strategy}: ERROR: {error:#}");
                summaries.push(summary);
            }
        }
    }
    write_json(&report_root.join("summary.json"), &summaries)?;
    println!("summary={}", report_root.join("summary.json").display());
    if args.cleanup_on_success && !failed {
        ensure_generated_report_path(&report_root)?;
        fs::remove_dir_all(&report_root)
            .with_context(|| format!("remove successful report {}", report_root.display()))?;
        println!("removed_success_report={}", report_root.display());
    }
    pool.close().await;
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn validate_args(args: &Args) -> Result<()> {
    for (name, value) in [
        ("settlement-gap-minutes", args.settlement_gap_minutes),
        ("overlap-minutes", args.overlap_minutes),
        ("baseline-minutes", args.baseline_minutes),
    ] {
        if value < 0 {
            bail!("--{name} must be non-negative");
        }
    }
    if !args.qty_epsilon.is_finite() || args.qty_epsilon < 0.0 {
        bail!("--qty-epsilon must be finite and non-negative");
    }
    Ok(())
}

async fn connect_postgres(database_url: Option<&str>) -> Result<PgPool> {
    let options = match database_url
        .map(str::to_string)
        .or_else(|| env::var("CRYPTO_NAV_DATABASE_URL").ok())
    {
        Some(url) => url
            .parse::<PgConnectOptions>()
            .context("parse database URL")?,
        None => PgConnectOptions::new()
            .host("/var/run/postgresql")
            .database("crypto_nav_manager")
            .username("ubuntu"),
    };
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .context("connect PostgreSQL")
}

fn prepare_report_root(args: &Args) -> Result<PathBuf> {
    if let Some(path) = &args.work_dir {
        fs::create_dir_all(path)
            .with_context(|| format!("create report root {}", path.display()))?;
        return path.canonicalize().context("resolve report root");
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = env::temp_dir()
        .join("crypto_nav_rocksdb_reconcile")
        .join(format!(
            "{stamp}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
    fs::create_dir_all(&path).with_context(|| format!("create report root {}", path.display()))?;
    Ok(path)
}

fn ensure_generated_report_path(path: &Path) -> Result<()> {
    let parent = env::temp_dir().join("crypto_nav_rocksdb_reconcile");
    if !path.starts_with(&parent) || path == parent {
        bail!(
            "refuse to remove non-generated report path {}",
            path.display()
        );
    }
    Ok(())
}

async fn reconcile(
    pool: &PgPool,
    args: &Args,
    strategy: &str,
    candidate_end_ms: i64,
    report_root: &Path,
    run_id: &str,
) -> Result<Summary> {
    start_status(pool, strategy, run_id, candidate_end_ms).await?;
    let checkpoint = load_checkpoint(pool, strategy).await?;
    let overlap_ms = args
        .overlap_minutes
        .checked_mul(60_000)
        .context("overlap overflow")?;
    let scan_start_ms = checkpoint
        .aligned_from_ms
        .max(((checkpoint.verified_through_ms - overlap_ms) / 60_000) * 60_000);
    set_phase(
        pool,
        strategy,
        "loading_watermark",
        10,
        Some(scan_start_ms),
        None,
        None,
    )
    .await?;

    let sync_error = if args.skip_sync {
        None
    } else {
        set_phase(pool, strategy, "syncing_trades", 15, None, None, None).await?;
        sync_trades(args, strategy, scan_start_ms, candidate_end_ms)
            .err()
            .map(|error| format!("{error:#}"))
    };

    let refreshed = load_checkpoint(pool, strategy).await?;
    let actual_end_ms = candidate_end_ms.min(refreshed.pg_success_end_ms);
    if actual_end_ms < scan_start_ms {
        bail!(
            "PostgreSQL watermark {} is before scan start {}",
            refreshed.pg_success_end_ms,
            scan_start_ms
        );
    }

    let strategy_root = report_root.join(strategy);
    fs::create_dir_all(&strategy_root)?;
    set_phase(
        pool,
        strategy,
        "exporting_pg",
        30,
        None,
        Some(refreshed.pg_success_end_ms),
        Some(actual_end_ms),
    )
    .await?;
    let pg_dir = export_pg(args, strategy, scan_start_ms, actual_end_ms, &strategy_root)?;
    let baseline_ms = args
        .baseline_minutes
        .checked_mul(60_000)
        .context("baseline overflow")?;
    let export_start_ms = checkpoint
        .aligned_from_ms
        .max(scan_start_ms.saturating_sub(baseline_ms));
    set_phase(pool, strategy, "exporting_orders", 50, None, None, None).await?;
    let order_dir = export_orders(
        args,
        strategy,
        export_start_ms,
        actual_end_ms,
        &strategy_root.join("orders"),
        run_id,
    )?;

    set_phase(pool, strategy, "comparing", 85, None, None, None).await?;
    let selected_symbols = selected_symbols(strategy);
    let pg = pg_groups(
        &pg_dir,
        scan_start_ms,
        actual_end_ms,
        selected_symbols.as_ref(),
    )?;
    let start_us = scan_start_ms
        .checked_mul(1_000)
        .context("start microseconds overflow")?;
    let end_us = actual_end_ms
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(999))
        .context("end microseconds overflow")?;
    let (uniform, client_ids) = uniform_groups(
        &order_dir.join("uniform_orders.parquet"),
        start_us,
        end_us,
        args.qty_epsilon,
        selected_symbols.as_ref(),
    )?;
    let (unmatched, represented, unmatched_only) = unmatched_groups(
        &order_dir.join("trade_updates_unmatched.parquet"),
        start_us,
        end_us,
        args.qty_epsilon,
        &client_ids,
        selected_symbols.as_ref(),
    )?;
    let groups = compare_groups(&pg, &uniform, &unmatched, args.qty_epsilon);
    let mismatch_count = groups.iter().filter(|row| row.status == "MISMATCH").count();
    let aligned = mismatch_count == 0;
    let advanced = aligned && actual_end_ms > checkpoint.verified_through_ms;
    if advanced {
        sqlx::query(
            "UPDATE rocksdb_alignment_checkpoints SET \
             verified_through_ms=GREATEST(verified_through_ms,$2), \
             verified_at=CURRENT_TIMESTAMP WHERE strategy_slug=$1",
        )
        .bind(strategy)
        .bind(actual_end_ms)
        .execute(pool)
        .await
        .context("advance RocksDB alignment checkpoint")?;
    }
    write_groups(&strategy_root.join("groups.csv"), &groups)?;

    let summary = Summary {
        strategy: strategy.to_string(),
        aligned,
        checkpoint_advanced: advanced,
        sync_error,
        aligned_from_ms: checkpoint.aligned_from_ms,
        previous_verified_through_ms: checkpoint.verified_through_ms,
        scan_start_ms,
        candidate_end_ms,
        pg_success_end_ms: refreshed.pg_success_end_ms,
        actual_end_ms,
        settlement_gap_minutes: args.settlement_gap_minutes,
        overlap_minutes: args.overlap_minutes,
        baseline_minutes: args.baseline_minutes,
        group_count: groups.len(),
        mismatched_group_count: mismatch_count,
        pg_event_count: pg.values().map(|value| value.events).sum(),
        uniform_event_count: uniform.values().map(|value| value.events).sum(),
        unmatched_represented_order_count: represented,
        unmatched_only_order_count: unmatched_only,
        pg_qty: pg.values().map(|value| value.qty).sum(),
        local_qty: uniform.values().map(|value| value.qty).sum::<f64>()
            + unmatched.values().map(|value| value.qty).sum::<f64>(),
    };
    complete_status(pool, &summary).await?;
    write_json(&strategy_root.join("summary.json"), &summary)?;
    println!(
        "{strategy}: aligned={} groups={} mismatches={} end={} advanced={}",
        aligned,
        groups.len(),
        mismatch_count,
        actual_end_ms,
        advanced
    );
    Ok(summary)
}

async fn start_status(
    pool: &PgPool,
    strategy: &str,
    run_id: &str,
    candidate_end_ms: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO rocksdb_alignment_status \
         (strategy_slug,state,phase,progress_percent,run_id,started_at,updated_at,completed_at,\
          candidate_end_ms,scan_start_ms,pg_success_end_ms,actual_end_ms,group_count,\
          mismatch_count,pg_event_count,local_event_count,message) \
         VALUES ($1,'running','preparing',5,$2,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP,NULL,\
                 $3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL) \
         ON CONFLICT (strategy_slug) DO UPDATE SET state='running',phase='preparing',\
         progress_percent=5,run_id=$2,started_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP,\
         completed_at=NULL,candidate_end_ms=$3,scan_start_ms=NULL,pg_success_end_ms=NULL,\
         actual_end_ms=NULL,group_count=NULL,mismatch_count=NULL,pg_event_count=NULL,\
         local_event_count=NULL,message=NULL",
    )
    .bind(strategy)
    .bind(run_id)
    .bind(candidate_end_ms)
    .execute(pool)
    .await
    .context("start RocksDB alignment status")?;
    Ok(())
}

async fn set_phase(
    pool: &PgPool,
    strategy: &str,
    phase: &str,
    progress: i32,
    scan_start_ms: Option<i64>,
    pg_success_end_ms: Option<i64>,
    actual_end_ms: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "UPDATE rocksdb_alignment_status SET state='running',phase=$2,progress_percent=$3,\
         updated_at=CURRENT_TIMESTAMP,scan_start_ms=COALESCE($4,scan_start_ms),\
         pg_success_end_ms=COALESCE($5,pg_success_end_ms),\
         actual_end_ms=COALESCE($6,actual_end_ms) WHERE strategy_slug=$1",
    )
    .bind(strategy)
    .bind(phase)
    .bind(progress)
    .bind(scan_start_ms)
    .bind(pg_success_end_ms)
    .bind(actual_end_ms)
    .execute(pool)
    .await
    .with_context(|| format!("update alignment phase for {strategy}"))?;
    Ok(())
}

async fn complete_status(pool: &PgPool, summary: &Summary) -> Result<()> {
    let state = if summary.aligned {
        "succeeded"
    } else {
        "mismatch"
    };
    let message = if summary.aligned {
        "全部分组匹配".to_string()
    } else {
        format!("{} 个分组存在差异", summary.mismatched_group_count)
    };
    sqlx::query(
        "UPDATE rocksdb_alignment_status SET state=$2,phase='complete',progress_percent=100,\
         updated_at=CURRENT_TIMESTAMP,completed_at=CURRENT_TIMESTAMP,group_count=$3,\
         mismatch_count=$4,pg_event_count=$5,local_event_count=$6,message=$7 \
         WHERE strategy_slug=$1",
    )
    .bind(&summary.strategy)
    .bind(state)
    .bind(i32::try_from(summary.group_count).context("group count exceeds i32")?)
    .bind(i32::try_from(summary.mismatched_group_count).context("mismatch count exceeds i32")?)
    .bind(i64::try_from(summary.pg_event_count).context("PG event count exceeds i64")?)
    .bind(i64::try_from(summary.uniform_event_count).context("local event count exceeds i64")?)
    .bind(message)
    .execute(pool)
    .await
    .context("complete alignment status")?;
    Ok(())
}

async fn fail_status(pool: &PgPool, strategy: &str, message: &str) -> Result<()> {
    let message = message.chars().take(1_000).collect::<String>();
    sqlx::query(
        "UPDATE rocksdb_alignment_status SET state='failed',phase='complete',\
         progress_percent=100,updated_at=CURRENT_TIMESTAMP,completed_at=CURRENT_TIMESTAMP,\
         message=$2 WHERE strategy_slug=$1",
    )
    .bind(strategy)
    .bind(message)
    .execute(pool)
    .await
    .context("mark alignment failed")?;
    Ok(())
}

async fn load_checkpoint(pool: &PgPool, strategy: &str) -> Result<Checkpoint> {
    let row = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT c.aligned_from_ms,c.verified_through_ms,w.success_end_ms \
         FROM rocksdb_alignment_checkpoints c JOIN history_sync_watermarks w \
         ON w.strategy_slug=c.strategy_slug AND w.dataset='trades' \
         WHERE c.strategy_slug=$1",
    )
    .bind(strategy)
    .fetch_optional(pool)
    .await
    .context("load alignment checkpoint")?
    .with_context(|| format!("missing checkpoint or PostgreSQL trades watermark: {strategy}"))?;
    Ok(Checkpoint {
        aligned_from_ms: row.0,
        verified_through_ms: row.1,
        pg_success_end_ms: row.2,
    })
}

fn sibling_binary(name: &str) -> Result<PathBuf> {
    Ok(env::current_exe()
        .context("resolve current executable")?
        .with_file_name(name))
}

fn sync_trades(args: &Args, strategy: &str, start_ms: i64, end_ms: i64) -> Result<()> {
    let mut command = Command::new(sibling_binary("sync_history")?);
    command
        .args(["--strategy", strategy, "--dataset", "trades", "--start-ms"])
        .arg(start_ms.to_string())
        .args(["--end-ms"])
        .arg(end_ms.to_string());
    if let Some(url) = &args.database_url {
        command.args(["--database-url", url]);
    }
    run(&mut command).map(|_| ())
}

fn export_pg(
    args: &Args,
    strategy: &str,
    start_ms: i64,
    end_ms: i64,
    strategy_root: &Path,
) -> Result<PathBuf> {
    let output_root = strategy_root.join("pg");
    let mut command = Command::new(sibling_binary("export_history")?);
    command
        .args(["--strategy", strategy, "--dataset", "trades", "--start-ms"])
        .arg(start_ms.to_string())
        .args(["--end-ms"])
        .arg(end_ms.to_string())
        .arg("--output-dir")
        .arg(&output_root);
    if let Some(url) = &args.database_url {
        command.args(["--database-url", url]);
    }
    run(&mut command)?;
    Ok(output_root.join(strategy))
}

fn export_center_orders(
    args: &Args,
    strategy: &str,
    start_ms: i64,
    end_ms: i64,
    output_root: &Path,
) -> Result<PathBuf> {
    let start_us = start_ms
        .checked_mul(1_000)
        .context("center start timestamp overflow")?;
    let end_us = end_ms
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(999))
        .context("center end timestamp overflow")?;
    let endpoint = format!("{}/v1/read", args.persist_read_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("build persist read client")?;
    fs::create_dir_all(output_root)
        .with_context(|| format!("create center export {}", output_root.display()))?;

    for table in [
        "uniform_orders",
        "order_updates_unmatched",
        "trade_updates_unmatched",
    ] {
        let mut frames = Vec::new();
        let mut cursor = start_us;
        while cursor < end_us {
            let window_end = cursor.saturating_add(3_600_000_000).min(end_us);
            let response = client
                .get(&endpoint)
                .query(&[
                    ("table", table.to_string()),
                    ("source_id", strategy.to_string()),
                    ("start_us", cursor.to_string()),
                    ("end_us", window_end.to_string()),
                    ("format", "parquet".to_string()),
                ])
                .send()
                .with_context(|| format!("read center {strategy}/{table} {cursor}..{window_end}"))?
                .error_for_status()
                .with_context(|| {
                    format!("center rejected {strategy}/{table} {cursor}..{window_end}")
                })?;
            let body = response.bytes().context("read center parquet body")?;
            let frame = ParquetReader::new(Cursor::new(body))
                .finish()
                .with_context(|| format!("decode center parquet for {strategy}/{table}"))?;
            frames.push(frame);
            cursor = window_end;
        }
        let mut frames = frames.into_iter();
        let mut frame = frames
            .next()
            .with_context(|| format!("empty center window for {strategy}/{table}"))?;
        for chunk in frames {
            frame
                .vstack_mut(&chunk)
                .with_context(|| format!("merge center parquet for {strategy}/{table}"))?;
        }
        let path = output_root.join(format!("{table}.parquet"));
        let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
        ParquetWriter::new(file)
            .finish(&mut frame)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(output_root.to_path_buf())
}

fn export_orders(
    args: &Args,
    strategy: &str,
    start_ms: i64,
    end_ms: i64,
    output_root: &Path,
    run_id: &str,
) -> Result<PathBuf> {
    if is_center(strategy) {
        return export_center_orders(args, strategy, start_ms, end_ms, output_root);
    }
    let exporter = args.mkt_signal_root.join("target/release/order_export");
    let start = rfc3339_us(
        start_ms
            .checked_mul(1_000)
            .context("start timestamp overflow")?,
    )?;
    let end = rfc3339_us(
        end_ms
            .checked_mul(1_000)
            .and_then(|value| value.checked_add(999))
            .context("end timestamp overflow")?,
    )?;
    if !is_remote(strategy) {
        let mut command = Command::new(&exporter);
        command
            .arg("--base-dir")
            .arg(&args.base_dir)
            .args(["--env-name", strategy, "--start", &start, "--end", &end])
            .arg("--output-root")
            .arg(output_root);
        run(&mut command)?;
        return locate_order_export(output_root);
    }

    let token = run_id.replace(|character: char| !character.is_ascii_alphanumeric(), "");
    let remote_root = format!("/tmp/crypto_nav_rocksdb_reconcile/{token}");
    let remote_binary = format!("{remote_root}/order_export");
    let remote_output = format!("{remote_root}/output");
    let result = (|| {
        let mut mkdir = Command::new("ssh");
        mkdir.args([&args.ssh_host, "mkdir", "-p", &remote_output]);
        run(&mut mkdir)?;

        let mut copy_binary = Command::new("scp");
        copy_binary
            .arg(&exporter)
            .arg(format!("{}:{remote_binary}", args.ssh_host));
        run(&mut copy_binary)?;

        let mut chmod = Command::new("ssh");
        chmod.args([&args.ssh_host, "chmod", "700", &remote_binary]);
        run(&mut chmod)?;

        let mut export = Command::new("ssh");
        export
            .args([&args.ssh_host, &remote_binary, "--base-dir"])
            .arg(&args.base_dir)
            .args(["--env-name", strategy, "--start", &start, "--end", &end])
            .args(["--output-root", &remote_output]);
        run(&mut export)?;

        fs::create_dir_all(output_root)?;
        let mut copy_output = Command::new("scp");
        copy_output
            .arg("-r")
            .arg(format!("{}:{remote_output}/.", args.ssh_host))
            .arg(output_root);
        run(&mut copy_output)?;
        locate_order_export(output_root)
    })();

    if !args.keep_remote {
        let mut cleanup = Command::new("ssh");
        cleanup.args([&args.ssh_host, "rm", "-rf", "--", &remote_root]);
        if let Err(error) = run(&mut cleanup) {
            eprintln!("remote cleanup failed for {remote_root}: {error:#}");
        }
    }
    result
}

fn is_center(strategy: &str) -> bool {
    strategy == "bybit_mm_alpha"
}

fn is_remote(strategy: &str) -> bool {
    matches!(strategy, "bybit-intra-arb01" | "bybit-intra-arb02")
}

fn rfc3339_us(timestamp_us: i64) -> Result<String> {
    let instant = Utc
        .timestamp_micros(timestamp_us)
        .single()
        .with_context(|| format!("invalid timestamp in microseconds: {timestamp_us}"))?;
    Ok(instant.to_rfc3339_opts(SecondsFormat::Micros, true))
}

fn locate_order_export(root: &Path) -> Result<PathBuf> {
    fn visit(path: &Path, matches: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
            let entry = entry?;
            let child = entry.path();
            if child.is_dir() {
                visit(&child, matches)?;
            } else if child.file_name().and_then(|name| name.to_str())
                == Some("uniform_orders.parquet")
            {
                matches.push(child);
            }
        }
        Ok(())
    }
    let mut matches = Vec::new();
    visit(root, &mut matches)?;
    if matches.len() != 1 {
        bail!(
            "expected one uniform_orders.parquet below {}, found {}",
            root.display(),
            matches.len()
        );
    }
    let directory = matches.remove(0).parent().unwrap().to_path_buf();
    if !directory.join("trade_updates_unmatched.parquet").is_file() {
        bail!(
            "missing trade_updates_unmatched.parquet below {}",
            directory.display()
        );
    }
    Ok(directory)
}

fn run(command: &mut Command) -> Result<String> {
    println!("+ {}", command.get_program().to_string_lossy());
    let output = command
        .output()
        .with_context(|| format!("run {:?}", command))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        print!("{stdout}");
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{:?} exited with {}: {}",
            command,
            output.status,
            stderr.trim()
        );
    }
    Ok(stdout.trim().to_string())
}

fn selected_symbols(strategy: &str) -> Option<BTreeSet<String>> {
    (strategy == "binance_mm_alpha").then(|| {
        [
            "BNBUSDT", "BTCUSDT", "DOGEUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    })
}

fn normalize_symbol(value: &str) -> Result<String> {
    let symbol = value.trim().to_ascii_uppercase().replace('-', "");
    if symbol.is_empty() {
        bail!("empty symbol");
    }
    Ok(symbol)
}

fn normalize_side(value: &str) -> Result<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "buy" => Ok("buy".to_string()),
        "sell" => Ok("sell".to_string()),
        _ => bail!("unsupported side {value:?}"),
    }
}

fn group_key(market: Market, symbol: &str, side: &str) -> Result<GroupKey> {
    Ok(GroupKey {
        market,
        symbol: normalize_symbol(symbol)?,
        side: normalize_side(side)?,
    })
}

fn add_stat(groups: &mut BTreeMap<GroupKey, Stat>, key: GroupKey, qty: f64, order_id: String) {
    let stat = groups.entry(key).or_default();
    stat.qty += qty;
    stat.events += 1;
    stat.orders.insert(order_id);
}

fn pg_groups(
    directory: &Path,
    start_ms: i64,
    end_ms: i64,
    symbols: Option<&BTreeSet<String>>,
) -> Result<BTreeMap<GroupKey, Stat>> {
    let mut groups = BTreeMap::new();
    let mut seen = BTreeMap::<(Market, String, String), (GroupKey, String, u64)>::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read PostgreSQL export {}", directory.display()))?
    {
        let path = entry?.path();
        let is_trade_csv = path.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("csv")
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.starts_with("trades_"));
        if !is_trade_csv {
            continue;
        }
        let mut reader =
            csv::Reader::from_path(&path).with_context(|| format!("open {}", path.display()))?;
        for row in reader.deserialize::<PgTradeRow>() {
            let row = row.with_context(|| format!("read {}", path.display()))?;
            if row.ts < start_ms || row.ts > end_ms || row.qty <= 0.0 {
                continue;
            }
            if !row.qty.is_finite() {
                bail!("non-finite PostgreSQL quantity in {}", path.display());
            }
            let market = Market::from_sid(row.sid.trim())?;
            let key = group_key(market, &row.symbol, &row.side)?;
            if symbols.is_some_and(|selected| !selected.contains(&key.symbol)) {
                continue;
            }
            let trade_key = (market, key.symbol.clone(), row.id.clone());
            let fingerprint = (key.clone(), row.order_id.clone(), row.qty.to_bits());
            if let Some(previous) = seen.get(&trade_key) {
                if previous != &fingerprint {
                    bail!("conflicting PostgreSQL duplicate: {trade_key:?}");
                }
                continue;
            }
            seen.insert(trade_key, fingerprint);
            add_stat(&mut groups, key, row.qty, row.order_id);
        }
    }
    Ok(groups)
}

fn read_parquet(path: &Path) -> Result<DataFrame> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    ParquetReader::new(file)
        .finish()
        .with_context(|| format!("read {}", path.display()))
}

fn uniform_groups(
    path: &Path,
    start_us: i64,
    end_us: i64,
    epsilon: f64,
    symbols: Option<&BTreeSet<String>>,
) -> Result<(BTreeMap<GroupKey, Stat>, BTreeMap<GroupKey, BTreeSet<i64>>)> {
    let frame = read_parquet(path)?;
    let update_ts = frame.column("update_ts")?.i64()?;
    let symbol = frame.column("symbol")?.str()?;
    let venue = frame.column("trading_venue")?.str()?;
    let side = frame.column("side")?.str()?;
    let amount = frame.column("amount_update")?.f64()?;
    let client_id = frame.column("client_order_id")?.i64()?;
    let mut groups = BTreeMap::new();
    let mut client_ids = BTreeMap::<GroupKey, BTreeSet<i64>>::new();
    for row in 0..frame.height() {
        let timestamp = update_ts.get(row).context("null uniform update_ts")?;
        if timestamp < start_us || timestamp > end_us {
            continue;
        }
        let qty = amount.get(row).context("null uniform amount_update")?;
        if qty < -epsilon {
            bail!("negative uniform amount_update: {qty}");
        }
        if qty <= epsilon {
            continue;
        }
        let key = group_key(
            Market::from_venue(venue.get(row).context("null uniform trading_venue")?)?,
            symbol.get(row).context("null uniform symbol")?,
            side.get(row).context("null uniform side")?,
        )?;
        if symbols.is_some_and(|selected| !selected.contains(&key.symbol)) {
            continue;
        }
        let id = client_id.get(row).context("null uniform client_order_id")?;
        add_stat(&mut groups, key.clone(), qty, id.to_string());
        client_ids.entry(key).or_default().insert(id);
    }
    Ok((groups, client_ids))
}

fn unmatched_groups(
    path: &Path,
    start_us: i64,
    end_us: i64,
    epsilon: f64,
    uniform_ids: &BTreeMap<GroupKey, BTreeSet<i64>>,
    symbols: Option<&BTreeSet<String>>,
) -> Result<(BTreeMap<GroupKey, Stat>, usize, usize)> {
    let frame = read_parquet(path)?;
    let event_time = frame.column("event_time")?.i64()?;
    let trade_time = frame.column("trade_time")?.i64()?;
    let symbol = frame.column("symbol")?.str()?;
    let order_id = frame.column("order_id")?.i64()?;
    let client_id = frame.column("client_order_id")?.i64()?;
    let side = frame.column("side")?.str()?;
    let venue = frame.column("trading_venue")?.str()?;
    let cumulative = frame.column("cumulative_filled_quantity")?.f64()?;
    let mut series = BTreeMap::<(Market, String, i64), UnmatchedSeries>::new();
    for row in 0..frame.height() {
        let trade_ts = trade_time.get(row).context("null unmatched trade_time")?;
        let event_ts = event_time.get(row).context("null unmatched event_time")?;
        let timestamp_us = if trade_ts > 0 { trade_ts } else { event_ts };
        if timestamp_us > end_us {
            continue;
        }
        let key = group_key(
            Market::from_venue(venue.get(row).context("null unmatched trading_venue")?)?,
            symbol.get(row).context("null unmatched symbol")?,
            side.get(row).context("null unmatched side")?,
        )?;
        if symbols.is_some_and(|selected| !selected.contains(&key.symbol)) {
            continue;
        }
        let qty = cumulative
            .get(row)
            .context("null unmatched cumulative_filled_quantity")?;
        if qty < -epsilon {
            bail!("negative unmatched cumulative quantity: {qty}");
        }
        let exchange_order_id = order_id.get(row).context("null unmatched order_id")?;
        let map_key = (key.market, key.symbol.clone(), exchange_order_id);
        let value = series.entry(map_key).or_insert_with(|| UnmatchedSeries {
            group: key.clone(),
            client_ids: BTreeSet::new(),
            observations: Vec::new(),
        });
        if value.group != key {
            bail!("unmatched order has inconsistent side");
        }
        value.client_ids.insert(
            client_id
                .get(row)
                .context("null unmatched client_order_id")?,
        );
        value.observations.push(Observation {
            timestamp_us,
            cumulative_qty: qty,
        });
    }

    let mut groups = BTreeMap::new();
    let mut represented = 0;
    let mut unmatched_only = 0;
    for ((_, _, exchange_order_id), values) in series {
        let Some(order) = summarize_unmatched(values, start_us, end_us, epsilon) else {
            continue;
        };
        if order.client_ids.iter().any(|id| {
            uniform_ids
                .get(&order.group)
                .is_some_and(|ids| ids.contains(id))
        }) {
            represented += 1;
            continue;
        }
        let stat = groups.entry(order.group).or_insert_with(Stat::default);
        stat.qty += order.qty;
        stat.events += order.events;
        stat.orders.insert(exchange_order_id.to_string());
        unmatched_only += 1;
    }
    Ok((groups, represented, unmatched_only))
}

fn summarize_unmatched(
    mut series: UnmatchedSeries,
    start_us: i64,
    end_us: i64,
    epsilon: f64,
) -> Option<UnmatchedOrder> {
    series
        .observations
        .sort_by_key(|observation| observation.timestamp_us);
    let baseline = series
        .observations
        .iter()
        .filter(|value| value.timestamp_us < start_us)
        .map(|value| value.cumulative_qty)
        .fold(0.0_f64, f64::max);
    let current = series
        .observations
        .iter()
        .filter(|value| value.timestamp_us >= start_us && value.timestamp_us <= end_us)
        .collect::<Vec<_>>();
    if current.is_empty() {
        return None;
    }
    let end = current
        .iter()
        .map(|value| value.cumulative_qty)
        .fold(baseline, f64::max);
    let qty = end - baseline;
    let events = current.len();
    (qty > epsilon).then_some(UnmatchedOrder {
        group: series.group,
        client_ids: series.client_ids,
        qty,
        events,
    })
}

fn compare_groups(
    pg: &BTreeMap<GroupKey, Stat>,
    uniform: &BTreeMap<GroupKey, Stat>,
    unmatched: &BTreeMap<GroupKey, Stat>,
    epsilon: f64,
) -> Vec<GroupReport> {
    let mut keys = BTreeSet::new();
    keys.extend(pg.keys().cloned());
    keys.extend(uniform.keys().cloned());
    keys.extend(unmatched.keys().cloned());
    keys.into_iter()
        .map(|key| {
            let pg = pg.get(&key).cloned().unwrap_or_default();
            let uniform = uniform.get(&key).cloned().unwrap_or_default();
            let unmatched = unmatched.get(&key).cloned().unwrap_or_default();
            let local_qty = uniform.qty + unmatched.qty;
            let qty_diff = local_qty - pg.qty;
            GroupReport {
                market: key.market.as_str(),
                symbol: key.symbol,
                side: key.side,
                pg_events: pg.events,
                pg_orders: pg.orders.len(),
                pg_qty: pg.qty,
                uniform_events: uniform.events,
                uniform_orders: uniform.orders.len(),
                uniform_qty: uniform.qty,
                unmatched_events: unmatched.events,
                unmatched_orders: unmatched.orders.len(),
                unmatched_qty: unmatched.qty,
                local_qty,
                qty_diff,
                status: if qty_diff.abs() <= epsilon {
                    "MATCH"
                } else {
                    "MISMATCH"
                },
            }
        })
        .collect()
}

fn write_groups(path: &Path, groups: &[GroupReport]) -> Result<()> {
    let mut writer =
        csv::Writer::from_path(path).with_context(|| format!("create {}", path.display()))?;
    for group in groups {
        writer.serialize(group)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, value).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> GroupKey {
        GroupKey {
            market: Market::Swap,
            symbol: "BTCUSDT".to_string(),
            side: "buy".to_string(),
        }
    }

    fn stat(qty: f64) -> Stat {
        Stat {
            qty,
            events: 1,
            orders: BTreeSet::from(["order".to_string()]),
        }
    }

    #[test]
    fn supports_bybit_venues() {
        assert_eq!(Market::from_venue("BybitMargin").unwrap(), Market::Spot);
        assert_eq!(Market::from_venue("BybitFutures").unwrap(), Market::Swap);
    }

    #[test]
    fn combines_uniform_and_unmatched_quantities() {
        let key = key();
        let rows = compare_groups(
            &BTreeMap::from([(key.clone(), stat(3.0))]),
            &BTreeMap::from([(key.clone(), stat(2.0))]),
            &BTreeMap::from([(key, stat(1.0))]),
            1e-8,
        );
        assert_eq!(rows[0].status, "MATCH");
        assert_eq!(rows[0].local_qty, 3.0);
    }

    #[test]
    fn unmatched_cumulative_uses_pre_window_baseline() {
        let result = summarize_unmatched(
            UnmatchedSeries {
                group: key(),
                client_ids: BTreeSet::from([7]),
                observations: vec![
                    Observation {
                        timestamp_us: 900,
                        cumulative_qty: 1.0,
                    },
                    Observation {
                        timestamp_us: 1_100,
                        cumulative_qty: 1.5,
                    },
                    Observation {
                        timestamp_us: 1_200,
                        cumulative_qty: 2.0,
                    },
                ],
            },
            1_000,
            2_000,
            1e-8,
        )
        .unwrap();
        assert!((result.qty - 1.0).abs() < 1e-12);
        assert_eq!(result.events, 2);
    }
}
