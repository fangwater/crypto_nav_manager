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
            'ALTER TABLE %I.intra_order_lifecycle
             ADD COLUMN IF NOT EXISTS last_fill_update_ts_us BIGINT',
            target_schema
        );
    END LOOP;
END;
$$;

COMMENT ON COLUMN binance_intra_arb01.intra_order_lifecycle.last_fill_update_ts_us IS
    'Latest actual fill event time: any positive fill delta, or a FILLED terminal update, in microseconds.';
