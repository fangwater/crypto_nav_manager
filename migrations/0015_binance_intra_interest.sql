DO $$
DECLARE
    target_schema TEXT;
    existing_rows BIGINT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'binance_intra_arb01'
    ]
    LOOP
        IF to_regclass(target_schema || '.interest') IS NULL THEN
            EXECUTE format(
                'SELECT COUNT(*) FROM %I.borrow_interest',
                target_schema
            ) INTO existing_rows;
            IF existing_rows <> 0 THEN
                RAISE EXCEPTION
                    '%.borrow_interest contains % rows; migrate them before replacing the table',
                    target_schema, existing_rows;
            END IF;

            EXECUTE format('DROP TABLE %I.borrow_interest', target_schema);
            EXECUTE format(
                'CREATE TABLE %I.interest (
                    "txId" BIGINT PRIMARY KEY,
                    asset TEXT NOT NULL,
                    interest TEXT NOT NULL,
                    "interestAccuredTime" BIGINT NOT NULL
                        CHECK ("interestAccuredTime" > 0)
                )',
                target_schema
            );
            EXECUTE format(
                'CREATE INDEX %I ON %I.interest ("interestAccuredTime")',
                target_schema || '_interest_time_idx',
                target_schema
            );
        END IF;
    END LOOP;
END;
$$;
