CREATE TABLE strategy_snapshots (
    strategy_slug TEXT NOT NULL REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    snapshot_ts_ms BIGINT NOT NULL CHECK (snapshot_ts_ms > 0),
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    source_url TEXT NOT NULL,
    payload JSONB NOT NULL,
    PRIMARY KEY (strategy_slug, snapshot_ts_ms)
);

CREATE INDEX strategy_snapshots_fetched_at_idx
    ON strategy_snapshots (fetched_at DESC);

UPDATE strategy_envs
SET config_url = 'http://47.131.162.78:4191' || config_url,
    updated_at = CURRENT_TIMESTAMP
WHERE host = 'sg'
  AND config_url LIKE '/%';
