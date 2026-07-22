CREATE TABLE contract_multipliers (
    exchange TEXT NOT NULL
        CHECK (exchange IN ('gate', 'okx')),
    market TEXT NOT NULL
        CHECK (market = 'usdt_futures'),
    instrument TEXT NOT NULL,
    symbol TEXT NOT NULL,
    base_asset TEXT NOT NULL,
    quote_asset TEXT NOT NULL,
    contract_value NUMERIC(38, 18) NOT NULL
        CHECK (contract_value > 0),
    contract_factor NUMERIC(38, 18) NOT NULL
        CHECK (contract_factor > 0),
    contract_multiplier NUMERIC(38, 18)
        GENERATED ALWAYS AS (contract_value * contract_factor) STORED,
    status TEXT,
    effective_at_ms BIGINT NOT NULL
        CHECK (effective_at_ms >= 0),
    raw JSONB NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (exchange, market, instrument, effective_at_ms)
);

CREATE INDEX contract_multipliers_symbol_time_idx
    ON contract_multipliers (
        exchange, market, symbol, effective_at_ms DESC
    );

CREATE INDEX contract_multipliers_instrument_time_idx
    ON contract_multipliers (
        exchange, market, instrument, effective_at_ms DESC
    );

COMMENT ON TABLE contract_multipliers IS
    'Daily UTC snapshots of Gate and OKX base-asset quantity per futures contract';

COMMENT ON COLUMN contract_multipliers.contract_value IS
    'Gate quanto_multiplier, or OKX ctVal';

COMMENT ON COLUMN contract_multipliers.contract_factor IS
    '1 for Gate, or OKX ctMult';

COMMENT ON COLUMN contract_multipliers.effective_at_ms IS
    'UTC day start for the exchange specification snapshot';
