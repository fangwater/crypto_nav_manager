use super::{
    ExchangeError,
    common::{Params, get_json, header_value, now_ms, query_string},
    fee_rates::normalize_bitget,
};
use crate::{
    models::{ProductCategory, TimeRange, TradingFeeRate},
    rest_dispatcher::{DispatchError, Dispatcher},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName};
use serde_json::Value;
use sha2::Sha256;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, sleep_until},
};

const EXCHANGE: &str = "bitget";
const BASE: &str = "https://api.bitget.com";
const THIRTY_DAYS_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const PAGE_LIMIT: usize = 100;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct BitgetCredentials {
    api_key: String,
    secret_key: String,
    passphrase: String,
}

impl BitgetCredentials {
    pub fn new(
        api_key: impl Into<String>,
        secret_key: impl Into<String>,
        passphrase: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            secret_key: secret_key.into(),
            passphrase: passphrase.into(),
        }
    }
}

impl std::fmt::Debug for BitgetCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BitgetCredentials")
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("passphrase", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct BitgetClient {
    dispatcher: Dispatcher,
    credentials: BitgetCredentials,
    history_policy: Option<HistoryRequestPolicy>,
}

#[derive(Clone, Debug)]
struct HistoryRequestPolicy {
    min_interval: Duration,
    max_429_retries: usize,
    next_request_at: Arc<Mutex<Instant>>,
}

impl HistoryRequestPolicy {
    async fn wait_turn(&self) {
        let mut next_request_at = self.next_request_at.lock().await;
        let now = Instant::now();
        if *next_request_at > now {
            sleep_until(*next_request_at).await;
        }
        *next_request_at = Instant::now() + self.min_interval;
    }
}

impl BitgetClient {
    pub fn new(dispatcher: Dispatcher, credentials: BitgetCredentials) -> Self {
        Self {
            dispatcher,
            credentials,
            history_policy: None,
        }
    }

    /// Applies pacing and 429 retries only to paginated private history APIs.
    pub fn with_history_request_policy(
        mut self,
        min_interval: Duration,
        max_429_retries: usize,
    ) -> Self {
        self.history_policy = Some(HistoryRequestPolicy {
            min_interval,
            max_429_retries,
            next_request_at: Arc::new(Mutex::new(Instant::now())),
        });
        self
    }

    pub async fn account_info(&self) -> Result<Value, ExchangeError> {
        self.private_get("/api/v3/account/info", Vec::new(), 1)
            .await
    }

    pub async fn account_settings(&self) -> Result<Value, ExchangeError> {
        self.private_get("/api/v3/account/settings", Vec::new(), 1)
            .await
    }

    pub async fn account_assets(&self) -> Result<Value, ExchangeError> {
        self.private_get("/api/v3/account/assets", Vec::new(), 1)
            .await
    }

    pub async fn loan_data(&self) -> Result<Value, ExchangeError> {
        self.private_get("/api/v3/trade/loan-data", Vec::new(), 1)
            .await
    }

    pub async fn fills(
        &self,
        category: ProductCategory,
        symbol: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut cursor: Option<String> = None;
            loop {
                let mut params = vec![
                    ("category".to_string(), category.bitget_value().to_string()),
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("limit".to_string(), PAGE_LIMIT.to_string()),
                ];
                if let Some(symbol) = symbol {
                    params.push(("symbol".to_string(), symbol.to_string()));
                }
                if let Some(cursor) = &cursor {
                    params.push(("cursor".to_string(), cursor.clone()));
                }
                let value = self.history_get("/api/v3/trade/fills", params).await?;
                let body = value
                    .get("data")
                    .and_then(Value::as_object)
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "fills response is missing data object".to_string(),
                    })?;
                let page = body
                    .get("list")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let next_cursor = body
                    .get("cursor")
                    .or_else(|| body.get("nextCursor"))
                    .or_else(|| body.get("endId"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let page_len = page.len();
                rows.extend(page);
                if page_len < PAGE_LIMIT || next_cursor.is_none() || next_cursor == cursor {
                    break;
                }
                cursor = next_cursor;
            }
        }
        dedup(&mut rows, &["execId", "orderId", "symbol"]);
        sort_by_ms(&mut rows, &["createdTime", "cTime", "ts"]);
        Ok(rows)
    }

    /// Queries this UTA account's liquidation orders from private order history.
    pub async fn liquidation_orders(
        &self,
        category: ProductCategory,
        symbol: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();

        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut cursor: Option<String> = None;
            loop {
                let mut params = vec![
                    ("category".to_string(), category.bitget_value().to_string()),
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("limit".to_string(), PAGE_LIMIT.to_string()),
                ];
                if let Some(symbol) = symbol {
                    params.push(("symbol".to_string(), symbol.to_string()));
                }
                if let Some(cursor) = &cursor {
                    params.push(("cursor".to_string(), cursor.clone()));
                }

                let value = self
                    .history_get("/api/v3/trade/history-orders", params)
                    .await?;
                let body = value
                    .get("data")
                    .and_then(Value::as_object)
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "history-orders response is missing data object".to_string(),
                    })?;
                let page = body
                    .get("list")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let next_cursor = body
                    .get("cursor")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let page_len = page.len();
                rows.extend(page.into_iter().filter(|row| {
                    is_liquidation_order(row)
                        && row_time_ms(row)
                            .is_some_and(|ts| range.start_ms <= ts && ts <= range.end_ms)
                }));

                if page_len < PAGE_LIMIT || next_cursor.is_none() || next_cursor == cursor {
                    break;
                }
                cursor = next_cursor;
            }
        }

        dedup(&mut rows, &["orderId", "symbol", "updatedTime"]);
        sort_by_ms(&mut rows, &["updatedTime", "createdTime"]);
        Ok(rows)
    }

    pub async fn financial_records(
        &self,
        category: ProductCategory,
        record_type: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut cursor: Option<String> = None;
            loop {
                let mut params = vec![
                    ("category".to_string(), category.bitget_value().to_string()),
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("limit".to_string(), PAGE_LIMIT.to_string()),
                ];
                if let Some(record_type) = record_type {
                    params.push(("type".to_string(), record_type.to_string()));
                }
                if let Some(cursor) = &cursor {
                    params.push(("cursor".to_string(), cursor.clone()));
                }
                let value = self
                    .history_get("/api/v3/account/financial-records", params)
                    .await?;
                let body = value
                    .get("data")
                    .and_then(Value::as_object)
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "financial-records response is missing data object".to_string(),
                    })?;
                let page = body
                    .get("list")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let next_cursor = body
                    .get("cursor")
                    .or_else(|| body.get("nextCursor"))
                    .or_else(|| body.get("endId"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let page_len = page.len();
                rows.extend(page);
                if page_len < PAGE_LIMIT || next_cursor.is_none() || next_cursor == cursor {
                    break;
                }
                cursor = next_cursor;
            }
        }
        dedup(&mut rows, &["id", "bizId", "type", "createdTime"]);
        sort_by_ms(&mut rows, &["createdTime", "cTime", "ts"]);
        Ok(rows)
    }

    pub async fn funding_fees(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        for record_type in [
            "CONTRACT_MAIN_SETTLE_FEE_USER_IN",
            "CONTRACT_MAIN_SETTLE_FEE_USER_OUT",
        ] {
            rows.extend(
                self.financial_records(ProductCategory::UsdtFutures, Some(record_type), range)
                    .await?,
            );
        }
        dedup(&mut rows, &["id", "bizId", "type", "createdTime"]);
        sort_by_ms(&mut rows, &["createdTime", "cTime", "ts"]);
        Ok(rows)
    }

    pub async fn raw_fee_rates(
        &self,
        category: ProductCategory,
        symbol: &str,
    ) -> Result<Vec<Value>, ExchangeError> {
        let params = vec![
            ("category".to_string(), category.bitget_value().to_string()),
            ("symbol".to_string(), symbol.to_string()),
        ];
        let value = self
            .history_get("/api/v3/account/all-fee-rate", params)
            .await?;
        value
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| ExchangeError::InvalidResponse {
                exchange: EXCHANGE,
                message: "all-fee-rate response is missing data array".to_string(),
            })
    }

    pub async fn fee_rates(
        &self,
        category: ProductCategory,
        symbol: &str,
    ) -> Result<Vec<TradingFeeRate>, ExchangeError> {
        let rows = self.raw_fee_rates(category, symbol).await?;
        normalize_bitget(rows, category.storage_value(), now_ms())
    }

    pub async fn margin_interest(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        self.financial_records(
            ProductCategory::Margin,
            Some("INTEREST_SETTLEMENT_OUT"),
            range,
        )
        .await
    }

    pub async fn tickers(
        &self,
        category: ProductCategory,
        symbol: Option<&str>,
    ) -> Result<Value, ExchangeError> {
        let mut params = vec![("category".to_string(), category.bitget_value().to_string())];
        if let Some(symbol) = symbol {
            params.push(("symbol".to_string(), symbol.to_string()));
        }
        self.public_get("/api/v3/market/tickers", params, 1).await
    }

    pub async fn margin_loan_rate(&self, coin: &str) -> Result<Value, ExchangeError> {
        self.public_get(
            "/api/v3/market/margin-loans",
            vec![("coin".to_string(), coin.to_string())],
            1,
        )
        .await
    }

    pub async fn discount_rates(&self) -> Result<Value, ExchangeError> {
        self.public_get("/api/v3/market/discount-rate", Vec::new(), 1)
            .await
    }

    async fn history_get(&self, path: &str, params: Params) -> Result<Value, ExchangeError> {
        let Some(policy) = &self.history_policy else {
            return self.private_get(path, params, 1).await;
        };

        for retry in 0..=policy.max_429_retries {
            policy.wait_turn().await;
            match self.private_get(path, params.clone(), 1).await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let Some(delay) = history_retry_delay(&error) else {
                        return Err(error);
                    };
                    if retry == policy.max_429_retries {
                        return Err(error);
                    }
                    eprintln!(
                        "Bitget history rate limited; retry {}/{} after {:.1}s",
                        retry + 1,
                        policy.max_429_retries,
                        delay.as_secs_f64()
                    );
                    sleep(delay).await;
                }
            }
        }

        unreachable!("history retry loop always returns")
    }

    async fn private_get(
        &self,
        path: &str,
        params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        let query = query_string(&params);
        let path_with_query = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        };
        let timestamp = now_ms();
        let signature = sign_request(
            timestamp,
            "GET",
            &path_with_query,
            "",
            &self.credentials.secret_key,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("access-key"),
            header_value("ACCESS-KEY", &self.credentials.api_key)?,
        );
        headers.insert(
            HeaderName::from_static("access-sign"),
            header_value("ACCESS-SIGN", &signature)?,
        );
        headers.insert(
            HeaderName::from_static("access-timestamp"),
            header_value("ACCESS-TIMESTAMP", &timestamp.to_string())?,
        );
        headers.insert(
            HeaderName::from_static("access-passphrase"),
            header_value("ACCESS-PASSPHRASE", &self.credentials.passphrase)?,
        );
        headers.insert(
            CONTENT_TYPE,
            header_value("Content-Type", "application/json")?,
        );
        let value = get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{BASE}{path_with_query}"),
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
        let value = get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{BASE}{path}{suffix}"),
            HeaderMap::new(),
            weight,
        )
        .await?;
        check_api_error(value)
    }
}

fn sign_request(timestamp_ms: i64, method: &str, path: &str, body: &str, secret: &str) -> String {
    let payload = format!("{}{method}{path}{body}", timestamp_ms);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(payload.as_bytes());
    BASE64.encode(mac.finalize().into_bytes())
}

fn history_retry_delay(error: &ExchangeError) -> Option<Duration> {
    match error {
        ExchangeError::Dispatch(DispatchError::RateLimited { retry_after, .. }) => {
            Some(*retry_after)
        }
        ExchangeError::Dispatch(DispatchError::NoAvailableIp {
            retry_after: Some(retry_after),
        }) => Some(*retry_after),
        ExchangeError::Api { code, .. } if code == "429" => Some(Duration::from_secs(60)),
        _ => None,
    }
}

fn check_api_error(value: Value) -> Result<Value, ExchangeError> {
    let code = value
        .get("code")
        .map(|code| {
            code.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| code.to_string())
        })
        .unwrap_or_else(|| "00000".to_string());
    if matches!(code.as_str(), "00000" | "0") {
        return Ok(value);
    }
    Err(ExchangeError::Api {
        exchange: EXCHANGE,
        code,
        message: value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown Bitget API error")
            .to_string(),
    })
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

fn is_liquidation_order(row: &Value) -> bool {
    if row.get("execType").and_then(Value::as_str) == Some("liquidation") {
        return true;
    }

    row.get("delegateType")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "liquidation" || value.starts_with("liquidation_take_over_"))
}

fn row_time_ms(row: &Value) -> Option<i64> {
    ["updatedTime", "createdTime"].into_iter().find_map(|key| {
        row.get(key)
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
    })
}

fn sort_by_ms(rows: &mut [Value], keys: &[&str]) {
    rows.sort_by_key(|row| {
        keys.iter()
            .find_map(|key| {
                row.get(*key)
                    .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
            })
            .unwrap_or_default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable() {
        assert_eq!(
            sign_request(
                0,
                "",
                "The quick brown fox jumps over the lazy dog",
                "",
                "key"
            ),
            "66Uzf6kK540OgwJU+t78+t+p5e37zqQ0AH6/fPHuxjg="
        );
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = BitgetCredentials::new(
            "actual-public-value",
            "actual-secret-value",
            "actual-passphrase-value",
        );
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("actual-public-value"));
        assert!(!debug.contains("actual-secret-value"));
        assert!(!debug.contains("actual-passphrase-value"));
    }

    #[test]
    fn data_array_helper_accepts_bitget_shape() {
        let value = serde_json::json!({"data": [1, 2]});
        assert_eq!(
            crate::exchange::common::data_array(EXCHANGE, &value)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn identifies_only_private_liquidation_orders() {
        assert!(is_liquidation_order(
            &serde_json::json!({"execType": "liquidation"})
        ));
        assert!(is_liquidation_order(
            &serde_json::json!({"delegateType": "liquidation_take_over_long"})
        ));
        assert!(!is_liquidation_order(
            &serde_json::json!({"execType": "normal", "delegateType": "market"})
        ));
    }
    #[test]
    fn history_request_policy_is_opt_in() {
        let dispatcher = Dispatcher::new(crate::rest_dispatcher::DispatcherConfig {
            local_ips: vec!["127.0.0.1".parse().unwrap()],
            ..crate::rest_dispatcher::DispatcherConfig::default()
        })
        .unwrap();
        let client = BitgetClient::new(
            dispatcher,
            BitgetCredentials::new("key", "secret", "passphrase"),
        );
        assert!(client.history_policy.is_none());

        let client = client.with_history_request_policy(Duration::from_millis(250), 3);
        let policy = client.history_policy.unwrap();
        assert_eq!(policy.min_interval, Duration::from_millis(250));
        assert_eq!(policy.max_429_retries, 3);
    }

    #[test]
    fn retries_api_level_history_rate_limits() {
        let error = ExchangeError::Api {
            exchange: EXCHANGE,
            code: "429".to_string(),
            message: "too many requests".to_string(),
        };
        assert_eq!(history_retry_delay(&error), Some(Duration::from_secs(60)));
    }
}
