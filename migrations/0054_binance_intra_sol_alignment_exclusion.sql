INSERT INTO rocksdb_alignment_trade_exclusions (
    strategy_slug,
    market,
    order_id,
    reason
) VALUES
    ('binance-intra-arb01', 'spot', '17598702027',
     'Confirmed persist telemetry gap: SOLUSDT sell trade 2021853832 qty 2.633 at 2026-08-17 03:18:07.059 UTC; trade retained in PostgreSQL')
ON CONFLICT DO NOTHING;
