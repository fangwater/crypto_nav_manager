use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, TimeDelta};
use clap::Parser;
use polars::prelude::*;
use sqlx::{
    AssertSqlSafe, PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process,
};

const SUPPORTED: [&str; 3] = [
    "binance-intra-arb01",
    "bybit-intra-arb01",
    "bybit-intra-arb02",
];
const MICROS_PER_DAY: i64 = 86_400_000_000;
const MAX_SNAPSHOT_RETRIES: usize = 5;

#[derive(Debug, Parser)]
#[command(about = "Export synthesized intra matched orders to daily Parquet files")]
struct Args {
    /// Strategy slug registered in strategy_envs. May be repeated. Defaults to all supported.
    #[arg(long)]
    strategy: Vec<String>,

    /// UTC trade date in YYYYMMDD form. May be repeated. Defaults to every changed date.
    #[arg(long, value_parser = parse_trade_date)]
    trade_date: Vec<NaiveDate>,

    #[arg(long, default_value = "/home/ubuntu/order_data")]
    output_root: PathBuf,

    /// Rebuild selected files even when their exported database state is current.
    #[arg(long)]
    force: bool,

    /// Overrides CRYPTO_NAV_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Debug)]
struct Strategy {
    slug: String,
    directory_alias: String,
    schema: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DayState {
    row_count: i64,
    max_updated_us: i64,
}

#[derive(Debug)]
struct OrderRow {
    fkey: i64,
    symbol: String,
    side: String,
    cts: i64,
    open_uts: i64,
    fts: Option<i64>,
    holding: i64,
    holding_close: Option<i64>,
    close_count: i64,
    price: f64,
    amount: f64,
    cprice: Option<f64>,
    camount: f64,
    range: f64,
    crange: f64,
    tlen: Option<f64>,
    pnlu: Option<f64>,
    open_fill_amount: f64,
    remaining_amount: f64,
    netted_amount: f64,
    close_notional: f64,
    matching_state: String,
    open_source_ts_us: i64,
    updated_at_us: i64,
}

#[derive(Debug, Default)]
struct ExportSummary {
    strategies: usize,
    files_written: usize,
    files_current: usize,
    rows_written: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    fs::create_dir_all(&args.output_root)
        .with_context(|| format!("create output root {}", args.output_root.display()))?;

    let pool = connect_postgres(args.database_url.as_deref()).await?;
    let strategies = load_strategies(&pool, &args.strategy).await?;
    let selected_dates = args.trade_date.iter().copied().collect::<BTreeSet<_>>();
    let mut summary = ExportSummary::default();

    for strategy in strategies {
        let day_states = load_day_states(&pool, &strategy.schema).await?;
        let dates = if selected_dates.is_empty() {
            day_states.keys().copied().collect::<Vec<_>>()
        } else {
            for date in &selected_dates {
                if !day_states.contains_key(date) {
                    bail!("{} has no matched orders on {date}", strategy.slug);
                }
            }
            selected_dates.iter().copied().collect::<Vec<_>>()
        };

        let output_dir = args
            .output_root
            .join(&strategy.directory_alias)
            .join("matched_order");
        let state_dir = args
            .output_root
            .join(".export_state")
            .join(&strategy.directory_alias)
            .join("matched_order");
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("create output directory {}", output_dir.display()))?;
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("create export state directory {}", state_dir.display()))?;

        for date in dates {
            let database_state = day_states[&date];
            let output_path = output_dir.join(format!("{}.parquet", date.format("%Y%m%d")));
            let state_path = state_dir.join(format!("{}.state", date.format("%Y%m%d")));
            if !args.force
                && output_path.is_file()
                && read_export_state(&state_path)? == Some(database_state)
            {
                summary.files_current += 1;
                continue;
            }

            let rows =
                export_stable_snapshot(&pool, &strategy, date, &output_path, &state_path).await?;
            summary.files_written += 1;
            summary.rows_written += rows;
            println!(
                "exported strategy={} alias={} trade_date={} rows={} path={}",
                strategy.slug,
                strategy.directory_alias,
                date.format("%Y%m%d"),
                rows,
                output_path.display()
            );
        }
        summary.strategies += 1;
    }

    println!(
        "matched_order export complete: strategies={} files_written={} files_current={} rows_written={}",
        summary.strategies, summary.files_written, summary.files_current, summary.rows_written
    );
    pool.close().await;
    Ok(())
}

fn parse_trade_date(value: &str) -> std::result::Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y%m%d")
        .map_err(|_| format!("invalid trade date {value:?}; expected YYYYMMDD"))
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

async fn load_strategies(pool: &PgPool, requested: &[String]) -> Result<Vec<Strategy>> {
    let slugs: Vec<String> = if requested.is_empty() {
        SUPPORTED.iter().map(|value| value.to_string()).collect()
    } else {
        let unique = requested.iter().cloned().collect::<BTreeSet<_>>();
        for slug in &unique {
            if !SUPPORTED.contains(&slug.as_str()) {
                bail!(
                    "unsupported matched-order strategy {slug:?}; configured: {}",
                    SUPPORTED.join(", ")
                );
            }
        }
        unique.into_iter().collect()
    };

    let mut strategies = Vec::with_capacity(slugs.len());
    for slug in slugs {
        let row =
            sqlx::query("SELECT slug,alias,db_schema FROM strategy_envs WHERE enabled AND slug=$1")
                .bind(&slug)
                .fetch_optional(pool)
                .await
                .with_context(|| format!("load strategy {slug}"))?
                .with_context(|| format!("enabled strategy not found: {slug}"))?;
        let schema: String = row.try_get("db_schema")?;
        validate_schema(&schema)?;
        let alias = row
            .try_get::<Option<String>, _>("alias")?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| slug.clone());
        strategies.push(Strategy {
            slug: row.try_get("slug")?,
            directory_alias: normalize_alias(&alias)?,
            schema,
        });
    }
    Ok(strategies)
}

fn validate_schema(schema: &str) -> Result<()> {
    if schema.is_empty()
        || !schema.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))
        })
    {
        bail!("invalid PostgreSQL strategy schema {schema:?}");
    }
    Ok(())
}

fn normalize_alias(alias: &str) -> Result<String> {
    let mut result = String::new();
    let mut separator = false;
    for character in alias.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !result.is_empty() {
                result.push('-');
            }
            result.push(character.to_ascii_lowercase());
            separator = false;
        } else if character.is_ascii_whitespace() || matches!(character, '-' | '_') {
            separator = true;
        } else {
            bail!("strategy alias contains unsupported character: {alias:?}");
        }
    }
    if result.is_empty() {
        bail!("strategy alias cannot be empty");
    }
    Ok(result)
}

async fn load_day_states(pool: &PgPool, schema: &str) -> Result<BTreeMap<NaiveDate, DayState>> {
    let sql = format!(
        "SELECT cts/{MICROS_PER_DAY} AS utc_day,COUNT(*)::bigint AS row_count,\
         (EXTRACT(EPOCH FROM MAX(updated_at))*1000000)::bigint AS max_updated_us \
         FROM {schema}.intra_orders GROUP BY 1 ORDER BY 1"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .with_context(|| format!("load {schema} matched-order day states"))?;
    rows.into_iter()
        .map(|row| {
            let utc_day: i64 = row.try_get("utc_day")?;
            Ok((
                date_from_utc_day(utc_day)?,
                DayState {
                    row_count: row.try_get("row_count")?,
                    max_updated_us: row.try_get("max_updated_us")?,
                },
            ))
        })
        .collect()
}

async fn export_stable_snapshot(
    pool: &PgPool,
    strategy: &Strategy,
    date: NaiveDate,
    output_path: &Path,
    state_path: &Path,
) -> Result<usize> {
    for attempt in 1..=MAX_SNAPSHOT_RETRIES {
        let (snapshot_state, rows) = load_day_snapshot(pool, &strategy.schema, date).await?;
        let mut frame = rows_to_frame(rows)?;
        write_parquet_atomic(output_path, &mut frame)?;

        let current_state = load_one_day_state(pool, &strategy.schema, date)
            .await?
            .with_context(|| format!("{} lost all rows for {date}", strategy.slug))?;
        if current_state == snapshot_state {
            write_export_state_atomic(state_path, snapshot_state)?;
            return Ok(frame.height());
        }
        eprintln!(
            "matched orders changed during export strategy={} trade_date={} attempt={attempt}; retrying",
            strategy.slug,
            date.format("%Y%m%d")
        );
    }
    bail!(
        "{} matched orders kept changing while exporting {date}; retry limit reached",
        strategy.slug
    )
}

async fn load_day_snapshot(
    pool: &PgPool,
    schema: &str,
    date: NaiveDate,
) -> Result<(DayState, Vec<OrderRow>)> {
    let (start_us, end_us) = day_bounds(date)?;
    let mut tx = pool.begin().await.context("begin matched-order snapshot")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .context("configure matched-order snapshot")?;
    let state = load_one_day_state_executor(&mut tx, schema, start_us, end_us)
        .await?
        .with_context(|| format!("no matched orders in {schema} on {date}"))?;
    let sql = format!(
        "SELECT fkey,symbol,side,cts,open_uts,fts,holding,holding_close,close_count,price,\
         amount,cprice,camount,range,crange,tlen,pnlu,open_fill_amount,remaining_amount,\
         netted_amount,close_notional,matching_state,open_source_ts_us,\
         (EXTRACT(EPOCH FROM updated_at)*1000000)::bigint AS updated_at_us \
         FROM {schema}.intra_orders WHERE cts >= $1 AND cts < $2 ORDER BY cts,fkey"
    );
    let query_rows = sqlx::query(AssertSqlSafe(sql))
        .bind(start_us)
        .bind(end_us)
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("load {schema} matched orders for {date}"))?;
    tx.commit().await.context("commit matched-order snapshot")?;
    if i64::try_from(query_rows.len()).ok() != Some(state.row_count) {
        bail!(
            "snapshot row count mismatch for {schema}/{date}: state={} rows={}",
            state.row_count,
            query_rows.len()
        );
    }
    let rows = query_rows
        .into_iter()
        .map(|row| {
            Ok(OrderRow {
                fkey: row.try_get("fkey")?,
                symbol: row.try_get("symbol")?,
                side: row.try_get("side")?,
                cts: row.try_get("cts")?,
                open_uts: row.try_get("open_uts")?,
                fts: row.try_get("fts")?,
                holding: row.try_get("holding")?,
                holding_close: row.try_get("holding_close")?,
                close_count: row.try_get("close_count")?,
                price: row.try_get("price")?,
                amount: row.try_get("amount")?,
                cprice: row.try_get("cprice")?,
                camount: row.try_get("camount")?,
                range: row.try_get("range")?,
                crange: row.try_get("crange")?,
                tlen: row.try_get("tlen")?,
                pnlu: row.try_get("pnlu")?,
                open_fill_amount: row.try_get("open_fill_amount")?,
                remaining_amount: row.try_get("remaining_amount")?,
                netted_amount: row.try_get("netted_amount")?,
                close_notional: row.try_get("close_notional")?,
                matching_state: row.try_get("matching_state")?,
                open_source_ts_us: row.try_get("open_source_ts_us")?,
                updated_at_us: row.try_get("updated_at_us")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((state, rows))
}

async fn load_one_day_state(
    pool: &PgPool,
    schema: &str,
    date: NaiveDate,
) -> Result<Option<DayState>> {
    let (start_us, end_us) = day_bounds(date)?;
    let sql = format!(
        "SELECT COUNT(*)::bigint AS row_count,\
         (EXTRACT(EPOCH FROM MAX(updated_at))*1000000)::bigint AS max_updated_us \
         FROM {schema}.intra_orders WHERE cts >= $1 AND cts < $2"
    );
    let row = sqlx::query(AssertSqlSafe(sql))
        .bind(start_us)
        .bind(end_us)
        .fetch_one(pool)
        .await
        .with_context(|| format!("load {schema} matched-order state for {date}"))?;
    decode_day_state(&row)
}

async fn load_one_day_state_executor(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    schema: &str,
    start_us: i64,
    end_us: i64,
) -> Result<Option<DayState>> {
    let sql = format!(
        "SELECT COUNT(*)::bigint AS row_count,\
         (EXTRACT(EPOCH FROM MAX(updated_at))*1000000)::bigint AS max_updated_us \
         FROM {schema}.intra_orders WHERE cts >= $1 AND cts < $2"
    );
    let row = sqlx::query(AssertSqlSafe(sql))
        .bind(start_us)
        .bind(end_us)
        .fetch_one(&mut **tx)
        .await
        .context("load matched-order snapshot state")?;
    decode_day_state(&row)
}

fn decode_day_state(row: &sqlx::postgres::PgRow) -> Result<Option<DayState>> {
    let row_count: i64 = row.try_get("row_count")?;
    if row_count == 0 {
        return Ok(None);
    }
    Ok(Some(DayState {
        row_count,
        max_updated_us: row.try_get("max_updated_us")?,
    }))
}

fn rows_to_frame(rows: Vec<OrderRow>) -> Result<DataFrame> {
    let mut fkey = Vec::with_capacity(rows.len());
    let mut symbol = Vec::with_capacity(rows.len());
    let mut side = Vec::with_capacity(rows.len());
    let mut cts = Vec::with_capacity(rows.len());
    let mut open_uts = Vec::with_capacity(rows.len());
    let mut fts = Vec::with_capacity(rows.len());
    let mut holding = Vec::with_capacity(rows.len());
    let mut holding_close = Vec::with_capacity(rows.len());
    let mut close_count = Vec::with_capacity(rows.len());
    let mut price = Vec::with_capacity(rows.len());
    let mut amount = Vec::with_capacity(rows.len());
    let mut cprice = Vec::with_capacity(rows.len());
    let mut camount = Vec::with_capacity(rows.len());
    let mut range = Vec::with_capacity(rows.len());
    let mut crange = Vec::with_capacity(rows.len());
    let mut tlen = Vec::with_capacity(rows.len());
    let mut pnlu = Vec::with_capacity(rows.len());
    let mut open_fill_amount = Vec::with_capacity(rows.len());
    let mut remaining_amount = Vec::with_capacity(rows.len());
    let mut netted_amount = Vec::with_capacity(rows.len());
    let mut close_notional = Vec::with_capacity(rows.len());
    let mut matching_state = Vec::with_capacity(rows.len());
    let mut open_source_ts_us = Vec::with_capacity(rows.len());
    let mut updated_at_us = Vec::with_capacity(rows.len());

    for row in rows {
        fkey.push(row.fkey);
        symbol.push(row.symbol);
        side.push(row.side);
        cts.push(row.cts);
        open_uts.push(row.open_uts);
        fts.push(row.fts);
        holding.push(row.holding);
        holding_close.push(row.holding_close);
        close_count.push(row.close_count);
        price.push(row.price);
        amount.push(row.amount);
        cprice.push(row.cprice);
        camount.push(row.camount);
        range.push(row.range);
        crange.push(row.crange);
        tlen.push(row.tlen);
        pnlu.push(row.pnlu);
        open_fill_amount.push(row.open_fill_amount);
        remaining_amount.push(row.remaining_amount);
        netted_amount.push(row.netted_amount);
        close_notional.push(row.close_notional);
        matching_state.push(row.matching_state);
        open_source_ts_us.push(row.open_source_ts_us);
        updated_at_us.push(row.updated_at_us);
    }

    DataFrame::new(vec![
        Series::new("fkey".into(), fkey),
        Series::new("symbol".into(), symbol),
        Series::new("side".into(), side),
        Series::new("cts".into(), cts),
        Series::new("open_uts".into(), open_uts),
        Series::new("fts".into(), fts),
        Series::new("holding".into(), holding),
        Series::new("holding_close".into(), holding_close),
        Series::new("close_count".into(), close_count),
        Series::new("price".into(), price),
        Series::new("amount".into(), amount),
        Series::new("cprice".into(), cprice),
        Series::new("camount".into(), camount),
        Series::new("range".into(), range),
        Series::new("crange".into(), crange),
        Series::new("tlen".into(), tlen),
        Series::new("pnlu".into(), pnlu),
        Series::new("open_fill_amount".into(), open_fill_amount),
        Series::new("remaining_amount".into(), remaining_amount),
        Series::new("netted_amount".into(), netted_amount),
        Series::new("close_notional".into(), close_notional),
        Series::new("matching_state".into(), matching_state),
        Series::new("open_source_ts_us".into(), open_source_ts_us),
        Series::new("updated_at_us".into(), updated_at_us),
    ])
    .context("build matched-order DataFrame")
}

fn write_parquet_atomic(path: &Path, frame: &mut DataFrame) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("output path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .with_context(|| format!("invalid output file name: {}", path.display()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", process::id()));
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)
            .with_context(|| format!("create temporary Parquet {}", temporary.display()))?;
        ParquetWriter::new(&mut file)
            .finish(frame)
            .with_context(|| format!("write temporary Parquet {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary Parquet {}", temporary.display()))?;
        fs::rename(&temporary, path).with_context(|| {
            format!(
                "replace Parquet {} with {}",
                path.display(),
                temporary.display()
            )
        })?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_export_state_atomic(path: &Path, state: DayState) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("state path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .with_context(|| format!("invalid state file name: {}", path.display()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", process::id()));
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)
            .with_context(|| format!("create export state {}", temporary.display()))?;
        writeln!(file, "{} {}", state.row_count, state.max_updated_us)
            .with_context(|| format!("write export state {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync export state {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace export state {}", path.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_export_state(path: &Path) -> Result<Option<DayState>> {
    let value = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut fields = value.split_whitespace();
    let row_count = fields.next().and_then(|field| field.parse::<i64>().ok());
    let max_updated_us = fields.next().and_then(|field| field.parse::<i64>().ok());
    if fields.next().is_some() || row_count.is_none() || max_updated_us.is_none() {
        return Ok(None);
    }
    Ok(Some(DayState {
        row_count: row_count.unwrap(),
        max_updated_us: max_updated_us.unwrap(),
    }))
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open directory {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

fn day_bounds(date: NaiveDate) -> Result<(i64, i64)> {
    let start = date
        .and_hms_opt(0, 0, 0)
        .context("construct UTC day start")?
        .and_utc()
        .timestamp_micros();
    let end = date
        .checked_add_signed(TimeDelta::days(1))
        .context("trade date overflows")?
        .and_hms_opt(0, 0, 0)
        .context("construct UTC day end")?
        .and_utc()
        .timestamp_micros();
    Ok((start, end))
}

fn date_from_utc_day(utc_day: i64) -> Result<NaiveDate> {
    let timestamp_us = utc_day
        .checked_mul(MICROS_PER_DAY)
        .context("UTC day overflows microseconds")?;
    chrono::DateTime::from_timestamp_micros(timestamp_us)
        .map(|timestamp| timestamp.date_naive())
        .with_context(|| format!("invalid UTC day {utc_day}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_configured_aliases_for_directories() {
        assert_eq!(normalize_alias("binance mt").unwrap(), "binance-mt");
        assert_eq!(normalize_alias(" bybit__cta ").unwrap(), "bybit-cta");
        assert!(normalize_alias("../bybit").is_err());
    }

    #[test]
    fn parses_and_bounds_utc_trade_dates() {
        let date = parse_trade_date("20260731").unwrap();
        assert_eq!(
            day_bounds(date).unwrap(),
            (1_785_456_000_000_000, 1_785_542_400_000_000)
        );
        assert!(parse_trade_date("2026-07-31").is_err());
    }

    #[test]
    fn converts_utc_day_numbers_to_dates() {
        let date = parse_trade_date("20260731").unwrap();
        let (start, _) = day_bounds(date).unwrap();
        assert_eq!(date_from_utc_day(start / MICROS_PER_DAY).unwrap(), date);
    }
}
