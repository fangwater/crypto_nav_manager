DO $$
DECLARE
    target_schema TEXT;
    existing_rows BIGINT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'gate_fr_arb01',
        'gate_fr_arb02'
    ]
    LOOP
        IF to_regclass(format('%I.trades', target_schema)) IS NULL THEN
            EXECUTE format(
                'SELECT COUNT(*) FROM %I.trade_fills',
                target_schema
            ) INTO existing_rows;
            IF existing_rows <> 0 THEN
                RAISE EXCEPTION
                    '%.trade_fills contains % rows; migrate them before replacing the table',
                    target_schema, existing_rows;
            END IF;

            EXECUTE format('DROP TABLE %I.trade_fills', target_schema);
            EXECUTE format(
                'CREATE TABLE %I.trades (
                    sid SMALLINT NOT NULL CHECK (sid IN (0, 1)),
                    key TEXT NOT NULL
                        CHECK (key IN (''gateswap'', ''gatespot'')),
                    symbol TEXT NOT NULL,
                    id TEXT NOT NULL,
                    "orderId" TEXT NOT NULL,
                    side TEXT NOT NULL CHECK (side IN (''buy'', ''sell'')),
                    price NUMERIC(38, 18) NOT NULL,
                    qty NUMERIC(38, 18) NOT NULL,
                    amountu NUMERIC(38, 18) NOT NULL,
                    fees NUMERIC(38, 18) NOT NULL,
                    "commissionAsset" TEXT NOT NULL,
                    "realizedPnl" NUMERIC(38, 18),
                    ts BIGINT NOT NULL CHECK (ts > 0),
                    ttype TEXT NOT NULL CHECK (ttype IN (''maker'', ''taker'')),
                    "positionSide" TEXT NOT NULL,
                    PRIMARY KEY (key, symbol, id)
                )',
                target_schema
            );
            EXECUTE format(
                'CREATE INDEX %I ON %I.trades (ts)',
                target_schema || '_trades_ts_idx',
                target_schema
            );
            EXECUTE format(
                'CREATE INDEX %I
                 ON %I.trades (key, symbol, ts DESC, id DESC)',
                target_schema || '_trades_symbol_ts_idx',
                target_schema
            );
        END IF;

        IF to_regclass(format('%I.funding', target_schema)) IS NULL THEN
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
                        CHECK ("transactionTime" > 0)
                )',
                target_schema
            );
            EXECUTE format(
                'CREATE INDEX %I ON %I.funding ("transactionTime")',
                target_schema || '_funding_time_idx',
                target_schema
            );
        END IF;

        IF to_regclass(format('%I.interest', target_schema)) IS NULL THEN
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

DO $$
DECLARE
    target_schema TEXT;
    old_index TEXT;
BEGIN
    FOR target_schema IN
        SELECT db_schema
        FROM strategy_envs
        WHERE strategy_kind = 'funding_rate'
          AND db_schema NOT IN ('gate_fr_arb01', 'gate_fr_arb02')
        ORDER BY db_schema
    LOOP
        IF to_regclass(format('%I.interest', target_schema)) IS NULL
           AND to_regclass(format('%I.borrow_interest', target_schema)) IS NOT NULL THEN
            EXECUTE format(
                'ALTER TABLE %I.borrow_interest RENAME TO interest',
                target_schema
            );

            old_index := target_schema || '_borrow_interest_time_idx';
            IF to_regclass(format('%I.%I', target_schema, old_index)) IS NOT NULL THEN
                EXECUTE format(
                    'ALTER INDEX %I.%I RENAME TO %I',
                    target_schema,
                    old_index,
                    target_schema || '_interest_time_idx'
                );
            END IF;
        END IF;
    END LOOP;
END;
$$;
