DO $$
DECLARE
    target_schema TEXT;
    existing_rows BIGINT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'bybit_intra_arb01',
        'bybit_intra_arb02'
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
                    id TEXT PRIMARY KEY,
                    currency TEXT NOT NULL,
                    interest TEXT NOT NULL,
                    "transactionTime" BIGINT NOT NULL
                        CHECK ("transactionTime" > 0)
                )',
                target_schema
            );
            EXECUTE format(
                'CREATE INDEX %I ON %I.interest ("transactionTime")',
                target_schema || '_interest_time_idx',
                target_schema
            );
        END IF;
    END LOOP;
END;
$$;
