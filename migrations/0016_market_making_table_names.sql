DO $$
DECLARE
    target_schema TEXT;
BEGIN
    FOR target_schema IN
        SELECT db_schema
        FROM strategy_envs
        WHERE strategy_kind = 'market_making'
        ORDER BY db_schema
    LOOP
        IF to_regclass(format('%I.trades', target_schema)) IS NULL
           AND to_regclass(format('%I.trade_fills', target_schema)) IS NOT NULL THEN
            EXECUTE format(
                'ALTER TABLE %I.trade_fills RENAME TO trades',
                target_schema
            );
        END IF;

        IF to_regclass(format('%I.funding', target_schema)) IS NULL
           AND to_regclass(format('%I.funding_fees', target_schema)) IS NOT NULL THEN
            EXECUTE format(
                'ALTER TABLE %I.funding_fees RENAME TO funding',
                target_schema
            );
        END IF;
    END LOOP;
END;
$$;
