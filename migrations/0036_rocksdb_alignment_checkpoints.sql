CREATE TABLE rocksdb_alignment_checkpoints (
    strategy_slug TEXT PRIMARY KEY
        REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    aligned_from_ms BIGINT NOT NULL CHECK (aligned_from_ms > 0),
    verified_through_ms BIGINT NOT NULL
        CHECK (verified_through_ms >= aligned_from_ms),
    verified_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE rocksdb_alignment_checkpoints IS
    'Continuous intervals where strategy RocksDB order fills reconcile with PostgreSQL trades.';

INSERT INTO rocksdb_alignment_checkpoints (
    strategy_slug,
    aligned_from_ms,
    verified_through_ms
) VALUES (
    'binance-intra-arb01',
    1784122620000,
    1785254399999
);
