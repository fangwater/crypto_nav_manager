use super::ExchangeError;
use crate::rest_dispatcher::{Dispatcher, RequestSpec};
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) type Params = Vec<(String, String)>;

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_millis() as i64
}

pub(crate) fn now_sec() -> i64 {
    now_ms() / 1_000
}

pub(crate) fn query_string(params: &Params) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in params {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

pub(crate) fn header_value(name: &'static str, value: &str) -> Result<HeaderValue, ExchangeError> {
    HeaderValue::from_str(value).map_err(|error| ExchangeError::Header {
        name,
        message: error.to_string(),
    })
}

pub(crate) async fn get_json(
    dispatcher: &Dispatcher,
    exchange: &'static str,
    url: String,
    headers: HeaderMap,
    weight: u32,
) -> Result<Value, ExchangeError> {
    let response = dispatcher
        .dispatch(RequestSpec {
            method: Method::GET,
            url,
            headers,
            body: None,
            weight,
        })
        .await?;
    let status = response.response.status();
    let body = response
        .response
        .text()
        .await
        .map_err(|error| ExchangeError::InvalidResponse {
            exchange,
            message: error.to_string(),
        })?;
    if !status.is_success() {
        return Err(ExchangeError::Http {
            exchange,
            status,
            body,
        });
    }
    serde_json::from_str(&body).map_err(|error| ExchangeError::InvalidResponse {
        exchange,
        message: format!("{error}; body={}", truncate(&body, 500)),
    })
}

pub(crate) fn data_array(
    exchange: &'static str,
    value: &Value,
) -> Result<Vec<Value>, ExchangeError> {
    value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ExchangeError::InvalidResponse {
            exchange,
            message: format!("missing data array: {}", truncate(&value.to_string(), 500)),
        })
}

pub(crate) fn root_array(
    exchange: &'static str,
    value: Value,
) -> Result<Vec<Value>, ExchangeError> {
    value
        .as_array()
        .cloned()
        .ok_or_else(|| ExchangeError::InvalidResponse {
            exchange,
            message: format!("expected array: {}", truncate(&value.to_string(), 500)),
        })
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
