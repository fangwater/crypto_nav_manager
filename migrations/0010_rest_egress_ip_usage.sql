CREATE TABLE rest_egress_ips (
    ip INET PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (family(ip) = 4 AND masklen(ip) = 32)
);

CREATE TABLE rest_egress_ip_envs (
    ip INET NOT NULL REFERENCES rest_egress_ips(ip) ON DELETE CASCADE,
    env TEXT NOT NULL CHECK (env ~ '^[a-z0-9][a-z0-9_-]*$'),
    exchange TEXT NOT NULL CHECK (exchange ~ '^[a-z0-9][a-z0-9_-]*$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (ip, env)
);

CREATE INDEX rest_egress_ip_envs_exchange_idx
    ON rest_egress_ip_envs(exchange, ip);

COMMENT ON TABLE rest_egress_ips IS
    'Local source IPs available to REST clients';
COMMENT ON TABLE rest_egress_ip_envs IS
    'Strategy envs currently using each local source IP; process names and PIDs are intentionally excluded';

INSERT INTO rest_egress_ips (ip, note)
VALUES
    ('172.31.35.228', 'default outbound IP'),
    ('172.31.35.229', ''),
    ('172.31.35.230', ''),
    ('172.31.35.231', ''),
    ('172.31.35.232', ''),
    ('172.31.35.233', ''),
    ('172.31.35.234', 'currently unused');

INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES
    ('172.31.35.228', 'gate_fr_arb01', 'gate'),
    ('172.31.35.228', 'gate_fr_arb02', 'gate'),
    ('172.31.35.228', 'bitget_fr_arb02', 'bitget'),
    ('172.31.35.228', 'okex_fr_arb01', 'okx'),
    ('172.31.35.228', 'okex-intra-arb01', 'okx'),
    ('172.31.35.228', 'bitget-intra-arb01', 'bitget'),
    ('172.31.35.228', 'binance-intra-arb01', 'binance'),
    ('172.31.35.229', 'binance_mm_alpha', 'binance'),
    ('172.31.35.230', 'binance_mm_alpha', 'binance'),
    ('172.31.35.231', 'binance_fr_arb01', 'binance'),
    ('172.31.35.231', 'binance_fr_arb02', 'binance'),
    ('172.31.35.231', 'binance_fr_arb03', 'binance'),
    ('172.31.35.231', 'binance_fr_arb04', 'binance'),
    ('172.31.35.231', 'bitget_fr_arb02', 'bitget'),
    ('172.31.35.231', 'gate_fr_arb01', 'gate'),
    ('172.31.35.231', 'okex_fr_arb01', 'okx'),
    ('172.31.35.232', 'gate_fr_arb02', 'gate'),
    ('172.31.35.233', 'gate_fr_arb02', 'gate');
