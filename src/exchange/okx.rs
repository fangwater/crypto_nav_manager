use super::{
    ExchangeError,
    common::{Params, data_array, get_json, header_value, now_ms, query_string},
    fee_rates::normalize_okx,
};
use crate::{
    models::{TimeRange, TradingFeeRate},
    rest_dispatcher::Dispatcher,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName};
use serde_json::Value;
use sha2::Sha256;
use std::collections::HashSet;

const EXCHANGE: &str = "okx";
const BASE: &str = "https://www.okx.com";
const PAGE_LIMIT: usize = 100;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct OkxCredentials {
    api_key: String,
    secret_key: String,
    passphrase: String,
}

impl OkxCredentials {
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

impl std::fmt::Debug for OkxCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OkxCredentials")
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("passphrase", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OkxInstrumentType {
    Spot,
    Margin,
    Swap,
    Futures,
}

impl OkxInstrumentType {
    fn value(self) -> &'static str {
        match self {
            Self::Spot => "SPOT",
            Self::Margin => "MARGIN",
            Self::Swap => "SWAP",
            Self::Futures => "FUTURES",
        }
    }

    fn storage_value(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Margin => "margin",
            Self::Swap => "swap",
            Self::Futures => "futures",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OkxClient {
    dispatcher: Dispatcher,
    credentials: OkxCredentials,
}

impl OkxClient {
    pub fn new(dispatcher: Dispatcher, credentials: OkxCredentials) -> Self {
        Self {
            dispatcher,
            credentials,
        }
    }

    pub async fn account_balance(&self, currencies: &[String]) -> Result<Value, ExchangeError> {
        let params = if currencies.is_empty() {
            Vec::new()
        } else {
            vec![("ccy".to_string(), currencies.join(","))]
        };
        self.private_get("/api/v5/account/balance", params, 1).await
    }

    pub async fn account_config(&self) -> Result<Value, ExchangeError> {
        self.private_get("/api/v5/account/config", Vec::new(), 1)
            .await
    }

    pub async fn fills(
        &self,
        instrument_type: OkxInstrumentType,
        instrument_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut params = vec![
                ("instType".to_string(), instrument_type.value().to_string()),
                ("begin".to_string(), range.start_ms.to_string()),
                ("end".to_string(), range.end_ms.to_string()),
                ("limit".to_string(), PAGE_LIMIT.to_string()),
            ];
            if let Some(instrument_id) = instrument_id {
                params.push(("instId".to_string(), instrument_id.to_string()));
            }
            if let Some(after) = &after {
                params.push(("after".to_string(), after.clone()));
            }
            let value = self
                .private_get("/api/v5/trade/fills-history", params, 1)
                .await?;
            let page = data_array(EXCHANGE, &value)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let last = page.last().cloned().unwrap_or(Value::Null);
            let last_ts = value_i64(&last, "ts").unwrap_or_default();
            rows.extend(page.into_iter().filter(|row| {
                value_i64(row, "ts").is_some_and(|ts| range.start_ms <= ts && ts <= range.end_ms)
            }));
            if page_len < PAGE_LIMIT || last_ts < range.start_ms {
                break;
            }
            let next = last
                .get("billId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if next.is_none() || next == after {
                break;
            }
            after = next;
        }
        dedup(&mut rows, &["tradeId", "ordId", "instId"]);
        sort_by_ts(&mut rows);
        Ok(rows)
    }

    pub async fn current_bills(
        &self,
        instrument_type: Option<OkxInstrumentType>,
        bill_type: Option<&str>,
        sub_type: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.bills(
            "/api/v5/account/bills",
            instrument_type,
            bill_type,
            sub_type,
            range,
        )
        .await
    }

    pub async fn archived_bills(
        &self,
        instrument_type: Option<OkxInstrumentType>,
        bill_type: Option<&str>,
        sub_type: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.bills(
            "/api/v5/account/bills-archive",
            instrument_type,
            bill_type,
            sub_type,
            range,
        )
        .await
    }

    pub async fn funding_fees(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = self
            .archived_bills(Some(OkxInstrumentType::Swap), Some("8"), None, range)
            .await?;
        rows.extend(
            self.current_bills(Some(OkxInstrumentType::Swap), Some("8"), None, range)
                .await?,
        );
        dedup(&mut rows, &["billId", "instId", "ts"]);
        sort_by_ts(&mut rows);
        Ok(rows)
    }

    pub async fn raw_fee_rates(
        &self,
        instrument_type: OkxInstrumentType,
        instrument: &str,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut params = vec![("instType".to_string(), instrument_type.value().to_string())];
        let (parameter, value) = match instrument_type {
            OkxInstrumentType::Spot | OkxInstrumentType::Margin => ("instId", instrument),
            OkxInstrumentType::Swap => (
                "instFamily",
                instrument.strip_suffix("-SWAP").unwrap_or(instrument),
            ),
            OkxInstrumentType::Futures => (
                "instFamily",
                instrument
                    .rsplit_once('-')
                    .map_or(instrument, |(family, _)| family),
            ),
        };
        params.push((parameter.to_string(), value.to_string()));
        let value = self
            .private_get("/api/v5/account/trade-fee", params, 1)
            .await?;
        data_array(EXCHANGE, &value)
    }

    pub async fn fee_rates(
        &self,
        instrument_type: OkxInstrumentType,
        instrument: &str,
    ) -> Result<Vec<TradingFeeRate>, ExchangeError> {
        let rows = self.raw_fee_rates(instrument_type, instrument).await?;
        normalize_okx(
            rows,
            instrument_type.storage_value(),
            Some(instrument),
            now_ms(),
        )
    }

    pub async fn interest_accrued(
        &self,
        currency: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        let mut after = Some(range.end_ms.saturating_add(1).to_string());
        loop {
            let mut params = vec![
                ("type".to_string(), "2".to_string()),
                ("limit".to_string(), PAGE_LIMIT.to_string()),
            ];
            if let Some(currency) = currency {
                params.push(("ccy".to_string(), currency.to_string()));
            }
            if let Some(after) = &after {
                params.push(("after".to_string(), after.clone()));
            }
            let value = self
                .private_get("/api/v5/account/interest-accrued", params, 1)
                .await?;
            let page = data_array(EXCHANGE, &value)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let last_ts = page
                .last()
                .and_then(|row| value_i64(row, "ts"))
                .unwrap_or_default();
            rows.extend(page.into_iter().filter(|row| {
                value_i64(row, "ts").is_some_and(|ts| range.start_ms <= ts && ts <= range.end_ms)
            }));
            if page_len < PAGE_LIMIT || last_ts < range.start_ms {
                break;
            }
            let next = Some(last_ts.to_string());
            if next == after {
                break;
            }
            after = next;
        }
        dedup(&mut rows, &["ccy", "ts", "interest"]);
        sort_by_ts(&mut rows);
        Ok(rows)
    }

    pub async fn interest_rates(
        &self,
        currency: Option<&str>,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut params = Vec::new();
        if let Some(currency) = currency {
            params.push(("ccy".to_string(), currency.to_string()));
        }
        let value = self
            .private_get("/api/v5/account/interest-rate", params, 1)
            .await?;
        data_array(EXCHANGE, &value)
    }

    pub async fn interest_limits(
        &self,
        currency: Option<&str>,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut params = vec![("type".to_string(), "2".to_string())];
        if let Some(currency) = currency {
            params.push(("ccy".to_string(), currency.to_string()));
        }
        let value = self
            .private_get("/api/v5/account/interest-limits", params, 1)
            .await?;
        data_array(EXCHANGE, &value)
    }

    pub async fn funding_rate_history(
        &self,
        instrument_id: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        let mut after = Some(range.end_ms.saturating_add(1).to_string());
        loop {
            let mut params = vec![
                ("instId".to_string(), instrument_id.to_string()),
                ("limit".to_string(), PAGE_LIMIT.to_string()),
            ];
            if let Some(after) = &after {
                params.push(("after".to_string(), after.clone()));
            }
            let value = self
                .public_get("/api/v5/public/funding-rate-history", params, 1)
                .await?;
            let page = data_array(EXCHANGE, &value)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let last_ts = page
                .last()
                .and_then(|row| value_i64(row, "fundingTime").or_else(|| value_i64(row, "ts")))
                .unwrap_or_default();
            rows.extend(page);
            if page_len < PAGE_LIMIT || last_ts <= range.start_ms {
                break;
            }
            let next = Some(last_ts.to_string());
            if next == after {
                break;
            }
            after = next;
        }
        dedup(&mut rows, &["instId", "fundingTime"]);
        sort_by_keys(&mut rows, &["fundingTime", "ts"]);
        Ok(rows)
    }

    async fn bills(
        &self,
        endpoint: &str,
        instrument_type: Option<OkxInstrumentType>,
        bill_type: Option<&str>,
        sub_type: Option<&str>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut params = vec![
                ("begin".to_string(), range.start_ms.to_string()),
                ("end".to_string(), range.end_ms.to_string()),
                ("limit".to_string(), PAGE_LIMIT.to_string()),
            ];
            if let Some(instrument_type) = instrument_type {
                params.push(("instType".to_string(), instrument_type.value().to_string()));
            }
            if let Some(bill_type) = bill_type {
                params.push(("type".to_string(), bill_type.to_string()));
            }
            if let Some(sub_type) = sub_type {
                params.push(("subType".to_string(), sub_type.to_string()));
            }
            if let Some(after) = &after {
                params.push(("after".to_string(), after.clone()));
            }
            let value = self.private_get(endpoint, params, 1).await?;
            let page = data_array(EXCHANGE, &value)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let last = page.last().cloned().unwrap_or(Value::Null);
            let last_ts = value_i64(&last, "ts").unwrap_or_default();
            rows.extend(page.into_iter().filter(|row| {
                value_i64(row, "ts").is_some_and(|ts| range.start_ms <= ts && ts <= range.end_ms)
            }));
            if page_len < PAGE_LIMIT || last_ts < range.start_ms {
                break;
            }
            let next = last
                .get("billId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if next.is_none() || next == after {
                break;
            }
            after = next;
        }
        dedup(&mut rows, &["billId", "instId", "ts"]);
        sort_by_ts(&mut rows);
        Ok(rows)
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
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let signature = sign_request(
            &timestamp,
            "GET",
            &path_with_query,
            "",
            &self.credentials.secret_key,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("ok-access-key"),
            header_value("OK-ACCESS-KEY", &self.credentials.api_key)?,
        );
        headers.insert(
            HeaderName::from_static("ok-access-sign"),
            header_value("OK-ACCESS-SIGN", &signature)?,
        );
        headers.insert(
            HeaderName::from_static("ok-access-timestamp"),
            header_value("OK-ACCESS-TIMESTAMP", &timestamp)?,
        );
        headers.insert(
            HeaderName::from_static("ok-access-passphrase"),
            header_value("OK-ACCESS-PASSPHRASE", &self.credentials.passphrase)?,
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

fn sign_request(timestamp: &str, method: &str, path: &str, body: &str, secret: &str) -> String {
    let payload = format!("{timestamp}{}{path}{body}", method.to_uppercase());
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(payload.as_bytes());
    BASE64.encode(mac.finalize().into_bytes())
}

fn check_api_error(value: Value) -> Result<Value, ExchangeError> {
    let code = value
        .get("code")
        .map(|code| {
            code.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| code.to_string())
        })
        .unwrap_or_else(|| "0".to_string());
    if code == "0" {
        return Ok(value);
    }
    Err(ExchangeError::Api {
        exchange: EXCHANGE,
        code,
        message: value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown OKX API error")
            .to_string(),
    })
}

fn value_i64(value: &Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
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

fn sort_by_ts(rows: &mut [Value]) {
    sort_by_keys(rows, &["ts"]);
}

fn sort_by_keys(rows: &mut [Value], keys: &[&str]) {
    rows.sort_by_key(|row| {
        keys.iter()
            .find_map(|key| value_i64(row, key))
            .unwrap_or_default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_standard_sha256_vector() {
        assert_eq!(
            sign_request(
                "",
                "",
                "The quick brown fox jumps over the lazy dog",
                "",
                "key"
            ),
            "97yD9DBThCSxMpjmqm+xQ+9NWaFJRhdZl0edvC0aPNg="
        );
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = OkxCredentials::new(
            "actual-public-value",
            "actual-secret-value",
            "actual-passphrase-value",
        );
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("actual-public-value"));
        assert!(!debug.contains("actual-secret-value"));
        assert!(!debug.contains("actual-passphrase-value"));
    }
}
