ALTER TABLE history_sync_watermarks
    DROP CONSTRAINT history_sync_watermarks_dataset_check;

ALTER TABLE history_sync_watermarks
    ADD CONSTRAINT history_sync_watermarks_dataset_check
    CHECK (dataset IN ('trades', 'funding', 'interest', 'rebates'));

DO $$
DECLARE
    target_schema TEXT;
BEGIN
    FOR target_schema IN
        SELECT db_schema
        FROM strategy_envs
        WHERE exchange = 'binance'
          AND strategy_kind = 'intra_exchange'
        ORDER BY db_schema
    LOOP
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I.rebates (
                record_id TEXT PRIMARY KEY,
                transaction_id TEXT,
                asset TEXT NOT NULL,
                amount NUMERIC(38, 18) NOT NULL,
                amount_usdt NUMERIC(38, 18),
                event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0),
                description TEXT NOT NULL,
                direction SMALLINT,
                raw JSONB NOT NULL,
                fetched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS %I ON %I.rebates (event_time_ms)',
            target_schema || '_rebates_time_idx',
            target_schema
        );
    END LOOP;
END;
$$;

COMMENT ON COLUMN history_sync_watermarks.dataset IS
    'History dataset name: trades, funding, interest, or rebates';
