use anyhow::{Context, Result, bail};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use std::{
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;
use tracing::{error, info, warn};

const DEFAULT_SYNC_INTERVAL_SECS: u64 = 900;
const SYNC_INTERVAL_ENV: &str = "CRYPTO_NAV_LIVE_SYNC_SECS";
const DEFAULT_REDIS_CLI: &str = "/usr/bin/redis-cli";
const DEFAULT_REDIS_HOST: &str = "127.0.0.1";
const DEFAULT_REDIS_PORT: &str = "6379";
const DEFAULT_REDIS_DB: &str = "0";
const FR_ONLINE_LISTS: [&str; 5] = [
    "dump_symbols",
    "pos_dump_symbols",
    "fwd_trade_symbols",
    "bwd_trade_symbols",
    "unimmr_close_symbols",
];
const INTRA_ONLINE_LISTS: [&str; 3] = ["dump_symbols", "fwd_trade_symbols", "bwd_trade_symbols"];

#[derive(Clone, Debug)]
struct LiveHistoryConfig {
    sync_interval: Duration,
    redis_cli: PathBuf,
    redis_host: String,
    redis_port: String,
    redis_db: String,
    sync_history: PathBuf,
    alignment_check: PathBuf,
    order_synthesis: PathBuf,
}

#[derive(Clone, Debug, FromRow)]
struct LiveHistoryStrategy {
    slug: String,
    env_path: String,
    exchange: String,
    strategy_kind: String,
    schedule_offset_minutes: i64,
}

#[derive(Debug)]
struct SyncReport {
    slug: String,
    symbol_count: usize,
    summaries: Vec<String>,
    alignment_summary: Option<String>,
    synthesis_summary: Option<String>,
}

pub fn spawn(pool: PgPool) -> Result<()> {
    let Some(config) = LiveHistoryConfig::from_env()? else {
        info!("live history sync disabled");
        return Ok(());
    };
    info!(
        sync_interval_secs = config.sync_interval.as_secs(),
        clock = "UTC",
        redis_host = %config.redis_host,
        redis_port = %config.redis_port,
        redis_db = %config.redis_db,
        "live account history sync enabled"
    );
    tokio::spawn(run(pool, config));
    Ok(())
}

impl LiveHistoryConfig {
    fn from_env() -> Result<Option<Self>> {
        let sync_interval_secs = env_u64(SYNC_INTERVAL_ENV, DEFAULT_SYNC_INTERVAL_SECS)?;
        if sync_interval_secs == 0 {
            return Ok(None);
        }
        if sync_interval_secs % 60 != 0 {
            bail!("{SYNC_INTERVAL_ENV} must be a whole number of UTC minutes");
        }
        let sync_history = env::var_os("CRYPTO_NAV_SYNC_HISTORY_BIN")
            .map(PathBuf::from)
            .unwrap_or(
                env::current_exe()
                    .context("resolve NAV server executable")?
                    .with_file_name("sync_history"),
            );
        let alignment_check = env::var_os("CRYPTO_NAV_ALIGNMENT_CHECK_BIN")
            .map(PathBuf::from)
            .unwrap_or(
                env::current_exe()
                    .context("resolve NAV server executable")?
                    .with_file_name("reconcile_rocksdb"),
            );
        let order_synthesis = env::var_os("CRYPTO_NAV_INTRA_ORDER_SYNC_BIN")
            .map(PathBuf::from)
            .unwrap_or(
                env::current_exe()
                    .context("resolve NAV server executable")?
                    .with_file_name("sync_intra_orders"),
            );
        Ok(Some(Self {
            sync_interval: Duration::from_secs(sync_interval_secs),
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
            alignment_check,
            order_synthesis,
        }))
    }
}

async fn run(pool: PgPool, config: LiveHistoryConfig) {
    let strategies = loop {
        match load_strategies(&pool).await {
            Ok(strategies) if !strategies.is_empty() => break strategies,
            Ok(_) => warn!("no enabled strategies configured for live history sync"),
            Err(error) => error!(error = ?error, "load live history strategies failed"),
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    };

    let interval_minutes = (config.sync_interval.as_secs() / 60) as i64;
    let mut tasks = JoinSet::new();
    for strategy in strategies {
        if strategy.schedule_offset_minutes >= interval_minutes {
            error!(
                strategy = %strategy.slug,
                offset_minutes = strategy.schedule_offset_minutes,
                interval_minutes,
                "live history schedule offset exceeds interval; strategy disabled"
            );
            continue;
        }
        let task_config = config.clone();
        tasks.spawn(run_strategy(pool.clone(), task_config, strategy));
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(()) => warn!("live history schedule task exited unexpectedly"),
            Err(error) => error!(error = ?error, "join live history schedule task failed"),
        }
    }
}

async fn run_strategy(pool: PgPool, config: LiveHistoryConfig, strategy: LiveHistoryStrategy) {
    loop {
        let (delay, next_sync_at_ms) = match delay_until_next_slot(
            SystemTime::now(),
            config.sync_interval,
            strategy.schedule_offset_minutes,
        ) {
            Ok(schedule) => schedule,
            Err(error) => {
                error!(strategy = %strategy.slug, error = ?error, "calculate live history schedule failed");
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };
        info!(
            strategy = %strategy.slug,
            exchange = %strategy.exchange,
            utc_offset_minutes = strategy.schedule_offset_minutes,
            next_sync_at_ms,
            "live history sync scheduled"
        );
        tokio::time::sleep(delay).await;

        let automatic_alignment_enabled =
            match load_automatic_alignment_enabled(&pool, &strategy.slug).await {
                Ok(enabled) => enabled,
                Err(error) => {
                    warn!(
                        strategy = %strategy.slug,
                        error = ?error,
                        "load automatic alignment switch failed; skip alignment"
                    );
                    false
                }
            };
        let task_config = config.clone();
        let task_strategy = strategy.clone();
        match tokio::task::spawn_blocking(move || {
            sync_strategy(&task_config, task_strategy, automatic_alignment_enabled)
        })
        .await
        {
            Ok(Ok(report)) => info!(
                strategy = %report.slug,
                online_symbols = report.symbol_count,
                summaries = %report.summaries.join("; "),
                alignment = report.alignment_summary.as_deref().unwrap_or("not scheduled"),
                synthesis = report.synthesis_summary.as_deref().unwrap_or("not scheduled"),
                "live history sync complete"
            ),
            Ok(Err(error)) => {
                error!(strategy = %strategy.slug, error = ?error, "live history strategy sync failed");
            }
            Err(error) => {
                error!(strategy = %strategy.slug, error = ?error, "join live history strategy sync failed");
            }
        }
    }
}

fn delay_until_next_slot(
    now: SystemTime,
    interval: Duration,
    offset_minutes: i64,
) -> Result<(Duration, i64)> {
    let now_ms = i64::try_from(
        now.duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_millis(),
    )
    .context("system clock milliseconds overflow i64")?;
    let interval_ms =
        i64::try_from(interval.as_millis()).context("sync interval milliseconds overflow i64")?;
    let offset_ms = offset_minutes
        .checked_mul(60_000)
        .context("schedule offset milliseconds overflow")?;
    let elapsed = (now_ms - offset_ms).rem_euclid(interval_ms);
    let delay_ms = interval_ms - elapsed;
    let next_sync_at_ms = now_ms
        .checked_add(delay_ms)
        .context("next sync timestamp overflow")?;
    Ok((Duration::from_millis(delay_ms as u64), next_sync_at_ms))
}

async fn load_automatic_alignment_enabled(pool: &PgPool, slug: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT automatic_enabled FROM rocksdb_alignment_status WHERE strategy_slug=$1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("load automatic RocksDB alignment switch")?
    .unwrap_or(true))
}

async fn load_strategies(pool: &PgPool) -> Result<Vec<LiveHistoryStrategy>> {
    sqlx::query_as(
        r#"SELECT s.slug, s.env_path, s.exchange, s.strategy_kind,
                  (ROW_NUMBER() OVER (
                    PARTITION BY s.exchange ORDER BY s.sort_order, s.slug
                  ) - 1)::bigint AS schedule_offset_minutes
           FROM strategy_envs s
           WHERE enabled
             AND (
               (host = 'local' AND exchange = 'binance' AND strategy_kind = 'funding_rate')
               OR slug IN (
                 'binance-intra-arb01',
                 'binance_mm_alpha',
                 'bybit_mm_alpha',
                 'bybit-intra-arb01',
                 'bybit-intra-arb02',
                 'bitget_fr_arb02',
                 'gate_fr_arb01',
                 'gate_fr_arb02'
               )
             )
           ORDER BY exchange, schedule_offset_minutes"#,
    )
    .fetch_all(pool)
    .await
    .context("query live history strategies")
}

fn sync_strategy(
    config: &LiveHistoryConfig,
    strategy: LiveHistoryStrategy,
    automatic_alignment_enabled: bool,
) -> Result<SyncReport> {
    let mut failures = Vec::new();
    let mut summaries = Vec::new();
    let needs_online_symbols = uses_online_symbols(&strategy);
    let symbols = if needs_online_symbols {
        match load_online_symbols(config, &strategy) {
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
        }
    } else {
        Vec::new()
    };

    let trades_succeeded = if !needs_online_symbols || !symbols.is_empty() {
        match run_incremental_or_bootstrap(config, &strategy.slug, "trades", &symbols) {
            Ok(summary) => {
                summaries.push(summary);
                true
            }
            Err(error) => {
                failures.push(format!("trades: {error:#}"));
                false
            }
        }
    } else {
        false
    };

    for dataset in account_datasets(&strategy) {
        match run_incremental_or_bootstrap(config, &strategy.slug, dataset, &[]) {
            Ok(summary) => summaries.push(summary),
            Err(error) => failures.push(format!("{dataset}: {error:#}")),
        }
    }

    let mut alignment_succeeded = false;
    let alignment_summary = if trades_succeeded
        && alignment_check_enabled(&strategy.slug, automatic_alignment_enabled)
    {
        match run_alignment_check(config, &strategy.slug) {
            Ok(summary) => {
                alignment_succeeded = true;
                Some(summary)
            }
            Err(error) => {
                warn!(
                    strategy = %strategy.slug,
                    error = ?error,
                    "automatic RocksDB alignment check failed"
                );
                Some(format!("failed: {error:#}"))
            }
        }
    } else {
        None
    };

    let synthesis_summary = if alignment_succeeded && order_synthesis_enabled(&strategy.slug) {
        match run_order_synthesis(config, &strategy.slug) {
            Ok(summary) => Some(summary),
            Err(error) => {
                warn!(
                    strategy = %strategy.slug,
                    error = ?error,
                    "automatic intra order synthesis failed"
                );
                failures.push(format!("order synthesis: {error:#}"));
                Some(format!("failed: {error:#}"))
            }
        }
    } else {
        None
    };

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
        summaries,
        alignment_summary,
        synthesis_summary,
    })
}

fn alignment_check_enabled(slug: &str, automatic_enabled: bool) -> bool {
    automatic_enabled
        && matches!(
            slug,
            "binance_mm_alpha"
                | "bybit_mm_alpha"
                | "binance-intra-arb01"
                | "bybit-intra-arb01"
                | "bybit-intra-arb02"
        )
}

fn order_synthesis_enabled(slug: &str) -> bool {
    matches!(
        slug,
        "binance-intra-arb01" | "bybit-intra-arb01" | "bybit-intra-arb02"
    )
}

fn uses_online_symbols(strategy: &LiveHistoryStrategy) -> bool {
    strategy.exchange == "binance" && strategy.strategy_kind != "market_making"
}

fn run_alignment_check(config: &LiveHistoryConfig, slug: &str) -> Result<String> {
    let output = Command::new(&config.alignment_check)
        .args(["--strategy", slug, "--skip-sync", "--cleanup-on-success"])
        .output()
        .with_context(|| format!("run {} for {slug}", config.alignment_check.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} exited with {}: {}",
            config.alignment_check.display(),
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

fn run_order_synthesis(config: &LiveHistoryConfig, slug: &str) -> Result<String> {
    let output = Command::new(&config.order_synthesis)
        .args(["--strategy", slug])
        .output()
        .with_context(|| format!("run {} for {slug}", config.order_synthesis.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} exited with {}: {}",
            config.order_synthesis.display(),
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

fn account_datasets(strategy: &LiveHistoryStrategy) -> &'static [&'static str] {
    match (strategy.exchange.as_str(), strategy.strategy_kind.as_str()) {
        ("binance", "funding_rate") | ("gate", "funding_rate") => {
            &["funding", "interest", "liquidations"]
        }
        ("bitget", "funding_rate") => &["funding", "interest"],
        ("binance", "intra_exchange") => &["funding", "interest"],
        ("bybit", "intra_exchange") => &["funding", "interest"],
        _ => &[],
    }
}

fn run_incremental_or_bootstrap(
    config: &LiveHistoryConfig,
    slug: &str,
    dataset: &str,
    symbols: &[String],
) -> Result<String> {
    match run_sync_history(config, slug, dataset, symbols, false) {
        Ok(summary) => Ok(summary),
        Err(error)
            if error
                .chain()
                .any(|cause| cause.to_string().contains("is not initialized")) =>
        {
            warn!(
                strategy = slug,
                dataset, "history dataset is empty; initialize from st_ms"
            );
            run_sync_history(config, slug, dataset, symbols, true)
        }
        Err(error) => Err(error),
    }
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
    let (namespace, suffix, lists): (&str, String, &[&str]) = match strategy.strategy_kind.as_str()
    {
        "funding_rate" => (
            "fr",
            format!("{exchange}-margin_{exchange}-futures"),
            &FR_ONLINE_LISTS,
        ),
        "intra_exchange" => ("intra", exchange, &INTRA_ONLINE_LISTS),
        value => bail!("unsupported online symbol strategy kind: {value}"),
    };
    Ok(lists
        .iter()
        .map(|list| format!("{prefix}:{namespace}_{list}:{suffix}"))
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
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        LiveHistoryStrategy, account_datasets, alignment_check_enabled, delay_until_next_slot,
        normalize_online_symbol, online_symbol_keys, order_synthesis_enabled, parse_redis_mget,
        uses_online_symbols,
    };

    fn strategy(slug: &str, exchange: &str, strategy_kind: &str) -> LiveHistoryStrategy {
        LiveHistoryStrategy {
            slug: slug.to_string(),
            env_path: format!("/home/ubuntu/{slug}/env.sh"),
            exchange: exchange.to_string(),
            strategy_kind: strategy_kind.to_string(),
            schedule_offset_minutes: 0,
        }
    }

    #[test]
    fn aligns_schedules_to_fixed_utc_slots() {
        let now = UNIX_EPOCH + Duration::from_secs(8 * 3600 + 7 * 60 + 30);
        let (base_delay, base_next) =
            delay_until_next_slot(now, Duration::from_secs(900), 0).unwrap();
        assert_eq!(base_delay, Duration::from_secs(7 * 60 + 30));
        assert_eq!(base_next, (8 * 3600 + 15 * 60) * 1_000);

        let (staggered_delay, staggered_next) =
            delay_until_next_slot(now, Duration::from_secs(900), 1).unwrap();
        assert_eq!(staggered_delay, Duration::from_secs(8 * 60 + 30));
        assert_eq!(staggered_next, (8 * 3600 + 16 * 60) * 1_000);
    }

    #[test]
    fn builds_mkt_signal_fr_online_keys() {
        let strategy = strategy("binance_fr_arb02", "binance", "funding_rate");
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
    fn builds_mkt_signal_intra_online_keys() {
        let strategy = strategy("binance-intra-arb01", "binance", "intra_exchange");
        assert_eq!(
            online_symbol_keys(&strategy).unwrap(),
            vec![
                "binance-intra-arb01:intra_dump_symbols:binance",
                "binance-intra-arb01:intra_fwd_trade_symbols:binance",
                "binance-intra-arb01:intra_bwd_trade_symbols:binance",
            ]
        );
    }

    #[test]
    fn selects_supported_account_datasets() {
        assert_eq!(
            account_datasets(&strategy(
                "binance-intra-arb01",
                "binance",
                "intra_exchange"
            )),
            ["funding", "interest"]
        );
        assert_eq!(
            account_datasets(&strategy("bybit-intra-arb01", "bybit", "intra_exchange")),
            ["funding", "interest"]
        );
        assert_eq!(
            account_datasets(&strategy("gate_fr_arb01", "gate", "funding_rate")),
            ["funding", "interest", "liquidations"]
        );
        assert_eq!(
            account_datasets(&strategy("bitget_fr_arb02", "bitget", "funding_rate")),
            ["funding", "interest"]
        );
    }

    #[test]
    fn limits_alignment_checks_to_reconciled_accounts() {
        assert!(alignment_check_enabled("binance_mm_alpha", true));
        assert!(alignment_check_enabled("bybit_mm_alpha", true));
        assert!(!alignment_check_enabled("bybit_mm_alpha", false));
        assert!(alignment_check_enabled("binance-intra-arb01", true));
        assert!(alignment_check_enabled("bybit-intra-arb01", true));
        assert!(alignment_check_enabled("bybit-intra-arb02", true));
        assert!(!alignment_check_enabled("binance_fr_arb03", true));
    }

    #[test]
    fn enables_order_synthesis_for_center_backed_intra() {
        assert!(order_synthesis_enabled("binance-intra-arb01"));
        assert!(order_synthesis_enabled("bybit-intra-arb01"));
        assert!(order_synthesis_enabled("bybit-intra-arb02"));
    }

    #[test]
    fn market_making_uses_symbols_already_stored_in_postgres() {
        assert!(!uses_online_symbols(&strategy(
            "binance_mm_alpha",
            "binance",
            "market_making"
        )));
        assert!(uses_online_symbols(&strategy(
            "binance-intra-arb01",
            "binance",
            "intra_exchange"
        )));
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
