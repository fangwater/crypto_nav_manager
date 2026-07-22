# crypto_nav_manager

A read-only crypto net asset value management system. It includes a multi-source
IP REST dispatcher plus authenticated REST clients for Binance, Gate, Bitget,
and OKX.

The dispatcher is distilled from `mkt_signal`'s trade-engine REST path. Exchange
clients add signing, pagination, time-window splitting, response validation, and
account-mode checks. The exchange clients expose no order placement, transfer,
borrowing, or other mutating API.

Trade fills include their historically charged fees. The four exchange clients
also expose read-only account fee-rate queries for estimating future maker/taker
costs; see docs/api-coverage.md for the endpoint and account-mode details.

## Dispatcher behavior

- Builds one connection-pooled `reqwest::Client` per local source IP.
- Atomically reserves request weight before sending, so cloned dispatchers are
  safe to use from concurrent tasks.
- Selects the IP with the most remaining quota; equal candidates rotate.
- Reads an optional exchange used-weight response header to correct local usage.
- On HTTP 429/418, honors numeric `Retry-After`, blocks that IP, and can retry on
  another IP.
- Does not retry transport errors, because an automatic retry could duplicate a
  non-idempotent order.

The listed IPs must already exist on a local network interface and have valid
routes. Binding a source IP does not create extra public IPs by itself; outbound
NAT must map them to distinct public addresses for IP-based exchange limits.

## Exchange-aware source IPs

PostgreSQL keeps the startup configuration in two tables:

- `rest_egress_ips` lists source IPs that REST clients may bind.
- `rest_egress_ip_envs` records only `IP + env + exchange` occupancy. It
  intentionally does not store component names or PIDs.

An exchange command reads the tables once before constructing its `Dispatcher`.
It excludes every IP occupied by an env on the same exchange, then keeps the
result as an in-memory pool for the lifetime of the process. Database changes
take effect only after that process restarts. With the seeded snapshot, Binance
uses `.232/.233/.234`, Gate uses `.229/.230/.234`, and Bybit can use all
seven local IPs because its envs run on another host.

Update occupancy with ordinary PostgreSQL statements. For example:

```sql
INSERT INTO rest_egress_ip_envs (ip, env, exchange)
VALUES ('172.31.35.234', 'binance_example', 'binance')
ON CONFLICT (ip, env) DO UPDATE
SET exchange = EXCLUDED.exchange,
    updated_at = CURRENT_TIMESTAMP;

DELETE FROM rest_egress_ip_envs
WHERE ip = '172.31.35.234'
  AND env = 'binance_example';
```

The `--local-ip` option remains an explicit startup override for diagnostics
and emergencies; when supplied, the program does not query the exchange pool.

## Account modes

- Binance USD-M Futures
- Binance Portfolio Margin
- Gate Unified Account
- Bitget UTA v3
- OKX Unified Account

See [docs/api-coverage.md](docs/api-coverage.md) for the nine notebook/account
profiles and the exact REST endpoint coverage.

## Database

PostgreSQL is the only application database. On this host the service connects
through the local Unix socket as the Linux/PostgreSQL user `ubuntu`, so no
database password is required. Set `CRYPTO_NAV_DATABASE_URL` to override the
connection for another deployment.

`strategy_envs` is an index of strategy aliases, execution hosts, env files,
CSV output directories, strategy run start time (`st_ms`), and per-strategy
PostgreSQL schemas. Strategy schemas
contain four independent business datasets: trades, funding fees, borrow
interest, and trading fee rates. Profiles are migrated to their existing Liang Torch names one at a
time so their CSV consumers do not need a compatibility translation.

The Binance FR profiles use per-strategy `trades` tables with the exact Liang
Torch columns from `trades_YYYY-MM-DD.csv`:

```text
sid,key,symbol,id,orderId,side,price,qty,amountu,fees,commissionAsset,realizedPnl,ts,ttype,positionSide
```

Import or refresh one profile's historical CSV files with:

```bash
cargo run --release --bin import_binance_fr_history -- --strategy binance_fr_arb01
```

The supported strategy values are `binance_fr_arb01` through
`binance_fr_arb04`. The import is idempotent on `(key, symbol, id)`. REST
ingestion must derive its cursor independently for each `(key, symbol)` from
the latest row:

```sql
SELECT id, ts
FROM binance_fr_arb01.trades
WHERE key = $1 AND symbol = $2
ORDER BY ts DESC, id DESC
LIMIT 1;
```

Sync any registered FR, intra, or market-making strategy with the unified
history command:

```bash
# First scan of an empty dataset.
cargo run --release --bin sync_history -- \
  --strategy binance_fr_arb01 \
  --full \
  --symbol BTCUSDT

# Subsequent incremental scan.
cargo run --release --bin sync_history -- \
  --strategy binance_fr_arb01 \
  --symbol BTCUSDT
```

The command infers the exchange, account mode, and strategy class from
`strategy_envs`. Incremental scans overlap the last successful scan by 30
minutes and upsert with exchange-native record IDs. Market-making strategies
and Binance intra strategies do not query interest. Repeat `--strategy` to
scan multiple accounts in one invocation.

Export one strategy's stored funding history from PostgreSQL with:

```bash
cargo run --release --bin export_binance_fr_funding -- \
  --strategy binance_fr_arb01
```

The exporter does not call Binance and has no export-mode flag. It always writes
`funding_YYYY-MM-DD.csv` files to the process current directory using Liang
Torch's existing 12-column funding format. The CSV `account` value is derived
from the registered strategy alias, so `binance nova02` becomes
`binance_nova02`.

The service runs embedded PostgreSQL migrations at startup. SQLite and
`CRYPTO_NAV_DB_PATH` are not supported.

## CLI example

```bash
cargo run --release --bin rest_dispatcher -- \
  --local-ip 10.0.0.10,10.0.0.11 \
  --max-weight-per-minute 1200 \
  --observed-weight-header x-mbx-used-weight-1m \
  --weight 1 \
  https://api.example.com/v1/time
```

Headers and a request body are also supported:

```bash
cargo run --release --bin rest_dispatcher -- \
  --local-ip 10.0.0.10 \
  --method POST \
  --header 'content-type:application/json' \
  --body '{"symbol":"BTCUSDT"}' \
  https://api.example.com/v1/order
```

The CLI is a dispatcher diagnostic tool. Application code should construct one
of `BinanceClient`, `GateClient`, `BitgetClient`, or `OkxClient`; those clients
perform exchange signing and route every request through a `Dispatcher`.

A `Dispatcher` is cheap to clone and all clones share quota and cooldown state.
Accounts using the same exchange and egress-IP pool should receive clones of the
same dispatcher so IP-level quota accounting is shared across those accounts.
