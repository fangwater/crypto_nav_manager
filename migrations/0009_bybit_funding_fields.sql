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
        IF to_regclass(target_schema || '.funding') IS NULL THEN
            EXECUTE format(
                'SELECT COUNT(*) FROM %I.funding_fees',
                target_schema
            ) INTO existing_rows;
            IF existing_rows <> 0 THEN
                RAISE EXCEPTION
                    '%.funding_fees contains % rows; migrate them before replacing the table',
                    target_schema, existing_rows;
            END IF;

            EXECUTE format('DROP TABLE %I.funding_fees', target_schema);
            EXECUTE format(
                'CREATE TABLE %I.funding (
                    id TEXT PRIMARY KEY,
                    symbol TEXT NOT NULL,
                    funding TEXT NOT NULL,
                    "transactionTime" BIGINT NOT NULL
                        CHECK ("transactionTime" >= 0)
                )',
                target_schema
            );
            EXECUTE format(
                'CREATE INDEX %I ON %I.funding ("transactionTime")',
                target_schema || '_funding_time_idx',
                target_schema
            );
        END IF;
    END LOOP;
END;
$$;
