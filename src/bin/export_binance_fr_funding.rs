use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use clap::Parser;
use serde::Serialize;
use serde_json::Value;
use sqlx::{
    AssertSqlSafe, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::BTreeMap, env, path::Path};

const EXPECTED_SCHEMA_PREFIX: &str = "binance_fr_arb";

#[derive(Debug, Parser)]
#[command(about = "Export one Binance FR strategy's funding history to the current directory")]
struct Args {
    /// Strategy slug registered in strategy_envs, such as binance_fr_arb01.
    #[arg(long)]
    strategy: String,

    /// Overrides CRYPTO_NAV_DATABASE_URL when provided.
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Debug)]
struct Strategy {
    schema: String,
    exchange: String,
    account: String,
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

#[derive(Serialize)]
struct LiangFundingRow<'a> {
    exchange: &'a str,
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;

    let strategy = load_strategy(&pool, &args.strategy).await?;
    let rows = load_rows(&pool, &strategy.schema).await?;
    let current_dir = env::current_dir().context("resolve current directory")?;
    let files = write_daily_csv(&current_dir, &strategy, &rows)?;

    println!(
        "funding export complete: strategy={}, rows={}, files={}, directory={}",
        args.strategy,
        rows.len(),
        files,
        current_dir.display()
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

async fn load_strategy(pool: &PgPool, slug: &str) -> Result<Strategy> {
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT db_schema, exchange, account_mode, alias, host \
         FROM strategy_envs WHERE slug = $1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("query strategy index")?;
    let (schema, exchange, account_mode, alias, host) =
        row.with_context(|| format!("strategy not found in strategy_envs: {slug}"))?;

    if exchange != "binance" || account_mode != "portfolio_margin" {
        bail!("strategy {slug} is not a Binance Portfolio Margin account");
    }
    if host != "local" {
        bail!("strategy {slug} is registered on host {host}");
    }
    if !schema.starts_with(EXPECTED_SCHEMA_PREFIX) || !valid_schema(&schema) {
        bail!("strategy {slug} does not use a Binance FR schema: {schema}");
    }

    Ok(Strategy {
        schema,
        exchange,
        account: alias.replace(' ', "_"),
    })
}

async fn load_rows(pool: &PgPool, schema: &str) -> Result<Vec<FundingRow>> {
    let sql = format!(
        "SELECT record_id, symbol, asset, COALESCE(raw->>'income', amount::text), \
         CASE WHEN amount_usdt IS NULL THEN NULL \
              ELSE COALESCE(raw->>'income', amount_usdt::text) END, \
         event_time_ms, raw \
         FROM {schema}.funding_fees ORDER BY event_time_ms, record_id"
    );
    let rows: Vec<(
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        i64,
        Value,
    )> = sqlx::query_as(AssertSqlSafe(sql.as_str()))
        .fetch_all(pool)
        .await
        .context("load funding rows")?;

    rows.into_iter()
        .map(
            |(record_id, symbol, asset, amount, amount_usdt, event_time_ms, raw)| {
                if DateTime::<Utc>::from_timestamp_millis(event_time_ms).is_none() {
                    bail!("invalid funding timestamp {event_time_ms} for record {record_id}");
                }
                Ok(FundingRow {
                    record_id,
                    symbol,
                    asset,
                    amount,
                    amount_usdt,
                    event_time_ms,
                    raw,
                })
            },
        )
        .collect()
}

fn write_daily_csv(output_dir: &Path, strategy: &Strategy, rows: &[FundingRow]) -> Result<usize> {
    let mut daily = BTreeMap::<String, Vec<&FundingRow>>::new();
    for row in rows {
        let timestamp = DateTime::<Utc>::from_timestamp_millis(row.event_time_ms)
            .expect("funding timestamps were validated while loading");
        daily
            .entry(timestamp.format("%Y-%m-%d").to_string())
            .or_default()
            .push(row);
    }

    let beijing = FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is valid");
    for (date, day_rows) in &daily {
        let path = output_dir.join(format!("funding_{date}.csv"));
        let mut writer = csv::Writer::from_path(&path)
            .with_context(|| format!("create funding CSV {}", path.display()))?;
        for row in day_rows {
            let timestamp = DateTime::<Utc>::from_timestamp_millis(row.event_time_ms)
                .expect("funding timestamps were validated while loading");
            writer.serialize(LiangFundingRow {
                exchange: &strategy.exchange,
                account: &strategy.account,
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
                raw: serde_json::to_string(&row.raw).context("serialize raw funding JSON")?,
            })?;
        }
        writer
            .flush()
            .with_context(|| format!("flush funding CSV {}", path.display()))?;
    }
    Ok(daily.len())
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

    #[test]
    fn liang_account_name_comes_from_alias() {
        assert_eq!("binance nova02".replace(' ', "_"), "binance_nova02");
    }

    #[test]
    fn accepts_only_safe_schema_names() {
        assert!(valid_schema("binance_fr_arb01"));
        assert!(!valid_schema("binance-fr-arb01"));
    }
}
