use crate::models::TradingFeeRate;
use anyhow::{Result, bail};
use sqlx::{AssertSqlSafe, PgPool};

pub async fn store_trading_fee_rates(
    pool: &PgPool,
    schema: &str,
    rates: &[TradingFeeRate],
) -> Result<u64> {
    if !valid_schema(schema) {
        bail!("invalid PostgreSQL schema name: {schema}");
    }
    if rates.is_empty() {
        return Ok(0);
    }

    let sql = format!(
        r#"
        INSERT INTO {schema}.trading_fee_rates (
            exchange, account_mode, market, instrument,
            maker_rate, taker_rate, fee_tier, fee_group,
            effective_at_ms, raw
        )
        VALUES ($1, $2, $3, $4, $5::text::numeric, $6::text::numeric, $7, $8, $9, $10)
        ON CONFLICT (exchange, market, instrument, fee_group, effective_at_ms)
        DO UPDATE SET
            account_mode = EXCLUDED.account_mode,
            maker_rate = EXCLUDED.maker_rate,
            taker_rate = EXCLUDED.taker_rate,
            fee_tier = EXCLUDED.fee_tier,
            raw = EXCLUDED.raw,
            fetched_at = CURRENT_TIMESTAMP
        "#
    );
    let mut transaction = pool.begin().await?;
    let mut affected = 0;
    for rate in rates {
        affected += sqlx::query(AssertSqlSafe(sql.as_str()))
            .bind(&rate.exchange)
            .bind(&rate.account_mode)
            .bind(&rate.market)
            .bind(&rate.instrument)
            .bind(&rate.maker_rate)
            .bind(&rate.taker_rate)
            .bind(&rate.fee_tier)
            .bind(rate.fee_group.as_deref().unwrap_or(""))
            .bind(rate.effective_at_ms)
            .bind(&rate.raw)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
    }
    transaction.commit().await?;
    Ok(affected)
}

fn valid_schema(schema: &str) -> bool {
    let mut characters = schema.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::valid_schema;

    #[test]
    fn validates_strategy_schema_names() {
        assert!(valid_schema("binance_fr_arb03"));
        assert!(!valid_schema(""));
        assert!(!valid_schema("Binance"));
        assert!(!valid_schema("safe; DROP TABLE strategy_envs"));
    }
}
