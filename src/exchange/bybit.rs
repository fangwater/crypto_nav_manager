use super::{
    ExchangeError,
    common::{Params, get_json, header_value, now_ms, query_string},
};
use crate::{models::TimeRange, rest_dispatcher::Dispatcher};
use hmac::{Hmac, Mac};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName};
use serde_json::Value;
use sha2::Sha256;
use std::collections::HashSet;

const EXCHANGE: &str = "bybit";
const BASE: &str = "https://api.bybit.com";
const SEVEN_DAYS_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const PAGE_LIMIT: usize = 50;
const RECV_WINDOW: &str = "5000";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct BybitCredentials {
    api_key: String,
    secret_key: String,
}

impl BybitCredentials {
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret_key: secret_key.into(),
        }
    }
}

impl std::fmt::Debug for BybitCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BybitCredentials")
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BybitCategory {
    Linear,
    Inverse,
}

impl BybitCategory {
    fn api_value(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Inverse => "inverse",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BybitClient {
    dispatcher: Dispatcher,
    credentials: BybitCredentials,
}

impl BybitClient {
    pub fn new(dispatcher: Dispatcher, credentials: BybitCredentials) -> Self {
        Self {
            dispatcher,
            credentials,
        }
    }

    /// Queries forced-liquidation orders belonging to this UTA account.
    /// ADL orders are deliberately excluded.
    pub async fn liquidation_orders(
        &self,
        category: BybitCategory,
        symbol: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();

        for chunk in range.chunks(SEVEN_DAYS_MS)? {
            let mut cursor: Option<String> = None;
            let mut seen_cursors = HashSet::new();
            loop {
                let mut params = vec![
                    ("category".to_string(), category.api_value().to_string()),
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("limit".to_string(), PAGE_LIMIT.to_string()),
                ];
                if let Some(symbol) = symbol {
                    params.push(("symbol".to_string(), symbol.to_ascii_uppercase()));
                }
                if let Some(cursor) = &cursor {
                    params.push(("cursor".to_string(), cursor.clone()));
                }

                let value = self.private_get("/v5/order/history", params, 1).await?;
                let result = value
                    .get("result")
                    .and_then(Value::as_object)
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "order history response is missing result object".to_string(),
                    })?;
                let page = result
                    .get("list")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "order history response is missing result.list".to_string(),
                    })?;
                let next_cursor = result
                    .get("nextPageCursor")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                rows.extend(page.into_iter().filter(|row| {
                    is_liquidation_order(row)
                        && order_time_ms(row)
                            .is_some_and(|ts| chunk.start_ms <= ts && ts <= chunk.end_ms)
                }));

                let Some(next_cursor) = next_cursor else {
                    break;
                };
                if !seen_cursors.insert(next_cursor.clone()) {
                    break;
                }
                cursor = Some(next_cursor);
            }
        }

        dedup(&mut rows, &["orderId", "symbol", "createType"]);
        rows.sort_by_key(|row| order_time_ms(row).unwrap_or_default());
        Ok(rows)
    }

    async fn private_get(
        &self,
        path: &str,
        params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        let query = query_string(&params);
        let timestamp = now_ms().to_string();
        let signature = sign_request(
            &timestamp,
            &self.credentials.api_key,
            RECV_WINDOW,
            &query,
            &self.credentials.secret_key,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-bapi-api-key"),
            header_value("X-BAPI-API-KEY", &self.credentials.api_key)?,
        );
        headers.insert(
            HeaderName::from_static("x-bapi-timestamp"),
            header_value("X-BAPI-TIMESTAMP", &timestamp)?,
        );
        headers.insert(
            HeaderName::from_static("x-bapi-recv-window"),
            header_value("X-BAPI-RECV-WINDOW", RECV_WINDOW)?,
        );
        headers.insert(
            HeaderName::from_static("x-bapi-sign"),
            header_value("X-BAPI-SIGN", &signature)?,
        );
        headers.insert(
            CONTENT_TYPE,
            header_value("Content-Type", "application/json")?,
        );
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        let value = get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{BASE}{path}{suffix}"),
            headers,
            weight,
        )
        .await?;
        check_api_error(value)
    }
}

fn sign_request(
    timestamp: &str,
    api_key: &str,
    recv_window: &str,
    query: &str,
    secret: &str,
) -> String {
    let payload = format!("{timestamp}{api_key}{recv_window}{query}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn check_api_error(value: Value) -> Result<Value, ExchangeError> {
    let code = value
        .get("retCode")
        .map(|code| {
            code.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| code.to_string())
        })
        .ok_or_else(|| ExchangeError::InvalidResponse {
            exchange: EXCHANGE,
            message: "response is missing retCode".to_string(),
        })?;
    if code == "0" {
        return Ok(value);
    }
    Err(ExchangeError::Api {
        exchange: EXCHANGE,
        code,
        message: value
            .get("retMsg")
            .and_then(Value::as_str)
            .unwrap_or("unknown Bybit API error")
            .to_string(),
    })
}

fn is_liquidation_order(row: &Value) -> bool {
    matches!(
        row.get("createType").and_then(Value::as_str),
        Some("CreateByLiq" | "CreateByTakeOver_PassThrough")
    )
}

fn order_time_ms(row: &Value) -> Option<i64> {
    ["createdTime", "updatedTime"].into_iter().find_map(|key| {
        row.get(key)
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_uses_bybit_v5_payload_order() {
        assert_eq!(
            sign_request("1", "api", "5000", "category=linear", "secret"),
            "3c7d359b41eb3e2593e0768d38b754b743a9e12c6e161dee039a5bcecba6ef1c"
        );
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = BybitCredentials::new("actual-public-value", "actual-secret-value");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("actual-public-value"));
        assert!(!debug.contains("actual-secret-value"));
    }

    #[test]
    fn identifies_liquidation_but_not_adl_orders() {
        assert!(is_liquidation_order(
            &serde_json::json!({"createType": "CreateByLiq"})
        ));
        assert!(is_liquidation_order(
            &serde_json::json!({"createType": "CreateByTakeOver_PassThrough"})
        ));
        assert!(!is_liquidation_order(
            &serde_json::json!({"createType": "CreateByAdl_PassThrough"})
        ));
    }
}
