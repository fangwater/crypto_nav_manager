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
            'DELETE FROM %1$I.trades AS noncanonical
             USING %1$I.trades AS canonical
             WHERE noncanonical.market = ''usdt_futures''
               AND canonical.market = ''swap''
               AND noncanonical.symbol = canonical.symbol
               AND noncanonical.trade_id = canonical.trade_id',
            target_schema
        );

        EXECUTE format(
            'UPDATE %I.trades
             SET market = ''swap''
             WHERE market = ''usdt_futures''',
            target_schema
        );
    END LOOP;
END;
$$;
