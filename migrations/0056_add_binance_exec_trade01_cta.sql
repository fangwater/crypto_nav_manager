CREATE SCHEMA IF NOT EXISTS binance_exec_trade01;

CREATE TABLE binance_exec_trade01.trades
    (LIKE binance_mm_alpha.trades INCLUDING ALL);

CREATE TABLE binance_exec_trade01.funding
    (LIKE binance_mm_alpha.funding INCLUDING ALL);

-- The Exec account took over the existing Binance MM account at this first
-- CTA fill. Reuse its locally normalized history before the targeted repair.
INSERT INTO binance_exec_trade01.trades
SELECT *
FROM binance_mm_alpha.trades
WHERE event_time_ms >= 1787377620383;

INSERT INTO binance_exec_trade01.funding
SELECT *
FROM binance_mm_alpha.funding
WHERE event_time_ms >= 1787377620383;

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
    'binance_exec_trade01',
    'binance CTA trade01',
    'binance_exec_trade01',
    'local',
    '/home/ubuntu/binance_exec_trade01/env.sh',
    '/home/ubuntu/binance_exec_trade01/data',
    1787377620383,
    'cta',
    'binance',
    'usdm_futures',
    '["BINANCE_API_KEY", "BINANCE_API_SECRET"]'::jsonb,
    '/manager/account/?source=binance_exec_trade01',
    15
);

INSERT INTO history_sync_watermarks (strategy_slug, dataset, success_end_ms)
SELECT
    'binance_exec_trade01',
    'trades',
    MAX(event_time_ms)
FROM binance_exec_trade01.trades;

UPDATE strategy_envs
SET enabled = FALSE,
    updated_at = CURRENT_TIMESTAMP
WHERE slug = 'binance_mm_alpha';
