#!/usr/bin/env python3
"""One-time Bybit UTA spot and linear trade backfill from strategy st_ms."""

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


API_URL = "https://api.bybit.com/v5/execution/list"
WINDOW_MS = 3 * 24 * 60 * 60 * 1000
PAGE_LIMIT = 100
RECV_WINDOW_MS = "20000"
SUPPORTED_STRATEGIES = ("bybit-intra-arb01", "bybit-intra-arb02")


class SourceAddressAdapter(HTTPAdapter):
    def __init__(self, source_ip: str):
        self.source_ip = source_ip
        super().__init__()

    def init_poolmanager(self, connections, maxsize, block=False, **pool_kwargs):
        pool_kwargs["source_address"] = (self.source_ip, 0)
        return super().init_poolmanager(
            connections, maxsize, block=block, **pool_kwargs
        )


def parse_args():
    parser = argparse.ArgumentParser(
        description="Backfill Bybit spot and linear trades from strategy st_ms"
    )
    parser.add_argument("--strategy", choices=SUPPORTED_STRATEGIES, required=True)
    parser.add_argument("--local-ip", required=True)
    parser.add_argument("--end-ms", type=int)
    parser.add_argument("--database", default="crypto_nav_manager")
    return parser.parse_args()


def require_env(name):
    value = os.environ.get(name, "")
    if not value:
        raise RuntimeError(f"{name} is not set")
    return value


def strategy_storage(database, strategy):
    sql = (
        "SELECT db_schema || '|' || st_ms::text FROM strategy_envs "
        f"WHERE slug = '{strategy}'"
    )
    output = subprocess.check_output(
        ["psql", "-X", "-A", "-t", "-d", database, "-c", sql],
        text=True,
    ).strip()
    if not output or "|" not in output:
        raise RuntimeError(f"strategy not found in PostgreSQL: {strategy}")
    schema, start_ms = output.split("|", 1)
    if schema not in {"bybit_intra_arb01", "bybit_intra_arb02"}:
        raise RuntimeError(f"unexpected Bybit schema: {schema}")
    return schema, int(start_ms)


def signed_get(session, api_key, secret, params):
    query = urlencode(params)
    timestamp = str(int(time.time() * 1000))
    payload = f"{timestamp}{api_key}{RECV_WINDOW_MS}{query}".encode()
    signature = hmac.new(secret.encode(), payload, hashlib.sha256).hexdigest()
    response = session.get(
        f"{API_URL}?{query}",
        headers={
            "X-BAPI-API-KEY": api_key,
            "X-BAPI-TIMESTAMP": timestamp,
            "X-BAPI-RECV-WINDOW": RECV_WINDOW_MS,
            "X-BAPI-SIGN": signature,
        },
        timeout=(10, 30),
    )
    response.raise_for_status()
    body = response.json()
    if body.get("retCode") != 0:
        raise RuntimeError(
            f"Bybit error {body.get('retCode')}: {body.get('retMsg')}"
        )
    return body


def decimal_text(value, field, allow_empty=False):
    text = "" if value is None else str(value)
    if allow_empty and not text:
        return None
    try:
        number = Decimal(text)
    except InvalidOperation as error:
        raise RuntimeError(f"invalid {field}: {text!r}") from error
    return format(number, "f")


def normalize(fill, category):
    if fill.get("execType") not in ("", "Trade"):
        raise RuntimeError(f"unexpected execType: {fill.get('execType')}")
    symbol = str(fill.get("symbol", "")).upper().replace("-", "")
    trade_id = str(fill.get("execId", ""))
    order_id = str(fill.get("orderId", ""))
    side = str(fill.get("side", "")).lower()
    price = decimal_text(fill.get("execPrice"), "execPrice")
    qty = decimal_text(fill.get("execQty"), "execQty")
    ts = int(fill.get("execTime", 0))
    if not symbol or not trade_id or not order_id:
        raise RuntimeError("Bybit execution is missing symbol/execId/orderId")
    if side not in ("buy", "sell") or ts <= 0:
        raise RuntimeError(f"invalid side/time for execId {trade_id}")

    amountu = str(fill.get("execValue", ""))
    if amountu:
        amountu = decimal_text(amountu, "execValue")
    else:
        amountu = format(Decimal(price) * Decimal(qty), "f")
    fees = decimal_text(fill.get("execFee", "0") or "0", "execFee")
    if category == "spot":
        sid, key = 1, "bybitspot"
        fallback_asset = symbol[:-4] if symbol.endswith("USDT") else "USDT"
    else:
        sid, key = 0, "bybitswap"
        fallback_asset = "USDT"
    commission_asset = str(
        fill.get("feeCurrency") or fill.get("feeCcy") or fallback_asset
    ).upper()
    realized_pnl = decimal_text(
        fill.get("execPnl"), "execPnl", allow_empty=True
    )
    is_maker = str(fill.get("isMaker", "")).lower() in ("true", "1")
    return {
        "sid": sid,
        "key": key,
        "symbol": symbol,
        "id": trade_id,
        "orderId": order_id,
        "side": side,
        "price": price,
        "qty": qty,
        "amountu": amountu,
        "fees": fees,
        "commissionAsset": commission_asset,
        "realizedPnl": realized_pnl,
        "ts": ts,
        "ttype": "maker" if is_maker else "taker",
        "positionSide": "BOTH",
    }


def fetch_all(session, api_key, secret, start_ms, end_ms):
    rows = {}
    for category in ("spot", "linear"):
        window_start = start_ms
        while window_start <= end_ms:
            window_end = min(window_start + WINDOW_MS - 1, end_ms)
            cursor = None
            seen_cursors = set()
            pages = 0
            fetched = 0
            while True:
                params = [
                    ("category", category),
                    ("startTime", str(window_start)),
                    ("endTime", str(window_end)),
                    ("limit", str(PAGE_LIMIT)),
                    ("execType", "Trade"),
                ]
                if cursor:
                    params.append(("cursor", cursor))
                result = (signed_get(session, api_key, secret, params).get("result") or {})
                page = result.get("list") or []
                for fill in page:
                    row = normalize(fill, category)
                    rows[(row["key"], row["symbol"], row["id"])] = row
                pages += 1
                fetched += len(page)
                next_cursor = (
                    result.get("nextPageCursor")
                    or result.get("nextPageToken")
                    or ""
                )
                if not page or not next_cursor:
                    break
                if next_cursor in seen_cursors:
                    raise RuntimeError("Bybit execution cursor did not advance")
                seen_cursors.add(next_cursor)
                cursor = next_cursor
                time.sleep(0.02)
            print(
                f"{category} {window_start}..{window_end}: "
                f"pages={pages}, fetched={fetched}, unique_total={len(rows)}",
                flush=True,
            )
            window_start = window_end + 1
    return sorted(rows.values(), key=lambda row: (row["ts"], row["key"], row["id"]))


def copy_to_postgres(database, schema, rows):
    process = subprocess.Popen(
        ["psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", database],
        stdin=subprocess.PIPE,
        text=True,
    )
    if process.stdin is None:
        raise RuntimeError("failed to open psql stdin")
    stream = process.stdin
    stream.write("BEGIN;\n")
    stream.write(f"CREATE TEMP TABLE trades_import (LIKE {schema}.trades);\n")
    columns = (
        'sid, key, symbol, id, "orderId", side, price, qty, amountu, fees, '
        '"commissionAsset", "realizedPnl", ts, ttype, "positionSide"'
    )
    stream.write(f"COPY trades_import ({columns}) FROM STDIN WITH (FORMAT csv);\n")
    writer = csv.writer(stream, lineterminator="\n")
    for row in rows:
        writer.writerow([row[name] for name in (
            "sid", "key", "symbol", "id", "orderId", "side", "price", "qty",
            "amountu", "fees", "commissionAsset", "realizedPnl", "ts", "ttype",
            "positionSide",
        )])
    stream.write("\\.\n")
    stream.write(
        f"INSERT INTO {schema}.trades ({columns}) SELECT {columns} FROM trades_import "
        "ON CONFLICT (key, symbol, id) DO UPDATE SET "
        'sid=EXCLUDED.sid, "orderId"=EXCLUDED."orderId", side=EXCLUDED.side, '
        "price=EXCLUDED.price, qty=EXCLUDED.qty, amountu=EXCLUDED.amountu, "
        'fees=EXCLUDED.fees, "commissionAsset"=EXCLUDED."commissionAsset", '
        '"realizedPnl"=EXCLUDED."realizedPnl", ts=EXCLUDED.ts, '
        'ttype=EXCLUDED.ttype, "positionSide"=EXCLUDED."positionSide";\n'
    )
    stream.write("COMMIT;\n")
    stream.close()
    if process.wait() != 0:
        raise RuntimeError("psql trade import failed")


def main():
    args = parse_args()
    schema, start_ms = strategy_storage(args.database, args.strategy)
    end_ms = args.end_ms or int(time.time() * 1000)
    if start_ms <= 0:
        raise RuntimeError(f"invalid strategy st_ms: {start_ms}")
    if start_ms > end_ms:
        raise RuntimeError("strategy st_ms must not exceed end-ms")
    session = requests.Session()
    session.trust_env = False
    session.mount("https://", SourceAddressAdapter(args.local_ip))
    rows = fetch_all(
        session,
        require_env("BYBIT_API_KEY"),
        require_env("BYBIT_API_SECRET"),
        start_ms,
        end_ms,
    )
    copy_to_postgres(args.database, schema, rows)
    print(
        f"trade import complete: strategy={args.strategy}, "
        f"start_ms={start_ms}, end_ms={end_ms}, rows={len(rows)}"
    )


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        raise
