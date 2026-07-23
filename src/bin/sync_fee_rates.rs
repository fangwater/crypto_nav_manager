use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_nav_manager::{
    exchange::{
        binance::{BinanceAccountMode, BinanceClient, BinanceCredentials},
        bitget::{BitgetClient, BitgetCredentials},
        bybit::{BybitCategory, BybitClient, BybitCredentials},
        gate::{GateClient, GateCredentials, GateFeeMarket},
        okx::{OkxClient, OkxCredentials, OkxInstrumentType},
    },
    fee_rate_store::store_trading_fee_rates,
    models::{ProductCategory, TradingFeeRate},
    rest_dispatcher::{Dispatcher, DispatcherConfig},
    rest_ip_pool::configured_or_exchange_local_ips,
    strategy_env::read_env_file,
};
use sqlx::{
    AssertSqlSafe, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
const MAX_SYMBOLS: usize = 7;
const BINANCE_MM_SYMBOLS: [&str; 6] = [
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "BNBUSDT", "DOGEUSDT",
];

#[derive(Debug, Parser)]
#[command(about = "Sync current trading fee rates for registered strategy accounts")]
struct Args {
    /// Strategy slug to sync. Repeat to select multiple; omit to sync all enabled strategies.
    #[arg(long)]
    strategy: Vec<String>,

    /// Override PostgreSQL-selected REST source IPs.
    #[arg(long)]
    local_ip: Vec<IpAddr>,

    /// Query symbols traded within this many days.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
    active_days: u32,

    /// Override active-symbol discovery. Repeat for multiple symbols.
    #[arg(long)]
    symbol: Vec<String>,

    /// Symbol used when the strategy has no stored trades.
    #[arg(long, default_value = "BTCUSDT")]
    fallback_symbol: String,
}

#[derive(Clone, Debug)]
struct Strategy {
    slug: String,
    schema: String,
    host: String,
    env_path: PathBuf,
    exchange: String,
    account_mode: String,
    strategy_kind: String,
    enabled: bool,
}

impl Strategy {
    fn includes_spot(&self) -> bool {
        self.strategy_kind != "market_making"
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let pool = connect_pool().await?;
    let strategies = load_strategies(&pool, &args.strategy).await?;
    let fallback_symbol = canonical_symbol(&args.fallback_symbol)?;
    let override_symbols = args
        .symbol
        .iter()
        .map(|symbol| canonical_symbol(symbol))
        .collect::<Result<BTreeSet<_>>>()?;
    if override_symbols.len() > MAX_SYMBOLS {
        bail!("at most {MAX_SYMBOLS} --symbol values are supported");
    }

    let mut failures = Vec::new();
    for strategy in strategies {
        let result = sync_strategy(
            &pool,
            &strategy,
            &args.local_ip,
            args.active_days,
            &override_symbols,
            &fallback_symbol,
        )
        .await;
        if let Err(error) = result {
            eprintln!("{}: {error:#}", strategy.slug);
            failures.push(strategy.slug);
        }
    }
    pool.close().await;

    if !failures.is_empty() {
        bail!(
            "{} strategy fee-rate sync(s) failed: {}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

async fn connect_pool() -> Result<PgPool> {
    let options = match env::var("CRYPTO_NAV_DATABASE_URL") {
        Ok(url) => url.parse::<PgConnectOptions>()?,
        Err(env::VarError::NotPresent) => PgConnectOptions::new()
            .host("/var/run/postgresql")
            .username("ubuntu")
            .database("crypto_nav_manager"),
        Err(error) => return Err(error.into()),
    };
    PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .context("connect PostgreSQL")
}

async fn load_strategies(pool: &PgPool, selected: &[String]) -> Result<Vec<Strategy>> {
    let rows: Vec<(String, String, String, String, String, String, String, bool)> = sqlx::query_as(
        "SELECT slug,db_schema,host,env_path,exchange,account_mode,strategy_kind,enabled \
         FROM strategy_envs ORDER BY sort_order,slug",
    )
    .fetch_all(pool)
    .await
    .context("load strategy_envs")?;

    let selected: HashSet<&str> = selected.iter().map(String::as_str).collect();
    let mut found = HashSet::new();
    let mut strategies = Vec::new();
    for (slug, schema, host, env_path, exchange, account_mode, strategy_kind, enabled) in rows {
        if selected.is_empty() {
            if !enabled {
                continue;
            }
        } else if !selected.contains(slug.as_str()) {
            continue;
        }
        if !valid_schema(&schema) {
            bail!("invalid PostgreSQL schema for {slug}: {schema}");
        }
        if !matches!(
            strategy_kind.as_str(),
            "funding_rate" | "intra_exchange" | "market_making"
        ) {
            bail!("unsupported strategy kind for {slug}: {strategy_kind}");
        }
        found.insert(slug.clone());
        strategies.push(Strategy {
            slug,
            schema,
            host,
            env_path: PathBuf::from(env_path),
            exchange: exchange.to_ascii_lowercase(),
            account_mode,
            strategy_kind,
            enabled,
        });
    }

    let missing: Vec<_> = selected
        .iter()
        .filter(|slug| !found.contains(**slug))
        .copied()
        .collect();
    if !missing.is_empty() {
        bail!("strategy not found: {}", missing.join(", "));
    }
    if strategies.is_empty() {
        bail!("no strategies selected");
    }
    Ok(strategies)
}

async fn sync_strategy(
    pool: &PgPool,
    strategy: &Strategy,
    local_ips: &[IpAddr],
    active_days: u32,
    override_symbols: &BTreeSet<String>,
    fallback_symbol: &str,
) -> Result<()> {
    ensure_fee_rate_table(pool, &strategy.schema).await?;
    let symbols = if override_symbols.is_empty() {
        if strategy.exchange == "binance" && strategy.strategy_kind == "market_making" {
            BINANCE_MM_SYMBOLS.into_iter().map(str::to_string).collect()
        } else {
            load_active_symbols(pool, strategy, active_days, fallback_symbol).await?
        }
    } else {
        override_symbols.clone()
    };
    let dispatcher = build_dispatcher(pool, &strategy.exchange, local_ips).await?;
    let credentials = read_env(&strategy.host, &strategy.env_path)?;
    let mut rates = Vec::new();
    let mut market_successes = BTreeMap::new();

    match strategy.exchange.as_str() {
        "binance" => {
            let mode = match strategy.account_mode.as_str() {
                "portfolio_margin" => BinanceAccountMode::PortfolioMargin,
                "usdm_futures" => BinanceAccountMode::UsdmFutures,
                value => bail!("unsupported Binance account mode: {value}"),
            };
            let client = BinanceClient::new(
                dispatcher,
                BinanceCredentials::new(
                    env_required(&credentials, "BINANCE_API_KEY")?,
                    env_required_any(&credentials, &["BINANCE_API_SECRET", "BINANCE_SECRET_KEY"])?,
                ),
                mode,
            );
            if strategy.includes_spot() {
                let mut successes = 0;
                for symbol in &symbols {
                    match client.spot_fee_rates(symbol).await {
                        Ok(rows) => {
                            successes += 1;
                            rates.extend(rows);
                        }
                        Err(error) => warn_symbol(strategy, "spot", symbol, &error),
                    }
                }
                market_successes.insert("spot", successes);
            }
            let mut successes = 0;
            for symbol in &symbols {
                match client.fee_rates(symbol).await {
                    Ok(rows) => {
                        successes += 1;
                        rates.extend(rows);
                    }
                    Err(error) => warn_symbol(strategy, "usdt_futures", symbol, &error),
                }
            }
            market_successes.insert("usdt_futures", successes);
        }
        "bybit" => {
            let client = BybitClient::new(
                dispatcher,
                BybitCredentials::new(
                    env_required(&credentials, "BYBIT_API_KEY")?,
                    env_required_any(&credentials, &["BYBIT_API_SECRET", "BYBIT_SECRET_KEY"])?,
                ),
            );
            if strategy.includes_spot() {
                let rows = client
                    .fee_rates(BybitCategory::Spot, None)
                    .await
                    .context("query Bybit spot fee rates")?;
                market_successes.insert("spot", usize::from(!rows.is_empty()));
                rates.extend(rows);
            }
            let rows = client
                .fee_rates(BybitCategory::Linear, None)
                .await
                .context("query Bybit linear fee rates")?;
            market_successes.insert("linear", usize::from(!rows.is_empty()));
            rates.extend(rows);
        }
        "gate" => {
            let client = GateClient::new(
                dispatcher,
                GateCredentials::new(
                    env_required(&credentials, "GATE_API_KEY")?,
                    env_required_any(&credentials, &["GATE_API_SECRET", "GATE_SECRET_KEY"])?,
                ),
            );
            if strategy.includes_spot() {
                let mut successes = 0;
                for symbol in &symbols {
                    let instrument = gate_instrument(symbol)?;
                    match client.fee_rates(GateFeeMarket::Spot, &instrument).await {
                        Ok(rows) => {
                            successes += 1;
                            rates.extend(rows);
                        }
                        Err(error) => warn_symbol(strategy, "spot", symbol, &error),
                    }
                }
                market_successes.insert("spot", successes);
            }
            let mut successes = 0;
            for symbol in &symbols {
                let instrument = gate_instrument(symbol)?;
                match client
                    .fee_rates(GateFeeMarket::UsdtFutures, &instrument)
                    .await
                {
                    Ok(rows) => {
                        successes += 1;
                        rates.extend(rows);
                    }
                    Err(error) => warn_symbol(strategy, "usdt_futures", symbol, &error),
                }
            }
            market_successes.insert("usdt_futures", successes);
        }
        "bitget" => {
            let client = BitgetClient::new(
                dispatcher,
                BitgetCredentials::new(
                    env_required(&credentials, "BITGET_API_KEY")?,
                    env_required_any(&credentials, &["BITGET_API_SECRET", "BITGET_SECRET_KEY"])?,
                    env_required_any(
                        &credentials,
                        &["BITGET_API_PASSPHRASE", "BITGET_PASSPHRASE"],
                    )?,
                ),
            )
            .with_history_request_policy(Duration::from_millis(250), 3);
            if strategy.includes_spot() {
                let mut successes = 0;
                for symbol in &symbols {
                    match client.fee_rates(ProductCategory::Margin, symbol).await {
                        Ok(rows) => {
                            successes += 1;
                            rates.extend(rows);
                        }
                        Err(error) => warn_symbol(strategy, "margin", symbol, &error),
                    }
                }
                market_successes.insert("margin", successes);
            }
            let mut successes = 0;
            for symbol in &symbols {
                match client.fee_rates(ProductCategory::UsdtFutures, symbol).await {
                    Ok(rows) => {
                        successes += 1;
                        rates.extend(rows);
                    }
                    Err(error) => warn_symbol(strategy, "usdt_futures", symbol, &error),
                }
            }
            market_successes.insert("usdt_futures", successes);
        }
        "okx" | "okex" => {
            let client = OkxClient::new(
                dispatcher,
                OkxCredentials::new(
                    env_required_any(&credentials, &["OKX_API_KEY", "OKEX_API_KEY"])?,
                    env_required_any(
                        &credentials,
                        &["OKX_API_SECRET", "OKX_SECRET_KEY", "OKEX_API_SECRET"],
                    )?,
                    env_required_any(
                        &credentials,
                        &["OKX_API_PASSPHRASE", "OKX_PASSPHRASE", "OKEX_PASSPHRASE"],
                    )?,
                ),
            );
            if strategy.includes_spot() {
                let mut successes = 0;
                for symbol in &symbols {
                    let instrument = okx_instrument(symbol)?;
                    match client
                        .fee_rates(OkxInstrumentType::Margin, &instrument)
                        .await
                    {
                        Ok(rows) => {
                            successes += 1;
                            rates.extend(rows);
                        }
                        Err(error) => warn_symbol(strategy, "margin", symbol, &error),
                    }
                }
                market_successes.insert("margin", successes);
            }
            let mut successes = 0;
            for symbol in &symbols {
                let instrument = okx_instrument(symbol)?;
                match client.fee_rates(OkxInstrumentType::Swap, &instrument).await {
                    Ok(rows) => {
                        successes += 1;
                        rates.extend(rows);
                    }
                    Err(error) => warn_symbol(strategy, "swap", symbol, &error),
                }
            }
            market_successes.insert("swap", successes);
        }
        value => bail!("unsupported exchange: {value}"),
    }

    let empty_markets: Vec<_> = market_successes
        .iter()
        .filter_map(|(market, successes)| (*successes == 0).then_some(*market))
        .collect();
    if !empty_markets.is_empty() {
        bail!(
            "no fee rates returned for market(s): {}",
            empty_markets.join(", ")
        );
    }
    if rates.is_empty() {
        bail!("exchange returned no fee rates");
    }

    let stored = store_trading_fee_rates(pool, &strategy.schema, &rates)
        .await
        .with_context(|| format!("store fee rates in {}", strategy.schema))?;
    print_summary(strategy, symbols.len(), stored, &rates);
    Ok(())
}

fn warn_symbol(strategy: &Strategy, market: &str, symbol: &str, error: &impl std::fmt::Display) {
    eprintln!(
        "{}: skip {market}/{symbol} fee rate: {error}",
        strategy.slug
    );
}

fn print_summary(strategy: &Strategy, symbol_count: usize, stored: u64, rates: &[TradingFeeRate]) {
    println!(
        "{}: exchange={}, enabled={}, symbols={}, stored={}",
        strategy.slug, strategy.exchange, strategy.enabled, symbol_count, stored
    );
    let mut groups = BTreeMap::<(&str, &str, &str), usize>::new();
    for rate in rates {
        *groups
            .entry((&rate.market, &rate.maker_rate, &rate.taker_rate))
            .or_default() += 1;
    }
    for ((market, maker, taker), count) in groups {
        println!("  market={market}, maker={maker}, taker={taker}, instruments={count}");
    }
}

async fn ensure_fee_rate_table(pool: &PgPool, schema: &str) -> Result<()> {
    sqlx::query("SELECT ensure_trading_fee_rate_storage($1)")
        .bind(schema)
        .execute(pool)
        .await
        .with_context(|| format!("ensure {schema}.trading_fee_rates"))?;
    Ok(())
}

async fn build_dispatcher(
    pool: &PgPool,
    exchange: &str,
    configured: &[IpAddr],
) -> Result<Dispatcher> {
    let local_ips = configured_or_exchange_local_ips(pool, exchange, configured.to_vec())
        .await
        .with_context(|| format!("select {exchange} REST source IPs"))?;
    let mut config = DispatcherConfig {
        local_ips,
        ..DispatcherConfig::default()
    };
    if exchange == "bitget" {
        config.max_rate_limit_retries = 0;
    }
    Dispatcher::new(config).with_context(|| format!("create {exchange} REST dispatcher"))
}

async fn load_active_symbols(
    pool: &PgPool,
    strategy: &Strategy,
    active_days: u32,
    fallback_symbol: &str,
) -> Result<BTreeSet<String>> {
    let cutoff_ms = now_ms().saturating_sub(i64::from(active_days) * DAY_MS);
    let mut symbols = BTreeSet::new();
    for table in ["trades", "trade_fills"] {
        if !table_exists(pool, &strategy.schema, table).await? {
            continue;
        }
        let time_column = if column_exists(pool, &strategy.schema, table, "event_time_ms").await? {
            Some("event_time_ms")
        } else if column_exists(pool, &strategy.schema, table, "ts").await? {
            Some("ts")
        } else {
            None
        };
        let volume_column =
            if column_exists(pool, &strategy.schema, table, "quote_quantity").await? {
                Some("quote_quantity")
            } else if column_exists(pool, &strategy.schema, table, "amountu").await? {
                Some("amountu")
            } else {
                None
            };
        let rank = volume_column
            .map(|column| format!("SUM(ABS(COALESCE({column}, 0)))"))
            .unwrap_or_else(|| "COUNT(*)".to_string());
        let sql = match time_column {
            Some(column) => format!(
                "SELECT symbol FROM {}.{} WHERE {} >= $1 AND symbol <> '' \
                 GROUP BY symbol ORDER BY {} DESC NULLS LAST, symbol LIMIT {}",
                strategy.schema, table, column, rank, MAX_SYMBOLS
            ),
            None => format!(
                "SELECT symbol FROM {}.{} WHERE symbol <> '' \
                 GROUP BY symbol ORDER BY {} DESC NULLS LAST, symbol LIMIT {}",
                strategy.schema, table, rank, MAX_SYMBOLS
            ),
        };
        let rows: Vec<String> = if time_column.is_some() {
            sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
                .bind(cutoff_ms)
                .fetch_all(pool)
                .await
        } else {
            sqlx::query_scalar(AssertSqlSafe(sql.as_str()))
                .fetch_all(pool)
                .await
        }
        .with_context(|| format!("load top active symbols from {}.{table}", strategy.schema))?;
        for symbol in rows {
            symbols.insert(canonical_symbol(&symbol)?);
        }
        if !symbols.is_empty() {
            break;
        }
    }
    if symbols.is_empty() {
        symbols.insert(fallback_symbol.to_string());
    }
    Ok(symbols)
}

async fn table_exists(pool: &PgPool, schema: &str, table: &str) -> Result<bool> {
    let qualified = format!("{schema}.{table}");
    let value: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(qualified)
        .fetch_one(pool)
        .await?;
    Ok(value.is_some())
}

async fn column_exists(pool: &PgPool, schema: &str, table: &str, column: &str) -> Result<bool> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema=$1 AND table_name=$2 AND column_name=$3)",
    )
    .bind(schema)
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

fn canonical_symbol(value: &str) -> Result<String> {
    let mut symbol: String = value
        .trim()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect();
    for suffix in ["SWAP", "PERP"] {
        if let Some(stripped) = symbol.strip_suffix(suffix) {
            symbol = stripped.to_string();
            break;
        }
    }
    if !symbol.ends_with("USDT") || symbol.len() <= 4 {
        bail!("unsupported non-USDT symbol: {value}");
    }
    Ok(symbol)
}

fn gate_instrument(symbol: &str) -> Result<String> {
    let base = symbol
        .strip_suffix("USDT")
        .filter(|base| !base.is_empty())
        .with_context(|| format!("invalid Gate USDT symbol: {symbol}"))?;
    Ok(format!("{base}_USDT"))
}

fn okx_instrument(symbol: &str) -> Result<String> {
    let base = symbol
        .strip_suffix("USDT")
        .filter(|base| !base.is_empty())
        .with_context(|| format!("invalid OKX USDT symbol: {symbol}"))?;
    Ok(format!("{base}-USDT"))
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

fn valid_schema(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_exchange_symbols() {
        assert_eq!(canonical_symbol("btc_usdt").unwrap(), "BTCUSDT");
        assert_eq!(canonical_symbol("BTC-USDT-SWAP").unwrap(), "BTCUSDT");
        assert_eq!(canonical_symbol("ethusdtperp").unwrap(), "ETHUSDT");
        assert!(canonical_symbol("BTCUSD").is_err());
    }

    #[test]
    fn formats_gate_and_okx_instruments() {
        assert_eq!(gate_instrument("BTCUSDT").unwrap(), "BTC_USDT");
        assert_eq!(okx_instrument("BTCUSDT").unwrap(), "BTC-USDT");
    }

    #[test]
    fn market_plan_excludes_spot_only_for_market_making() {
        let strategy = Strategy {
            slug: "mm".to_string(),
            schema: "mm".to_string(),
            host: "local".to_string(),
            env_path: PathBuf::new(),
            exchange: "binance".to_string(),
            account_mode: "usdm_futures".to_string(),
            strategy_kind: "market_making".to_string(),
            enabled: true,
        };
        assert!(!strategy.includes_spot());
        assert!(
            Strategy {
                strategy_kind: "intra_exchange".to_string(),
                ..strategy
            }
            .includes_spot()
        );
    }

    #[test]
    fn binance_market_making_uses_the_default_symbol_set() {
        assert_eq!(
            BINANCE_MM_SYMBOLS,
            [
                "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "BNBUSDT", "DOGEUSDT"
            ]
        );
        assert!(BINANCE_MM_SYMBOLS.len() <= MAX_SYMBOLS);
    }
}
