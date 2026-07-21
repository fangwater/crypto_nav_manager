ALTER TABLE strategy_envs
    DROP COLUMN funding_csv_account;

UPDATE strategy_envs
SET funding_csv_dir = '.'
WHERE funding_csv_dir IS NOT NULL;
