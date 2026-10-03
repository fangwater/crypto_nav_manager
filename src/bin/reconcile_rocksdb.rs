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
const MAX_PERSISTED_GROUPS: usize = 1_000;

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

    #[arg(long, default_value_t = 1e-9)]
    rel_epsilon: f64,

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

    /// Retain CSV and Parquet exports for troubleshooting, including failed runs.
    #[arg(long)]
    keep_work_dir: bool,
}

struct WorkDirGuard {
    work_root: PathBuf,
    reports_root: PathBuf,
    cleanup_on_success: bool,
    keep_work_dir: bool,
    succeeded: bool,
    finished: bool,
}

impl WorkDirGuard {
    fn new(work_root: PathBuf, cleanup_on_success: bool, keep_work_dir: bool) -> Self {
        let run_name = work_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("custom");
        let reports_root = env::temp_dir()
            .join("crypto_nav_rocksdb_reconcile_reports")
            .join(format!("{run_name}-{}", Utc::now().timestamp_micros()));
        Self::with_reports_root(work_root, reports_root, cleanup_on_success, keep_work_dir)
    }

    fn with_reports_root(
        work_root: PathBuf,
        reports_root: PathBuf,
        cleanup_on_success: bool,
        keep_work_dir: bool,
    ) -> Self {
        Self {
            work_root,
            reports_root,
            cleanup_on_success,
            keep_work_dir,
            succeeded: false,
            finished: false,
        }
    }

    fn finish(&mut self, succeeded: bool) -> Result<PathBuf> {
        self.succeeded = succeeded;
        let summary_path = self.finalize()?;
        self.finished = true;
        Ok(summary_path)
    }

    fn finalize(&self) -> Result<PathBuf> {
        let report_result = if self.succeeded {
            Ok(self.work_root.join("summary.json"))
        } else {
            persist_small_reports(&self.work_root, &self.reports_root)
                .map(|()| self.reports_root.join("summary.json"))
        };
        let cleanup_result = if !self.keep_work_dir && (!self.succeeded || self.cleanup_on_success)
        {
            fs::remove_dir_all(&self.work_root)
                .with_context(|| format!("remove work directory {}", self.work_root.display()))
        } else {
            Ok(())
        };
        cleanup_result?;
        report_result
    }
}

struct RemoteWorkGuard {
    host: String,
    path: String,
    keep: bool,
}

impl Drop for RemoteWorkGuard {
    fn drop(&mut self) {
        if !self.keep {
            let mut cleanup = Command::new("ssh");
            cleanup.args([&self.host, "rm", "-rf", "--", &self.path]);
            if let Err(error) = run(&mut cleanup) {
                eprintln!("remote cleanup failed for {}: {error:#}", self.path);
            }
        }
    }
}

impl Drop for WorkDirGuard {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(error) = self.finalize() {
                eprintln!("reconciliation work directory cleanup failed: {error:#}");
            }
        }
    }
}

fn persist_small_reports(work_root: &Path, reports_root: &Path) -> Result<()> {
    fs::create_dir_all(reports_root)?;
    let mut top_summary = read_or_fallback_summary(&work_root.join("summary.json"));
    for entry in fs::read_dir(work_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let strategy = entry.file_name();
        let source = entry.path();
        let source_groups = source.join("groups.csv");
        let source_summary = source.join("summary.json");
        if !source_groups.exists() && !source_summary.exists() {
            continue;
        }
        let destination = reports_root.join(&strategy);
        fs::create_dir_all(&destination)?;
        let (rows, truncated) = if source_groups.exists() {
            copy_limited_groups(&source_groups, &destination.join("groups.csv"))?
        } else {
            (0, false)
        };
        let mut summary = read_or_fallback_summary(&source_summary);
        add_group_report_metadata(&mut summary, rows, truncated);
        write_json(&destination.join("summary.json"), &summary)?;
        if let Some(items) = top_summary.as_array_mut() {
            for item in items.iter_mut() {
                if item.get("strategy").and_then(|value| value.as_str()) == strategy.to_str() {
                    add_group_report_metadata(item, rows, truncated);
                }
            }
        }
    }
    write_json(&reports_root.join("summary.json"), &top_summary)
}

fn read_or_fallback_summary(path: &Path) -> serde_json::Value {
    File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "aligned": false,
                "error": "reconciliation ended before a complete summary was written",
                "panicked": std::thread::panicking(),
            })
        })
}

fn add_group_report_metadata(summary: &mut serde_json::Value, rows: usize, truncated: bool) {
    if let Some(object) = summary.as_object_mut() {
        object.insert("groups_csv_rows".into(), rows.into());
        object.insert("groups_csv_truncated".into(), truncated.into());
    }
}

fn copy_limited_groups(source: &Path, destination: &Path) -> Result<(usize, bool)> {
    let mut reader = csv::Reader::from_path(source)?;
    let mut writer = csv::Writer::from_path(destination)?;
    writer.write_record(reader.headers()?)?;
    let mut rows = 0;
    let mut truncated = false;
    for record in reader.records() {
        if rows == MAX_PERSISTED_GROUPS {
            truncated = true;
            break;
        }
        writer.write_record(&record?)?;
        rows += 1;
    }
    writer.flush()?;
    Ok((rows, truncated))
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

    fn from_alignment_name(value: &str) -> Result<Self> {
        match value {
            "spot" => Ok(Self::Spot),
            "swap" => Ok(Self::Swap),
            _ => bail!("unsupported alignment exclusion market {value:?}"),
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
    let mut work_guard = WorkDirGuard::new(
        report_root.clone(),
        args.cleanup_on_success,
        args.keep_work_dir,
    );
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
    let summary_path = work_guard.finish(!failed)?;
    println!("summary={}", summary_path.display());
    if args.cleanup_on_success && !failed && !args.keep_work_dir {
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
    if !args.rel_epsilon.is_finite() || args.rel_epsilon < 0.0 {
        bail!("--rel-epsilon must be finite and non-negative");
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
        if path.is_symlink() {
            bail!("--work-dir must not be a symbolic link");
        }
        if path.exists() && fs::read_dir(path)?.next().is_some() {
            bail!("--work-dir must be empty: {}", path.display());
        }
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
    fs::create_dir_all(path.parent().context("report root has no parent")?)?;
    fs::create_dir(&path).with_context(|| format!("create report root {}", path.display()))?;
    Ok(path)
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
    let trade_exclusions = load_trade_exclusions(pool, strategy).await?;
    let pg = pg_groups(
        &pg_dir,
        scan_start_ms,
        actual_end_ms,
        selected_symbols.as_ref(),
        &trade_exclusions,
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
    let groups = compare_groups(
        &pg,
        &uniform,
        &unmatched,
        args.qty_epsilon,
        args.rel_epsilon,
    );
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

async fn load_trade_exclusions(
    pool: &PgPool,
    strategy: &str,
) -> Result<BTreeSet<(Market, String)>> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT market,order_id FROM rocksdb_alignment_trade_exclusions \
         WHERE strategy_slug=$1",
    )
    .bind(strategy)
    .fetch_all(pool)
    .await
    .with_context(|| format!("load alignment trade exclusions for {strategy}"))?;
    rows.into_iter()
        .map(|(market, order_id)| Ok((Market::from_alignment_name(&market)?, order_id)))
        .collect()
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
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(60))
        .build()
        .context("build persist read client")?;
    fs::create_dir_all(output_root)
        .with_context(|| format!("create center export {}", output_root.display()))?;
    let mut use_curl = false;

    for table in [
        "uniform_orders",
        "order_updates_unmatched",
        "trade_updates_unmatched",
    ] {
        let mut frames = Vec::new();
        let mut cursor = start_us;
        while cursor < end_us {
            let window_end = cursor.saturating_add(3_600_000_000).min(end_us);
            let params = [
                ("table", table.to_string()),
                ("source_id", strategy.to_string()),
                ("start_us", cursor.to_string()),
                ("end_us", window_end.to_string()),
                ("format", "parquet".to_string()),
            ];
            let url = reqwest::Url::parse_with_params(&endpoint, &params)
                .with_context(|| format!("build center URL for {strategy}/{table}"))?;
            let read_with_curl = || -> Result<Vec<u8>> {
                let output = Command::new("curl")
                    .args(["--fail", "--silent", "--show-error", "--max-time", "60"])
                    .arg(url.as_str())
                    .output()
                    .with_context(|| {
                        format!("run curl for {strategy}/{table} {cursor}..{window_end}")
                    })?;
                if !output.status.success() {
                    bail!(
                        "curl failed for {strategy}/{table} {cursor}..{window_end}: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                Ok(output.stdout)
            };
            let body = if use_curl {
                read_with_curl()?
            } else {
                match client
                    .get(url.clone())
                    .header(reqwest::header::CONNECTION, "close")
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .and_then(reqwest::blocking::Response::bytes)
                {
                    Ok(body) => body.to_vec(),
                    Err(error) => {
                        use_curl = true;
                        eprintln!(
                            "center read switching to curl strategy={strategy} table={table} window={cursor}..{window_end}: {error}"
                        );
                        read_with_curl()?
                    }
                }
            };
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
    let _remote_guard = RemoteWorkGuard {
        host: args.ssh_host.clone(),
        path: remote_root.clone(),
        keep: args.keep_remote,
    };
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

    result
}

fn is_center(strategy: &str) -> bool {
    matches!(
        strategy,
        "bybit_mm_alpha" | "binance-intra-arb01" | "bybit-intra-arb01" | "bybit-intra-arb02"
    )
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
    exclusions: &BTreeSet<(Market, String)>,
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
            if row.ts < start_ms || row.ts > end_ms {
                continue;
            }
            if !row.qty.is_finite() {
                bail!("non-finite PostgreSQL quantity in {}", path.display());
            }
            if row.qty <= 0.0 {
                continue;
            }
            let market = Market::from_sid(row.sid.trim())?;
            if exclusions.contains(&(market, row.order_id.clone())) {
                continue;
            }
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
        if !qty.is_finite() {
            bail!("non-finite uniform amount_update: {qty}");
        }
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
        if !qty.is_finite() {
            bail!("non-finite unmatched cumulative quantity: {qty}");
        }
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
    abs_epsilon: f64,
    rel_epsilon: f64,
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
                status: if quantities_equal(local_qty, pg.qty, abs_epsilon, rel_epsilon) {
                    "MATCH"
                } else {
                    "MISMATCH"
                },
            }
        })
        .collect()
}

fn quantities_equal(a: f64, b: f64, abs_epsilon: f64, rel_epsilon: f64) -> bool {
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    let diff = (a - b).abs();
    diff <= abs_epsilon || diff <= rel_epsilon * a.abs().max(b.abs())
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

    fn test_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "reconcile_rocksdb_test_{label}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

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
    fn reads_configured_order_sources_from_center() {
        assert!(is_center("binance-intra-arb01"));
        assert!(is_center("bybit_mm_alpha"));
        assert!(is_center("bybit-intra-arb01"));
        assert!(is_center("bybit-intra-arb02"));
    }

    #[test]
    fn combines_uniform_and_unmatched_quantities() {
        let key = key();
        let rows = compare_groups(
            &BTreeMap::from([(key.clone(), stat(3.0))]),
            &BTreeMap::from([(key.clone(), stat(2.0))]),
            &BTreeMap::from([(key, stat(1.0))]),
            1e-8,
            1e-9,
        );
        assert_eq!(rows[0].status, "MATCH");
        assert_eq!(rows[0].local_qty, 3.0);
    }

    #[test]
    fn zkusdt_rounding_differences_match() {
        for diff in [1.41561031e-7, 3.05473804e-7, 1.34110451e-6, 7.97212124e-7] {
            assert!(quantities_equal(
                40_000_000.0,
                40_000_000.0 + diff,
                1e-8,
                1e-9
            ));
        }
    }

    #[test]
    fn material_quantity_differences_do_not_match() {
        assert!(!quantities_equal(1.0, 1.001, 1e-8, 1e-9));
        assert!(!quantities_equal(40_000_000.0, 40_000_040.0, 1e-8, 1e-9));
        assert!(quantities_equal(1.0, 1.0 + 5e-9, 1e-8, 1e-9));
    }

    #[test]
    fn non_finite_quantities_never_match() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!quantities_equal(value, value, 1e-8, 1e-9));
            assert!(!quantities_equal(value, 1.0, 1e-8, 1e-9));
        }
    }

    #[test]
    fn failed_work_dir_keeps_only_limited_reports() {
        let root = test_root("failed");
        let work = root.join("work");
        let reports = root.join("reports");
        let strategy = work.join("bybit-intra-arb01");
        fs::create_dir_all(&strategy).unwrap();
        fs::write(
            work.join("summary.json"),
            "[{\"strategy\":\"bybit-intra-arb01\"}]",
        )
        .unwrap();
        fs::write(strategy.join("summary.json"), "{\"aligned\":false}").unwrap();
        fs::write(strategy.join("uniform_orders.parquet"), b"large export").unwrap();
        let mut groups = csv::Writer::from_path(strategy.join("groups.csv")).unwrap();
        groups.write_record(["symbol", "status"]).unwrap();
        for _ in 0..=MAX_PERSISTED_GROUPS {
            groups.write_record(["ZKUSDT", "MISMATCH"]).unwrap();
        }
        groups.flush().unwrap();
        drop(groups);

        let mut guard = WorkDirGuard::with_reports_root(work.clone(), reports.clone(), true, false);
        assert_eq!(guard.finish(false).unwrap(), reports.join("summary.json"));
        assert!(!work.exists());
        assert!(
            !reports
                .join("bybit-intra-arb01/uniform_orders.parquet")
                .exists()
        );
        let summary: serde_json::Value = serde_json::from_slice(
            &fs::read(reports.join("bybit-intra-arb01/summary.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(summary["groups_csv_rows"], MAX_PERSISTED_GROUPS);
        assert_eq!(summary["groups_csv_truncated"], true);
        let rows = csv::Reader::from_path(reports.join("bybit-intra-arb01/groups.csv"))
            .unwrap()
            .records()
            .count();
        assert_eq!(rows, MAX_PERSISTED_GROUPS);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_work_dir_obeys_cleanup_flag() {
        for cleanup in [false, true] {
            let root = test_root("success");
            let work = root.join("work");
            fs::create_dir(&work).unwrap();
            fs::write(work.join("summary.json"), "[]").unwrap();
            let mut guard =
                WorkDirGuard::with_reports_root(work.clone(), root.join("reports"), cleanup, false);
            guard.finish(true).unwrap();
            assert_eq!(work.exists(), !cleanup);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn panic_cleans_work_dir_and_writes_fallback_summary() {
        let root = test_root("panic");
        let work = root.join("work");
        let reports = root.join("reports");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("uniform_orders.parquet"), b"large export").unwrap();
        let result = std::panic::catch_unwind(|| {
            let _guard =
                WorkDirGuard::with_reports_root(work.clone(), reports.clone(), false, false);
            panic!("simulated reconciliation panic");
        });
        assert!(result.is_err());
        assert!(!work.exists());
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(reports.join("summary.json")).unwrap()).unwrap();
        assert_eq!(summary["panicked"], true);
        fs::remove_dir_all(root).unwrap();
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
