INSERT INTO rocksdb_alignment_checkpoints (
    strategy_slug,
    aligned_from_ms,
    verified_through_ms
) VALUES (
    'binance_mm_alpha',
    1782286020000,
    1782286020000
)
ON CONFLICT (strategy_slug) DO NOTHING;

INSERT INTO rocksdb_alignment_status (
    strategy_slug,
    state,
    phase,
    progress_percent,
    message
) VALUES (
    'binance_mm_alpha',
    'waiting',
    'waiting',
    0,
    '等待首次校对'
)
ON CONFLICT (strategy_slug) DO NOTHING;
