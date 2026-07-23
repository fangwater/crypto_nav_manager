#!/usr/bin/env python3
"""One-time Binance MT trade, funding, and margin-interest backfill."""

import argparse
import csv
import hashlib
import hmac
import os
import subprocess
import sys
import time
from decimal import Decimal, InvalidOperation
from urllib.parse import urlencode

import requests
from requests.adapters import HTTPAdapter

SPOT_BASE = "https://api.binance.com"
FAPI_BASE = "https://fapi.binance.com"
DAY_MS = 86_400_000
LIMIT = 1000
STRATEGY = "binance-intra-arb01"
SCHEMA = "binance_intra_arb01"
DEFAULT_SYMBOLS = (
    "SYNUSDT", "IOTXUSDT", "TONUSDT", "ZENUSDT", "QNTUSDT",
    "VETUSDT", "KNCUSDT", "MANAUSDT", "ZAMAUSDT", "SUNUSDT",
    "IOTAUSDT", "AIGENSYNUSDT", "BTCUSDT", "ETHUSDT", "BNBUSDT",
    "SOLUSDT", "XRPUSDT", "DOGEUSDT",
)


class SourceAddressAdapter(HTTPAdapter):
    def __init__(self, source_ip):
        self.source_ip = source_ip
        super().__init__()

    def init_poolmanager(self, connections, maxsize, block=False, **kwargs):
        kwargs["source_address"] = (self.source_ip, 0)
        return super().init_poolmanager(connections, maxsize, block=block, **kwargs)


class BinanceApiError(RuntimeError):
    def __init__(self, path, code, message):
        self.code = code
        super().__init__(f"Binance {path} error {code}: {message}")


def parse_args():
    parser = argparse.ArgumentParser(description="Backfill Binance MT from st_ms")
    parser.add_argument(
        "--dataset", choices=("all", "trades", "funding", "interest"),
        default="all",
    )
    parser.add_argument("--local-ip", required=True)
    parser.add_argument("--symbol", action="append", default=[])
    parser.add_argument("--end-ms", type=int)
    parser.add_argument("--database", default="crypto_nav_manager")
    return parser.parse_args()


def require_env(name):
    value = os.environ.get(name, "")
    if not value:
        raise RuntimeError(f"{name} is not set")
    return value


def psql_scalar(database, sql):
    return subprocess.check_output(
        ["psql", "-X", "-A", "-t", "-d", database, "-c", sql], text=True
    ).strip()


def strategy_storage(database):
    output = psql_scalar(
        database,
        "SELECT db_schema || '|' || st_ms::text FROM strategy_envs "
        f"WHERE slug = '{STRATEGY}'",
    )
    if "|" not in output:
        raise RuntimeError(f"strategy not found: {STRATEGY}")
    schema, start_ms = output.split("|", 1)
    if schema != SCHEMA:
        raise RuntimeError(f"unexpected schema: {schema}")
    return schema, int(start_ms)


def stored_funding_symbols(database, schema):
    output = psql_scalar(
        database, f"SELECT DISTINCT symbol FROM {schema}.funding ORDER BY symbol"
    )
    return tuple(line for line in output.splitlines() if line)


def signed_get(session, key, secret, base, path, params):
    params = list(params) + [
        ("timestamp", str(int(time.time() * 1000))), ("recvWindow", "60000")
    ]
    query = urlencode(params)
    signature = hmac.new(secret.encode(), query.encode(), hashlib.sha256).hexdigest()
    response = session.get(
        f"{base}{path}?{query}&signature={signature}",
        headers={"X-MBX-APIKEY": key}, timeout=(10, 30),
    )
    body = response.json()
    if isinstance(body, dict) and "code" in body:
        raise BinanceApiError(path, body.get("code"), body.get("msg", ""))
    response.raise_for_status()
    time.sleep(1.0)
    return body


def decimal_text(value, field, allow_empty=False):
    text = "" if value is None else str(value)
    if allow_empty and not text:
        return None
    try:
        return format(Decimal(text), "f")
    except InvalidOperation as error:
        raise RuntimeError(f"invalid {field}: {text!r}") from error


def required_int(row, field):
    try:
        return int(row[field])
    except (KeyError, TypeError, ValueError) as error:
        raise RuntimeError(f"missing numeric {field}") from error


def symbol_text(value):
    return str(value or "").upper().replace("-", "").replace("_", "")


def flag(value):
    return value is True or str(value).lower() in ("true", "1")


def normalize_trade(raw, category):
    symbol = symbol_text(raw.get("symbol"))
    trade_id = required_int(raw, "id")
    price = decimal_text(raw.get("price"), "price")
    qty = decimal_text(raw.get("qty"), "qty")
    ts = required_int(raw, "time")
    if not symbol or Decimal(price) <= 0 or Decimal(qty) <= 0 or ts <= 0:
        raise RuntimeError(f"invalid trade {category}/{symbol}/{trade_id}")
    quote_qty = str(raw.get("quoteQty") or "")
    amountu = decimal_text(quote_qty, "quoteQty") if quote_qty else str(
        Decimal(price) * Decimal(qty)
    )
    if category == "spot":
        sid, key = 1, "binancespot"
        side = "buy" if flag(raw.get("isBuyer")) else "sell"
        maker = flag(raw.get("isMaker"))
        realized_pnl = None
        position_side = "BOTH"
    else:
        sid, key = 0, "binanceswap"
        side = str(raw.get("side") or "").lower()
        maker = flag(raw.get("maker"))
        realized_pnl = decimal_text(
            raw.get("realizedPnl"), "realizedPnl", allow_empty=True
        )
        position_side = str(raw.get("positionSide") or "BOTH").upper()
    if side not in ("buy", "sell"):
        raise RuntimeError(f"invalid side for {category}/{symbol}/{trade_id}")
    return {
        "sid": sid, "key": key, "symbol": symbol, "id": trade_id,
        "orderId": required_int(raw, "orderId"), "side": side,
        "price": price, "qty": qty, "amountu": amountu,
        "fees": decimal_text(raw.get("commission", "0") or "0", "commission"),
        "commissionAsset": str(raw.get("commissionAsset") or "USDT").upper(),
        "realizedPnl": realized_pnl, "ts": ts,
        "ttype": "maker" if maker else "taker",
        "positionSide": position_side,
    }


def fetch_symbol_trades(session, key, secret, symbol, category, start_ms, end_ms):
    if category == "spot":
        base, path, span = SPOT_BASE, "/api/v3/myTrades", DAY_MS
    else:
        base, path, span = FAPI_BASE, "/fapi/v1/userTrades", 7 * DAY_MS
    result = {}
    start = start_ms
    pages = 0
    while start <= end_ms:
        end = min(start + span - 1, end_ms)
        try:
            page = signed_get(
                session, key, secret, base, path,
                [("symbol", symbol), ("startTime", str(start)),
                 ("endTime", str(end)), ("limit", str(LIMIT))],
            )
        except BinanceApiError as error:
            if error.code == -1121:
                return []
            raise
        last_seen = None
        while True:
            if not isinstance(page, list):
                raise RuntimeError(f"{path} response is not a list")
            pages += 1
            for raw in page:
                ts = required_int(raw, "time")
                if start <= ts <= end:
                    row = normalize_trade(raw, category)
                    result[(row["key"], row["symbol"], row["id"])] = row
            if len(page) < LIMIT:
                break
            last_id = max(required_int(raw, "id") for raw in page)
            if last_seen is not None and last_id <= last_seen:
                raise RuntimeError(f"{path} fromId did not advance")
            last_seen = last_id
            if any(required_int(raw, "time") > end for raw in page):
                break
            page = signed_get(
                session, key, secret, base, path,
                [("symbol", symbol), ("fromId", str(last_id + 1)),
                 ("limit", str(LIMIT))],
            )
        start = end + 1
    rows = sorted(result.values(), key=lambda row: (row["ts"], row["id"]))
    print(f"{category} {symbol}: pages={pages}, rows={len(rows)}", flush=True)
    return rows


def fetch_trades(session, key, secret, symbols, start_ms, end_ms):
    result = {}
    for symbol in symbols:
        for category in ("spot", "um"):
            for row in fetch_symbol_trades(
                session, key, secret, symbol, category, start_ms, end_ms
            ):
                result[(row["key"], row["symbol"], row["id"])] = row
    return sorted(result.values(), key=lambda row: (row["ts"], row["key"], row["id"]))


def fetch_funding(session, key, secret, start_ms, end_ms):
    result = {}
    start = start_ms
    while start <= end_ms:
        end = min(start + 7 * DAY_MS - 1, end_ms)
        cursor = start
        pages = fetched = 0
        while cursor <= end:
            page = signed_get(
                session, key, secret, FAPI_BASE, "/fapi/v1/income",
                [("incomeType", "FUNDING_FEE"), ("startTime", str(cursor)),
                 ("endTime", str(end)), ("limit", str(LIMIT))],
            )
            if not isinstance(page, list):
                raise RuntimeError("funding response is not a list")
            pages += 1
            fetched += len(page)
            for raw in page:
                if raw.get("incomeType") != "FUNDING_FEE":
                    raise RuntimeError("unexpected incomeType")
                if str(raw.get("asset") or "").upper() != "USDT":
                    raise RuntimeError("funding asset is not USDT")
                row = {
                    "tranId": required_int(raw, "tranId"),
                    "symbol": symbol_text(raw.get("symbol")),
                    "income": decimal_text(raw.get("income"), "income"),
                    "time": required_int(raw, "time"),
                }
                result[row["tranId"]] = row
            if len(page) < LIMIT:
                break
            cursor_next = max(required_int(row, "time") for row in page) + 1
            if cursor_next <= cursor:
                raise RuntimeError("funding cursor did not advance")
            cursor = cursor_next
        print(f"funding {start}..{end}: pages={pages}, fetched={fetched}", flush=True)
        start = end + 1
    return sorted(result.values(), key=lambda row: (row["time"], row["tranId"]))


def fetch_interest(session, key, secret, start_ms, end_ms):
    result = {}
    start = start_ms
    while start <= end_ms:
        end = min(start + 30 * DAY_MS - 1, end_ms)
        current = 1
        while True:
            body = signed_get(
                session, key, secret, SPOT_BASE, "/sapi/v1/margin/interestHistory",
                [("startTime", str(start)), ("endTime", str(end)),
                 ("size", "100"), ("current", str(current))],
            )
            page = body.get("rows") if isinstance(body, dict) else None
            if not isinstance(page, list):
                raise RuntimeError("interest response is missing rows")
            for raw in page:
                row = {
                    "txId": required_int(raw, "txId"),
                    "asset": str(raw.get("asset") or "").upper(),
                    "interest": decimal_text(raw.get("interest"), "interest"),
                    "interestAccuredTime": required_int(raw, "interestAccuredTime"),
                }
                result[row["txId"]] = row
            if len(page) < 100:
                break
            current += 1
        start = end + 1
    return sorted(
        result.values(), key=lambda row: (row["interestAccuredTime"], row["txId"])
    )


def copy_rows(stream, rows, names):
    writer = csv.writer(stream, lineterminator="\n")
    for row in rows:
        writer.writerow([row[name] for name in names])
    stream.write("\\.\n")


def copy_to_postgres(database, schema, datasets):
    process = subprocess.Popen(
        ["psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", database],
        stdin=subprocess.PIPE, text=True,
    )
    stream = process.stdin
    if stream is None:
        raise RuntimeError("failed to open psql stdin")
    stream.write("BEGIN;\n")
    if "trades" in datasets:
        columns = (
            'sid,key,symbol,id,"orderId",side,price,qty,amountu,fees,'
            '"commissionAsset","realizedPnl",ts,ttype,"positionSide"'
        )
        names = (
            "sid", "key", "symbol", "id", "orderId", "side", "price", "qty",
            "amountu", "fees", "commissionAsset", "realizedPnl", "ts", "ttype",
            "positionSide",
        )
        stream.write(f"CREATE TEMP TABLE trades_import (LIKE {schema}.trades);\n")
        stream.write(f"COPY trades_import ({columns}) FROM STDIN WITH (FORMAT csv);\n")
        copy_rows(stream, datasets["trades"], names)
        stream.write(
            f"INSERT INTO {schema}.trades ({columns}) SELECT {columns} FROM trades_import "
            "ON CONFLICT (key,symbol,id) DO UPDATE SET "
            'sid=EXCLUDED.sid,"orderId"=EXCLUDED."orderId",side=EXCLUDED.side,'
            "price=EXCLUDED.price,qty=EXCLUDED.qty,amountu=EXCLUDED.amountu,"
            'fees=EXCLUDED.fees,"commissionAsset"=EXCLUDED."commissionAsset",'
            '"realizedPnl"=EXCLUDED."realizedPnl",ts=EXCLUDED.ts,'
            'ttype=EXCLUDED.ttype,"positionSide"=EXCLUDED."positionSide";\n'
        )
    if "funding" in datasets:
        stream.write(f"CREATE TEMP TABLE funding_import (LIKE {schema}.funding);\n")
        stream.write('COPY funding_import ("tranId",symbol,income,time) FROM STDIN WITH (FORMAT csv);\n')
        copy_rows(stream, datasets["funding"], ("tranId", "symbol", "income", "time"))
        stream.write(
            f'INSERT INTO {schema}.funding ("tranId",symbol,income,time) '
            'SELECT "tranId",symbol,income,time FROM funding_import '
            'ON CONFLICT ("tranId") DO UPDATE SET symbol=EXCLUDED.symbol,'
            'income=EXCLUDED.income,time=EXCLUDED.time;\n'
        )
    if "interest" in datasets:
        stream.write(f"CREATE TEMP TABLE interest_import (LIKE {schema}.interest);\n")
        stream.write('COPY interest_import ("txId",asset,interest,"interestAccuredTime") FROM STDIN WITH (FORMAT csv);\n')
        copy_rows(
            stream, datasets["interest"],
            ("txId", "asset", "interest", "interestAccuredTime"),
        )
        stream.write(
            f'INSERT INTO {schema}.interest ("txId",asset,interest,"interestAccuredTime") '
            'SELECT "txId",asset,interest,"interestAccuredTime" FROM interest_import '
            'ON CONFLICT ("txId") DO UPDATE SET asset=EXCLUDED.asset,'
            'interest=EXCLUDED.interest,'
            '"interestAccuredTime"=EXCLUDED."interestAccuredTime";\n'
        )
    stream.write("COMMIT;\n")
    stream.close()
    if process.wait() != 0:
        raise RuntimeError("psql import failed")


def main():
    args = parse_args()
    schema, start_ms = strategy_storage(args.database)
    end_ms = args.end_ms or int(time.time() * 1000)
    if start_ms <= 0 or start_ms > end_ms:
        raise RuntimeError("invalid st_ms/end-ms range")
    requested = {symbol_text(value) for value in args.symbol if value}
    symbols = tuple(sorted(requested))
    if not symbols:
        symbols = tuple(sorted(
            set(DEFAULT_SYMBOLS) | set(stored_funding_symbols(args.database, schema))
        ))
    session = requests.Session()
    session.trust_env = False
    session.mount("https://", SourceAddressAdapter(args.local_ip))
    key = require_env("BINANCE_API_KEY")
    secret = require_env("BINANCE_API_SECRET")
    wanted = (
        ("funding", "interest", "trades") if args.dataset == "all"
        else (args.dataset,)
    )
    datasets = {}
    for name in wanted:
        if name == "funding":
            datasets[name] = fetch_funding(session, key, secret, start_ms, end_ms)
        elif name == "interest":
            datasets[name] = fetch_interest(session, key, secret, start_ms, end_ms)
        else:
            datasets[name] = fetch_trades(
                session, key, secret, symbols, start_ms, end_ms
            )
    copy_to_postgres(args.database, schema, datasets)
    counts = ", ".join(f"{name}={len(rows)}" for name, rows in datasets.items())
    print(f"Binance MT import complete: {counts}")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        raise
