use super::model::{
    CashStorage, Dataset, Strategy, StrategyClass, TradeStorage, normalize_account,
    valid_identifier,
};
use anyhow::{Context, Result, bail};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::env;

pub(crate) async fn connect_postgres(database_url: Option<&str>) -> Result<PgPool> {
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

pub(crate) async fn load_strategy(pool: &PgPool, slug: &str) -> Result<Strategy> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT db_schema,exchange,alias,strategy_kind FROM strategy_envs WHERE slug=$1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("query strategy_envs")?;
    let (schema, exchange, alias, kind) =
        row.with_context(|| format!("strategy not found: {slug}"))?;
    if !valid_identifier(&schema) {
        bail!("invalid PostgreSQL schema for {slug}: {schema}");
    }
    let class = match kind.as_str() {
        "funding_rate" => StrategyClass::Fr,
        "intra_exchange" => StrategyClass::Intra,
        "market_making" => StrategyClass::Mm,
        value => bail!("unsupported strategy kind for {slug}: {value}"),
    };
    Ok(Strategy {
        slug: slug.to_string(),
        schema,
        exchange: exchange.to_ascii_lowercase(),
        account: normalize_account(&alias),
        class,
    })
}

pub(crate) async fn trade_storage(pool: &PgPool, schema: &str) -> Result<TradeStorage> {
    if column_exists(pool, schema, "trades", "sid").await? {
        return Ok(TradeStorage::Liang);
    }
    for table in ["trades", "trade_fills"] {
        if column_exists(pool, schema, table, "event_time_ms").await? {
            return Ok(TradeStorage::Generic(table.to_string()));
        }
    }
    bail!("{schema} has no supported trade storage")
}

pub(crate) async fn cash_storage(
    pool: &PgPool,
    schema: &str,
    dataset: Dataset,
) -> Result<CashStorage> {
    let primary = dataset.name();
    let generic_fallback = match dataset {
        Dataset::Funding => "funding_fees",
        Dataset::Interest => "borrow_interest",
        _ => bail!("cash storage requested for {}", dataset.name()),
    };
    if table_exists(pool, schema, primary).await? {
        if column_exists(pool, schema, primary, "event_time_ms").await? {
            return Ok(CashStorage::Generic(primary.to_string()));
        }
        let binance_id = if dataset == Dataset::Funding {
            "tranId"
        } else {
            "txId"
        };
        if column_exists(pool, schema, primary, binance_id).await? {
            return Ok(CashStorage::Binance(primary.to_string()));
        }
        if column_exists(pool, schema, primary, "id").await? {
            return Ok(CashStorage::Text(primary.to_string()));
        }
    }
    if column_exists(pool, schema, generic_fallback, "event_time_ms").await? {
        return Ok(CashStorage::Generic(generic_fallback.to_string()));
    }
    bail!("{schema} has no supported {} storage", dataset.name())
}

async fn table_exists(pool: &PgPool, schema: &str, table: &str) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
         WHERE table_schema=$1 AND table_name=$2)",
    )
    .bind(schema)
    .bind(table)
    .fetch_one(pool)
    .await
    .context("inspect history storage table")
}

async fn column_exists(pool: &PgPool, schema: &str, table: &str, column: &str) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns \
         WHERE table_schema=$1 AND table_name=$2 AND column_name=$3)",
    )
    .bind(schema)
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .context("inspect history storage column")
}
