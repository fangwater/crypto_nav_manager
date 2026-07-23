use anyhow::{Context, Result, bail};
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use iceoryx2::service::ipc;
use serde::Deserialize;
use std::{
    collections::HashMap,
    process,
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant},
};
use tracing::{info, warn};

const DERIVATIVES_PAYLOAD_BYTES: usize = 128;
const DERIVATIVES_HISTORY_SIZE: usize = 50;
const DERIVATIVES_MAX_SUBSCRIBERS: usize = 64;
const DERIVATIVES_SUBSCRIBER_BUFFER: usize = 8192;
const MARK_PRICE_MESSAGE_TYPE: u32 = 1011;
const IPC_POLL_INTERVAL: Duration = Duration::from_millis(2);
const IPC_RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
const IPC_WARNING_INTERVAL: Duration = Duration::from_secs(30);
const REST_TIMEOUT: Duration = Duration::from_secs(8);
const BINANCE_MARK_PRICES_URL: &str = "https://fapi.binance.com/fapi/v1/premiumIndex";
const GATE_CONTRACTS_URL: &str = "https://api.gateio.ws/api/v4/futures/usdt/contracts";
const OKX_MARK_PRICES_URL: &str = "https://www.okx.com/api/v5/public/mark-price";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkPriceExchange {
    Binance,
    Gate,
    Okx,
}

impl MarkPriceExchange {
    fn as_str(self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Gate => "gate",
            Self::Okx => "okx",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MarkPrice {
    price: f64,
    exchange_ts_ms: i64,
}

type ExchangePrices = HashMap<String, MarkPrice>;

#[derive(Clone, Default)]
pub struct MarkPriceCache {
    prices: Arc<RwLock<HashMap<MarkPriceExchange, ExchangePrices>>>,
}

impl MarkPriceCache {
    pub async fn start() -> Self {
        let cache = Self::default();
        cache.bootstrap_rest().await;
        cache.spawn_ipc_listener();
        cache
    }

    pub fn price(&self, exchange: MarkPriceExchange, symbol: &str) -> Option<f64> {
        let symbol = normalize_symbol(symbol)?;
        self.prices
            .read()
            .ok()?
            .get(&exchange)?
            .get(&symbol)
            .map(|entry| entry.price)
    }

    pub fn len(&self, exchange: MarkPriceExchange) -> usize {
        self.prices
            .read()
            .ok()
            .and_then(|prices| prices.get(&exchange).map(HashMap::len))
            .unwrap_or(0)
    }

    pub(crate) fn update(
        &self,
        exchange: MarkPriceExchange,
        symbol: &str,
        price: f64,
        exchange_ts_ms: i64,
    ) -> bool {
        let Some(symbol) = normalize_symbol(symbol) else {
            return false;
        };
        if !price.is_finite() || price <= 0.0 || exchange_ts_ms < 0 {
            return false;
        }
        let Ok(mut prices) = self.prices.write() else {
            return false;
        };
        let exchange_prices = prices.entry(exchange).or_default();
        if exchange_prices
            .get(&symbol)
            .is_some_and(|existing| existing.exchange_ts_ms > exchange_ts_ms)
        {
            return false;
        }
        exchange_prices.insert(
            symbol,
            MarkPrice {
                price,
                exchange_ts_ms,
            },
        );
        true
    }

    async fn bootstrap_rest(&self) {
        let client = match reqwest::Client::builder().timeout(REST_TIMEOUT).build() {
            Ok(client) => client,
            Err(error) => {
                warn!(?error, "mark price REST client initialization failed");
                return;
            }
        };
        let (binance, gate, okx) = tokio::join!(
            fetch_binance_mark_prices(&client),
            fetch_gate_mark_prices(&client),
            fetch_okx_mark_prices(&client)
        );
        self.store_bootstrap_result(MarkPriceExchange::Binance, binance);
        self.store_bootstrap_result(MarkPriceExchange::Gate, gate);
        self.store_bootstrap_result(MarkPriceExchange::Okx, okx);
    }

    fn store_bootstrap_result(
        &self,
        exchange: MarkPriceExchange,
        result: Result<Vec<(String, f64)>>,
    ) {
        match result {
            Ok(rows) => {
                let mut stored = 0usize;
                for (symbol, price) in rows {
                    stored += usize::from(self.update(exchange, &symbol, price, 0));
                }
                info!(
                    exchange = exchange.as_str(),
                    stored, "mark price REST bootstrap complete"
                );
            }
            Err(error) => warn!(
                exchange = exchange.as_str(),
                ?error,
                "mark price REST bootstrap failed; waiting for IPC"
            ),
        }
    }

    fn spawn_ipc_listener(&self) {
        let cache = self.clone();
        thread::Builder::new()
            .name("mark-price-ipc".to_string())
            .spawn(move || run_ipc_listener(cache))
            .expect("spawn mark price IPC listener");
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BinanceMarkPrice {
    symbol: String,
    mark_price: String,
}

async fn fetch_binance_mark_prices(client: &reqwest::Client) -> Result<Vec<(String, f64)>> {
    let rows = client
        .get(BINANCE_MARK_PRICES_URL)
        .send()
        .await
        .context("request Binance premium index")?
        .error_for_status()
        .context("Binance premium index status")?
        .json::<Vec<BinanceMarkPrice>>()
        .await
        .context("decode Binance premium index")?;
    parse_rest_rows(rows.into_iter().map(|row| (row.symbol, row.mark_price)))
}

#[derive(Debug, Deserialize)]
struct GateContract {
    name: String,
    mark_price: String,
}

async fn fetch_gate_mark_prices(client: &reqwest::Client) -> Result<Vec<(String, f64)>> {
    let rows = client
        .get(GATE_CONTRACTS_URL)
        .send()
        .await
        .context("request Gate futures contracts")?
        .error_for_status()
        .context("Gate futures contracts status")?
        .json::<Vec<GateContract>>()
        .await
        .context("decode Gate futures contracts")?;
    parse_rest_rows(rows.into_iter().map(|row| (row.name, row.mark_price)))
}

#[derive(Debug, Deserialize)]
struct OkxMarkPriceResponse {
    code: String,
    msg: String,
    data: Vec<OkxMarkPrice>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxMarkPrice {
    inst_id: String,
    mark_px: String,
}

async fn fetch_okx_mark_prices(client: &reqwest::Client) -> Result<Vec<(String, f64)>> {
    let response = client
        .get(OKX_MARK_PRICES_URL)
        .query(&[("instType", "SWAP")])
        .send()
        .await
        .context("request OKX mark prices")?
        .error_for_status()
        .context("OKX mark prices status")?
        .json::<OkxMarkPriceResponse>()
        .await
        .context("decode OKX mark prices")?;
    if response.code != "0" {
        bail!("OKX mark prices error {}: {}", response.code, response.msg);
    }
    parse_rest_rows(response.data.into_iter().filter_map(|row| {
        row.inst_id
            .strip_suffix("-SWAP")
            .map(|symbol| (symbol.to_string(), row.mark_px))
    }))
}

fn parse_rest_rows(rows: impl IntoIterator<Item = (String, String)>) -> Result<Vec<(String, f64)>> {
    let mut parsed = Vec::new();
    for (symbol, price) in rows {
        let Some(symbol) = normalize_symbol(&symbol) else {
            continue;
        };
        let price = price
            .parse::<f64>()
            .with_context(|| format!("invalid mark price for {symbol}"))?;
        if price.is_finite() && price > 0.0 {
            parsed.push((symbol, price));
        }
    }
    if parsed.is_empty() {
        bail!("mark price response contains no usable rows");
    }
    Ok(parsed)
}

type DerivativesSubscriber = Subscriber<ipc::Service, [u8; DERIVATIVES_PAYLOAD_BYTES], ()>;

struct IpcFeed {
    exchange: MarkPriceExchange,
    service_name: &'static str,
    subscriber: Option<DerivativesSubscriber>,
    next_open_attempt_at: Instant,
    last_warning_at: Option<Instant>,
}

impl IpcFeed {
    fn new(exchange: MarkPriceExchange, service_name: &'static str) -> Self {
        Self {
            exchange,
            service_name,
            subscriber: None,
            next_open_attempt_at: Instant::now(),
            last_warning_at: None,
        }
    }

    fn ensure_subscriber(&mut self, node: &Node<ipc::Service>) {
        if self.subscriber.is_some() || Instant::now() < self.next_open_attempt_at {
            return;
        }
        match open_derivatives_subscriber(node, self.service_name) {
            Ok(subscriber) => {
                info!(
                    exchange = self.exchange.as_str(),
                    service = self.service_name,
                    "mark price IPC subscribed"
                );
                self.subscriber = Some(subscriber);
                self.last_warning_at = None;
            }
            Err(error) => {
                let now = Instant::now();
                if self
                    .last_warning_at
                    .is_none_or(|last| now.duration_since(last) >= IPC_WARNING_INTERVAL)
                {
                    warn!(
                        exchange = self.exchange.as_str(),
                        service = self.service_name,
                        ?error,
                        "mark price IPC service unavailable"
                    );
                    self.last_warning_at = Some(now);
                }
                self.next_open_attempt_at = now + IPC_RECONNECT_INTERVAL;
            }
        }
    }

    fn drain(&mut self, cache: &MarkPriceCache) {
        let Some(subscriber) = self.subscriber.as_ref() else {
            return;
        };
        for _ in 0..DERIVATIVES_SUBSCRIBER_BUFFER {
            match subscriber.receive() {
                Ok(Some(sample)) => {
                    if let Some(update) = parse_mark_price(sample.payload()) {
                        cache.update(
                            self.exchange,
                            &update.symbol,
                            update.price,
                            update.exchange_ts_ms,
                        );
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    warn!(
                        exchange = self.exchange.as_str(),
                        service = self.service_name,
                        ?error,
                        "mark price IPC receive failed; reconnecting"
                    );
                    self.subscriber = None;
                    self.next_open_attempt_at = Instant::now() + IPC_RECONNECT_INTERVAL;
                    break;
                }
            }
        }
    }
}

fn run_ipc_listener(cache: MarkPriceCache) {
    let node_name = format!("crypto_nav_mark_prices_{}", process::id());
    let node = match NodeBuilder::new()
        .name(&NodeName::new(&node_name).expect("valid mark price node name"))
        .create::<ipc::Service>()
    {
        Ok(node) => node,
        Err(error) => {
            warn!(?error, "mark price IPC node creation failed");
            return;
        }
    };
    let mut feeds = [
        IpcFeed::new(
            MarkPriceExchange::Binance,
            "dat_pbs/binance-futures/derivatives",
        ),
        IpcFeed::new(MarkPriceExchange::Gate, "dat_pbs/gate-futures/derivatives"),
        IpcFeed::new(MarkPriceExchange::Okx, "dat_pbs/okex-futures/derivatives"),
    ];
    loop {
        for feed in &mut feeds {
            feed.ensure_subscriber(&node);
            feed.drain(&cache);
        }
        thread::sleep(IPC_POLL_INTERVAL);
    }
}

fn open_derivatives_subscriber(
    node: &Node<ipc::Service>,
    service_name: &str,
) -> Result<DerivativesSubscriber> {
    let service = node
        .service_builder(&ServiceName::new(service_name)?)
        .publish_subscribe::<[u8; DERIVATIVES_PAYLOAD_BYTES]>()
        .max_publishers(1)
        .max_subscribers(DERIVATIVES_MAX_SUBSCRIBERS)
        .history_size(DERIVATIVES_HISTORY_SIZE)
        .subscriber_max_buffer_size(DERIVATIVES_SUBSCRIBER_BUFFER)
        .open()
        .with_context(|| format!("open {service_name}"))?;
    service
        .subscriber_builder()
        .buffer_size(DERIVATIVES_SUBSCRIBER_BUFFER)
        .create()
        .with_context(|| format!("create subscriber for {service_name}"))
}

#[derive(Debug, PartialEq)]
struct MarkPriceUpdate {
    symbol: String,
    price: f64,
    exchange_ts_ms: i64,
}

fn parse_mark_price(payload: &[u8]) -> Option<MarkPriceUpdate> {
    if payload.len() < 24 || read_u32(payload, 0)? != MARK_PRICE_MESSAGE_TYPE {
        return None;
    }
    let symbol_len = read_u32(payload, 4)? as usize;
    let price_offset = 8usize.checked_add(symbol_len)?;
    let timestamp_offset = price_offset.checked_add(8)?;
    if timestamp_offset.checked_add(8)? > payload.len() {
        return None;
    }
    let symbol = std::str::from_utf8(payload.get(8..price_offset)?).ok()?;
    let symbol = normalize_symbol(symbol)?;
    let price = read_f64(payload, price_offset)?;
    let exchange_ts_ms = read_i64(payload, timestamp_offset)?;
    if !price.is_finite() || price <= 0.0 || exchange_ts_ms <= 0 {
        return None;
    }
    Some(MarkPriceUpdate {
        symbol,
        price,
        exchange_ts_ms,
    })
}

fn normalize_symbol(value: &str) -> Option<String> {
    let symbol = value
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | '/'))
        .flat_map(char::to_uppercase)
        .collect::<String>();
    (!symbol.is_empty()
        && symbol
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
    .then_some(symbol)
}

fn read_u32(payload: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        payload.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_i64(payload: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_le_bytes(
        payload.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn read_f64(payload: &[u8], offset: usize) -> Option<f64> {
    Some(f64::from_le_bytes(
        payload.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_padded_mark_price_payload() {
        let symbol = b"BNBUSDT";
        let mut payload = [0u8; DERIVATIVES_PAYLOAD_BYTES];
        payload[0..4].copy_from_slice(&MARK_PRICE_MESSAGE_TYPE.to_le_bytes());
        payload[4..8].copy_from_slice(&(symbol.len() as u32).to_le_bytes());
        payload[8..8 + symbol.len()].copy_from_slice(symbol);
        let price_offset = 8 + symbol.len();
        payload[price_offset..price_offset + 8].copy_from_slice(&578.25f64.to_le_bytes());
        payload[price_offset + 8..price_offset + 16]
            .copy_from_slice(&1_700_000_000_123i64.to_le_bytes());

        assert_eq!(
            parse_mark_price(&payload),
            Some(MarkPriceUpdate {
                symbol: "BNBUSDT".to_string(),
                price: 578.25,
                exchange_ts_ms: 1_700_000_000_123,
            })
        );
    }

    #[test]
    fn cache_is_partitioned_by_exchange_and_rejects_older_updates() {
        let cache = MarkPriceCache::default();
        assert!(cache.update(MarkPriceExchange::Binance, "BNBUSDT", 578.0, 20));
        assert!(cache.update(MarkPriceExchange::Gate, "BNB_USDT", 579.0, 20));
        assert!(!cache.update(MarkPriceExchange::Binance, "BNBUSDT", 500.0, 10));

        assert_eq!(
            cache.price(MarkPriceExchange::Binance, "bnb_usdt"),
            Some(578.0)
        );
        assert_eq!(cache.price(MarkPriceExchange::Gate, "BNBUSDT"), Some(579.0));
    }

    #[test]
    fn rejects_invalid_mark_price_payloads() {
        let mut payload = [0u8; DERIVATIVES_PAYLOAD_BYTES];
        payload[0..4].copy_from_slice(&MARK_PRICE_MESSAGE_TYPE.to_le_bytes());
        payload[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_mark_price(&payload), None);
    }
}
