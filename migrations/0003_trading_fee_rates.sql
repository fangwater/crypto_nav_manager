CREATE OR REPLACE FUNCTION ensure_trading_fee_rate_storage(target_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    IF target_schema !~ '^[a-z][a-z0-9_]*$' THEN
        RAISE EXCEPTION 'invalid strategy schema: %', target_schema;
    END IF;

    EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I', target_schema);

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.trading_fee_rates (
            exchange TEXT NOT NULL,
            account_mode TEXT NOT NULL,
            market TEXT NOT NULL,
            instrument TEXT NOT NULL,
            maker_rate NUMERIC(38, 18) NOT NULL,
            taker_rate NUMERIC(38, 18) NOT NULL,
            fee_tier TEXT,
            fee_group TEXT NOT NULL DEFAULT '''',
            effective_at_ms BIGINT NOT NULL CHECK (effective_at_ms >= 0),
            raw JSONB NOT NULL,
            fetched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (exchange, market, instrument, fee_group, effective_at_ms)
        )',
        target_schema
    );

    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I
            ON %I.trading_fee_rates (effective_at_ms DESC)',
        target_schema || '_trading_fee_rates_time_idx',
        target_schema
    );

    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I
            ON %I.trading_fee_rates (market, instrument, effective_at_ms DESC)',
        target_schema || '_trading_fee_rates_market_time_idx',
        target_schema
    );
END;
$$;

SELECT ensure_trading_fee_rate_storage(db_schema)
FROM strategy_envs
ORDER BY sort_order;

ALTER FUNCTION ensure_strategy_storage(TEXT)
    RENAME TO ensure_strategy_storage_without_trading_fee_rates;

CREATE FUNCTION ensure_strategy_storage(target_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM ensure_strategy_storage_without_trading_fee_rates(target_schema);
    PERFORM ensure_trading_fee_rate_storage(target_schema);
END;
$$;
