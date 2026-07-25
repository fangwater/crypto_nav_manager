ALTER TABLE history_sync_watermarks
    DROP CONSTRAINT history_sync_watermarks_dataset_check;

ALTER TABLE history_sync_watermarks
    ADD CONSTRAINT history_sync_watermarks_dataset_check
    CHECK (dataset IN ('trades', 'funding', 'interest', 'rebates', 'liquidations'));

DO $$
DECLARE
    target_schema TEXT;
BEGIN
    FOR target_schema IN
        SELECT db_schema
        FROM strategy_envs
        WHERE exchange = 'gate'
          AND strategy_kind = 'funding_rate'
        ORDER BY db_schema
    LOOP
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I.liquidations (
                order_id TEXT NOT NULL,
                symbol TEXT NOT NULL,
                position_size_contracts NUMERIC(38, 18) NOT NULL,
                contract_multiplier NUMERIC(38, 18) NOT NULL,
                position_quantity NUMERIC(38, 18) GENERATED ALWAYS AS
                    (position_size_contracts * contract_multiplier) STORED,
                leverage NUMERIC(38, 18),
                margin NUMERIC(38, 18),
                entry_price NUMERIC(38, 18),
                liquidation_price NUMERIC(38, 18),
                mark_price NUMERIC(38, 18),
                order_price NUMERIC(38, 18),
                fill_price NUMERIC(38, 18),
                remaining_size_contracts NUMERIC(38, 18),
                event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0),
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

COMMENT ON COLUMN history_sync_watermarks.dataset IS
    'History dataset name: trades, funding, interest, rebates, or liquidations';
