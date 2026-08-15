CREATE TABLE IF NOT EXISTS intra_hourly_latency (
    strategy_slug TEXT NOT NULL,
    window_start_ms BIGINT NOT NULL,
    window_end_ms BIGINT NOT NULL,
    computed_at_ms BIGINT NOT NULL,
    margin_new_create_count BIGINT NOT NULL,
    margin_new_create_normal_count BIGINT NOT NULL,
    margin_new_create_p50_ms DOUBLE PRECISION,
    margin_new_create_p90_ms DOUBLE PRECISION,
    futures_new_create_count BIGINT NOT NULL,
    futures_new_create_normal_count BIGINT NOT NULL,
    futures_new_create_p50_ms DOUBLE PRECISION,
    futures_new_create_p90_ms DOUBLE PRECISION,
    spot_trigger_count BIGINT NOT NULL,
    spot_trigger_normal_count BIGINT NOT NULL,
    spot_trigger_p50_ms DOUBLE PRECISION,
    spot_trigger_p90_ms DOUBLE PRECISION,
    futures_trigger_count BIGINT NOT NULL,
    futures_trigger_normal_count BIGINT NOT NULL,
    futures_trigger_p50_ms DOUBLE PRECISION,
    futures_trigger_p90_ms DOUBLE PRECISION,
    PRIMARY KEY (strategy_slug, window_start_ms)
);

CREATE INDEX IF NOT EXISTS intra_hourly_latency_slug_window_idx
    ON intra_hourly_latency (strategy_slug, window_start_ms DESC);

COMMENT ON TABLE intra_hourly_latency IS
    'Hourly persist-derived NEW-create and trigger-split market latency for intra arb01 strategies.';
