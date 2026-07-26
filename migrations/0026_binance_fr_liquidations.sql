DO $$
DECLARE
    target_schema TEXT;
BEGIN
    FOR target_schema IN
        SELECT db_schema
        FROM strategy_envs
        WHERE exchange = 'binance'
          AND strategy_kind = 'funding_rate'
        ORDER BY db_schema
    LOOP
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I.liquidations (
                order_id TEXT NOT NULL,
                symbol TEXT NOT NULL,
                status TEXT NOT NULL,
                client_order_id TEXT,
                order_price NUMERIC(38, 18),
                average_price NUMERIC(38, 18),
                original_quantity NUMERIC(38, 18) NOT NULL,
                executed_quantity NUMERIC(38, 18) NOT NULL,
                cumulative_quote NUMERIC(38, 18),
                time_in_force TEXT,
                order_type TEXT,
                reduce_only BOOLEAN,
                side TEXT NOT NULL,
                position_side TEXT,
                original_type TEXT,
                event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0),
                update_time_ms BIGINT,
                raw JSONB NOT NULL,
                fetched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (symbol, order_id, event_time_ms)
            )',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS %I ON %I.liquidations (event_time_ms)',
            target_schema || '_liquidations_time_idx',
            target_schema
        );
    END LOOP;
END;
$$;
