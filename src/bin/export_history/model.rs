use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Dataset {
    All,
    Trades,
    Funding,
    Interest,
}

impl Dataset {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Trades => "trades",
            Self::Funding => "funding",
            Self::Interest => "interest",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StrategyClass {
    Fr,
    Intra,
    Mm,
}

#[derive(Debug)]
pub(crate) struct Strategy {
    pub(crate) slug: String,
    pub(crate) schema: String,
    pub(crate) exchange: String,
    pub(crate) account: String,
    pub(crate) class: StrategyClass,
}

impl Strategy {
    pub(crate) fn supports(&self, dataset: Dataset) -> bool {
        match dataset {
            Dataset::All | Dataset::Trades | Dataset::Funding => true,
            Dataset::Interest => self.class != StrategyClass::Mm,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TradeStorage {
    Liang,
    Generic(String),
}

#[derive(Clone, Debug)]
pub(crate) enum CashStorage {
    Binance(String),
    Text(String),
    Generic(String),
}

#[derive(Debug, Serialize)]
pub(crate) struct TradeCsvRow {
    pub(crate) sid: i16,
    pub(crate) key: String,
    pub(crate) symbol: String,
    pub(crate) id: String,
    #[serde(rename = "orderId")]
    pub(crate) order_id: String,
    pub(crate) side: String,
    pub(crate) price: String,
    pub(crate) qty: String,
    pub(crate) amountu: String,
    pub(crate) fees: String,
    #[serde(rename = "commissionAsset")]
    pub(crate) commission_asset: String,
    #[serde(rename = "realizedPnl")]
    pub(crate) realized_pnl: Option<String>,
    pub(crate) ts: i64,
    pub(crate) ttype: String,
    #[serde(rename = "positionSide")]
    pub(crate) position_side: String,
}

#[derive(Debug)]
pub(crate) struct CashRow {
    pub(crate) record_id: String,
    pub(crate) symbol: Option<String>,
    pub(crate) asset: String,
    pub(crate) amount: String,
    pub(crate) amount_usdt: Option<String>,
    pub(crate) event_time_ms: i64,
    pub(crate) raw: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CashCsvRow<'a> {
    pub(crate) exchange: &'a str,
    pub(crate) account: &'a str,
    pub(crate) symbol: &'a str,
    pub(crate) asset: &'a str,
    pub(crate) amount: &'a str,
    pub(crate) amountu: &'a str,
    #[serde(rename = "type")]
    pub(crate) row_type: &'static str,
    pub(crate) record_id: &'a str,
    pub(crate) ts: i64,
    pub(crate) dt_utc: String,
    pub(crate) dt_bj: String,
    pub(crate) raw: &'a str,
}

pub(crate) fn selected_datasets(dataset: Dataset) -> Vec<Dataset> {
    match dataset {
        Dataset::All => vec![Dataset::Trades, Dataset::Funding, Dataset::Interest],
        value => vec![value],
    }
}

pub(crate) fn validate_time_range(start_ms: Option<i64>, end_ms: Option<i64>) -> Result<()> {
    if start_ms.is_some_and(|value| value < 0) || end_ms.is_some_and(|value| value < 0) {
        bail!("start_ms and end_ms must be non-negative");
    }
    if let (Some(start), Some(end)) = (start_ms, end_ms)
        && start > end
    {
        bail!("start_ms {start} is later than end_ms {end}");
    }
    Ok(())
}

pub(crate) fn validate_timestamp(timestamp: i64, dataset: &str, id: &str) -> Result<()> {
    if DateTime::<Utc>::from_timestamp_millis(timestamp).is_none() {
        bail!("invalid {dataset} timestamp {timestamp} for record {id}");
    }
    Ok(())
}

pub(crate) fn strategy_output_dir(root: &Path, slug: &str) -> Result<PathBuf> {
    let mut components = Path::new(slug).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("strategy slug is not a safe output directory name: {slug}");
    }
    Ok(root.join(slug))
}

pub(crate) fn normalize_account(alias: &str) -> String {
    alias.split_whitespace().collect::<Vec<_>>().join("_")
}

pub(crate) fn market_sid(market: &str) -> i16 {
    let market = market.to_ascii_lowercase();
    if market == "spot" || market == "margin" || market.ends_with("spot") {
        1
    } else {
        0
    }
}

pub(crate) fn trade_key(exchange: &str, sid: i16) -> String {
    let exchange = if exchange == "okex" { "okx" } else { exchange };
    format!("{exchange}{}", if sid == 1 { "spot" } else { "swap" })
}

pub(crate) fn stable_asset(asset: &str) -> bool {
    matches!(asset, "USD" | "USDC" | "USDT")
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strategy(exchange: &str, class: StrategyClass) -> Strategy {
        Strategy {
            slug: "example-strategy".to_string(),
            schema: "example_strategy".to_string(),
            exchange: exchange.to_string(),
            account: "example_account".to_string(),
            class,
        }
    }

    #[test]
    fn all_selects_each_history_dataset() {
        assert_eq!(
            selected_datasets(Dataset::All),
            vec![Dataset::Trades, Dataset::Funding, Dataset::Interest]
        );
    }

    #[test]
    fn exports_stored_interest_when_available() {
        assert!(strategy("binance", StrategyClass::Intra).supports(Dataset::Interest));
        assert!(!strategy("bybit", StrategyClass::Mm).supports(Dataset::Interest));
        assert!(strategy("gate", StrategyClass::Fr).supports(Dataset::Interest));
    }

    #[test]
    fn strategy_output_is_isolated_below_data_root() {
        assert_eq!(
            strategy_output_dir(Path::new("data"), "binance_fr_arb01").unwrap(),
            PathBuf::from("data/binance_fr_arb01")
        );
        assert!(strategy_output_dir(Path::new("data"), "../escape").is_err());
    }

    #[test]
    fn normalizes_account_and_trade_keys() {
        assert_eq!(normalize_account("binance nova02"), "binance_nova02");
        assert_eq!(trade_key("gate", market_sid("spot")), "gatespot");
        assert_eq!(trade_key("okex", market_sid("swap")), "okxswap");
    }

    #[test]
    fn validates_identifiers_and_ranges() {
        assert!(valid_identifier("binance_fr_arb01"));
        assert!(!valid_identifier("binance-fr-arb01"));
        assert!(validate_time_range(Some(10), Some(9)).is_err());
        assert!(validate_time_range(Some(9), Some(10)).is_ok());
    }
}
