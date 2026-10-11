-- RX02/RX03 have been flattened. Remove only their NAV-owned datasets;
-- deployed trading environments and their RocksDB histories remain untouched.
-- An explicit internal Viz URL avoids gateway login when NAV collects snapshots.
ALTER TABLE strategy_envs ADD COLUMN snapshot_url TEXT;

DELETE FROM strategy_initial_snapshots
WHERE strategy_slug IN ('binance_cta_special_rx02', 'binance_cta_special_rx03');

DELETE FROM intra_hourly_latency
WHERE strategy_slug IN ('binance_cta_special_rx02', 'binance_cta_special_rx03');

DELETE FROM rest_egress_ip_envs
WHERE env IN ('binance_cta_special_rx02', 'binance_cta_special_rx03');

-- Snapshots, grants, history watermarks and alignment state cascade with
-- the strategy registry entries.
DELETE FROM strategy_envs
WHERE slug IN ('binance_cta_special_rx02', 'binance_cta_special_rx03');

DROP SCHEMA IF EXISTS binance_cta_special_rx02 CASCADE;
DROP SCHEMA IF EXISTS binance_cta_special_rx03 CASCADE;

-- Exec05 uses the former RX02 RapidX portfolio under its own IPC namespace.
-- Its first actual fill was confirmed from CTA Manager's Exec05 order history.
INSERT INTO strategy_envs (
    slug, alias, db_schema, host, env_path, csv_output_dir,
    st_ms, strategy_kind, exchange, account_mode, required_keys,
    config_url, snapshot_url, sort_order
) VALUES (
    'binance_exec_trade05', 'lgq_cta', 'binance_exec_trade05',
    'local', '/home/ubuntu/binance_exec_trade05/env.sh',
    '/home/ubuntu/binance_exec_trade05/data',
    1791686890453, 'cta', 'binance', 'rapidx',
    '["LTP_API_KEY", "LTP_API_SECRET", "LTP_PORTFOLIO_ID"]'::jsonb,
    '/exec_trade05/config', 'http://127.0.0.1:10045/snapshot', 18
);

SELECT ensure_strategy_storage('binance_exec_trade05');

INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES ('172.31.35.228', 'binance_exec_trade05', 'ltp')
ON CONFLICT DO NOTHING;
