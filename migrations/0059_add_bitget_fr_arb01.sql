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
    'bitget_fr_arb01',
    'bitget 资费 arb01',
    'bitget_fr_arb01',
    'local',
    '/home/ubuntu/bitget_fr_arb01/env.sh',
    '/home/ubuntu/bitget_fr_arb01/data',
    1787801276961,
    'funding_rate',
    'bitget',
    'unified',
    '["BITGET_API_KEY", "BITGET_API_SECRET", "BITGET_API_PASSPHRASE"]'::jsonb,
    '/fr/bitget_fr_arb01/config',
    115
);

SELECT ensure_strategy_storage('bitget_fr_arb01');
SELECT ensure_trading_fee_rate_storage('bitget_fr_arb01');

INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES ('172.31.35.231', 'bitget_fr_arb01', 'bitget')
ON CONFLICT DO NOTHING;
