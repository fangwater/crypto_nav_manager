use super::model::{
    CashRow, CashStorage, Dataset, Strategy, TradeCsvRow, TradeStorage, market_sid, trade_key,
    valid_identifier, validate_timestamp,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sqlx::{AssertSqlSafe, PgPool};

pub(crate) async fn load_trade_rows(
    pool: &PgPool,
    strategy: &Strategy,
    storage: TradeStorage,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> Result<Vec<TradeCsvRow>> {
    match storage {
        TradeStorage::Liang => {
            let sql = format!(
                "SELECT sid,key,symbol,id::TEXT,\"orderId\"::TEXT,side,price::TEXT,qty::TEXT,\
                 amountu::TEXT,fees::TEXT,\"commissionAsset\",\"realizedPnl\"::TEXT,ts,ttype,\
                 \"positionSide\" FROM {}.trades {} ORDER BY ts,key,symbol,id",
                strategy.schema,
                time_filter("ts")?
            );
            let rows = sqlx::query_as::<
                _,
                (
                    i16,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    Option<String>,
                    i64,
                    String,
                    String,
                ),
            >(AssertSqlSafe(sql.as_str()))
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all(pool)
            .await
            .with_context(|| format!("load Liang trades from {}.trades", strategy.schema))?;
            rows.into_iter()
                .map(|row| {
                    validate_timestamp(row.12, "trade", &row.3)?;
                    Ok(TradeCsvRow {
                        sid: row.0,
                        key: row.1,
                        symbol: row.2,
                        id: row.3,
                        order_id: row.4,
                        side: row.5,
                        price: row.6,
                        qty: row.7,
                        amountu: row.8,
                        fees: row.9,
                        commission_asset: row.10,
                        realized_pnl: row.11,
                        ts: row.12,
                        ttype: row.13,
                        position_side: row.14,
                    })
                })
                .collect()
        }
        TradeStorage::Generic(table) => {
            if !valid_identifier(&table) {
                bail!("invalid trade table name: {table}");
            }
            let sql = format!(
                "SELECT market,symbol,trade_id,order_id,side,liquidity_role,price::TEXT,\
                 quantity::TEXT,COALESCE(quote_quantity,price*quantity)::TEXT,\
                 COALESCE(fee_amount,0)::TEXT,COALESCE(fee_asset,'USDT'),realized_pnl::TEXT,\
                 event_time_ms FROM {}.{} {} ORDER BY event_time_ms,market,symbol,trade_id",
                strategy.schema,
                table,
                time_filter("event_time_ms")?
            );
            let rows = sqlx::query_as::<
                _,
                (
                    String,
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    String,
                    String,
                    String,
                    String,
                    String,
                    Option<String>,
                    i64,
                ),
            >(AssertSqlSafe(sql.as_str()))
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all(pool)
            .await
            .with_context(|| format!("load generic trades from {}.{table}", strategy.schema))?;
            rows.into_iter()
                .map(|row| {
                    validate_timestamp(row.12, "trade", &row.2)?;
                    let sid = market_sid(&row.0);
                    Ok(TradeCsvRow {
                        sid,
                        key: trade_key(&strategy.exchange, sid),
                        symbol: row.1,
                        id: row.2,
                        order_id: row.3.unwrap_or_default(),
                        side: row.4.unwrap_or_default(),
                        price: row.6,
                        qty: row.7,
                        amountu: row.8,
                        fees: row.9,
                        commission_asset: row.10,
                        realized_pnl: row.11,
                        ts: row.12,
                        ttype: row.5.unwrap_or_default(),
                        position_side: "BOTH".to_string(),
                    })
                })
                .collect()
        }
    }
}

pub(crate) async fn load_cash_rows(
    pool: &PgPool,
    schema: &str,
    dataset: Dataset,
    storage: CashStorage,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> Result<Vec<CashRow>> {
    let (sql, source) = match storage {
        CashStorage::Binance(table) if dataset == Dataset::Funding => (
            format!(
                "SELECT \"tranId\"::TEXT,symbol,'USDT'::TEXT,income,NULL::TEXT,time,\
                 NULL::JSONB FROM {schema}.{table} {} ORDER BY time,\"tranId\"",
                time_filter("time")?
            ),
            table,
        ),
        CashStorage::Binance(table) => (
            format!(
                "SELECT \"txId\"::TEXT,NULL::TEXT,asset,interest,NULL::TEXT,\
                 \"interestAccuredTime\",NULL::JSONB FROM {schema}.{table} {} \
                 ORDER BY \"interestAccuredTime\",\"txId\"",
                time_filter("interestAccuredTime")?
            ),
            table,
        ),
        CashStorage::Text(table) if dataset == Dataset::Funding => (
            format!(
                "SELECT id,symbol,'USDT'::TEXT,funding,NULL::TEXT,\"transactionTime\",\
                 NULL::JSONB FROM {schema}.{table} {} ORDER BY \"transactionTime\",id",
                time_filter("transactionTime")?
            ),
            table,
        ),
        CashStorage::Text(table) => (
            format!(
                "SELECT id,NULL::TEXT,currency,interest,NULL::TEXT,\"transactionTime\",\
                 NULL::JSONB FROM {schema}.{table} {} ORDER BY \"transactionTime\",id",
                time_filter("transactionTime")?
            ),
            table,
        ),
        CashStorage::Generic(table) => (
            format!(
                "SELECT record_id,symbol,COALESCE(asset,''),amount::TEXT,amount_usdt::TEXT,\
                 event_time_ms,raw FROM {schema}.{table} {} ORDER BY event_time_ms,record_id",
                time_filter("event_time_ms")?
            ),
            table,
        ),
    };
    if !valid_identifier(schema) || !valid_identifier(&source) {
        bail!("unsafe cash storage identifier");
    }
    let rows = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            String,
            String,
            Option<String>,
            i64,
            Option<Value>,
        ),
    >(AssertSqlSafe(sql.as_str()))
    .bind(start_ms)
    .bind(end_ms)
    .fetch_all(pool)
    .await
    .with_context(|| format!("load {} rows from {schema}.{source}", dataset.name()))?;

    rows.into_iter()
        .map(|row| {
            validate_timestamp(row.5, dataset.name(), &row.0)?;
            let asset = if dataset == Dataset::Funding && row.2.is_empty() {
                "USDT".to_string()
            } else {
                row.2.to_ascii_uppercase()
            };
            Ok(CashRow {
                record_id: row.0,
                symbol: row.1,
                asset,
                amount: row.3,
                amount_usdt: row.4,
                event_time_ms: row.5,
                raw: row.6.map(|value| value.to_string()).unwrap_or_default(),
            })
        })
        .collect()
}

fn time_filter(column: &str) -> Result<String> {
    if !matches!(
        column,
        "ts" | "event_time_ms" | "time" | "interestAccuredTime" | "transactionTime"
    ) {
        bail!("invalid timestamp column: {column}");
    }
    Ok(format!(
        "WHERE ($1::BIGINT IS NULL OR \"{column}\">=$1) \
         AND ($2::BIGINT IS NULL OR \"{column}\"<=$2)"
    ))
}
