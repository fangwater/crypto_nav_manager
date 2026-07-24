use super::{
    ExchangeError,
    common::{Params, get_json, header_value, now_ms, query_string, root_array},
    fee_rates::{normalize_binance, normalize_binance_spot},
};
use crate::{
    models::{TimeRange, TradingFeeRate},
    rest_dispatcher::{DispatchError, Dispatcher},
};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName};
use serde_json::Value;
use sha2::Sha256;

const EXCHANGE: &str = "binance";
const FAPI_BASE: &str = "https://fapi.binance.com";
const PAPI_BASE: &str = "https://papi.binance.com";
const API_BASE: &str = "https://api.binance.com";
const ONE_DAY_MS: i64 = 24 * 60 * 60 * 1_000;
const THIRTY_DAYS_MS: i64 = 30 * ONE_DAY_MS;
const SIX_MONTHS_MS: i64 = 180 * ONE_DAY_MS;
const SEVEN_DAYS_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const ASSET_DIVIDEND_MAX_SPAN_MS: i64 = 180 * ONE_DAY_MS;
const ASSET_DIVIDEND_LIMIT: usize = 500;
const LIMIT: usize = 1_000;
const FORCE_ORDER_LIMIT: usize = 100;
const HISTORY_RATE_LIMIT_RETRIES: usize = 3;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct BinanceCredentials {
    api_key: String,
    secret_key: String,
}

impl BinanceCredentials {
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret_key: secret_key.into(),
        }
    }
}

impl std::fmt::Debug for BinanceCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BinanceCredentials")
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinanceAccountMode {
    UsdmFutures,
    PortfolioMargin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinanceAutoCloseType {
    Liquidation,
    Adl,
}

impl BinanceAutoCloseType {
    fn api_value(self) -> &'static str {
        match self {
            Self::Liquidation => "LIQUIDATION",
            Self::Adl => "ADL",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BinanceClient {
    dispatcher: Dispatcher,
    credentials: BinanceCredentials,
    mode: BinanceAccountMode,
}

impl BinanceClient {
    pub fn new(
        dispatcher: Dispatcher,
        credentials: BinanceCredentials,
        mode: BinanceAccountMode,
    ) -> Self {
        Self {
            dispatcher,
            credentials,
            mode,
        }
    }

    pub fn mode(&self) -> BinanceAccountMode {
        self.mode
    }

    pub async fn account_snapshot(&self) -> Result<Value, ExchangeError> {
        match self.mode {
            BinanceAccountMode::UsdmFutures => {
                self.signed_get(FAPI_BASE, "/fapi/v2/account", Vec::new(), 5)
                    .await
            }
            BinanceAccountMode::PortfolioMargin => {
                self.signed_get(PAPI_BASE, "/papi/v1/account", Vec::new(), 5)
                    .await
            }
        }
    }

    pub async fn user_trades(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        match self.mode {
            BinanceAccountMode::UsdmFutures => {
                self.paged_trades(
                    FAPI_BASE,
                    "/fapi/v1/userTrades",
                    symbol,
                    range,
                    SEVEN_DAYS_MS,
                    5,
                )
                .await
            }
            BinanceAccountMode::PortfolioMargin => Err(ExchangeError::InvalidQuery(
                "portfolio margin must choose margin_trades or um_trades".to_string(),
            )),
        }
    }

    /// Queries Spot account trades belonging to a standard Binance account.
    pub async fn spot_trades(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.require_standard_account()?;
        self.paged_trades(API_BASE, "/api/v3/myTrades", symbol, range, ONE_DAY_MS, 20)
            .await
    }

    pub async fn margin_trades(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.require_portfolio_margin()?;
        self.paged_trades(
            PAPI_BASE,
            "/papi/v1/margin/myTrades",
            symbol,
            range,
            ONE_DAY_MS,
            5,
        )
        .await
    }

    pub async fn um_trades(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.require_portfolio_margin()?;
        self.paged_trades(
            PAPI_BASE,
            "/papi/v1/um/userTrades",
            symbol,
            range,
            SEVEN_DAYS_MS,
            5,
        )
        .await
    }

    pub async fn open_orders(&self, symbol: Option<&str>) -> Result<Vec<Value>, ExchangeError> {
        if self.mode != BinanceAccountMode::UsdmFutures {
            return Err(ExchangeError::InvalidQuery(
                "open_orders currently maps to the standard UM /fapi endpoint".to_string(),
            ));
        }
        let mut params = Vec::new();
        if let Some(symbol) = symbol {
            params.push(("symbol".to_string(), symbol.to_string()));
        }
        let value = self
            .signed_get(FAPI_BASE, "/fapi/v1/openOrders", params, 1)
            .await?;
        root_array(EXCHANGE, value)
    }

    /// Queries USD-M liquidation and ADL orders. Standard futures accounts use
    /// fapi while Portfolio Margin accounts use the corresponding papi UM API.
    pub async fn force_orders(
        &self,
        symbol: Option<&str>,
        auto_close_type: Option<BinanceAutoCloseType>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let (base, path) = match self.mode {
            BinanceAccountMode::UsdmFutures => (FAPI_BASE, "/fapi/v1/forceOrders"),
            BinanceAccountMode::PortfolioMargin => (PAPI_BASE, "/papi/v1/um/forceOrders"),
        };
        self.paged_force_orders(base, path, symbol, auto_close_type, range)
            .await
    }

    /// Queries forced orders on the Portfolio Margin cross-margin leg.
    pub async fn margin_force_orders(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        self.require_portfolio_margin()?;
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();

        for chunk in range.chunks(SEVEN_DAYS_MS)? {
            let mut current = 1_i64;
            loop {
                let params = vec![
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("current".to_string(), current.to_string()),
                    ("size".to_string(), FORCE_ORDER_LIMIT.to_string()),
                ];
                let value = self
                    .signed_get(PAPI_BASE, "/papi/v1/margin/forceOrders", params, 1)
                    .await?;
                let total = value_i64(&value, "total");
                let page = value
                    .get("rows")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "margin force orders response is missing rows".to_string(),
                    })?;
                let page_len = page.len();
                rows.extend(page.into_iter().filter(|row| {
                    value_i64(row, "updatedTime")
                        .is_some_and(|ts| chunk.start_ms <= ts && ts <= chunk.end_ms)
                }));

                let covered = current.saturating_mul(FORCE_ORDER_LIMIT as i64);
                if page_len < FORCE_ORDER_LIMIT || total.is_some_and(|total| covered >= total) {
                    break;
                }
                current = current.saturating_add(1);
            }
        }

        dedup(&mut rows, &["orderId", "symbol"]);
        rows.sort_by_key(|row| value_i64(row, "updatedTime").unwrap_or_default());
        Ok(rows)
    }

    pub async fn funding_fees(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        match self.mode {
            BinanceAccountMode::UsdmFutures => self.usdm_funding_fees(range).await,
            BinanceAccountMode::PortfolioMargin => self.portfolio_margin_funding_fees(range).await,
        }
    }

    /// Queries Spot wallet distributions, including hourly Spot MM rebates.
    /// Saturated windows are split recursively because this endpoint has no
    /// page cursor and returns at most 500 rows.
    pub async fn asset_dividends(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        self.require_standard_account()?;
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut pending = range.chunks(ASSET_DIVIDEND_MAX_SPAN_MS)?;
        pending.reverse();
        let mut rows = Vec::new();

        while let Some(window) = pending.pop() {
            let params = vec![
                ("startTime".to_string(), window.start_ms.to_string()),
                ("endTime".to_string(), window.end_ms.to_string()),
                ("limit".to_string(), ASSET_DIVIDEND_LIMIT.to_string()),
            ];
            let value = self
                .signed_get(API_BASE, "/sapi/v1/asset/assetDividend", params, 10)
                .await?;
            let total = value_i64(&value, "total");
            let page = value
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| ExchangeError::InvalidResponse {
                    exchange: EXCHANGE,
                    message: "asset dividend response is missing rows".to_string(),
                })?;
            let page_len = page.len();

            if asset_dividend_page_is_saturated(total, page_len) {
                let (left, right) = split_asset_dividend_range(window)?;
                pending.push(right);
                pending.push(left);
                continue;
            }

            rows.extend(page.into_iter().filter(|row| {
                value_i64(row, "divTime")
                    .is_some_and(|ts| window.start_ms <= ts && ts <= window.end_ms)
            }));
        }

        dedup(&mut rows, &["id"]);
        rows.sort_by_key(|row| value_i64(row, "divTime").unwrap_or_default());
        Ok(rows)
    }

    async fn usdm_funding_fees(&self, range: TimeRange) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        for chunk in range.chunks(SEVEN_DAYS_MS)? {
            let mut page_number = 1_i64;
            loop {
                let params = vec![
                    ("incomeType".to_string(), "FUNDING_FEE".to_string()),
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("page".to_string(), page_number.to_string()),
                    ("limit".to_string(), LIMIT.to_string()),
                ];
                let value = self
                    .paced_history_get(FAPI_BASE, "/fapi/v1/income", params, 30)
                    .await?;
                let page = root_array(EXCHANGE, value)?;
                let page_len = page.len();
                rows.extend(page.into_iter().filter(|row| {
                    value_i64(row, "time")
                        .is_some_and(|ts| chunk.start_ms <= ts && ts <= chunk.end_ms)
                }));
                if page_len < LIMIT {
                    break;
                }
                page_number =
                    page_number
                        .checked_add(1)
                        .ok_or_else(|| ExchangeError::InvalidResponse {
                            exchange: EXCHANGE,
                            message: "standard UM income pagination overflowed".to_string(),
                        })?;
            }
        }
        dedup(&mut rows, &["tranId"]);
        rows.sort_by_key(|row| value_i64(row, "time").unwrap_or_default());
        Ok(rows)
    }

    async fn portfolio_margin_funding_fees(
        &self,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.require_portfolio_margin()?;
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut rows = Vec::new();
        let mut page_number = 1_i64;
        loop {
            let params = vec![
                ("incomeType".to_string(), "FUNDING_FEE".to_string()),
                ("startTime".to_string(), range.start_ms.to_string()),
                ("endTime".to_string(), range.end_ms.to_string()),
                ("page".to_string(), page_number.to_string()),
                ("limit".to_string(), LIMIT.to_string()),
                ("recvWindow".to_string(), "60000".to_string()),
            ];
            let value = self
                .signed_get(PAPI_BASE, "/papi/v1/um/income", params, 30)
                .await?;
            let page = root_array(EXCHANGE, value)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            rows.extend(page.into_iter().filter(|row| {
                value_i64(row, "time").is_some_and(|ts| range.start_ms <= ts && ts <= range.end_ms)
            }));
            if page_len < LIMIT {
                break;
            }
            let next_page = page_number.saturating_add(1);
            if next_page <= page_number {
                return Err(ExchangeError::InvalidResponse {
                    exchange: EXCHANGE,
                    message: "income pagination did not advance".to_string(),
                });
            }
            page_number = next_page;
        }
        dedup(&mut rows, &["tranId"]);
        rows.sort_by_key(|row| value_i64(row, "time").unwrap_or_default());
        Ok(rows)
    }

    pub async fn raw_fee_rates(&self, symbol: &str) -> Result<Value, ExchangeError> {
        let (base, path) = match self.mode {
            BinanceAccountMode::UsdmFutures => (FAPI_BASE, "/fapi/v1/commissionRate"),
            BinanceAccountMode::PortfolioMargin => (PAPI_BASE, "/papi/v1/um/commissionRate"),
        };
        self.signed_get(
            base,
            path,
            vec![("symbol".to_string(), symbol.to_string())],
            20,
        )
        .await
    }

    pub async fn fee_rates(&self, symbol: &str) -> Result<Vec<TradingFeeRate>, ExchangeError> {
        let raw = self.raw_fee_rates(symbol).await?;
        let account_mode = match self.mode {
            BinanceAccountMode::UsdmFutures => "usdm_futures",
            BinanceAccountMode::PortfolioMargin => "portfolio_margin",
        };
        normalize_binance(raw, account_mode, now_ms())
    }

    pub async fn raw_spot_fee_rates(&self, symbol: &str) -> Result<Value, ExchangeError> {
        self.signed_get(
            API_BASE,
            "/api/v3/account/commission",
            vec![("symbol".to_string(), symbol.to_ascii_uppercase())],
            20,
        )
        .await
    }

    pub async fn spot_fee_rates(&self, symbol: &str) -> Result<Vec<TradingFeeRate>, ExchangeError> {
        let account_mode = match self.mode {
            BinanceAccountMode::UsdmFutures => "usdm_futures",
            BinanceAccountMode::PortfolioMargin => "portfolio_margin",
        };
        normalize_binance_spot(
            self.raw_spot_fee_rates(symbol).await?,
            account_mode,
            now_ms(),
        )
    }

    pub async fn margin_interest(
        &self,
        range: TimeRange,
        asset: Option<&str>,
        archived: bool,
    ) -> Result<Vec<Value>, ExchangeError> {
        self.require_portfolio_margin()?;
        let mut rows = Vec::new();
        for chunk in range.chunks(THIRTY_DAYS_MS)? {
            let mut current = 1_u32;
            loop {
                let mut params = vec![
                    ("startTime".to_string(), chunk.start_ms.to_string()),
                    ("endTime".to_string(), chunk.end_ms.to_string()),
                    ("current".to_string(), current.to_string()),
                    ("size".to_string(), "100".to_string()),
                    ("archived".to_string(), archived.to_string()),
                ];
                if let Some(asset) = asset {
                    params.push(("asset".to_string(), asset.to_string()));
                }
                let value = self
                    .history_signed_get(
                        PAPI_BASE,
                        "/papi/v1/margin/marginInterestHistory",
                        params,
                        1,
                    )
                    .await?;
                let page = value
                    .get("rows")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: "margin interest response is missing rows".to_string(),
                    })?;
                let page_len = page.len();
                rows.extend(page.into_iter().filter(|row| {
                    value_i64(row, "interestAccuredTime")
                        .is_some_and(|ts| chunk.start_ms <= ts && ts <= chunk.end_ms)
                }));
                if page_len < 100 {
                    break;
                }
                current = current.saturating_add(1);
            }
        }
        dedup(&mut rows, &["txId"]);
        rows.sort_by_key(|row| value_i64(row, "interestAccuredTime").unwrap_or_default());
        Ok(rows)
    }

    pub async fn margin_interest_history(
        &self,
        range: TimeRange,
        asset: Option<&str>,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut rows = Vec::new();
        for (query_range, archived) in margin_interest_query_ranges(range, now_ms())? {
            rows.extend(self.margin_interest(query_range, asset, archived).await?);
        }
        dedup(&mut rows, &["txId"]);
        rows.sort_by_key(|row| value_i64(row, "interestAccuredTime").unwrap_or_default());
        Ok(rows)
    }

    pub async fn premium_index(&self, symbol: Option<&str>) -> Result<Value, ExchangeError> {
        let mut params = Vec::new();
        if let Some(symbol) = symbol {
            params.push(("symbol".to_string(), symbol.to_string()));
        }
        self.public_get(FAPI_BASE, "/fapi/v1/premiumIndex", params, 1)
            .await
    }

    pub async fn futures_ticker_price(&self, symbol: Option<&str>) -> Result<Value, ExchangeError> {
        let mut params = Vec::new();
        if let Some(symbol) = symbol {
            params.push(("symbol".to_string(), symbol.to_string()));
        }
        self.public_get(FAPI_BASE, "/fapi/v2/ticker/price", params, 1)
            .await
    }

    pub async fn spot_ticker_price(&self, symbol: &str) -> Result<Value, ExchangeError> {
        self.public_get(
            API_BASE,
            "/api/v3/ticker/price",
            vec![("symbol".to_string(), symbol.to_string())],
            2,
        )
        .await
    }

    pub async fn collateral_rates(&self) -> Result<Value, ExchangeError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-mbx-apikey"),
            header_value("X-MBX-APIKEY", &self.credentials.api_key)?,
        );
        get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{API_BASE}/sapi/v1/portfolio/collateralRate"),
            headers,
            50,
        )
        .await
    }

    async fn paged_trades(
        &self,
        base: &str,
        path: &str,
        symbol: &str,
        range: TimeRange,
        chunk_span_ms: i64,
        weight: u32,
    ) -> Result<Vec<Value>, ExchangeError> {
        let mut all_rows = Vec::new();
        for chunk in range.chunks(chunk_span_ms)? {
            let mut from_id: Option<i64> = None;
            loop {
                let mut params = vec![
                    ("symbol".to_string(), symbol.to_string()),
                    ("limit".to_string(), LIMIT.to_string()),
                ];
                if let Some(from_id) = from_id {
                    params.push(("fromId".to_string(), from_id.to_string()));
                } else {
                    params.push(("startTime".to_string(), chunk.start_ms.to_string()));
                    params.push(("endTime".to_string(), chunk.end_ms.to_string()));
                }
                let value = self.paced_history_get(base, path, params, weight).await?;
                let mut page = root_array(EXCHANGE, value)?;
                if page.is_empty() {
                    break;
                }
                page.sort_by_key(|row| value_i64(row, "id").unwrap_or_default());
                let page_len = page.len();
                let last_id = page
                    .last()
                    .and_then(|row| value_i64(row, "id"))
                    .ok_or_else(|| ExchangeError::InvalidResponse {
                        exchange: EXCHANGE,
                        message: format!("{path} page has no numeric id"),
                    })?;
                let last_time = page
                    .last()
                    .and_then(|row| value_i64(row, "time"))
                    .unwrap_or_default();
                all_rows.extend(page.into_iter().filter(|row| {
                    value_i64(row, "time")
                        .is_some_and(|ts| chunk.start_ms <= ts && ts <= chunk.end_ms)
                }));
                if page_len < LIMIT || last_time > chunk.end_ms {
                    break;
                }
                from_id = Some(last_id.saturating_add(1));
            }
        }
        dedup(&mut all_rows, &["symbol", "id"]);
        all_rows.sort_by_key(|row| value_i64(row, "time").unwrap_or_default());
        Ok(all_rows)
    }

    async fn paged_force_orders(
        &self,
        base: &str,
        path: &str,
        symbol: Option<&str>,
        auto_close_type: Option<BinanceAutoCloseType>,
        range: TimeRange,
    ) -> Result<Vec<Value>, ExchangeError> {
        let range = TimeRange::new(range.start_ms, range.end_ms)?;
        let mut pending = range.chunks(SEVEN_DAYS_MS)?;
        pending.reverse();
        let mut rows = Vec::new();

        while let Some(window) = pending.pop() {
            let mut params = vec![
                ("startTime".to_string(), window.start_ms.to_string()),
                ("endTime".to_string(), window.end_ms.to_string()),
                ("limit".to_string(), FORCE_ORDER_LIMIT.to_string()),
            ];
            if let Some(symbol) = symbol {
                params.push(("symbol".to_string(), symbol.to_string()));
            }
            if let Some(auto_close_type) = auto_close_type {
                params.push((
                    "autoCloseType".to_string(),
                    auto_close_type.api_value().to_string(),
                ));
            }

            let weight = if symbol.is_some() { 20 } else { 50 };
            let value = self.signed_get(base, path, params, weight).await?;
            let page = root_array(EXCHANGE, value)?;

            if page.len() == FORCE_ORDER_LIMIT {
                let (left, right) = split_force_order_range(window)?;
                pending.push(right);
                pending.push(left);
                continue;
            }

            rows.extend(page.into_iter().filter(|row| {
                value_i64(row, "time")
                    .is_some_and(|ts| window.start_ms <= ts && ts <= window.end_ms)
            }));
        }

        dedup(&mut rows, &["orderId", "symbol"]);
        rows.sort_by_key(|row| value_i64(row, "time").unwrap_or_default());
        Ok(rows)
    }

    async fn paced_history_get(
        &self,
        base: &str,
        path: &str,
        params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        let value = self.history_signed_get(base, path, params, weight).await?;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        Ok(value)
    }

    async fn history_signed_get(
        &self,
        base: &str,
        path: &str,
        mut params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        if !params.iter().any(|(key, _)| key == "recvWindow") {
            params.push(("recvWindow".to_string(), "60000".to_string()));
        }
        for retry in 0..=HISTORY_RATE_LIMIT_RETRIES {
            match self.signed_get(base, path, params.clone(), weight).await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let Some(delay) = history_retry_delay(&error) else {
                        return Err(error);
                    };
                    if retry == HISTORY_RATE_LIMIT_RETRIES {
                        return Err(error);
                    }
                    eprintln!(
                        "Binance history rate limited; retry {}/{} after {:.1}s",
                        retry + 1,
                        HISTORY_RATE_LIMIT_RETRIES,
                        delay.as_secs_f64()
                    );
                    tokio::time::sleep(delay + std::time::Duration::from_millis(10)).await;
                }
            }
        }

        unreachable!("history retry loop always returns")
    }

    async fn signed_get(
        &self,
        base: &str,
        path: &str,
        mut params: Params,
        weight: u32,
    ) -> Result<Value, ExchangeError> {
        if !params.iter().any(|(key, _)| key == "recvWindow") {
            params.push(("recvWindow".to_string(), "5000".to_string()));
        }
        params.push(("timestamp".to_string(), now_ms().to_string()));
        let query = query_string(&params);
        let signature = sign_query(&query, &self.credentials.secret_key);
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-mbx-apikey"),
            header_value("X-MBX-APIKEY", &self.credentials.api_key)?,
        );
        let value = get_json(
            &self.dispatcher,
            EXCHANGE,
            format!("{base}{path}?{query}&signature={signature}"),
            headers,
            weight,
        )
        .await?;
        check_api_error(value)
    }

    async fn public_get(
        &self,
        base: &str,
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
            format!("{base}{path}{suffix}"),
            HeaderMap::new(),
            weight,
        )
        .await
    }

    fn require_portfolio_margin(&self) -> Result<(), ExchangeError> {
        if self.mode == BinanceAccountMode::PortfolioMargin {
            Ok(())
        } else {
            Err(ExchangeError::InvalidQuery(
                "endpoint requires Binance Portfolio Margin mode".to_string(),
            ))
        }
    }

    fn require_standard_account(&self) -> Result<(), ExchangeError> {
        if self.mode == BinanceAccountMode::UsdmFutures {
            Ok(())
        } else {
            Err(ExchangeError::InvalidQuery(
                "endpoint requires a standard Binance account".to_string(),
            ))
        }
    }
}

fn sign_query(query: &str, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn check_api_error(value: Value) -> Result<Value, ExchangeError> {
    let Some(code) = value.get("code") else {
        return Ok(value);
    };
    let code_text = code
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| code.to_string());
    if matches!(code_text.as_str(), "0" | "200") {
        return Ok(value);
    }
    Err(ExchangeError::Api {
        exchange: EXCHANGE,
        code: code_text,
        message: value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown Binance API error")
            .to_string(),
    })
}

fn history_retry_delay(error: &ExchangeError) -> Option<std::time::Duration> {
    match error {
        ExchangeError::Dispatch(DispatchError::RateLimited { retry_after, .. }) => {
            Some(*retry_after)
        }
        ExchangeError::Dispatch(DispatchError::NoAvailableIp {
            retry_after: Some(retry_after),
        }) => Some(*retry_after),
        ExchangeError::Dispatch(DispatchError::Request { .. }) => {
            Some(std::time::Duration::from_secs(1))
        }
        ExchangeError::InvalidResponse { message, .. }
            if message.contains("error decoding response body") =>
        {
            Some(std::time::Duration::from_secs(1))
        }
        ExchangeError::Http { body, .. } if body.contains("\"code\":-1021") => {
            Some(std::time::Duration::from_secs(1))
        }
        _ => None,
    }
}

fn margin_interest_query_ranges(
    range: TimeRange,
    current_time_ms: i64,
) -> Result<Vec<(TimeRange, bool)>, ExchangeError> {
    let archive_cutoff = current_time_ms.saturating_sub(SIX_MONTHS_MS);
    let archived_end = archive_cutoff.saturating_add(SEVEN_DAYS_MS);
    let recent_start = archive_cutoff.saturating_sub(SEVEN_DAYS_MS);
    let mut ranges = Vec::with_capacity(2);

    if range.start_ms <= archived_end && range.start_ms <= range.end_ms {
        ranges.push((
            TimeRange::new(range.start_ms, range.end_ms.min(archived_end))?,
            true,
        ));
    }
    if range.end_ms >= recent_start && range.start_ms <= range.end_ms {
        ranges.push((
            TimeRange::new(range.start_ms.max(recent_start), range.end_ms)?,
            false,
        ));
    }
    Ok(ranges)
}

fn value_i64(value: &Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
}

fn split_force_order_range(range: TimeRange) -> Result<(TimeRange, TimeRange), ExchangeError> {
    if range.start_ms == range.end_ms {
        return Err(ExchangeError::InvalidResponse {
            exchange: EXCHANGE,
            message: format!(
                "force orders exceeded the {} row limit at timestamp {}; query by symbol",
                FORCE_ORDER_LIMIT, range.start_ms
            ),
        });
    }
    let middle = range.start_ms + (range.end_ms - range.start_ms) / 2;
    Ok((
        TimeRange {
            start_ms: range.start_ms,
            end_ms: middle,
        },
        TimeRange {
            start_ms: middle + 1,
            end_ms: range.end_ms,
        },
    ))
}

fn asset_dividend_page_is_saturated(total: Option<i64>, page_len: usize) -> bool {
    total
        .map(|total| total > page_len as i64)
        .unwrap_or(page_len == ASSET_DIVIDEND_LIMIT)
}

fn split_asset_dividend_range(range: TimeRange) -> Result<(TimeRange, TimeRange), ExchangeError> {
    if range.start_ms == range.end_ms {
        return Err(ExchangeError::InvalidResponse {
            exchange: EXCHANGE,
            message: format!(
                "asset dividends exceeded the {} row limit at timestamp {}",
                ASSET_DIVIDEND_LIMIT, range.start_ms
            ),
        });
    }
    let middle = range.start_ms + (range.end_ms - range.start_ms) / 2;
    Ok((
        TimeRange {
            start_ms: range.start_ms,
            end_ms: middle,
        },
        TimeRange {
            start_ms: middle + 1,
            end_ms: range.end_ms,
        },
    ))
}

fn dedup(rows: &mut Vec<Value>, keys: &[&str]) {
    let mut seen = std::collections::HashSet::new();
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
    fn hmac_matches_standard_sha256_vector() {
        assert_eq!(
            sign_query("The quick brown fox jumps over the lazy dog", "key"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn credentials_debug_is_redacted() {
        let credentials = BinanceCredentials::new("actual-public-value", "actual-secret-value");
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("actual-public-value"));
        assert!(!debug.contains("actual-secret-value"));
    }

    #[test]
    fn auto_close_types_match_binance_values() {
        assert_eq!(BinanceAutoCloseType::Liquidation.api_value(), "LIQUIDATION");
        assert_eq!(BinanceAutoCloseType::Adl.api_value(), "ADL");
    }

    #[test]
    fn saturated_force_order_ranges_split_without_overlap() {
        let (left, right) = split_force_order_range(TimeRange {
            start_ms: 100,
            end_ms: 199,
        })
        .expect("range should split");
        assert_eq!(
            left,
            TimeRange {
                start_ms: 100,
                end_ms: 149
            }
        );
        assert_eq!(
            right,
            TimeRange {
                start_ms: 150,
                end_ms: 199
            }
        );
    }

    #[test]
    fn trade_dedup_keeps_distinct_fills_from_the_same_order() {
        let mut rows = vec![
            serde_json::json!({"symbol": "BTCUSDT", "id": 1, "orderId": 9}),
            serde_json::json!({"symbol": "BTCUSDT", "id": 2, "orderId": 9}),
            serde_json::json!({"symbol": "BTCUSDT", "id": 1, "orderId": 9}),
        ];

        dedup(&mut rows, &["symbol", "id"]);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn history_retry_uses_dispatcher_delay() {
        let error = ExchangeError::Dispatch(DispatchError::NoAvailableIp {
            retry_after: Some(std::time::Duration::from_secs(42)),
        });
        assert_eq!(
            history_retry_delay(&error),
            Some(std::time::Duration::from_secs(42))
        );
    }

    #[test]
    fn history_retry_recovers_idempotent_transport_failures() {
        let error = ExchangeError::InvalidResponse {
            exchange: EXCHANGE,
            message: "error decoding response body".to_string(),
        };
        assert_eq!(
            history_retry_delay(&error),
            Some(std::time::Duration::from_secs(1))
        );
    }

    #[test]
    fn history_retry_refreshes_timestamp_after_recv_window_error() {
        let error = ExchangeError::Http {
            exchange: EXCHANGE,
            status: reqwest::StatusCode::BAD_REQUEST,
            body: r#"{"code":-1021,"msg":"Timestamp outside recvWindow"}"#.to_string(),
        };
        assert_eq!(
            history_retry_delay(&error),
            Some(std::time::Duration::from_secs(1))
        );
    }

    #[test]
    fn margin_interest_ranges_cover_archive_boundary_with_overlap() {
        let current = 365 * ONE_DAY_MS;
        let range = TimeRange::new(100 * ONE_DAY_MS, 300 * ONE_DAY_MS).unwrap();
        let ranges = margin_interest_query_ranges(range, current).unwrap();

        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0.start_ms, 100 * ONE_DAY_MS);
        assert_eq!(ranges[0].0.end_ms, 192 * ONE_DAY_MS);
        assert!(ranges[0].1);
        assert_eq!(ranges[1].0.start_ms, 178 * ONE_DAY_MS);
        assert_eq!(ranges[1].0.end_ms, 300 * ONE_DAY_MS);
        assert!(!ranges[1].1);
    }

    #[test]
    fn margin_interest_ranges_select_one_storage_tier_when_unambiguous() {
        let current = 365 * ONE_DAY_MS;
        let old = margin_interest_query_ranges(
            TimeRange::new(10 * ONE_DAY_MS, 100 * ONE_DAY_MS).unwrap(),
            current,
        )
        .unwrap();
        let recent = margin_interest_query_ranges(
            TimeRange::new(300 * ONE_DAY_MS, current).unwrap(),
            current,
        )
        .unwrap();

        assert_eq!(old.len(), 1);
        assert!(old[0].1);
        assert_eq!(recent.len(), 1);
        assert!(!recent[0].1);
    }

    #[test]
    fn asset_dividend_saturation_uses_reported_total() {
        assert!(asset_dividend_page_is_saturated(Some(501), 500));
        assert!(!asset_dividend_page_is_saturated(Some(500), 500));
        assert!(asset_dividend_page_is_saturated(None, 500));
        assert!(!asset_dividend_page_is_saturated(None, 499));
    }

    #[test]
    fn saturated_asset_dividend_ranges_split_without_overlap() {
        let (left, right) = split_asset_dividend_range(TimeRange {
            start_ms: 100,
            end_ms: 199,
        })
        .expect("range should split");
        assert_eq!(left.end_ms, 149);
        assert_eq!(right.start_ms, 150);
        assert_eq!(right.end_ms, 199);
    }
}
