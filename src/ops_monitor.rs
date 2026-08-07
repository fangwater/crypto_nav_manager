use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    net::SocketAddr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

const DEFAULT_CONFIG_PATH: &str = "config/ops-monitor.json";
const DEFAULT_BIND: &str = "127.0.0.1:4210";
const DEFAULT_SCAN_INTERVAL_SECS: u64 = 5;
const DEFAULT_WARNING_WINDOW_SECS: i64 = 15 * 60;
const INITIAL_LOG_BYTES: u64 = 512 * 1024;
const MAX_LOG_READ_BYTES: u64 = 2 * 1024 * 1024;
const MAX_EVENTS_PER_COMPONENT: usize = 2_000;
const MAX_SAMPLES_PER_COMPONENT: usize = 5;
const ORDER_CONTEXT_WINDOW_MS: i64 = 30 * 60 * 1_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonitorConfig {
    bind: Option<String>,
    scan_interval_secs: Option<u64>,
    warning_window_secs: Option<i64>,
    #[serde(default)]
    max_position_threshold_whitelist_usd: Vec<f64>,
    environments: Vec<EnvironmentConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentConfig {
    slug: String,
    host: String,
    root: PathBuf,
    exchange: String,
    tag: String,
    profile: DeploymentProfile,
    snapshot_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeploymentProfile {
    FundingRate,
    IntraExchange,
    MarketMaking,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentRole {
    TradeSignal,
    PreTrade,
    AccountMonitor,
    TradeEngine,
    PersistManager,
    VizServer,
}

const COMPONENT_ROLES: [ComponentRole; 6] = [
    ComponentRole::TradeSignal,
    ComponentRole::PreTrade,
    ComponentRole::AccountMonitor,
    ComponentRole::TradeEngine,
    ComponentRole::PersistManager,
    ComponentRole::VizServer,
];

impl ComponentRole {
    fn executable_name(self, environment: &EnvironmentConfig) -> String {
        match self {
            Self::TradeSignal => "trade_signal".to_string(),
            Self::PreTrade => "pre_trade".to_string(),
            Self::AccountMonitor => match environment.profile {
                DeploymentProfile::IntraExchange => {
                    format!("account_monitor_{}", environment.exchange)
                }
                _ => "account_monitor".to_string(),
            },
            Self::TradeEngine => "trade_engine".to_string(),
            Self::PersistManager => "persist_manager".to_string(),
            Self::VizServer => "viz_server".to_string(),
        }
    }

    fn critical(self) -> bool {
        matches!(
            self,
            Self::TradeSignal | Self::PreTrade | Self::AccountMonitor | Self::TradeEngine
        )
    }

    fn short_code(self, profile: DeploymentProfile) -> &'static str {
        match self {
            Self::TradeSignal => "ts",
            Self::PreTrade => "pt",
            Self::AccountMonitor => "am",
            Self::TradeEngine => "te",
            Self::PersistManager => "pm",
            Self::VizServer if matches!(profile, DeploymentProfile::FundingRate) => "vz",
            Self::VizServer => "viz",
        }
    }
}

#[derive(Clone, Debug)]
struct ComponentSpec {
    role: ComponentRole,
    executable: PathBuf,
    manager: &'static str,
    manager_name: String,
    log_path: PathBuf,
}

impl ComponentSpec {
    fn from_environment(environment: &EnvironmentConfig, role: ComponentRole) -> Self {
        let executable = environment.root.join(role.executable_name(environment));
        let exchange_short = short_exchange(&environment.exchange);
        let manager_name = match environment.profile {
            DeploymentProfile::FundingRate => format!(
                "fr_{}_{}_{}",
                role.short_code(environment.profile),
                exchange_short,
                environment.tag
            ),
            DeploymentProfile::MarketMaking if role == ComponentRole::TradeSignal => format!(
                "mm_{}_futures_{}_trade_signal",
                environment.exchange, environment.tag
            ),
            DeploymentProfile::MarketMaking => format!(
                "mm_{}_{}_{}",
                role.short_code(environment.profile),
                environment.exchange,
                environment.tag
            ),
            DeploymentProfile::IntraExchange if role == ComponentRole::TradeSignal => format!(
                "intra_{}_{}_trade_signal",
                environment.exchange, environment.tag
            ),
            DeploymentProfile::IntraExchange => format!(
                "intra_{}_{}_{}",
                role.short_code(environment.profile),
                environment.exchange,
                environment.tag
            ),
        };
        let (manager, log_path) = if role == ComponentRole::TradeSignal {
            let file_name = manager_name.replace('_', "-");
            (
                "pm2",
                PathBuf::from(format!("/home/ubuntu/.pm2/logs/{file_name}-error.log")),
            )
        } else {
            (
                "pmdaemon",
                PathBuf::from(format!(
                    "/home/ubuntu/.pmdaemon/logs/{manager_name}-error.log"
                )),
            )
        };

        Self {
            role,
            executable,
            manager,
            manager_name,
            log_path,
        }
    }
}

fn short_exchange(exchange: &str) -> &str {
    match exchange {
        "binance" => "bn",
        "bybit" => "bb",
        "bitget" => "bg",
        "gate" => "gt",
        "okex" | "okx" => "ok",
        other => other,
    }
}

impl MonitorConfig {
    fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("read ops monitor config {}", path.display()))?;
        let config: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse ops monitor config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.environments.is_empty() {
            bail!("ops monitor config has no environments");
        }
        if self
            .max_position_threshold_whitelist_usd
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            bail!("max position threshold whitelist must contain positive finite USD values");
        }
        let mut slugs = HashSet::new();
        for environment in &self.environments {
            if !slugs.insert(&environment.slug) {
                bail!("duplicate ops monitor slug: {}", environment.slug);
            }
            if environment.host != "local" {
                bail!(
                    "unsupported host {} for {}; deploy a host probe before enabling it",
                    environment.host,
                    environment.slug
                );
            }
            if !environment.root.is_absolute() {
                bail!("environment root must be absolute: {}", environment.slug);
            }
            if environment.exchange.is_empty() || environment.tag.is_empty() {
                bail!("environment exchange/tag missing: {}", environment.slug);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ProcessEntry {
    pid: u32,
    executable: PathBuf,
    state: char,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewResponse {
    generated_at_ms: i64,
    environments: Vec<EnvironmentStatusResponse>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentStatusResponse {
    strategy_slug: String,
    host: String,
    profile: DeploymentProfile,
    status: EnvironmentHealth,
    components: Vec<ComponentStatusResponse>,
    trading_blocks: Vec<TradingBlockResponse>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentHealth {
    Healthy,
    Warning,
    Critical,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatusResponse {
    role: ComponentRole,
    critical: bool,
    status: ComponentHealth,
    pid: Option<u32>,
    instances: usize,
    linux_state: Option<String>,
    manager: &'static str,
    manager_name: String,
    alerts: AlertSummaryResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentHealth {
    Online,
    Warning,
    Offline,
    Duplicate,
    Zombie,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertSummaryResponse {
    warning_count: usize,
    error_count: usize,
    last_alert_at_ms: Option<i64>,
    truncated: bool,
    samples: Vec<AlertSampleResponse>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertSampleResponse {
    severity: AlertSeverity,
    at_ms: i64,
    count: usize,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingLeg {
    Margin,
    Futures,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionStatus {
    Live,
    Unavailable,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentPositionResponse {
    margin_qty: f64,
    futures_qty: f64,
    net_qty: f64,
    margin_usd: f64,
    futures_usd: f64,
    net_usd: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TradingBlockResponse {
    symbol: String,
    asset: String,
    blocked_leg: TradingLeg,
    venue: String,
    side: String,
    order_qty: Option<f64>,
    order_price: Option<f64>,
    http_status: Option<u16>,
    error_code: Option<i64>,
    error_label: String,
    error_message: String,
    first_seen_at_ms: i64,
    last_seen_at_ms: i64,
    count: usize,
    latest_client_order_id: String,
    position_status: PositionStatus,
    position_snapshot_at_ms: Option<i64>,
    position_error: Option<String>,
    current_position: Option<CurrentPositionResponse>,
}

#[derive(Clone, Debug)]
struct LogEvent {
    severity: AlertSeverity,
    at_ms: i64,
    message: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct OrderContext {
    at_ms: i64,
    client_order_id: String,
    symbol: String,
    venue: String,
    side: String,
    qty: Option<f64>,
    price: Option<f64>,
}

#[derive(Clone, Debug)]
struct TradingBlockFailure {
    at_ms: i64,
    client_order_id: String,
    http_status: Option<u16>,
    error_code: Option<i64>,
    error_label: String,
    error_message: String,
}

#[derive(Debug)]
struct ExposureSnapshot {
    at_ms: i64,
    positions: HashMap<String, CurrentPositionResponse>,
}

#[derive(Debug, Default)]
struct LogCursor {
    inode: u64,
    offset: u64,
    partial: String,
}

#[derive(Debug, Default)]
struct LogMonitor {
    cursors: HashMap<PathBuf, LogCursor>,
    events: HashMap<(String, ComponentRole), VecDeque<LogEvent>>,
    truncated: HashMap<(String, ComponentRole), bool>,
    orders: HashMap<(String, String), OrderContext>,
    trading_block_failures: HashMap<(String, String), TradingBlockFailure>,
}

impl LogMonitor {
    fn scan(
        &mut self,
        slug: &str,
        role: ComponentRole,
        path: &Path,
        max_position_threshold_whitelist_usd: &[f64],
        now_ms: i64,
        window_ms: i64,
    ) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        let inode = metadata.ino();
        let file_len = metadata.len();
        let (cursor_inode, cursor_offset, cursor_partial) = self
            .cursors
            .get(path)
            .map(|cursor| (cursor.inode, cursor.offset, cursor.partial.clone()))
            .unwrap_or_default();
        let reset = cursor_inode != inode || file_len < cursor_offset;
        let mut start = if cursor_inode == 0 || reset {
            file_len.saturating_sub(INITIAL_LOG_BYTES)
        } else {
            cursor_offset
        };
        if file_len.saturating_sub(start) > MAX_LOG_READ_BYTES {
            start = file_len.saturating_sub(MAX_LOG_READ_BYTES);
            self.truncated.insert((slug.to_string(), role), true);
        }

        let Ok(mut file) = File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
        let mut bytes = Vec::with_capacity((file_len.saturating_sub(start)) as usize);
        if file
            .take(MAX_LOG_READ_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return;
        }

        let starts_mid_file = start > 0 && (cursor_inode == 0 || reset || start != cursor_offset);
        let mut text = String::new();
        if !starts_mid_file {
            text.push_str(&cursor_partial);
        }
        text.push_str(&String::from_utf8_lossy(&bytes));
        if starts_mid_file {
            if let Some(newline) = text.find('\n') {
                text.drain(..=newline);
            } else {
                text.clear();
            }
        }

        let complete_len = text.rfind('\n').map_or(0, |index| index + 1);
        let partial = text.split_off(complete_len);
        let mut parsed_events = Vec::new();
        for line in text.lines() {
            self.observe_trading_line(slug, line, now_ms, window_ms);
            if is_whitelisted_max_position_rejection(line, max_position_threshold_whitelist_usd) {
                continue;
            }
            if let Some(event) = parse_log_event(line, now_ms, window_ms) {
                parsed_events.push(event);
            }
        }

        let key = (slug.to_string(), role);
        let queue = self.events.entry(key.clone()).or_default();
        queue.extend(parsed_events);
        while queue
            .front()
            .is_some_and(|event| now_ms - event.at_ms > window_ms)
        {
            queue.pop_front();
        }
        if queue.len() > MAX_EVENTS_PER_COMPONENT {
            let remove_count = queue.len() - MAX_EVENTS_PER_COMPONENT;
            queue.drain(..remove_count);
            self.truncated.insert(key, true);
        }

        self.cursors.insert(
            path.to_path_buf(),
            LogCursor {
                inode,
                offset: start.saturating_add(bytes.len() as u64),
                partial,
            },
        );
        self.prune_trading_state(now_ms, window_ms);
    }

    fn observe_trading_line(&mut self, slug: &str, line: &str, now_ms: i64, window_ms: i64) {
        let Some(at_ms) = parse_log_timestamp(line) else {
            return;
        };
        if now_ms - at_ms > ORDER_CONTEXT_WINDOW_MS || at_ms - now_ms > 5 * 60 * 1_000 {
            return;
        }
        if let Some(order) = parse_order_context(line, at_ms) {
            let key = (slug.to_string(), order.client_order_id.clone());
            self.orders
                .entry(key)
                .and_modify(|existing| {
                    existing.at_ms = existing.at_ms.max(order.at_ms);
                    existing.symbol.clone_from(&order.symbol);
                    existing.venue.clone_from(&order.venue);
                    existing.side.clone_from(&order.side);
                    if order.qty.is_some() {
                        existing.qty = order.qty;
                    }
                    if order.price.is_some() {
                        existing.price = order.price;
                    }
                })
                .or_insert(order);
        }
        if now_ms - at_ms <= window_ms
            && let Some(failure) = parse_trading_block_failure(line, at_ms)
        {
            self.trading_block_failures
                .insert((slug.to_string(), failure.client_order_id.clone()), failure);
        }
    }

    fn prune_trading_state(&mut self, now_ms: i64, window_ms: i64) {
        self.orders
            .retain(|_, order| now_ms - order.at_ms <= ORDER_CONTEXT_WINDOW_MS);
        self.trading_block_failures
            .retain(|_, failure| now_ms - failure.at_ms <= window_ms);
    }

    fn trading_blocks(&self, slug: &str) -> Vec<TradingBlockResponse> {
        let mut groups: HashMap<(String, TradingLeg, String), TradingBlockResponse> =
            HashMap::new();
        for ((failure_slug, client_order_id), failure) in &self.trading_block_failures {
            if failure_slug != slug {
                continue;
            }
            let Some(order) = self
                .orders
                .get(&(failure_slug.clone(), client_order_id.clone()))
            else {
                continue;
            };
            let blocked_leg = trading_leg(&order.venue);
            let asset = symbol_asset(&order.symbol);
            let group = groups
                .entry((
                    order.symbol.clone(),
                    blocked_leg,
                    failure.error_label.clone(),
                ))
                .or_insert_with(|| TradingBlockResponse {
                    symbol: order.symbol.clone(),
                    asset,
                    blocked_leg,
                    venue: order.venue.clone(),
                    side: order.side.clone(),
                    order_qty: order.qty,
                    order_price: order.price,
                    http_status: failure.http_status,
                    error_code: failure.error_code,
                    error_label: failure.error_label.clone(),
                    error_message: failure.error_message.clone(),
                    first_seen_at_ms: failure.at_ms,
                    last_seen_at_ms: failure.at_ms,
                    count: 0,
                    latest_client_order_id: failure.client_order_id.clone(),
                    position_status: PositionStatus::Unavailable,
                    position_snapshot_at_ms: None,
                    position_error: None,
                    current_position: None,
                });
            group.count += 1;
            group.first_seen_at_ms = group.first_seen_at_ms.min(failure.at_ms);
            if failure.at_ms >= group.last_seen_at_ms {
                group.last_seen_at_ms = failure.at_ms;
                group
                    .latest_client_order_id
                    .clone_from(&failure.client_order_id);
                group.venue.clone_from(&order.venue);
                group.side.clone_from(&order.side);
                group.order_qty = order.qty;
                group.order_price = order.price;
                group.http_status = failure.http_status;
                group.error_code = failure.error_code;
                group.error_message.clone_from(&failure.error_message);
            }
        }
        let mut blocks: Vec<_> = groups.into_values().collect();
        blocks.sort_by_key(|block| std::cmp::Reverse(block.last_seen_at_ms));
        blocks
    }

    fn summary(&self, slug: &str, role: ComponentRole) -> AlertSummaryResponse {
        let key = (slug.to_string(), role);
        let Some(events) = self.events.get(&key) else {
            return AlertSummaryResponse::default();
        };
        let mut groups: HashMap<(AlertSeverity, String), AlertSampleResponse> = HashMap::new();
        for event in events {
            let group = groups
                .entry((event.severity, event.fingerprint.clone()))
                .or_insert_with(|| AlertSampleResponse {
                    severity: event.severity,
                    at_ms: event.at_ms,
                    count: 0,
                    message: event.message.clone(),
                });
            group.count += 1;
            if event.at_ms >= group.at_ms {
                group.at_ms = event.at_ms;
                group.message.clone_from(&event.message);
            }
        }
        let mut samples: Vec<_> = groups.into_values().collect();
        samples.sort_by_key(|sample| std::cmp::Reverse(sample.at_ms));
        samples.truncate(MAX_SAMPLES_PER_COMPONENT);

        AlertSummaryResponse {
            warning_count: events
                .iter()
                .filter(|event| event.severity == AlertSeverity::Warning)
                .count(),
            error_count: events
                .iter()
                .filter(|event| event.severity == AlertSeverity::Error)
                .count(),
            last_alert_at_ms: events.back().map(|event| event.at_ms),
            truncated: self.truncated.get(&key).copied().unwrap_or(false),
            samples,
        }
    }
}

struct Monitor {
    config: MonitorConfig,
    logs: LogMonitor,
    snapshot_client: reqwest::blocking::Client,
}

impl Monitor {
    fn new(config: MonitorConfig) -> Self {
        let snapshot_client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(3))
            .build()
            .expect("build snapshot HTTP client");
        Self {
            config,
            logs: LogMonitor::default(),
            snapshot_client,
        }
    }

    fn refresh(&mut self) -> OverviewResponse {
        let now_ms = Utc::now().timestamp_millis();
        let window_ms = self
            .config
            .warning_window_secs
            .unwrap_or(DEFAULT_WARNING_WINDOW_SECS)
            * 1_000;
        let processes = scan_processes();
        let mut environments = Vec::with_capacity(self.config.environments.len());

        for environment in &self.config.environments {
            let mut components = Vec::with_capacity(COMPONENT_ROLES.len());
            for role in COMPONENT_ROLES {
                let spec = ComponentSpec::from_environment(environment, role);
                self.logs.scan(
                    &environment.slug,
                    role,
                    &spec.log_path,
                    &self.config.max_position_threshold_whitelist_usd,
                    now_ms,
                    window_ms,
                );
                let alerts = self.logs.summary(&environment.slug, role);
                let expected = canonical_or_original(&spec.executable);
                let matching: Vec<_> = processes
                    .iter()
                    .filter(|process| process.executable == expected)
                    .collect();
                let process_health = if matching.len() > 1 {
                    ComponentHealth::Duplicate
                } else if matching.first().is_some_and(|process| process.state == 'Z') {
                    ComponentHealth::Zombie
                } else if matching.is_empty() {
                    ComponentHealth::Offline
                } else if matching.first().is_some_and(|process| process.state == 'D')
                    || alerts.warning_count > 0
                    || alerts.error_count > 0
                {
                    ComponentHealth::Warning
                } else {
                    ComponentHealth::Online
                };
                components.push(ComponentStatusResponse {
                    role: spec.role,
                    critical: role.critical(),
                    status: process_health,
                    pid: matching.first().map(|process| process.pid),
                    instances: matching.len(),
                    linux_state: matching.first().map(|process| process.state.to_string()),
                    manager: spec.manager,
                    manager_name: spec.manager_name,
                    alerts,
                });
            }

            let mut trading_blocks = self.logs.trading_blocks(&environment.slug);
            if !trading_blocks.is_empty() {
                match fetch_exposure_snapshot(&self.snapshot_client, environment) {
                    Ok(snapshot) => {
                        for block in &mut trading_blocks {
                            block.position_status = PositionStatus::Live;
                            block.position_snapshot_at_ms = Some(snapshot.at_ms);
                            block.current_position = Some(
                                snapshot
                                    .positions
                                    .get(&block.asset)
                                    .cloned()
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    Err(reason) => {
                        let message = truncate_chars(&format!("{reason:#}"), 240);
                        for block in &mut trading_blocks {
                            block.position_error = Some(message.clone());
                        }
                    }
                }
            }
            let status = environment_health(&components);
            environments.push(EnvironmentStatusResponse {
                strategy_slug: environment.slug.clone(),
                host: environment.host.clone(),
                profile: environment.profile,
                status,
                components,
                trading_blocks,
            });
        }

        OverviewResponse {
            generated_at_ms: now_ms,
            environments,
        }
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn scan_processes() -> Vec<ProcessEntry> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_string_lossy().parse::<u32>().ok()?;
            let process_dir = entry.path();
            let executable_link = fs::read_link(process_dir.join("exe")).ok()?;
            let executable_text = executable_link.to_string_lossy();
            let executable = PathBuf::from(
                executable_text
                    .strip_suffix(" (deleted)")
                    .unwrap_or(&executable_text),
            );
            let stat = fs::read_to_string(process_dir.join("stat")).ok()?;
            let close_paren = stat.rfind(')')?;
            let state = stat[close_paren + 1..]
                .split_whitespace()
                .next()?
                .chars()
                .next()?;
            Some(ProcessEntry {
                pid,
                executable,
                state,
            })
        })
        .collect()
}

fn environment_health(components: &[ComponentStatusResponse]) -> EnvironmentHealth {
    if components.iter().any(|component| {
        component.critical
            && matches!(
                component.status,
                ComponentHealth::Offline | ComponentHealth::Duplicate | ComponentHealth::Zombie
            )
    }) {
        EnvironmentHealth::Critical
    } else if components
        .iter()
        .any(|component| component.status != ComponentHealth::Online)
    {
        EnvironmentHealth::Warning
    } else {
        EnvironmentHealth::Healthy
    }
}

fn parse_log_timestamp(line: &str) -> Option<i64> {
    let timestamp_end = line.find('Z')?;
    let timestamp_start = line.find('[')? + 1;
    DateTime::parse_from_rfc3339(&line[timestamp_start..=timestamp_end])
        .ok()
        .map(|value| value.timestamp_millis())
}

fn field_token<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    let start = value.find(marker)? + marker.len();
    let tail = &value[start..];
    let end = tail
        .find(|character: char| {
            character.is_whitespace() || matches!(character, ',' | ')' | ']' | '}')
        })
        .unwrap_or(tail.len());
    Some(&tail[..end])
}

fn numeric_field(value: &str, marker: &str) -> Option<i64> {
    let token = field_token(value, marker)?;
    let mut chars = token.chars().peekable();
    let mut number = String::new();
    if chars.peek() == Some(&'-') {
        number.push('-');
        chars.next();
    }
    number.extend(chars.take_while(char::is_ascii_digit));
    if number.is_empty() || number == "-" {
        None
    } else {
        number.parse().ok()
    }
}

fn parse_order_context(line: &str, at_ms: i64) -> Option<OrderContext> {
    if !line.contains("订单已创建") && !line.contains("OrderSlowTrace") {
        return None;
    }
    let client_order_id = field_token(line, "client_order_id=")?.to_string();
    let symbol_start = line.find("symbol=")? + "symbol=".len();
    let symbol_tail = &line[symbol_start..];
    let mut symbol_parts = symbol_tail.split_whitespace();
    let symbol = symbol_parts.next()?.to_string();
    let venue = field_token(line, "venue=")
        .map(str::to_string)
        .or_else(|| symbol_parts.next().map(str::to_string))?;
    let side = field_token(line, "side=")?.to_string();
    let qty = field_token(line, "qty=").and_then(|value| value.parse().ok());
    let price = field_token(line, "price=").and_then(|value| value.parse().ok());
    Some(OrderContext {
        at_ms,
        client_order_id,
        symbol,
        venue,
        side,
        qty,
        price,
    })
}

fn parse_trading_block_failure(line: &str, at_ms: i64) -> Option<TradingBlockFailure> {
    if !line.contains("RISK_CHECK_MARKET_FORBIDDEN")
        && !line.contains("code=-100510")
        && !line.contains("Risk check prohibits market order")
    {
        return None;
    }
    let client_order_id = field_token(line, "client_order_id=")
        .or_else(|| field_token(line, "cli_ord_id="))?
        .to_string();
    Some(TradingBlockFailure {
        at_ms,
        client_order_id,
        http_status: numeric_field(line, "status=").and_then(|value| value.try_into().ok()),
        error_code: numeric_field(line, "code="),
        error_label: "RISK_CHECK_MARKET_FORBIDDEN".to_string(),
        error_message: "Risk check prohibits this trading market".to_string(),
    })
}

fn trading_leg(venue: &str) -> TradingLeg {
    let venue = venue.to_ascii_lowercase();
    if venue.contains("margin") || venue.contains("spot") {
        TradingLeg::Margin
    } else if venue.contains("futures") || venue.contains("swap") {
        TradingLeg::Futures
    } else {
        TradingLeg::Unknown
    }
}

fn symbol_asset(symbol: &str) -> String {
    let normalized = symbol.replace(['_', '-'], "");
    for quote in ["USDT", "USDC", "USD"] {
        if let Some(asset) = normalized.strip_suffix(quote) {
            return asset.to_string();
        }
    }
    normalized
}

fn json_f64(value: Option<&serde_json::Value>) -> f64 {
    value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .unwrap_or(0.0)
}

fn parse_exposure_snapshot(payload: &serde_json::Value) -> Result<ExposureSnapshot> {
    let entries = payload
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .context("snapshot entries are missing")?;
    let exposure = entries
        .iter()
        .find(|entry| {
            entry.get("channel").and_then(serde_json::Value::as_str) == Some("pre_trade_exposure")
        })
        .context("snapshot pre_trade_exposure entry is missing")?;
    let entry = exposure
        .get("entry")
        .context("snapshot exposure payload is missing")?;
    let rows = entry
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .context("snapshot exposure rows are missing")?;
    let at_ms = entry
        .get("ts_ms")
        .or_else(|| exposure.get("ts_ms"))
        .or_else(|| payload.get("ts_ms"))
        .and_then(serde_json::Value::as_i64)
        .context("snapshot exposure timestamp is missing")?;
    let mut positions = HashMap::new();
    for row in rows {
        let Some(asset) = row.get("asset").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if asset == "TOTAL"
            || row.get("is_total").and_then(serde_json::Value::as_bool) == Some(true)
        {
            continue;
        }
        positions.insert(
            asset.to_string(),
            CurrentPositionResponse {
                margin_qty: json_f64(row.get("open_qty")),
                futures_qty: json_f64(row.get("hedge_qty")),
                net_qty: json_f64(row.get("net_qty")),
                margin_usd: json_f64(row.get("open_usdt")),
                futures_usd: json_f64(row.get("hedge_usdt")),
                net_usd: json_f64(row.get("net_usdt")),
            },
        );
    }
    Ok(ExposureSnapshot { at_ms, positions })
}

fn fetch_exposure_snapshot(
    client: &reqwest::blocking::Client,
    environment: &EnvironmentConfig,
) -> Result<ExposureSnapshot> {
    let url = environment
        .snapshot_url
        .as_deref()
        .context("snapshot URL is not configured")?;
    let payload = client
        .get(url)
        .send()
        .with_context(|| format!("request snapshot for {}", environment.slug))?
        .error_for_status()
        .with_context(|| format!("snapshot status for {}", environment.slug))?
        .json()
        .with_context(|| format!("decode snapshot for {}", environment.slug))?;
    parse_exposure_snapshot(&payload)
}

fn is_whitelisted_max_position_rejection(line: &str, whitelist_usd: &[f64]) -> bool {
    if !line.contains("下单后持仓") || !line.contains("超过阈值") {
        return false;
    }
    let Some(threshold) = usdt_value_after(line, "超过阈值 ").map(f64::abs) else {
        return false;
    };
    whitelist_usd.iter().any(|allowed| {
        let tolerance = allowed.abs().max(1.0) * 1e-6;
        allowed.is_finite() && *allowed > 0.0 && (threshold - *allowed).abs() <= tolerance
    })
}

fn is_expected_cancel_race(line: &str) -> bool {
    let cancel_request =
        (line.contains("Cancel") && line.contains("Order")) || line.contains("cancel_order");
    let unknown_order = line.contains("code=-2011") || line.contains("code=Some(-2011)");
    cancel_request && unknown_order
}

fn is_generic_http_classification(line: &str) -> bool {
    line.contains("trade_engine::dispatcher] http classify:")
}

fn is_unlocked_account_open_block(line: &str) -> bool {
    line.contains("mkt_signal::pre_trade::account_open_block]")
        && line.contains("AccountOpenBlock:")
        && field_token(line, "state=") == Some("unlocked")
}

fn parse_log_event(line: &str, now_ms: i64, window_ms: i64) -> Option<LogEvent> {
    let (severity, marker) = if line.contains(" ERROR ") {
        (AlertSeverity::Error, " ERROR ")
    } else if line.contains(" WARN  ") {
        (AlertSeverity::Warning, " WARN  ")
    } else if line.contains(" WARNING ") {
        (AlertSeverity::Warning, " WARNING ")
    } else {
        return None;
    };
    if is_expected_position_limit_rejection(line)
        || is_expected_binance_position_limit_rejection(line)
        || is_expected_leverage_rejection(line)
        || is_expected_cancel_race(line)
        || is_generic_http_classification(line)
        || is_unlocked_account_open_block(line)
    {
        return None;
    }
    let timestamp = parse_log_timestamp(line)?;
    if now_ms - timestamp > window_ms || timestamp - now_ms > 5 * 60 * 1_000 {
        return None;
    }
    let marker_index = line.find(marker)?;
    let message = redact_and_truncate(&line[marker_index + marker.len()..]);
    let fingerprint = fingerprint(&message);
    Some(LogEvent {
        severity,
        at_ms: timestamp,
        message,
        fingerprint,
    })
}

fn usdt_value_after<'a>(line: &'a str, marker: &str) -> Option<f64> {
    let tail = line.split_once(marker)?.1;
    let value = tail.split_once("USDT")?.0;
    value
        .rsplit_once('(')
        .map_or(value, |(_, usd)| usd)
        .trim()
        .parse()
        .ok()
}

fn number_after(line: &str, marker: &str) -> Option<f64> {
    let tail = line.split_once(marker)?.1.trim_start();
    let end = tail
        .find(|character: char| {
            !(character.is_ascii_digit() || matches!(character, '.' | '-' | '+'))
        })
        .unwrap_or(tail.len());
    tail[..end].parse().ok()
}

fn is_expected_binance_position_limit_rejection(line: &str) -> bool {
    let direct_rejection = line.contains("Binance限仓检查失败:");
    let summarized_rejection = line.contains("mkt_signal::pre_trade::log_throttle]")
        && line.contains("reason=binance position-limit failed:");
    if !line.contains("Binance FR position-limit cap exceeded")
        || (!direct_rejection && !summarized_rejection)
    {
        return false;
    }
    let open_next = usdt_value_after(line, "open_next=").map(f64::abs);
    let futures_next = usdt_value_after(line, "futures_next=").map(f64::abs);
    let cap = usdt_value_after(line, "cap=").map(f64::abs);
    let (Some(open_next), Some(futures_next), Some(cap)) = (open_next, futures_next, cap) else {
        return false;
    };
    let largest_leg = open_next.max(futures_next);
    cap > 0.0
        && largest_leg > cap
        && largest_leg <= cap * 1.05
        && (open_next - futures_next).abs() <= cap * 0.01
}

fn is_expected_leverage_rejection(line: &str) -> bool {
    let relevant = line.contains("杠杆风控检查失败")
        || (line.contains("mkt_signal::pre_trade::log_throttle]")
            && line.contains("reason_class=leverage")
            && line.contains("leverage risk failed:"));
    if !relevant {
        return false;
    }
    let actual = number_after(line, "杠杆率 ").map(f64::abs);
    let limit = number_after(line, "超过限制 ").map(f64::abs);
    matches!((actual, limit), (Some(actual), Some(limit))
        if limit > 0.0 && actual > limit && actual <= limit * 1.05)
}

fn is_expected_position_limit_rejection(line: &str) -> bool {
    let projected = usdt_value_after(line, "下单后持仓=")
        .or_else(|| usdt_value_after(line, "下单后持仓 "))
        .map(f64::abs);
    let limit = usdt_value_after(line, "超过阈值 ").map(f64::abs);
    let (Some(projected), Some(limit)) = (projected, limit) else {
        return false;
    };
    if limit <= 0.0 || projected <= limit {
        return false;
    }

    let projected_is_near_limit = projected <= limit * 1.05;
    let summarized_rejection = line.contains("mkt_signal::pre_trade::log_throttle]")
        && (line.contains("reason_class=max_position")
            || line.contains("reason=max position risk failed:"));
    if (line.contains("mkt_signal::strategy::open_strategy_common]")
        && line.contains("仓位限制检查失败:"))
        || summarized_rejection
    {
        return projected_is_near_limit;
    }
    if !line.contains("mkt_signal::pre_trade::monitor_channel]") {
        return false;
    }

    let current = usdt_value_after(line, "当前持仓=").map(f64::abs);
    let order = usdt_value_after(line, "下单数量=").map(f64::abs);
    matches!((current, order), (Some(current), Some(order))
        if current <= limit * 1.05
            && order <= limit * 0.05
            && projected_is_near_limit)
}

fn redact_and_truncate(value: &str) -> String {
    let mut output = redact_url_queries(value);
    for key in [
        "signature=",
        "api_key=",
        "apiKey=",
        "apikey=",
        "token=",
        "secret=",
    ] {
        output = redact_assignment(&output, key);
    }
    truncate_chars(output.trim(), 800)
}

fn redact_url_queries(value: &str) -> String {
    let mut output = value.to_string();
    let mut search_from = 0;
    while let Some(relative) = output[search_from..]
        .find("http://")
        .or_else(|| output[search_from..].find("https://"))
    {
        let url_start = search_from + relative;
        let Some(query_relative) = output[url_start..].find('?') else {
            search_from = url_start + 7;
            continue;
        };
        let query_start = url_start + query_relative;
        let query_end = output[query_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ')' | ']' | '}')
            })
            .map_or(output.len(), |relative_end| query_start + relative_end);
        output.replace_range(query_start..query_end, "?<redacted>");
        search_from = query_start + "?<redacted>".len();
    }
    output
}

fn redact_assignment(value: &str, key: &str) -> String {
    let mut output = value.to_string();
    let mut search_from = 0;
    while let Some(relative) = output[search_from..].find(key) {
        let value_start = search_from + relative + key.len();
        let value_end = output[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '&' | ',' | ')' | ']' | '}')
            })
            .map_or(output.len(), |relative_end| value_start + relative_end);
        output.replace_range(value_start..value_end, "<redacted>");
        search_from = value_start + "<redacted>".len();
    }
    output
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let truncated: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        truncated + "..."
    } else {
        truncated
    }
}

fn fingerprint(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_digits = false;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                output.push('#');
                in_digits = true;
            }
        } else {
            in_digits = false;
            output.push(character);
        }
    }
    output
}

#[derive(Clone)]
struct AppState {
    snapshot: Arc<RwLock<OverviewResponse>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    generated_at_ms: i64,
    environments: usize,
}

pub async fn run() -> Result<()> {
    let config_path = env::var("CRYPTO_OPS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH));
    let config = MonitorConfig::load(&config_path)?;
    let bind = env::var("CRYPTO_OPS_BIND")
        .ok()
        .or_else(|| config.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());
    let bind: SocketAddr = bind.parse().context("invalid CRYPTO_OPS_BIND")?;
    let scan_interval = Duration::from_secs(
        config
            .scan_interval_secs
            .unwrap_or(DEFAULT_SCAN_INTERVAL_SECS)
            .max(1),
    );

    let (monitor, initial) = tokio::task::spawn_blocking(move || {
        let mut monitor = Monitor::new(config);
        let initial = monitor.refresh();
        (monitor, initial)
    })
    .await
    .context("initialize ops monitor")?;
    let snapshot = Arc::new(RwLock::new(initial));
    let monitor = Arc::new(Mutex::new(monitor));
    spawn_refresh_loop(monitor, snapshot.clone(), scan_interval);

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/overview", get(overview))
        .route("/api/v1/environments/{slug}", get(environment_status))
        .with_state(AppState { snapshot })
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    info!(%bind, config = %config_path.display(), "ops monitor API started");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve ops monitor HTTP")?;
    Ok(())
}

fn spawn_refresh_loop(
    monitor: Arc<Mutex<Monitor>>,
    snapshot: Arc<RwLock<OverviewResponse>>,
    scan_interval: Duration,
) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(scan_interval);
        timer.tick().await;
        loop {
            timer.tick().await;
            let monitor = monitor.clone();
            let refreshed = tokio::task::spawn_blocking(move || {
                monitor
                    .lock()
                    .map(|mut monitor| monitor.refresh())
                    .map_err(|_| anyhow::anyhow!("ops monitor lock poisoned"))
            })
            .await;
            match refreshed {
                Ok(Ok(refreshed)) => *snapshot.write().await = refreshed,
                Ok(Err(reason)) => error!(error = ?reason, "ops monitor refresh failed"),
                Err(reason) => error!(error = ?reason, "ops monitor refresh task failed"),
            }
        }
    });
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let snapshot = state.snapshot.read().await;
    Json(HealthResponse {
        status: "ok",
        generated_at_ms: snapshot.generated_at_ms,
        environments: snapshot.environments.len(),
    })
}

async fn overview(State(state): State<AppState>) -> Json<OverviewResponse> {
    Json(state.snapshot.read().await.clone())
}

async fn environment_status(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<EnvironmentStatusResponse>, StatusCode> {
    state
        .snapshot
        .read()
        .await
        .environments
        .iter()
        .find(|environment| environment.strategy_slug == slug)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(profile: DeploymentProfile) -> EnvironmentConfig {
        EnvironmentConfig {
            slug: "binance_fr_arb03".to_string(),
            host: "local".to_string(),
            root: PathBuf::from("/home/ubuntu/binance_fr_arb03"),
            exchange: "binance".to_string(),
            tag: "arb03".to_string(),
            profile,
            snapshot_url: None,
        }
    }

    #[test]
    fn derives_funding_rate_component_names() {
        let environment = environment(DeploymentProfile::FundingRate);
        let signal = ComponentSpec::from_environment(&environment, ComponentRole::TradeSignal);
        let monitor = ComponentSpec::from_environment(&environment, ComponentRole::AccountMonitor);
        assert_eq!(signal.manager_name, "fr_ts_bn_arb03");
        assert_eq!(
            signal.log_path,
            PathBuf::from("/home/ubuntu/.pm2/logs/fr-ts-bn-arb03-error.log")
        );
        assert_eq!(monitor.manager_name, "fr_am_bn_arb03");
    }

    #[test]
    fn parses_and_redacts_recent_warning() {
        let now_ms = DateTime::parse_from_rfc3339("2026-08-03T02:16:00Z")
            .unwrap()
            .timestamp_millis();
        let event = parse_log_event(
            "[2026-08-03T02:15:35Z WARN  trade_engine::dispatcher] request failed url=https://example.test/order?timestamp=1&signature=secret signature=secret",
            now_ms,
            15 * 60 * 1_000,
        )
        .unwrap();
        assert_eq!(event.severity, AlertSeverity::Warning);
        assert!(!event.message.contains("secret"));
        assert!(event.message.contains("?<redacted>"));
    }

    #[test]
    fn filters_only_unlocked_account_open_block() {
        let now_ms = DateTime::parse_from_rfc3339("2026-08-05T09:03:00Z")
            .unwrap()
            .timestamp_millis();
        let window_ms = 15 * 60 * 1_000;
        let unlocked = "[2026-08-05T09:02:59Z WARN  mkt_signal::pre_trade::account_open_block] AccountOpenBlock: bitget_unified capacity initial_margin_headroom=48590.93464363 max_borrowable=0.00000000 capacity=48590.93464363 threshold=2000.00000000 query_latency_us=0 state=unlocked";
        let locked = "[2026-08-05T09:02:59Z WARN  mkt_signal::pre_trade::account_open_block] AccountOpenBlock: bitget_unified capacity initial_margin_headroom=0.00000000 max_borrowable=0.00000000 capacity=0.00000000 threshold=2000.00000000 query_latency_us=0 state=locked";

        assert!(parse_log_event(unlocked, now_ms, window_ms).is_none());
        assert!(parse_log_event(locked, now_ms, window_ms).is_some());
    }

    #[test]
    fn filters_only_small_near_limit_position_rejections() {
        let now_ms = DateTime::parse_from_rfc3339("2026-08-03T06:25:00Z")
            .unwrap()
            .timestamp_millis();
        let window_ms = 15 * 60 * 1_000;

        let normal_detail = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::monitor_channel] symbol=ADAUSDT 当前持仓=16526.300000(3038.9238USDT) 下单数量=272.000000(50.0165USDT) 下单后持仓=3088.9403USDT 超过阈值 3000.0000USDT";
        let normal_summary = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=45866868 仓位限制检查失败: symbol=ADAUSDT 下单后持仓 3088.9403USDT 超过阈值 3000.0000USDT，标记策略为不活跃";
        assert!(parse_log_event(normal_detail, now_ms, window_ms).is_none());
        assert!(parse_log_event(normal_summary, now_ms, window_ms).is_none());

        let large_existing_position = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::monitor_channel] symbol=ADAUSDT 当前持仓=19500.000000(3600.0000USDT) 下单数量=272.000000(50.0000USDT) 下单后持仓=3650.0000USDT 超过阈值 3000.0000USDT";
        let large_position_summary = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=45866869 仓位限制检查失败: symbol=ADAUSDT 下单后持仓 3650.0000USDT 超过阈值 3000.0000USDT，标记策略为不活跃";
        assert!(parse_log_event(large_existing_position, now_ms, window_ms).is_some());
        assert!(parse_log_event(large_position_summary, now_ms, window_ms).is_some());

        let large_order = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::monitor_channel] symbol=ADAUSDT 当前持仓=10000.000000(1800.0000USDT) 下单数量=7000.000000(1260.0000USDT) 下单后持仓=3060.0000USDT 超过阈值 3000.0000USDT";
        assert!(parse_log_event(large_order, now_ms, window_ms).is_some());

        let normal_throttled = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::log_throttle] ArbOpen: symbol=- 未激活 summary: suppressed=4 last_strategy_id=Some(45872518) reason_class=max_position reason=max position risk failed: symbol=ADAUSDT 下单后持仓 3092.7936USDT 超过阈值 3000.0000USDT";
        assert!(parse_log_event(normal_throttled, now_ms, window_ms).is_none());

        let large_position_throttled = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::log_throttle] ArbOpen: symbol=- 未激活 summary: suppressed=4 last_strategy_id=Some(45872519) reason_class=max_position reason=max position risk failed: symbol=ADAUSDT 下单后持仓 3650.0000USDT 超过阈值 3000.0000USDT";
        assert!(parse_log_event(large_position_throttled, now_ms, window_ms).is_some());
    }

    #[test]
    fn filters_only_small_binance_caps_and_leverage_overages() {
        let now_ms = DateTime::parse_from_rfc3339("2026-08-03T06:25:00Z")
            .unwrap()
            .timestamp_millis();
        let window_ms = 15 * 60 * 1_000;

        let normal_binance_cap = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=1102201690 Binance限仓检查失败: Binance FR position-limit cap exceeded symbol=GENIUSUSDT open_next=50796.1270USDT futures_next=50796.6048USDT cap=49000.0000USDT max_notional_value=50000.0000 buffer=1000.0000，标记策略为不活跃";
        let large_binance_cap = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=1102201691 Binance限仓检查失败: Binance FR position-limit cap exceeded symbol=GENIUSUSDT open_next=55000.0000USDT futures_next=55000.0000USDT cap=49000.0000USDT max_notional_value=50000.0000 buffer=1000.0000，标记策略为不活跃";
        let imbalanced_binance_cap = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=1102201692 Binance限仓检查失败: Binance FR position-limit cap exceeded symbol=GENIUSUSDT open_next=50796.1270USDT futures_next=40000.0000USDT cap=49000.0000USDT max_notional_value=50000.0000 buffer=1000.0000，标记策略为不活跃";
        assert!(parse_log_event(normal_binance_cap, now_ms, window_ms).is_none());
        assert!(parse_log_event(large_binance_cap, now_ms, window_ms).is_some());
        assert!(parse_log_event(imbalanced_binance_cap, now_ms, window_ms).is_some());

        let normal_binance_cap_summary = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::log_throttle] ArbOpen: symbol=- 未激活 summary: suppressed=120 last_strategy_id=Some(1102218421) reason_class=other reason=binance position-limit failed: Binance FR position-limit cap exceeded symbol=HUMAUSDT open_next=49869.6765USDT futures_next=49862.4684USDT cap=49000.0000USDT max_notional_value=50000.0000 buffer=1000.0000";
        assert!(parse_log_event(normal_binance_cap_summary, now_ms, window_ms).is_none());

        let normal_leverage = "[2026-08-03T06:24:59Z ERROR mkt_signal::pre_trade::log_throttle] ArbOpenStrategy: symbol=EDUUSDT 杠杆风控检查失败 summary: suppressed=40 last_strategy_id=Some(1098806172) reason=杠杆率 1.85 超过限制 1.80";
        let normal_leverage_summary = "[2026-08-03T06:24:59Z WARN  mkt_signal::pre_trade::log_throttle] ArbOpen: symbol=- 未激活 summary: suppressed=40 last_strategy_id=Some(1098806172) reason_class=leverage reason=leverage risk failed: 杠杆率 1.85 超过限制 1.80";
        let large_leverage = "[2026-08-03T06:24:59Z ERROR mkt_signal::pre_trade::log_throttle] ArbOpenStrategy: symbol=EDUUSDT 杠杆风控检查失败 summary: suppressed=40 last_strategy_id=Some(1098806173) reason=杠杆率 2.00 超过限制 1.80";
        assert!(parse_log_event(normal_leverage, now_ms, window_ms).is_none());
        assert!(parse_log_event(normal_leverage_summary, now_ms, window_ms).is_none());
        assert!(parse_log_event(large_leverage, now_ms, window_ms).is_some());
    }

    #[test]
    fn max_position_whitelist_is_threshold_and_risk_specific() {
        let whitelist = vec![100.0];
        let kite_at_100 = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=1 仓位限制检查失败: symbol=KITEUSDT 下单后持仓 44871.4540USDT 超过阈值 100.0000USDT";
        let other_at_100 = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=2 仓位限制检查失败: symbol=OTHERUSDT 下单后持仓 44871.4540USDT 超过阈值 100.0000USDT";
        let kite_at_3000 = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=3 仓位限制检查失败: symbol=KITEUSDT 下单后持仓 44871.4540USDT 超过阈值 3000.0000USDT";
        let kite_exposure = "[2026-08-03T06:24:59Z ERROR mkt_signal::strategy::open_strategy_common] ArbOpenStrategy: strategy_id=4 symbol=KITEUSDT 单品种敞口风控检查失败";

        assert!(is_whitelisted_max_position_rejection(
            kite_at_100,
            &whitelist
        ));
        assert!(is_whitelisted_max_position_rejection(
            other_at_100,
            &whitelist
        ));
        assert!(!is_whitelisted_max_position_rejection(
            kite_at_3000,
            &whitelist
        ));
        assert!(!is_whitelisted_max_position_rejection(
            kite_exposure,
            &whitelist
        ));
    }

    #[test]
    fn filters_cancel_unknown_order_as_expected_race() {
        let now_ms = DateTime::parse_from_rfc3339("2026-08-03T06:25:00Z")
            .unwrap()
            .timestamp_millis();
        let window_ms = 15 * 60 * 1_000;
        let cancel_race = "[2026-08-03T06:24:59Z WARN  trade_engine::dispatcher] rest dispatch error: req_type=BinanceCancelMarginOrder symbol=BREVUSDT status=400 code=Some(-2011) msg=Some(\"Unknown order sent.\")";
        let cancel_response = "[2026-08-03T06:24:59Z WARN  trade_engine::ws_client] recv cancel_order response req_type=BinanceWsCancelMarginOrder status=400 code=-2011";
        let non_cancel = "[2026-08-03T06:24:59Z WARN  trade_engine::dispatcher] rest dispatch error: req_type=BinanceNewMarginOrder symbol=BREVUSDT status=400 code=Some(-2011)";
        let classifier = "[2026-08-03T06:24:59Z WARN  trade_engine::dispatcher] http classify: status=400, 4XX client request error";

        assert!(parse_log_event(cancel_race, now_ms, window_ms).is_none());
        assert!(parse_log_event(cancel_response, now_ms, window_ms).is_none());
        assert!(parse_log_event(non_cancel, now_ms, window_ms).is_some());
        assert!(parse_log_event(classifier, now_ms, window_ms).is_none());
    }

    #[test]
    fn critical_component_offline_makes_environment_critical() {
        let component = ComponentStatusResponse {
            role: ComponentRole::TradeEngine,
            critical: true,
            status: ComponentHealth::Offline,
            pid: None,
            instances: 0,
            linux_state: None,
            manager: "pmdaemon",
            manager_name: "test".to_string(),
            alerts: AlertSummaryResponse::default(),
        };
        assert_eq!(
            environment_health(&[component]),
            EnvironmentHealth::Critical
        );
    }

    #[test]
    fn correlates_market_forbidden_with_margin_order() {
        let now_ms = DateTime::parse_from_rfc3339("2026-08-03T03:26:54Z")
            .unwrap()
            .timestamp_millis();
        let mut logs = LogMonitor::default();
        logs.observe_trading_line(
            "gate_fr_arb02",
            "[2026-08-03T03:26:53Z INFO  mkt_signal::strategy::open_strategy_common] ArbOpen订单已创建: strategy_id=534737808 client_order_id=2296681397294727169 symbol=XNYUSDT GateMargin side=Buy qty=15100 price=0.006589",
            now_ms,
            15 * 60 * 1_000,
        );
        logs.observe_trading_line(
            "gate_fr_arb02",
            "[2026-08-03T03:26:53Z WARN  trade_engine::trade_response_handle] trade resp error: ex=Gate type=GateUnifiedNewOrder cli_ord_id=2296681397294727169 status=403 code=-100510 msg=RISK_CHECK_MARKET_FORBIDDEN",
            now_ms,
            15 * 60 * 1_000,
        );

        let blocks = logs.trading_blocks("gate_fr_arb02");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].symbol, "XNYUSDT");
        assert_eq!(blocks[0].asset, "XNY");
        assert_eq!(blocks[0].blocked_leg, TradingLeg::Margin);
        assert_eq!(blocks[0].http_status, Some(403));
        assert_eq!(blocks[0].error_code, Some(-100510));
        assert_eq!(blocks[0].count, 1);
    }

    #[test]
    fn parses_margin_and_futures_positions_from_snapshot() {
        let payload = serde_json::json!({
            "ts_ms": 1785728057820_i64,
            "entries": [{
                "channel": "pre_trade_exposure",
                "entry": {
                    "ts_ms": 1785728057741_i64,
                    "rows": [
                        {
                            "asset": "GWEI",
                            "open_qty": 1754138.0825,
                            "hedge_qty": -1742300.0,
                            "net_qty": 11838.0825,
                            "open_usdt": 29316.91,
                            "hedge_usdt": -29119.06,
                            "net_usdt": 197.85,
                            "is_total": false
                        },
                        {"asset": "TOTAL", "is_total": true}
                    ]
                }
            }]
        });

        let snapshot = parse_exposure_snapshot(&payload).unwrap();
        let position = snapshot.positions.get("GWEI").unwrap();
        assert_eq!(snapshot.at_ms, 1785728057741);
        assert!((position.margin_qty - 1754138.0825).abs() < 1e-8);
        assert!((position.futures_qty + 1742300.0).abs() < 1e-8);
        assert!(snapshot.positions.get("XNY").is_none());
        let zero = snapshot.positions.get("XNY").cloned().unwrap_or_default();
        assert_eq!(zero.net_qty, 0.0);
    }
}
