CREATE TABLE history_sync_watermarks (
    strategy_slug TEXT NOT NULL
        REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    dataset TEXT NOT NULL
        CHECK (dataset IN ('trades', 'funding', 'interest')),
    success_end_ms BIGINT NOT NULL CHECK (success_end_ms > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (strategy_slug, dataset)
);

COMMENT ON TABLE history_sync_watermarks IS
    'Last fully successful REST history scan boundary per strategy and dataset';
