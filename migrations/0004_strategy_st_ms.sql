ALTER TABLE strategy_envs
    ADD COLUMN st_ms BIGINT;

UPDATE strategy_envs AS strategy
SET st_ms = source.st_ms,
    updated_at = CURRENT_TIMESTAMP
FROM (VALUES
    ('binance_mm_alpha', 1776077992740::BIGINT),
    ('bybit_mm_alpha', 1776698022048::BIGINT),
    ('binance-intra-arb01', 1782604800000::BIGINT),
    ('bybit-intra-arb01', 1783468800000::BIGINT),
    ('bybit-intra-arb02', 1784073600000::BIGINT),
    ('binance_fr_arb04', 1759276800000::BIGINT),
    ('binance_fr_arb02', 1759276800000::BIGINT),
    ('binance_fr_arb01', 1759276800000::BIGINT),
    ('binance_fr_arb03', 1759276800000::BIGINT),
    ('gate_fr_arb02', 1782777600000::BIGINT),
    ('bitget_fr_arb02', 1778820160042::BIGINT),
    ('gate_fr_arb01', 1779667200000::BIGINT)
) AS source(slug, st_ms)
WHERE strategy.slug = source.slug;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM strategy_envs WHERE st_ms IS NULL) THEN
        RAISE EXCEPTION 'every strategy_envs row must have st_ms';
    END IF;
END;
$$;

ALTER TABLE strategy_envs
    ALTER COLUMN st_ms SET NOT NULL,
    ADD CONSTRAINT strategy_envs_st_ms_check CHECK (st_ms > 0);

COMMENT ON COLUMN strategy_envs.st_ms IS
    'Strategy run start time in Unix milliseconds (UTC)';
