use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_nav_manager::intra_order_match::{
    FuturesOrder, HedgeResult, MarginOrder, MatchEngine, MatchEvent, MatchingState, Side,
};
use polars::prelude::*;
use serde::Serialize;
use sqlx::{
    AssertSqlSafe, PgPool, Postgres, QueryBuilder, Row, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::BTreeMap, env, io::Cursor, time::Duration};

const SUPPORTED: [&str; 1] = ["binance-intra-arb01"];

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
}

#[derive(Clone, Debug)]
struct Lifecycle {
    venue: String,
    client_order_id: i64,
    first_source_ts_us: i64,
    last_source_ts_us: i64,
    create_ts_us: i64,
    update_ts_us: i64,
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

    let (schema, verified_through_ms) = strategy_scope(&pool, &args.strategy).await?;
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
    let (events, released_keys, margin_count, futures_count) =
        lifecycle_events(&ready, args.qty_epsilon)?;
    let mut engine = MatchEngine::new(pending, args.qty_epsilon)?;
    let hedges = engine.apply(events)?;
    let orders = engine.into_orders();

    upsert_orders(&mut tx, &schema, &orders).await?;
    insert_hedges(&mut tx, &schema, &hedges).await?;
    delete_released_lifecycles(&mut tx, &schema, &released_keys).await?;
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

async fn strategy_scope(pool: &PgPool, strategy: &str) -> Result<(String, i64)> {
    let row = sqlx::query(
        "SELECT s.db_schema,c.verified_through_ms FROM strategy_envs s \
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
    Ok((schema, row.try_get("verified_through_ms")?))
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
        .timeout(Duration::from_secs(120))
        .build()
        .context("build persist read client")?;
    let chunk_us = args
        .source_chunk_minutes
        .checked_mul(60_000_000)
        .context("source chunk overflow")?;
    let mut lifecycle = BTreeMap::<(String, i64), Lifecycle>::new();
    let mut row_count = 0usize;
    let mut cursor = start_us;
    while cursor < end_us {
        let window_end = cursor.saturating_add(chunk_us).min(end_us);
        let response = client
            .get(&endpoint)
            .query(&[
                ("table", "uniform_orders".to_string()),
                ("source_id", args.strategy.clone()),
                ("start_us", cursor.to_string()),
                ("end_us", window_end.to_string()),
                ("format", "parquet".to_string()),
            ])
            .send()
            .with_context(|| format!("read center {} {cursor}..{window_end}", args.strategy))?
            .error_for_status()
            .with_context(|| format!("center rejected {} {cursor}..{window_end}", args.strategy))?;
        let body = response.bytes().context("read center parquet body")?;
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
    let create_ts = frame.column("create_ts")?.i64()?;
    let update_ts = frame.column("update_ts")?.i64()?;
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
        let update = update_ts.get(row).context("null update_ts")?;
        let row_status = status.get(row).context("null status")?;
        let fill = amount_update.get(row).context("null amount_update")?;
        if !fill.is_finite() || fill < 0.0 {
            bail!("invalid amount_update {fill} for {venue}/{id}");
        }
        let row_price = price.get(row).context("null price")?;
        let key = (venue.to_string(), id);
        let terminal = is_terminal(row_status);
        match lifecycle.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Lifecycle {
                    venue: venue.to_string(),
                    client_order_id: id,
                    first_source_ts_us: source,
                    last_source_ts_us: source,
                    create_ts_us: create_ts.get(row).context("null create_ts")?,
                    update_ts_us: update,
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
                aggregate.first_source_ts_us = aggregate.first_source_ts_us.min(source);
                aggregate.last_source_ts_us = aggregate.last_source_ts_us.max(source);
                aggregate.create_ts_us = aggregate
                    .create_ts_us
                    .min(create_ts.get(row).context("null create_ts")?);
                aggregate.filled_amount += fill;
                aggregate.fill_notional += fill * row_price;
                aggregate.event_count += 1;
                aggregate.terminal |= terminal;
                if (update, source) >= (aggregate.update_ts_us, aggregate.last_source_ts_us) {
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
             first_source_ts_us,last_source_ts_us,create_ts_us,update_ts_us,symbol,side,price,\
             price_offset,amount_init,filled_amount,fill_notional,status,from_key,event_count,terminal) "
        ));
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(&row.venue)
                .push_bind(row.client_order_id)
                .push_bind(row.first_source_ts_us)
                .push_bind(row.last_source_ts_us)
                .push_bind(row.create_ts_us)
                .push_bind(row.update_ts_us)
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
             create_ts_us=LEAST({schema}.intra_order_lifecycle.create_ts_us,EXCLUDED.create_ts_us),\
             update_ts_us=GREATEST({schema}.intra_order_lifecycle.update_ts_us,EXCLUDED.update_ts_us),\
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
         create_ts_us,update_ts_us,symbol,side,price,price_offset,amount_init,filled_amount,\
         fill_notional,status,from_key,event_count,terminal \
         FROM {schema}.intra_order_lifecycle \
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
                create_ts_us: row.try_get("create_ts_us")?,
                update_ts_us: row.try_get("update_ts_us")?,
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
        "SELECT fkey,symbol,side,cts,open_uts,fts,close_count,price,amount,range,tlen,\
         open_fill_amount,remaining_amount,camount,netted_amount,close_notional,\
         open_source_ts_us FROM {schema}.intra_orders WHERE matching_state='pending' \
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

fn lifecycle_events(
    rows: &[Lifecycle],
    epsilon: f64,
) -> Result<(Vec<MatchEvent>, Vec<(String, i64)>, usize, usize)> {
    let mut events = Vec::new();
    let mut released = Vec::with_capacity(rows.len());
    let mut margins = 0usize;
    let mut futures = 0usize;
    for row in rows {
        released.push((row.venue.clone(), row.client_order_id));
        if row.filled_amount <= epsilon {
            continue;
        }
        let side = Side::parse(&row.side)?;
        if row.venue.ends_with("Margin") {
            margins += 1;
            events.push(MatchEvent::Margin(MarginOrder {
                fkey: row.client_order_id,
                symbol: row.symbol.clone(),
                side,
                cts: row.create_ts_us,
                open_uts: row.update_ts_us,
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
            futures += 1;
            events.push(MatchEvent::Futures(FuturesOrder {
                client_order_id: row.client_order_id,
                main_fkey: parse_main_fkey(&row.from_key),
                symbol: row.symbol.clone(),
                side,
                create_ts_us: row.create_ts_us,
                update_ts_us: row.update_ts_us,
                source_ts_us: row.last_source_ts_us,
                amount: row.filled_amount,
                cprice: (row.filled_amount > epsilon)
                    .then(|| row.fill_notional / row.filled_amount),
                event_count: row.event_count,
            }));
        }
    }
    Ok((events, released, margins, futures))
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
            "INSERT INTO {schema}.intra_orders (fkey,symbol,side,cts,open_uts,fts,holding,\
             holding_close,close_count,price,amount,cprice,camount,range,crange,tlen,pnlu,\
             open_fill_amount,remaining_amount,netted_amount,close_notional,matching_state,\
             open_source_ts_us,updated_at) "
        ));
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(row.fkey)
                .push_bind(&row.symbol)
                .push_bind(row.side.as_str())
                .push_bind(row.cts)
                .push_bind(row.open_uts)
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
            " ON CONFLICT (fkey) DO UPDATE SET fts=EXCLUDED.fts,\
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
             create_ts_us,update_ts_us,source_ts_us,amount,cprice,event_count,allocated_amount,\
             unallocated_amount,anchor_matched) "
        ));
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(row.order.client_order_id)
                .push_bind(row.order.main_fkey)
                .push_bind(&row.order.symbol)
                .push_bind(row.order.side.as_str())
                .push_bind(row.order.create_ts_us)
                .push_bind(row.order.update_ts_us)
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
    rows: &[(String, i64)],
) -> Result<()> {
    for chunk in rows.chunks(2_000) {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "DELETE FROM {schema}.intra_order_lifecycle WHERE (trading_venue,client_order_id) IN "
        ));
        query.push_tuples(chunk, |mut tuple, (venue, client_id)| {
            tuple.push_bind(venue).push_bind(*client_id);
        });
        query
            .build()
            .execute(&mut **tx)
            .await
            .context("delete released lifecycle aggregates")?;
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
}
