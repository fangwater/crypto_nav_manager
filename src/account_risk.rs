use anyhow::{Result, bail};
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use iceoryx2::service::ipc;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing::{info, warn};

const ACCOUNT_PAYLOAD_BYTES: usize = 16_384;
const ACCOUNT_RISK_EVENT_TYPE: u32 = 4_007;
const ACCOUNT_EVENT_HEADER_BYTES: usize = 12;
const ACCOUNT_RISK_BODY_BYTES: usize = 4 + 8 + 8 * 7;
const IPC_POLL_INTERVAL: Duration = Duration::from_millis(2);
const IPC_RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
const IPC_WARNING_INTERVAL: Duration = Duration::from_secs(30);
const STALE_AFTER_MS: i64 = 90_000;

type AccountSubscriber = Subscriber<ipc::Service, [u8; ACCOUNT_PAYLOAD_BYTES], ()>;

#[derive(Clone, Debug)]
pub struct AccountRiskFeed {
    strategy_slug: String,
    exchange: String,
    service_name: String,
    sort_order: i32,
}

impl AccountRiskFeed {
    pub fn new(strategy_slug: String, exchange: String, sort_order: i32) -> Option<Self> {
        let service_exchange = match exchange.as_str() {
            "binance" | "gate" | "bitget" | "bybit" => exchange.as_str(),
            "okx" => "okex",
            _ => return None,
        };
        let service_name = format!("{strategy_slug}/account_pubs/{service_exchange}_pm");
        Some(Self {
            strategy_slug,
            exchange,
            service_name,
            sort_order,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct AccountRiskReading {
    source_ts_ms: i64,
    scope: AccountScope,
    adjusted_equity_usd: f64,
    actual_equity_usd: f64,
    maintenance_margin_usd: f64,
    initial_margin_usd: f64,
    margin_ratio: f64,
    borrowed_usd: f64,
    notional_usd: f64,
    received_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccountScope {
    BinanceUnified,
    BinanceStandardSpot,
    BinanceStandardUm,
    OkxUnified,
    GateUnified,
    BitgetUnified,
    BybitUnified,
}

impl AccountScope {
    fn decode(value: u32) -> Result<Self> {
        Ok(match value {
            1 => Self::BinanceUnified,
            2 => Self::BinanceStandardSpot,
            3 => Self::BinanceStandardUm,
            10 => Self::OkxUnified,
            11 => Self::GateUnified,
            12 => Self::BitgetUnified,
            13 => Self::BybitUnified,
            _ => bail!("unsupported account risk scope: {value}"),
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::BinanceUnified => "binance_unified",
            Self::BinanceStandardSpot => "binance_std_spot",
            Self::BinanceStandardUm => "binance_std_um",
            Self::OkxUnified => "okx_unified",
            Self::GateUnified => "gate_unified",
            Self::BitgetUnified => "bitget_unified",
            Self::BybitUnified => "bybit_unified",
        }
    }
}

#[derive(Default)]
struct CacheState {
    connected: HashMap<String, bool>,
    readings: HashMap<String, AccountRiskReading>,
}

#[derive(Clone, Default)]
pub struct AccountRiskCache {
    feeds: Arc<Vec<AccountRiskFeed>>,
    state: Arc<RwLock<CacheState>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRiskSnapshot {
    pub strategy_slug: String,
    pub exchange: String,
    pub connected: bool,
    pub status: &'static str,
    pub risk_level: Option<&'static str>,
    pub scope: Option<&'static str>,
    pub source_ts_ms: Option<i64>,
    pub received_at_ms: Option<i64>,
    pub uni_mmr: Option<f64>,
    pub adjusted_equity_usd: Option<f64>,
    pub actual_equity_usd: Option<f64>,
    pub maintenance_margin_usd: Option<f64>,
    pub initial_margin_usd: Option<f64>,
    pub borrowed_usd: Option<f64>,
    pub notional_usd: Option<f64>,
}

impl AccountRiskCache {
    pub fn start(feeds: Vec<AccountRiskFeed>) -> Self {
        let state = CacheState {
            connected: feeds
                .iter()
                .map(|feed| (feed.strategy_slug.clone(), false))
                .collect(),
            readings: HashMap::new(),
        };
        let cache = Self {
            feeds: Arc::new(feeds),
            state: Arc::new(RwLock::new(state)),
        };
        if !cache.feeds.is_empty() {
            cache.spawn_ipc_listener();
        }
        cache
    }

    pub fn snapshots(&self) -> Vec<AccountRiskSnapshot> {
        let now_ms = unix_time_ms();
        let Ok(state) = self.state.read() else {
            return Vec::new();
        };
        let mut feeds = self.feeds.iter().collect::<Vec<_>>();
        feeds.sort_by(|left, right| {
            left.sort_order
                .cmp(&right.sort_order)
                .then_with(|| left.strategy_slug.cmp(&right.strategy_slug))
        });
        feeds
            .into_iter()
            .map(|feed| {
                let connected = state
                    .connected
                    .get(&feed.strategy_slug)
                    .copied()
                    .unwrap_or(false);
                snapshot_for_feed(
                    feed,
                    connected,
                    state.readings.get(&feed.strategy_slug),
                    now_ms,
                )
            })
            .collect()
    }

    fn spawn_ipc_listener(&self) {
        let cache = self.clone();
        thread::Builder::new()
            .name("account-risk-ipc".to_string())
            .spawn(move || run_ipc_listener(cache))
            .expect("spawn account risk IPC listener");
    }

    fn set_connected(&self, strategy_slug: &str, connected: bool) {
        if let Ok(mut state) = self.state.write() {
            state.connected.insert(strategy_slug.to_string(), connected);
        }
    }

    fn update(&self, strategy_slug: &str, mut reading: AccountRiskReading) {
        reading.received_at_ms = unix_time_ms();
        let Ok(mut state) = self.state.write() else {
            return;
        };
        if state
            .readings
            .get(strategy_slug)
            .is_some_and(|existing| existing.source_ts_ms > reading.source_ts_ms)
        {
            return;
        }
        state.readings.insert(strategy_slug.to_string(), reading);
    }
}

fn snapshot_for_feed(
    feed: &AccountRiskFeed,
    connected: bool,
    reading: Option<&AccountRiskReading>,
    now_ms: i64,
) -> AccountRiskSnapshot {
    let stale = reading
        .is_some_and(|reading| now_ms.saturating_sub(reading.received_at_ms) > STALE_AFTER_MS);
    let status = if !connected {
        "unavailable"
    } else if reading.is_none() {
        "waiting"
    } else if stale {
        "stale"
    } else {
        "live"
    };
    let uni_mmr = reading.and_then(|reading| {
        (reading.maintenance_margin_usd > f64::EPSILON).then_some(reading.margin_ratio)
    });
    let risk_level = reading.map(|reading| {
        if reading.maintenance_margin_usd <= f64::EPSILON {
            "free_trade"
        } else if reading.margin_ratio > 1.5 {
            "free_trade"
        } else if reading.margin_ratio > 1.2 {
            "warning"
        } else if reading.margin_ratio > 1.05 {
            "reduce_only"
        } else {
            "liquidation"
        }
    });

    AccountRiskSnapshot {
        strategy_slug: feed.strategy_slug.clone(),
        exchange: feed.exchange.clone(),
        connected,
        status,
        risk_level,
        scope: reading.map(|reading| reading.scope.as_str()),
        source_ts_ms: reading.map(|reading| reading.source_ts_ms),
        received_at_ms: reading.map(|reading| reading.received_at_ms),
        uni_mmr,
        adjusted_equity_usd: reading.map(|reading| reading.adjusted_equity_usd),
        actual_equity_usd: reading.map(|reading| reading.actual_equity_usd),
        maintenance_margin_usd: reading.map(|reading| reading.maintenance_margin_usd),
        initial_margin_usd: reading.map(|reading| reading.initial_margin_usd),
        borrowed_usd: reading.map(|reading| reading.borrowed_usd),
        notional_usd: reading.map(|reading| reading.notional_usd),
    }
}

struct IpcFeed {
    config: AccountRiskFeed,
    subscriber: Option<AccountSubscriber>,
    next_open_attempt_at: Instant,
    last_warning_at: Option<Instant>,
}

impl IpcFeed {
    fn new(config: AccountRiskFeed) -> Self {
        Self {
            config,
            subscriber: None,
            next_open_attempt_at: Instant::now(),
            last_warning_at: None,
        }
    }

    fn ensure_subscriber(&mut self, node: &Node<ipc::Service>, cache: &AccountRiskCache) {
        if self.subscriber.is_some() || Instant::now() < self.next_open_attempt_at {
            return;
        }
        let result = ServiceName::new(&self.config.service_name)
            .map_err(anyhow::Error::from)
            .and_then(|service_name| {
                node.service_builder(&service_name)
                    .publish_subscribe::<[u8; ACCOUNT_PAYLOAD_BYTES]>()
                    .open()
                    .map_err(anyhow::Error::from)
            })
            .and_then(|service| {
                service
                    .subscriber_builder()
                    .create()
                    .map_err(anyhow::Error::from)
            });
        match result {
            Ok(subscriber) => {
                self.subscriber = Some(subscriber);
                self.last_warning_at = None;
                cache.set_connected(&self.config.strategy_slug, true);
                info!(
                    strategy = %self.config.strategy_slug,
                    service = %self.config.service_name,
                    "account risk IPC subscribed"
                );
            }
            Err(error) => {
                cache.set_connected(&self.config.strategy_slug, false);
                let now = Instant::now();
                if self
                    .last_warning_at
                    .is_none_or(|last| now.duration_since(last) >= IPC_WARNING_INTERVAL)
                {
                    warn!(
                        strategy = %self.config.strategy_slug,
                        service = %self.config.service_name,
                        ?error,
                        "account risk IPC service unavailable"
                    );
                    self.last_warning_at = Some(now);
                }
                self.next_open_attempt_at = now + IPC_RECONNECT_INTERVAL;
            }
        }
    }

    fn drain(&mut self, cache: &AccountRiskCache) {
        let Some(subscriber) = &self.subscriber else {
            return;
        };
        loop {
            match subscriber.receive() {
                Ok(Some(sample)) => match decode_account_risk(sample.payload()) {
                    Ok(Some(reading)) => cache.update(&self.config.strategy_slug, reading),
                    Ok(None) => {}
                    Err(error) => warn!(
                        strategy = %self.config.strategy_slug,
                        service = %self.config.service_name,
                        ?error,
                        "account risk IPC decode failed"
                    ),
                },
                Ok(None) => break,
                Err(error) => {
                    warn!(
                        strategy = %self.config.strategy_slug,
                        service = %self.config.service_name,
                        ?error,
                        "account risk IPC receive failed; reconnecting"
                    );
                    self.subscriber = None;
                    self.next_open_attempt_at = Instant::now() + IPC_RECONNECT_INTERVAL;
                    cache.set_connected(&self.config.strategy_slug, false);
                    break;
                }
            }
        }
    }
}

fn run_ipc_listener(cache: AccountRiskCache) {
    let node = match NodeBuilder::new()
        .name(&NodeName::new("crypto_nav_account_risk").expect("valid node name"))
        .create::<ipc::Service>()
    {
        Ok(node) => node,
        Err(error) => {
            warn!(?error, "account risk IPC node creation failed");
            return;
        }
    };
    let mut feeds = cache
        .feeds
        .iter()
        .cloned()
        .map(IpcFeed::new)
        .collect::<Vec<_>>();
    loop {
        for feed in &mut feeds {
            feed.ensure_subscriber(&node, &cache);
            feed.drain(&cache);
        }
        thread::sleep(IPC_POLL_INTERVAL);
    }
}

fn decode_account_risk(data: &[u8]) -> Result<Option<AccountRiskReading>> {
    if data.len() < ACCOUNT_EVENT_HEADER_BYTES {
        bail!("account event too short: {}", data.len());
    }
    let event_type = read_u32(data, 0)?;
    if event_type != ACCOUNT_RISK_EVENT_TYPE {
        return Ok(None);
    }
    let scope = AccountScope::decode(read_u32(data, 4)?)?;
    let body_len = read_u32(data, 8)? as usize;
    if body_len != ACCOUNT_RISK_BODY_BYTES {
        bail!(
            "unsupported account risk body length: {body_len}, expected {ACCOUNT_RISK_BODY_BYTES}"
        );
    }
    let total_len = ACCOUNT_EVENT_HEADER_BYTES + body_len;
    if data.len() < total_len {
        bail!("truncated account risk event: {} < {total_len}", data.len());
    }
    let body = &data[ACCOUNT_EVENT_HEADER_BYTES..total_len];
    if read_u32(body, 0)? != ACCOUNT_RISK_EVENT_TYPE {
        bail!("account risk inner event type does not match wrapper");
    }
    let reading = AccountRiskReading {
        source_ts_ms: read_i64(body, 4)?,
        scope,
        adjusted_equity_usd: read_f64(body, 12)?,
        actual_equity_usd: read_f64(body, 20)?,
        maintenance_margin_usd: read_f64(body, 28)?,
        initial_margin_usd: read_f64(body, 36)?,
        margin_ratio: read_f64(body, 44)?,
        borrowed_usd: read_f64(body, 52)?,
        notional_usd: read_f64(body, 60)?,
        received_at_ms: 0,
    };
    if reading.source_ts_ms <= 0 {
        bail!("account risk timestamp must be positive");
    }
    for (name, value) in [
        ("adjusted_equity_usd", reading.adjusted_equity_usd),
        ("actual_equity_usd", reading.actual_equity_usd),
        ("maintenance_margin_usd", reading.maintenance_margin_usd),
        ("initial_margin_usd", reading.initial_margin_usd),
        ("borrowed_usd", reading.borrowed_usd),
        ("notional_usd", reading.notional_usd),
    ] {
        if !value.is_finite() {
            bail!("account risk {name} is not finite");
        }
    }
    let zero_margin_infinity =
        reading.maintenance_margin_usd <= f64::EPSILON && reading.margin_ratio == f64::INFINITY;
    if !reading.margin_ratio.is_finite() && !zero_margin_infinity {
        bail!("account risk margin_ratio is not finite");
    }
    Ok(Some(reading))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(read_array(data, offset)?))
}

fn read_i64(data: &[u8], offset: usize) -> Result<i64> {
    Ok(i64::from_le_bytes(read_array(data, offset)?))
}

fn read_f64(data: &[u8], offset: usize) -> Result<f64> {
    Ok(f64::from_le_bytes(read_array(data, offset)?))
}

fn read_array<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    data.get(offset..offset + N)
        .ok_or_else(|| anyhow::anyhow!("message truncated at byte {offset}"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid field width at byte {offset}"))
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risk_event(scope: u32, maintenance_margin: f64, margin_ratio: f64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&ACCOUNT_RISK_EVENT_TYPE.to_le_bytes());
        body.extend_from_slice(&1_700_000_000_000_i64.to_le_bytes());
        for value in [
            100_000.0,
            101_000.0,
            maintenance_margin,
            12_000.0,
            margin_ratio,
            2_500.0,
            300_000.0,
        ] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        let mut event = Vec::new();
        event.extend_from_slice(&ACCOUNT_RISK_EVENT_TYPE.to_le_bytes());
        event.extend_from_slice(&scope.to_le_bytes());
        event.extend_from_slice(&(body.len() as u32).to_le_bytes());
        event.extend_from_slice(&body);
        event.resize(ACCOUNT_PAYLOAD_BYTES, 0);
        event
    }

    #[test]
    fn decodes_account_monitor_risk_wire_format() {
        let reading = decode_account_risk(&risk_event(11, 5_000.0, 20.0))
            .unwrap()
            .unwrap();
        assert_eq!(reading.scope, AccountScope::GateUnified);
        assert_eq!(reading.source_ts_ms, 1_700_000_000_000);
        assert_eq!(reading.adjusted_equity_usd, 100_000.0);
        assert_eq!(reading.maintenance_margin_usd, 5_000.0);
        assert_eq!(reading.margin_ratio, 20.0);
        assert_eq!(reading.notional_usd, 300_000.0);
    }

    #[test]
    fn ignores_other_account_event_types() {
        let mut event = vec![0; ACCOUNT_PAYLOAD_BYTES];
        event[..4].copy_from_slice(&4_002_u32.to_le_bytes());
        assert!(decode_account_risk(&event).unwrap().is_none());
    }

    #[test]
    fn rejects_unknown_scope_and_non_finite_values() {
        assert!(decode_account_risk(&risk_event(99, 5_000.0, 20.0)).is_err());
        assert!(decode_account_risk(&risk_event(1, 5_000.0, f64::NAN)).is_err());
    }

    #[test]
    fn zero_maintenance_margin_is_free_without_a_numeric_unimmr() {
        let feed = AccountRiskFeed::new("test".into(), "binance".into(), 1).unwrap();
        let mut reading = decode_account_risk(&risk_event(1, 0.0, f64::INFINITY))
            .unwrap()
            .unwrap();
        reading.received_at_ms = unix_time_ms();
        let snapshot = snapshot_for_feed(&feed, true, Some(&reading), unix_time_ms());
        assert_eq!(snapshot.status, "live");
        assert_eq!(snapshot.risk_level, Some("free_trade"));
        assert_eq!(snapshot.uni_mmr, None);
    }

    #[test]
    fn maps_okx_strategy_to_okex_service_name() {
        let feed = AccountRiskFeed::new("okex_fr_arb01".into(), "okx".into(), 1).unwrap();
        assert_eq!(feed.service_name, "okex_fr_arb01/account_pubs/okex_pm");
    }
}
