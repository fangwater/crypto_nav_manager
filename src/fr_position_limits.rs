use crate::strategy_env::read_env_file;
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Client, Url};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Mutex, task::JoinSet};

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;

pub const ALERT_THRESHOLD_RATIO: f64 = 0.90;
const CACHE_TTL: Duration = Duration::from_secs(20);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const POSITION_DISPLAY_MIN_USD: f64 = 1.0;
const DEFAULT_AMOUNT_U: f64 = 100.0;

#[derive(Clone, Debug)]
pub struct FrLimitSource {
    pub strategy_slug: String,
    pub host: String,
    pub env_path: String,
    pub exchange: String,
    pub snapshot_url: String,
    pub sort_order: usize,
}

#[derive(Clone)]
pub struct FrPositionLimitMonitor {
    client: Client,
    cache: Arc<Mutex<Option<CachedOverview>>>,
}

struct CachedOverview {
    stored_at: Instant,
    overview: FrPositionLimitOverview,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrPositionLimitOverview {
    pub generated_at_ms: i64,
    pub alert_threshold_ratio: f64,
    pub environments: Vec<FrLimitEnvironment>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrLimitEnvironment {
    pub strategy_slug: String,
    pub exchange: String,
    pub status: String,
    pub snapshot_ts_ms: Option<i64>,
    pub exchange_fetched_at_ms: Option<i64>,
    pub params_live: bool,
    pub source_counts: FrLimitSourceCounts,
    pub rows: Vec<FrLimitRow>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    #[serde(skip)]
    sort_order: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrLimitSourceCounts {
    pub snapshot_rows: usize,
    pub exchange_limit_rows: usize,
    pub exchange_position_rows: usize,
    pub displayed_rows: usize,
    pub near_limit_rows: usize,
    pub symbol_config_rows: Option<usize>,
    pub position_risk_rows: Option<usize>,
    pub leverage_bracket_rows: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrLimitRow {
    pub symbol: String,
    pub asset: String,
    pub status: String,
    pub side: String,
    pub tracked_in_snapshot: bool,
    pub position_source: String,
    pub position_notional_usdt: f64,
    pub snapshot_open_usdt: Option<f64>,
    pub snapshot_futures_usdt: Option<f64>,
    pub snapshot_rest_delta_usdt: Option<f64>,
    pub exchange_limit_usdt: Option<f64>,
    pub guard_buffer_usdt: Option<f64>,
    pub guard_cap_usdt: Option<f64>,
    pub remaining_usdt: Option<f64>,
    pub usage_ratio: Option<f64>,
    pub near_limit: bool,
    pub amount_u: Option<f64>,
    pub pending_limit_orders: Option<i32>,
    pub leverage: Option<f64>,
    pub symbol_config_limit_usdt: Option<f64>,
    pub position_risk_limit_usdt: Option<f64>,
    pub bracket_limit_usdt: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct SnapshotPosition {
    asset: String,
    open_usdt: Option<f64>,
    futures_usdt: Option<f64>,
}

#[derive(Debug)]
struct SnapshotData {
    ts_ms: i64,
    positions: BTreeMap<String, SnapshotPosition>,
}

#[derive(Clone, Debug)]
struct Credentials {
    api_key: String,
    secret_key: String,
    api_base: String,
    settle: String,
}

#[derive(Clone, Debug)]
struct GuardParams {
    pending_buy: i32,
    pending_sell: i32,
    default_amount_u: f64,
    amount_u_overrides: HashMap<String, f64>,
}

impl Default for GuardParams {
    fn default() -> Self {
        Self {
            pending_buy: 0,
            pending_sell: 0,
            default_amount_u: DEFAULT_AMOUNT_U,
            amount_u_overrides: HashMap::new(),
        }
    }
}

impl GuardParams {
    fn amount_u(&self, symbol: &str) -> f64 {
        self.amount_u_overrides
            .get(symbol)
            .copied()
            .unwrap_or(self.default_amount_u)
    }

    fn pending_for_position(&self, position_notional: f64) -> i32 {
        if position_notional < 0.0 {
            self.pending_buy
        } else if position_notional > 0.0 {
            self.pending_sell
        } else {
            self.pending_buy.max(self.pending_sell)
        }
        .max(0)
    }
}

struct GuardParamsLoad {
    params: GuardParams,
    live: bool,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct BinanceLimit {
    symbol: String,
    symbol_config_limit: Option<f64>,
    position_risk_limit: Option<f64>,
    bracket_limit: Option<f64>,
    leverage: Option<f64>,
    current_notional: Option<f64>,
}

impl BinanceLimit {
    fn effective_limit(&self) -> Option<f64> {
        let mut cap =
            positive(self.symbol_config_limit).or_else(|| positive(self.position_risk_limit));
        if let Some(bracket) = positive(self.bracket_limit) {
            cap = Some(cap.map_or(bracket, |current| current.min(bracket)));
        }
        cap
    }
}

struct BinanceData {
    limits: BTreeMap<String, BinanceLimit>,
    symbol_config_rows: usize,
    position_risk_rows: usize,
    leverage_bracket_rows: usize,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct GateLimit {
    contract: String,
    risk_limit: f64,
    current_notional: f64,
    cross_leverage_limit: Option<f64>,
}

struct GateData {
    limits: BTreeMap<String, GateLimit>,
}

enum ExchangeData {
    Binance(BinanceData),
    Gate(GateData),
}

struct GuardMetrics {
    buffer: f64,
    cap: f64,
    remaining: f64,
    usage_ratio: f64,
    amount_u: f64,
    pending: i32,
}

impl FrPositionLimitMonitor {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("build FR position-limit HTTP client")?;
        Ok(Self {
            client,
            cache: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn overview(&self, sources: Vec<FrLimitSource>) -> FrPositionLimitOverview {
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.stored_at.elapsed() < CACHE_TTL
        {
            return cached.overview.clone();
        }

        let mut tasks = JoinSet::new();
        for source in sources {
            let client = self.client.clone();
            tasks.spawn(async move { fetch_environment(&client, source).await });
        }

        let mut environments = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(environment) => environments.push(environment),
                Err(error) => environments.push(FrLimitEnvironment {
                    strategy_slug: "unknown".to_string(),
                    exchange: "unknown".to_string(),
                    status: "error".to_string(),
                    snapshot_ts_ms: None,
                    exchange_fetched_at_ms: None,
                    params_live: false,
                    source_counts: FrLimitSourceCounts::default(),
                    rows: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(format!("monitor task failed: {error}")),
                    sort_order: usize::MAX,
                }),
            }
        }
        environments.sort_by_key(|environment| environment.sort_order);

        let overview = FrPositionLimitOverview {
            generated_at_ms: Utc::now().timestamp_millis(),
            alert_threshold_ratio: ALERT_THRESHOLD_RATIO,
            environments,
        };
        *cache = Some(CachedOverview {
            stored_at: Instant::now(),
            overview: overview.clone(),
        });
        overview
    }
}

async fn fetch_environment(client: &Client, source: FrLimitSource) -> FrLimitEnvironment {
    let credentials = match load_credentials(&source).await {
        Ok(credentials) => credentials,
        Err(error) => return failed_environment(source, format!("{error:#}"), Vec::new()),
    };

    let snapshot_future = fetch_snapshot(client, &source);
    let params_future = fetch_guard_params(client, &source);
    let exchange_future = fetch_exchange_data(client, &source, &credentials);
    let (snapshot_result, params_result, exchange_result) =
        tokio::join!(snapshot_future, params_future, exchange_future);

    let mut warnings = Vec::new();
    let (snapshot_ts_ms, snapshot_positions) = match snapshot_result {
        Ok(snapshot) => (Some(snapshot.ts_ms), snapshot.positions),
        Err(error) => {
            warnings.push(format!("snapshot unavailable: {error:#}"));
            (None, BTreeMap::new())
        }
    };

    let params_load = match params_result {
        Ok(params) => params,
        Err(error) => GuardParamsLoad {
            params: GuardParams::default(),
            live: false,
            warnings: vec![format!(
                "pre-trade params unavailable; exchange-limit fallback is active: {error:#}"
            )],
        },
    };
    warnings.extend(params_load.warnings);

    let exchange_data = match exchange_result {
        Ok(data) => data,
        Err(error) => {
            return FrLimitEnvironment {
                strategy_slug: source.strategy_slug,
                exchange: source.exchange,
                status: "error".to_string(),
                snapshot_ts_ms,
                exchange_fetched_at_ms: None,
                params_live: params_load.live,
                source_counts: FrLimitSourceCounts {
                    snapshot_rows: snapshot_positions.len(),
                    ..FrLimitSourceCounts::default()
                },
                rows: Vec::new(),
                warnings,
                error: Some(format!("exchange limits unavailable: {error:#}")),
                sort_order: source.sort_order,
            };
        }
    };

    let exchange_fetched_at_ms = Utc::now().timestamp_millis();
    let (mut rows, mut counts, exchange_warnings) = match exchange_data {
        ExchangeData::Binance(data) => {
            let warnings = data.warnings.clone();
            let (rows, counts) =
                build_binance_rows(&data, &snapshot_positions, &params_load.params);
            (rows, counts, warnings)
        }
        ExchangeData::Gate(data) => {
            let (rows, counts) = build_gate_rows(&data, &snapshot_positions, &params_load.params);
            (rows, counts, Vec::new())
        }
    };
    warnings.extend(exchange_warnings);
    sort_rows(&mut rows);
    counts.snapshot_rows = snapshot_positions.len();
    counts.displayed_rows = rows.len();
    counts.near_limit_rows = rows.iter().filter(|row| row.near_limit).count();

    let status = if counts.near_limit_rows > 0 || !warnings.is_empty() {
        "warning"
    } else {
        "healthy"
    };

    FrLimitEnvironment {
        strategy_slug: source.strategy_slug,
        exchange: source.exchange,
        status: status.to_string(),
        snapshot_ts_ms,
        exchange_fetched_at_ms: Some(exchange_fetched_at_ms),
        params_live: params_load.live,
        source_counts: counts,
        rows,
        warnings,
        error: None,
        sort_order: source.sort_order,
    }
}

fn failed_environment(
    source: FrLimitSource,
    error: String,
    warnings: Vec<String>,
) -> FrLimitEnvironment {
    FrLimitEnvironment {
        strategy_slug: source.strategy_slug,
        exchange: source.exchange,
        status: "error".to_string(),
        snapshot_ts_ms: None,
        exchange_fetched_at_ms: None,
        params_live: false,
        source_counts: FrLimitSourceCounts::default(),
        rows: Vec::new(),
        warnings,
        error: Some(error),
        sort_order: source.sort_order,
    }
}

async fn load_credentials(source: &FrLimitSource) -> Result<Credentials> {
    let host = source.host.clone();
    let path = PathBuf::from(&source.env_path);
    let content = tokio::task::spawn_blocking(move || read_env_file(&host, &path))
        .await
        .context("join env reader")??;
    let values = parse_env_values(&content);
    let (api_key_name, secret_name, default_base) = match source.exchange.as_str() {
        "binance" => (
            "BINANCE_API_KEY",
            "BINANCE_API_SECRET",
            "https://papi.binance.com",
        ),
        "gate" => ("GATE_API_KEY", "GATE_API_SECRET", "https://api.gateio.ws"),
        exchange => bail!("unsupported FR limit exchange: {exchange}"),
    };
    let api_key = required_env_value(&values, api_key_name)?;
    let secret_key = required_env_value(&values, secret_name)?;
    let api_base_name = if source.exchange == "binance" {
        "BINANCE_PAPI_URL"
    } else {
        "GATE_API_BASE"
    };
    let api_base = values
        .get(api_base_name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(default_base)
        .trim_end_matches('/')
        .to_string();
    let settle = values
        .get("GATE_FUTURES_SETTLE")
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "usdt".to_string());
    if !settle
        .chars()
        .all(|character| character.is_ascii_alphanumeric())
    {
        bail!("GATE_FUTURES_SETTLE contains unsupported characters");
    }
    Ok(Credentials {
        api_key,
        secret_key,
        api_base,
        settle,
    })
}

fn required_env_value(values: &HashMap<String, String>, name: &str) -> Result<String> {
    values
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .with_context(|| format!("{name} is not assigned"))
}

fn parse_env_values(content: &str) -> HashMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let (key, raw_value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty()
                || !key
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric())
            {
                return None;
            }
            let value = raw_value.trim();
            let value = if value.len() >= 2
                && matches!(value.as_bytes()[0], b'\'' | b'"')
                && value.as_bytes()[0] == value.as_bytes()[value.len() - 1]
            {
                &value[1..value.len() - 1]
            } else {
                value
                    .split_once(" #")
                    .map_or(value, |(head, _)| head)
                    .trim()
            };
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

async fn fetch_snapshot(client: &Client, source: &FrLimitSource) -> Result<SnapshotData> {
    let value = get_json(client.get(&source.snapshot_url), "FR snapshot").await?;
    parse_snapshot(&value)
}

fn parse_snapshot(value: &Value) -> Result<SnapshotData> {
    let ts_ms = value
        .get("ts_ms")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .context("snapshot has no valid ts_ms")?;
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .context("snapshot entries is not an array")?;
    let mut positions: BTreeMap<String, SnapshotPosition> = BTreeMap::new();
    let mut found = false;
    for envelope in entries {
        if envelope.get("type").and_then(Value::as_str) != Some("pre_trade_exposure") {
            continue;
        }
        found = true;
        let rows = envelope
            .get("entry")
            .and_then(|entry| entry.get("rows"))
            .and_then(Value::as_array)
            .context("pre_trade_exposure rows is not an array")?;
        for row in rows {
            if row.get("is_total").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(asset) = row
                .get("asset")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|asset| !asset.is_empty())
            else {
                continue;
            };
            let Some(symbol) = normalize_symbol(asset) else {
                continue;
            };
            let entry = positions.entry(symbol).or_default();
            if entry.asset.is_empty() {
                entry.asset = asset.to_ascii_uppercase();
            }
            entry.open_usdt = add_optional(entry.open_usdt, number_field(row, "open_usdt"));
            entry.futures_usdt = add_optional(entry.futures_usdt, number_field(row, "hedge_usdt"));
        }
    }
    if !found {
        bail!("snapshot has no pre_trade_exposure entry");
    }
    Ok(SnapshotData { ts_ms, positions })
}

async fn fetch_guard_params(client: &Client, source: &FrLimitSource) -> Result<GuardParamsLoad> {
    let risk_url = config_api_url(source, "risk-params")?;
    let strategy_url = config_api_url(source, "strategy-params")?;
    let amount_url = config_api_url(source, "amount-u")?;
    let (risk_result, strategy_result, amount_result) = tokio::join!(
        get_json(client.get(risk_url), "risk params"),
        get_json(client.get(strategy_url), "strategy params"),
        get_json(client.get(amount_url), "amount-u overrides"),
    );
    let risk = payload_values(&risk_result?)?;
    let strategy = payload_values(&strategy_result?)?;

    let pending_buy = map_i32(&risk, "arb_max_pending_limit_buy_orders")
        .unwrap_or(0)
        .max(0);
    let pending_sell = map_i32(&risk, "arb_max_pending_limit_sell_orders")
        .unwrap_or(0)
        .max(0);
    let default_amount_u = map_f64(&strategy, "order_amount")
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_AMOUNT_U);

    let mut warnings = Vec::new();
    let (amount_u_overrides, live) = match amount_result {
        Ok(value) => (parse_amount_u_overrides(&value), true),
        Err(error) => {
            warnings.push(format!(
                "amount-u overrides unavailable; using strategy order_amount: {error:#}"
            ));
            (HashMap::new(), false)
        }
    };

    Ok(GuardParamsLoad {
        params: GuardParams {
            pending_buy,
            pending_sell,
            default_amount_u,
            amount_u_overrides,
        },
        live,
        warnings,
    })
}

fn config_api_url(source: &FrLimitSource, endpoint: &str) -> Result<Url> {
    let mut url = Url::parse(&source.snapshot_url).context("parse snapshot URL")?;
    let current_path = url.path().trim_end_matches('/');
    let base = current_path
        .strip_suffix("/snapshot")
        .context("snapshot URL does not end in /snapshot")?;
    url.set_path(&format!("{base}/config/api/{endpoint}"));
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("exchange", &source.exchange)
        .append_pair(
            "open_venue",
            if source.exchange == "binance" {
                "binance-margin"
            } else {
                "gate-margin"
            },
        )
        .append_pair(
            "hedge_venue",
            if source.exchange == "binance" {
                "binance-futures"
            } else {
                "gate-futures"
            },
        );
    Ok(url)
}

fn payload_values(value: &Value) -> Result<HashMap<String, Value>> {
    value
        .get("values")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .context("config response has no values object")
}

fn parse_amount_u_overrides(value: &Value) -> HashMap<String, f64> {
    value
        .get("values")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(symbol, value)| {
            let symbol = normalize_symbol(symbol)?;
            let amount = number_value(value)?;
            (amount.is_finite() && amount > 0.0).then_some((symbol, amount))
        })
        .collect()
}

async fn fetch_exchange_data(
    client: &Client,
    source: &FrLimitSource,
    credentials: &Credentials,
) -> Result<ExchangeData> {
    match source.exchange.as_str() {
        "binance" => fetch_binance_data(client, credentials)
            .await
            .map(ExchangeData::Binance),
        "gate" => fetch_gate_data(client, credentials)
            .await
            .map(ExchangeData::Gate),
        exchange => bail!("unsupported FR limit exchange: {exchange}"),
    }
}

async fn fetch_binance_data(client: &Client, credentials: &Credentials) -> Result<BinanceData> {
    let (config_result, position_result, bracket_result) = tokio::join!(
        binance_signed_get(
            client,
            credentials,
            "/papi/v1/um/symbolConfig",
            "Binance symbolConfig"
        ),
        binance_signed_get(
            client,
            credentials,
            "/papi/v1/um/positionRisk",
            "Binance positionRisk"
        ),
        binance_signed_get(
            client,
            credentials,
            "/papi/v1/um/leverageBracket",
            "Binance leverageBracket"
        ),
    );

    let config_value = config_result?;
    let config_rows = root_array(&config_value, "Binance symbolConfig")?;
    let mut limits = parse_binance_symbol_configs(config_rows);
    let symbol_config_rows = config_rows.len();
    let mut warnings = Vec::new();

    let position_risk_rows = match position_result {
        Ok(value) => {
            let rows = root_array(&value, "Binance positionRisk")?;
            merge_binance_positions(&mut limits, rows);
            rows.len()
        }
        Err(error) => {
            warnings.push(format!(
                "Binance positionRisk unavailable; snapshot notional fallback is active: {error:#}"
            ));
            0
        }
    };

    let leverage_bracket_rows = match bracket_result {
        Ok(value) => {
            let rows = root_array(&value, "Binance leverageBracket")?;
            merge_binance_brackets(&mut limits, rows);
            rows.len()
        }
        Err(error) => {
            warnings.push(format!(
                "Binance leverageBracket unavailable; symbolConfig cap is active: {error:#}"
            ));
            0
        }
    };

    Ok(BinanceData {
        limits,
        symbol_config_rows,
        position_risk_rows,
        leverage_bracket_rows,
        warnings,
    })
}

fn parse_binance_symbol_configs(rows: &[Value]) -> BTreeMap<String, BinanceLimit> {
    let mut limits = BTreeMap::new();
    for row in rows {
        let Some(raw_symbol) = row.get("symbol").and_then(Value::as_str) else {
            continue;
        };
        let Some(symbol) = normalize_symbol(raw_symbol) else {
            continue;
        };
        let Some(cap) = positive(number_field(row, "maxNotionalValue")) else {
            continue;
        };
        limits.insert(
            symbol.clone(),
            BinanceLimit {
                symbol: raw_symbol.trim().to_ascii_uppercase(),
                symbol_config_limit: Some(cap),
                leverage: positive(number_field(row, "leverage")),
                ..BinanceLimit::default()
            },
        );
    }
    limits
}

fn merge_binance_positions(limits: &mut BTreeMap<String, BinanceLimit>, rows: &[Value]) {
    for row in rows {
        let Some(raw_symbol) = row.get("symbol").and_then(Value::as_str) else {
            continue;
        };
        let Some(symbol) = normalize_symbol(raw_symbol) else {
            continue;
        };
        let position_amount = number_field(row, "positionAmt").unwrap_or(0.0);
        let mark_price = number_field(row, "markPrice").unwrap_or(0.0);
        let mut notional = number_field(row, "notional");
        if notional.is_none_or(|value| value.abs() < f64::EPSILON)
            && position_amount.abs() > f64::EPSILON
            && mark_price > 0.0
        {
            notional = Some(position_amount * mark_price);
        }
        let record = limits.entry(symbol).or_default();
        if record.symbol.is_empty() {
            record.symbol = raw_symbol.trim().to_ascii_uppercase();
        }
        let position_risk_limit = positive(number_field(row, "maxNotionalValue"));
        record.position_risk_limit =
            minimum_positive(record.position_risk_limit, position_risk_limit);
        record.current_notional = add_optional(
            record.current_notional,
            notional.filter(|value| value.is_finite()),
        );
        if record.leverage.is_none() {
            record.leverage = positive(number_field(row, "leverage"));
        }
    }
}

fn merge_binance_brackets(limits: &mut BTreeMap<String, BinanceLimit>, rows: &[Value]) {
    for row in rows {
        let Some(raw_symbol) = row.get("symbol").and_then(Value::as_str) else {
            continue;
        };
        let Some(symbol) = normalize_symbol(raw_symbol) else {
            continue;
        };
        let max_cap = row
            .get("brackets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|bracket| positive(number_field(bracket, "notionalCap")))
            .max_by(f64::total_cmp);
        let Some(max_cap) = max_cap else {
            continue;
        };
        if let Some(record) = limits.get_mut(&symbol) {
            record.bracket_limit = Some(max_cap);
        }
    }
}

async fn binance_signed_get(
    client: &Client,
    credentials: &Credentials,
    path: &str,
    label: &str,
) -> Result<Value> {
    let query = format!(
        "recvWindow=5000&timestamp={}",
        Utc::now().timestamp_millis()
    );
    let mut mac = HmacSha256::new_from_slice(credentials.secret_key.as_bytes())
        .expect("HMAC accepts any key");
    mac.update(query.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    let url = format!(
        "{}{}?{}&signature={}",
        credentials.api_base, path, query, signature
    );
    get_json(
        client
            .get(url)
            .header("Accept", "application/json")
            .header("X-MBX-APIKEY", &credentials.api_key),
        label,
    )
    .await
}

async fn fetch_gate_data(client: &Client, credentials: &Credentials) -> Result<GateData> {
    let request_path = format!("/api/v4/futures/{}/positions", credentials.settle);
    let timestamp = Utc::now().timestamp();
    let body_hash = hex::encode(Sha512::digest(b""));
    let payload = format!("GET\n{request_path}\n\n{body_hash}\n{timestamp}");
    let mut mac = HmacSha512::new_from_slice(credentials.secret_key.as_bytes())
        .expect("HMAC accepts any key");
    mac.update(payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    let base = credentials
        .api_base
        .strip_suffix("/api/v4")
        .unwrap_or(&credentials.api_base);
    let value = get_json(
        client
            .get(format!("{base}{request_path}"))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("X-Gate-Size-Decimal", "1")
            .header("KEY", &credentials.api_key)
            .header("Timestamp", timestamp.to_string())
            .header("SIGN", signature),
        "Gate futures positions",
    )
    .await?;
    let rows = root_array(&value, "Gate futures positions")?;
    Ok(GateData {
        limits: parse_gate_positions(rows),
    })
}

fn parse_gate_positions(rows: &[Value]) -> BTreeMap<String, GateLimit> {
    let mut limits: BTreeMap<String, GateLimit> = BTreeMap::new();
    for row in rows {
        let Some(contract) = row.get("contract").and_then(Value::as_str) else {
            continue;
        };
        let Some(symbol) = normalize_symbol(contract) else {
            continue;
        };
        let Some(risk_limit) = positive(number_field(row, "risk_limit")) else {
            continue;
        };
        let raw_value = number_field(row, "value")
            .filter(|value| value.is_finite())
            .unwrap_or(0.0);
        let current_notional = match number_field(row, "size") {
            Some(size) if size < 0.0 => -raw_value.abs(),
            Some(size) if size > 0.0 => raw_value.abs(),
            Some(_) => 0.0,
            None => raw_value,
        };
        let cross_leverage_limit = positive(number_field(row, "cross_leverage_limit"));

        if let Some(record) = limits.get_mut(&symbol) {
            record.risk_limit = record.risk_limit.min(risk_limit);
            record.current_notional += current_notional;
            record.cross_leverage_limit =
                minimum_positive(record.cross_leverage_limit, cross_leverage_limit);
        } else {
            limits.insert(
                symbol,
                GateLimit {
                    contract: contract.trim().to_ascii_uppercase(),
                    risk_limit,
                    current_notional,
                    cross_leverage_limit,
                },
            );
        }
    }
    limits
}

async fn get_json(builder: reqwest::RequestBuilder, label: &str) -> Result<Value> {
    let response = builder
        .send()
        .await
        .map_err(|error| anyhow!("{label} request failed: {}", error.without_url()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("{label} body read failed"))?;
    if !status.is_success() {
        bail!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate(&body, 300)
        );
    }
    serde_json::from_str(&body).with_context(|| format!("{label} response is not JSON"))
}

fn root_array<'a>(value: &'a Value, label: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .with_context(|| format!("{label} response is not an array"))
}

fn build_binance_rows(
    data: &BinanceData,
    snapshot: &BTreeMap<String, SnapshotPosition>,
    params: &GuardParams,
) -> (Vec<FrLimitRow>, FrLimitSourceCounts) {
    let mut symbols = relevant_snapshot_symbols(snapshot);
    symbols.extend(data.limits.iter().filter_map(|(symbol, limit)| {
        limit
            .current_notional
            .is_some_and(|value| value.abs() >= POSITION_DISPLAY_MIN_USD)
            .then(|| symbol.clone())
    }));

    let rows = symbols
        .into_iter()
        .map(|symbol| {
            let snapshot_position = snapshot.get(&symbol);
            let record = data.limits.get(&symbol);
            build_row(
                record
                    .map(|record| record.symbol.clone())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| symbol.clone()),
                &symbol,
                snapshot_position,
                record.and_then(|record| record.current_notional),
                record.and_then(BinanceLimit::effective_limit),
                record.and_then(|record| record.leverage),
                record.and_then(|record| record.symbol_config_limit),
                record.and_then(|record| record.position_risk_limit),
                record.and_then(|record| record.bracket_limit),
                params,
            )
        })
        .collect();

    let counts = FrLimitSourceCounts {
        exchange_limit_rows: data
            .limits
            .values()
            .filter(|record| record.effective_limit().is_some())
            .count(),
        exchange_position_rows: data
            .limits
            .values()
            .filter(|record| {
                record
                    .current_notional
                    .is_some_and(|value| value.abs() >= POSITION_DISPLAY_MIN_USD)
            })
            .count(),
        symbol_config_rows: Some(data.symbol_config_rows),
        position_risk_rows: Some(data.position_risk_rows),
        leverage_bracket_rows: Some(data.leverage_bracket_rows),
        ..FrLimitSourceCounts::default()
    };
    (rows, counts)
}

fn build_gate_rows(
    data: &GateData,
    snapshot: &BTreeMap<String, SnapshotPosition>,
    params: &GuardParams,
) -> (Vec<FrLimitRow>, FrLimitSourceCounts) {
    let mut symbols = relevant_snapshot_symbols(snapshot);
    symbols.extend(data.limits.iter().filter_map(|(symbol, limit)| {
        (limit.current_notional.abs() >= POSITION_DISPLAY_MIN_USD).then(|| symbol.clone())
    }));

    let rows = symbols
        .into_iter()
        .map(|symbol| {
            let snapshot_position = snapshot.get(&symbol);
            let record = data.limits.get(&symbol);
            build_row(
                record
                    .map(|record| record.contract.clone())
                    .unwrap_or_else(|| gate_contract(&symbol)),
                &symbol,
                snapshot_position,
                record.map(|record| record.current_notional),
                record.map(|record| record.risk_limit),
                record.and_then(|record| record.cross_leverage_limit),
                None,
                None,
                None,
                params,
            )
        })
        .collect();

    let counts = FrLimitSourceCounts {
        exchange_limit_rows: data.limits.len(),
        exchange_position_rows: data
            .limits
            .values()
            .filter(|record| record.current_notional.abs() >= POSITION_DISPLAY_MIN_USD)
            .count(),
        ..FrLimitSourceCounts::default()
    };
    (rows, counts)
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    display_symbol: String,
    normalized_symbol: &str,
    snapshot: Option<&SnapshotPosition>,
    exchange_notional: Option<f64>,
    exchange_limit: Option<f64>,
    leverage: Option<f64>,
    symbol_config_limit: Option<f64>,
    position_risk_limit: Option<f64>,
    bracket_limit: Option<f64>,
    params: &GuardParams,
) -> FrLimitRow {
    let snapshot_futures = snapshot.and_then(|position| position.futures_usdt);
    let position_notional = exchange_notional.or(snapshot_futures).unwrap_or(0.0);
    let position_source = if exchange_notional.is_some() {
        "exchange"
    } else {
        "snapshot"
    };
    let asset = snapshot
        .map(|position| position.asset.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| asset_from_symbol(normalized_symbol));

    let (metrics, metrics_error) = match exchange_limit {
        Some(limit) => match guard_metrics(limit, position_notional, normalized_symbol, params) {
            Ok(metrics) => (Some(metrics), None),
            Err(error) => (None, Some(error)),
        },
        None => (
            None,
            Some("exchange position-limit row is missing".to_string()),
        ),
    };
    let near_limit = metrics
        .as_ref()
        .is_some_and(|metrics| metrics.usage_ratio >= ALERT_THRESHOLD_RATIO);
    let status = if near_limit {
        "warning"
    } else if metrics.is_none() {
        "unavailable"
    } else {
        "healthy"
    };

    FrLimitRow {
        symbol: display_symbol,
        asset,
        status: status.to_string(),
        side: position_side(position_notional).to_string(),
        tracked_in_snapshot: snapshot.is_some(),
        position_source: position_source.to_string(),
        position_notional_usdt: position_notional,
        snapshot_open_usdt: snapshot.and_then(|position| position.open_usdt),
        snapshot_futures_usdt: snapshot_futures,
        snapshot_rest_delta_usdt: exchange_notional
            .zip(snapshot_futures)
            .map(|(exchange, snapshot)| exchange - snapshot),
        exchange_limit_usdt: exchange_limit,
        guard_buffer_usdt: metrics.as_ref().map(|metrics| metrics.buffer),
        guard_cap_usdt: metrics.as_ref().map(|metrics| metrics.cap),
        remaining_usdt: metrics.as_ref().map(|metrics| metrics.remaining),
        usage_ratio: metrics.as_ref().map(|metrics| metrics.usage_ratio),
        near_limit,
        amount_u: metrics.as_ref().map(|metrics| metrics.amount_u),
        pending_limit_orders: metrics.as_ref().map(|metrics| metrics.pending),
        leverage,
        symbol_config_limit_usdt: symbol_config_limit,
        position_risk_limit_usdt: position_risk_limit,
        bracket_limit_usdt: bracket_limit,
        error: metrics_error,
    }
}

fn guard_metrics(
    exchange_limit: f64,
    position_notional: f64,
    symbol: &str,
    params: &GuardParams,
) -> Result<GuardMetrics, String> {
    if !(exchange_limit.is_finite() && exchange_limit > 0.0) {
        return Err("exchange position limit is invalid".to_string());
    }
    let amount_u = params.amount_u(symbol);
    if !(amount_u.is_finite() && amount_u > 0.0) {
        return Err("pre-trade amount_u is invalid".to_string());
    }
    let pending = params.pending_for_position(position_notional);
    let buffer = pending as f64 * amount_u;
    let cap = exchange_limit - buffer;
    if !(cap.is_finite() && cap > 0.0) {
        return Err("pre-trade guard cap is not positive".to_string());
    }
    let absolute_notional = position_notional.abs();
    Ok(GuardMetrics {
        buffer,
        cap,
        remaining: cap - absolute_notional,
        usage_ratio: absolute_notional / cap,
        amount_u,
        pending,
    })
}

fn relevant_snapshot_symbols(snapshot: &BTreeMap<String, SnapshotPosition>) -> BTreeSet<String> {
    snapshot
        .iter()
        .filter_map(|(symbol, position)| {
            let open = position.open_usdt.unwrap_or(0.0).abs();
            let futures = position.futures_usdt.unwrap_or(0.0).abs();
            (open.max(futures) >= POSITION_DISPLAY_MIN_USD).then(|| symbol.clone())
        })
        .collect()
}

fn sort_rows(rows: &mut [FrLimitRow]) {
    rows.sort_by(|left, right| {
        right
            .near_limit
            .cmp(&left.near_limit)
            .then_with(|| {
                right
                    .usage_ratio
                    .unwrap_or(-1.0)
                    .total_cmp(&left.usage_ratio.unwrap_or(-1.0))
            })
            .then_with(|| {
                right
                    .position_notional_usdt
                    .abs()
                    .total_cmp(&left.position_notional_usdt.abs())
            })
            .then_with(|| left.symbol.cmp(&right.symbol))
    });
}

fn normalize_symbol(value: &str) -> Option<String> {
    let value = value.split_once('@').map_or(value, |(head, _)| head);
    let mut cleaned = value
        .trim()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect::<String>();
    if let Some(stripped) = cleaned.strip_suffix("SWAP") {
        cleaned = stripped.to_string();
    }
    if cleaned.is_empty() || cleaned == "USDT" {
        return None;
    }
    if !cleaned.ends_with("USDT") {
        cleaned.push_str("USDT");
    }
    Some(cleaned)
}

fn asset_from_symbol(symbol: &str) -> String {
    symbol.strip_suffix("USDT").unwrap_or(symbol).to_string()
}

fn gate_contract(symbol: &str) -> String {
    format!("{}_USDT", asset_from_symbol(symbol))
}

fn position_side(notional: f64) -> &'static str {
    if notional < -f64::EPSILON {
        "short"
    } else if notional > f64::EPSILON {
        "long"
    } else {
        "flat"
    }
}

fn add_optional(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn minimum_positive(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (positive(left), positive(right)) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn number_field(value: &Value, field: &str) -> Option<f64> {
    value.get(field).and_then(number_value)
}

fn number_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn map_f64(values: &HashMap<String, Value>, key: &str) -> Option<f64> {
    values.get(key).and_then(number_value)
}

fn map_i32(values: &HashMap<String, Value>, key: &str) -> Option<i32> {
    map_f64(values, key).and_then(|value| {
        (value.is_finite() && value >= i32::MIN as f64 && value <= i32::MAX as f64)
            .then_some(value as i32)
    })
}

fn truncate(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    let mut end = max_len.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}...<{} bytes>", &value[..end], value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_aggregates_snapshot_exposure_rows() {
        let snapshot = serde_json::json!({
            "ts_ms": 123,
            "entries": [{
                "type": "pre_trade_exposure",
                "entry": {"rows": [
                    {
                        "asset": "AEVO",
                        "is_total": false,
                        "open_usdt": 100.0,
                        "hedge_usdt": -99.0
                    },
                    {
                        "asset": "AEVO",
                        "is_total": false,
                        "open_usdt": 2.0,
                        "hedge_usdt": -2.0
                    },
                    {"asset": "TOTAL", "is_total": true}
                ]}
            }]
        });

        let parsed = parse_snapshot(&snapshot).unwrap();
        let aevo = parsed.positions.get("AEVOUSDT").unwrap();
        assert_eq!(parsed.ts_ms, 123);
        assert_eq!(aevo.open_usdt, Some(102.0));
        assert_eq!(aevo.futures_usdt, Some(-101.0));
    }

    #[test]
    fn binance_effective_limit_uses_smallest_valid_cap() {
        let limit = BinanceLimit {
            symbol_config_limit: Some(500_000.0),
            position_risk_limit: Some(600_000.0),
            bracket_limit: Some(250_000.0),
            ..BinanceLimit::default()
        };
        assert_eq!(limit.effective_limit(), Some(250_000.0));
    }

    #[test]
    fn binance_positions_are_aggregated_by_symbol() {
        let mut limits = parse_binance_symbol_configs(&[serde_json::json!({
            "symbol": "AINUSDT",
            "maxNotionalValue": "50000"
        })]);
        let rows = vec![
            serde_json::json!({
                "symbol": "AINUSDT",
                "notional": "-1200.5",
                "maxNotionalValue": "40000"
            }),
            serde_json::json!({
                "symbol": "AINUSDT",
                "notional": "200.5",
                "maxNotionalValue": "45000"
            }),
        ];

        merge_binance_positions(&mut limits, &rows);

        let ain = limits.get("AINUSDT").unwrap();
        assert_eq!(ain.current_notional, Some(-1000.0));
        assert_eq!(ain.position_risk_limit, Some(40_000.0));
        assert_eq!(ain.effective_limit(), Some(50_000.0));
    }

    #[test]
    fn gate_positions_use_size_sign_and_aggregate_by_symbol() {
        let rows = vec![
            serde_json::json!({
                "contract": "AIN_USDT",
                "risk_limit": "20000",
                "size": "-2255",
                "value": "16966.62",
                "cross_leverage_limit": "3"
            }),
            serde_json::json!({
                "contract": "AIN_USDT",
                "risk_limit": "25000",
                "size": "10",
                "value": "75",
                "cross_leverage_limit": "4"
            }),
        ];

        let limits = parse_gate_positions(&rows);
        let ain = limits.get("AINUSDT").unwrap();

        assert!((ain.current_notional + 16_891.62).abs() < 1e-9);
        assert_eq!(ain.risk_limit, 20_000.0);
        assert_eq!(ain.cross_leverage_limit, Some(3.0));
    }

    #[test]
    fn guard_metrics_match_side_specific_pre_trade_buffer() {
        let params = GuardParams {
            pending_buy: 10,
            pending_sell: 3,
            default_amount_u: 100.0,
            amount_u_overrides: HashMap::from([("AEVOUSDT".to_string(), 250.0)]),
        };
        let short = guard_metrics(100_000.0, -90_000.0, "AEVOUSDT", &params).unwrap();
        let long = guard_metrics(100_000.0, 90_000.0, "AEVOUSDT", &params).unwrap();

        assert_eq!(short.buffer, 2_500.0);
        assert_eq!(short.cap, 97_500.0);
        assert_eq!(long.buffer, 750.0);
        assert_eq!(long.cap, 99_250.0);
    }

    #[test]
    fn threshold_includes_exactly_ninety_percent() {
        let metrics =
            guard_metrics(100_000.0, 90_000.0, "BTCUSDT", &GuardParams::default()).unwrap();
        assert!(metrics.usage_ratio >= ALERT_THRESHOLD_RATIO);
    }

    #[test]
    fn parses_only_assignment_values_from_env() {
        let values = parse_env_values(
            r#"
            export API_KEY="abc"
            API_SECRET='def'
            COMMENTED=value # note
            INVALID-KEY=nope
            "#,
        );
        assert_eq!(values.get("API_KEY").map(String::as_str), Some("abc"));
        assert_eq!(values.get("API_SECRET").map(String::as_str), Some("def"));
        assert_eq!(values.get("COMMENTED").map(String::as_str), Some("value"));
        assert!(!values.contains_key("INVALID-KEY"));
    }
}
