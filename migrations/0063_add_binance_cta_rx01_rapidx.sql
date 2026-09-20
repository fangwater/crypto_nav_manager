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
    'binance_cta_rx01',
    'binance CTA rx01',
    'binance_cta_rx01',
    'local',
    '/home/ubuntu/binance-cta-rx01/env.sh',
    '/home/ubuntu/binance-cta-rx01/data',
    1789571847005,
    'cta',
    'binance',
    'rapidx',
    '["LTP_API_KEY", "LTP_API_SECRET", "LTP_PORTFOLIO_ID"]'::jsonb,
    '/cta/binance-cta-rx01/config',
    16
);

SELECT ensure_strategy_storage('binance_cta_rx01');

-- RapidX/LTP REST must egress from the env's own bound source IP rather than
-- the generic exchange pool; 'ltp' marks which exchange stream uses it.
INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES ('172.31.35.228', 'binance_cta_rx01', 'ltp')
ON CONFLICT DO NOTHING;
