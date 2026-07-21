ALTER TABLE strategy_envs
    ADD COLUMN funding_csv_dir TEXT,
    ADD COLUMN funding_csv_account TEXT;

UPDATE strategy_envs
SET funding_csv_dir = '/home/ubuntu/liang_torch/funding_rate_analysis/fr_data_binance_nova02/funding',
    funding_csv_account = 'binance_nova02'
WHERE slug = 'binance_fr_arb01';
