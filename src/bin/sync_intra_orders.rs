use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_nav_manager::intra_order_match::{
    FuturesOrder, HedgeHistory, HedgeResult, HedgeTiming, MarginOrder, MatchEngine, MatchEvent,
    MatchingState, Side,
};
use polars::prelude::*;
use serde::Serialize;
use sqlx::{
    AssertSqlSafe, PgPool, Postgres, QueryBuilder, Row, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::BTreeMap, env, io::Cursor, process::Command, time::Duration};

const SUPPORTED: [&str; 3] = [
    "binance-intra-arb01",
    "bybit-intra-arb01",
    "bybit-intra-arb02",
];

#[derive(Clone, Debug, Parser)]
#[command(about = "Incrementally synthesize intra Margin/Futures order lifecycles")]
struct Args {
    #[arg(long, default_value = "binance-intra-arb01")]
    strategy: String,

    /// Overrides CRYPTO_NAV_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,

    #[arg(long, default_value = "http://127.0.0.1:8822")]
    persist_read_url: String,

    /// Optional verified endpoint override. It can only lower the PG checkpoint.
    #[arg(long)]
    end_ms: Option<i64>,

    #[arg(long, default_value_t = 60)]
    source_chunk_minutes: i64,

    #[arg(long, default_value_t = 10)]
    reorder_minutes: i64,

    #[arg(long, default_value_t = 1e-8)]
    qty_epsilon: f64,

    /// Clears synthesized state and rebuilds this strategy from its aligned source start.
    #[arg(long)]
    rebuild: bool,
}

#[derive(Clone, Debug)]
struct Lifecycle {
    venue: String,
    client_order_id: i64,
    first_source_ts_us: i64,
    last_source_ts_us: i64,
    mkt_ts_us: Option<i64>,
    mkt_source_ts_us: Option<i64>,
    create_ts_us: i64,
    new_ts_us: Option<i64>,
    new_source_ts_us: Option<i64>,
    update_ts_us: i64,
    terminal_ts_us: Option<i64>,
    terminal_ts_local_us: Option<i64>,
    terminal_source_ts_us: Option<i64>,
    last_fill_update_ts_us: Option<i64>,
    symbol: String,
    side: String,
    price: f64,
    price_offset: f64,
    amount_init: f64,
    filled_amount: f64,
    fill_notional: f64,
    status: String,
    from_key: String,
    event_count: i64,
    terminal: bool,
}

#[derive(Debug)]
struct Watermark {
    source_read_through_us: i64,
    events_released_through_us: i64,
}

#[derive(Debug, Serialize)]
struct Summary {
    strategy: String,
    source_start_us: i64,
    source_end_us: i64,
    released_through_us: i64,
    margin_finalized_through_us: i64,
    downloaded_rows: usize,
    lifecycle_orders: usize,
    released_orders: usize,
    margin_orders: usize,
    futures_orders: usize,
    pending_margin_orders: usize,
    unallocated_futures_orders: usize,
    anchor_misses: usize,
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

    let (schema, aligned_from_ms, verified_through_ms) =
        strategy_scope(&pool, &args.strategy).await?;
    if args.rebuild {
        reset_synthesis(&pool, &schema, aligned_from_ms).await?;
    }
    let target_ms = args
        .end_ms
        .unwrap_or(verified_through_ms)
        .min(verified_through_ms);
    let source_end_us = target_ms
        .checked_add(1)
        .and_then(|value| value.checked_mul(1_000))
        .context("verified endpoint overflows microseconds")?;
    let watermark = load_watermark(&pool, &schema).await?;
    let source_start_us = watermark.source_read_through_us;
    if source_end_us < source_start_us {
        bail!("requested endpoint {source_end_us} is behind source cursor {source_start_us}");
    }

    let fetch_args = args.clone();
    let (deltas, downloaded_rows) = tokio::task::spawn_blocking(move || {
        fetch_lifecycle_deltas(&fetch_args, source_start_us, source_end_us)
    })
    .await
    .context("join center order download")??;
    let reorder_us = args
        .reorder_minutes
        .checked_mul(60_000_000)
        .context("reorder window overflow")?;
    let released_through_us = source_end_us
        .saturating_sub(reorder_us)
        .saturating_sub(1)
        .max(watermark.events_released_through_us);

    let mut tx = pool.begin().await.context("begin synthesis transaction")?;
    let locked = lock_watermark(&mut tx, &schema).await?;
    if locked.source_read_through_us != source_start_us {
        bail!(
            "concurrent synthesis advanced source cursor from {source_start_us} to {}",
            locked.source_read_through_us
        );
    }
    upsert_lifecycles(&mut tx, &schema, &deltas).await?;
    let ready = load_ready_lifecycles(&mut tx, &schema, released_through_us).await?;
    let pending = load_pending_orders(&mut tx, &schema).await?;
    let hedge_history = load_hedge_history(&mut tx, &schema, &ready, args.qty_epsilon).await?;
    let (events, margin_count, futures_count) = lifecycle_events(&ready, args.qty_epsilon)?;
    let mut engine = MatchEngine::new(pending, args.qty_epsilon)?;
    engine.seed_hedge_history(hedge_history);
    let hedges = engine.apply(events)?;
    let orders = engine.into_orders();

    upsert_orders(&mut tx, &schema, &orders).await?;
    insert_hedges(&mut tx, &schema, &hedges).await?;
    delete_released_lifecycles(
        &mut tx,
        &schema,
        released_through_us,
        u64::try_from(ready.len()).context("released lifecycle count exceeds u64")?,
    )
    .await?;
    let margin_finalized_through_us =
        margin_finalized_watermark(&mut tx, &schema, released_through_us).await?;
    update_watermark(
        &mut tx,
        &schema,
        source_end_us,
        released_through_us,
        margin_finalized_through_us,
        target_ms,
        reorder_us,
    )
    .await?;
    tx.commit().await.context("commit synthesis transaction")?;

    let summary = Summary {
        strategy: args.strategy,
        source_start_us,
        source_end_us,
        released_through_us,
        margin_finalized_through_us,
        downloaded_rows,
        lifecycle_orders: deltas.len(),
        released_orders: ready.len(),
        margin_orders: margin_count,
        futures_orders: futures_count,
        pending_margin_orders: orders
            .iter()
            .filter(|order| order.matching_state == MatchingState::Pending)
            .count(),
        unallocated_futures_orders: hedges
            .iter()
            .filter(|hedge| hedge.unallocated_amount > args.qty_epsilon)
            .count(),
        anchor_misses: hedges
            .iter()
            .filter(|hedge| hedge.order.main_fkey.is_some() && !hedge.anchor_matched)
            .count(),
    };
    println!("{}", serde_json::to_string(&summary)?);
    pool.close().await;
    Ok(())
}

fn validate_args(args: &Args) -> Result<()> {
    if !SUPPORTED.contains(&args.strategy.as_str()) {
        bail!(
            "unsupported online source {:?}; configured: {}",
            args.strategy,
            SUPPORTED.join(", ")
        );
    }
    if args.source_chunk_minutes <= 0 {
        bail!("--source-chunk-minutes must be positive");
    }
    if args.reorder_minutes < 0 {
        bail!("--reorder-minutes must be non-negative");
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

async fn strategy_scope(pool: &PgPool, strategy: &str) -> Result<(String, i64, i64)> {
    let row = sqlx::query(
        "SELECT s.db_schema,c.aligned_from_ms,c.verified_through_ms FROM strategy_envs s \
         JOIN rocksdb_alignment_checkpoints c ON c.strategy_slug=s.slug \
         WHERE s.slug=$1",
    )
    .bind(strategy)
    .fetch_optional(pool)
    .await
    .context("load strategy synthesis scope")?
    .with_context(|| format!("missing strategy/checkpoint for {strategy}"))?;
    let schema: String = row.try_get("db_schema")?;
    validate_schema(&schema)?;
    Ok((
        schema,
        row.try_get("aligned_from_ms")?,
        row.try_get("verified_through_ms")?,
    ))
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

async fn load_watermark(pool: &PgPool, schema: &str) -> Result<Watermark> {
    let sql = format!(
        "SELECT source_read_through_us,events_released_through_us \
         FROM {schema}.intra_match_watermark WHERE singleton"
    );
    let row = sqlx::query(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .with_context(|| format!("load {schema} synthesis watermark"))?;
    Ok(Watermark {
        source_read_through_us: row.try_get("source_read_through_us")?,
        events_released_through_us: row.try_get("events_released_through_us")?,
    })
}

async fn reset_synthesis(pool: &PgPool, schema: &str, aligned_from_ms: i64) -> Result<()> {
    let start_us = aligned_from_ms
        .checked_mul(1_000)
        .context("aligned source start overflows microseconds")?;
    let mut tx = pool
        .begin()
        .await
        .context("begin synthesis rebuild reset")?;
    let sql = format!("LOCK TABLE {schema}.intra_match_watermark IN EXCLUSIVE MODE");
    sqlx::query(AssertSqlSafe(sql))
        .execute(&mut *tx)
        .await
        .context("lock synthesis watermark table for rebuild")?;
    for table in ["intra_order_lifecycle", "intra_hedges", "intra_orders"] {
        let sql = format!("DELETE FROM {schema}.{table}");
        sqlx::query(AssertSqlSafe(sql))
            .execute(&mut *tx)
            .await
            .with_context(|| format!("clear {schema}.{table} for rebuild"))?;
    }
    let sql = format!(
        "INSERT INTO {schema}.intra_match_watermark (singleton,source_read_through_us,\
         events_released_through_us,margin_finalized_through_us,verified_through_ms,\
         reorder_window_us,updated_at) VALUES (TRUE,$1,$1-1,$1-1,$2,600000000,CURRENT_TIMESTAMP) \
         ON CONFLICT (singleton) DO UPDATE SET source_read_through_us=EXCLUDED.source_read_through_us,\
         events_released_through_us=EXCLUDED.events_released_through_us,\
         margin_finalized_through_us=EXCLUDED.margin_finalized_through_us,\
         verified_through_ms=EXCLUDED.verified_through_ms,updated_at=CURRENT_TIMESTAMP"
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(start_us)
        .bind(aligned_from_ms)
        .execute(&mut *tx)
        .await
        .context("recreate synthesis watermark")?;
    tx.commit()
        .await
        .context("commit synthesis rebuild reset")?;
    Ok(())
}

async fn lock_watermark(tx: &mut Transaction<'_, Postgres>, schema: &str) -> Result<Watermark> {
    let sql = format!(
        "SELECT source_read_through_us,events_released_through_us \
         FROM {schema}.intra_match_watermark WHERE singleton FOR UPDATE"
    );
    let row = sqlx::query(AssertSqlSafe(sql))
        .fetch_one(&mut **tx)
        .await
        .context("lock synthesis watermark")?;
    Ok(Watermark {
        source_read_through_us: row.try_get("source_read_through_us")?,
        events_released_through_us: row.try_get("events_released_through_us")?,
    })
}

fn fetch_lifecycle_deltas(
    args: &Args,
    start_us: i64,
    end_us: i64,
) -> Result<(Vec<Lifecycle>, usize)> {
    if start_us == end_us {
        return Ok((Vec::new(), 0));
    }
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
    let mut lifecycle = BTreeMap::<(String, i64), Lifecycle>::new();
    let mut row_count = 0usize;
    let mut use_curl = false;
    let mut cursor = start_us;
    while cursor < end_us {
        let window_end = cursor.saturating_add(chunk_us).min(end_us);
        let params = [
            ("table", "uniform_orders".to_string()),
            ("source_id", args.strategy.clone()),
            ("start_us", cursor.to_string()),
            ("end_us", window_end.to_string()),
            ("format", "parquet".to_string()),
        ];
        let url = reqwest::Url::parse_with_params(&endpoint, &params)
            .with_context(|| format!("build center URL for {}", args.strategy))?;
        let read_with_curl = || -> Result<Vec<u8>> {
            let output = Command::new("curl")
                .args(["--fail", "--silent", "--show-error", "--max-time", "60"])
                .arg(url.as_str())
                .output()
                .with_context(|| {
                    format!(
                        "run curl for {} uniform_orders {cursor}..{window_end}",
                        args.strategy
                    )
                })?;
            if !output.status.success() {
                bail!(
                    "curl failed for {} uniform_orders {cursor}..{window_end}: {}",
                    args.strategy,
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
                        "center read switching to curl strategy={} table=uniform_orders window={cursor}..{window_end}: {error}",
                        args.strategy
                    );
                    read_with_curl()?
                }
            }
        };
        let frame = ParquetReader::new(Cursor::new(body))
            .finish()
            .context("decode center uniform_orders parquet")?;
        row_count += fold_frame(&frame, &mut lifecycle)?;
        cursor = window_end;
    }
    Ok((lifecycle.into_values().collect(), row_count))
}

fn fold_frame(
    frame: &DataFrame,
    lifecycle: &mut BTreeMap<(String, i64), Lifecycle>,
) -> Result<usize> {
    let source_ts = frame.column("ts_us")?.i64()?;
    let client_id = frame.column("client_order_id")?.i64()?;
    let venue = frame.column("trading_venue")?.str()?;
    let mkt_ts = frame.column("mkt_ts")?.i64()?;
    let create_ts = frame.column("create_ts")?.i64()?;
    let update_ts = frame.column("update_ts")?.i64()?;
    let local_ts = frame.column("local_ts")?.i64()?;
    let symbol = frame.column("symbol")?.str()?;
    let side = frame.column("side")?.str()?;
    let price = frame.column("price")?.f64()?;
    let price_offset = frame.column("price_offset")?.f64()?;
    let amount_init = frame.column("amount_init")?.f64()?;
    let amount_update = frame.column("amount_update")?.f64()?;
    let status = frame.column("status")?.str()?;
    let from_key = frame.column("from_key")?.str()?;
    for row in 0..frame.height() {
        let venue = venue.get(row).context("null trading_venue")?;
        if !matches!(
            venue,
            "BinanceMargin" | "BinanceFutures" | "BybitMargin" | "BybitFutures"
        ) {
            continue;
        }
        let id = client_id.get(row).context("null client_order_id")?;
        let source = source_ts.get(row).context("null ts_us")?;
        let market = positive_timestamp(mkt_ts.get(row).context("null mkt_ts")?);
        let create = create_ts.get(row).context("null create_ts")?;
        let update = update_ts.get(row).context("null update_ts")?;
        let local = local_ts.get(row).context("null local_ts")?;
        let row_status = status.get(row).context("null status")?;
        let fill = amount_update.get(row).context("null amount_update")?;
        if !fill.is_finite() || fill < 0.0 {
            bail!("invalid amount_update {fill} for {venue}/{id}");
        }
        let row_price = price.get(row).context("null price")?;
        let key = (venue.to_string(), id);
        let terminal = is_terminal(row_status);
        let new_event = row_status
            .eq_ignore_ascii_case("NEW")
            .then_some(update)
            .and_then(positive_timestamp);
        let terminal_event = terminal.then_some(update).and_then(positive_timestamp);
        let terminal_local = terminal_event.and_then(|_| positive_timestamp(local));
        let fill_update = fill_update_ts(row_status, fill, update);
        match lifecycle.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Lifecycle {
                    venue: venue.to_string(),
                    client_order_id: id,
                    first_source_ts_us: source,
                    last_source_ts_us: source,
                    mkt_ts_us: market,
                    mkt_source_ts_us: market.map(|_| source),
                    create_ts_us: create,
                    new_ts_us: new_event,
                    new_source_ts_us: new_event.map(|_| source),
                    update_ts_us: update,
                    terminal_ts_us: terminal_event,
                    terminal_ts_local_us: terminal_local,
                    terminal_source_ts_us: terminal_event.map(|_| source),
                    last_fill_update_ts_us: fill_update,
                    symbol: symbol.get(row).context("null symbol")?.to_string(),
                    side: side.get(row).context("null side")?.to_ascii_lowercase(),
                    price: row_price,
                    price_offset: price_offset.get(row).context("null price_offset")?,
                    amount_init: amount_init.get(row).context("null amount_init")?,
                    filled_amount: fill,
                    fill_notional: fill * row_price,
                    status: row_status.to_string(),
                    from_key: from_key.get(row).unwrap_or_default().to_string(),
                    event_count: 1,
                    terminal,
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let aggregate = entry.get_mut();
                let previous_latest = (aggregate.update_ts_us, aggregate.last_source_ts_us);
                aggregate.first_source_ts_us = aggregate.first_source_ts_us.min(source);
                aggregate.last_source_ts_us = aggregate.last_source_ts_us.max(source);
                aggregate.create_ts_us = first_positive_timestamp(aggregate.create_ts_us, create);
                if market.is_some()
                    && should_replace_first_event(aggregate.mkt_source_ts_us, source)
                {
                    aggregate.mkt_ts_us = market;
                    aggregate.mkt_source_ts_us = Some(source);
                }
                if new_event.is_some()
                    && should_replace_first_event(aggregate.new_source_ts_us, source)
                {
                    aggregate.new_ts_us = new_event;
                    aggregate.new_source_ts_us = Some(source);
                }
                if let Some(incoming_terminal_ts) = terminal_event {
                    if should_replace_first_event(aggregate.terminal_source_ts_us, source) {
                        aggregate.terminal_ts_us = Some(incoming_terminal_ts);
                        aggregate.terminal_ts_local_us = terminal_local;
                        aggregate.terminal_source_ts_us = Some(source);
                    } else if aggregate.terminal_ts_us == Some(incoming_terminal_ts) {
                        aggregate.terminal_ts_local_us = earliest_optional_timestamp(
                            aggregate.terminal_ts_local_us,
                            terminal_local,
                        );
                    }
                }
                aggregate.filled_amount += fill;
                aggregate.fill_notional += fill * row_price;
                aggregate.event_count += 1;
                aggregate.terminal |= terminal;
                aggregate.last_fill_update_ts_us =
                    match (aggregate.last_fill_update_ts_us, fill_update) {
                        (Some(current), Some(incoming)) => Some(current.max(incoming)),
                        (current, incoming) => current.or(incoming),
                    };
                if (update, source) >= previous_latest {
                    aggregate.update_ts_us = update;
                    aggregate.symbol = symbol.get(row).context("null symbol")?.to_string();
                    aggregate.side = side.get(row).context("null side")?.to_ascii_lowercase();
                    aggregate.price = row_price;
                    aggregate.price_offset = price_offset.get(row).context("null price_offset")?;
                    aggregate.amount_init = amount_init.get(row).context("null amount_init")?;
                    aggregate.status = row_status.to_string();
                    aggregate.from_key = from_key.get(row).unwrap_or_default().to_string();
                }
            }
        }
    }
    Ok(frame.height())
}

fn positive_timestamp(value: i64) -> Option<i64> {
    (value > 0).then_some(value)
}

fn first_positive_timestamp(current: i64, incoming: i64) -> i64 {
    match (current > 0, incoming > 0) {
        (true, true) => current.min(incoming),
        (false, true) => incoming,
        _ => current,
    }
}

fn earliest_optional_timestamp(current: Option<i64>, incoming: Option<i64>) -> Option<i64> {
    match (current, incoming) {
        (Some(current), Some(incoming)) => Some(current.min(incoming)),
        (current, incoming) => current.or(incoming),
    }
}

fn should_replace_first_event(current_source: Option<i64>, incoming_source: i64) -> bool {
    current_source.map_or(true, |current| incoming_source < current)
}

fn fill_update_ts(status: &str, amount_update: f64, update_ts: i64) -> Option<i64> {
    if amount_update > 0.0 || status.eq_ignore_ascii_case("FILLED") {
        Some(update_ts)
    } else {
        None
    }
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status.to_ascii_uppercase().as_str(),
        "FILLED" | "CANCELED" | "CANCELLED" | "EXPIRED" | "EXPIRED_IN_MATCH" | "REJECTED"
    )
}

async fn upsert_lifecycles(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    rows: &[Lifecycle],
) -> Result<()> {
    for chunk in rows.chunks(2_000) {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "INSERT INTO {schema}.intra_order_lifecycle (trading_venue,client_order_id,\
             first_source_ts_us,last_source_ts_us,mkt_ts_us,mkt_source_ts_us,create_ts_us,\
             new_ts_us,new_source_ts_us,update_ts_us,terminal_ts_us,terminal_ts_local_us,\
             terminal_source_ts_us,last_fill_update_ts_us,symbol,side,price,price_offset,\
             amount_init,filled_amount,fill_notional,status,from_key,event_count,terminal) "
        ));
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(&row.venue)
                .push_bind(row.client_order_id)
                .push_bind(row.first_source_ts_us)
                .push_bind(row.last_source_ts_us)
                .push_bind(row.mkt_ts_us)
                .push_bind(row.mkt_source_ts_us)
                .push_bind(row.create_ts_us)
                .push_bind(row.new_ts_us)
                .push_bind(row.new_source_ts_us)
                .push_bind(row.update_ts_us)
                .push_bind(row.terminal_ts_us)
                .push_bind(row.terminal_ts_local_us)
                .push_bind(row.terminal_source_ts_us)
                .push_bind(row.last_fill_update_ts_us)
                .push_bind(&row.symbol)
                .push_bind(&row.side)
                .push_bind(row.price)
                .push_bind(row.price_offset)
                .push_bind(row.amount_init)
                .push_bind(row.filled_amount)
                .push_bind(row.fill_notional)
                .push_bind(&row.status)
                .push_bind(&row.from_key)
                .push_bind(row.event_count)
                .push_bind(row.terminal);
        });
        query.push(format!(
            " ON CONFLICT (trading_venue,client_order_id) DO UPDATE SET \
             first_source_ts_us=LEAST({schema}.intra_order_lifecycle.first_source_ts_us,EXCLUDED.first_source_ts_us),\
             last_source_ts_us=GREATEST({schema}.intra_order_lifecycle.last_source_ts_us,EXCLUDED.last_source_ts_us),\
             mkt_ts_us=CASE WHEN EXCLUDED.mkt_source_ts_us IS NOT NULL AND \
               ({schema}.intra_order_lifecycle.mkt_source_ts_us IS NULL OR \
                EXCLUDED.mkt_source_ts_us<{schema}.intra_order_lifecycle.mkt_source_ts_us) \
               THEN EXCLUDED.mkt_ts_us ELSE {schema}.intra_order_lifecycle.mkt_ts_us END,\
             mkt_source_ts_us=CASE WHEN EXCLUDED.mkt_source_ts_us IS NOT NULL AND \
               ({schema}.intra_order_lifecycle.mkt_source_ts_us IS NULL OR \
                EXCLUDED.mkt_source_ts_us<{schema}.intra_order_lifecycle.mkt_source_ts_us) \
               THEN EXCLUDED.mkt_source_ts_us ELSE {schema}.intra_order_lifecycle.mkt_source_ts_us END,\
             create_ts_us=CASE \
               WHEN {schema}.intra_order_lifecycle.create_ts_us<=0 AND EXCLUDED.create_ts_us>0 \
                 THEN EXCLUDED.create_ts_us \
               WHEN EXCLUDED.create_ts_us<=0 THEN {schema}.intra_order_lifecycle.create_ts_us \
               ELSE LEAST({schema}.intra_order_lifecycle.create_ts_us,EXCLUDED.create_ts_us) END,\
             new_ts_us=CASE WHEN EXCLUDED.new_source_ts_us IS NOT NULL AND \
               ({schema}.intra_order_lifecycle.new_source_ts_us IS NULL OR \
                EXCLUDED.new_source_ts_us<{schema}.intra_order_lifecycle.new_source_ts_us) \
               THEN EXCLUDED.new_ts_us ELSE {schema}.intra_order_lifecycle.new_ts_us END,\
             new_source_ts_us=CASE WHEN EXCLUDED.new_source_ts_us IS NOT NULL AND \
               ({schema}.intra_order_lifecycle.new_source_ts_us IS NULL OR \
                EXCLUDED.new_source_ts_us<{schema}.intra_order_lifecycle.new_source_ts_us) \
               THEN EXCLUDED.new_source_ts_us ELSE {schema}.intra_order_lifecycle.new_source_ts_us END,\
             update_ts_us=GREATEST({schema}.intra_order_lifecycle.update_ts_us,EXCLUDED.update_ts_us),\
             terminal_ts_us=CASE WHEN EXCLUDED.terminal_source_ts_us IS NOT NULL AND \
               ({schema}.intra_order_lifecycle.terminal_source_ts_us IS NULL OR \
                EXCLUDED.terminal_source_ts_us<{schema}.intra_order_lifecycle.terminal_source_ts_us) \
               THEN EXCLUDED.terminal_ts_us ELSE {schema}.intra_order_lifecycle.terminal_ts_us END,\
             terminal_ts_local_us=CASE \
               WHEN EXCLUDED.terminal_source_ts_us IS NOT NULL AND \
                 ({schema}.intra_order_lifecycle.terminal_source_ts_us IS NULL OR \
                  EXCLUDED.terminal_source_ts_us<{schema}.intra_order_lifecycle.terminal_source_ts_us) \
                 THEN EXCLUDED.terminal_ts_local_us \
               WHEN {schema}.intra_order_lifecycle.terminal_ts_us=EXCLUDED.terminal_ts_us THEN CASE \
                 WHEN {schema}.intra_order_lifecycle.terminal_ts_local_us IS NULL \
                   THEN EXCLUDED.terminal_ts_local_us \
                 WHEN EXCLUDED.terminal_ts_local_us IS NULL \
                   THEN {schema}.intra_order_lifecycle.terminal_ts_local_us \
                 ELSE LEAST({schema}.intra_order_lifecycle.terminal_ts_local_us,\
                            EXCLUDED.terminal_ts_local_us) END \
               ELSE {schema}.intra_order_lifecycle.terminal_ts_local_us END,\
             terminal_source_ts_us=CASE WHEN EXCLUDED.terminal_source_ts_us IS NOT NULL AND \
               ({schema}.intra_order_lifecycle.terminal_source_ts_us IS NULL OR \
                EXCLUDED.terminal_source_ts_us<{schema}.intra_order_lifecycle.terminal_source_ts_us) \
               THEN EXCLUDED.terminal_source_ts_us \
               ELSE {schema}.intra_order_lifecycle.terminal_source_ts_us END,\
             last_fill_update_ts_us=GREATEST(\
               {schema}.intra_order_lifecycle.last_fill_update_ts_us,\
               EXCLUDED.last_fill_update_ts_us),\
             symbol=EXCLUDED.symbol,side=EXCLUDED.side,price=EXCLUDED.price,\
             price_offset=EXCLUDED.price_offset,amount_init=EXCLUDED.amount_init,\
             filled_amount={schema}.intra_order_lifecycle.filled_amount+EXCLUDED.filled_amount,\
             fill_notional={schema}.intra_order_lifecycle.fill_notional+EXCLUDED.fill_notional,\
             status=EXCLUDED.status,from_key=EXCLUDED.from_key,\
             event_count={schema}.intra_order_lifecycle.event_count+EXCLUDED.event_count,\
             terminal={schema}.intra_order_lifecycle.terminal OR EXCLUDED.terminal"
        ));
        query
            .build()
            .execute(&mut **tx)
            .await
            .context("upsert order lifecycle aggregates")?;
    }
    Ok(())
}

async fn load_ready_lifecycles(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    released_through_us: i64,
) -> Result<Vec<Lifecycle>> {
    let sql = format!(
        "SELECT trading_venue,client_order_id,first_source_ts_us,last_source_ts_us,\
         mkt_ts_us,mkt_source_ts_us,create_ts_us,new_ts_us,new_source_ts_us,update_ts_us,\
         terminal_ts_us,terminal_ts_local_us,terminal_source_ts_us,last_fill_update_ts_us,\
         symbol,side,price,price_offset,amount_init,filled_amount,fill_notional,status,from_key,\
         event_count,terminal FROM {schema}.intra_order_lifecycle \
         WHERE terminal AND last_source_ts_us <= $1"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(released_through_us)
        .fetch_all(&mut **tx)
        .await
        .context("load mature lifecycle orders")?;
    rows.into_iter()
        .map(|row| {
            Ok(Lifecycle {
                venue: row.try_get("trading_venue")?,
                client_order_id: row.try_get("client_order_id")?,
                first_source_ts_us: row.try_get("first_source_ts_us")?,
                last_source_ts_us: row.try_get("last_source_ts_us")?,
                mkt_ts_us: row.try_get("mkt_ts_us")?,
                mkt_source_ts_us: row.try_get("mkt_source_ts_us")?,
                create_ts_us: row.try_get("create_ts_us")?,
                new_ts_us: row.try_get("new_ts_us")?,
                new_source_ts_us: row.try_get("new_source_ts_us")?,
                update_ts_us: row.try_get("update_ts_us")?,
                terminal_ts_us: row.try_get("terminal_ts_us")?,
                terminal_ts_local_us: row.try_get("terminal_ts_local_us")?,
                terminal_source_ts_us: row.try_get("terminal_source_ts_us")?,
                last_fill_update_ts_us: row.try_get("last_fill_update_ts_us")?,
                symbol: row.try_get("symbol")?,
                side: row.try_get("side")?,
                price: row.try_get("price")?,
                price_offset: row.try_get("price_offset")?,
                amount_init: row.try_get("amount_init")?,
                filled_amount: row.try_get("filled_amount")?,
                fill_notional: row.try_get("fill_notional")?,
                status: row.try_get("status")?,
                from_key: row.try_get("from_key")?,
                event_count: row.try_get("event_count")?,
                terminal: row.try_get("terminal")?,
            })
        })
        .collect()
}

async fn load_pending_orders(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
) -> Result<Vec<MarginOrder>> {
    let sql = format!(
        "SELECT fkey,symbol,side,cts,open_uts,open_mkt_ts_us,open_new_ts_us,\
         open_terminal_ts_us,open_terminal_ts_local_us,hts,hedge_new_ts_us,\
         hedge_terminal_ts_us,fts,close_count,price,amount,range,tlen,open_fill_amount,\
         remaining_amount,camount,netted_amount,close_notional,open_source_ts_us \
         FROM {schema}.intra_orders WHERE matching_state='pending' \
         ORDER BY open_uts,open_source_ts_us,fkey"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(&mut **tx)
        .await
        .context("load pending Margin snapshot")?;
    rows.into_iter()
        .map(|row| {
            Ok(MarginOrder {
                fkey: row.try_get("fkey")?,
                symbol: row.try_get("symbol")?,
                side: Side::parse(row.try_get::<&str, _>("side")?)?,
                cts: row.try_get("cts")?,
                open_uts: row.try_get("open_uts")?,
                open_mkt_ts_us: row.try_get("open_mkt_ts_us")?,
                open_new_ts_us: row.try_get("open_new_ts_us")?,
                open_terminal_ts_us: row.try_get("open_terminal_ts_us")?,
                open_terminal_ts_local_us: row.try_get("open_terminal_ts_local_us")?,
                hts: row.try_get("hts")?,
                hedge_new_ts_us: row.try_get("hedge_new_ts_us")?,
                hedge_terminal_ts_us: row.try_get("hedge_terminal_ts_us")?,
                fts: row.try_get("fts")?,
                close_count: row.try_get("close_count")?,
                price: row.try_get("price")?,
                amount: row.try_get("amount")?,
                range: row.try_get("range")?,
                tlen: row.try_get("tlen")?,
                open_fill_amount: row.try_get("open_fill_amount")?,
                remaining_amount: row.try_get("remaining_amount")?,
                camount: row.try_get("camount")?,
                netted_amount: row.try_get("netted_amount")?,
                close_notional: row.try_get("close_notional")?,
                matching_state: MatchingState::Pending,
                open_source_ts_us: row.try_get("open_source_ts_us")?,
            })
        })
        .collect()
}

async fn load_hedge_history(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    ready: &[Lifecycle],
    epsilon: f64,
) -> Result<Vec<HedgeHistory>> {
    let mut fkeys = ready
        .iter()
        .filter(|row| row.venue.ends_with("Margin") && row.filled_amount > epsilon)
        .map(|row| row.client_order_id)
        .collect::<Vec<_>>();
    fkeys.sort_unstable();
    fkeys.dedup();
    if fkeys.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "WITH hedge_ranked AS (\
           SELECT main_fkey,client_order_id,create_ts_us,new_ts_us,terminal_ts_us,\
                  source_ts_us,MAX(fill_ts_us) OVER (PARTITION BY main_fkey) AS fts,\
                  COUNT(*) OVER (PARTITION BY main_fkey) AS close_count,\
                  ROW_NUMBER() OVER (PARTITION BY main_fkey \
                    ORDER BY create_ts_us,source_ts_us,client_order_id) AS hedge_rank \
           FROM {schema}.intra_hedges WHERE main_fkey=ANY($1)\
         ) SELECT main_fkey,client_order_id,create_ts_us,new_ts_us,terminal_ts_us,\
                  source_ts_us,fts,close_count \
           FROM hedge_ranked WHERE hedge_rank=1"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(&fkeys)
        .fetch_all(&mut **tx)
        .await
        .context("load historical Futures hedge summaries")?;
    rows.into_iter()
        .map(|row| {
            Ok(HedgeHistory {
                main_fkey: row.try_get("main_fkey")?,
                first_order: HedgeTiming {
                    client_order_id: row.try_get("client_order_id")?,
                    create_ts_us: row.try_get("create_ts_us")?,
                    new_ts_us: row.try_get("new_ts_us")?,
                    terminal_ts_us: row.try_get("terminal_ts_us")?,
                    source_ts_us: row.try_get("source_ts_us")?,
                },
                last_fill_ts_us: row.try_get("fts")?,
                order_count: row.try_get("close_count")?,
            })
        })
        .collect()
}

fn lifecycle_events(rows: &[Lifecycle], epsilon: f64) -> Result<(Vec<MatchEvent>, usize, usize)> {
    let mut events = Vec::new();
    let mut margins = 0usize;
    let mut futures = 0usize;
    for row in rows {
        let side = Side::parse(&row.side)?;
        if row.venue.ends_with("Margin") {
            if row.filled_amount <= epsilon {
                continue;
            }
            let open_uts = row.last_fill_update_ts_us.with_context(|| {
                format!(
                    "filled Margin order {}/{} has no FILLED or PARTIALLY_FILLED timestamp",
                    row.venue, row.client_order_id
                )
            })?;
            margins += 1;
            events.push(MatchEvent::Margin(MarginOrder {
                fkey: row.client_order_id,
                symbol: row.symbol.clone(),
                side,
                cts: row.create_ts_us,
                open_uts,
                open_mkt_ts_us: row.mkt_ts_us,
                open_new_ts_us: row.new_ts_us,
                open_terminal_ts_us: row.terminal_ts_us,
                open_terminal_ts_local_us: row.terminal_ts_local_us,
                hts: None,
                hedge_new_ts_us: None,
                hedge_terminal_ts_us: None,
                fts: None,
                close_count: 0,
                price: row.price,
                amount: row.amount_init,
                range: row.price_offset * 10_000.0,
                tlen: parse_tlen(&row.from_key).map(|quantity| quantity * row.price),
                open_fill_amount: row.filled_amount,
                remaining_amount: row.filled_amount,
                camount: 0.0,
                netted_amount: 0.0,
                close_notional: 0.0,
                matching_state: MatchingState::Pending,
                open_source_ts_us: row.last_source_ts_us,
            }));
        } else if row.venue.ends_with("Futures") {
            let fill_ts_us = if row.filled_amount > epsilon {
                Some(row.last_fill_update_ts_us.with_context(|| {
                    format!(
                        "filled Futures order {}/{} has no FILLED or PARTIALLY_FILLED timestamp",
                        row.venue, row.client_order_id
                    )
                })?)
            } else {
                None
            };
            futures += 1;
            events.push(MatchEvent::Futures(FuturesOrder {
                client_order_id: row.client_order_id,
                main_fkey: parse_main_fkey(&row.from_key),
                symbol: row.symbol.clone(),
                side,
                create_ts_us: row.create_ts_us,
                update_ts_us: row.update_ts_us,
                new_ts_us: row.new_ts_us,
                terminal_ts_us: row.terminal_ts_us,
                fill_ts_us,
                source_ts_us: row.last_source_ts_us,
                amount: row.filled_amount,
                cprice: (row.filled_amount > epsilon)
                    .then(|| row.fill_notional / row.filled_amount),
                event_count: row.event_count,
            }));
        }
    }
    Ok((events, margins, futures))
}

fn parse_tlen(from_key: &str) -> Option<f64> {
    from_key
        .rsplit_once(":tlen=")
        .and_then(|(_, value)| value.parse().ok())
}

fn parse_main_fkey(from_key: &str) -> Option<i64> {
    from_key.split('|').next()?.parse().ok()
}

async fn upsert_orders(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    rows: &[MarginOrder],
) -> Result<()> {
    for chunk in rows.chunks(1_500) {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "INSERT INTO {schema}.intra_orders (fkey,symbol,side,cts,open_uts,open_mkt_ts_us,\
             open_new_ts_us,open_terminal_ts_us,open_terminal_ts_local_us,hts,hedge_new_ts_us,\
             hedge_terminal_ts_us,fts,holding,holding_close,close_count,price,amount,cprice,\
             camount,range,crange,tlen,pnlu,open_fill_amount,remaining_amount,netted_amount,\
             close_notional,matching_state,open_source_ts_us,updated_at) "
        ));
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(row.fkey)
                .push_bind(&row.symbol)
                .push_bind(row.side.as_str())
                .push_bind(row.cts)
                .push_bind(row.open_uts)
                .push_bind(row.open_mkt_ts_us)
                .push_bind(row.open_new_ts_us)
                .push_bind(row.open_terminal_ts_us)
                .push_bind(row.open_terminal_ts_local_us)
                .push_bind(row.hts)
                .push_bind(row.hedge_new_ts_us)
                .push_bind(row.hedge_terminal_ts_us)
                .push_bind(row.fts)
                .push_bind(row.holding())
                .push_bind(row.holding_close())
                .push_bind(row.close_count)
                .push_bind(row.price)
                .push_bind(row.amount)
                .push_bind(row.cprice())
                .push_bind(row.camount)
                .push_bind(row.range)
                .push_bind(-1.0_f64)
                .push_bind(row.tlen)
                .push_bind(row.pnlu())
                .push_bind(row.open_fill_amount)
                .push_bind(row.remaining_amount)
                .push_bind(row.netted_amount)
                .push_bind(row.close_notional)
                .push_bind(row.matching_state.as_str())
                .push_bind(row.open_source_ts_us)
                .push("CURRENT_TIMESTAMP");
        });
        query.push(
            " ON CONFLICT (fkey) DO UPDATE SET \
             open_mkt_ts_us=EXCLUDED.open_mkt_ts_us,open_new_ts_us=EXCLUDED.open_new_ts_us,\
             open_terminal_ts_us=EXCLUDED.open_terminal_ts_us,\
             open_terminal_ts_local_us=EXCLUDED.open_terminal_ts_local_us,\
             hts=EXCLUDED.hts,hedge_new_ts_us=EXCLUDED.hedge_new_ts_us,\
             hedge_terminal_ts_us=EXCLUDED.hedge_terminal_ts_us,fts=EXCLUDED.fts,\
             holding_close=EXCLUDED.holding_close,close_count=EXCLUDED.close_count,\
             cprice=EXCLUDED.cprice,camount=EXCLUDED.camount,pnlu=EXCLUDED.pnlu,\
             remaining_amount=EXCLUDED.remaining_amount,netted_amount=EXCLUDED.netted_amount,\
             close_notional=EXCLUDED.close_notional,matching_state=EXCLUDED.matching_state,\
             updated_at=CURRENT_TIMESTAMP",
        );
        query
            .build()
            .execute(&mut **tx)
            .await
            .context("upsert synthesized intra orders")?;
    }
    Ok(())
}

async fn insert_hedges(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    rows: &[HedgeResult],
) -> Result<()> {
    for chunk in rows.chunks(2_000) {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "INSERT INTO {schema}.intra_hedges (client_order_id,main_fkey,symbol,side,\
             create_ts_us,new_ts_us,terminal_ts_us,update_ts_us,fill_ts_us,source_ts_us,\
             amount,cprice,event_count,allocated_amount,unallocated_amount,anchor_matched) "
        ));
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(row.order.client_order_id)
                .push_bind(row.order.main_fkey)
                .push_bind(&row.order.symbol)
                .push_bind(row.order.side.as_str())
                .push_bind(row.order.create_ts_us)
                .push_bind(row.order.new_ts_us)
                .push_bind(row.order.terminal_ts_us)
                .push_bind(row.order.update_ts_us)
                .push_bind(row.order.fill_ts_us)
                .push_bind(row.order.source_ts_us)
                .push_bind(row.order.amount)
                .push_bind(row.order.cprice)
                .push_bind(row.order.event_count)
                .push_bind(row.allocated_amount)
                .push_bind(row.unallocated_amount)
                .push_bind(row.anchor_matched);
        });
        query.push(" ON CONFLICT (client_order_id) DO NOTHING");
        query
            .build()
            .execute(&mut **tx)
            .await
            .context("insert processed Futures hedge orders")?;
    }
    Ok(())
}

async fn delete_released_lifecycles(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    released_through_us: i64,
    expected: u64,
) -> Result<()> {
    let sql = format!(
        "DELETE FROM {schema}.intra_order_lifecycle \
         WHERE terminal AND last_source_ts_us <= $1"
    );
    let deleted = sqlx::query(AssertSqlSafe(sql))
        .bind(released_through_us)
        .execute(&mut **tx)
        .await
        .context("delete released lifecycle aggregates")?
        .rows_affected();
    if deleted != expected {
        bail!("released lifecycle delete mismatch: expected {expected}, deleted {deleted}");
    }
    Ok(())
}

async fn margin_finalized_watermark(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    released_through_us: i64,
) -> Result<i64> {
    let sql = format!(
        "SELECT LEAST(\
           COALESCE((SELECT MIN(open_source_ts_us)-1 FROM {schema}.intra_orders \
                     WHERE matching_state='pending'), $1),\
           COALESCE((SELECT MIN(first_source_ts_us)-1 FROM {schema}.intra_order_lifecycle \
                     WHERE filled_amount > 0), $1),\
           $1) AS watermark"
    );
    let row = sqlx::query(AssertSqlSafe(sql))
        .bind(released_through_us)
        .fetch_one(&mut **tx)
        .await
        .context("calculate Margin finalized watermark")?;
    row.try_get("watermark").context("decode Margin watermark")
}

async fn update_watermark(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    source_end_us: i64,
    released_through_us: i64,
    margin_finalized_through_us: i64,
    verified_through_ms: i64,
    reorder_window_us: i64,
) -> Result<()> {
    let sql = format!(
        "UPDATE {schema}.intra_match_watermark SET source_read_through_us=$1,\
         events_released_through_us=$2,margin_finalized_through_us=$3,\
         verified_through_ms=$4,reorder_window_us=$5,updated_at=CURRENT_TIMESTAMP \
         WHERE singleton"
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(source_end_us)
        .bind(released_through_us)
        .bind(margin_finalized_through_us)
        .bind(verified_through_ms)
        .bind(reorder_window_us)
        .execute(&mut **tx)
        .await
        .context("advance intra synthesis watermarks")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strategy_fields() {
        assert_eq!(parse_main_fkey("12345|leg=0"), Some(12345));
        assert_eq!(parse_main_fkey("bad|leg=0"), None);
        assert_eq!(parse_tlen("x:y:tlen=12.5"), Some(12.5));
        assert_eq!(parse_tlen("x:y"), None);
    }

    #[test]
    fn recognizes_all_observed_terminal_statuses() {
        for status in ["FILLED", "CANCELED", "EXPIRED", "EXPIRED_IN_MATCH"] {
            assert!(is_terminal(status));
        }
        assert!(!is_terminal("NEW"));
        assert!(!is_terminal("PARTIALLY_FILLED"));
    }

    #[test]
    fn selects_only_actual_fill_timestamps() {
        assert_eq!(fill_update_ts("PARTIALLY_FILLED", 2.5, 1_000), Some(1_000));
        assert_eq!(fill_update_ts("FILLED", 0.0, 2_000), Some(2_000));
        assert_eq!(fill_update_ts("PARTIALLY_FILLED", 0.0, 3_000), None);
        assert_eq!(fill_update_ts("CANCELED", 2.5, 4_000), Some(4_000));
        assert_eq!(fill_update_ts("CANCELED", 0.0, 4_500), None);
        assert_eq!(fill_update_ts("EXPIRED", 0.0, 5_000), None);
    }

    #[test]
    fn folds_named_timestamps_from_order_lifecycle_events() {
        let frame = df!(
            "ts_us" => [300_i64, 100, 400, 200, 500],
            "client_order_id" => [7_i64; 5],
            "trading_venue" => ["BinanceMargin"; 5],
            "mkt_ts" => [0_i64, 10, 0, 0, 0],
            "create_ts" => [20_i64; 5],
            "update_ts" => [40_i64, 30, 40, 35, 40],
            "local_ts" => [45_i64, 32, 44, 37, 43],
            "symbol" => ["BTCUSDT"; 5],
            "side" => ["buy"; 5],
            "price" => [100.0_f64; 5],
            "price_offset" => [0.0002_f64; 5],
            "amount_init" => [1.0_f64; 5],
            "amount_update" => [0.0_f64, 0.0, 1.0, 0.0, 0.0],
            "status" => ["FILLED", "NEW", "FILLED", "PARTIALLY_FILLED", "FILLED"],
            "from_key" => [""; 5],
        )
        .unwrap();
        let mut lifecycles = BTreeMap::new();

        assert_eq!(fold_frame(&frame, &mut lifecycles).unwrap(), 5);
        let order = lifecycles.values().next().unwrap();
        assert_eq!(order.mkt_ts_us, Some(10));
        assert_eq!(order.create_ts_us, 20);
        assert_eq!(order.new_ts_us, Some(30));
        assert_eq!(order.terminal_ts_us, Some(40));
        assert_eq!(order.terminal_ts_local_us, Some(43));
    }
}
