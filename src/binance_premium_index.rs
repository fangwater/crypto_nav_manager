use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::Client;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use std::time::Duration;
use tracing::{info, warn};

const ENDPOINT: &str = "https://fapi.binance.com/fapi/v1/premiumIndexKlines";
const INTERVAL: &str = "1m";
const INTERVAL_MS: i64 = 60_000;
const PAGE_LIMIT: usize = 1_000;
const REQUEST_PAUSE: Duration = Duration::from_millis(250);
const SYNC_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

type BinanceKline = (
    i64,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    i64,
    String,
    String,
    String,
);

#[derive(Clone, Debug, PartialEq)]
struct PremiumKline {
    symbol: String,
    open_time_ms: i64,
    close_time_ms: i64,
    open_rate: f64,
    high_rate: f64,
    low_rate: f64,
    close_rate: f64,
    sample_count: i64,
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
                warn!(error = ?error, "build Binance premium-index client failed");
                return;
            }
        };
        loop {
            if let Err(error) = sync_cycle(&pool, &client).await {
                warn!(error = ?error, "Binance premium-index sync cycle failed");
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
                    "sync Binance premium-index symbol failed"
                );
            }
        }
    }

    info!(
        symbols = symbols.len(),
        failed_symbols,
        upserted_rows = total_rows,
        complete_through_ms,
        "Binance premium-index sync complete"
    );
    Ok(())
}

async fn load_traded_symbols(pool: &PgPool) -> Result<Vec<TradedSymbol>> {
    sqlx::query_as::<_, TradedSymbol>(
        r#"SELECT upper(symbol) AS symbol,
                  min(COALESCE(fts, open_uts) / 1000)::bigint AS first_trade_ms
           FROM binance_intra_arb01.intra_orders
           WHERE cprice IS NOT NULL AND camount > 1e-10
           GROUP BY upper(symbol)
           ORDER BY upper(symbol)"#,
    )
    .fetch_all(pool)
    .await
    .context("load Binance intra symbols for premium-index sync")
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
           FROM binance_premium_index_klines
           WHERE symbol = $1 AND interval = $2"#,
    )
    .bind(&traded.symbol)
    .bind(INTERVAL)
    .fetch_one(pool)
    .await
    .with_context(|| format!("load stored premium-index range for {}", traded.symbol))?;

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
            bail!(
                "Binance premium-index returned no rows for {symbol} in {cursor}..={page_end_ms}"
            );
        }
        let last_open_ms = rows
            .last()
            .expect("non-empty premium-index page")
            .open_time_ms;
        if last_open_ms < cursor || last_open_ms > page_end_ms {
            bail!("Binance premium-index page for {symbol} ended at invalid time {last_open_ms}");
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
            ("symbol", symbol.to_string()),
            ("interval", INTERVAL.to_string()),
            ("startTime", start_ms.to_string()),
            (
                "endTime",
                end_ms.saturating_add(INTERVAL_MS - 1).to_string(),
            ),
            ("limit", PAGE_LIMIT.to_string()),
        ])
        .send()
        .await
        .with_context(|| format!("request Binance premium-index for {symbol}"))?
        .error_for_status()
        .with_context(|| format!("Binance premium-index status for {symbol}"))?;
    let wire = response
        .json::<Vec<BinanceKline>>()
        .await
        .with_context(|| format!("decode Binance premium-index for {symbol}"))?;
    wire.into_iter()
        .map(|row| parse_kline(symbol, row))
        .collect()
}

fn parse_kline(symbol: &str, row: BinanceKline) -> Result<PremiumKline> {
    let (
        open_time_ms,
        open_rate,
        high_rate,
        low_rate,
        close_rate,
        _,
        close_time_ms,
        _,
        sample_count,
        _,
        _,
        _,
    ) = row;
    let parsed = PremiumKline {
        symbol: symbol.to_ascii_uppercase(),
        open_time_ms,
        close_time_ms,
        open_rate: parse_rate(symbol, open_time_ms, "open", &open_rate)?,
        high_rate: parse_rate(symbol, open_time_ms, "high", &high_rate)?,
        low_rate: parse_rate(symbol, open_time_ms, "low", &low_rate)?,
        close_rate: parse_rate(symbol, open_time_ms, "close", &close_rate)?,
        sample_count,
    };
    if parsed.open_time_ms != minute_open(parsed.open_time_ms)
        || parsed.close_time_ms != parsed.open_time_ms + INTERVAL_MS - 1
        || parsed.sample_count < 0
        || parsed.high_rate < parsed.open_rate
        || parsed.high_rate < parsed.close_rate
        || parsed.low_rate > parsed.open_rate
        || parsed.low_rate > parsed.close_rate
    {
        bail!("invalid Binance premium-index candle for {symbol} at {open_time_ms}");
    }
    Ok(parsed)
}

fn parse_rate(symbol: &str, open_time_ms: i64, field: &str, value: &str) -> Result<f64> {
    let parsed = value.parse::<f64>().with_context(|| {
        format!("parse {symbol} premium-index {field} at {open_time_ms}: {value:?}")
    })?;
    if !parsed.is_finite() {
        bail!("non-finite {symbol} premium-index {field} at {open_time_ms}");
    }
    Ok(parsed)
}

async fn upsert_rows(pool: &PgPool, rows: &[PremiumKline]) -> Result<u64> {
    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO binance_premium_index_klines (symbol,interval,open_time_ms,close_time_ms,\
         open_rate,high_rate,low_rate,close_rate,sample_count,fetched_at) ",
    );
    query.push_values(rows, |mut values, row| {
        values
            .push_bind(&row.symbol)
            .push_bind(INTERVAL)
            .push_bind(row.open_time_ms)
            .push_bind(row.close_time_ms)
            .push_bind(row.open_rate)
            .push_bind(row.high_rate)
            .push_bind(row.low_rate)
            .push_bind(row.close_rate)
            .push_bind(row.sample_count)
            .push("CURRENT_TIMESTAMP");
    });
    query.push(
        " ON CONFLICT (symbol,interval,open_time_ms) DO UPDATE SET \
         close_time_ms=EXCLUDED.close_time_ms,open_rate=EXCLUDED.open_rate,\
         high_rate=EXCLUDED.high_rate,low_rate=EXCLUDED.low_rate,\
         close_rate=EXCLUDED.close_rate,sample_count=EXCLUDED.sample_count,\
         fetched_at=CURRENT_TIMESTAMP",
    );
    Ok(query
        .build()
        .execute(pool)
        .await
        .context("upsert Binance premium-index K-lines")?
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
    fn parses_binance_premium_kline() {
        let row = (
            1_786_235_640_000,
            "0.00000000".to_string(),
            "0.00292603".to_string(),
            "-0.00030659".to_string(),
            "0.00260168".to_string(),
            "0".to_string(),
            1_786_235_699_999,
            "0".to_string(),
            12,
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
        );
        let parsed = parse_kline("iotxusdt", row).unwrap();
        assert_eq!(parsed.symbol, "IOTXUSDT");
        assert_eq!(parsed.sample_count, 12);
        assert!((parsed.high_rate - 0.00292603).abs() < 1e-12);
        assert!((parsed.close_rate - 0.00260168).abs() < 1e-12);
    }

    #[test]
    fn selects_only_the_latest_closed_minute() {
        assert_eq!(
            latest_complete_open_ms(1_786_235_681_892),
            1_786_235_580_000
        );
    }
}
