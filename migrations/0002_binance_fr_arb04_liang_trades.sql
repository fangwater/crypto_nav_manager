DO $$
BEGIN
    IF to_regclass('binance_fr_arb04.trade_fills') IS NOT NULL
       AND EXISTS (SELECT 1 FROM binance_fr_arb04.trade_fills LIMIT 1) THEN
        RAISE EXCEPTION 'binance_fr_arb04.trade_fills is not empty; migrate its rows before changing the Liang Torch schema';
    END IF;
END;
$$;

DROP TABLE IF EXISTS binance_fr_arb04.trade_fills;

CREATE TABLE binance_fr_arb04.trades (
    sid SMALLINT NOT NULL CHECK (sid IN (0, 1)),
    key TEXT NOT NULL CHECK (key IN ('binanceswap', 'binancespot')),
    symbol TEXT NOT NULL,
    id BIGINT NOT NULL,
    "orderId" BIGINT NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('buy', 'sell')),
    price NUMERIC(38, 18) NOT NULL,
    qty NUMERIC(38, 18) NOT NULL,
    amountu NUMERIC(38, 18) NOT NULL,
    fees NUMERIC(38, 18) NOT NULL,
    "commissionAsset" TEXT NOT NULL,
    "realizedPnl" NUMERIC(38, 18),
    ts BIGINT NOT NULL CHECK (ts >= 0),
    ttype TEXT NOT NULL CHECK (ttype IN ('maker', 'taker')),
    "positionSide" TEXT NOT NULL,
    PRIMARY KEY (key, symbol, id)
);

CREATE INDEX binance_fr_arb04_trades_ts_idx
    ON binance_fr_arb04.trades (ts);

CREATE INDEX binance_fr_arb04_trades_symbol_ts_idx
    ON binance_fr_arb04.trades (key, symbol, ts DESC, id DESC);

COMMENT ON TABLE binance_fr_arb04.trades IS
    'Liang Torch trades_YYYY-MM-DD.csv columns for binance 外部资金';

ALTER FUNCTION ensure_strategy_storage(TEXT)
    RENAME TO ensure_strategy_storage_base;

CREATE FUNCTION ensure_strategy_storage(target_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    IF target_schema = 'binance_fr_arb04' THEN
        EXECUTE 'CREATE SCHEMA IF NOT EXISTS binance_fr_arb04';
        EXECUTE 'CREATE TABLE IF NOT EXISTS binance_fr_arb04.trades (
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
        )';
        EXECUTE 'CREATE INDEX IF NOT EXISTS binance_fr_arb04_trades_ts_idx
            ON binance_fr_arb04.trades (ts)';
        EXECUTE 'CREATE INDEX IF NOT EXISTS binance_fr_arb04_trades_symbol_ts_idx
            ON binance_fr_arb04.trades (key, symbol, ts DESC, id DESC)';
    ELSE
        PERFORM ensure_strategy_storage_base(target_schema);
    END IF;
END;
$$;
