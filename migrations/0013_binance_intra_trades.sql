DO $$
DECLARE
    target_schema TEXT;
    existing_rows BIGINT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'binance_intra_arb01'
    ]
    LOOP
        IF to_regclass(target_schema || '.trades') IS NULL THEN
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
                        CHECK (key IN (''binanceswap'', ''binancespot'')),
                    symbol TEXT NOT NULL,
                    id BIGINT NOT NULL,
                    "orderId" BIGINT NOT NULL,
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
    END LOOP;
END;
$$;
