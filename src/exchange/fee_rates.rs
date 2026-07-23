use super::ExchangeError;
use crate::models::TradingFeeRate;
use serde_json::Value;

pub(crate) fn normalize_binance(
    raw: Value,
    account_mode: &str,
    fetched_at_ms: i64,
) -> Result<Vec<TradingFeeRate>, ExchangeError> {
    Ok(vec![TradingFeeRate {
        exchange: "binance".to_string(),
        account_mode: account_mode.to_string(),
        market: "usdt_futures".to_string(),
        instrument: required_string("binance", &raw, "symbol")?,
        maker_rate: required_string("binance", &raw, "makerCommissionRate")?,
        taker_rate: required_string("binance", &raw, "takerCommissionRate")?,
        fee_tier: None,
        fee_group: None,
        effective_at_ms: fetched_at_ms,
        raw,
    }])
}

pub(crate) fn normalize_binance_spot(
    raw: Value,
    account_mode: &str,
    fetched_at_ms: i64,
) -> Result<Vec<TradingFeeRate>, ExchangeError> {
    let standard = raw
        .get("standardCommission")
        .ok_or_else(|| invalid("binance", "spot fee response is missing standardCommission"))?;
    Ok(vec![TradingFeeRate {
        exchange: "binance".to_string(),
        account_mode: account_mode.to_string(),
        market: "spot".to_string(),
        instrument: required_string("binance", &raw, "symbol")?,
        maker_rate: required_string("binance", standard, "maker")?,
        taker_rate: required_string("binance", standard, "taker")?,
        fee_tier: Some("standard_commission".to_string()),
        fee_group: None,
        effective_at_ms: fetched_at_ms,
        raw,
    }])
}

pub(crate) fn normalize_gate(
    raw: Value,
    market: &str,
    instrument: &str,
    fetched_at_ms: i64,
) -> Result<Vec<TradingFeeRate>, ExchangeError> {
    let fee = raw.get(instrument).unwrap_or(&raw);
    let maker_rate = required_string("gate", fee, "maker_fee")?;
    let taker_rate = required_string("gate", fee, "taker_fee")?;
    Ok(vec![TradingFeeRate {
        exchange: "gate".to_string(),
        account_mode: "unified".to_string(),
        market: market.to_string(),
        instrument: instrument.to_string(),
        maker_rate,
        taker_rate,
        fee_tier: None,
        fee_group: None,
        effective_at_ms: fetched_at_ms,
        raw,
    }])
}

pub(crate) fn normalize_bitget(
    raw_rows: Vec<Value>,
    market: &str,
    fetched_at_ms: i64,
) -> Result<Vec<TradingFeeRate>, ExchangeError> {
    raw_rows
        .into_iter()
        .map(|raw| {
            Ok(TradingFeeRate {
                exchange: "bitget".to_string(),
                account_mode: "unified".to_string(),
                market: market.to_string(),
                instrument: required_string("bitget", &raw, "symbol")?,
                maker_rate: required_string("bitget", &raw, "makerFeeRate")?,
                taker_rate: required_string("bitget", &raw, "takerFeeRate")?,
                fee_tier: None,
                fee_group: None,
                effective_at_ms: fetched_at_ms,
                raw,
            })
        })
        .collect()
}

pub(crate) fn normalize_bybit(
    raw_rows: Vec<Value>,
    market: &str,
    instrument: Option<&str>,
    fetched_at_ms: i64,
) -> Result<Vec<TradingFeeRate>, ExchangeError> {
    if raw_rows.is_empty() {
        return Err(invalid("bybit", "fee-rate response contains no fee rates"));
    }

    raw_rows
        .into_iter()
        .map(|raw| {
            let row_instrument = optional_string(&raw, "symbol")
                .or_else(|| optional_string(&raw, "baseCoin"))
                .or_else(|| instrument.map(str::to_string))
                .unwrap_or_else(|| "*".to_string());
            Ok(TradingFeeRate {
                exchange: "bybit".to_string(),
                account_mode: "unified".to_string(),
                market: market.to_string(),
                instrument: row_instrument,
                maker_rate: required_string("bybit", &raw, "makerFeeRate")?,
                taker_rate: required_string("bybit", &raw, "takerFeeRate")?,
                fee_tier: None,
                fee_group: None,
                effective_at_ms: fetched_at_ms,
                raw,
            })
        })
        .collect()
}

pub(crate) fn normalize_okx(
    raw_rows: Vec<Value>,
    market: &str,
    instrument: Option<&str>,
    fetched_at_ms: i64,
) -> Result<Vec<TradingFeeRate>, ExchangeError> {
    let mut normalized = Vec::new();
    for raw in raw_rows {
        let effective_at_ms = optional_i64(&raw, "ts").unwrap_or(fetched_at_ms);
        let fee_tier = optional_string(&raw, "level");
        let groups = raw
            .get("feeGroup")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if groups.is_empty() {
            let (maker, taker) = okx_top_level_rates(&raw, market)?;
            normalized.push(TradingFeeRate {
                exchange: "okx".to_string(),
                account_mode: "unified".to_string(),
                market: market.to_string(),
                instrument: instrument.unwrap_or("*").to_string(),
                maker_rate: invert_rate_sign(&maker),
                taker_rate: invert_rate_sign(&taker),
                fee_tier: fee_tier.clone(),
                fee_group: None,
                effective_at_ms,
                raw: raw.clone(),
            });
            continue;
        }

        for group in groups {
            normalized.push(TradingFeeRate {
                exchange: "okx".to_string(),
                account_mode: "unified".to_string(),
                market: market.to_string(),
                instrument: instrument.unwrap_or("*").to_string(),
                maker_rate: invert_rate_sign(&required_string("okx", &group, "maker")?),
                taker_rate: invert_rate_sign(&required_string("okx", &group, "taker")?),
                fee_tier: fee_tier.clone(),
                fee_group: optional_string(&group, "groupId"),
                effective_at_ms,
                raw: raw.clone(),
            });
        }
    }
    if normalized.is_empty() {
        return Err(invalid("okx", "trade-fee response contains no fee rates"));
    }
    Ok(normalized)
}

fn okx_top_level_rates(raw: &Value, market: &str) -> Result<(String, String), ExchangeError> {
    let key_pairs: &[(&str, &str)] = if matches!(market, "spot" | "margin") {
        &[("maker", "taker")]
    } else {
        &[
            ("makerU", "takerU"),
            ("makerUSDC", "takerUSDC"),
            ("maker", "taker"),
        ]
    };
    key_pairs
        .iter()
        .find_map(|(maker_key, taker_key)| {
            Some((
                optional_string(raw, maker_key)?,
                optional_string(raw, taker_key)?,
            ))
        })
        .ok_or_else(|| invalid("okx", "trade-fee response has no complete maker/taker pair"))
}

fn required_string(
    exchange: &'static str,
    value: &Value,
    key: &str,
) -> Result<String, ExchangeError> {
    optional_string(value, key)
        .ok_or_else(|| invalid(exchange, &format!("fee response is missing {key}")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn invert_rate_sign(rate: &str) -> String {
    if let Some(unsigned) = rate.strip_prefix('-') {
        unsigned.to_string()
    } else if rate.parse::<f64>().is_ok_and(|rate| rate != 0.0) {
        format!("-{rate}")
    } else {
        rate.to_string()
    }
}

fn invalid(exchange: &'static str, message: &str) -> ExchangeError {
    ExchangeError::InvalidResponse {
        exchange,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_binance_rate() {
        let rows = normalize_binance(
            json!({
                "symbol": "BTCUSDT",
                "makerCommissionRate": "0.000100",
                "takerCommissionRate": "0.000300"
            }),
            "portfolio_margin",
            123,
        )
        .unwrap();

        assert_eq!(rows[0].market, "usdt_futures");
        assert_eq!(rows[0].maker_rate, "0.000100");
        assert_eq!(rows[0].effective_at_ms, 123);
    }

    #[test]
    fn normalizes_binance_spot_standard_commission() {
        let rows = normalize_binance_spot(
            json!({
                "symbol": "LINKUSDT",
                "standardCommission": {
                    "maker": "0.00000000",
                    "taker": "0.00023000",
                    "buyer": "0.00000000",
                    "seller": "0.00000000"
                },
                "specialCommission": {"maker": "0", "taker": "0"},
                "taxCommission": {"maker": "0", "taker": "0"},
                "discount": {"enabledForAccount": true, "discount": "0.75"}
            }),
            "usdm_futures",
            123,
        )
        .unwrap();

        assert_eq!(rows[0].market, "spot");
        assert_eq!(rows[0].instrument, "LINKUSDT");
        assert_eq!(rows[0].maker_rate, "0.00000000");
        assert_eq!(rows[0].taker_rate, "0.00023000");
        assert_eq!(rows[0].fee_tier.as_deref(), Some("standard_commission"));
    }

    #[test]
    fn normalizes_gate_market_and_preserves_rebate_sign() {
        let rows = normalize_gate(
            json!({
                "BTC_USDT": {
                    "maker_fee": "-0.000075",
                    "taker_fee": "0.000175"
                }
            }),
            "usdt_futures",
            "BTC_USDT",
            123,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].instrument, "BTC_USDT");
        assert_eq!(rows[0].market, "usdt_futures");
        assert_eq!(rows[0].maker_rate, "-0.000075");
    }

    #[test]
    fn normalizes_bitget_rows() {
        let rows = normalize_bitget(
            vec![json!({
                "symbol": "BTCUSDT",
                "makerFeeRate": "-0.00004",
                "takerFeeRate": "0.00023"
            })],
            "usdt_futures",
            123,
        )
        .unwrap();

        assert_eq!(rows[0].maker_rate, "-0.00004");
        assert_eq!(rows[0].taker_rate, "0.00023");
    }

    #[test]
    fn normalizes_bybit_rate() {
        let rows = normalize_bybit(
            vec![json!({
                "symbol": "BTCUSDT",
                "makerFeeRate": "0.0001",
                "takerFeeRate": "0.0006"
            })],
            "linear",
            None,
            123,
        )
        .unwrap();

        assert_eq!(rows[0].exchange, "bybit");
        assert_eq!(rows[0].account_mode, "unified");
        assert_eq!(rows[0].instrument, "BTCUSDT");
        assert_eq!(rows[0].maker_rate, "0.0001");
        assert_eq!(rows[0].taker_rate, "0.0006");
    }

    #[test]
    fn flattens_okx_groups_and_normalizes_cost_sign() {
        let rows = normalize_okx(
            vec![json!({
                "feeGroup": [{
                    "groupId": "4",
                    "maker": "-0.00015",
                    "taker": "-0.00036"
                }],
                "level": "VIP2",
                "ts": "1784623822499"
            })],
            "usdt_swap",
            Some("BTC-USDT"),
            123,
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].maker_rate, "0.00015");
        assert_eq!(rows[0].taker_rate, "0.00036");
        assert_eq!(rows[0].fee_group.as_deref(), Some("4"));
        assert_eq!(rows[0].effective_at_ms, 1_784_623_822_499);
    }
}
