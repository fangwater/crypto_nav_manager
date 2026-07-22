use crate::{
    rest_dispatcher::{Dispatcher, DispatcherConfig, RequestSpec},
    rest_ip_pool::exchange_local_ips,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::{
    Method,
    header::{HeaderName, HeaderValue},
};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use std::{collections::HashMap, time::Duration};
use tracing::{error, info};

const MARKET: &str = "usdt_futures";
const GATE_CONTRACTS_URL: &str = "https://api.gateio.ws/api/v4/futures/usdt/contracts";
const OKX_INSTRUMENTS_URL: &str = "https://www.okx.com/api/v5/public/instruments?instType=SWAP";
const BATCH_SIZE: usize = 500;

#[derive(Clone, Debug)]
struct ContractSpec {
    exchange: &'static str,
    instrument: String,
    symbol: String,
    base_asset: String,
    quote_asset: String,
    contract_value: String,
    contract_factor: String,
    status: Option<String>,
    raw: Value,
}

#[derive(Clone, Copy, Debug)]
struct MultiplierSnapshot {
    effective_at_ms: i64,
    multiplier: f64,
}

#[derive(Debug, FromRow)]
struct MultiplierRow {
    symbol: String,
    contract_multiplier: f64,
    effective_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct ContractMultiplierBook {
    exchange: String,
    by_symbol: HashMap<String, Vec<MultiplierSnapshot>>,
}

impl ContractMultiplierBook {
    pub async fn load(pool: &PgPool, exchange: &str) -> Result<Self> {
        if !matches!(exchange, "gate" | "okx") {
            bail!("contract multipliers are not supported for exchange {exchange}");
        }

        let rows = sqlx::query_as::<_, MultiplierRow>(
            r#"SELECT symbol, contract_multiplier::float8 AS contract_multiplier,
                      effective_at_ms
               FROM contract_multipliers
               WHERE exchange = $1 AND market = $2
               ORDER BY symbol, effective_at_ms"#,
        )
        .bind(exchange)
        .bind(MARKET)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load {exchange} contract multipliers"))?;
        Self::from_rows(exchange, rows)
    }

    fn from_rows(exchange: &str, rows: Vec<MultiplierRow>) -> Result<Self> {
        if rows.is_empty() {
            bail!("no contract multipliers found for {exchange}/{MARKET}");
        }

        let mut by_symbol: HashMap<String, Vec<MultiplierSnapshot>> = HashMap::new();
        for row in rows {
            if !row.contract_multiplier.is_finite() || row.contract_multiplier <= 0.0 {
                bail!(
                    "invalid {exchange} contract multiplier for {} at {}: {}",
                    row.symbol,
                    row.effective_at_ms,
                    row.contract_multiplier
                );
            }
            let snapshots = by_symbol
                .entry(row.symbol.to_ascii_uppercase())
                .or_default();
            if let Some(previous) = snapshots.last()
                && previous.effective_at_ms == row.effective_at_ms
            {
                if previous.multiplier != row.contract_multiplier {
                    bail!(
                        "conflicting {exchange} contract multipliers for {} at {}",
                        row.symbol,
                        row.effective_at_ms
                    );
                }
                continue;
            }
            snapshots.push(MultiplierSnapshot {
                effective_at_ms: row.effective_at_ms,
                multiplier: row.contract_multiplier,
            });
        }

        Ok(Self {
            exchange: exchange.to_string(),
            by_symbol,
        })
    }

    pub fn multiplier_at(&self, symbol: &str, trade_ts: i64) -> Result<f64> {
        let symbol = symbol.to_ascii_uppercase();
        let snapshots = self.by_symbol.get(&symbol).with_context(|| {
            format!(
                "missing {} contract multiplier for {symbol} at {trade_ts}",
                self.exchange
            )
        })?;
        let index = snapshots.partition_point(|snapshot| snapshot.effective_at_ms <= trade_ts);
        let snapshot = if index == 0 {
            snapshots.first()
        } else {
            snapshots.get(index - 1)
        }
        .with_context(|| {
            format!(
                "missing {} contract multiplier snapshot for {symbol} at {trade_ts}",
                self.exchange
            )
        })?;
        Ok(snapshot.multiplier)
    }
}

pub fn spawn(pool: PgPool) {
    tokio::spawn(async move {
        loop {
            refresh_cycle(&pool).await;
            let delay = until_next_utc_midnight();
            info!(
                delay_seconds = delay.as_secs(),
                "next contract multiplier refresh scheduled for UTC midnight"
            );
            tokio::time::sleep(delay).await;
        }
    });
}

async fn refresh_cycle(pool: &PgPool) {
    let effective_at_ms = utc_day_start_ms();
    for exchange in ["gate", "okx"] {
        match refresh_exchange(pool, exchange, effective_at_ms).await {
            Ok((fetched, upserted)) => info!(
                exchange,
                fetched, upserted, effective_at_ms, "contract multipliers refreshed"
            ),
            Err(error) => error!(
                exchange,
                error = ?error,
                "contract multiplier refresh failed"
            ),
        }
    }
}

async fn refresh_exchange(
    pool: &PgPool,
    exchange: &'static str,
    effective_at_ms: i64,
) -> Result<(usize, u64)> {
    let local_ips = exchange_local_ips(pool, exchange)
        .await
        .with_context(|| format!("select {exchange} REST source IPs"))?;
    info!(exchange, ?local_ips, "selected contract REST source IPs");

    let dispatcher = Dispatcher::new(DispatcherConfig {
        local_ips,
        request_timeout: Duration::from_secs(30),
        ..DispatcherConfig::default()
    })
    .with_context(|| format!("create {exchange} REST dispatcher"))?;

    let rows = match exchange {
        "gate" => parse_gate_contracts(fetch_gate_contracts(&dispatcher).await?)?,
        "okx" => parse_okx_contracts(fetch_okx_contracts(&dispatcher).await?)?,
        _ => bail!("unsupported contract multiplier exchange: {exchange}"),
    };
    if rows.is_empty() {
        bail!("{exchange} contract endpoint returned no supported USDT contracts");
    }

    let fetched = rows.len();
    let upserted = upsert_specs(pool, &rows, effective_at_ms).await?;
    Ok((fetched, upserted))
}

async fn fetch_gate_contracts(dispatcher: &Dispatcher) -> Result<Value> {
    let mut request = RequestSpec::new(Method::GET, GATE_CONTRACTS_URL);
    request.headers.insert(
        HeaderName::from_static("x-gate-size-decimal"),
        HeaderValue::from_static("1"),
    );
    fetch_json(dispatcher, "gate", request).await
}

async fn fetch_okx_contracts(dispatcher: &Dispatcher) -> Result<Value> {
    fetch_json(
        dispatcher,
        "okx",
        RequestSpec::new(Method::GET, OKX_INSTRUMENTS_URL),
    )
    .await
}

async fn fetch_json(
    dispatcher: &Dispatcher,
    exchange: &'static str,
    request: RequestSpec,
) -> Result<Value> {
    let dispatched = dispatcher
        .dispatch(request)
        .await
        .with_context(|| format!("dispatch {exchange} contract request"))?;
    let status = dispatched.response.status();
    let body = dispatched
        .response
        .text()
        .await
        .with_context(|| format!("read {exchange} contract response"))?;
    if !status.is_success() {
        bail!(
            "{exchange} contract endpoint returned HTTP {status}: {}",
            truncate(&body, 500)
        );
    }
    serde_json::from_str(&body).with_context(|| {
        format!(
            "parse {exchange} contract response: {}",
            truncate(&body, 500)
        )
    })
}

fn parse_gate_contracts(value: Value) -> Result<Vec<ContractSpec>> {
    let contracts = value
        .as_array()
        .context("Gate contracts response is not an array")?;
    let mut rows = Vec::with_capacity(contracts.len());
    for raw in contracts {
        let instrument = required_text(raw, "name", "Gate contract")?;
        let (base_asset, quote_asset) = instrument
            .split_once('_')
            .with_context(|| format!("Gate contract has invalid name: {instrument}"))?;
        if !quote_asset.eq_ignore_ascii_case("USDT") {
            continue;
        }
        let base_asset = base_asset.to_ascii_uppercase();
        let quote_asset = quote_asset.to_ascii_uppercase();
        let contract_value = required_positive_numeric(raw, "quanto_multiplier", &instrument)?;
        rows.push(ContractSpec {
            exchange: "gate",
            symbol: normalize_symbol(&instrument)?,
            instrument,
            base_asset,
            quote_asset,
            contract_value,
            contract_factor: "1".to_string(),
            status: optional_text(raw, "status"),
            raw: raw.clone(),
        });
    }
    Ok(rows)
}

fn parse_okx_contracts(value: Value) -> Result<Vec<ContractSpec>> {
    let code = required_text(&value, "code", "OKX response")?;
    if code != "0" {
        bail!(
            "OKX contract API error {code}: {}",
            optional_text(&value, "msg").unwrap_or_else(|| "unknown error".to_string())
        );
    }
    let contracts = value
        .get("data")
        .and_then(Value::as_array)
        .context("OKX contracts response is missing data array")?;
    let mut rows = Vec::with_capacity(contracts.len());
    for raw in contracts {
        if optional_text(raw, "ctType").as_deref() != Some("linear")
            || optional_text(raw, "settleCcy").as_deref() != Some("USDT")
        {
            continue;
        }

        let instrument = required_text(raw, "instId", "OKX contract")?;
        let base_asset = instrument
            .split('-')
            .next()
            .filter(|value| !value.is_empty())
            .with_context(|| format!("OKX contract has invalid instId: {instrument}"))?
            .to_ascii_uppercase();
        let contract_value = required_positive_numeric(raw, "ctVal", &instrument)?;
        let contract_factor = match optional_text(raw, "ctMult") {
            Some(_) => required_positive_numeric(raw, "ctMult", &instrument)?,
            None => "1".to_string(),
        };

        rows.push(ContractSpec {
            exchange: "okx",
            symbol: normalize_symbol(instrument.trim_end_matches("-SWAP"))?,
            instrument,
            base_asset,
            quote_asset: "USDT".to_string(),
            contract_value,
            contract_factor,
            status: optional_text(raw, "state"),
            raw: raw.clone(),
        });
    }
    Ok(rows)
}

async fn upsert_specs(pool: &PgPool, rows: &[ContractSpec], effective_at_ms: i64) -> Result<u64> {
    let mut transaction = pool
        .begin()
        .await
        .context("begin contract multiplier transaction")?;
    let mut affected = 0;
    for batch in rows.chunks(BATCH_SIZE) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO contract_multipliers (
                exchange, market, instrument, symbol, base_asset, quote_asset,
                contract_value, contract_factor, status, effective_at_ms, raw
            ) ",
        );
        query.push_values(batch, |mut values, row| {
            values
                .push_bind(row.exchange)
                .push_bind(MARKET)
                .push_bind(&row.instrument)
                .push_bind(&row.symbol)
                .push_bind(&row.base_asset)
                .push_bind(&row.quote_asset)
                .push_bind(&row.contract_value)
                .push_unseparated("::numeric")
                .push_bind(&row.contract_factor)
                .push_unseparated("::numeric")
                .push_bind(&row.status)
                .push_bind(effective_at_ms)
                .push_bind(&row.raw);
        });
        query.push(
            " ON CONFLICT (exchange, market, instrument, effective_at_ms)
              DO UPDATE SET
                symbol = EXCLUDED.symbol,
                base_asset = EXCLUDED.base_asset,
                quote_asset = EXCLUDED.quote_asset,
                contract_value = EXCLUDED.contract_value,
                contract_factor = EXCLUDED.contract_factor,
                status = EXCLUDED.status,
                raw = EXCLUDED.raw,
                fetched_at = CURRENT_TIMESTAMP",
        );
        affected += query
            .build()
            .execute(&mut *transaction)
            .await
            .context("upsert contract multiplier batch")?
            .rows_affected();
    }
    transaction
        .commit()
        .await
        .context("commit contract multiplier transaction")?;
    Ok(affected)
}

fn required_text(value: &Value, key: &str, context: &str) -> Result<String> {
    value
        .get(key)
        .and_then(value_text)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{context} is missing {key}"))
}

fn optional_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(value_text)
        .filter(|value| !value.is_empty())
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn required_positive_numeric(value: &Value, key: &str, instrument: &str) -> Result<String> {
    let text = required_text(value, key, instrument)?;
    let number = text
        .parse::<f64>()
        .with_context(|| format!("{instrument} has invalid {key}: {text}"))?;
    if !number.is_finite() || number <= 0.0 {
        bail!("{instrument} has non-positive {key}: {text}");
    }
    Ok(text)
}

fn normalize_symbol(value: &str) -> Result<String> {
    let symbol = value
        .trim()
        .to_ascii_uppercase()
        .replace(['-', '_', '/'], "");
    if symbol.is_empty() || !symbol.chars().all(|character| character.is_alphanumeric()) {
        bail!("invalid normalized symbol from {value}");
    }
    Ok(symbol)
}

fn utc_day_start_ms() -> i64 {
    Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp_millis()
}

fn until_next_utc_midnight() -> Duration {
    let now = Utc::now();
    let next = now
        .date_naive()
        .succ_opt()
        .expect("next UTC date exists")
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc();
    (next - now)
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(1))
        .max(Duration::from_secs(1))
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_gate_quanto_multiplier() {
        let rows = parse_gate_contracts(json!([
            {
                "name": "BTC_USDT",
                "quanto_multiplier": "0.0001",
                "status": "trading"
            }
        ]))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, "BTCUSDT");
        assert_eq!(rows[0].contract_value, "0.0001");
        assert_eq!(rows[0].contract_factor, "1");
    }

    #[test]
    fn parses_only_okx_linear_usdt_swaps() {
        let rows = parse_okx_contracts(json!({
            "code": "0",
            "msg": "",
            "data": [
                {
                    "instId": "BTC-USDT-SWAP",
                    "ctType": "linear",
                    "settleCcy": "USDT",
                    "ctVal": "0.01",
                    "ctMult": "10",
                    "state": "live"
                },
                {
                    "instId": "BTC-USD-SWAP",
                    "ctType": "inverse",
                    "settleCcy": "BTC",
                    "ctVal": "100",
                    "ctMult": "1"
                }
            ]
        }))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, "BTCUSDT");
        assert_eq!(rows[0].contract_value, "0.01");
        assert_eq!(rows[0].contract_factor, "10");
    }

    #[test]
    fn normalizes_exchange_instrument_symbols() {
        assert_eq!(normalize_symbol("BTC_USDT").unwrap(), "BTCUSDT");
        assert_eq!(normalize_symbol("BTC-USDT").unwrap(), "BTCUSDT");
        assert_eq!(normalize_symbol("币安人生_USDT").unwrap(), "币安人生USDT");
        assert!(normalize_symbol("BTC;DROP").is_err());
    }

    #[test]
    fn selects_multiplier_snapshot_for_trade_time() {
        let book = ContractMultiplierBook::from_rows(
            "gate",
            vec![
                MultiplierRow {
                    symbol: "BTCUSDT".to_string(),
                    contract_multiplier: 0.001,
                    effective_at_ms: 2_000,
                },
                MultiplierRow {
                    symbol: "BTCUSDT".to_string(),
                    contract_multiplier: 0.01,
                    effective_at_ms: 3_000,
                },
            ],
        )
        .unwrap();

        assert_eq!(book.multiplier_at("btcusdt", 1_000).unwrap(), 0.001);
        assert_eq!(book.multiplier_at("BTCUSDT", 2_500).unwrap(), 0.001);
        assert_eq!(book.multiplier_at("BTCUSDT", 3_500).unwrap(), 0.01);
        assert!(book.multiplier_at("ETHUSDT", 3_500).is_err());
    }

    #[test]
    fn rejects_missing_exchange_multiplier_rows() {
        assert!(ContractMultiplierBook::from_rows("okx", Vec::new()).is_err());
    }
}
