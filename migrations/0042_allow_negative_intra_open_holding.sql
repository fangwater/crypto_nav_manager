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
            'ALTER TABLE %I.intra_orders DROP CONSTRAINT IF EXISTS intra_orders_check',
            target_schema
        );
    END LOOP;
END;
$$;

COMMENT ON COLUMN binance_intra_arb01.intra_orders.holding IS
    'open_uts - cts in microseconds; small negative values are valid when exchange millisecond update time is compared with local microsecond create time.';
