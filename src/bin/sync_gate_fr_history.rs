use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::{Parser, ValueEnum};
use crypto_nav_manager::{
    exchange::gate::{GateClient, GateCredentials},
    models::TimeRange,
    rest_dispatcher::{Dispatcher, DispatcherConfig},
    rest_ip_pool::configured_or_exchange_local_ips,
};
use serde_json::Value;
use sqlx::{
    AssertSqlSafe, PgPool, Postgres, QueryBuilder,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeSet, HashMap},
    env, fs,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_STRATEGIES: &[&str] = &["gate_fr_arb01", "gate_fr_arb02"];
const BATCH_SIZE: usize = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Dataset {
    All,
    Trades,
    Funding,
    Interest,
}

#[derive(Debug, Parser)]
#[command(about = "Backfill Gate FR trades, funding, and interest from strategy st_ms")]
struct Args {
    /// Strategy slug. May be repeated. Defaults to gate_fr_arb01 and gate_fr_arb02.
    #[arg(long)]
    strategy: Vec<String>,

    /// Restrict the backfill to one of the three FR datasets.
    #[arg(long, value_enum, default_value_t = Dataset::All)]
    dataset: Dataset,

    /// Override the PostgreSQL-selected Dispatcher source IP. May be repeated.
    #[arg(long)]
    local_ip: Vec<IpAddr>,

    /// Inclusive end timestamp. Defaults to now.
    #[arg(long)]
    end_ms: Option<i64>,

    /// Overrides CRYPTO_NAV_DATABASE_URL when provided.
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Debug)]
struct StrategyStorage {
    slug: String,
    schema: String,
    env_path: PathBuf,
    st_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TradeLeg {
    Spot,
    Futures,
}

impl TradeLeg {
    fn sid(self) -> i16 {
        match self {
            Self::Spot => 1,
            Self::Futures => 0,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Spot => "gatespot",
            Self::Futures => "gateswap",
        }
    }
}

#[derive(Debug)]
struct TradeRow {
    sid: i16,
    key: &'static str,
    symbol: String,
    id: String,
    order_id: String,
    side: String,
    price: String,
    qty: String,
    amountu: String,
    fees: String,
    commission_asset: String,
    realized_pnl: Option<String>,
    ts: i64,
    ttype: String,
    position_side: &'static str,
}

#[derive(Debug)]
struct FundingRow {
    id: String,
    symbol: String,
    funding: String,
    transaction_time: i64,
}

#[derive(Debug)]
struct InterestRow {
    id: String,
    currency: String,
    interest: String,
    transaction_time: i64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;

    let requested = requested_strategies(&args.strategy)?;
    let mut strategies = Vec::with_capacity(requested.len());
    let mut credentials = HashMap::new();
    for slug in requested {
        let strategy = strategy_storage(&pool, &slug).await?;
        credentials.insert(
            slug.clone(),
            read_gate_credentials(&strategy.env_path)
                .with_context(|| format!("load credentials for {slug}"))?,
        );
        strategies.push(strategy);
    }

    let end_ms = args.end_ms.unwrap_or_else(now_ms);
    for strategy in &strategies {
        if end_ms < strategy.st_ms {
            bail!(
                "end_ms {} is earlier than {} st_ms {}",
                end_ms,
                strategy.slug,
                strategy.st_ms
            );
        }
    }

    let local_ips = configured_or_exchange_local_ips(&pool, "gate", args.local_ip)
        .await
        .context("select Gate REST source IPs")?;
    println!("REST source IPs: {local_ips:?}");
    let dispatcher = Dispatcher::new(DispatcherConfig {
        local_ips,
        ..DispatcherConfig::default()
    })
    .context("create Gate REST dispatcher")?;

    let contract_multipliers = if matches!(args.dataset, Dataset::All | Dataset::Trades) {
        let (key, secret) = credentials
            .get(&strategies[0].slug)
            .context("missing first strategy credentials")?;
        let client = GateClient::new(
            dispatcher.clone(),
            GateCredentials::new(key.clone(), secret.clone()),
        );
        load_contract_multipliers(&client).await?
    } else {
        HashMap::new()
    };

    for strategy in &strategies {
        let (key, secret) = credentials
            .remove(&strategy.slug)
            .with_context(|| format!("missing credentials for {}", strategy.slug))?;
        let client = GateClient::new(dispatcher.clone(), GateCredentials::new(key, secret));
        println!(
            "\nbackfill {}: {} .. {}",
            strategy.slug,
            format_timestamp(strategy.st_ms),
            format_timestamp(end_ms)
        );
        let range = TimeRange::new(strategy.st_ms, end_ms)?;

        if matches!(args.dataset, Dataset::All | Dataset::Funding) {
            sync_funding(&pool, &client, strategy, range).await?;
        }
        if matches!(args.dataset, Dataset::All | Dataset::Interest) {
            sync_interest(&pool, &client, strategy, range).await?;
        }
        if matches!(args.dataset, Dataset::All | Dataset::Trades) {
            sync_trades(&pool, &client, strategy, range, &contract_multipliers).await?;
        }
        print_progress(&pool, strategy).await?;
    }

    pool.close().await;
    Ok(())
}

async fn sync_trades(
    pool: &PgPool,
    client: &GateClient,
    strategy: &StrategyStorage,
    range: TimeRange,
    contract_multipliers: &HashMap<String, f64>,
) -> Result<()> {
    println!("sync trades from strategy st_ms");
    let spot = client
        .spot_trades(range)
        .await
        .context("fetch Gate unified spot trades")?;
    let futures = client
        .futures_trades(range)
        .await
        .context("fetch Gate USDT futures trades")?;
    let spot_count = spot.len();
    let futures_count = futures.len();
    let mut rows = Vec::with_capacity(spot_count + futures_count);
    for raw in spot {
        rows.push(normalize_spot_trade(raw)?);
    }
    for raw in futures {
        rows.push(normalize_futures_trade(raw, contract_multipliers)?);
    }
    rows.sort_by(|left, right| {
        (left.ts, left.sid, &left.symbol, &left.id).cmp(&(
            right.ts,
            right.sid,
            &right.symbol,
            &right.id,
        ))
    });
    let affected = upsert_trades(pool, &strategy.schema, &rows).await?;
    println!("trades complete: spot={spot_count}, futures={futures_count}, upserted={affected}");
    Ok(())
}

async fn sync_funding(
    pool: &PgPool,
    client: &GateClient,
    strategy: &StrategyStorage,
    range: TimeRange,
) -> Result<()> {
    println!("sync funding from strategy st_ms");
    let raw = client
        .funding_fees(range)
        .await
        .context("fetch Gate futures funding account book")?;
    let rows = raw
        .into_iter()
        .map(normalize_funding)
        .collect::<Result<Vec<_>>>()?;
    let affected = upsert_funding(pool, &strategy.schema, &rows).await?;
    println!(
        "funding complete: fetched={}, upserted={affected}",
        rows.len()
    );
    Ok(())
}

async fn sync_interest(
    pool: &PgPool,
    client: &GateClient,
    strategy: &StrategyStorage,
    range: TimeRange,
) -> Result<()> {
    println!("sync interest from strategy st_ms");
    let raw = client
        .interest_records(range)
        .await
        .context("fetch Gate unified margin interest")?;
    let rows = raw
        .into_iter()
        .map(normalize_interest)
        .collect::<Result<Vec<_>>>()?;
    let affected = upsert_interest(pool, &strategy.schema, &rows).await?;
    println!(
        "interest complete: fetched={}, upserted={affected}",
        rows.len()
    );
    Ok(())
}

async fn load_contract_multipliers(client: &GateClient) -> Result<HashMap<String, f64>> {
    let contracts = client
        .futures_contracts()
        .await
        .context("fetch Gate USDT futures contracts")?;
    let mut multipliers = HashMap::with_capacity(contracts.len());
    for row in contracts {
        let name = required_string(&row, "name")?.to_ascii_uppercase();
        let multiplier = required_f64(&row, "quanto_multiplier")?;
        if multiplier <= 0.0 {
            bail!("invalid Gate contract multiplier for {name}: {multiplier}");
        }
        multipliers.insert(name, multiplier);
    }
    Ok(multipliers)
}

fn normalize_spot_trade(raw: Value) -> Result<TradeRow> {
    let leg = TradeLeg::Spot;
    let pair = required_string(&raw, "currency_pair")?;
    let symbol = compact_symbol(&pair)?;
    let id = required_text(&raw, "id")?;
    let side = required_string(&raw, "side")?.to_ascii_lowercase();
    validate_side(&side, &symbol, &id)?;
    let role = normalized_role(&raw, &symbol, &id)?;
    let price = required_numeric(&raw, "price")?;
    let qty = required_numeric(&raw, "amount")?;
    let price_value = positive_f64(&price, "price", &symbol, &id)?;
    let qty_value = positive_f64(&qty, "amount", &symbol, &id)?;
    let fee_value = optional_f64(&raw, "fee")?.unwrap_or_default().abs();
    let commission_asset = raw
        .get("fee_currency")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("USDT")
        .to_ascii_uppercase();
    let ts = required_timestamp_ms(&raw)?;

    Ok(TradeRow {
        sid: leg.sid(),
        key: leg.key(),
        symbol,
        id,
        order_id: optional_text(&raw, "order_id").unwrap_or_default(),
        side,
        price,
        qty,
        amountu: decimal_string(price_value * qty_value),
        fees: decimal_string(fee_value),
        commission_asset,
        realized_pnl: None,
        ts,
        ttype: role,
        position_side: "BOTH",
    })
}

fn normalize_futures_trade(
    raw: Value,
    contract_multipliers: &HashMap<String, f64>,
) -> Result<TradeRow> {
    let leg = TradeLeg::Futures;
    let contract = required_string(&raw, "contract")?.to_ascii_uppercase();
    let symbol = compact_symbol(&contract)?;
    let id = required_text(&raw, "trade_id")?;
    let size_text = required_numeric(&raw, "size")?;
    let size = size_text
        .parse::<f64>()
        .with_context(|| format!("parse Gate futures size for {symbol}/{id}"))?;
    if !size.is_finite() || size == 0.0 {
        bail!("invalid Gate futures size for {symbol}/{id}: {size_text}");
    }
    let multiplier = contract_multipliers
        .get(&contract)
        .copied()
        .with_context(|| format!("missing Gate contract multiplier for {contract}"))?;
    let price = required_numeric(&raw, "price")?;
    let price_value = positive_f64(&price, "price", &symbol, &id)?;
    let qty_value = size.abs() * multiplier;
    if !qty_value.is_finite() || qty_value <= 0.0 {
        bail!("invalid Gate futures base quantity for {symbol}/{id}");
    }
    let fee_value = optional_f64(&raw, "fee")?.unwrap_or_default().abs();
    let role = normalized_role(&raw, &symbol, &id)?;

    Ok(TradeRow {
        sid: leg.sid(),
        key: leg.key(),
        symbol,
        id,
        order_id: optional_text(&raw, "order_id").unwrap_or_default(),
        side: if size > 0.0 { "buy" } else { "sell" }.to_string(),
        price,
        qty: decimal_string(qty_value),
        amountu: decimal_string(price_value * qty_value),
        fees: decimal_string(fee_value),
        commission_asset: "USDT".to_string(),
        realized_pnl: optional_numeric(&raw, "pnl")?,
        ts: required_timestamp_ms(&raw)?,
        ttype: role,
        position_side: "BOTH",
    })
}

fn normalize_funding(raw: Value) -> Result<FundingRow> {
    if raw.get("type").and_then(Value::as_str) != Some("fund") {
        bail!("unexpected Gate account book type: {raw}");
    }
    let id = required_text(&raw, "id")?;
    let contract = raw
        .get("contract")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| contract_from_text(raw.get("text").and_then(Value::as_str)))
        .with_context(|| format!("Gate funding row {id} has no contract"))?;
    Ok(FundingRow {
        id,
        symbol: compact_symbol(&contract)?,
        funding: required_numeric(&raw, "change")?,
        transaction_time: required_timestamp_ms(&raw)?,
    })
}

fn normalize_interest(raw: Value) -> Result<InterestRow> {
    let currency = required_string(&raw, "currency")?.to_ascii_uppercase();
    let timestamp = required_timestamp_ms(&raw)?;
    let raw_interest = required_f64(&raw, "interest")?;
    if !raw_interest.is_finite() {
        bail!("invalid Gate interest for {currency}: {raw_interest}");
    }
    let id = optional_text(&raw, "id")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}:{}:{}",
                currency,
                timestamp,
                required_value_text(&raw, "interest").unwrap_or_default()
            )
        });
    Ok(InterestRow {
        id,
        currency,
        interest: decimal_string(-raw_interest.abs()),
        transaction_time: timestamp,
    })
}

async fn upsert_trades(pool: &PgPool, schema: &str, rows: &[TradeRow]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut transaction = pool.begin().await.context("begin Gate trade transaction")?;
    let mut affected = 0;
    for batch in rows.chunks(BATCH_SIZE) {
        let sql = format!(
            "INSERT INTO {schema}.trades \
             (sid,key,symbol,id,\"orderId\",side,price,qty,amountu,fees,\
              \"commissionAsset\",\"realizedPnl\",ts,ttype,\"positionSide\") "
        );
        let mut query = QueryBuilder::<Postgres>::new(sql);
        query.push_values(batch, |mut values, row| {
            values
                .push_bind(row.sid)
                .push_bind(row.key)
                .push_bind(&row.symbol)
                .push_bind(&row.id)
                .push_bind(&row.order_id)
                .push_bind(&row.side)
                .push_bind(&row.price)
                .push_unseparated("::numeric")
                .push_bind(&row.qty)
                .push_unseparated("::numeric")
                .push_bind(&row.amountu)
                .push_unseparated("::numeric")
                .push_bind(&row.fees)
                .push_unseparated("::numeric")
                .push_bind(&row.commission_asset)
                .push_bind(&row.realized_pnl)
                .push_unseparated("::numeric")
                .push_bind(row.ts)
                .push_bind(&row.ttype)
                .push_bind(row.position_side);
        });
        query.push(
            " ON CONFLICT (key,symbol,id) DO UPDATE SET \
             sid=EXCLUDED.sid,\"orderId\"=EXCLUDED.\"orderId\",side=EXCLUDED.side,\
             price=EXCLUDED.price,qty=EXCLUDED.qty,amountu=EXCLUDED.amountu,\
             fees=EXCLUDED.fees,\"commissionAsset\"=EXCLUDED.\"commissionAsset\",\
             \"realizedPnl\"=EXCLUDED.\"realizedPnl\",ts=EXCLUDED.ts,\
             ttype=EXCLUDED.ttype,\"positionSide\"=EXCLUDED.\"positionSide\"",
        );
        affected += query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert Gate trade batch")?
            .rows_affected();
    }
    transaction
        .commit()
        .await
        .context("commit Gate trade transaction")?;
    Ok(affected)
}

async fn upsert_funding(pool: &PgPool, schema: &str, rows: &[FundingRow]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut transaction = pool
        .begin()
        .await
        .context("begin Gate funding transaction")?;
    let mut affected = 0;
    for batch in rows.chunks(BATCH_SIZE) {
        let sql = format!("INSERT INTO {schema}.funding (id,symbol,funding,\"transactionTime\") ");
        let mut query = QueryBuilder::<Postgres>::new(sql);
        query.push_values(batch, |mut values, row| {
            values
                .push_bind(&row.id)
                .push_bind(&row.symbol)
                .push_bind(&row.funding)
                .push_bind(row.transaction_time);
        });
        query.push(
            " ON CONFLICT (id) DO UPDATE SET symbol=EXCLUDED.symbol,\
             funding=EXCLUDED.funding,\"transactionTime\"=EXCLUDED.\"transactionTime\"",
        );
        affected += query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert Gate funding batch")?
            .rows_affected();
    }
    transaction
        .commit()
        .await
        .context("commit Gate funding transaction")?;
    Ok(affected)
}

async fn upsert_interest(pool: &PgPool, schema: &str, rows: &[InterestRow]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut transaction = pool
        .begin()
        .await
        .context("begin Gate interest transaction")?;
    let mut affected = 0;
    for batch in rows.chunks(BATCH_SIZE) {
        let sql =
            format!("INSERT INTO {schema}.interest (id,currency,interest,\"transactionTime\") ");
        let mut query = QueryBuilder::<Postgres>::new(sql);
        query.push_values(batch, |mut values, row| {
            values
                .push_bind(&row.id)
                .push_bind(&row.currency)
                .push_bind(&row.interest)
                .push_bind(row.transaction_time);
        });
        query.push(
            " ON CONFLICT (id) DO UPDATE SET currency=EXCLUDED.currency,\
             interest=EXCLUDED.interest,\"transactionTime\"=EXCLUDED.\"transactionTime\"",
        );
        affected += query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert Gate interest batch")?
            .rows_affected();
    }
    transaction
        .commit()
        .await
        .context("commit Gate interest transaction")?;
    Ok(affected)
}

async fn print_progress(pool: &PgPool, strategy: &StrategyStorage) -> Result<()> {
    for (table, column) in [
        ("trades", "ts"),
        ("funding", "\"transactionTime\""),
        ("interest", "\"transactionTime\""),
    ] {
        let sql = format!(
            "SELECT COUNT(*),MIN({column}),MAX({column}) FROM {}.{table}",
            strategy.schema
        );
        let (count, min_ms, max_ms): (i64, Option<i64>, Option<i64>) =
            sqlx::query_as(AssertSqlSafe(sql.as_str()))
                .fetch_one(pool)
                .await
                .with_context(|| format!("query {}.{table} progress", strategy.schema))?;
        println!(
            "{}.{}: rows={}, range={} .. {}",
            strategy.schema,
            table,
            count,
            min_ms
                .map(format_timestamp)
                .unwrap_or_else(|| "none".to_string()),
            max_ms
                .map(format_timestamp)
                .unwrap_or_else(|| "none".to_string())
        );
    }
    Ok(())
}

fn requested_strategies(configured: &[String]) -> Result<Vec<String>> {
    let values = if configured.is_empty() {
        DEFAULT_STRATEGIES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    } else {
        configured.to_vec()
    };
    let allowed = DEFAULT_STRATEGIES.iter().copied().collect::<BTreeSet<_>>();
    let mut unique = BTreeSet::new();
    for value in values {
        if !allowed.contains(value.as_str()) {
            bail!("unsupported Gate FR history strategy: {value}");
        }
        unique.insert(value);
    }
    Ok(unique.into_iter().collect())
}

async fn strategy_storage(pool: &PgPool, slug: &str) -> Result<StrategyStorage> {
    let row: Option<(String, String, i64, String, String, String, String)> = sqlx::query_as(
        "SELECT db_schema,env_path,st_ms,exchange,account_mode,strategy_kind,host \
         FROM strategy_envs WHERE slug=$1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("query Gate FR strategy storage")?;
    let (schema, env_path, st_ms, exchange, account_mode, strategy_kind, host) =
        row.with_context(|| format!("strategy not found: {slug}"))?;
    if exchange != "gate" || account_mode != "unified" || strategy_kind != "funding_rate" {
        bail!("strategy {slug} is not a Gate unified funding-rate account");
    }
    if host != "local" {
        bail!("strategy {slug} is registered on remote host {host}");
    }
    if !valid_schema(&schema) {
        bail!("invalid PostgreSQL schema: {schema}");
    }
    Ok(StrategyStorage {
        slug: slug.to_string(),
        schema,
        env_path: PathBuf::from(env_path),
        st_ms,
    })
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

fn read_gate_credentials(path: &Path) -> Result<(String, String)> {
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
        if matches!(name.trim(), "GATE_API_KEY" | "GATE_API_SECRET") {
            values.insert(
                name.trim().to_string(),
                parse_env_value(value).with_context(|| {
                    format!("parse {} line {}", path.display(), line_number + 1)
                })?,
            );
        }
    }
    Ok((
        values
            .remove("GATE_API_KEY")
            .context("GATE_API_KEY is missing")?,
        values
            .remove("GATE_API_SECRET")
            .context("GATE_API_SECRET is missing")?,
    ))
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
        bail!("credential value must be a literal, optionally quoted");
    }
    Ok(value.to_string())
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .with_context(|| format!("Gate row is missing string field {field}: {value}"))
}

fn required_text(value: &Value, field: &str) -> Result<String> {
    required_value_text(value, field)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Gate row is missing field {field}: {value}"))
}

fn optional_text(value: &Value, field: &str) -> Option<String> {
    required_value_text(value, field).filter(|value| !value.is_empty())
}

fn required_value_text(value: &Value, field: &str) -> Option<String> {
    match value.get(field)? {
        Value::String(text) => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn required_numeric(value: &Value, field: &str) -> Result<String> {
    let text = required_value_text(value, field)
        .with_context(|| format!("Gate row is missing numeric field {field}: {value}"))?;
    let parsed = text
        .parse::<f64>()
        .with_context(|| format!("invalid Gate numeric field {field}: {text}"))?;
    if !parsed.is_finite() {
        bail!("non-finite Gate numeric field {field}: {text}");
    }
    Ok(text)
}

fn optional_numeric(value: &Value, field: &str) -> Result<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(_) => required_numeric(value, field).map(Some),
    }
}

fn required_f64(value: &Value, field: &str) -> Result<f64> {
    required_numeric(value, field)?
        .parse::<f64>()
        .with_context(|| format!("parse Gate numeric field {field}"))
}

fn optional_f64(value: &Value, field: &str) -> Result<Option<f64>> {
    optional_numeric(value, field)?
        .map(|text| {
            text.parse::<f64>()
                .with_context(|| format!("parse Gate numeric field {field}"))
        })
        .transpose()
}

fn positive_f64(text: &str, field: &str, symbol: &str, id: &str) -> Result<f64> {
    let value = text
        .parse::<f64>()
        .with_context(|| format!("parse Gate {field} for {symbol}/{id}"))?;
    if !value.is_finite() || value <= 0.0 {
        bail!("invalid Gate {field} for {symbol}/{id}: {text}");
    }
    Ok(value)
}

fn normalized_role(value: &Value, symbol: &str, id: &str) -> Result<String> {
    let role = required_string(value, "role")?.to_ascii_lowercase();
    if !matches!(role.as_str(), "maker" | "taker") {
        bail!("invalid Gate role for {symbol}/{id}: {role}");
    }
    Ok(role)
}

fn validate_side(side: &str, symbol: &str, id: &str) -> Result<()> {
    if !matches!(side, "buy" | "sell") {
        bail!("invalid Gate side for {symbol}/{id}: {side}");
    }
    Ok(())
}

fn required_timestamp_ms(value: &Value) -> Result<i64> {
    for field in [
        "create_time_ms",
        "time_ms",
        "transaction_time_ms",
        "transaction_time",
        "create_time",
        "time",
        "timestamp",
    ] {
        if let Some(timestamp) = value.get(field).and_then(timestamp_value_ms) {
            if timestamp > 0 {
                return Ok(timestamp);
            }
        }
    }
    bail!("Gate row has no positive timestamp: {value}")
}

fn timestamp_value_ms(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(number) => number.parse::<f64>().ok(),
        _ => None,
    }?;
    if !number.is_finite() || number <= 0.0 {
        return None;
    }
    let milliseconds = if number < 100_000_000_000.0 {
        number * 1_000.0
    } else {
        number
    };
    (milliseconds <= i64::MAX as f64).then(|| milliseconds.round() as i64)
}

fn compact_symbol(value: &str) -> Result<String> {
    let symbol = value
        .chars()
        .filter(|character| *character != '_' && *character != '-')
        .flat_map(char::to_uppercase)
        .collect::<String>();
    if symbol.is_empty() || !symbol.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        bail!("invalid Gate symbol: {value}");
    }
    Ok(symbol)
}

fn contract_from_text(text: Option<&str>) -> Option<String> {
    text?
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|part| part.ends_with("_USDT"))
        .map(str::to_string)
}

fn decimal_string(value: f64) -> String {
    let mut text = format!("{value:.18}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

fn valid_schema(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|character| {
            character == '_' || character.is_ascii_lowercase() || character.is_ascii_digit()
        })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn format_timestamp(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| timestamp_ms.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_spot_trade_to_fr_shape() {
        let row = normalize_spot_trade(serde_json::json!({
            "id": "12",
            "order_id": "34",
            "currency_pair": "BTC_USDT",
            "side": "buy",
            "role": "maker",
            "amount": "0.25",
            "price": "40000",
            "fee": "0.1",
            "fee_currency": "USDT",
            "create_time_ms": "1779667200.123"
        }))
        .unwrap();
        assert_eq!(row.key, "gatespot");
        assert_eq!(row.symbol, "BTCUSDT");
        assert_eq!(row.amountu, "10000");
        assert_eq!(row.ts, 1_779_667_200_123);
    }

    #[test]
    fn normalizes_futures_contract_size_to_base_quantity() {
        let row = normalize_futures_trade(
            serde_json::json!({
                "trade_id": "56",
                "order_id": "78",
                "contract": "BTC_USDT",
                "size": "-2.5",
                "price": "40000",
                "fee": "0.02",
                "role": "taker",
                "create_time": 1779667200.5
            }),
            &HashMap::from([("BTC_USDT".to_string(), 0.001)]),
        )
        .unwrap();
        assert_eq!(row.key, "gateswap");
        assert_eq!(row.side, "sell");
        assert_eq!(row.qty, "0.0025");
        assert_eq!(row.amountu, "100");
    }

    #[test]
    fn interest_is_stored_as_a_negative_cost() {
        let row = normalize_interest(serde_json::json!({
            "id": "90",
            "currency": "ETH",
            "interest": "0.001",
            "create_time": 1779667200000_i64
        }))
        .unwrap();
        assert_eq!(row.currency, "ETH");
        assert_eq!(row.interest, "-0.001");
        assert_eq!(row.transaction_time, 1_779_667_200_000);
    }
}
