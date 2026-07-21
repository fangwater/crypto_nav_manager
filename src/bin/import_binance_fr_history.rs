use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Parser;
use serde::Deserialize;
use sqlx::{
    AssertSqlSafe, PgPool, Postgres, QueryBuilder,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

const EXPECTED_HEADERS: [&str; 15] = [
    "sid",
    "key",
    "symbol",
    "id",
    "orderId",
    "side",
    "price",
    "qty",
    "amountu",
    "fees",
    "commissionAsset",
    "realizedPnl",
    "ts",
    "ttype",
    "positionSide",
];
const BATCH_SIZE: usize = 1_000;

#[derive(Debug, Parser)]
#[command(about = "One-time import of Binance FR Liang Torch trade CSV history")]
struct Args {
    /// Strategy slug registered in strategy_envs, such as binance_fr_arb01.
    #[arg(long)]
    strategy: String,

    /// Overrides the strategy's registered csv_output_dir.
    #[arg(long)]
    csv_dir: Option<PathBuf>,

    /// Overrides CRYPTO_NAV_DATABASE_URL when provided.
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TradeRow {
    sid: i16,
    key: String,
    symbol: String,
    id: i64,
    #[serde(rename = "orderId")]
    order_id: i64,
    side: String,
    price: String,
    qty: String,
    amountu: String,
    fees: String,
    #[serde(rename = "commissionAsset")]
    commission_asset: String,
    #[serde(rename = "realizedPnl")]
    realized_pnl: Option<String>,
    ts: i64,
    ttype: String,
    #[serde(rename = "positionSide")]
    position_side: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let database_url = args
        .database_url
        .or_else(|| env::var("CRYPTO_NAV_DATABASE_URL").ok());
    let pool_options = PgPoolOptions::new().max_connections(2);
    let pool = match database_url {
        Some(url) => pool_options
            .connect(&url)
            .await
            .context("connect PostgreSQL using CRYPTO_NAV_DATABASE_URL")?,
        None => pool_options
            .connect_with(
                PgConnectOptions::new()
                    .host("/var/run/postgresql")
                    .database("crypto_nav_manager")
                    .username("ubuntu"),
            )
            .await
            .context("connect local PostgreSQL")?,
    };

    let (schema, registered_csv_dir) = strategy_storage(&pool, &args.strategy).await?;
    let csv_dir = args
        .csv_dir
        .unwrap_or_else(|| PathBuf::from(registered_csv_dir));
    let files = find_csv_files(&csv_dir)?;
    let rows = read_and_validate(&files)?;

    import_rows(&pool, &schema, &rows).await?;
    print_progress(&pool, &schema, files.len(), rows.len()).await?;
    Ok(())
}

async fn strategy_storage(pool: &PgPool, strategy: &str) -> Result<(String, String)> {
    let storage: Option<(String, String)> =
        sqlx::query_as("SELECT db_schema, csv_output_dir FROM strategy_envs WHERE slug = $1")
            .bind(strategy)
            .fetch_optional(pool)
            .await
            .context("query strategy storage")?;
    let (schema, csv_output_dir) =
        storage.with_context(|| format!("strategy not found in strategy_envs: {strategy}"))?;

    const SUPPORTED_SCHEMAS: [&str; 4] = [
        "binance_fr_arb01",
        "binance_fr_arb02",
        "binance_fr_arb03",
        "binance_fr_arb04",
    ];
    if !SUPPORTED_SCHEMAS.contains(&schema.as_str()) {
        bail!("strategy {strategy} is not a supported Binance FR profile");
    }

    Ok((schema, csv_output_dir))
}

fn find_csv_files(csv_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(csv_dir)
        .with_context(|| format!("read CSV directory {}", csv_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("trades_") && name.ends_with(".csv"))
        })
        .collect::<Vec<_>>();
    files.sort();

    if files.is_empty() {
        bail!("no trades_*.csv files found in {}", csv_dir.display());
    }
    Ok(files)
}

fn read_and_validate(files: &[PathBuf]) -> Result<Vec<TradeRow>> {
    let mut rows = Vec::new();
    let mut keys = HashSet::new();

    for file in files {
        let mut reader =
            csv::Reader::from_path(file).with_context(|| format!("open CSV {}", file.display()))?;
        let headers = reader
            .headers()
            .with_context(|| format!("read header from {}", file.display()))?
            .clone();
        let actual_headers = headers.iter().collect::<Vec<_>>();
        if actual_headers != EXPECTED_HEADERS {
            bail!(
                "unexpected header in {}\nexpected: {}\nactual:   {}",
                file.display(),
                EXPECTED_HEADERS.join(","),
                headers.iter().collect::<Vec<_>>().join(",")
            );
        }

        let before = rows.len();
        for (index, result) in reader.deserialize::<TradeRow>().enumerate() {
            let row = result
                .with_context(|| format!("parse {} data row {}", file.display(), index + 1))?;
            validate_row(&row, file, index + 1)?;

            let unique_key = (row.key.clone(), row.symbol.clone(), row.id);
            if !keys.insert(unique_key) {
                bail!(
                    "duplicate (key, symbol, id) in source history at {} data row {}",
                    file.display(),
                    index + 1
                );
            }
            rows.push(row);
        }
        println!(
            "validated {} rows from {}",
            rows.len() - before,
            file.display()
        );
    }

    Ok(rows)
}

fn validate_row(row: &TradeRow, file: &Path, row_number: usize) -> Result<()> {
    let market_matches_sid =
        (row.key == "binancespot" && row.sid == 1) || (row.key == "binanceswap" && row.sid == 0);
    if !market_matches_sid {
        bail!(
            "invalid sid/key pair in {} data row {}: sid={}, key={}",
            file.display(),
            row_number,
            row.sid,
            row.key
        );
    }
    if row.ts < 0 {
        bail!(
            "negative ts in {} data row {}: {}",
            file.display(),
            row_number,
            row.ts
        );
    }
    Ok(())
}

async fn import_rows(pool: &PgPool, schema: &str, rows: &[TradeRow]) -> Result<()> {
    let mut transaction = pool.begin().await.context("begin import transaction")?;

    for batch in rows.chunks(BATCH_SIZE) {
        let insert_sql = format!(
            "INSERT INTO {schema}.trades (\
             sid, key, symbol, id, \"orderId\", side, price, qty, amountu, fees, \
             \"commissionAsset\", \"realizedPnl\", ts, ttype, \"positionSide\") "
        );
        let mut query = QueryBuilder::<Postgres>::new(insert_sql);
        query.push_values(batch, |mut values, row| {
            values
                .push_bind(row.sid)
                .push_bind(&row.key)
                .push_bind(&row.symbol)
                .push_bind(row.id)
                .push_bind(row.order_id)
                .push_bind(&row.side)
                .push("CAST(")
                .push_bind_unseparated(&row.price)
                .push_unseparated(" AS NUMERIC)")
                .push("CAST(")
                .push_bind_unseparated(&row.qty)
                .push_unseparated(" AS NUMERIC)")
                .push("CAST(")
                .push_bind_unseparated(&row.amountu)
                .push_unseparated(" AS NUMERIC)")
                .push("CAST(")
                .push_bind_unseparated(&row.fees)
                .push_unseparated(" AS NUMERIC)")
                .push_bind(&row.commission_asset)
                .push("CAST(")
                .push_bind_unseparated(row.realized_pnl.as_deref())
                .push_unseparated(" AS NUMERIC)")
                .push_bind(row.ts)
                .push_bind(&row.ttype)
                .push_bind(&row.position_side);
        });
        query.push(
            " ON CONFLICT (key, symbol, id) DO UPDATE SET \
             sid = EXCLUDED.sid, \
             \"orderId\" = EXCLUDED.\"orderId\", \
             side = EXCLUDED.side, \
             price = EXCLUDED.price, \
             qty = EXCLUDED.qty, \
             amountu = EXCLUDED.amountu, \
             fees = EXCLUDED.fees, \
             \"commissionAsset\" = EXCLUDED.\"commissionAsset\", \
             \"realizedPnl\" = EXCLUDED.\"realizedPnl\", \
             ts = EXCLUDED.ts, \
             ttype = EXCLUDED.ttype, \
             \"positionSide\" = EXCLUDED.\"positionSide\"",
        );
        query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert trade batch")?;
    }

    transaction.commit().await.context("commit import")?;
    Ok(())
}

async fn print_progress(
    pool: &PgPool,
    schema: &str,
    file_count: usize,
    source_rows: usize,
) -> Result<()> {
    let summary_sql = format!(
        "SELECT COUNT(*), COUNT(DISTINCT (key, symbol)), MIN(ts), MAX(ts) \
         FROM {schema}.trades"
    );
    let (database_rows, cursor_count, first_ts, latest_ts): (i64, i64, Option<i64>, Option<i64>) =
        sqlx::query_as(AssertSqlSafe(summary_sql.as_str()))
            .fetch_one(pool)
            .await
            .context("query import summary")?;

    println!(
        "import complete: strategy={schema}, files={file_count}, source_rows={source_rows}, \
         database_rows={database_rows}, cursors={cursor_count}"
    );
    println!(
        "event range: {} .. {}",
        format_timestamp(first_ts),
        format_timestamp(latest_ts)
    );

    let market_sql = format!(
        "SELECT key, COUNT(*), COUNT(DISTINCT symbol), MAX(ts) \
         FROM {schema}.trades GROUP BY key ORDER BY key"
    );
    let markets: Vec<(String, i64, i64, Option<i64>)> =
        sqlx::query_as(AssertSqlSafe(market_sql.as_str()))
            .fetch_all(pool)
            .await
            .context("query market progress")?;
    for (key, row_count, symbols, ts) in markets {
        println!(
            "{key}: rows={row_count}, symbols={symbols}, latest={}",
            format_timestamp(ts)
        );
    }
    Ok(())
}

fn format_timestamp(timestamp_ms: Option<i64>) -> String {
    timestamp_ms
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| "none".to_string())
}
