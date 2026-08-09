COMMENT ON TABLE rocksdb_alignment_trade_exclusions IS
    'Exchange orders intentionally excluded from RocksDB fill reconciliation after they are proven external to the strategy or belong to an exact, confirmed local telemetry outage. Canonical PostgreSQL trades remain unchanged.';

INSERT INTO rocksdb_alignment_trade_exclusions (
    strategy_slug,
    market,
    order_id,
    reason
) VALUES
    ('binance-intra-arb01', 'spot', '65249170442',
     'Confirmed restart telemetry gap: BTCUSDT sell trade 6564331640 at 2026-08-08 17:25:50.194 UTC; trade retained in PostgreSQL'),
    ('binance-intra-arb01', 'swap', '93949474185',
     'Confirmed restart telemetry gap: BNBUSDT buy trade 2221475759 at 2026-08-08 17:26:46.159 UTC; trade retained in PostgreSQL'),
    ('binance-intra-arb01', 'swap', '40603602493',
     'Confirmed restart telemetry gap: SUIUSDT buy trade 1532567185 at 2026-08-08 17:27:38.818 UTC; trade retained in PostgreSQL'),
    ('binance-intra-arb01', 'spot', '189040248',
     'Confirmed restart telemetry gap: ZAMAUSDT sell trades 25606341 and 25606342 at 2026-08-08 17:27:51.143 UTC; trades retained in PostgreSQL'),
    ('binance-intra-arb01', 'spot', '14758579562',
     'Confirmed restart telemetry gap: DOGEUSDT sell trade 1585764019 at 2026-08-08 17:28:02.580 UTC; trade retained in PostgreSQL'),
    ('binance-intra-arb01', 'swap', '8389766250962931489',
     'Confirmed restart telemetry gap: ETHUSDT buy trade 8549761105 at 2026-08-08 17:28:22.372 UTC; trade retained in PostgreSQL'),
    ('binance-intra-arb01', 'spot', '3730174285',
     'Confirmed restart telemetry gap: VETUSDT sell trade 273424023 at 2026-08-08 17:28:31.466 UTC; trade retained in PostgreSQL')
ON CONFLICT DO NOTHING;
