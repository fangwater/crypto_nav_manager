CREATE TABLE rocksdb_alignment_status (
    strategy_slug TEXT PRIMARY KEY
        REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    state TEXT NOT NULL
        CHECK (state IN ('waiting', 'running', 'succeeded', 'mismatch', 'failed')),
    phase TEXT NOT NULL,
    progress_percent INTEGER NOT NULL DEFAULT 0
        CHECK (progress_percent BETWEEN 0 AND 100),
    run_id TEXT,
    started_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMPTZ,
    candidate_end_ms BIGINT,
    scan_start_ms BIGINT,
    pg_success_end_ms BIGINT,
    actual_end_ms BIGINT,
    group_count INTEGER,
    mismatch_count INTEGER,
    pg_event_count BIGINT,
    local_event_count BIGINT,
    message TEXT
);

COMMENT ON TABLE rocksdb_alignment_status IS
    'Live and last-completed progress for strategy-center RocksDB versus PostgreSQL trade reconciliation.';

INSERT INTO rocksdb_alignment_status (
    strategy_slug,
    state,
    phase,
    progress_percent,
    completed_at,
    actual_end_ms,
    group_count,
    mismatch_count,
    message
)
SELECT
    strategy_slug,
    'succeeded',
    'complete',
    100,
    verified_at,
    verified_through_ms,
    0,
    0,
    '历史校对已通过'
FROM rocksdb_alignment_checkpoints
ON CONFLICT (strategy_slug) DO NOTHING;

UPDATE strategy_envs
SET enabled = FALSE,
    updated_at = CURRENT_TIMESTAMP
WHERE slug IN ('binance_fr_arb01', 'binance_fr_arb02');
