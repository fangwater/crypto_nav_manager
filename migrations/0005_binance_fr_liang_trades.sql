DO $$
DECLARE
    target_schema TEXT;
    existing_rows BIGINT;
BEGIN
    FOREACH target_schema IN ARRAY ARRAY[
        'binance_fr_arb01',
        'binance_fr_arb02',
        'binance_fr_arb03'
    ]
    LOOP
        EXECUTE format('SELECT COUNT(*) FROM %I.trade_fills', target_schema)
            INTO existing_rows;
        IF existing_rows > 0 THEN
            RAISE EXCEPTION '%.trade_fills is not empty; migrate its rows first', target_schema;
        END IF;

        EXECUTE format('DROP TABLE %I.trade_fills', target_schema);
        EXECUTE format(
            'CREATE TABLE %I.trades (
                sid SMALLINT NOT NULL CHECK (sid IN (0, 1)),
                key TEXT NOT NULL CHECK (key IN (''binanceswap'', ''binancespot'')),
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
                ts BIGINT NOT NULL CHECK (ts >= 0),
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
            'CREATE INDEX %I ON %I.trades (key, symbol, ts DESC, id DESC)',
            target_schema || '_trades_symbol_ts_idx',
            target_schema
        );
    END LOOP;
END;
$$;

CREATE OR REPLACE FUNCTION ensure_strategy_storage_without_trading_fee_rates(target_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    IF target_schema = ANY (ARRAY[
        'binance_fr_arb01',
        'binance_fr_arb02',
        'binance_fr_arb03',
        'binance_fr_arb04'
    ]) THEN
        EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I', target_schema);
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I.trades (
                sid SMALLINT NOT NULL CHECK (sid IN (0, 1)),
                key TEXT NOT NULL CHECK (key IN (''binanceswap'', ''binancespot'')),
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
                ts BIGINT NOT NULL CHECK (ts >= 0),
                ttype TEXT NOT NULL CHECK (ttype IN (''maker'', ''taker'')),
                "positionSide" TEXT NOT NULL,
                PRIMARY KEY (key, symbol, id)
            )',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS %I ON %I.trades (ts)',
            target_schema || '_trades_ts_idx',
            target_schema
        );
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS %I
                ON %I.trades (key, symbol, ts DESC, id DESC)',
            target_schema || '_trades_symbol_ts_idx',
            target_schema
        );
    ELSE
        PERFORM ensure_strategy_storage_base(target_schema);
    END IF;
END;
$$;
