ALTER TABLE rocksdb_alignment_status
    ADD COLUMN automatic_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN automatic_updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP;

COMMENT ON COLUMN rocksdb_alignment_status.automatic_enabled IS
    'Whether the live history scheduler should run RocksDB alignment for this strategy.';

UPDATE rocksdb_alignment_status
SET automatic_enabled = FALSE,
    automatic_updated_at = CURRENT_TIMESTAMP
WHERE strategy_slug = 'bybit_mm_alpha';
