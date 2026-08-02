CREATE TABLE rocksdb_alignment_trade_exclusions (
    strategy_slug TEXT NOT NULL
        REFERENCES strategy_envs(slug) ON DELETE CASCADE,
    market TEXT NOT NULL CHECK (market IN ('spot', 'swap')),
    order_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (strategy_slug, market, order_id)
);

COMMENT ON TABLE rocksdb_alignment_trade_exclusions IS
    'Exchange orders proven not to belong to a strategy and excluded from RocksDB fill reconciliation.';

INSERT INTO rocksdb_alignment_trade_exclusions (
    strategy_slug,
    market,
    order_id,
    reason
) VALUES
    ('bybit-intra-arb01', 'swap', '00f42882-52ec-4dc4-a4e2-f58efc3ab4e1',
     'External Bybit market order with empty orderLinkId during 2026-08-02 disk recovery'),
    ('bybit-intra-arb01', 'swap', '9cc1cfd6-ae69-4bff-9143-183de4ee6683',
     'External Bybit market order with empty orderLinkId during 2026-08-02 disk recovery'),
    ('bybit-intra-arb01', 'swap', 'e3a911ee-e444-40cd-bbe5-c8ebae5dc845',
     'External Bybit market order with empty orderLinkId during 2026-08-02 disk recovery'),
    ('bybit-intra-arb01', 'swap', '79b34a85-669d-4e6b-856e-e2585617460a',
     'External Bybit market order with empty orderLinkId during 2026-08-02 disk recovery')
ON CONFLICT DO NOTHING;
