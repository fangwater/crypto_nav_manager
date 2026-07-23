DO $$
DECLARE
    target_schema TEXT;
    source_table TEXT;
    source_rows BIGINT;
    normalized_rows BIGINT;
BEGIN
    FOR target_schema IN
        SELECT db_schema FROM strategy_envs ORDER BY db_schema
    LOOP
        EXECUTE format(
            'CREATE TABLE %I.trades_normalized (
                market TEXT NOT NULL,
                symbol TEXT NOT NULL,
                trade_id TEXT NOT NULL,
                order_id TEXT NOT NULL,
                side TEXT NOT NULL,
                liquidity_role TEXT NOT NULL,
                price NUMERIC(38, 18) NOT NULL,
                quantity NUMERIC(38, 18) NOT NULL,
                quote_quantity NUMERIC(38, 18),
                fee_amount NUMERIC(38, 18) NOT NULL,
                fee_asset TEXT,
                fee_usdt NUMERIC(38, 18),
                realized_pnl NUMERIC(38, 18),
                event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0),
                PRIMARY KEY (market, symbol, trade_id)
            )',
            target_schema
        );

        IF to_regclass(format('%I.trades', target_schema)) IS NOT NULL THEN
            source_table := 'trades';
        ELSIF to_regclass(format('%I.trade_fills', target_schema)) IS NOT NULL THEN
            source_table := 'trade_fills';
        ELSE
            source_table := NULL;
        END IF;

        IF source_table IS NOT NULL THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = target_schema
                  AND table_name = source_table
                  AND column_name = 'sid'
            ) THEN
                EXECUTE format(
                    'INSERT INTO %I.trades_normalized (
                        market, symbol, trade_id, order_id, side, liquidity_role,
                        price, quantity, quote_quantity, fee_amount, fee_asset,
                        fee_usdt, realized_pnl, event_time_ms
                    )
                    SELECT
                        CASE WHEN sid = 1 THEN ''spot'' ELSE ''swap'' END,
                        symbol,
                        id::TEXT,
                        "orderId"::TEXT,
                        side,
                        ttype,
                        price,
                        qty,
                        amountu,
                        fees,
                        "commissionAsset",
                        CASE
                            WHEN UPPER("commissionAsset") IN (''USD'', ''USDC'', ''USDT'')
                            THEN fees
                            ELSE NULL
                        END,
                        "realizedPnl",
                        ts
                    FROM %I.%I',
                    target_schema,
                    target_schema,
                    source_table
                );
            ELSE
                EXECUTE format(
                    'INSERT INTO %I.trades_normalized (
                        market, symbol, trade_id, order_id, side, liquidity_role,
                        price, quantity, quote_quantity, fee_amount, fee_asset,
                        fee_usdt, realized_pnl, event_time_ms
                    )
                    SELECT
                        market,
                        symbol,
                        trade_id,
                        COALESCE(order_id, ''''),
                        COALESCE(side, ''''),
                        COALESCE(liquidity_role, ''''),
                        price,
                        quantity,
                        quote_quantity,
                        COALESCE(fee_amount, 0),
                        fee_asset,
                        fee_usdt,
                        realized_pnl,
                        event_time_ms
                    FROM %I.%I',
                    target_schema,
                    target_schema,
                    source_table
                );
            END IF;

            EXECUTE format('SELECT COUNT(*) FROM %I.%I', target_schema, source_table)
                INTO source_rows;
            EXECUTE format('SELECT COUNT(*) FROM %I.trades_normalized', target_schema)
                INTO normalized_rows;
            IF source_rows <> normalized_rows THEN
                RAISE EXCEPTION
                    '%.% trade row count mismatch: source %, normalized %',
                    target_schema, source_table, source_rows, normalized_rows;
            END IF;

            EXECUTE format('DROP TABLE %I.%I', target_schema, source_table);
        END IF;

        IF source_table IS DISTINCT FROM 'trade_fills'
           AND to_regclass(format('%I.trade_fills', target_schema)) IS NOT NULL THEN
            EXECUTE format('SELECT COUNT(*) FROM %I.trade_fills', target_schema)
                INTO source_rows;
            IF source_rows <> 0 THEN
                RAISE EXCEPTION
                    '%.trade_fills still contains % rows', target_schema, source_rows;
            END IF;
            EXECUTE format('DROP TABLE %I.trade_fills', target_schema);
        END IF;

        EXECUTE format(
            'ALTER TABLE %I.trades_normalized RENAME TO trades',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX %I ON %I.trades (event_time_ms)',
            target_schema || '_trades_time_idx',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX %I ON %I.trades (symbol, event_time_ms)',
            target_schema || '_trades_symbol_time_idx',
            target_schema
        );
    END LOOP;
END;
$$;

DO $$
DECLARE
    target_schema TEXT;
    dataset TEXT;
    source_table TEXT;
    fallback_table TEXT;
    source_rows BIGINT;
    normalized_rows BIGINT;
BEGIN
    FOR target_schema IN
        SELECT db_schema FROM strategy_envs ORDER BY db_schema
    LOOP
        FOREACH dataset IN ARRAY ARRAY['funding', 'interest']
        LOOP
            fallback_table := CASE dataset
                WHEN 'funding' THEN 'funding_fees'
                ELSE 'borrow_interest'
            END;

            EXECUTE format(
                'CREATE TABLE %I.%I (
                    record_id TEXT PRIMARY KEY,
                    symbol TEXT,
                    asset TEXT NOT NULL,
                    amount NUMERIC(38, 18) NOT NULL,
                    amount_usdt NUMERIC(38, 18),
                    event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0)
                )',
                target_schema,
                dataset || '_normalized'
            );

            IF to_regclass(format('%I.%I', target_schema, dataset)) IS NOT NULL THEN
                source_table := dataset;
            ELSIF to_regclass(format('%I.%I', target_schema, fallback_table)) IS NOT NULL THEN
                source_table := fallback_table;
            ELSE
                source_table := NULL;
            END IF;

            IF source_table IS NOT NULL THEN
                IF EXISTS (
                    SELECT 1
                    FROM information_schema.columns
                    WHERE table_schema = target_schema
                      AND table_name = source_table
                      AND column_name = 'record_id'
                ) THEN
                    EXECUTE format(
                        'INSERT INTO %I.%I (
                            record_id, symbol, asset, amount, amount_usdt, event_time_ms
                        )
                        SELECT
                            record_id,
                            symbol,
                            COALESCE(asset, CASE WHEN %L = ''funding'' THEN ''USDT'' ELSE '''' END),
                            amount,
                            amount_usdt,
                            event_time_ms
                        FROM %I.%I',
                        target_schema,
                        dataset || '_normalized',
                        dataset,
                        target_schema,
                        source_table
                    );
                ELSIF dataset = 'funding' AND EXISTS (
                    SELECT 1
                    FROM information_schema.columns
                    WHERE table_schema = target_schema
                      AND table_name = source_table
                      AND column_name = 'tranId'
                ) THEN
                    EXECUTE format(
                        'INSERT INTO %I.funding_normalized (
                            record_id, symbol, asset, amount, amount_usdt, event_time_ms
                        )
                        SELECT "tranId"::TEXT, symbol, ''USDT'', income::NUMERIC,
                               income::NUMERIC, time
                        FROM %I.%I',
                        target_schema,
                        target_schema,
                        source_table
                    );
                ELSIF dataset = 'interest' AND EXISTS (
                    SELECT 1
                    FROM information_schema.columns
                    WHERE table_schema = target_schema
                      AND table_name = source_table
                      AND column_name = 'txId'
                ) THEN
                    EXECUTE format(
                        'INSERT INTO %I.interest_normalized (
                            record_id, symbol, asset, amount, amount_usdt, event_time_ms
                        )
                        SELECT "txId"::TEXT, NULL, UPPER(asset), interest::NUMERIC,
                               CASE WHEN UPPER(asset) IN (''USD'', ''USDC'', ''USDT'')
                                    THEN interest::NUMERIC ELSE NULL END,
                               "interestAccuredTime"
                        FROM %I.%I',
                        target_schema,
                        target_schema,
                        source_table
                    );
                ELSIF dataset = 'funding' THEN
                    EXECUTE format(
                        'INSERT INTO %I.funding_normalized (
                            record_id, symbol, asset, amount, amount_usdt, event_time_ms
                        )
                        SELECT id, symbol, ''USDT'', funding::NUMERIC,
                               funding::NUMERIC, "transactionTime"
                        FROM %I.%I',
                        target_schema,
                        target_schema,
                        source_table
                    );
                ELSE
                    EXECUTE format(
                        'INSERT INTO %I.interest_normalized (
                            record_id, symbol, asset, amount, amount_usdt, event_time_ms
                        )
                        SELECT id, NULL, UPPER(currency), interest::NUMERIC,
                               CASE WHEN UPPER(currency) IN (''USD'', ''USDC'', ''USDT'')
                                    THEN interest::NUMERIC ELSE NULL END,
                               "transactionTime"
                        FROM %I.%I',
                        target_schema,
                        target_schema,
                        source_table
                    );
                END IF;

                EXECUTE format('SELECT COUNT(*) FROM %I.%I', target_schema, source_table)
                    INTO source_rows;
                EXECUTE format(
                    'SELECT COUNT(*) FROM %I.%I',
                    target_schema,
                    dataset || '_normalized'
                ) INTO normalized_rows;
                IF source_rows <> normalized_rows THEN
                    RAISE EXCEPTION
                        '%.% row count mismatch: source %, normalized %',
                        target_schema, source_table, source_rows, normalized_rows;
                END IF;

                EXECUTE format('DROP TABLE %I.%I', target_schema, source_table);
            END IF;

            IF source_table IS DISTINCT FROM fallback_table
               AND to_regclass(format('%I.%I', target_schema, fallback_table)) IS NOT NULL THEN
                EXECUTE format(
                    'SELECT COUNT(*) FROM %I.%I',
                    target_schema,
                    fallback_table
                ) INTO source_rows;
                IF source_rows <> 0 THEN
                    RAISE EXCEPTION
                        '%.% still contains % rows',
                        target_schema, fallback_table, source_rows;
                END IF;
                EXECUTE format('DROP TABLE %I.%I', target_schema, fallback_table);
            END IF;

            EXECUTE format(
                'ALTER TABLE %I.%I RENAME TO %I',
                target_schema,
                dataset || '_normalized',
                dataset
            );
            EXECUTE format(
                'CREATE INDEX %I ON %I.%I (event_time_ms)',
                target_schema || '_' || dataset || '_time_idx',
                target_schema,
                dataset
            );
        END LOOP;
    END LOOP;
END;
$$;

CREATE OR REPLACE FUNCTION ensure_strategy_storage(target_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    IF target_schema !~ '^[a-z][a-z0-9_]*$' THEN
        RAISE EXCEPTION 'invalid strategy schema: %', target_schema;
    END IF;

    EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I', target_schema);
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.trades (
            market TEXT NOT NULL,
            symbol TEXT NOT NULL,
            trade_id TEXT NOT NULL,
            order_id TEXT NOT NULL,
            side TEXT NOT NULL,
            liquidity_role TEXT NOT NULL,
            price NUMERIC(38, 18) NOT NULL,
            quantity NUMERIC(38, 18) NOT NULL,
            quote_quantity NUMERIC(38, 18),
            fee_amount NUMERIC(38, 18) NOT NULL,
            fee_asset TEXT,
            fee_usdt NUMERIC(38, 18),
            realized_pnl NUMERIC(38, 18),
            event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0),
            PRIMARY KEY (market, symbol, trade_id)
        )',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.trades (event_time_ms)',
        target_schema || '_trades_time_idx',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.trades (symbol, event_time_ms)',
        target_schema || '_trades_symbol_time_idx',
        target_schema
    );

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.funding (
            record_id TEXT PRIMARY KEY,
            symbol TEXT,
            asset TEXT NOT NULL,
            amount NUMERIC(38, 18) NOT NULL,
            amount_usdt NUMERIC(38, 18),
            event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0)
        )',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.funding (event_time_ms)',
        target_schema || '_funding_time_idx',
        target_schema
    );

    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I.interest (
            record_id TEXT PRIMARY KEY,
            symbol TEXT,
            asset TEXT NOT NULL,
            amount NUMERIC(38, 18) NOT NULL,
            amount_usdt NUMERIC(38, 18),
            event_time_ms BIGINT NOT NULL CHECK (event_time_ms > 0)
        )',
        target_schema
    );
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I.interest (event_time_ms)',
        target_schema || '_interest_time_idx',
        target_schema
    );
END;
$$;
