INSERT INTO rocksdb_alignment_trade_exclusions (
    strategy_slug,
    market,
    order_id,
    reason
) VALUES
    ('bybit-intra-arb01', 'swap', 'efc0fbbe-a329-47e1-addd-fa8fe69eb2bf',
     'Confirmed flatten order: ZKUSDT sell 41695.2 on 2026-08-17; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', '3efbdca4-926d-4547-b4fb-3097255f376c',
     'Confirmed flatten order: ETHUSDT sell 0.16 on 2026-08-17; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', 'acb07fd9-6419-4649-80a3-3540c3f19b2d',
     'Confirmed flatten order: SOLUSDT sell 3.6 on 2026-08-17; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', '5ba50bdc-58a9-4a7f-b34f-e0d23c669995',
     'Confirmed flatten order: BTCUSDT sell 0.004 on 2026-08-17; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', 'f5293019-e7f2-4255-bb9a-0233d96ac18e',
     'Confirmed flatten order: DOGEUSDT sell 3929 on 2026-08-17; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', '52f41598-5f5c-4ddf-85cc-5b807c0c5c9a',
     'Confirmed flatten order: XRPUSDT sell 251.8 on 2026-08-17; trade retained in PostgreSQL'),
    ('bybit-intra-arb01', 'swap', 'cd80e8df-0024-4db9-a7d4-0424ff62575a',
     'Confirmed flatten order: BNBUSDT sell 0.22 on 2026-08-17; trade retained in PostgreSQL')
ON CONFLICT DO NOTHING;
