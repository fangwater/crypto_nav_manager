CREATE TABLE strategy_initial_snapshots (
    strategy_slug TEXT PRIMARY KEY REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    snapshot_ts_ms BIGINT NOT NULL CHECK (snapshot_ts_ms > 0),
    selected_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (strategy_slug, snapshot_ts_ms)
        REFERENCES strategy_snapshots(strategy_slug, snapshot_ts_ms)
        ON DELETE RESTRICT
);

