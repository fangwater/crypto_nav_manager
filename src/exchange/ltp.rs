use super::{
    ExchangeError,
    common::{Params, get_json, header_value, query_string},
};
use crate::{models::TimeRange, rest_dispatcher::Dispatcher};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName};
use serde_json::Value;
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EXCHANGE: &str = "ltp";
const DEFAULT_BASE_URL: &str = "https://api.liquiditytech.com";
const EXECUTIONS_PATH: &str = "/api/v1/trading/executions/pageable";
const ARCHIVED_EXECUTIONS_PATH: &str = "/api/v1/trading/archive/executions/pageable";
const STATEMENT_PATH: &str = "/api/v1/trading/statement";
const PAGE_SIZE: usize = 1_000;
const MAX_PAGES: usize = 100;
const ONE_DAY_MS: i64 = 86_400_000;
const RECENT_SPAN_MS: i64 = 7 * ONE_DAY_MS;
const ARCHIVE_SPAN_MS: i64 = 90 * ONE_DAY_MS;
const EXECUTION_PAGE_INTERVAL: Duration = Duration::from_millis(2_100);
const STATEMENT_PAGE_INTERVAL: Duration = Duration::from_millis(1_500);

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct LtpCredentials {
    api_key: String,
    secret_key: String,
}

impl LtpCredentials {
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret_key: secret_key.into(),
        }
    }
}

impl std::fmt::Debug for LtpCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LtpCredentials")
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct LtpClient {
    dispatcher: Dispatcher,
    credentials: LtpCredentials,
    portfolio_id: String,
    exchange: &'static str,
    base_url: String,
}

impl LtpClient {
    pub fn new(
        dispatcher: Dispatcher,
        credentials: LtpCredentials,
        portfolio_id: impl Into<String>,
        exchange: &'static str,
        base_url: Option<String>,
    ) -> Result<Self, ExchangeError> {
        if !matches!(exchange, "BINANCE" | "OKX") {
            return Err(ExchangeError::InvalidQuery(format!(
                "unsupported RapidX exchange {exchange}"
            )));
        }
        let portfolio_id = portfolio_id.into();
        if portfolio_id.is_empty() {
            return Err(ExchangeError::InvalidQuery(
                "RapidX portfolio id is empty".to_string(),
            ));
        }
        Ok(Self {
            dispatcher,
            credentials,
            portfolio_id,
            exchange,
            base_url: base_url
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
        })
    }

    /// Fetches a complete RapidX execution window, splitting it at the
    /// documented 7-day recent/90-day archive boundary. Rows are deduplicated
    /// by `transactionId` across the boundary; a conflicting duplicate aborts
    /// the fetch rather than silently merging.
    pub async fn execution_history(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let ranges = execution_ranges(range, now_ms())?;
        let mut rows = Vec::new();
        let mut seen: HashMap<String, Value> = HashMap::new();
        for (index, (path, window)) in ranges.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(EXECUTION_PAGE_INTERVAL).await;
            }
            for row in self.execution_window(path, *window).await? {
                let id = required_string(&row, "transactionId")?.to_string();
                if let Some(previous) = seen.get(&id) {
                    if previous != &row {
                        return Err(invalid_response(format!(
                            "conflicting execution {id} across recent/archive boundary"
                        )));
                    }
                } else {
                    seen.insert(id, row.clone());
                    rows.push(row);
                }
            }
        }
        rows.sort_by_key(|row| timestamp_ms(row, "createAt").unwrap_or_default());
        Ok(rows)
    }

    async fn execution_window(
        &self,
        path: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        let mut ids = HashSet::new();
        let mut expected_pages = None;
        let mut expected_total = None;
        for page in 1..=MAX_PAGES {
            if page > 1 {
                tokio::time::sleep(EXECUTION_PAGE_INTERVAL).await;
            }
            let params = BTreeMap::from([
                ("exchange".to_string(), self.exchange.to_string()),
                ("begin".to_string(), range.start_ms.to_string()),
                ("end".to_string(), range.end_ms.to_string()),
                ("page".to_string(), page.to_string()),
                ("pageSize".to_string(), PAGE_SIZE.to_string()),
            ]);
            let value = self.signed_get(path, &params).await?;
            let parsed = validate_execution_page(
                &value,
                page,
                self.exchange,
                &self.portfolio_id,
                range.start_ms,
                range.end_ms,
                expected_pages,
                expected_total,
                &mut ids,
            )?;
            expected_pages = Some(parsed.pages);
            expected_total = Some(parsed.total);
            rows.extend(parsed.rows);
            if page >= parsed.pages {
                if rows.len() != parsed.total {
                    return Err(invalid_response("execution pagination totalSize mismatch"));
                }
                return Ok(rows);
            }
        }
        Err(invalid_response(format!(
            "execution history exceeds the {MAX_PAGES} page limit"
        )))
    }

    /// Fetches account statement rows of one `statementType` (for example
    /// `FUNDING_FEE` or `DEDUCT_INTEREST`) inside the documented 90-day
    /// retention window. Rows are deduplicated by `statementId`.
    pub async fn statement_history(
        &self,
        range: TimeRange,
        statement_type: &str,
    ) -> Result<Vec<Value>, ExchangeError> {
        let now = now_ms();
        if range.start_ms <= 0
            || range.end_ms > now
            || range.start_ms < now.saturating_sub(ARCHIVE_SPAN_MS)
        {
            return Err(ExchangeError::InvalidQuery(
                "statement history is outside the documented 90-day retention window".to_string(),
            ));
        }
        let mut rows = Vec::new();
        let mut ids = HashSet::new();
        let mut expected_pages = None;
        let mut expected_total = None;
        for page in 1..=MAX_PAGES {
            if page > 1 {
                tokio::time::sleep(STATEMENT_PAGE_INTERVAL).await;
            }
            let params = BTreeMap::from([
                ("exchange".to_string(), self.exchange.to_string()),
                ("startTime".to_string(), range.start_ms.to_string()),
                ("endTime".to_string(), range.end_ms.to_string()),
                ("statementType".to_string(), statement_type.to_string()),
                ("page".to_string(), page.to_string()),
                ("pageSize".to_string(), PAGE_SIZE.to_string()),
            ]);
            let value = self.signed_get(STATEMENT_PATH, &params).await?;
            let parsed = validate_statement_page(
                &value,
                page,
                self.exchange,
                &self.portfolio_id,
                range.start_ms,
                range.end_ms,
                expected_pages,
                expected_total,
                &mut ids,
            )?;
            expected_pages = Some(parsed.pages);
            expected_total = Some(parsed.total);
            rows.extend(parsed.rows);
            if page >= parsed.pages {
                if rows.len() != parsed.total {
                    return Err(invalid_response("statement pagination totalSize mismatch"));
                }
                return Ok(rows);
            }
        }
        Err(invalid_response(format!(
            "statement history exceeds the {MAX_PAGES} page limit"
        )))
    }

    /// RapidX signs the raw `k=v&..&nonce` parameter string with HMAC-SHA256
    /// and carries the signature plus nonce/ts in headers; the URL query is
    /// independently form-encoded. This differs from Binance, where the signed
    /// payload is the URL query itself.
    async fn signed_get(
        &self,
        path: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Value, ExchangeError> {
        let nonce = unix_seconds().to_string();
        let signature = sign_params(&self.credentials.secret_key, params, &nonce);
        let query = query_string(
            &params
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Params>(),
        );
        let url = if query.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}{}?{}", self.base_url, path, query)
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-mbx-apikey"),
            header_value("X-MBX-APIKEY", &self.credentials.api_key)?,
        );
        headers.insert(
            HeaderName::from_static("nonce"),
            header_value("nonce", &nonce)?,
        );
        headers.insert(
            HeaderName::from_static("ts"),
            header_value("ts", &unix_micros().to_string())?,
        );
        headers.insert(
            HeaderName::from_static("signature"),
            header_value("signature", &signature)?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            header_value("Content-Type", "application/json")?,
        );
        let value = get_json(&self.dispatcher, EXCHANGE, url, headers, 1).await?;
        check_ltp_code(value)
    }
}

fn execution_ranges(
    range: TimeRange,
    now_ms: i64,
) -> Result<Vec<(&'static str, TimeRange)>, ExchangeError> {
    if range.start_ms <= 0 || range.end_ms > now_ms {
        return Err(ExchangeError::InvalidQuery(
            "invalid execution history window".to_string(),
        ));
    }
    if range.start_ms < now_ms.saturating_sub(ARCHIVE_SPAN_MS) {
        return Err(ExchangeError::InvalidQuery(
            "execution recovery exceeds the documented 90-day archive".to_string(),
        ));
    }
    let cutoff = now_ms.saturating_sub(RECENT_SPAN_MS);
    let mut ranges = Vec::with_capacity(2);
    if range.start_ms < cutoff {
        ranges.push((
            ARCHIVED_EXECUTIONS_PATH,
            TimeRange::new(range.start_ms, range.end_ms.min(cutoff))?,
        ));
    }
    if range.end_ms >= cutoff {
        ranges.push((
            EXECUTIONS_PATH,
            TimeRange::new(range.start_ms.max(cutoff), range.end_ms)?,
        ));
    }
    Ok(ranges)
}

struct Page {
    pages: usize,
    total: usize,
    rows: Vec<Value>,
}

#[allow(clippy::too_many_arguments)]
fn validate_execution_page(
    value: &Value,
    requested_page: usize,
    exchange: &str,
    portfolio_id: &str,
    begin_ms: i64,
    end_ms: i64,
    expected_pages: Option<usize>,
    expected_total: Option<usize>,
    ids: &mut HashSet<String>,
) -> Result<Page, ExchangeError> {
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_response("execution response missing data object"))?;
    let page = integer(data, "page")?;
    let page_size = integer(data, "pageSize")?;
    let pages = integer(data, "pageNum")?;
    let total = integer(data, "totalSize")?;
    if page != requested_page
        || page_size != PAGE_SIZE
        || pages > MAX_PAGES
        || (total > 0 && (pages == 0 || page > pages))
        || (total == 0 && (pages > 1 || page != 1))
    {
        return Err(invalid_response("invalid execution pagination envelope"));
    }
    if expected_pages.is_some_and(|value| value != pages)
        || expected_total.is_some_and(|value| value != total)
    {
        return Err(invalid_response(
            "execution pagination changed during fetch",
        ));
    }
    let list = data
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("execution response missing list"))?;
    if list.is_empty() && total != 0 {
        return Err(invalid_response("unexpected empty execution page"));
    }
    if list.len() > PAGE_SIZE || list.len() > total || (page < pages && list.len() != PAGE_SIZE) {
        return Err(invalid_response(
            "execution page is incomplete or oversized",
        ));
    }
    let mut rows = Vec::with_capacity(list.len());
    for row in list {
        let id = required_string(row, "transactionId")?;
        if !ids.insert(id.to_string()) {
            return Err(invalid_response(format!("duplicate transactionId {id}")));
        }
        if portfolio_text(row, "portfolioId")? != portfolio_id
            || !required_string(row, "exchangeType")?.eq_ignore_ascii_case(exchange)
        {
            return Err(invalid_response("execution scope mismatch"));
        }
        let timestamp = timestamp_ms(row, "createAt")?;
        if timestamp < begin_ms || timestamp > end_ms {
            return Err(invalid_response(
                "execution timestamp outside requested window",
            ));
        }
        rows.push(row.clone());
    }
    Ok(Page { pages, total, rows })
}

#[allow(clippy::too_many_arguments)]
fn validate_statement_page(
    value: &Value,
    requested_page: usize,
    exchange: &str,
    portfolio_id: &str,
    begin_ms: i64,
    end_ms: i64,
    expected_pages: Option<usize>,
    expected_total: Option<usize>,
    ids: &mut HashSet<String>,
) -> Result<Page, ExchangeError> {
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_response("statement response missing data object"))?;
    let page = integer(data, "page")?;
    let page_size = integer(data, "pageSize")?;
    let pages = integer(data, "pageNum")?;
    let total = integer(data, "totalSize")?;
    if page != requested_page
        || page_size != PAGE_SIZE
        || pages > MAX_PAGES
        || total > MAX_PAGES * PAGE_SIZE
        || (total > 0 && (pages != total.div_ceil(PAGE_SIZE) || page > pages))
        || (total == 0 && (pages > 1 || page != 1))
        || expected_pages.is_some_and(|value| value != pages)
        || expected_total.is_some_and(|value| value != total)
    {
        return Err(invalid_response("invalid statement pagination envelope"));
    }
    let list = data
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("statement response missing list"))?;
    let expected_len = total.saturating_sub((page - 1) * PAGE_SIZE).min(PAGE_SIZE);
    if list.len() != expected_len {
        return Err(invalid_response("incomplete or oversized statement page"));
    }
    let mut rows = Vec::with_capacity(list.len());
    for row in list {
        let id = required_string(row, "statementId")?;
        if !ids.insert(id.to_string()) {
            return Err(invalid_response(format!("duplicate statementId {id}")));
        }
        if portfolio_text(row, "portfolioId")? != portfolio_id
            || !required_string(row, "exchangeType")?.eq_ignore_ascii_case(exchange)
        {
            return Err(invalid_response("statement scope mismatch"));
        }
        let timestamp = timestamp_ms(row, "createAt")?;
        if timestamp < begin_ms || timestamp > end_ms {
            return Err(invalid_response(
                "statement timestamp outside requested window",
            ));
        }
        rows.push(row.clone());
    }
    Ok(Page { pages, total, rows })
}

fn check_ltp_code(value: Value) -> Result<Value, ExchangeError> {
    let Some(code) = value.get("code") else {
        return Err(invalid_response("RapidX response missing code"));
    };
    let code_text = code
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| code.to_string());
    if matches!(code_text.as_str(), "200" | "200000") {
        return Ok(value);
    }
    Err(ExchangeError::Api {
        exchange: EXCHANGE,
        code: code_text,
        message: value
            .get("msg")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown RapidX API error")
            .to_string(),
    })
}

fn sign_params(secret: &str, params: &BTreeMap<String, String>, nonce: &str) -> String {
    let mut message = String::new();
    for (index, (key, value)) in params.iter().enumerate() {
        if index > 0 {
            message.push('&');
        }
        message.push_str(key);
        message.push('=');
        message.push_str(value);
    }
    message.push('&');
    message.push_str(nonce);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn integer(object: &serde_json::Map<String, Value>, key: &str) -> Result<usize, ExchangeError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_response(format!("missing integer {key}")))
}

fn required_string<'a>(row: &'a Value, key: &str) -> Result<&'a str, ExchangeError> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_response(format!("row missing string {key}")))
}

/// `portfolioId` is an integer in statement rows but a string in execution
/// rows; both must compare equal to the configured portfolio.
fn portfolio_text(row: &Value, key: &str) -> Result<String, ExchangeError> {
    match row.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(Value::Number(number)) => Ok(number.to_string()),
        _ => Err(invalid_response(format!("row missing {key}"))),
    }
}

fn timestamp_ms(row: &Value, key: &str) -> Result<i64, ExchangeError> {
    let timestamp = match row.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| invalid_response(format!("{key} is not an integer timestamp")))?,
        Some(Value::String(text)) => text
            .parse::<i64>()
            .map_err(|_| invalid_response(format!("{key} is not milliseconds")))?,
        _ => return Err(invalid_response(format!("row missing {key}"))),
    };
    if timestamp <= 0 {
        return Err(invalid_response(format!("{key} must be positive")));
    }
    Ok(timestamp)
}

fn invalid_response(message: impl Into<String>) -> ExchangeError {
    ExchangeError::InvalidResponse {
        exchange: EXCHANGE,
        message: message.into(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_millis() as i64
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_secs() as i64
}

fn unix_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_micros() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_archive_boundary_without_skipping_it_or_overstating_retention() {
        let now = 100 * ONE_DAY_MS;
        let cutoff = now - RECENT_SPAN_MS;
        let range = TimeRange::new(cutoff - 1_000, cutoff + 1_000).unwrap();
        let ranges = execution_ranges(range, now).unwrap();
        assert_eq!(
            ranges,
            vec![
                (
                    ARCHIVED_EXECUTIONS_PATH,
                    TimeRange::new(cutoff - 1_000, cutoff).unwrap()
                ),
                (
                    EXECUTIONS_PATH,
                    TimeRange::new(cutoff, cutoff + 1_000).unwrap()
                )
            ]
        );
        let recent = execution_ranges(TimeRange::new(cutoff, now).unwrap(), now).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].0, EXECUTIONS_PATH);
        assert!(
            execution_ranges(TimeRange::new(now - 91 * ONE_DAY_MS, now).unwrap(), now).is_err()
        );
        assert!(execution_ranges(TimeRange::new(now, now + 1).unwrap(), now).is_err());
    }

    fn execution_page(page: usize, pages: usize, total: usize, id: &str) -> Value {
        serde_json::json!({
            "code": 200000,
            "data": {
                "page": page,
                "pageSize": 1000,
                "pageNum": pages,
                "totalSize": total,
                "list": [{
                    "transactionId": id,
                    "portfolioId": "p",
                    "exchangeType": "BINANCE",
                    "createAt": "10",
                    "fee": "0.1",
                    "feeCoin": "USDT"
                }]
            }
        })
    }

    #[test]
    fn validates_stable_paginated_execution_rows() {
        let mut ids = HashSet::new();
        let parsed = validate_execution_page(
            &execution_page(1, 1, 1, "id1"),
            1,
            "BINANCE",
            "p",
            0,
            10,
            None,
            None,
            &mut ids,
        )
        .unwrap();
        assert_eq!(parsed.rows[0]["transactionId"], "id1");
    }

    #[test]
    fn rejects_duplicate_or_changed_execution_pagination() {
        let mut ids = HashSet::new();
        let mut first = execution_page(1, 2, 1001, "id1");
        let row = first["data"]["list"][0].clone();
        first["data"]["list"] = Value::Array(
            (0..1000)
                .map(|index| {
                    let mut row = row.clone();
                    row["transactionId"] = Value::String(format!("id{index}"));
                    row
                })
                .collect(),
        );
        validate_execution_page(&first, 1, "BINANCE", "p", 0, 10, None, None, &mut ids).unwrap();
        assert!(
            validate_execution_page(
                &execution_page(2, 3, 2, "id1"),
                2,
                "BINANCE",
                "p",
                0,
                10,
                Some(2),
                Some(1001),
                &mut ids
            )
            .is_err()
        );
        assert!(
            validate_execution_page(
                &execution_page(2, 2, 1001, "id1"),
                2,
                "BINANCE",
                "p",
                0,
                10,
                Some(2),
                Some(1001),
                &mut ids
            )
            .is_err()
        );
    }

    #[test]
    fn empty_execution_page_is_valid_but_truncation_is_not() {
        for pages in [0, 1] {
            let body = serde_json::json!({"code":200000,"data":{"page":1,"pageSize":1000,"pageNum":pages,"totalSize":0,"list":[]}});
            assert!(
                validate_execution_page(
                    &body,
                    1,
                    "BINANCE",
                    "p",
                    0,
                    10,
                    None,
                    None,
                    &mut HashSet::new()
                )
                .unwrap()
                .rows
                .is_empty()
            );
        }
        assert!(
            validate_execution_page(
                &execution_page(1, 2, 1001, "id"),
                1,
                "BINANCE",
                "p",
                0,
                10,
                None,
                None,
                &mut HashSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_execution_scope_timestamp_and_missing_data() {
        let mut ids = HashSet::new();
        for (page, exchange, portfolio, end) in [
            (execution_page(1, 1, 1, "id"), "OKX", "p", 10),
            (execution_page(1, 1, 1, "id"), "BINANCE", "other", 10),
            (execution_page(1, 1, 1, "id"), "BINANCE", "p", 9),
            (serde_json::json!({}), "BINANCE", "p", 10),
        ] {
            assert!(
                validate_execution_page(
                    &page, 1, exchange, portfolio, 0, end, None, None, &mut ids
                )
                .is_err()
            );
            ids.clear();
        }
    }

    fn statement_body(
        page: usize,
        pages: usize,
        total: usize,
        id: &str,
        portfolio: Value,
        ts: i64,
    ) -> Value {
        serde_json::json!({"code":200000,"data":{"page":page,"pageSize":1000,"pageNum":pages,
            "totalSize":total,"list":[{"portfolioId":portfolio,"statementId":id,"requestId":"r",
            "coin":"USDT","sym":"","statementType":"FUNDING_FEE","exchangeType":"BINANCE",
            "businessType":"PERP","beforeAvailable":"0","afterAvailable":"1","beforeOverdraw":"0",
            "afterOverdraw":"0","beforeBorrow":"0","afterBorrow":"0","deltaAmount":"1","createAt":ts}]}})
    }

    #[test]
    fn statement_page_accepts_numeric_portfolio_and_valid_envelope() {
        let mut ids = HashSet::new();
        let parsed = validate_statement_page(
            &statement_body(1, 1, 1, "a", Value::from(123), 10),
            1,
            "BINANCE",
            "123",
            0,
            10,
            None,
            None,
            &mut ids,
        )
        .unwrap();
        assert_eq!(parsed.rows.len(), 1);
    }

    #[test]
    fn rejects_invalid_statement_pagination_and_duplicates() {
        for (pages, total) in [(2, 1001), (1, 0), (101, 100_001)] {
            assert!(
                validate_statement_page(
                    &statement_body(1, pages, total, "a", Value::from(123), 10),
                    1,
                    "BINANCE",
                    "123",
                    0,
                    10,
                    None,
                    None,
                    &mut HashSet::new()
                )
                .is_err()
            );
        }
        let mut ids = HashSet::new();
        validate_statement_page(
            &statement_body(1, 1, 1, "a", Value::from(123), 10),
            1,
            "BINANCE",
            "123",
            0,
            10,
            None,
            None,
            &mut ids,
        )
        .unwrap();
        assert!(
            validate_statement_page(
                &statement_body(1, 1, 1, "a", Value::from(123), 10),
                1,
                "BINANCE",
                "123",
                0,
                10,
                Some(1),
                Some(1),
                &mut ids
            )
            .is_err()
        );
        let mut ids = HashSet::new();
        assert!(
            validate_statement_page(
                &statement_body(1, 1, 1, "a", Value::from(456), 10),
                1,
                "BINANCE",
                "123",
                0,
                10,
                None,
                None,
                &mut ids
            )
            .is_err()
        );
        let mut ids = HashSet::new();
        assert!(
            validate_statement_page(
                &statement_body(1, 1, 1, "a", Value::from(123), 11),
                1,
                "BINANCE",
                "123",
                0,
                10,
                None,
                None,
                &mut ids
            )
            .is_err()
        );
    }

    #[test]
    fn sign_params_uses_sorted_raw_params_and_appended_nonce() {
        let params = BTreeMap::from([
            ("page".to_string(), "1".to_string()),
            ("begin".to_string(), "1000".to_string()),
            ("exchange".to_string(), "BINANCE".to_string()),
        ]);
        let signature = sign_params("key", &params, "42");
        let mut mac = HmacSha256::new_from_slice(b"key").unwrap();
        mac.update(b"begin=1000&exchange=BINANCE&page=1&42");
        let expected = hex::encode(mac.finalize().into_bytes());
        assert_eq!(signature, expected);
    }

    #[test]
    fn ltp_error_code_maps_to_api_error() {
        let error =
            check_ltp_code(serde_json::json!({"code": 401001, "msg": "denied"})).unwrap_err();
        assert!(matches!(error, ExchangeError::Api { code, .. } if code == "401001"));
        assert!(check_ltp_code(serde_json::json!({"code": 200, "data": {}})).is_ok());
        assert!(check_ltp_code(serde_json::json!({"code": 200000, "data": {}})).is_ok());
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = LtpCredentials::new("actual-public-value", "actual-secret-value");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("actual-public-value"));
        assert!(!debug.contains("actual-secret-value"));
    }
}
