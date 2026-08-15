use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_nav_manager::intra_latency::{
    self, HourlyLatencyPoint, LatencyOrderRow, planned_hour_windows,
};
use polars::prelude::*;
use serde::Serialize;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    env,
    io::Cursor,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DEFAULT_STRATEGIES: [&str; 2] = ["binance-intra-arb01", "bybit-intra-arb01"];

#[derive(Clone, Debug, Parser)]
#[command(
    about = "Store hourly persist latency snapshots; default last complete UTC hour, or recompute an hour-aligned range"
)]
struct Args {
    #[arg(long)]
    strategy: Vec<String>,

    /// Overrides CRYPTO_NAV_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,

    #[arg(long, default_value = "http://127.0.0.1:8822")]
    persist_read_url: String,

    /// Inclusive hour-aligned start in UTC milliseconds. Defaults to the last complete hour.
    #[arg(long)]
    window_start_ms: Option<i64>,

    /// Exclusive hour-aligned end in UTC milliseconds. Incomplete hours are skipped.
    #[arg(long)]
    window_end_ms: Option<i64>,

    #[arg(long, default_value_t = 60)]
    source_chunk_minutes: i64,
}

#[derive(Debug, Serialize)]
struct Summary {
    strategy: String,
    window_start_ms: i64,
    window_end_ms: i64,
    downloaded_rows: usize,
    margin_new_create_count: u64,
    futures_new_create_count: u64,
    spot_trigger_count: u64,
    futures_trigger_count: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.source_chunk_minutes <= 0 {
        bail!("--source-chunk-minutes must be positive");
    }
    let strategies = if args.strategy.is_empty() {
        DEFAULT_STRATEGIES
            .iter()
            .map(|slug| slug.to_string())
            .collect::<Vec<_>>()
    } else {
        args.strategy.clone()
    };
    for strategy in &strategies {
        if !intra_latency::supports_hourly_latency(strategy) {
            bail!("unsupported hourly latency strategy {strategy}");
        }
    }

    let now_ms = now_ms()?;
    let windows = planned_hour_windows(now_ms, args.window_start_ms, args.window_end_ms)?;

    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;

    let mut summaries = Vec::new();
    for strategy in strategies {
        for window in &windows {
            let fetch_args = args.clone();
            let fetch_strategy = strategy.clone();
            let window_start_ms = window.start_ms;
            let window_end_ms = window.end_ms;
            let (rows, downloaded_rows) = tokio::task::spawn_blocking(move || {
                fetch_latency_rows(&fetch_args, &fetch_strategy, window_start_ms, window_end_ms)
            })
            .await
            .context("join persist latency download")??;
            let point = intra_latency::compute_hourly_latency(
                &strategy,
                window_start_ms,
                window_end_ms,
                now_ms,
                &rows,
            )?;
            upsert_point(&pool, &point).await?;
            summaries.push(Summary {
                strategy: strategy.clone(),
                window_start_ms,
                window_end_ms,
                downloaded_rows,
                margin_new_create_count: point.margin_new_create.sample_count,
                futures_new_create_count: point.futures_new_create.sample_count,
                spot_trigger_count: point.spot_trigger.sample_count,
                futures_trigger_count: point.futures_trigger.sample_count,
            });
        }
    }
    println!("{}", serde_json::to_string(&summaries)?);
    pool.close().await;
    Ok(())
}

fn now_ms() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_millis(),
    )
    .context("system clock milliseconds overflow i64")?)
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

async fn upsert_point(pool: &PgPool, point: &HourlyLatencyPoint) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO intra_hourly_latency (
               strategy_slug, window_start_ms, window_end_ms, computed_at_ms,
               margin_new_create_count, margin_new_create_normal_count,
               margin_new_create_p50_ms, margin_new_create_p90_ms,
               futures_new_create_count, futures_new_create_normal_count,
               futures_new_create_p50_ms, futures_new_create_p90_ms,
               spot_trigger_count, spot_trigger_normal_count,
               spot_trigger_p50_ms, spot_trigger_p90_ms,
               futures_trigger_count, futures_trigger_normal_count,
               futures_trigger_p50_ms, futures_trigger_p90_ms
           ) VALUES (
               $1,$2,$3,$4,
               $5,$6,$7,$8,
               $9,$10,$11,$12,
               $13,$14,$15,$16,
               $17,$18,$19,$20
           )
           ON CONFLICT (strategy_slug, window_start_ms) DO UPDATE SET
               window_end_ms=EXCLUDED.window_end_ms,
               computed_at_ms=EXCLUDED.computed_at_ms,
               margin_new_create_count=EXCLUDED.margin_new_create_count,
               margin_new_create_normal_count=EXCLUDED.margin_new_create_normal_count,
               margin_new_create_p50_ms=EXCLUDED.margin_new_create_p50_ms,
               margin_new_create_p90_ms=EXCLUDED.margin_new_create_p90_ms,
               futures_new_create_count=EXCLUDED.futures_new_create_count,
               futures_new_create_normal_count=EXCLUDED.futures_new_create_normal_count,
               futures_new_create_p50_ms=EXCLUDED.futures_new_create_p50_ms,
               futures_new_create_p90_ms=EXCLUDED.futures_new_create_p90_ms,
               spot_trigger_count=EXCLUDED.spot_trigger_count,
               spot_trigger_normal_count=EXCLUDED.spot_trigger_normal_count,
               spot_trigger_p50_ms=EXCLUDED.spot_trigger_p50_ms,
               spot_trigger_p90_ms=EXCLUDED.spot_trigger_p90_ms,
               futures_trigger_count=EXCLUDED.futures_trigger_count,
               futures_trigger_normal_count=EXCLUDED.futures_trigger_normal_count,
               futures_trigger_p50_ms=EXCLUDED.futures_trigger_p50_ms,
               futures_trigger_p90_ms=EXCLUDED.futures_trigger_p90_ms"#,
    )
    .bind(&point.strategy_slug)
    .bind(point.window_start_ms)
    .bind(point.window_end_ms)
    .bind(point.computed_at_ms)
    .bind(i64::try_from(point.margin_new_create.sample_count)?)
    .bind(i64::try_from(point.margin_new_create.normal_count)?)
    .bind(point.margin_new_create.p50_ms)
    .bind(point.margin_new_create.p90_ms)
    .bind(i64::try_from(point.futures_new_create.sample_count)?)
    .bind(i64::try_from(point.futures_new_create.normal_count)?)
    .bind(point.futures_new_create.p50_ms)
    .bind(point.futures_new_create.p90_ms)
    .bind(i64::try_from(point.spot_trigger.sample_count)?)
    .bind(i64::try_from(point.spot_trigger.normal_count)?)
    .bind(point.spot_trigger.p50_ms)
    .bind(point.spot_trigger.p90_ms)
    .bind(i64::try_from(point.futures_trigger.sample_count)?)
    .bind(i64::try_from(point.futures_trigger.normal_count)?)
    .bind(point.futures_trigger.p50_ms)
    .bind(point.futures_trigger.p90_ms)
    .execute(pool)
    .await
    .context("upsert hourly latency point")?;
    Ok(())
}

fn fetch_latency_rows(
    args: &Args,
    strategy: &str,
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<(Vec<LatencyOrderRow>, usize)> {
    let start_us = window_start_ms
        .checked_mul(1_000)
        .context("window start overflows microseconds")?;
    let end_us = window_end_ms
        .checked_mul(1_000)
        .context("window end overflows microseconds")?;
    let endpoint = format!("{}/v1/read", args.persist_read_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(60))
        .build()
        .context("build persist read client")?;
    let chunk_us = args
        .source_chunk_minutes
        .checked_mul(60_000_000)
        .context("source chunk overflow")?;
    let mut rows = Vec::new();
    let mut downloaded_rows = 0usize;
    let mut use_curl = false;
    let mut cursor = start_us;
    while cursor < end_us {
        let window_end = cursor.saturating_add(chunk_us).min(end_us);
        let params = [
            ("table", "uniform_orders".to_string()),
            ("source_id", strategy.to_string()),
            ("start_us", cursor.to_string()),
            ("end_us", window_end.to_string()),
            ("format", "parquet".to_string()),
        ];
        let url = reqwest::Url::parse_with_params(&endpoint, &params)
            .with_context(|| format!("build persist URL for {strategy}"))?;
        let read_with_curl = || -> Result<Vec<u8>> {
            let output = Command::new("curl")
                .args(["--fail", "--silent", "--show-error", "--max-time", "60"])
                .arg(url.as_str())
                .output()
                .with_context(|| {
                    format!("run curl for {strategy} uniform_orders {cursor}..{window_end}")
                })?;
            if !output.status.success() {
                bail!(
                    "curl failed for {strategy} uniform_orders {cursor}..{window_end}: {}",
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
                        "center read switching to curl strategy={strategy} table=uniform_orders window={cursor}..{window_end}: {error}"
                    );
                    read_with_curl()?
                }
            }
        };
        let frame = ParquetReader::new(Cursor::new(body))
            .finish()
            .context("decode persist uniform_orders parquet")?;
        downloaded_rows += frame.height();
        append_latency_rows(&frame, &mut rows)?;
        cursor = window_end;
    }
    Ok((rows, downloaded_rows))
}

fn append_latency_rows(frame: &DataFrame, rows: &mut Vec<LatencyOrderRow>) -> Result<()> {
    let source_ts = frame.column("ts_us")?.i64()?;
    let venue = frame.column("trading_venue")?.str()?;
    let status = frame.column("status")?.str()?;
    let create_ts = frame.column("create_ts")?.i64()?;
    let update_ts = frame.column("update_ts")?.i64()?;
    let signal_ts = optional_i64(frame, "signal_ts")?;
    let signal_open_ts = optional_i64(frame, "signal_open_ts")?;
    let signal_hedge_ts = optional_i64(frame, "signal_hedge_ts")?;
    for row in 0..frame.height() {
        rows.push(LatencyOrderRow {
            ts_us: source_ts.get(row).context("null ts_us")?,
            trading_venue: venue.get(row).unwrap_or_default().to_string(),
            status: status.get(row).unwrap_or_default().to_string(),
            create_ts: create_ts.get(row).unwrap_or(0),
            update_ts: update_ts.get(row).unwrap_or(0),
            signal_ts: signal_ts
                .as_ref()
                .and_then(|column| column.get(row))
                .unwrap_or(0),
            signal_open_ts: signal_open_ts
                .as_ref()
                .and_then(|column| column.get(row))
                .unwrap_or(0),
            signal_hedge_ts: signal_hedge_ts
                .as_ref()
                .and_then(|column| column.get(row))
                .unwrap_or(0),
        });
    }
    Ok(())
}

fn optional_i64(frame: &DataFrame, name: &str) -> Result<Option<Int64Chunked>> {
    match frame.column(name) {
        Ok(column) => Ok(Some(column.i64()?.clone())),
        Err(_) => Ok(None),
    }
}
