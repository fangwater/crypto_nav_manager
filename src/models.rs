use crate::exchange::ExchangeError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl TimeRange {
    pub fn new(start_ms: i64, end_ms: i64) -> Result<Self, ExchangeError> {
        if start_ms < 0 {
            return Err(ExchangeError::InvalidQuery(
                "start_ms must not be negative".to_string(),
            ));
        }
        if end_ms < start_ms {
            return Err(ExchangeError::InvalidQuery(
                "end_ms must be greater than or equal to start_ms".to_string(),
            ));
        }
        Ok(Self { start_ms, end_ms })
    }

    pub fn chunks(self, max_span_ms: i64) -> Result<Vec<Self>, ExchangeError> {
        if max_span_ms <= 0 {
            return Err(ExchangeError::InvalidQuery(
                "max_span_ms must be greater than zero".to_string(),
            ));
        }

        let mut chunks = Vec::new();
        let mut start = self.start_ms;
        while start <= self.end_ms {
            let end = start
                .saturating_add(max_span_ms)
                .saturating_sub(1)
                .min(self.end_ms);
            chunks.push(Self {
                start_ms: start,
                end_ms: end,
            });
            if end == self.end_ms {
                break;
            }
            start = end.saturating_add(1);
        }
        Ok(chunks)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountMode {
    BinanceUsdmFutures,
    BinancePortfolioMargin,
    GateUnified,
    BitgetUnified,
    BybitUnified,
    OkxUnified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProductCategory {
    Spot,
    Margin,
    UsdtFutures,
    CoinFutures,
    UsdcFutures,
}

impl ProductCategory {
    pub fn bitget_value(self) -> &'static str {
        match self {
            Self::Spot => "SPOT",
            Self::Margin => "MARGIN",
            Self::UsdtFutures => "USDT-FUTURES",
            Self::CoinFutures => "COIN-FUTURES",
            Self::UsdcFutures => "USDC-FUTURES",
        }
    }

    pub fn storage_value(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Margin => "margin",
            Self::UsdtFutures => "usdt_futures",
            Self::CoinFutures => "coin_futures",
            Self::UsdcFutures => "usdc_futures",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TradingFeeRate {
    pub exchange: String,
    pub account_mode: String,
    pub market: String,
    /// Exact symbol/family when the exchange scopes the response, otherwise "*".
    pub instrument: String,
    /// Positive values are costs and negative values are rebates.
    pub maker_rate: String,
    /// Positive values are costs and negative values are rebates.
    pub taker_rate: String,
    pub fee_tier: Option<String>,
    pub fee_group: Option<String>,
    pub effective_at_ms: i64,
    pub raw: Value,
}
