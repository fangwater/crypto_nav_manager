SELECT ensure_trading_fee_rate_storage(db_schema)
FROM strategy_envs
ORDER BY sort_order, slug;
