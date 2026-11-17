//! Read-through public premium history for FR. Closed candles are immutable;
//! missing samples are never filled with zero or another venue's values.
use crate::fr_entry_cost::{MINUTE_MS, minute};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Mutex;

type Series = BTreeMap<i64, f64>;
type Cache = BTreeMap<String, Arc<Mutex<Series>>>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

/// Overview reads only existing observations. It never fans out public backfills
/// across all account symbols, or waits behind an in-flight detail backfill.
pub async fn cached(
    pool: &PgPool,
    exchange: &str,
    symbol: &str,
    start: i64,
    end: i64,
) -> Result<Series> {
    let start = minute(start);
    let end = minute(end).min(minute(chrono::Utc::now().timestamp_millis()) - MINUTE_MS);
    let mut values = Series::new();
    if end < start {
        return Ok(values);
    }
    if let Some(cache) = CACHE.get() {
        let shared = cache
            .lock()
            .await
            .get(&format!("{exchange}/{symbol}"))
            .cloned();
        if let Some(shared) = shared {
            if let Ok(known) = shared.try_lock() {
                values.extend(known.range(start..=end).map(|(&t, &v)| (t, v)));
            }
        }
    }
    if let Some(table) = premium_table(exchange) {
        let sql = format!(
            "SELECT open_time_ms,close_rate FROM {table} WHERE symbol=$1 AND interval='1m' AND open_time_ms >= $2 AND open_time_ms <= $3 ORDER BY open_time_ms"
        );
        let rows = sqlx::query_as::<_, (i64, f64)>(sqlx::AssertSqlSafe(sql))
            .bind(symbol)
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await?;
        values.extend(rows.into_iter().filter(|(_, p)| p.is_finite()));
    }
    Ok(values)
}

fn premium_table(exchange: &str) -> Option<&'static str> {
    match exchange {
        "binance" => Some("binance_premium_index_klines"),
        "bybit" => Some("bybit_premium_index_klines"),
        _ => None,
    }
}

pub async fn load(
    pool: &PgPool,
    exchange: &str,
    symbol: &str,
    start: i64,
    end: i64,
    anchors: &[i64],
    entry_windows: Option<&[(i64, i64)]>,
) -> (Series, String, Vec<String>) {
    let source = format!("{exchange}_premium_index_1m_close");
    if !matches!(exchange, "binance" | "bybit" | "gate") {
        return (
            Series::new(),
            "unavailable".into(),
            vec![format!(
                "{exchange} 的 premium 历史适配器尚未验证；可输入明确的参考基差假设进行情景计算。"
            )],
        );
    }
    let start = minute(start);
    let end = minute(end).min(minute(chrono::Utc::now().timestamp_millis()) - MINUTE_MS);
    if end < start {
        return (
            Series::new(),
            source,
            vec!["建仓所在分钟尚未收盘，参考暂不可用。".into()],
        );
    }
    let key = format!("{exchange}/{symbol}");
    let shared = {
        let mut cache = CACHE.get_or_init(|| Mutex::new(Cache::new())).lock().await;
        if cache.len() >= 64 && !cache.contains_key(&key) {
            cache.pop_first();
        }
        cache.entry(key).or_default().clone()
    };
    let mut values = shared.lock().await;
    let mut issues = Vec::new();
    if let Some(table) = premium_table(exchange) {
        let sql = format!(
            "SELECT open_time_ms,close_rate FROM {table} WHERE symbol=$1 AND interval='1m' AND open_time_ms >= $2 AND open_time_ms <= $3 ORDER BY open_time_ms"
        );
        match sqlx::query_as::<_, (i64, f64)>(sqlx::AssertSqlSafe(sql))
            .bind(symbol)
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await
        {
            Ok(rows) => values.extend(rows.into_iter().filter(|(_, p)| p.is_finite())),
            Err(_) => issues.push("已有分钟缓存读取失败，尝试公开历史接口。".into()),
        }
    }
    let client = match Client::builder().timeout(Duration::from_secs(12)).build() {
        Ok(c) => c,
        Err(_) => {
            return (
                values.range(start..=end).map(|(&t, &v)| (t, v)).collect(),
                source,
                vec!["公开数据客户端初始化失败".into()],
            );
        }
    };
    let fetch = async {
        let mut cursor = start;
        let mut requests = 0;
        // Fetch the comparison endpoints before a potentially long historical
        // backfill. A partial backfill must not starve the latest closed minute.
        let mut priority = Vec::new();
        if entry_windows.is_none() {
            priority.push(end);
            for &t in anchors {
                let t = minute(t);
                if t >= start && t <= end && !priority.contains(&t) {
                    priority.push(t);
                }
            }
            if !priority.contains(&start) {
                priority.push(start);
            }
        }
        for t in priority {
            if !values.contains_key(&t) {
                values.extend(fetch_page(&client, exchange, symbol, t, t).await?);
                requests += 1;
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        if let Some(windows) = entry_windows {
            for &(begin, finish) in windows {
                let mut cursor = begin.max(start);
                let finish = finish.min(end);
                while cursor <= finish && requests < 8 {
                    if values.contains_key(&cursor) {
                        cursor += MINUTE_MS;
                        continue;
                    }
                    // Use one full exchange page around a needed minute, so
                    // nearby paired fills share the same public request.
                    let stop = (cursor + 999 * MINUTE_MS).min(end);
                    values.extend(fetch_page(&client, exchange, symbol, cursor, stop).await?);
                    requests += 1;
                    cursor = stop + MINUTE_MS;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                if requests >= 8 {
                    break;
                }
            }
            return Ok::<(), anyhow::Error>(());
        }
        while cursor <= end {
            if values.contains_key(&cursor) {
                cursor += MINUTE_MS;
                continue;
            }
            if requests >= 32 {
                issues
                    .push("本次参考补齐达到 32 页；已获取分钟保留在缓存，刷新可继续补齐。".into());
                break;
            }
            let stop = (cursor + 999 * MINUTE_MS).min(end);
            let page = fetch_page(&client, exchange, symbol, cursor, stop).await?;
            values.extend(page);
            requests += 1;
            cursor = stop + MINUTE_MS;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Ok::<(), anyhow::Error>(())
    };
    let seconds = if entry_windows.is_some() { 8 } else { 40 };
    match tokio::time::timeout(Duration::from_secs(seconds), fetch).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => issues
            .push("公开 premium 历史请求失败；保留已取得分钟，缺口不插值，刷新可重试。".into()),
        Err(_) => issues.push(format!(
            "参考补齐达到 {seconds} 秒；已获取分钟保留在缓存，刷新可继续。"
        )),
    }
    // Bound per-symbol memory while retaining the requested historical window.
    if values.len() > 600_000 {
        values.retain(|&t, _| t >= start && t <= end);
    }
    (
        values.range(start..=end).map(|(&t, &v)| (t, v)).collect(),
        source,
        issues,
    )
}

async fn fetch_page(
    client: &Client,
    exchange: &str,
    symbol: &str,
    start: i64,
    end: i64,
) -> Result<Series> {
    let response = match exchange {
        "binance" => client
            .get("https://fapi.binance.com/fapi/v1/premiumIndexKlines")
            .query(&[
                ("symbol", symbol.to_string()),
                ("interval", "1m".into()),
                ("startTime", start.to_string()),
                ("endTime", (end + 59_999).to_string()),
                ("limit", "1000".into()),
            ]),
        "bybit" => client
            .get("https://api.bybit.com/v5/market/premium-index-price-kline")
            .query(&[
                ("category", "linear".into()),
                ("symbol", symbol.to_string()),
                ("interval", "1".into()),
                ("start", start.to_string()),
                ("end", (end + 59_999).to_string()),
                ("limit", "1000".into()),
            ]),
        "gate" => {
            let base = symbol
                .strip_suffix("USDT")
                .context("Gate premium requires USDT symbol")?;
            client
                .get("https://api.gateio.ws/api/v4/futures/usdt/premium_index")
                .query(&[
                    ("contract", format!("{base}_USDT")),
                    ("interval", "1m".into()),
                    ("from", (start / 1000).to_string()),
                    ("to", (end / 1000 + 59).to_string()),
                ])
        }
        _ => bail!("unsupported premium exchange"),
    }
    .send()
    .await?
    .error_for_status()?
    .json::<Value>()
    .await?;
    parse_page(exchange, symbol, &response, start, end)
}

fn parse_page(exchange: &str, symbol: &str, data: &Value, start: i64, end: i64) -> Result<Series> {
    let rows = if exchange == "bybit" {
        if data["retCode"].as_i64() != Some(0)
            || data["result"]["symbol"].as_str() != Some(symbol)
            || data["result"]["category"].as_str() != Some("linear")
        {
            bail!("Bybit premium market/status mismatch");
        }
        data["result"]["list"].as_array()
    } else {
        data.as_array()
    }
    .context("invalid premium array")?;
    let mut result = Series::new();
    for row in rows {
        let (time, value) = if exchange == "gate" {
            (&row["t"], &row["c"])
        } else {
            (&row[0], &row[4])
        };
        let ts = time
            .as_i64()
            .or_else(|| time.as_str()?.parse().ok())
            .context("invalid premium time")?;
        let ts = if exchange == "gate" {
            ts.checked_mul(1000).context("premium timestamp overflow")?
        } else {
            ts
        };
        let value: f64 = value
            .as_str()
            .and_then(|v| v.parse().ok())
            .or_else(|| value.as_f64())
            .context("invalid premium value")?;
        if !value.is_finite() || ts % MINUTE_MS != 0 {
            bail!("invalid premium candle");
        }
        if ts >= start && ts <= end {
            if result.insert(ts, value).is_some() {
                bail!("duplicate premium minute");
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_exchange_timestamps_without_scaling_rates() {
        let bn = parse_page(
            "binance",
            "BTCUSDT",
            &json!([[60000, "0", "0", "0", "-0.001"]]),
            0,
            60000,
        )
        .unwrap();
        let gate =
            parse_page("gate", "BTCUSDT", &json!([{"t":60,"c":"-0.001"}]), 0, 60000).unwrap();
        assert_eq!(bn, gate);
        assert_eq!(bn[&60000], -0.001);
        let by = parse_page("bybit", "BTCUSDT", &json!({"retCode":0,"result":{"symbol":"BTCUSDT","category":"linear","list":[["60000","0","0","0","-0.001"],["0","0","0","0","0"]]}}), 60000, 60000).unwrap();
        assert_eq!(by, bn);
    }

    #[test]
    fn rejects_errors_malformed_rows_and_duplicate_minutes() {
        assert!(parse_page("bybit", "BTCUSDT", &json!({"retCode":1}), 0, 60000).is_err());
        assert!(parse_page("binance", "BTCUSDT", &json!([[0]]), 0, 60000).is_err());
        assert!(parse_page("gate", "BTCUSDT", &json!([{"t":0,"c":"NaN"}]), 0, 60000).is_err());
        assert!(
            parse_page(
                "gate",
                "BTCUSDT",
                &json!([{"t":0,"c":"0"},{"t":0,"c":"1"}]),
                0,
                60000
            )
            .is_err()
        );
    }
}
