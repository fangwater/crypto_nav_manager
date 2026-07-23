DO $$
DECLARE
    target_schema TEXT;
    source_rows BIGINT;
    migrated_rows BIGINT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'binance_fr_arb01',
        'binance_fr_arb02',
        'binance_fr_arb03',
        'binance_fr_arb04'
    ]
    LOOP
        EXECUTE format(
            'CREATE TABLE %I.funding_v2 (
                "tranId" BIGINT PRIMARY KEY,
                symbol TEXT NOT NULL,
                income TEXT NOT NULL,
                time BIGINT NOT NULL CHECK (time >= 0)
            )',
            target_schema
        );

        EXECUTE format(
            'INSERT INTO %I.funding_v2 ("tranId", symbol, income, time)
             SELECT
                 (raw->>''tranId'')::BIGINT,
                 raw->>''symbol'',
                 raw->>''income'',
                 (raw->>''time'')::BIGINT
             FROM %I.funding_fees',
            target_schema,
            target_schema
        );

        EXECUTE format('SELECT COUNT(*) FROM %I.funding_fees', target_schema)
            INTO source_rows;
        EXECUTE format('SELECT COUNT(*) FROM %I.funding_v2', target_schema)
            INTO migrated_rows;
        IF source_rows <> migrated_rows THEN
            RAISE EXCEPTION
                'funding migration row count mismatch for %: source %, migrated %',
                target_schema, source_rows, migrated_rows;
        END IF;

        EXECUTE format('DROP TABLE %I.funding_fees', target_schema);
        EXECUTE format(
            'ALTER TABLE %I.funding_v2 RENAME TO funding',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX %I ON %I.funding (time)',
            target_schema || '_funding_time_idx',
            target_schema
        );
    END LOOP;
END;
$$;
