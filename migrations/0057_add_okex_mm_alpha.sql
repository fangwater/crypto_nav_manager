CREATE SCHEMA IF NOT EXISTS okex_mm_alpha;

CREATE TABLE okex_mm_alpha.trades
    (LIKE binance_mm_alpha.trades INCLUDING ALL);

CREATE TABLE okex_mm_alpha.funding
    (LIKE binance_mm_alpha.funding INCLUDING ALL);

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
    'okex_mm_alpha',
    'okex 做市',
    'okex_mm_alpha',
    'local',
    '/home/ubuntu/okex_mm_alpha/env.sh',
    '/home/ubuntu/okex_mm_alpha/data',
    1786892326041,
    'market_making',
    'okx',
    'unified',
    '["OKX_API_KEY", "OKX_API_SECRET", "OKX_PASSPHRASE"]'::jsonb,
    '/mm/okex_mm_alpha/config',
    25
);
