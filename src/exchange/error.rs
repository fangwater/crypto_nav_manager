use crate::rest_dispatcher::DispatchError;
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("{exchange} returned HTTP {status}: {body}")]
    Http {
        exchange: &'static str,
        status: StatusCode,
        body: String,
    },
    #[error("{exchange} returned API error {code}: {message}")]
    Api {
        exchange: &'static str,
        code: String,
        message: String,
    },
    #[error("{exchange} returned an invalid response: {message}")]
    InvalidResponse {
        exchange: &'static str,
        message: String,
    },
    #[error("failed to construct request header {name}: {message}")]
    Header { name: &'static str, message: String },
}
