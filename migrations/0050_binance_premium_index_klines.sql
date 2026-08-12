CREATE TABLE binance_premium_index_klines (
    symbol TEXT NOT NULL,
    interval TEXT NOT NULL DEFAULT '1m',
    open_time_ms BIGINT NOT NULL,
    close_time_ms BIGINT NOT NULL,
    open_rate DOUBLE PRECISION NOT NULL,
    high_rate DOUBLE PRECISION NOT NULL,
    low_rate DOUBLE PRECISION NOT NULL,
    close_rate DOUBLE PRECISION NOT NULL,
    sample_count BIGINT NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (symbol, interval, open_time_ms),
    CHECK (symbol = upper(symbol)),
    CHECK (interval = '1m'),
    CHECK (open_time_ms > 0),
    CHECK (close_time_ms = open_time_ms + 59999),
    CHECK (sample_count >= 0),
    CHECK (high_rate >= open_rate),
    CHECK (high_rate >= close_rate),
    CHECK (low_rate <= open_rate),
    CHECK (low_rate <= close_rate)
);

CREATE INDEX binance_premium_index_klines_time_idx
    ON binance_premium_index_klines (open_time_ms, symbol);

COMMENT ON TABLE binance_premium_index_klines IS
    'Binance USD-M premium-index K-lines from /fapi/v1/premiumIndexKlines; research data only.';
COMMENT ON COLUMN binance_premium_index_klines.close_rate IS
    'Premium index as a decimal rate. For example, 0.001 is 10 basis points.';
