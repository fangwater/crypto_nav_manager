-- Register the newly deployed gate_fr_arb03 and bitget_fr_arb03 envs and
-- reactivate binance_fr_arb02, which was retired alongside nova01 in
-- 0037_alignment_status_and_retire_nova.sql but has since been redeployed
-- for a different Binance account. All three envs share the 172.31.35.231
-- egress binding (mkt_signal docs/jp-meta-elvpn_ip_binding.md).

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
) VALUES
    (
        'gate_fr_arb03',
        'gate 资费 arb03',
        'gate_fr_arb03',
        'local',
        '/home/ubuntu/gate_fr_arb03/env.sh',
        '/home/ubuntu/gate_fr_arb03/data',
        1789516800000,
        'funding_rate',
        'gate',
        'unified',
        '["GATE_API_KEY", "GATE_API_SECRET"]'::jsonb,
        '/fr/gate_fr_arb03/config',
        125
    ),
    (
        'bitget_fr_arb03',
        'bitget 资费 arb03',
        'bitget_fr_arb03',
        'local',
        '/home/ubuntu/bitget_fr_arb03/env.sh',
        '/home/ubuntu/bitget_fr_arb03/data',
        1789516800000,
        'funding_rate',
        'bitget',
        'unified',
        '["BITGET_API_KEY", "BITGET_API_SECRET", "BITGET_API_PASSPHRASE"]'::jsonb,
        '/fr/bitget_fr_arb03/config',
        130
    );

SELECT ensure_strategy_storage('gate_fr_arb03');
SELECT ensure_trading_fee_rate_storage('gate_fr_arb03');
SELECT ensure_strategy_storage('bitget_fr_arb03');
SELECT ensure_trading_fee_rate_storage('bitget_fr_arb03');

-- The env was redeployed on 2026-09-16 for a different Binance account; keep
-- the slug/schema so Redis key prefixes stay aligned, refresh the display
-- metadata, and resume history sync from the redeploy date rather than the
-- retired nova01 watermark.
UPDATE strategy_envs
SET alias = 'binance 资费 arb02',
    csv_output_dir = '/home/ubuntu/binance_fr_arb02/data',
    st_ms = 1789516800000,
    enabled = TRUE,
    updated_at = CURRENT_TIMESTAMP
WHERE slug = 'binance_fr_arb02';

INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES
    ('172.31.35.231', 'gate_fr_arb03', 'gate'),
    ('172.31.35.231', 'bitget_fr_arb03', 'bitget')
ON CONFLICT DO NOTHING;
