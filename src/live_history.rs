use anyhow::{Context, Result, bail};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use std::{
    collections::{BTreeSet, HashMap},
    env,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tokio::{
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};
use tracing::{error, info, warn};

const DEFAULT_TRADE_INTERVAL_SECS: u64 = 60;
const DEFAULT_ACCOUNT_INTERVAL_SECS: u64 = 900;
const DEFAULT_REDIS_CLI: &str = "/usr/bin/redis-cli";
const DEFAULT_REDIS_HOST: &str = "127.0.0.1";
const DEFAULT_REDIS_PORT: &str = "6379";
const DEFAULT_REDIS_DB: &str = "0";
const ONLINE_LISTS: [&str; 5] = [
    "dump_symbols",
    "pos_dump_symbols",
    "fwd_trade_symbols",
    "bwd_trade_symbols",
    "unimmr_close_symbols",
];

#[derive(Clone, Debug)]
struct LiveHistoryConfig {
    trade_interval: Duration,
    account_interval: Option<Duration>,
    redis_cli: PathBuf,
    redis_host: String,
    redis_port: String,
    redis_db: String,
    sync_history: PathBuf,
}

#[derive(Clone, Debug, FromRow)]
struct LiveHistoryStrategy {
    slug: String,
    env_path: String,
    exchange: String,
    funding_initialized: bool,
    interest_initialized: bool,
    liquidations_initialized: bool,
}

#[derive(Debug)]
struct SyncReport {
    slug: String,
    symbol_count: usize,
    account_synced: bool,
    summaries: Vec<String>,
}

pub fn spawn(pool: PgPool) -> Result<()> {
    let Some(config) = LiveHistoryConfig::from_env()? else {
        info!("live history sync disabled");
        return Ok(());
    };
    info!(
        trade_interval_secs = config.trade_interval.as_secs(),
        account_interval_secs = config.account_interval.map(|value| value.as_secs()),
        redis_host = %config.redis_host,
        redis_port = %config.redis_port,
        redis_db = %config.redis_db,
        "live Binance FR history sync enabled"
    );
    tokio::spawn(run(pool, config));
    Ok(())
}

impl LiveHistoryConfig {
    fn from_env() -> Result<Option<Self>> {
        let trade_interval_secs = env_u64(
            "CRYPTO_NAV_LIVE_TRADE_SYNC_SECS",
            DEFAULT_TRADE_INTERVAL_SECS,
        )?;
        if trade_interval_secs == 0 {
            return Ok(None);
        }
        let account_interval_secs = env_u64(
            "CRYPTO_NAV_LIVE_ACCOUNT_SYNC_SECS",
            DEFAULT_ACCOUNT_INTERVAL_SECS,
        )?;
        let sync_history = env::var_os("CRYPTO_NAV_SYNC_HISTORY_BIN")
            .map(PathBuf::from)
            .unwrap_or(
                env::current_exe()
                    .context("resolve NAV server executable")?
                    .with_file_name("sync_history"),
            );
        Ok(Some(Self {
            trade_interval: Duration::from_secs(trade_interval_secs),
            account_interval: (account_interval_secs > 0)
                .then(|| Duration::from_secs(account_interval_secs)),
            redis_cli: env::var_os("CRYPTO_NAV_REDIS_CLI")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_REDIS_CLI)),
            redis_host: env::var("CRYPTO_NAV_REDIS_HOST")
                .unwrap_or_else(|_| DEFAULT_REDIS_HOST.to_string()),
            redis_port: env::var("CRYPTO_NAV_REDIS_PORT")
                .unwrap_or_else(|_| DEFAULT_REDIS_PORT.to_string()),
            redis_db: env::var("CRYPTO_NAV_REDIS_DB")
                .unwrap_or_else(|_| DEFAULT_REDIS_DB.to_string()),
            sync_history,
        }))
    }
}

async fn run(pool: PgPool, config: LiveHistoryConfig) {
    let mut ticker = tokio::time::interval(config.trade_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_account_attempt = HashMap::<String, Instant>::new();

    loop {
        ticker.tick().await;
        let strategies = match load_strategies(&pool).await {
            Ok(strategies) => strategies,
            Err(error) => {
                error!(error = ?error, "load live history strategies failed");
                continue;
            }
        };
        if strategies.is_empty() {
            warn!("no enabled local Binance FR strategies for live history sync");
            continue;
        }

        let now = Instant::now();
        let mut tasks = JoinSet::new();
        for strategy in strategies {
            let account_due = config.account_interval.is_some_and(|interval| {
                last_account_attempt
                    .get(&strategy.slug)
                    .is_none_or(|last| now.duration_since(*last) >= interval)
            });
            if account_due {
                last_account_attempt.insert(strategy.slug.clone(), now);
            }
            let task_config = config.clone();
            tasks.spawn_blocking(move || sync_strategy(&task_config, strategy, account_due));
        }

        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(report)) => info!(
                    strategy = %report.slug,
                    online_symbols = report.symbol_count,
                    account_synced = report.account_synced,
                    summaries = %report.summaries.join("; "),
                    "live history sync complete"
                ),
                Ok(Err(error)) => {
                    error!(error = ?error, "live history strategy sync failed");
                }
                Err(error) => {
                    error!(error = ?error, "join live history strategy sync failed");
                }
            }
        }
    }
}

async fn load_strategies(pool: &PgPool) -> Result<Vec<LiveHistoryStrategy>> {
    sqlx::query_as(
        r#"SELECT s.slug, s.env_path, s.exchange,
                  EXISTS (SELECT 1 FROM history_sync_watermarks
                          WHERE strategy_slug = s.slug AND dataset = 'funding')
                    AS funding_initialized,
                  EXISTS (SELECT 1 FROM history_sync_watermarks
                          WHERE strategy_slug = s.slug AND dataset = 'interest')
                    AS interest_initialized,
                  EXISTS (SELECT 1 FROM history_sync_watermarks
                          WHERE strategy_slug = s.slug AND dataset = 'liquidations')
                    AS liquidations_initialized
           FROM strategy_envs s
           WHERE enabled
             AND host = 'local'
             AND exchange = 'binance'
             AND strategy_kind = 'funding_rate'
           ORDER BY sort_order, slug"#,
    )
    .fetch_all(pool)
    .await
    .context("query live Binance FR strategies")
}

fn sync_strategy(
    config: &LiveHistoryConfig,
    strategy: LiveHistoryStrategy,
    account_due: bool,
) -> Result<SyncReport> {
    let mut failures = Vec::new();
    let mut summaries = Vec::new();
    let symbols = match load_online_symbols(config, &strategy) {
        Ok(symbols) if symbols.is_empty() => {
            warn!(strategy = %strategy.slug, "online symbol union is empty; skip live trades");
            Vec::new()
        }
        Ok(symbols) => symbols,
        Err(error) => {
            warn!(
                strategy = %strategy.slug,
                error = ?error,
                "load online symbols failed; skip live trades"
            );
            Vec::new()
        }
    };

    if !symbols.is_empty() {
        match run_sync_history(config, &strategy.slug, "trades", &symbols, false) {
            Ok(summary) => summaries.push(summary),
            Err(error) => failures.push(format!("trades: {error:#}")),
        }
    }

    if account_due {
        for dataset in ["funding", "interest", "liquidations"] {
            let full = match dataset {
                "funding" => !strategy.funding_initialized,
                "interest" => !strategy.interest_initialized,
                "liquidations" => !strategy.liquidations_initialized,
                _ => false,
            };
            match run_sync_history(config, &strategy.slug, dataset, &[], full) {
                Ok(summary) => summaries.push(summary),
                Err(error) => failures.push(format!("{dataset}: {error:#}")),
            }
        }
    }

    if !failures.is_empty() {
        bail!(
            "live history sync failed for {}: {}",
            strategy.slug,
            failures.join("; ")
        );
    }
    Ok(SyncReport {
        slug: strategy.slug,
        symbol_count: symbols.len(),
        account_synced: account_due,
        summaries,
    })
}

fn run_sync_history(
    config: &LiveHistoryConfig,
    slug: &str,
    dataset: &str,
    symbols: &[String],
    full: bool,
) -> Result<String> {
    let mut command = Command::new(&config.sync_history);
    command.args(["--strategy", slug, "--dataset", dataset]);
    if full {
        command.arg("--full");
    }
    for symbol in symbols {
        command.args(["--symbol", symbol]);
    }
    let output = command.output().with_context(|| {
        format!(
            "run {} for {slug} dataset={dataset}",
            config.sync_history.display()
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} exited with {}: {}",
            config.sync_history.display(),
            output.status,
            stderr.trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("completed")
        .trim()
        .to_string())
}

fn load_online_symbols(
    config: &LiveHistoryConfig,
    strategy: &LiveHistoryStrategy,
) -> Result<Vec<String>> {
    let keys = online_symbol_keys(strategy)?;
    let output = Command::new(&config.redis_cli)
        .args([
            "-h",
            &config.redis_host,
            "-p",
            &config.redis_port,
            "-n",
            &config.redis_db,
            "--json",
            "MGET",
        ])
        .args(&keys)
        .output()
        .with_context(|| format!("run {}", config.redis_cli.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "redis MGET exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    parse_redis_mget(&output.stdout)
}

fn online_symbol_keys(strategy: &LiveHistoryStrategy) -> Result<Vec<String>> {
    let prefix = Path::new(&strategy.env_path)
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("infer env prefix from {}", strategy.env_path))?
        .to_ascii_lowercase();
    let exchange = strategy.exchange.trim().to_ascii_lowercase();
    if exchange.is_empty() {
        bail!("empty exchange for {}", strategy.slug);
    }
    let suffix = format!("{exchange}-margin_{exchange}-futures");
    Ok(ONLINE_LISTS
        .iter()
        .map(|list| format!("{prefix}:fr_{list}:{suffix}"))
        .collect())
}

fn parse_redis_mget(stdout: &[u8]) -> Result<Vec<String>> {
    let values: Vec<Option<String>> =
        serde_json::from_slice(stdout).context("parse redis-cli JSON MGET response")?;
    let mut symbols = BTreeSet::new();
    for raw in values.into_iter().flatten() {
        let list: Vec<Value> =
            serde_json::from_str(&raw).context("parse online symbol Redis JSON list")?;
        for value in list {
            let text = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            if let Some(symbol) = normalize_online_symbol(&text) {
                symbols.insert(symbol);
            }
        }
    }
    Ok(symbols.into_iter().collect())
}

fn normalize_online_symbol(value: &str) -> Option<String> {
    let mut text = value.trim().to_ascii_uppercase();
    if text.is_empty() {
        return None;
    }
    if let Some((head, _)) = text.split_once('@') {
        text = head.trim().to_string();
    }
    let canonical = text.replace(['_', '/'], "-");
    let asset = if let Some(stripped) = canonical.strip_suffix("-USDT-SWAP") {
        stripped.to_string()
    } else if let Some(index) = canonical.find("-USDT-") {
        canonical[..index].to_string()
    } else if let Some(stripped) = canonical.strip_suffix("-USDT") {
        stripped.to_string()
    } else {
        let cleaned = clean_symbol_text(&canonical);
        if cleaned.ends_with("USDT") && cleaned.len() > "USDT".len() {
            cleaned[..cleaned.len() - "USDT".len()].to_string()
        } else {
            cleaned
        }
    };
    let asset = clean_symbol_text(&asset);
    (!asset.is_empty()).then(|| format!("{asset}USDT"))
}

fn clean_symbol_text(value: &str) -> String {
    value.chars().filter(char::is_ascii_alphanumeric).collect()
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("parse {name} as non-negative seconds")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("read {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LiveHistoryStrategy, normalize_online_symbol, online_symbol_keys, parse_redis_mget,
    };

    #[test]
    fn builds_mkt_signal_fr_online_keys() {
        let strategy = LiveHistoryStrategy {
            slug: "binance_fr_arb02".to_string(),
            env_path: "/home/ubuntu/binance_fr_arb02/env.sh".to_string(),
            exchange: "binance".to_string(),
            funding_initialized: true,
            interest_initialized: true,
            liquidations_initialized: true,
        };
        assert_eq!(
            online_symbol_keys(&strategy).unwrap(),
            vec![
                "binance_fr_arb02:fr_dump_symbols:binance-margin_binance-futures",
                "binance_fr_arb02:fr_pos_dump_symbols:binance-margin_binance-futures",
                "binance_fr_arb02:fr_fwd_trade_symbols:binance-margin_binance-futures",
                "binance_fr_arb02:fr_bwd_trade_symbols:binance-margin_binance-futures",
                "binance_fr_arb02:fr_unimmr_close_symbols:binance-margin_binance-futures",
            ]
        );
    }

    #[test]
    fn parses_and_normalizes_online_symbol_union() {
        let payload = serde_json::to_vec(&vec![
            Some(r#"["btc-usdt-swap","ETH_USDT@hedge"]"#),
            None,
            Some(r#"["BTCUSDT","SOL"]"#),
        ])
        .unwrap();
        assert_eq!(
            parse_redis_mget(&payload).unwrap(),
            vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]
        );
    }

    #[test]
    fn normalizes_mkt_signal_symbol_shapes() {
        assert_eq!(
            normalize_online_symbol("BTC-USDT-SWAP"),
            Some("BTCUSDT".to_string())
        );
        assert_eq!(
            normalize_online_symbol("1000PEPE_USDT@binance"),
            Some("1000PEPEUSDT".to_string())
        );
        assert_eq!(normalize_online_symbol(""), None);
    }
}
