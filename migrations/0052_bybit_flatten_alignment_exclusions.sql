INSERT INTO rocksdb_alignment_trade_exclusions (
    strategy_slug,
    market,
    order_id,
    reason
) VALUES
    ('bybit-intra-arb01', 'swap', 'b395188d-85e3-454b-9191-77a2492e69b4',
     'Confirmed flatten order: HOMEUSDT buy 23000 on 2026-08-14; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', '05ed8ab6-9b99-41c8-b212-a9da81785f5f',
     'Confirmed flatten order: TRXUSDT buy 127 on 2026-08-14; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', '3c4c8b6e-6339-4c47-9495-7f0ad1012560',
     'Confirmed flatten order: SOLUSDT buy 0.5 on 2026-08-14; trade retained in PostgreSQL')
ON CONFLICT DO NOTHING;
