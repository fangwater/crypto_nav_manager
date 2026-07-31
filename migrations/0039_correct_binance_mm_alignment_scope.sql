UPDATE rocksdb_alignment_checkpoints
SET aligned_from_ms = 1782777600000,
    verified_through_ms = 1782777600000,
    verified_at = CURRENT_TIMESTAMP
WHERE strategy_slug = 'binance_mm_alpha'
  AND verified_through_ms = 1782286020000;

UPDATE rocksdb_alignment_status
SET state = 'waiting',
    phase = 'waiting',
    progress_percent = 0,
    started_at = NULL,
    updated_at = CURRENT_TIMESTAMP,
    completed_at = NULL,
    candidate_end_ms = NULL,
    scan_start_ms = NULL,
    pg_success_end_ms = NULL,
    actual_end_ms = NULL,
    group_count = NULL,
    mismatch_count = NULL,
    pg_event_count = NULL,
    local_event_count = NULL,
    message = '等待六币范围首次校对'
WHERE strategy_slug = 'binance_mm_alpha'
  AND state = 'mismatch';
