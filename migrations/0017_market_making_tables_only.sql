DO $$
DECLARE
    target_schema TEXT;
    target_table TEXT;
    row_count BIGINT;
BEGIN
    FOR target_schema IN
        SELECT db_schema
        FROM strategy_envs
        WHERE strategy_kind = 'market_making'
        ORDER BY db_schema
    LOOP
        FOREACH target_table IN ARRAY ARRAY['borrow_interest', 'trading_fee_rates']
        LOOP
            IF to_regclass(format('%I.%I', target_schema, target_table)) IS NOT NULL THEN
                EXECUTE format(
                    'SELECT COUNT(*) FROM %I.%I',
                    target_schema,
                    target_table
                ) INTO row_count;

                IF row_count > 0 THEN
                    RAISE EXCEPTION '%.% contains % rows; refusing to drop it',
                        target_schema, target_table, row_count;
                END IF;

                EXECUTE format(
                    'DROP TABLE %I.%I',
                    target_schema,
                    target_table
                );
            END IF;
        END LOOP;
    END LOOP;
END;
$$;
