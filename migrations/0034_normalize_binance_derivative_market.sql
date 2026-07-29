DO $$
DECLARE
    target_schema TEXT;
BEGIN
    FOR target_schema IN
        SELECT DISTINCT db_schema
        FROM strategy_envs
        WHERE exchange = 'binance'
        ORDER BY db_schema
    LOOP
        IF to_regclass(format('%I.trades', target_schema)) IS NULL THEN
            CONTINUE;
        END IF;

        EXECUTE format(
            'DELETE FROM %1$I.trades AS legacy
             USING %1$I.trades AS canonical
             WHERE legacy.market = ''swap''
               AND canonical.market = ''usdm_futures''
               AND legacy.symbol = canonical.symbol
               AND legacy.trade_id = canonical.trade_id',
            target_schema
        );

        EXECUTE format(
            'UPDATE %I.trades
             SET market = ''usdm_futures''
             WHERE market = ''swap''',
            target_schema
        );
    END LOOP;
END;
$$;
