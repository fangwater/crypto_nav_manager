UPDATE strategy_envs
SET enabled = FALSE,
    updated_at = CURRENT_TIMESTAMP
WHERE slug = 'binance-intra-arb01';
