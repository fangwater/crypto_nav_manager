use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::{Parser, ValueEnum};
use crypto_nav_manager::{
    exchange::{
        ExchangeError,
        binance::{BinanceAccountMode, BinanceClient, BinanceCredentials},
        bitget::{BitgetClient, BitgetCredentials},
        bybit::{BybitCategory, BybitClient, BybitCredentials},
        gate::{GateClient, GateCredentials},
        okx::{OkxClient, OkxCredentials, OkxInstrumentType},
    },
    models::{ProductCategory, TimeRange},
    rest_dispatcher::{Dispatcher, DispatcherConfig},
    rest_ip_pool::configured_or_exchange_local_ips,
    strategy_env::read_env_file,
};
use serde_json::Value;
use sqlx::{
    AssertSqlSafe, PgConnection, PgPool, Postgres, QueryBuilder,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeSet, HashMap},
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const BATCH_SIZE: usize = 1_000;
const DEFAULT_OVERLAP_MINUTES: i64 = 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Dataset {
    All,
    Trades,
    Funding,
    Interest,
    Rebates,
}

impl Dataset {
    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Trades => "trades",
            Self::Funding => "funding",
            Self::Interest => "interest",
            Self::Rebates => "rebates",
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "One-shot full or incremental exchange account history sync")]
struct Args {
    /// Strategy slug registered in strategy_envs. May be repeated.
    #[arg(long, required = true)]
    strategy: Vec<String>,

    /// Dataset to sync.
    #[arg(long, value_enum, default_value_t = Dataset::All)]
    dataset: Dataset,

    /// Start from strategy st_ms. Required for an empty dataset without a watermark.
    #[arg(long)]
    full: bool,

    /// Amount of already scanned history to re-read during incremental sync.
    #[arg(long, default_value_t = DEFAULT_OVERLAP_MINUTES, value_parser = clap::value_parser!(i64).range(0..))]
    overlap_minutes: i64,

    /// Inclusive end timestamp. Defaults to now.
    #[arg(long)]
    end_ms: Option<i64>,

    /// Restrict Binance trade sync to these symbols. May be repeated.
    #[arg(long)]
    symbol: Vec<String>,

    /// Override automatically selected REST source IPs. May be repeated.
    #[arg(long)]
    local_ip: Vec<IpAddr>,

    /// Overrides CRYPTO_NAV_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StrategyClass {
    Fr,
    Intra,
    Mm,
}

#[derive(Debug)]
struct Strategy {
    slug: String,
    schema: String,
    host: String,
    env_path: PathBuf,
    exchange: String,
    account_mode: String,
    class: StrategyClass,
    st_ms: i64,
}

impl Strategy {
    fn supports(&self, dataset: Dataset) -> bool {
        match dataset {
            Dataset::All | Dataset::Trades | Dataset::Funding => true,
            Dataset::Interest => {
                self.class != StrategyClass::Mm
                    && !(self.class == StrategyClass::Intra && self.exchange == "binance")
            }
            Dataset::Rebates => self.exchange == "binance" && self.class == StrategyClass::Intra,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Leg {
    Spot,
    Derivative,
}

impl Leg {
    fn sid(self) -> i16 {
        match self {
            Self::Spot => 1,
            Self::Derivative => 0,
        }
    }
}

#[derive(Debug)]
struct TradeRow {
    leg: Leg,
    market: String,
    symbol: String,
    trade_id: String,
    order_id: String,
    side: String,
    role: String,
    price: String,
    quantity: String,
    quote_quantity: String,
    fee_amount: String,
    fee_asset: String,
    realized_pnl: Option<String>,
    event_time_ms: i64,
}

#[derive(Debug)]
struct CashRow {
    record_id: String,
    symbol: Option<String>,
    asset: String,
    amount: String,
    event_time_ms: i64,
}

#[derive(Debug)]
struct RebateRow {
    record_id: String,
    transaction_id: Option<String>,
    asset: String,
    amount: String,
    event_time_ms: i64,
    description: String,
    direction: Option<i16>,
    raw: Value,
}

#[derive(Clone, Debug)]
enum TradeStorage {
    Liang,
    Generic(String),
}

#[derive(Clone, Debug)]
enum CashStorage {
    Binance(String),
    Text(String),
    Generic(String),
}

#[derive(Debug)]
enum ExchangeClient {
    Binance(BinanceClient),
    Bybit(BybitClient),
    Gate(GateClient),
    Bitget(BitgetClient),
    Okx(OkxClient),
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;

    let end_ms = args.end_ms.unwrap_or_else(now_ms);
    let overlap_ms = args
        .overlap_minutes
        .checked_mul(60_000)
        .context("overlap minutes overflow")?;
    let mut slugs = BTreeSet::new();
    slugs.extend(args.strategy.iter().cloned());

    for slug in slugs {
        let strategy = load_strategy(&pool, &slug).await?;
        if end_ms < strategy.st_ms {
            bail!(
                "end_ms {end_ms} is earlier than {slug} st_ms {}",
                strategy.st_ms
            );
        }
        let client = build_client(&pool, &strategy, &args.local_ip).await?;
        println!(
            "\n{}: exchange={}, class={:?}, end={}",
            strategy.slug,
            strategy.exchange,
            strategy.class,
            format_timestamp(end_ms)
        );

        for dataset in selected_datasets(args.dataset) {
            if !strategy.supports(dataset) {
                if args.dataset == dataset {
                    bail!(
                        "{} {:?} does not support the {} dataset",
                        strategy.exchange,
                        strategy.class,
                        dataset.name()
                    );
                }
                println!(
                    "skip {}: disabled for this exchange/strategy class",
                    dataset.name()
                );
                continue;
            }
            sync_dataset(
                &pool,
                &strategy,
                &client,
                dataset,
                &args.symbol,
                args.full,
                overlap_ms,
                end_ms,
            )
            .await
            .with_context(|| format!("sync {} {}", strategy.slug, dataset.name()))?;
        }
    }

    pool.close().await;
    Ok(())
}

fn selected_datasets(dataset: Dataset) -> Vec<Dataset> {
    match dataset {
        Dataset::All => vec![
            Dataset::Trades,
            Dataset::Funding,
            Dataset::Interest,
            Dataset::Rebates,
        ],
        value => vec![value],
    }
}

async fn sync_dataset(
    pool: &PgPool,
    strategy: &Strategy,
    client: &ExchangeClient,
    dataset: Dataset,
    requested_symbols: &[String],
    full: bool,
    overlap_ms: i64,
    end_ms: i64,
) -> Result<()> {
    let latest = latest_dataset_time(pool, strategy, dataset).await?;
    let watermark = watermark(pool, &strategy.slug, dataset).await?;
    let start_ms =
        scan_start(strategy.st_ms, watermark, latest, full, overlap_ms).with_context(|| {
            format!(
                "{} {} is not initialized; rerun with --strategy {} --dataset {} --full",
                strategy.slug,
                dataset.name(),
                strategy.slug,
                dataset.name()
            )
        })?;
    if start_ms > end_ms {
        println!("{} already beyond requested end; skipped", dataset.name());
        return Ok(());
    }
    println!(
        "{}: {} .. {} (watermark={}, latest={})",
        dataset.name(),
        format_timestamp(start_ms),
        format_timestamp(end_ms),
        watermark
            .map(format_timestamp)
            .unwrap_or_else(|| "none".into()),
        latest
            .map(format_timestamp)
            .unwrap_or_else(|| "none".into()),
    );
    let range = TimeRange::new(start_ms, end_ms)?;

    match dataset {
        Dataset::Trades => {
            let symbols = load_trade_symbols(pool, strategy, requested_symbols).await?;
            let raw = fetch_trades(client, strategy, &symbols, range).await?;
            let multipliers = load_gate_multipliers(client).await?;
            let mut rows = raw
                .into_iter()
                .map(|(leg, value)| normalize_trade(&strategy.exchange, leg, value, &multipliers))
                .collect::<Result<Vec<_>>>()?;
            rows.sort_by(|a, b| {
                (a.event_time_ms, &a.market, &a.symbol, &a.trade_id).cmp(&(
                    b.event_time_ms,
                    &b.market,
                    &b.symbol,
                    &b.trade_id,
                ))
            });
            let storage = trade_storage(pool, &strategy.schema).await?;
            let affected = commit_trades(pool, strategy, storage, &rows, end_ms).await?;
            println!(
                "trades complete: fetched={}, upserted={affected}",
                rows.len()
            );
        }
        Dataset::Funding => {
            let raw = fetch_funding(client, range).await?;
            let rows = raw
                .into_iter()
                .map(|value| normalize_cash(&strategy.exchange, Dataset::Funding, value))
                .collect::<Result<Vec<_>>>()?;
            let storage = cash_storage(pool, &strategy.schema, Dataset::Funding).await?;
            let affected = commit_cash(pool, strategy, dataset, storage, &rows, end_ms).await?;
            println!(
                "funding complete: fetched={}, upserted={affected}",
                rows.len()
            );
        }
        Dataset::Interest => {
            let raw = fetch_interest(client, range).await?;
            let rows = raw
                .into_iter()
                .map(|value| normalize_cash(&strategy.exchange, Dataset::Interest, value))
                .collect::<Result<Vec<_>>>()?;
            let storage = cash_storage(pool, &strategy.schema, Dataset::Interest).await?;
            let affected = commit_cash(pool, strategy, dataset, storage, &rows, end_ms).await?;
            println!(
                "interest complete: fetched={}, upserted={affected}",
                rows.len()
            );
        }
        Dataset::Rebates => {
            let raw = fetch_rebates(client, range).await?;
            let mut rows = raw
                .into_iter()
                .map(normalize_binance_rebate)
                .collect::<Result<Vec<_>>>()?;
            rows.sort_by_key(|row| row.event_time_ms);
            let affected = commit_rebates(pool, strategy, &rows, end_ms).await?;
            println!(
                "rebates complete: fetched={}, upserted={affected}",
                rows.len()
            );
        }
        Dataset::All => unreachable!(),
    }
    Ok(())
}

fn scan_start(
    st_ms: i64,
    watermark: Option<i64>,
    latest: Option<i64>,
    full: bool,
    overlap_ms: i64,
) -> Result<i64> {
    if full {
        return Ok(st_ms);
    }
    watermark
        .or(latest)
        .map(|cursor| cursor.saturating_sub(overlap_ms).max(st_ms))
        .context("no successful scan watermark or stored rows; run once with --full")
}

async fn watermark(pool: &PgPool, slug: &str, dataset: Dataset) -> Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT success_end_ms FROM history_sync_watermarks \
         WHERE strategy_slug=$1 AND dataset=$2",
    )
    .bind(slug)
    .bind(dataset.name())
    .fetch_optional(pool)
    .await
    .context("load history sync watermark")
}

async fn advance_watermark(
    connection: &mut PgConnection,
    slug: &str,
    dataset: Dataset,
    end_ms: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO history_sync_watermarks(strategy_slug,dataset,success_end_ms) \
         VALUES($1,$2,$3) ON CONFLICT(strategy_slug,dataset) DO UPDATE SET \
         success_end_ms=GREATEST(history_sync_watermarks.success_end_ms,EXCLUDED.success_end_ms),\
         updated_at=CURRENT_TIMESTAMP",
    )
    .bind(slug)
    .bind(dataset.name())
    .bind(end_ms)
    .execute(connection)
    .await
    .context("advance history sync watermark")?;
    Ok(())
}
async fn fetch_trades(
    client: &ExchangeClient,
    strategy: &Strategy,
    symbols: &[String],
    range: TimeRange,
) -> Result<Vec<(Leg, Value)>> {
    let include_spot = strategy.class != StrategyClass::Mm;
    let mut rows = Vec::new();
    match client {
        ExchangeClient::Binance(client) => {
            if symbols.is_empty() {
                bail!("Binance trade sync needs at least one --symbol or a stored symbol");
            }
            for symbol in symbols {
                if include_spot {
                    let result = match strategy.account_mode.as_str() {
                        "portfolio_margin" => client.margin_trades(symbol, range).await,
                        _ => client.spot_trades(symbol, range).await,
                    };
                    append_binance_trades(&mut rows, Leg::Spot, symbol, result)?;
                }
                let result = match strategy.account_mode.as_str() {
                    "portfolio_margin" => client.um_trades(symbol, range).await,
                    _ => client.user_trades(symbol, range).await,
                };
                append_binance_trades(&mut rows, Leg::Derivative, symbol, result)?;
            }
        }
        ExchangeClient::Bybit(client) => {
            if include_spot {
                rows.extend(
                    client
                        .trades(BybitCategory::Spot, None, range)
                        .await
                        .context("fetch Bybit spot trades")?
                        .into_iter()
                        .map(|value| (Leg::Spot, value)),
                );
            }
            rows.extend(
                client
                    .trades(BybitCategory::Linear, None, range)
                    .await
                    .context("fetch Bybit linear trades")?
                    .into_iter()
                    .map(|value| (Leg::Derivative, value)),
            );
        }
        ExchangeClient::Gate(client) => {
            if include_spot {
                rows.extend(
                    client
                        .spot_trades(range)
                        .await
                        .context("fetch Gate spot trades")?
                        .into_iter()
                        .map(|value| (Leg::Spot, value)),
                );
            }
            rows.extend(
                client
                    .futures_trades(range)
                    .await
                    .context("fetch Gate futures trades")?
                    .into_iter()
                    .map(|value| (Leg::Derivative, value)),
            );
        }
        ExchangeClient::Bitget(client) => {
            if include_spot {
                rows.extend(
                    client
                        .fills(ProductCategory::Margin, None, range)
                        .await
                        .context("fetch Bitget margin fills")?
                        .into_iter()
                        .map(|value| (Leg::Spot, value)),
                );
            }
            rows.extend(
                client
                    .fills(ProductCategory::UsdtFutures, None, range)
                    .await
                    .context("fetch Bitget futures fills")?
                    .into_iter()
                    .map(|value| (Leg::Derivative, value)),
            );
        }
        ExchangeClient::Okx(client) => {
            if include_spot {
                rows.extend(
                    client
                        .fills(OkxInstrumentType::Margin, None, range)
                        .await
                        .context("fetch OKX margin fills")?
                        .into_iter()
                        .map(|value| (Leg::Spot, value)),
                );
            }
            rows.extend(
                client
                    .fills(OkxInstrumentType::Swap, None, range)
                    .await
                    .context("fetch OKX swap fills")?
                    .into_iter()
                    .map(|value| (Leg::Derivative, value)),
            );
        }
    }
    Ok(rows)
}

fn append_binance_trades(
    output: &mut Vec<(Leg, Value)>,
    leg: Leg,
    symbol: &str,
    result: std::result::Result<Vec<Value>, ExchangeError>,
) -> Result<()> {
    match result {
        Ok(rows) => output.extend(rows.into_iter().map(|value| (leg, value))),
        Err(ExchangeError::Api { code, .. }) if code == "-1121" => {
            println!("Binance {symbol}: invalid or delisted symbol; skipped");
        }
        Err(error) => return Err(error).with_context(|| format!("fetch Binance {symbol}")),
    }
    Ok(())
}

async fn fetch_funding(client: &ExchangeClient, range: TimeRange) -> Result<Vec<Value>> {
    match client {
        ExchangeClient::Binance(client) => client.funding_fees(range).await,
        ExchangeClient::Bybit(client) => client.funding_fees(BybitCategory::Linear, range).await,
        ExchangeClient::Gate(client) => client.funding_fees(range).await,
        ExchangeClient::Bitget(client) => client.funding_fees(range).await,
        ExchangeClient::Okx(client) => client.funding_fees(range).await,
    }
    .context("fetch funding history")
}

async fn fetch_interest(client: &ExchangeClient, range: TimeRange) -> Result<Vec<Value>> {
    match client {
        ExchangeClient::Binance(client) => client.margin_interest_history(range, None).await,
        ExchangeClient::Bybit(client) => client.borrow_interest(range).await,
        ExchangeClient::Gate(client) => client.interest_records(range).await,
        ExchangeClient::Bitget(client) => client.margin_interest(range).await,
        ExchangeClient::Okx(client) => client.interest_accrued(None, range).await,
    }
    .context("fetch interest history")
}

async fn fetch_rebates(client: &ExchangeClient, range: TimeRange) -> Result<Vec<Value>> {
    let ExchangeClient::Binance(client) = client else {
        bail!("rebate history is only supported for Binance");
    };
    client
        .asset_dividends(range)
        .await
        .context("fetch Binance wallet distributions")
}

async fn load_gate_multipliers(client: &ExchangeClient) -> Result<HashMap<String, f64>> {
    let ExchangeClient::Gate(client) = client else {
        return Ok(HashMap::new());
    };
    let contracts = client
        .futures_contracts()
        .await
        .context("fetch Gate futures contract multipliers")?;
    let mut values = HashMap::with_capacity(contracts.len());
    for row in contracts {
        let name = text_field(&row, &["name"])?;
        let multiplier = number_field(&row, &["quanto_multiplier"])?
            .parse::<f64>()
            .context("parse Gate quanto_multiplier")?;
        if !multiplier.is_finite() || multiplier <= 0.0 {
            bail!("invalid Gate multiplier for {name}: {multiplier}");
        }
        values.insert(name.to_ascii_uppercase(), multiplier);
    }
    Ok(values)
}
async fn build_client(
    pool: &PgPool,
    strategy: &Strategy,
    local_ip: &[IpAddr],
) -> Result<ExchangeClient> {
    let local_ips = configured_or_exchange_local_ips(pool, &strategy.exchange, local_ip.to_vec())
        .await
        .with_context(|| format!("select {} REST source IPs", strategy.exchange))?;
    let mut dispatcher_config = DispatcherConfig {
        local_ips,
        ..DispatcherConfig::default()
    };
    if strategy.exchange == "bitget" {
        // Bitget history limits are UID-scoped, so immediate alternate-IP retries do not help.
        dispatcher_config.max_rate_limit_retries = 0;
    }
    let dispatcher = Dispatcher::new(dispatcher_config)
        .with_context(|| format!("create {} REST dispatcher", strategy.exchange))?;
    let values = read_env(&strategy.host, &strategy.env_path)?;
    let client = match strategy.exchange.as_str() {
        "binance" => {
            let mode = match strategy.account_mode.as_str() {
                "usdm_futures" => BinanceAccountMode::UsdmFutures,
                "portfolio_margin" => BinanceAccountMode::PortfolioMargin,
                value => bail!("unsupported Binance account mode: {value}"),
            };
            ExchangeClient::Binance(BinanceClient::new(
                dispatcher,
                BinanceCredentials::new(
                    env_required(&values, "BINANCE_API_KEY")?,
                    env_required_any(&values, &["BINANCE_API_SECRET", "BINANCE_SECRET_KEY"])?,
                ),
                mode,
            ))
        }
        "bybit" => ExchangeClient::Bybit(BybitClient::new(
            dispatcher,
            BybitCredentials::new(
                env_required(&values, "BYBIT_API_KEY")?,
                env_required_any(&values, &["BYBIT_API_SECRET", "BYBIT_SECRET_KEY"])?,
            ),
        )),
        "gate" => ExchangeClient::Gate(GateClient::new(
            dispatcher,
            GateCredentials::new(
                env_required(&values, "GATE_API_KEY")?,
                env_required_any(&values, &["GATE_API_SECRET", "GATE_SECRET_KEY"])?,
            ),
        )),
        "bitget" => ExchangeClient::Bitget(
            BitgetClient::new(
                dispatcher,
                BitgetCredentials::new(
                    env_required(&values, "BITGET_API_KEY")?,
                    env_required_any(&values, &["BITGET_API_SECRET", "BITGET_SECRET_KEY"])?,
                    env_required_any(&values, &["BITGET_API_PASSPHRASE", "BITGET_PASSPHRASE"])?,
                ),
            )
            .with_history_request_policy(Duration::from_millis(250), 3),
        ),
        "okx" | "okex" => ExchangeClient::Okx(OkxClient::new(
            dispatcher,
            OkxCredentials::new(
                env_required_any(&values, &["OKX_API_KEY", "OKEX_API_KEY"])?,
                env_required_any(
                    &values,
                    &["OKX_API_SECRET", "OKX_SECRET_KEY", "OKEX_API_SECRET"],
                )?,
                env_required_any(
                    &values,
                    &["OKX_API_PASSPHRASE", "OKX_PASSPHRASE", "OKEX_PASSPHRASE"],
                )?,
            ),
        )),
        value => bail!("unsupported exchange for history sync: {value}"),
    };
    Ok(client)
}

async fn load_strategy(pool: &PgPool, slug: &str) -> Result<Strategy> {
    let row: Option<(String, String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT db_schema,host,env_path,exchange,account_mode,strategy_kind,st_ms \
         FROM strategy_envs WHERE slug=$1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("query strategy_envs")?;
    let (schema, host, env_path, exchange, account_mode, kind, st_ms) =
        row.with_context(|| format!("strategy not found: {slug}"))?;
    if !valid_schema(&schema) {
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
        host,
        env_path: PathBuf::from(env_path),
        exchange: exchange.to_ascii_lowercase(),
        account_mode,
        class,
        st_ms,
    })
}

fn read_env(host: &str, path: &Path) -> Result<HashMap<String, String>> {
    let contents = read_env_file(host, path)?;
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
        let name = name.trim();
        if name.ends_with("API_KEY")
            || name.ends_with("API_SECRET")
            || name.ends_with("SECRET_KEY")
            || name.ends_with("PASSPHRASE")
        {
            values.insert(
                name.to_string(),
                parse_env_value(value).with_context(|| {
                    format!("parse {} line {}", path.display(), line_number + 1)
                })?,
            );
        }
    }
    Ok(values)
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
    if value.contains('$') || value.contains(char::from(96)) || value.contains(char::is_whitespace)
    {
        bail!("credential value must be a literal, optionally quoted");
    }
    Ok(value.to_string())
}

fn env_required(values: &HashMap<String, String>, key: &str) -> Result<String> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .with_context(|| format!("{key} is missing"))
}

fn env_required_any(values: &HashMap<String, String>, keys: &[&str]) -> Result<String> {
    keys.iter()
        .find_map(|key| values.get(*key).filter(|value| !value.is_empty()).cloned())
        .with_context(|| format!("one of {} is required", keys.join(", ")))
}
fn normalize_trade(
    exchange: &str,
    leg: Leg,
    raw: Value,
    gate_multipliers: &HashMap<String, f64>,
) -> Result<TradeRow> {
    match exchange {
        "binance" => normalize_binance_trade(leg, raw),
        "bybit" => normalize_bybit_trade(leg, raw),
        "gate" => normalize_gate_trade(leg, raw, gate_multipliers),
        "bitget" => normalize_bitget_trade(leg, raw),
        "okx" | "okex" => normalize_okx_trade(leg, raw),
        value => bail!("unsupported trade normalizer: {value}"),
    }
}

fn normalize_binance_trade(leg: Leg, raw: Value) -> Result<TradeRow> {
    let symbol = normalize_symbol(&text_field(&raw, &["symbol"])?);
    let trade_id = text_field(&raw, &["id"])?;
    let side = if leg == Leg::Spot {
        if bool_field(&raw, &["isBuyer"]).unwrap_or(false) {
            "buy".to_string()
        } else {
            "sell".to_string()
        }
    } else {
        lower_side(&text_field(&raw, &["side"])?)
    };
    let role = if bool_field(&raw, &["isMaker", "maker"]).unwrap_or(false) {
        "maker"
    } else {
        "taker"
    };
    make_trade(
        leg,
        if leg == Leg::Spot {
            "spot"
        } else {
            "usdm_futures"
        },
        symbol,
        trade_id,
        text_field(&raw, &["orderId"])?,
        side,
        role,
        number_field(&raw, &["price"])?,
        number_field(&raw, &["qty"])?,
        optional_number(&raw, &["quoteQty"]),
        number_field(&raw, &["commission"]).unwrap_or_else(|_| "0".into()),
        optional_text(&raw, &["commissionAsset"]).unwrap_or_else(|| "USDT".into()),
        optional_number(&raw, &["realizedPnl"]),
        timestamp_field(&raw, &["time"])?,
        raw,
    )
}

fn normalize_bybit_trade(leg: Leg, raw: Value) -> Result<TradeRow> {
    let symbol = normalize_symbol(&text_field(&raw, &["symbol"])?);
    let role = if bool_field(&raw, &["isMaker"]).unwrap_or(false) {
        "maker"
    } else {
        "taker"
    };
    make_trade(
        leg,
        if leg == Leg::Spot { "spot" } else { "linear" },
        symbol,
        text_field(&raw, &["execId"])?,
        text_field(&raw, &["orderId"])?,
        lower_side(&text_field(&raw, &["side"])?),
        role,
        number_field(&raw, &["execPrice"])?,
        number_field(&raw, &["execQty"])?,
        optional_number(&raw, &["execValue"]),
        number_field(&raw, &["execFee"]).unwrap_or_else(|_| "0".into()),
        optional_text(&raw, &["feeCurrency", "feeCcy"]).unwrap_or_else(|| "USDT".into()),
        optional_number(&raw, &["execPnl"]),
        timestamp_field(&raw, &["execTime"])?,
        raw,
    )
}

fn normalize_gate_trade(
    leg: Leg,
    raw: Value,
    multipliers: &HashMap<String, f64>,
) -> Result<TradeRow> {
    if leg == Leg::Spot {
        let pair = text_field(&raw, &["currency_pair"])?;
        return make_trade(
            leg,
            "spot",
            normalize_symbol(&pair),
            text_field(&raw, &["id"])?,
            optional_text(&raw, &["order_id"]).unwrap_or_default(),
            lower_side(&text_field(&raw, &["side"])?),
            gate_role(&raw),
            number_field(&raw, &["price"])?,
            number_field(&raw, &["amount"])?,
            None,
            number_field(&raw, &["fee"]).unwrap_or_else(|_| "0".into()),
            optional_text(&raw, &["fee_currency"]).unwrap_or_else(|| "USDT".into()),
            None,
            timestamp_field(&raw, &["create_time_ms", "create_time"])?,
            raw,
        );
    }

    let contract = text_field(&raw, &["contract"])?.to_ascii_uppercase();
    let size = number_field(&raw, &["size"])?;
    let size_value = parse_number(&size)?;
    if size_value == 0.0 {
        bail!("Gate futures trade has zero size");
    }
    let multiplier = multipliers
        .get(&contract)
        .copied()
        .with_context(|| format!("missing Gate multiplier for {contract}"))?;
    let quantity = decimal_string(size_value.abs() * multiplier);
    make_trade(
        leg,
        "usdt_futures",
        normalize_symbol(&contract),
        text_field(&raw, &["trade_id"])?,
        optional_text(&raw, &["order_id"]).unwrap_or_default(),
        if size_value > 0.0 { "buy" } else { "sell" }.to_string(),
        gate_role(&raw),
        number_field(&raw, &["price"])?,
        quantity,
        None,
        number_field(&raw, &["fee"]).unwrap_or_else(|_| "0".into()),
        "USDT".to_string(),
        optional_number(&raw, &["pnl"]),
        timestamp_field(&raw, &["create_time_ms", "create_time"])?,
        raw,
    )
}

fn gate_role(raw: &Value) -> &'static str {
    if bool_field(raw, &["is_maker"]).unwrap_or(false)
        || optional_text(raw, &["role"]).is_some_and(|value| value.eq_ignore_ascii_case("maker"))
    {
        "maker"
    } else {
        "taker"
    }
}

fn normalize_bitget_trade(leg: Leg, raw: Value) -> Result<TradeRow> {
    let role_text = optional_text(&raw, &["tradeRole", "execType", "tradeScope"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let role = if role_text == "maker" || role_text == "m" {
        "maker"
    } else {
        "taker"
    };
    let (fee_amount, fee_asset) = bitget_fee(&raw);
    make_trade(
        leg,
        if leg == Leg::Spot {
            "margin"
        } else {
            "usdt_futures"
        },
        normalize_symbol(&text_field(&raw, &["symbol"])?),
        text_field(&raw, &["execId", "tradeId", "id"])?,
        optional_text(&raw, &["orderId"]).unwrap_or_default(),
        lower_side(&text_field(&raw, &["side"])?),
        role,
        number_field(&raw, &["price", "fillPrice", "execPrice"])?,
        number_field(&raw, &["size", "qty", "baseVolume", "execQty"])?,
        optional_number(&raw, &["quoteVolume", "amount", "execValue"]),
        fee_amount,
        fee_asset,
        optional_number(&raw, &["profit", "pnl", "realizedPnl", "execPnl"]),
        timestamp_field(&raw, &["createdTime", "cTime", "ts"])?,
        raw,
    )
}

fn bitget_fee(raw: &Value) -> (String, String) {
    let detail = raw
        .get("feeDetail")
        .and_then(Value::as_array)
        .and_then(|values| values.first());
    let amount = detail
        .and_then(|value| optional_number(value, &["fee", "totalFee"]))
        .or_else(|| optional_number(raw, &["fee", "execFee"]))
        .unwrap_or_else(|| "0".into());
    let asset = detail
        .and_then(|value| optional_text(value, &["feeCoin"]))
        .or_else(|| optional_text(raw, &["feeCoin", "feeCurrency", "feeCcy"]))
        .unwrap_or_else(|| "USDT".into());
    (amount, asset)
}

fn normalize_okx_trade(leg: Leg, raw: Value) -> Result<TradeRow> {
    let exec_type = optional_text(&raw, &["execType"]).unwrap_or_default();
    // OKX reports charges as negative and rebates as positive. Internally fees
    // are signed costs, so paid fees are positive and rebates are negative.
    let fee_amount = negate_decimal(&number_field(&raw, &["fee"]).unwrap_or_else(|_| "0".into()))?;
    make_trade(
        leg,
        if leg == Leg::Spot { "margin" } else { "swap" },
        normalize_symbol(&text_field(&raw, &["instId"])?),
        text_field(&raw, &["tradeId"])?,
        optional_text(&raw, &["ordId"]).unwrap_or_default(),
        lower_side(&text_field(&raw, &["side"])?),
        if exec_type.eq_ignore_ascii_case("M") {
            "maker"
        } else {
            "taker"
        },
        number_field(&raw, &["fillPx"])?,
        number_field(&raw, &["fillSz"])?,
        None,
        fee_amount,
        optional_text(&raw, &["feeCcy"]).unwrap_or_else(|| "USDT".into()),
        optional_number(&raw, &["fillPnl"]),
        timestamp_field(&raw, &["ts", "fillTime"])?,
        raw,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_trade(
    leg: Leg,
    market: &str,
    symbol: String,
    trade_id: String,
    order_id: String,
    side: String,
    role: &str,
    price: String,
    quantity: String,
    quote_quantity: Option<String>,
    fee_amount: String,
    fee_asset: String,
    realized_pnl: Option<String>,
    event_time_ms: i64,
    _raw: Value,
) -> Result<TradeRow> {
    if !matches!(side.as_str(), "buy" | "sell") {
        bail!("invalid trade side for {symbol}/{trade_id}: {side}");
    }
    let price_value = parse_number(&price)?;
    let quantity_value = parse_number(&quantity)?.abs();
    if price_value <= 0.0 || quantity_value <= 0.0 || event_time_ms <= 0 {
        bail!("invalid trade values for {symbol}/{trade_id}");
    }
    let quote_quantity =
        quote_quantity.unwrap_or_else(|| decimal_string(price_value * quantity_value));
    parse_number(&quote_quantity)?;
    parse_number(&fee_amount)?;
    if let Some(value) = &realized_pnl {
        parse_number(value)?;
    }
    Ok(TradeRow {
        leg,
        market: market.to_string(),
        symbol,
        trade_id,
        order_id,
        side,
        role: role.to_string(),
        price,
        quantity: absolute_decimal(&quantity)?,
        quote_quantity,
        fee_amount: signed_decimal(&fee_amount)?,
        fee_asset: fee_asset.to_ascii_uppercase(),
        realized_pnl,
        event_time_ms,
    })
}

fn normalize_cash(exchange: &str, dataset: Dataset, raw: Value) -> Result<CashRow> {
    let (record_id, symbol, asset, amount, event_time_ms) = match (exchange, dataset) {
        ("binance", Dataset::Funding) => (
            text_field(&raw, &["tranId"])?,
            optional_text(&raw, &["symbol"]).map(|value| normalize_symbol(&value)),
            text_field(&raw, &["asset"])?,
            number_field(&raw, &["income"])?,
            timestamp_field(&raw, &["time"])?,
        ),
        ("binance", Dataset::Interest) => (
            text_field(&raw, &["txId"])?,
            None,
            text_field(&raw, &["asset"])?,
            number_field(&raw, &["interest"])?,
            timestamp_field(&raw, &["interestAccuredTime"])?,
        ),
        ("bybit", Dataset::Funding) => (
            text_field(&raw, &["id"])?,
            Some(normalize_symbol(&text_field(&raw, &["symbol"])?)),
            optional_text(&raw, &["currency"]).unwrap_or_else(|| "USDT".into()),
            nonzero_number(&raw, &["cashFlow", "change"])?,
            timestamp_field(&raw, &["transactionTime"])?,
        ),
        ("bybit", Dataset::Interest) => (
            text_field(&raw, &["id"])?,
            None,
            text_field(&raw, &["currency"])?,
            nonzero_number(&raw, &["cashFlow", "change"])?,
            timestamp_field(&raw, &["transactionTime"])?,
        ),
        ("gate", Dataset::Funding) => {
            let id = text_field(&raw, &["id"])?;
            let contract = optional_text(&raw, &["contract"])
                .or_else(|| contract_from_text(optional_text(&raw, &["text"]).as_deref()))
                .with_context(|| format!("Gate funding row {id} has no contract"))?;
            (
                id,
                Some(normalize_symbol(&contract)),
                "USDT".to_string(),
                number_field(&raw, &["change"])?,
                timestamp_field(&raw, &["time_ms", "time"])?,
            )
        }
        ("gate", Dataset::Interest) => {
            let asset = text_field(&raw, &["currency"])?;
            let ts = timestamp_field(&raw, &["time_ms", "time"])?;
            let amount = number_field(&raw, &["interest"])?;
            let id =
                optional_text(&raw, &["id"]).unwrap_or_else(|| format!("{asset}:{ts}:{amount}"));
            (
                id,
                None,
                asset,
                decimal_string(-parse_number(&amount)?.abs()),
                ts,
            )
        }
        ("bitget", _) => {
            let record_type = optional_text(&raw, &["type"]).unwrap_or_default();
            let raw_amount = nonzero_number(&raw, &["amount", "change", "cashFlow"])?;
            let amount = if record_type.ends_with("_OUT") {
                format!("-{}", absolute_decimal(&raw_amount)?)
            } else if record_type.ends_with("_IN") {
                absolute_decimal(&raw_amount)?
            } else {
                raw_amount
            };
            (
                text_field(&raw, &["id", "bizId"])?,
                optional_text(&raw, &["symbol"]).map(|value| normalize_symbol(&value)),
                optional_text(&raw, &["coin", "currency", "asset"])
                    .unwrap_or_else(|| "USDT".into()),
                amount,
                timestamp_field(&raw, &["createdTime", "cTime", "ts"])?,
            )
        }
        ("okx" | "okex", Dataset::Funding) => (
            text_field(&raw, &["billId"])?,
            optional_text(&raw, &["instId"]).map(|value| normalize_symbol(&value)),
            optional_text(&raw, &["ccy"]).unwrap_or_else(|| "USDT".into()),
            nonzero_number(&raw, &["balChg", "pnl"])?,
            timestamp_field(&raw, &["ts"])?,
        ),
        ("okx" | "okex", Dataset::Interest) => {
            let asset = text_field(&raw, &["ccy"])?;
            let ts = timestamp_field(&raw, &["ts"])?;
            let amount = number_field(&raw, &["interest"])?;
            (
                optional_text(&raw, &["billId"])
                    .unwrap_or_else(|| format!("{asset}:{ts}:{amount}")),
                None,
                asset,
                amount,
                ts,
            )
        }
        _ => bail!("unsupported {exchange} {} normalizer", dataset.name()),
    };
    parse_number(&amount)?;
    if event_time_ms <= 0 {
        bail!(
            "invalid {} timestamp for {exchange}/{record_id}",
            dataset.name()
        );
    }
    Ok(CashRow {
        record_id,
        symbol,
        asset: asset.to_ascii_uppercase(),
        amount,
        event_time_ms,
    })
}

fn normalize_binance_rebate(raw: Value) -> Result<RebateRow> {
    let record_id = text_field(&raw, &["id"])?;
    let amount = number_field(&raw, &["amount"])?;
    parse_number(&amount)?;
    let event_time_ms = timestamp_field(&raw, &["divTime"])?;
    if event_time_ms <= 0 {
        bail!("invalid rebate timestamp for Binance/{record_id}");
    }
    let direction = optional_text(&raw, &["direction"])
        .map(|value| {
            value
                .parse::<i16>()
                .context("parse Binance rebate direction")
        })
        .transpose()?;
    Ok(RebateRow {
        record_id,
        transaction_id: optional_text(&raw, &["tranId"]),
        asset: text_field(&raw, &["asset"])?.to_ascii_uppercase(),
        amount,
        event_time_ms,
        description: text_field(&raw, &["enInfo"])?,
        direction,
        raw,
    })
}
async fn trade_storage(pool: &PgPool, schema: &str) -> Result<TradeStorage> {
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

async fn cash_storage(pool: &PgPool, schema: &str, dataset: Dataset) -> Result<CashStorage> {
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
        let binance_id = match dataset {
            Dataset::Funding => "tranId",
            Dataset::Interest => "txId",
            _ => unreachable!(),
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

async fn latest_dataset_time(
    pool: &PgPool,
    strategy: &Strategy,
    dataset: Dataset,
) -> Result<Option<i64>> {
    let (table, column) = match dataset {
        Dataset::Trades => match trade_storage(pool, &strategy.schema).await? {
            TradeStorage::Liang => ("trades".to_string(), "ts".to_string()),
            TradeStorage::Generic(table) => (table, "event_time_ms".to_string()),
        },
        Dataset::Funding | Dataset::Interest => {
            match cash_storage(pool, &strategy.schema, dataset).await? {
                CashStorage::Generic(table) => (table, "event_time_ms".to_string()),
                CashStorage::Binance(table) if dataset == Dataset::Funding => {
                    (table, "time".to_string())
                }
                CashStorage::Binance(table) => (table, "interestAccuredTime".to_string()),
                CashStorage::Text(table) => (table, "transactionTime".to_string()),
            }
        }
        Dataset::Rebates => ("rebates".to_string(), "event_time_ms".to_string()),
        Dataset::All => unreachable!(),
    };
    if !valid_schema(&strategy.schema) || !valid_identifier(&table) || !valid_identifier(&column) {
        bail!("unsafe cursor SQL identifier");
    }
    let sql = format!(
        "SELECT MAX(\"{column}\") FROM {}.\"{table}\"",
        strategy.schema
    );
    sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
        .fetch_one(pool)
        .await
        .with_context(|| format!("query latest {} timestamp", dataset.name()))
}

async fn load_trade_symbols(
    pool: &PgPool,
    strategy: &Strategy,
    requested: &[String],
) -> Result<Vec<String>> {
    if strategy.exchange != "binance" {
        return Ok(Vec::new());
    }
    let mut symbols = requested
        .iter()
        .map(|value| normalize_symbol(value))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    if symbols.is_empty() {
        for table in ["trades", "trade_fills", "funding", "funding_fees"] {
            if !table_exists(pool, &strategy.schema, table).await? {
                continue;
            }
            let sql = format!(
                "SELECT DISTINCT symbol FROM {}.{table} WHERE symbol IS NOT NULL",
                strategy.schema
            );
            let stored: Vec<String> = sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
                .fetch_all(pool)
                .await
                .with_context(|| format!("load symbols from {}.{table}", strategy.schema))?;
            symbols.extend(stored.into_iter().map(|value| normalize_symbol(&value)));
        }
    }
    if symbols.is_empty() {
        bail!(
            "{} has no stored Binance symbols; pass one or more --symbol values",
            strategy.slug
        );
    }
    Ok(symbols.into_iter().collect())
}

async fn commit_trades(
    pool: &PgPool,
    strategy: &Strategy,
    storage: TradeStorage,
    rows: &[TradeRow],
    end_ms: i64,
) -> Result<u64> {
    let mut transaction = pool.begin().await.context("begin trade sync transaction")?;
    let mut affected = 0;
    match storage {
        TradeStorage::Liang => {
            let numeric_ids = strategy.exchange == "binance";
            let key_prefix = match strategy.exchange.as_str() {
                "binance" => "binance",
                "bybit" => "bybit",
                "gate" => "gate",
                "bitget" => "bitget",
                "okx" | "okex" => "okx",
                value => bail!("unsupported Liang trade key for {value}"),
            };
            for batch in rows.chunks(BATCH_SIZE) {
                let sql = format!(
                    "INSERT INTO {}.trades \
                     (sid,key,symbol,id,\"orderId\",side,price,qty,amountu,fees,\
                      \"commissionAsset\",\"realizedPnl\",ts,ttype,\"positionSide\") ",
                    strategy.schema
                );
                let mut query = QueryBuilder::<Postgres>::new(sql);
                query.push_values(batch, |mut values, row| {
                    values
                        .push_bind(row.leg.sid())
                        .push_bind(format!(
                            "{key_prefix}{}",
                            if row.leg == Leg::Spot { "spot" } else { "swap" }
                        ))
                        .push_bind(&row.symbol)
                        .push_bind(&row.trade_id);
                    if numeric_ids {
                        values.push_unseparated("::bigint");
                    }
                    values.push_bind(&row.order_id);
                    if numeric_ids {
                        values.push_unseparated("::bigint");
                    }
                    values
                        .push_bind(&row.side)
                        .push_bind(&row.price)
                        .push_unseparated("::numeric")
                        .push_bind(&row.quantity)
                        .push_unseparated("::numeric")
                        .push_bind(&row.quote_quantity)
                        .push_unseparated("::numeric")
                        .push_bind(&row.fee_amount)
                        .push_unseparated("::numeric")
                        .push_bind(&row.fee_asset)
                        .push_bind(&row.realized_pnl)
                        .push_unseparated("::numeric")
                        .push_bind(row.event_time_ms)
                        .push_bind(&row.role)
                        .push_bind("BOTH");
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
                    .context("upsert Liang trade batch")?
                    .rows_affected();
            }
        }
        TradeStorage::Generic(table) => {
            for batch in rows.chunks(BATCH_SIZE) {
                let sql = format!(
                    "INSERT INTO {}.{table} \
                     (market,symbol,trade_id,order_id,side,liquidity_role,price,quantity,\
                      quote_quantity,fee_amount,fee_asset,fee_usdt,realized_pnl,event_time_ms) ",
                    strategy.schema
                );
                let mut query = QueryBuilder::<Postgres>::new(sql);
                query.push_values(batch, |mut values, row| {
                    let fee_usdt = row
                        .fee_asset
                        .eq_ignore_ascii_case("USDT")
                        .then_some(row.fee_amount.as_str());
                    values
                        .push_bind(&row.market)
                        .push_bind(&row.symbol)
                        .push_bind(&row.trade_id)
                        .push_bind(&row.order_id)
                        .push_bind(&row.side)
                        .push_bind(&row.role)
                        .push_bind(&row.price)
                        .push_unseparated("::numeric")
                        .push_bind(&row.quantity)
                        .push_unseparated("::numeric")
                        .push_bind(&row.quote_quantity)
                        .push_unseparated("::numeric")
                        .push_bind(&row.fee_amount)
                        .push_unseparated("::numeric")
                        .push_bind(&row.fee_asset)
                        .push_bind(fee_usdt)
                        .push_unseparated("::numeric")
                        .push_bind(&row.realized_pnl)
                        .push_unseparated("::numeric")
                        .push_bind(row.event_time_ms);
                });
                query.push(
                    " ON CONFLICT (market,symbol,trade_id) DO UPDATE SET \
                     order_id=EXCLUDED.order_id,side=EXCLUDED.side,\
                     liquidity_role=EXCLUDED.liquidity_role,price=EXCLUDED.price,\
                     quantity=EXCLUDED.quantity,quote_quantity=EXCLUDED.quote_quantity,\
                     fee_amount=EXCLUDED.fee_amount,fee_asset=EXCLUDED.fee_asset,\
                     fee_usdt=EXCLUDED.fee_usdt,realized_pnl=EXCLUDED.realized_pnl,\
                     event_time_ms=EXCLUDED.event_time_ms",
                );
                affected += query
                    .build()
                    .execute(&mut *transaction)
                    .await
                    .context("upsert generic trade batch")?
                    .rows_affected();
            }
        }
    }
    advance_watermark(&mut transaction, &strategy.slug, Dataset::Trades, end_ms).await?;
    transaction.commit().await.context("commit trade sync")?;
    Ok(affected)
}
async fn commit_cash(
    pool: &PgPool,
    strategy: &Strategy,
    dataset: Dataset,
    storage: CashStorage,
    rows: &[CashRow],
    end_ms: i64,
) -> Result<u64> {
    let mut transaction = pool.begin().await.context("begin cash sync transaction")?;
    let mut affected = 0;
    match storage {
        CashStorage::Binance(table) => {
            let funding = dataset == Dataset::Funding;
            for batch in rows.chunks(BATCH_SIZE) {
                let sql = if funding {
                    format!(
                        "INSERT INTO {}.{table} (\"tranId\",symbol,income,time) ",
                        strategy.schema
                    )
                } else {
                    format!(
                        "INSERT INTO {}.{table} (\"txId\",asset,interest,\"interestAccuredTime\") ",
                        strategy.schema
                    )
                };
                let mut query = QueryBuilder::<Postgres>::new(sql);
                query.push_values(batch, |mut values, row| {
                    values
                        .push_bind(&row.record_id)
                        .push_unseparated("::bigint");
                    if funding {
                        values.push_bind(row.symbol.as_deref().unwrap_or(""));
                    } else {
                        values.push_bind(&row.asset);
                    }
                    values.push_bind(&row.amount).push_bind(row.event_time_ms);
                });
                if funding {
                    query.push(
                        " ON CONFLICT (\"tranId\") DO UPDATE SET symbol=EXCLUDED.symbol,\
                         income=EXCLUDED.income,time=EXCLUDED.time",
                    );
                } else {
                    query.push(
                        " ON CONFLICT (\"txId\") DO UPDATE SET asset=EXCLUDED.asset,\
                         interest=EXCLUDED.interest,\
                         \"interestAccuredTime\"=EXCLUDED.\"interestAccuredTime\"",
                    );
                }
                affected += query
                    .build()
                    .execute(&mut *transaction)
                    .await
                    .context("upsert Binance cash batch")?
                    .rows_affected();
            }
        }
        CashStorage::Text(table) => {
            let funding = dataset == Dataset::Funding;
            for batch in rows.chunks(BATCH_SIZE) {
                let sql = if funding {
                    format!(
                        "INSERT INTO {}.{table} (id,symbol,funding,\"transactionTime\") ",
                        strategy.schema
                    )
                } else {
                    format!(
                        "INSERT INTO {}.{table} (id,currency,interest,\"transactionTime\") ",
                        strategy.schema
                    )
                };
                let mut query = QueryBuilder::<Postgres>::new(sql);
                query.push_values(batch, |mut values, row| {
                    values.push_bind(&row.record_id);
                    if funding {
                        values.push_bind(row.symbol.as_deref().unwrap_or(""));
                    } else {
                        values.push_bind(&row.asset);
                    }
                    values.push_bind(&row.amount).push_bind(row.event_time_ms);
                });
                if funding {
                    query.push(
                        " ON CONFLICT (id) DO UPDATE SET symbol=EXCLUDED.symbol,\
                         funding=EXCLUDED.funding,\
                         \"transactionTime\"=EXCLUDED.\"transactionTime\"",
                    );
                } else {
                    query.push(
                        " ON CONFLICT (id) DO UPDATE SET currency=EXCLUDED.currency,\
                         interest=EXCLUDED.interest,\
                         \"transactionTime\"=EXCLUDED.\"transactionTime\"",
                    );
                }
                affected += query
                    .build()
                    .execute(&mut *transaction)
                    .await
                    .context("upsert text cash batch")?
                    .rows_affected();
            }
        }
        CashStorage::Generic(table) => {
            for batch in rows.chunks(BATCH_SIZE) {
                let sql = format!(
                    "INSERT INTO {}.{table} \
                     (record_id,symbol,asset,amount,amount_usdt,event_time_ms) ",
                    strategy.schema
                );
                let mut query = QueryBuilder::<Postgres>::new(sql);
                query.push_values(batch, |mut values, row| {
                    let amount_usdt = row
                        .asset
                        .eq_ignore_ascii_case("USDT")
                        .then_some(row.amount.as_str());
                    values
                        .push_bind(&row.record_id)
                        .push_bind(&row.symbol)
                        .push_bind(&row.asset)
                        .push_bind(&row.amount)
                        .push_unseparated("::numeric")
                        .push_bind(amount_usdt)
                        .push_unseparated("::numeric")
                        .push_bind(row.event_time_ms);
                });
                query.push(
                    " ON CONFLICT (record_id) DO UPDATE SET symbol=EXCLUDED.symbol,\
                     asset=EXCLUDED.asset,amount=EXCLUDED.amount,\
                     amount_usdt=EXCLUDED.amount_usdt,event_time_ms=EXCLUDED.event_time_ms",
                );
                affected += query
                    .build()
                    .execute(&mut *transaction)
                    .await
                    .context("upsert generic cash batch")?
                    .rows_affected();
            }
        }
    }
    advance_watermark(&mut transaction, &strategy.slug, dataset, end_ms).await?;
    transaction.commit().await.context("commit cash sync")?;
    Ok(affected)
}

async fn commit_rebates(
    pool: &PgPool,
    strategy: &Strategy,
    rows: &[RebateRow],
    end_ms: i64,
) -> Result<u64> {
    let mut transaction = pool
        .begin()
        .await
        .context("begin rebate sync transaction")?;
    let mut affected = 0;
    for batch in rows.chunks(BATCH_SIZE) {
        let sql = format!(
            "INSERT INTO {}.rebates \
             (record_id,transaction_id,asset,amount,amount_usdt,event_time_ms,\
              description,direction,raw) ",
            strategy.schema
        );
        let mut query = QueryBuilder::<Postgres>::new(sql);
        query.push_values(batch, |mut values, row| {
            let amount_usdt = matches!(row.asset.as_str(), "USD" | "USDC" | "USDT")
                .then_some(row.amount.as_str());
            values
                .push_bind(&row.record_id)
                .push_bind(&row.transaction_id)
                .push_bind(&row.asset)
                .push_bind(&row.amount)
                .push_unseparated("::numeric")
                .push_bind(amount_usdt)
                .push_unseparated("::numeric")
                .push_bind(row.event_time_ms)
                .push_bind(&row.description)
                .push_bind(row.direction)
                .push_bind(&row.raw);
        });
        query.push(
            " ON CONFLICT (record_id) DO UPDATE SET \
             transaction_id=EXCLUDED.transaction_id,asset=EXCLUDED.asset,\
             amount=EXCLUDED.amount,amount_usdt=EXCLUDED.amount_usdt,\
             event_time_ms=EXCLUDED.event_time_ms,description=EXCLUDED.description,\
             direction=EXCLUDED.direction,raw=EXCLUDED.raw,\
             fetched_at=CURRENT_TIMESTAMP",
        );
        affected += query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert Binance rebate batch")?
            .rows_affected();
    }
    advance_watermark(&mut transaction, &strategy.slug, Dataset::Rebates, end_ms).await?;
    transaction.commit().await.context("commit rebate sync")?;
    Ok(affected)
}

fn text_field(value: &Value, fields: &[&str]) -> Result<String> {
    fields
        .iter()
        .find_map(|field| optional_text(value, &[*field]))
        .with_context(|| format!("row is missing field {}: {value}", fields.join("/")))
}

fn optional_text(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| match value.get(*field)? {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

fn number_field(value: &Value, fields: &[&str]) -> Result<String> {
    let text = text_field(value, fields)?;
    parse_number(&text)
        .with_context(|| format!("invalid numeric field {}: {text}", fields.join("/")))?;
    Ok(text)
}

fn optional_number(value: &Value, fields: &[&str]) -> Option<String> {
    let text = optional_text(value, fields)?;
    parse_number(&text).ok().map(|_| text)
}

fn nonzero_number(value: &Value, fields: &[&str]) -> Result<String> {
    for field in fields {
        if let Some(text) = optional_number(value, &[*field]) {
            if parse_number(&text)? != 0.0 {
                return Ok(text);
            }
        }
    }
    number_field(value, fields)
}

fn bool_field(value: &Value, fields: &[&str]) -> Option<bool> {
    fields.iter().find_map(|field| {
        let item = value.get(*field)?;
        item.as_bool().or_else(|| match item.as_str()? {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        })
    })
}

fn timestamp_field(value: &Value, fields: &[&str]) -> Result<i64> {
    let text = text_field(value, fields)?;
    let number = text
        .parse::<f64>()
        .with_context(|| format!("invalid timestamp {}: {text}", fields.join("/")))?;
    if !number.is_finite() || number <= 0.0 {
        bail!("invalid timestamp {}: {text}", fields.join("/"));
    }
    let milliseconds = if number < 100_000_000_000.0 {
        number * 1_000.0
    } else {
        number
    };
    if milliseconds > i64::MAX as f64 {
        bail!("timestamp overflow: {text}");
    }
    Ok(milliseconds.round() as i64)
}

fn parse_number(value: &str) -> Result<f64> {
    let number = value.parse::<f64>().context("parse decimal")?;
    if !number.is_finite() {
        bail!("decimal is not finite");
    }
    Ok(number)
}

fn absolute_decimal(value: &str) -> Result<String> {
    let number = parse_number(value)?;
    if number == 0.0 {
        return Ok("0".to_string());
    }
    Ok(value
        .trim()
        .strip_prefix(['-', '+'])
        .unwrap_or(value.trim())
        .to_string())
}

fn signed_decimal(value: &str) -> Result<String> {
    let number = parse_number(value)?;
    if number == 0.0 {
        return Ok("0".to_string());
    }
    Ok(value
        .trim()
        .strip_prefix('+')
        .unwrap_or(value.trim())
        .to_string())
}

fn negate_decimal(value: &str) -> Result<String> {
    let value = signed_decimal(value)?;
    if value == "0" {
        return Ok(value);
    }
    Ok(match value.strip_prefix('-') {
        Some(value) => value.to_string(),
        None => format!("-{value}"),
    })
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

fn lower_side(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_symbol(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| *character != '-' && *character != '_')
        .flat_map(char::to_uppercase)
        .collect()
}

fn contract_from_text(text: Option<&str>) -> Option<String> {
    text?
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|part| part.ends_with("_USDT"))
        .map(str::to_string)
}

fn valid_schema(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|character| {
            character == '_' || character.is_ascii_lowercase() || character.is_ascii_digit()
        })
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
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

    fn strategy(exchange: &str, class: StrategyClass) -> Strategy {
        Strategy {
            slug: "test".into(),
            schema: "test".into(),
            host: "local".into(),
            env_path: PathBuf::new(),
            exchange: exchange.into(),
            account_mode: "unified".into(),
            class,
            st_ms: 1_000_000,
        }
    }

    #[test]
    fn interest_policy_matches_strategy_rules() {
        assert!(!strategy("bybit", StrategyClass::Mm).supports(Dataset::Interest));
        assert!(!strategy("binance", StrategyClass::Intra).supports(Dataset::Interest));
        assert!(strategy("bybit", StrategyClass::Intra).supports(Dataset::Interest));
        assert!(strategy("bitget", StrategyClass::Fr).supports(Dataset::Interest));
    }

    #[test]
    fn rebate_policy_only_enables_binance_intra() {
        assert!(strategy("binance", StrategyClass::Intra).supports(Dataset::Rebates));
        assert!(!strategy("binance", StrategyClass::Fr).supports(Dataset::Rebates));
        assert!(!strategy("bybit", StrategyClass::Intra).supports(Dataset::Rebates));
    }

    #[test]
    fn incremental_range_prefers_success_watermark() {
        assert_eq!(
            scan_start(1_000_000, Some(2_000_000), Some(3_000_000), false, 600_000).unwrap(),
            1_400_000
        );
    }

    #[test]
    fn incremental_range_can_seed_from_existing_rows() {
        assert_eq!(
            scan_start(1_000_000, None, Some(2_000_000), false, 600_000).unwrap(),
            1_400_000
        );
        assert!(scan_start(1_000_000, None, None, false, 600_000).is_err());
        assert_eq!(
            scan_start(1_000_000, None, None, true, 600_000).unwrap(),
            1_000_000
        );
    }
    #[test]
    fn bitget_uta_fill_uses_exec_fields_and_fee_detail() {
        let row = normalize_bitget_trade(
            Leg::Derivative,
            serde_json::json!({
                "execId": "131",
                "orderId": "120",
                "symbol": "BTCUSDT",
                "side": "sell",
                "execPrice": "106950.1",
                "execQty": "0.01",
                "execValue": "1069.501",
                "tradeScope": "maker",
                "feeDetail": [{"feeCoin": "USDT", "fee": "-0.6417006"}],
                "createdTime": "1750141421721",
                "execPnl": "-0.002"
            }),
        )
        .unwrap();
        assert_eq!(row.trade_id, "131");
        assert_eq!(row.quantity, "0.01");
        assert_eq!(row.fee_amount, "-0.6417006");
        assert_eq!(row.fee_asset, "USDT");
        assert_eq!(row.role, "maker");
        assert_eq!(row.realized_pnl.as_deref(), Some("-0.002"));
    }

    #[test]
    fn non_okx_trade_fees_preserve_rebate_sign() {
        let binance = normalize_binance_trade(
            Leg::Derivative,
            serde_json::json!({
                "symbol": "BTCUSDT",
                "id": "1",
                "orderId": "2",
                "side": "BUY",
                "price": "100000",
                "qty": "0.01",
                "commission": "-0.10",
                "commissionAsset": "USDT",
                "time": 1750141421721_i64
            }),
        )
        .unwrap();
        assert_eq!(binance.fee_amount, "-0.10");

        let bybit = normalize_bybit_trade(
            Leg::Derivative,
            serde_json::json!({
                "symbol": "BTCUSDT",
                "execId": "1",
                "orderId": "2",
                "side": "Buy",
                "execPrice": "100000",
                "execQty": "0.01",
                "execFee": "-0.10",
                "feeCurrency": "USDT",
                "execTime": "1750141421721"
            }),
        )
        .unwrap();
        assert_eq!(bybit.fee_amount, "-0.10");

        let gate = normalize_gate_trade(
            Leg::Derivative,
            serde_json::json!({
                "contract": "BTC_USDT",
                "trade_id": "1",
                "order_id": "2",
                "size": "1",
                "price": "100000",
                "fee": "-0.10",
                "create_time_ms": 1750141421721_i64
            }),
            &HashMap::from([("BTC_USDT".to_string(), 0.001)]),
        )
        .unwrap();
        assert_eq!(gate.fee_amount, "-0.10");
    }

    #[test]
    fn binance_wallet_distribution_normalizes_as_rebate() {
        let row = normalize_binance_rebate(serde_json::json!({
            "id": 1637366104_i64,
            "amount": "0.2267047",
            "asset": "USDT",
            "divTime": 1784795703000_i64,
            "enInfo": "Spot MM Rebate-Spot 26-07-23 07:00",
            "tranId": 2968885920_i64,
            "direction": 1
        }))
        .unwrap();
        assert_eq!(row.record_id, "1637366104");
        assert_eq!(row.transaction_id.as_deref(), Some("2968885920"));
        assert_eq!(row.amount, "0.2267047");
        assert_eq!(row.asset, "USDT");
        assert_eq!(row.direction, Some(1));
        assert_eq!(row.event_time_ms, 1784795703000_i64);
    }

    #[test]
    fn okx_trade_fee_is_converted_to_signed_cost() {
        let normalize = |fee: &str| {
            normalize_okx_trade(
                Leg::Derivative,
                serde_json::json!({
                    "instId": "BTC-USDT-SWAP",
                    "tradeId": "1",
                    "ordId": "2",
                    "side": "buy",
                    "execType": "M",
                    "fillPx": "100000",
                    "fillSz": "0.01",
                    "fee": fee,
                    "feeCcy": "USDT",
                    "ts": "1750141421721"
                }),
            )
            .unwrap()
            .fee_amount
        };

        assert_eq!(normalize("-0.10"), "0.10");
        assert_eq!(normalize("0.05"), "-0.05");
        assert_eq!(normalize("0"), "0");
    }

    #[test]
    fn bitget_cash_outflow_is_negative() {
        let row = normalize_cash(
            "bitget",
            Dataset::Interest,
            serde_json::json!({
                "id": "91",
                "symbol": "BTCUSDT",
                "coin": "USDT",
                "type": "INTEREST_SETTLEMENT_OUT",
                "amount": "0.125",
                "ts": "1750141421721"
            }),
        )
        .unwrap();
        assert_eq!(row.amount, "-0.125");
    }
}
