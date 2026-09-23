INSERT INTO strategy_envs (
    slug, alias, db_schema, host, env_path, csv_output_dir,
    st_ms, strategy_kind, exchange, account_mode, required_keys,
    config_url, sort_order
) VALUES
    (
        'binance_cta_special_rx02', 'binance CTA rx02', 'binance_cta_special_rx02',
        'local', '/home/ubuntu/binance-cta-special-rx02/env.sh',
        '/home/ubuntu/binance-cta-special-rx02/data',
        1790074800000, 'market_making', 'binance', 'rapidx',
        '["LTP_API_KEY", "LTP_API_SECRET", "LTP_PORTFOLIO_ID"]'::jsonb,
        '/cta/binance-cta-special-rx02/config', 18
    ),
    (
        'binance_cta_special_rx03', 'binance CTA rx03', 'binance_cta_special_rx03',
        'local', '/home/ubuntu/binance-cta-special-rx03/env.sh',
        '/home/ubuntu/binance-cta-special-rx03/data',
        1790074800000, 'market_making', 'binance', 'rapidx',
        '["LTP_API_KEY", "LTP_API_SECRET", "LTP_PORTFOLIO_ID"]'::jsonb,
        '/cta/binance-cta-special-rx03/config', 19
    );

SELECT ensure_strategy_storage('binance_cta_special_rx02');
SELECT ensure_strategy_storage('binance_cta_special_rx03');

INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES
    ('172.31.35.228', 'binance_cta_special_rx02', 'ltp'),
    ('172.31.35.228', 'binance_cta_special_rx03', 'ltp')
ON CONFLICT DO NOTHING;

UPDATE strategy_envs
SET st_ms = 1790074800000,
    updated_at = CURRENT_TIMESTAMP
WHERE slug IN ('binance_fr_arb02', 'gate_fr_arb03');
