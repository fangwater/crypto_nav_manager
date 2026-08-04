DO $$
DECLARE
    target_schema TEXT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'binance_intra_arb01',
        'bybit_intra_arb01',
        'bybit_intra_arb02'
    ]
    LOOP
        EXECUTE format(
            'ALTER TABLE %I.intra_orders
             ADD COLUMN IF NOT EXISTS hts BIGINT',
            target_schema
        );
        EXECUTE format(
            'ALTER TABLE %I.intra_hedges
             ADD COLUMN IF NOT EXISTS fill_ts_us BIGINT',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS %I ON %I.intra_hedges
             (main_fkey, create_ts_us)',
            target_schema || '_intra_hedges_main_fkey_idx',
            target_schema
        );
    END LOOP;
END;
$$;

COMMENT ON COLUMN binance_intra_arb01.intra_orders.hts IS
    'Earliest create time among Futures hedge orders allocated to this Margin order, in microseconds.';

COMMENT ON COLUMN binance_intra_arb01.intra_orders.fts IS
    'Latest actual fill event time among allocated Futures hedge orders, in microseconds.';
