UPDATE strategy_envs
SET alias = 'prc_cta_01',
    updated_at = CURRENT_TIMESTAMP
WHERE slug = 'binance_exec_trade01';

INSERT INTO strategy_envs (
    slug,
    alias,
    db_schema,
    host,
    env_path,
    csv_output_dir,
    st_ms,
    strategy_kind,
    exchange,
    account_mode,
    required_keys,
    config_url,
    sort_order
) VALUES (
    'binance_exec_trade02',
    'prc_cta_02',
    'binance_exec_trade02',
    'local',
    '/home/ubuntu/binance_exec_trade02/env.sh',
    '/home/ubuntu/binance_exec_trade02/data',
    1790051458324,
    'cta',
    'binance',
    'usdm_futures',
    '["BINANCE_API_KEY", "BINANCE_API_SECRET"]'::jsonb,
    '/manager/account/?source=binance_exec_trade02',
    17
);

SELECT ensure_strategy_storage('binance_exec_trade02');
