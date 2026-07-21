use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use clap::Parser;
use crypto_nav_manager::{
    exchange::binance::{BinanceAccountMode, BinanceClient, BinanceCredentials},
    models::TimeRange,
    rest_dispatcher::{Dispatcher, DispatcherConfig},
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{
    AssertSqlSafe, PgPool, Postgres, QueryBuilder,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const BATCH_SIZE: usize = 1_000;
const ROLLING_OVERLAP_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Parser)]
#[command(about = "Backfill or incrementally sync Binance Portfolio Margin UM funding fees")]
struct Args {
    /// Strategy slug registered in strategy_envs, such as binance_fr_arb01.
    #[arg(long)]
    strategy: String,

    /// Source IP used for Binance REST requests. May be supplied more than once.
    #[arg(long)]
    local_ip: Vec<IpAddr>,

    /// Ignore the database cursor and request from the strategy's st_ms.
    #[arg(long)]
    full: bool,

    /// Optional inclusive end timestamp. Defaults to the current time.
    #[arg(long)]
    end_ms: Option<i64>,

    /// Export all stored rows to LiangTorch daily CSV files without calling Binance.
    #[arg(long)]
    export_csv_only: bool,

    /// Overrides the strategy's registered LiangTorch funding CSV directory.
    #[arg(long)]
    csv_dir: Option<PathBuf>,

    /// Overrides CRYPTO_NAV_DATABASE_URL when provided.
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Debug)]
struct StrategyStorage {
    schema: String,
    env_path: PathBuf,
    st_ms: i64,
    funding_csv_dir: PathBuf,
    funding_csv_account: String,
}

#[derive(Debug)]
struct FundingRow {
    record_id: String,
    symbol: Option<String>,
    asset: String,
    amount: String,
    amount_usdt: Option<String>,
    event_time_ms: i64,
    raw: Value,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;
    let mut strategy = strategy_storage(&pool, &args.strategy).await?;
    if let Some(csv_dir) = args.csv_dir {
        strategy.funding_csv_dir = csv_dir;
    }

    if args.export_csv_only {
        let rows = load_all_rows(&pool, &strategy.schema).await?;
        let files = write_daily_csv(&strategy, &rows)?;
        println!(
            "CSV export complete: strategy={}, rows={}, files={}, directory={}",
            args.strategy,
            rows.len(),
            files,
            strategy.funding_csv_dir.display()
        );
        pool.close().await;
        return Ok(());
    }
    if args.local_ip.is_empty() {
        bail!("at least one --local-ip is required unless --export-csv-only is used");
    }
    let credentials = read_binance_credentials(&strategy.env_path)?;
    let latest_ms = latest_event_time(&pool, &strategy.schema).await?;
    let end_ms = args.end_ms.unwrap_or_else(now_ms);
    let start_ms = if args.full {
        strategy.st_ms
    } else {
        latest_ms
            .map(|cursor| cursor.saturating_sub(ROLLING_OVERLAP_MS))
            .unwrap_or(strategy.st_ms)
            .max(strategy.st_ms)
    };
    let range = TimeRange::new(start_ms, end_ms).context("validate funding query range")?;

    println!(
        "sync funding: strategy={}, mode=portfolio_margin, endpoint=/papi/v1/um/income",
        args.strategy
    );
    println!(
        "requested range: {} .. {}; database cursor before: {}",
        format_timestamp(Some(start_ms)),
        format_timestamp(Some(end_ms)),
        format_timestamp(latest_ms)
    );

    let dispatcher = Dispatcher::new(DispatcherConfig {
        local_ips: args.local_ip,
        ..DispatcherConfig::default()
    })
    .context("create Binance REST dispatcher")?;
    let client = BinanceClient::new(
        dispatcher,
        BinanceCredentials::new(credentials.0, credentials.1),
        BinanceAccountMode::PortfolioMargin,
    );
    let raw_rows = client
        .funding_fees(range)
        .await
        .context("fetch Binance Portfolio Margin UM funding income")?;
    let rows = raw_rows
        .into_iter()
        .map(normalize_funding_row)
        .collect::<Result<Vec<_>>>()?;

    print_format(&rows);
    let affected = upsert_rows(&pool, &strategy.schema, &rows).await?;
    let csv_rows = load_affected_days(&pool, &strategy.schema, &rows).await?;
    let csv_files = write_daily_csv(&strategy, &csv_rows)?;
    print_progress(&pool, &strategy.schema, rows.len(), affected).await?;
    println!(
        "CSV refresh: rows={}, files={}, directory={}",
        csv_rows.len(),
        csv_files,
        strategy.funding_csv_dir.display()
    );
    pool.close().await;
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

async fn strategy_storage(pool: &PgPool, slug: &str) -> Result<StrategyStorage> {
    let row: Option<(
        String,
        String,
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT db_schema, env_path, st_ms, exchange, account_mode, host, \
                funding_csv_dir, funding_csv_account \
         FROM strategy_envs WHERE slug = $1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("query strategy storage")?;
    let (
        schema,
        env_path,
        st_ms,
        exchange,
        account_mode,
        host,
        funding_csv_dir,
        funding_csv_account,
    ) = row.with_context(|| format!("strategy not found in strategy_envs: {slug}"))?;
    if exchange != "binance" || account_mode != "portfolio_margin" {
        bail!("strategy {slug} is not a Binance Portfolio Margin account");
    }
    if host != "local" {
        bail!("strategy {slug} is registered on host {host}; remote env loading is not supported");
    }
    if !valid_schema(&schema) {
        bail!("invalid PostgreSQL schema name in strategy_envs: {schema}");
    }
    Ok(StrategyStorage {
        schema,
        env_path: PathBuf::from(env_path),
        st_ms,
        funding_csv_dir: PathBuf::from(
            funding_csv_dir.context("strategy has no funding_csv_dir configured")?,
        ),
        funding_csv_account: funding_csv_account
            .context("strategy has no funding_csv_account configured")?,
    })
}

fn read_binance_credentials(path: &Path) -> Result<(String, String)> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("read strategy env file {}", path.display()))?;
    let mut values = HashMap::new();
    for (line_number, original) in contents.lines().enumerate() {
        let mut line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if matches!(name.trim(), "BINANCE_API_KEY" | "BINANCE_API_SECRET") {
            values.insert(
                name.trim().to_string(),
                parse_env_value(value).with_context(|| {
                    format!("parse {} line {}", path.display(), line_number + 1)
                })?,
            );
        }
    }
    let api_key = values
        .remove("BINANCE_API_KEY")
        .context("BINANCE_API_KEY is missing from strategy env file")?;
    let secret = values
        .remove("BINANCE_API_SECRET")
        .context("BINANCE_API_SECRET is missing from strategy env file")?;
    Ok((api_key, secret))
}

fn parse_env_value(value: &str) -> Result<String> {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return Ok(value[1..value.len() - 1].to_string());
        }
    }
    if value.contains(['$', '`']) || value.contains(char::is_whitespace) {
        bail!("credential value must be a literal, optionally quoted")
    }
    Ok(value.to_string())
}

async fn latest_event_time(pool: &PgPool, schema: &str) -> Result<Option<i64>> {
    let sql = format!("SELECT MAX(event_time_ms) FROM {schema}.funding_fees");
    sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
        .fetch_one(pool)
        .await
        .context("query funding cursor")
}

fn normalize_funding_row(raw: Value) -> Result<FundingRow> {
    let income_type = required_string(&raw, "incomeType")?;
    if income_type != "FUNDING_FEE" {
        bail!("unexpected Binance incomeType: {income_type}");
    }
    let record_id = required_string(&raw, "tranId")?;
    let asset = required_string(&raw, "asset")?;
    let amount = required_string(&raw, "income")?;
    let event_time_ms = required_i64(&raw, "time")?;
    if event_time_ms < 0 {
        bail!("negative Binance funding time for tranId {record_id}");
    }
    let symbol = raw
        .get("symbol")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let amount_usdt = (asset == "USDT").then(|| amount.clone());
    Ok(FundingRow {
        record_id,
        symbol,
        asset,
        amount,
        amount_usdt,
        event_time_ms,
        raw,
    })
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(|item| match item {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .with_context(|| format!("Binance funding row is missing {key}"))
}

fn required_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .with_context(|| format!("Binance funding row is missing numeric {key}"))
}

async fn upsert_rows(pool: &PgPool, schema: &str, rows: &[FundingRow]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut transaction = pool.begin().await.context("begin funding transaction")?;
    let mut affected = 0;
    for batch in rows.chunks(BATCH_SIZE) {
        let sql = format!(
            "INSERT INTO {schema}.funding_fees (record_id, symbol, asset, amount, amount_usdt, \
             funding_rate, event_time_ms, raw) "
        );
        let mut query = QueryBuilder::<Postgres>::new(sql);
        query.push_values(batch, |mut values, row| {
            values
                .push_bind(&row.record_id)
                .push_bind(row.symbol.as_deref())
                .push_bind(&row.asset)
                .push("CAST(")
                .push_bind_unseparated(&row.amount)
                .push_unseparated(" AS NUMERIC)")
                .push("CAST(")
                .push_bind_unseparated(row.amount_usdt.as_deref())
                .push_unseparated(" AS NUMERIC)")
                .push("NULL")
                .push_bind(row.event_time_ms)
                .push_bind(&row.raw);
        });
        query.push(
            " ON CONFLICT (record_id) DO UPDATE SET \
             symbol = EXCLUDED.symbol, asset = EXCLUDED.asset, amount = EXCLUDED.amount, \
             amount_usdt = EXCLUDED.amount_usdt, funding_rate = EXCLUDED.funding_rate, \
             event_time_ms = EXCLUDED.event_time_ms, raw = EXCLUDED.raw, \
             fetched_at = CURRENT_TIMESTAMP",
        );
        affected += query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert funding batch")?
            .rows_affected();
    }
    transaction.commit().await.context("commit funding sync")?;
    Ok(affected)
}

async fn load_all_rows(pool: &PgPool, schema: &str) -> Result<Vec<FundingRow>> {
    load_rows(pool, schema, None).await
}

async fn load_affected_days(
    pool: &PgPool,
    schema: &str,
    fetched: &[FundingRow],
) -> Result<Vec<FundingRow>> {
    let Some(first_day_ms) = fetched
        .iter()
        .map(|row| utc_day_start_ms(row.event_time_ms))
        .min()
    else {
        return Ok(Vec::new());
    };
    let last_day_ms = fetched
        .iter()
        .map(|row| utc_day_start_ms(row.event_time_ms))
        .max()
        .expect("non-empty funding rows have a last day");
    load_rows(
        pool,
        schema,
        Some((
            first_day_ms,
            last_day_ms.saturating_add(24 * 60 * 60 * 1_000),
        )),
    )
    .await
}

async fn load_rows(
    pool: &PgPool,
    schema: &str,
    range: Option<(i64, i64)>,
) -> Result<Vec<FundingRow>> {
    let mut sql = format!(
        "SELECT record_id, symbol, asset, COALESCE(raw->>'income', amount::text), \
         CASE WHEN amount_usdt IS NULL THEN NULL ELSE COALESCE(raw->>'income', amount_usdt::text) END, \
         event_time_ms, raw FROM {schema}.funding_fees"
    );
    if range.is_some() {
        sql.push_str(" WHERE event_time_ms >= $1 AND event_time_ms < $2");
    }
    sql.push_str(" ORDER BY event_time_ms, record_id");
    let rows: Vec<(
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        i64,
        Value,
    )> = if let Some((start_ms, end_ms)) = range {
        sqlx::query_as(AssertSqlSafe(sql.as_str()))
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as(AssertSqlSafe(sql.as_str()))
            .fetch_all(pool)
            .await
    }
    .context("load stored funding rows for CSV")?;
    Ok(rows
        .into_iter()
        .map(
            |(record_id, symbol, asset, amount, amount_usdt, event_time_ms, raw)| FundingRow {
                record_id,
                symbol,
                asset,
                amount,
                amount_usdt,
                event_time_ms,
                raw,
            },
        )
        .collect())
}

fn write_daily_csv(strategy: &StrategyStorage, rows: &[FundingRow]) -> Result<usize> {
    let mut daily = BTreeMap::<String, Vec<&FundingRow>>::new();
    for row in rows {
        let timestamp = DateTime::<Utc>::from_timestamp_millis(row.event_time_ms)
            .with_context(|| format!("invalid funding timestamp {}", row.event_time_ms))?;
        daily
            .entry(timestamp.format("%Y-%m-%d").to_string())
            .or_default()
            .push(row);
    }
    fs::create_dir_all(&strategy.funding_csv_dir).with_context(|| {
        format!(
            "create funding CSV directory {}",
            strategy.funding_csv_dir.display()
        )
    })?;
    let beijing = FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is a valid offset");
    for (date, rows) in &daily {
        let path = strategy.funding_csv_dir.join(format!("{date}.csv"));
        let temporary = path.with_extension(format!("csv.tmp.{}", std::process::id()));
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(&temporary)
            .with_context(|| format!("create temporary CSV {}", temporary.display()))?;
        writer.write_record([
            "exchange",
            "account",
            "symbol",
            "asset",
            "amount",
            "amountu",
            "type",
            "record_id",
            "ts",
            "dt_utc",
            "dt_bj",
            "raw",
        ])?;
        for row in rows {
            let timestamp = DateTime::<Utc>::from_timestamp_millis(row.event_time_ms)
                .expect("funding timestamp was validated while grouping");
            writer.serialize(LiangFundingCsvRow {
                exchange: "binance",
                account: &strategy.funding_csv_account,
                symbol: row.symbol.as_deref().unwrap_or(""),
                asset: &row.asset,
                amount: &row.amount,
                amountu: row.amount_usdt.as_deref().unwrap_or(""),
                row_type: "FUNDING_FEE",
                record_id: &row.record_id,
                ts: row.event_time_ms,
                dt_utc: timestamp.to_rfc3339_opts(SecondsFormat::Secs, false),
                dt_bj: timestamp
                    .with_timezone(&beijing)
                    .format("%Y-%m-%dT%H:%M:%S")
                    .to_string(),
                raw: serde_json::to_string(&row.raw).context("serialize funding raw JSON")?,
            })?;
        }
        writer
            .flush()
            .with_context(|| format!("flush temporary CSV {}", temporary.display()))?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("replace funding CSV {}", path.display()))?;
    }
    Ok(daily.len())
}

#[derive(Serialize)]
struct LiangFundingCsvRow<'a> {
    exchange: &'static str,
    account: &'a str,
    symbol: &'a str,
    asset: &'a str,
    amount: &'a str,
    amountu: &'a str,
    #[serde(rename = "type")]
    row_type: &'static str,
    record_id: &'a str,
    ts: i64,
    dt_utc: String,
    dt_bj: String,
    raw: String,
}

fn utc_day_start_ms(timestamp_ms: i64) -> i64 {
    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
    timestamp_ms.div_euclid(DAY_MS) * DAY_MS
}

fn print_format(rows: &[FundingRow]) {
    println!("source format: symbol,incomeType,income,asset,info,time,tranId,tradeId");
    println!(
        "normalized format: record_id,symbol,asset,amount,amount_usdt,funding_rate,event_time_ms,raw"
    );
    for row in rows.iter().take(3) {
        println!(
            "sample: record_id={}, symbol={}, asset={}, amount={}, time={}",
            row.record_id,
            row.symbol.as_deref().unwrap_or(""),
            row.asset,
            row.amount,
            format_timestamp(Some(row.event_time_ms))
        );
    }
}

async fn print_progress(pool: &PgPool, schema: &str, fetched: usize, affected: u64) -> Result<()> {
    let sql = format!(
        "SELECT COUNT(*), COUNT(DISTINCT symbol), MIN(event_time_ms), MAX(event_time_ms) \
         FROM {schema}.funding_fees"
    );
    let (rows, symbols, first_ms, latest_ms): (i64, i64, Option<i64>, Option<i64>) =
        sqlx::query_as(AssertSqlSafe(sql.as_str()))
            .fetch_one(pool)
            .await
            .context("query funding summary")?;
    println!(
        "sync complete: schema={schema}, fetched={fetched}, upserted={affected}, \
         database_rows={rows}, symbols={symbols}"
    );
    println!(
        "database range: {} .. {}",
        format_timestamp(first_ms),
        format_timestamp(latest_ms)
    );
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after Unix epoch")
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn format_timestamp(timestamp_ms: Option<i64>) -> String {
    timestamp_ms
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| "none".to_string())
}

fn valid_schema(schema: &str) -> bool {
    let mut characters = schema.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_portfolio_margin_funding_income() {
        let row = normalize_funding_row(json!({
            "symbol": "BTCUSDT",
            "incomeType": "FUNDING_FEE",
            "income": "-1.25000000",
            "asset": "USDT",
            "info": "",
            "time": 1_700_000_000_000_i64,
            "tranId": "9689322392",
            "tradeId": ""
        }))
        .expect("valid funding row");
        assert_eq!(row.record_id, "9689322392");
        assert_eq!(row.symbol.as_deref(), Some("BTCUSDT"));
        assert_eq!(row.amount, "-1.25000000");
        assert_eq!(row.amount_usdt.as_deref(), Some("-1.25000000"));
    }

    #[test]
    fn rejects_non_funding_income() {
        let error = normalize_funding_row(json!({
            "symbol": "",
            "incomeType": "TRANSFER",
            "income": "1",
            "asset": "USDT",
            "time": 1,
            "tranId": "2"
        }))
        .expect_err("transfer is not funding");
        assert!(error.to_string().contains("TRANSFER"));
    }

    #[test]
    fn parses_literal_env_values() {
        assert_eq!(parse_env_value("'abc123'").unwrap(), "abc123");
        assert_eq!(parse_env_value("\"abc123\"").unwrap(), "abc123");
        assert_eq!(parse_env_value("abc123").unwrap(), "abc123");
        assert!(parse_env_value("$OTHER").is_err());
    }
}
