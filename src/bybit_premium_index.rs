use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use std::time::Duration;
use tracing::{info, warn};

const ENDPOINT: &str = "https://api.bybit.com/v5/market/premium-index-price-kline";
const API_INTERVAL: &str = "1";
const STORED_INTERVAL: &str = "1m";
const INTERVAL_MS: i64 = 60_000;
const PAGE_LIMIT: usize = 1_000;
const REQUEST_PAUSE: Duration = Duration::from_millis(250);
const SYNC_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BybitResponse {
    ret_code: i64,
    ret_msg: String,
    result: BybitResult,
}

#[derive(Debug, Default, Deserialize)]
struct BybitResult {
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    list: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
struct PremiumKline {
    symbol: String,
    open_time_ms: i64,
    close_time_ms: i64,
    open_rate: f64,
    high_rate: f64,
    low_rate: f64,
    close_rate: f64,
}

#[derive(Debug, FromRow)]
struct TradedSymbol {
    symbol: String,
    first_trade_ms: i64,
}

#[derive(Debug, FromRow)]
struct StoredRange {
    first_open_ms: Option<i64>,
    last_open_ms: Option<i64>,
}

pub fn spawn(pool: PgPool) {
    tokio::spawn(async move {
        let client = match Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            Err(error) => {
                warn!(error = ?error, "build Bybit premium-index client failed");
                return;
            }
        };
        loop {
            if let Err(error) = sync_cycle(&pool, &client).await {
                warn!(error = ?error, "Bybit premium-index sync cycle failed");
            }
            tokio::time::sleep(SYNC_INTERVAL).await;
        }
    });
}

async fn sync_cycle(pool: &PgPool, client: &Client) -> Result<()> {
    let symbols = load_traded_symbols(pool).await?;
    let complete_through_ms = latest_complete_open_ms(Utc::now().timestamp_millis());
    let mut total_rows = 0_u64;
    let mut failed_symbols = 0_usize;

    for symbol in &symbols {
        match sync_symbol(pool, client, symbol, complete_through_ms).await {
            Ok(rows) => total_rows += rows,
            Err(error) => {
                failed_symbols += 1;
                warn!(
                    symbol = %symbol.symbol,
                    error = ?error,
                    "sync Bybit premium-index symbol failed"
                );
            }
        }
    }

    info!(
        symbols = symbols.len(),
        failed_symbols,
        upserted_rows = total_rows,
        complete_through_ms,
        "Bybit premium-index sync complete"
    );
    Ok(())
}

async fn load_traded_symbols(pool: &PgPool) -> Result<Vec<TradedSymbol>> {
    sqlx::query_as::<_, TradedSymbol>(
        r#"SELECT upper(symbol) AS symbol,
                  min(COALESCE(fts, open_uts) / 1000)::bigint AS first_trade_ms
           FROM bybit_intra_arb01.intra_orders
           WHERE open_fill_amount > 1e-10
           GROUP BY upper(symbol)
           ORDER BY upper(symbol)"#,
    )
    .fetch_all(pool)
    .await
    .context("load Bybit intra arb01 symbols for premium-index sync")
}

async fn sync_symbol(
    pool: &PgPool,
    client: &Client,
    traded: &TradedSymbol,
    complete_through_ms: i64,
) -> Result<u64> {
    let desired_start_ms = minute_open(traded.first_trade_ms);
    if desired_start_ms > complete_through_ms {
        return Ok(0);
    }
    let stored = sqlx::query_as::<_, StoredRange>(
        r#"SELECT min(open_time_ms) AS first_open_ms,
                  max(open_time_ms) AS last_open_ms
           FROM bybit_premium_index_klines
           WHERE symbol = $1 AND interval = $2"#,
    )
    .bind(&traded.symbol)
    .bind(STORED_INTERVAL)
    .fetch_one(pool)
    .await
    .with_context(|| {
        format!(
            "load stored Bybit premium-index range for {}",
            traded.symbol
        )
    })?;

    let mut upserted = 0_u64;
    if let Some(first_open_ms) = stored.first_open_ms
        && desired_start_ms < first_open_ms
    {
        upserted += sync_range(
            pool,
            client,
            &traded.symbol,
            desired_start_ms,
            first_open_ms - INTERVAL_MS,
        )
        .await?;
    }

    let forward_start_ms = stored
        .last_open_ms
        .map_or(desired_start_ms, |last| last.saturating_add(INTERVAL_MS));
    if forward_start_ms <= complete_through_ms {
        upserted += sync_range(
            pool,
            client,
            &traded.symbol,
            forward_start_ms,
            complete_through_ms,
        )
        .await?;
    }
    Ok(upserted)
}

async fn sync_range(
    pool: &PgPool,
    client: &Client,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<u64> {
    let mut cursor = start_ms;
    let mut upserted = 0_u64;
    while cursor <= end_ms {
        let page_end_ms = cursor
            .saturating_add((PAGE_LIMIT as i64 - 1) * INTERVAL_MS)
            .min(end_ms);
        let rows = fetch_page(client, symbol, cursor, page_end_ms).await?;
        if rows.is_empty() {
            bail!("Bybit premium-index returned no rows for {symbol} in {cursor}..={page_end_ms}");
        }
        let last_open_ms = rows
            .last()
            .expect("non-empty premium-index page")
            .open_time_ms;
        if last_open_ms < cursor || last_open_ms > page_end_ms {
            bail!("Bybit premium-index page for {symbol} ended at invalid time {last_open_ms}");
        }
        upserted += upsert_rows(pool, &rows).await?;
        cursor = last_open_ms.saturating_add(INTERVAL_MS);
        if cursor <= end_ms {
            tokio::time::sleep(REQUEST_PAUSE).await;
        }
    }
    Ok(upserted)
}

async fn fetch_page(
    client: &Client,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<PremiumKline>> {
    let response = client
        .get(ENDPOINT)
        .query(&[
            ("category", "linear".to_string()),
            ("symbol", symbol.to_string()),
            ("interval", API_INTERVAL.to_string()),
            ("start", start_ms.to_string()),
            ("end", end_ms.saturating_add(INTERVAL_MS - 1).to_string()),
            ("limit", PAGE_LIMIT.to_string()),
        ])
        .send()
        .await
        .with_context(|| format!("request Bybit premium-index for {symbol}"))?
        .error_for_status()
        .with_context(|| format!("Bybit premium-index HTTP status for {symbol}"))?;
    let wire = response
        .json::<BybitResponse>()
        .await
        .with_context(|| format!("decode Bybit premium-index for {symbol}"))?;
    parse_response(symbol, wire)
}

fn parse_response(symbol: &str, wire: BybitResponse) -> Result<Vec<PremiumKline>> {
    if wire.ret_code != 0 {
        bail!(
            "Bybit premium-index returned code {} for {symbol}: {}",
            wire.ret_code,
            wire.ret_msg
        );
    }
    if wire.result.category != "linear" || !wire.result.symbol.eq_ignore_ascii_case(symbol) {
        bail!(
            "Bybit premium-index returned mismatched market for {symbol}: {}/{}",
            wire.result.category,
            wire.result.symbol
        );
    }
    let mut rows = wire
        .result
        .list
        .into_iter()
        .map(|row| parse_kline(symbol, row))
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by_key(|row| row.open_time_ms);
    rows.dedup_by_key(|row| row.open_time_ms);
    Ok(rows)
}

fn parse_kline(symbol: &str, row: Vec<String>) -> Result<PremiumKline> {
    let [open_time_ms, open_rate, high_rate, low_rate, close_rate]: [String; 5] = row
        .try_into()
        .map_err(|row: Vec<String>| anyhow::anyhow!("invalid Bybit premium-index row: {row:?}"))?;
    let open_time_ms = open_time_ms
        .parse::<i64>()
        .with_context(|| format!("parse {symbol} Bybit premium-index timestamp"))?;
    let parsed = PremiumKline {
        symbol: symbol.to_ascii_uppercase(),
        open_time_ms,
        close_time_ms: open_time_ms.saturating_add(INTERVAL_MS - 1),
        open_rate: parse_rate(symbol, open_time_ms, "open", &open_rate)?,
        high_rate: parse_rate(symbol, open_time_ms, "high", &high_rate)?,
        low_rate: parse_rate(symbol, open_time_ms, "low", &low_rate)?,
        close_rate: parse_rate(symbol, open_time_ms, "close", &close_rate)?,
    };
    if parsed.open_time_ms != minute_open(parsed.open_time_ms)
        || parsed.high_rate < parsed.open_rate
        || parsed.high_rate < parsed.close_rate
        || parsed.low_rate > parsed.open_rate
        || parsed.low_rate > parsed.close_rate
    {
        bail!("invalid Bybit premium-index candle for {symbol} at {open_time_ms}");
    }
    Ok(parsed)
}

fn parse_rate(symbol: &str, open_time_ms: i64, field: &str, value: &str) -> Result<f64> {
    let parsed = value.parse::<f64>().with_context(|| {
        format!("parse {symbol} Bybit premium-index {field} at {open_time_ms}: {value:?}")
    })?;
    if !parsed.is_finite() {
        bail!("non-finite {symbol} Bybit premium-index {field} at {open_time_ms}");
    }
    Ok(parsed)
}

async fn upsert_rows(pool: &PgPool, rows: &[PremiumKline]) -> Result<u64> {
    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO bybit_premium_index_klines (symbol,interval,open_time_ms,close_time_ms,open_rate,high_rate,low_rate,close_rate,fetched_at) ",
    );
    query.push_values(rows, |mut values, row| {
        values
            .push_bind(&row.symbol)
            .push_bind(STORED_INTERVAL)
            .push_bind(row.open_time_ms)
            .push_bind(row.close_time_ms)
            .push_bind(row.open_rate)
            .push_bind(row.high_rate)
            .push_bind(row.low_rate)
            .push_bind(row.close_rate)
            .push("CURRENT_TIMESTAMP");
    });
    query.push(
        " ON CONFLICT (symbol,interval,open_time_ms) DO UPDATE SET \
         close_time_ms=EXCLUDED.close_time_ms,open_rate=EXCLUDED.open_rate,\
         high_rate=EXCLUDED.high_rate,low_rate=EXCLUDED.low_rate,\
         close_rate=EXCLUDED.close_rate,fetched_at=CURRENT_TIMESTAMP",
    );
    Ok(query
        .build()
        .execute(pool)
        .await
        .context("upsert Bybit premium-index K-lines")?
        .rows_affected())
}

fn minute_open(timestamp_ms: i64) -> i64 {
    timestamp_ms.div_euclid(INTERVAL_MS) * INTERVAL_MS
}

fn latest_complete_open_ms(now_ms: i64) -> i64 {
    minute_open(now_ms).saturating_sub(INTERVAL_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_sorts_bybit_premium_klines() {
        let wire: BybitResponse = serde_json::from_value(serde_json::json!({
            "retCode": 0,
            "retMsg": "OK",
            "result": {
                "symbol": "BTCUSDT",
                "category": "linear",
                "list": [
                    ["1786463160000", "-0.00047060", "-0.00043943", "-0.00047060", "-0.00043943"],
                    ["1786463100000", "-0.00040735", "-0.00040735", "-0.00047060", "-0.00047060"]
                ]
            }
        }))
        .unwrap();
        let parsed = parse_response("btcusdt", wire).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].symbol, "BTCUSDT");
        assert_eq!(parsed[0].open_time_ms, 1_786_463_100_000);
        assert_eq!(parsed[1].open_time_ms, 1_786_463_160_000);
        assert!((parsed[1].close_rate - -0.00043943).abs() < 1e-12);
    }

    #[test]
    fn selects_only_the_latest_closed_minute() {
        assert_eq!(
            latest_complete_open_ms(1_786_235_681_892),
            1_786_235_580_000
        );
    }
}
