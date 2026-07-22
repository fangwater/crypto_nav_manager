use super::{
    ExchangeError,
    common::{Params, get_json, header_value, now_ms, now_sec, query_string, root_array},
    fee_rates::normalize_gate,
};
use crate::{
    models::{TimeRange, TradingFeeRate},
    rest_dispatcher::Dispatcher,
};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName};
use serde_json::Value;
use sha2::{Digest, Sha512};
use std::collections::HashSet;

const EXCHANGE: &str = "gate";
const BASE: &str = "https://api.gateio.ws";
const API_PREFIX: &str = "/api/v4";
const THIRTY_DAYS_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

type HmacSha512 = Hmac<Sha512>;

#[derive(Clone)]
pub struct GateCredentials {
    api_key: String,
    secret_key: String,
}

impl GateCredentials {
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret_key: secret_key.into(),
        }
    }
}

impl std::fmt::Debug for GateCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GateCredentials")
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateBookType {
    Funding,
    TradingFee,
    RealizedPnl,
    DepositWithdrawal,
}

impl GateBookType {
    fn value(self) -> &'static str {
        match self {
            Self::Funding => "fund",
            Self::TradingFee => "fee",
            Self::RealizedPnl => "pnl",
            Self::DepositWithdrawal => "dnw",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateFeeMarket {
    Spot,
    UsdtFutures,
}

impl GateFeeMarket {
    fn storage_value(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::UsdtFutures => "usdt_futures",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GateClient {
    dispatcher: Dispatcher,
    credentials: GateCredentials,
}

impl GateClient {
    pub fn new(dispatcher: Dispatcher, credentials: GateCredentials) -> Self {
        Self {
            dispatcher,
            credentials,
        }
    }

    pub async fn account_snapshot(&self) -> Result<Value, ExchangeError> {
        self.private_get("/unified/accounts", Vec::new(), 1).await
    }

    pub async fn unified_mode(&self) -> Result<Value, ExchangeError> {
        self.private_get("/unified/unified_mode", Vec::new(), 1)
            .await
    }

    pub async fn futures_account(&self) -> Result<Value, ExchangeError> {
        self.private_get("/futures/usdt/accounts", Vec::new(), 1)
            .await
    }

    pub async fn futures_positions(&self) -> Result<Vec<Value>, ExchangeError> {
        root_array(
            EXCHANGE,
            self.private_get("/futures/usdt/positions", Vec::new(), 1)
                .await?,
        )
    }

    pub async fn spot_trades(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut page = 1_u32;
            loop {
                let params = vec![
                    ("account".to_string(), "unified".to_string()),
                    ("from".to_string(), (chunk.start_ms / 1_000).to_string()),
                    ("to".to_string(), (chunk.end_ms / 1_000).to_string()),
                    ("limit".to_string(), "100".to_string()),
                    ("page".to_string(), page.to_string()),
                ];
                let batch = root_array(
                    EXCHANGE,
                    self.private_get("/spot/my_trades", params, 1).await?,
                )?;
                let batch_len = batch.len();
                extend_in_range(&mut rows, batch, range);
                if batch_len < 100 {
                    break;
                }
                page = page.saturating_add(1);
            }
        }
        dedup(&mut rows, &["id", "order_id", "currency_pair"]);
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn futures_trades(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut offset = 0_u32;
            loop {
                let params = vec![
                    ("from".to_string(), (chunk.start_ms / 1_000).to_string()),
                    ("to".to_string(), (chunk.end_ms / 1_000).to_string()),
                    ("limit".to_string(), "1000".to_string()),
                    ("offset".to_string(), offset.to_string()),
                ];
                let batch = root_array(
                    EXCHANGE,
                    self.private_get("/futures/usdt/my_trades_timerange", params, 1)
                        .await?,
                )?;
                let batch_len = batch.len();
                extend_in_range(&mut rows, batch, range);
                if batch_len < 1_000 {
                    break;
                }
                offset = offset.saturating_add(1_000);
            }
        }
        dedup(&mut rows, &["trade_id", "order_id", "contract"]);
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn futures_account_book(
        &self,
        range: TimeRange,
        book_type: GateBookType,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut offset = 0_u32;
            loop {
                let params = vec![
                    ("from".to_string(), (chunk.start_ms / 1_000).to_string()),
                    ("to".to_string(), (chunk.end_ms / 1_000).to_string()),
                    ("type".to_string(), book_type.value().to_string()),
                    ("limit".to_string(), "1000".to_string()),
                    ("offset".to_string(), offset.to_string()),
                ];
                let batch = root_array(
                    EXCHANGE,
                    self.private_get("/futures/usdt/account_book", params, 1)
                        .await?,
                )?;
                let batch_len = batch.len();
                extend_in_range(&mut rows, batch, range);
                if batch_len < 1_000 {
                    break;
                }
                offset = offset.saturating_add(1_000);
            }
        }
        dedup(&mut rows, &["id", "contract", "time", "change"]);
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn funding_fees(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        self.futures_account_book(range, GateBookType::Funding)
            .await
    }

    /// Queries this account's USDT futures liquidation history.
    pub async fn liquidation_history(
        &self,
        contract: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();

        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut offset = 0_usize;
            loop {
                let mut params = vec![
                    ("from".to_string(), (chunk.start_ms / 1_000).to_string()),
                    ("to".to_string(), (chunk.end_ms / 1_000).to_string()),
                    ("limit".to_string(), "100".to_string()),
                    ("offset".to_string(), offset.to_string()),
                ];
                if let Some(contract) = contract {
                    params.push(("contract".to_string(), contract.to_string()));
                }
                let batch = root_array(
                    EXCHANGE,
                    self.private_get("/futures/usdt/liquidates", params, 1)
                        .await?,
                )?;
                let batch_len = batch.len();
                extend_in_range(&mut rows, batch, range);
                if batch_len < 100 {
                    break;
                }
                offset = offset.saturating_add(100);
            }
        }

        dedup(&mut rows, &["order_id", "contract", "time"]);
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn raw_fee_rates(
        &self,
        market: GateFeeMarket,
        instrument: &str,
    ) -> Result<Value, ExchangeError> {
        let (path, parameter) = match market {
            GateFeeMarket::Spot => ("/wallet/fee", "currency_pair"),
            GateFeeMarket::UsdtFutures => ("/futures/usdt/fee", "contract"),
        };
        self.private_get(
            path,
            vec![(parameter.to_string(), instrument.to_string())],
            1,
        )
        .await
    }

    pub async fn fee_rates(
        &self,
        market: GateFeeMarket,
        instrument: &str,
    ) -> Result<Vec<TradingFeeRate>, ExchangeError> {
        let raw = self.raw_fee_rates(market, instrument).await?;
        normalize_gate(raw, market.storage_value(), instrument, now_ms())
    }

    pub async fn interest_records(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut page = 1_u32;
            loop {
                let params = vec![
                    ("from".to_string(), (chunk.start_ms / 1_000).to_string()),
                    ("to".to_string(), (chunk.end_ms / 1_000).to_string()),
                    ("type".to_string(), "margin".to_string()),
                    ("limit".to_string(), "100".to_string()),
                    ("page".to_string(), page.to_string()),
                ];
                let batch = root_array(
                    EXCHANGE,
                    self.private_get("/unified/interest_records", params, 1)
                        .await?,
                )?;
                let batch_len = batch.len();
                extend_in_range(&mut rows, batch, range);
                if batch_len < 100 {
                    break;
                }
                page = page.saturating_add(1);
            }
        }
        dedup(&mut rows, &["id", "currency", "time", "interest"]);
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn spot_account_book(
        &self,
        currency: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut page = 1_u32;
            loop {
                let params = vec![
                    ("currency".to_string(), currency.to_string()),
                    ("from".to_string(), (chunk.start_ms / 1_000).to_string()),
                    ("to".to_string(), (chunk.end_ms / 1_000).to_string()),
                    ("limit".to_string(), "1000".to_string()),
                    ("page".to_string(), page.to_string()),
                ];
                let batch = root_array(
                    EXCHANGE,
                    self.private_get("/spot/account_book", params, 1).await?,
                )?;
                let batch_len = batch.len();
                extend_in_range(&mut rows, batch, range);
                if batch_len < 1_000 {
                    break;
                }
                page = page.saturating_add(1);
            }
        }
        dedup(&mut rows, &["id", "currency", "time", "change"]);
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn current_loans(&self, currency: Option<&str>) -> Result<Value, ExchangeError> {
        let mut params = Vec::new();
        if let Some(currency) = currency {
            params.push(("currency".to_string(), currency.to_string()));
        }
        self.private_get("/unified/loans", params, 1).await
    }

    pub async fn estimated_loan_rates(
        &self,
        currencies: &[String],
    ) -> Result<Value, ExchangeError> {
        if currencies.is_empty() {
            return Err(ExchangeError::InvalidQuery(
                "at least one currency is required".to_string(),
            ));
        }
        self.private_get(
            "/unified/estimate_rate",
            vec![("currencies".to_string(), currencies.join(","))],
            1,
        )
        .await
    }

    pub async fn funding_rate_history(
        &self,
        contract: &str,
        range: Option<TimeRange>,
        limit: u32,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = range
            .map(|range| TimeRange::new(range.start_ms, range.end_ms))
            .transpose()?;
        let mut params = vec![
            ("contract".to_string(), contract.to_string()),
            ("limit".to_string(), limit.clamp(1, 1_000).to_string()),
        ];
        if let Some(range) = range {
            params.push(("from".to_string(), (range.start_ms / 1_000).to_string()));
            params.push(("to".to_string(), (range.end_ms / 1_000).to_string()));
        }
        let mut rows = root_array(
            EXCHANGE,
            self.public_get("/futures/usdt/funding_rate", params, 1)
                .await?,
        )?;
        if let Some(range) = range {
            rows.retain(|row| {
                timestamp_ms(row).is_some_and(|timestamp| in_range(range, timestamp))
            });
        }
        sort_by_timestamp(&mut rows);
        Ok(rows)
    }

    pub async fn futures_contracts(&self) -> Result<Vec<Value>, ExchangeError> {
        root_array(
            EXCHANGE,
            self.public_get("/futures/usdt/contracts", Vec::new(), 1)
                .await?,
        )
    }

    pub async fn spot_tickers(&self) -> Result<Vec<Value>, ExchangeError> {
        root_array(
            EXCHANGE,
            self.public_get("/spot/tickers", Vec::new(), 1).await?,
        )
    }

    pub async fn futures_tickers(&self) -> Result<Vec<Value>, ExchangeError> {
        root_array(
            EXCHANGE,
            self.public_get("/futures/usdt/tickers", Vec::new(), 1)
                .await?,
        )
    }

    async fn private_get(
        &self,
        path: &str,
        params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        let query = query_string(&params);
        let full_path = format!("{API_PREFIX}{path}");
        let timestamp = now_sec();
        let signature = sign_request(
            &self.credentials.secret_key,
            "GET",
            &full_path,
            &query,
            "",
            timestamp,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("key"),
            header_value("KEY", &self.credentials.api_key)?,
        );
        headers.insert(
            HeaderName::from_static("timestamp"),
            header_value("Timestamp", &timestamp.to_string())?,
        );
        headers.insert(
            HeaderName::from_static("sign"),
            header_value("SIGN", &signature)?,
        );
        if path.starts_with("/futures/") {
            headers.insert(
                HeaderName::from_static("x-gate-size-decimal"),
                header_value("X-Gate-Size-Decimal", "1")?,
            );
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        let value = get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{BASE}{full_path}{suffix}"),
            headers,
            weight,
        )
        .await?;
        check_api_error(value)
    }

    async fn public_get(
        &self,
        path: &str,
        params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        let query = query_string(&params);
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{BASE}{API_PREFIX}{path}{suffix}"),
            HeaderMap::new(),
            weight,
        )
        .await
    }
}

fn sign_request(
    secret: &str,
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    timestamp: i64,
) -> String {
    let body_hash = hex::encode(Sha512::digest(body.as_bytes()));
    let payload = format!(
        "{}\n{}\n{}\n{}\n{}",
        method.to_uppercase(),
        path,
        query,
        body_hash,
        timestamp
    );
    let mut mac = HmacSha512::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn check_api_error(value: Value) -> Result<Value, ExchangeError> {
    if let Some(label) = value.get("label").and_then(Value::as_str) {
        return Err(ExchangeError::Api {
            exchange: EXCHANGE,
            code: label.to_string(),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown Gate API error")
                .to_string(),
        });
    }
    Ok(value)
}

fn dedup(rows: &mut Vec<Value>, keys: &[&str]) {
    let mut seen = HashSet::new();
    rows.retain(|row| {
        let key = keys
            .iter()
            .map(|key| row.get(*key).map(Value::to_string).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("|");
        seen.insert(key)
    });
}

fn extend_in_range(rows: &mut Vec<Value>, batch: Vec<Value>, range: TimeRange) {
    rows.extend(
        batch
            .into_iter()
            .filter(|row| timestamp_ms(row).is_some_and(|timestamp| in_range(range, timestamp))),
    );
}

fn in_range(range: TimeRange, timestamp_ms: i64) -> bool {
    range.start_ms <= timestamp_ms && timestamp_ms <= range.end_ms
}

fn timestamp_ms(row: &Value) -> Option<i64> {
    [
        "create_time_ms",
        "time_ms",
        "transaction_time_ms",
        "transaction_time",
        "create_time",
        "time",
        "timestamp",
        "t",
    ]
    .into_iter()
    .find_map(|key| row.get(key).and_then(timestamp_value_ms))
}

fn timestamp_value_ms(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(number) => number.parse::<f64>().ok(),
        _ => None,
    }?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }

    let milliseconds = if number < 100_000_000_000.0 {
        number * 1_000.0
    } else {
        number
    };
    (milliseconds <= i64::MAX as f64).then(|| milliseconds.round() as i64)
}

fn sort_by_timestamp(rows: &mut [Value]) {
    rows.sort_by_key(|row| timestamp_ms(row).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_signature_is_stable() {
        assert_eq!(
            sign_request("secret", "GET", "/api/v4/test", "a=1", "", 1_700_000_000),
            "0894400e042cb393ddd64de852b3d03b317c0067fac11c1605fd459c3c48c0f5d0de84f622edd612f63e96cc12948466a2add34995081bf56d9e27bf8cfb8ef0"
        );
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = GateCredentials::new("actual-public-value", "actual-secret-value");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("actual-public-value"));
        assert!(!debug.contains("actual-secret-value"));
    }

    #[test]
    fn timestamp_supports_seconds_milliseconds_and_fractional_seconds() {
        assert_eq!(
            timestamp_ms(&serde_json::json!({"time": 1_700_000_000})),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            timestamp_ms(&serde_json::json!({"time_ms": 1_700_000_000_123_i64})),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            timestamp_ms(&serde_json::json!({"create_time_ms": "1700000000.123"})),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            timestamp_ms(&serde_json::json!({"t": "1700000000"})),
            Some(1_700_000_000_000)
        );
    }

    #[test]
    fn exact_millisecond_range_filter_drops_boundary_overflow() {
        let mut rows = Vec::new();
        extend_in_range(
            &mut rows,
            vec![
                serde_json::json!({"time_ms": 1_700_000_000_122_i64}),
                serde_json::json!({"time_ms": 1_700_000_000_123_i64}),
                serde_json::json!({"time_ms": 1_700_000_000_124_i64}),
                serde_json::json!({"time": "invalid"}),
            ],
            TimeRange {
                start_ms: 1_700_000_000_123,
                end_ms: 1_700_000_000_123,
            },
        );
        assert_eq!(rows.len(), 1);
    }
}
