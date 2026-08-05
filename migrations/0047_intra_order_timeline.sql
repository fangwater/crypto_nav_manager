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
             ADD COLUMN IF NOT EXISTS mkt_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS mkt_source_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS new_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS new_source_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS terminal_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS terminal_ts_local_us BIGINT,
             ADD COLUMN IF NOT EXISTS terminal_source_ts_us BIGINT',
            target_schema
        );
        EXECUTE format(
            'ALTER TABLE %I.intra_orders
             ADD COLUMN IF NOT EXISTS open_mkt_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS open_new_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS open_terminal_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS open_terminal_ts_local_us BIGINT,
             ADD COLUMN IF NOT EXISTS hedge_new_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS hedge_terminal_ts_us BIGINT',
            target_schema
        );
        EXECUTE format(
            'ALTER TABLE %I.intra_hedges
             ADD COLUMN IF NOT EXISTS new_ts_us BIGINT,
             ADD COLUMN IF NOT EXISTS terminal_ts_us BIGINT',
            target_schema
        );
    END LOOP;
END;
$$;

COMMENT ON COLUMN binance_intra_arb01.intra_orders.open_mkt_ts_us IS
    'First positive market event timestamp from the Margin order lifecycle, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.cts IS
    'First positive local create timestamp for the Margin order, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.open_new_ts_us IS
    'Exchange update timestamp from the first NEW event for the Margin order, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.open_terminal_ts_us IS
    'Exchange update timestamp from the first terminal event for the Margin order, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.open_terminal_ts_local_us IS
    'Local receive timestamp paired with the first terminal event for the Margin order, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.hts IS
    'Local create timestamp for the earliest mapped Futures hedge order, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.hedge_new_ts_us IS
    'Exchange update timestamp from the first NEW event of the earliest mapped Futures hedge order, in microseconds.';
COMMENT ON COLUMN binance_intra_arb01.intra_orders.hedge_terminal_ts_us IS
    'Exchange update timestamp from the first terminal event of the earliest mapped Futures hedge order, in microseconds.';
