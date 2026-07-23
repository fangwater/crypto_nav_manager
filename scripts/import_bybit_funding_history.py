#!/usr/bin/env python3
"""One-time Bybit UTA SETTLEMENT backfill into PostgreSQL."""

import argparse
import csv
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from urllib.parse import urlencode

import requests
from requests.adapters import HTTPAdapter


API_URL = "https://api.bybit.com/v5/account/transaction-log"
DAY_MS = 24 * 60 * 60 * 1000
MAX_WINDOW_MS = 7 * DAY_MS
PAGE_LIMIT = 50
RECV_WINDOW_MS = "20000"
DEFAULT_START_MS = 1767225600000  # 2026-01-01T00:00:00Z
STRATEGY_SCHEMAS = {
    "bybit-intra-arb01": "bybit_intra_arb01",
    "bybit-intra-arb02": "bybit_intra_arb02",
}


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
        description="Backfill Bybit UTA funding from a fixed historical start"
    )
    parser.add_argument("--strategy", choices=sorted(STRATEGY_SCHEMAS), required=True)
    parser.add_argument("--local-ip", required=True)
    parser.add_argument("--start-ms", type=int, default=DEFAULT_START_MS)
    parser.add_argument("--end-ms", type=int)
    parser.add_argument("--database", default="crypto_nav_manager")
    return parser.parse_args()


def require_env(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise RuntimeError(f"{name} is not set")
    return value


def signed_get(session, api_key, secret, params):
    query = urlencode(params)
    timestamp = str(int(time.time() * 1000))
    payload = f"{timestamp}{api_key}{RECV_WINDOW_MS}{query}".encode()
    signature = hmac.new(secret.encode(), payload, hashlib.sha256).hexdigest()
    headers = {
        "X-BAPI-API-KEY": api_key,
        "X-BAPI-TIMESTAMP": timestamp,
        "X-BAPI-RECV-WINDOW": RECV_WINDOW_MS,
        "X-BAPI-SIGN": signature,
    }
    response = session.get(
        f"{API_URL}?{query}", headers=headers, timeout=(10, 30)
    )
    response.raise_for_status()
    body = response.json()
    if body.get("retCode") != 0:
        raise RuntimeError(
            f"Bybit error {body.get('retCode')}: {body.get('retMsg')}"
        )
    return body


def normalize(row):
    if row.get("type") != "SETTLEMENT":
        raise RuntimeError(f"unexpected transaction type: {row.get('type')}")
    values = {
        "id": str(row.get("id", "")),
        "symbol": str(row.get("symbol", "")),
        "funding": str(row.get("funding", "")),
        "transactionTime": int(row.get("transactionTime", 0)),
    }
    if not values["id"] or not values["symbol"] or not values["funding"]:
        raise RuntimeError(f"incomplete Bybit funding row: {json.dumps(values)}")
    if values["transactionTime"] < 0:
        raise RuntimeError(f"negative transactionTime for {values['id']}")
    return values


def fetch_all(session, api_key, secret, start_ms, end_ms):
    rows = {}
    window_start = start_ms
    while window_start <= end_ms:
        window_end = min(window_start + MAX_WINDOW_MS - 1, end_ms)
        cursor = None
        seen_cursors = set()
        pages = 0
        fetched = 0
        while True:
            params = [
                ("accountType", "UNIFIED"),
                ("category", "linear"),
                ("type", "SETTLEMENT"),
                ("startTime", str(window_start)),
                ("endTime", str(window_end)),
                ("limit", str(PAGE_LIMIT)),
            ]
            if cursor:
                params.append(("cursor", cursor))
            body = signed_get(session, api_key, secret, params)
            result = body.get("result") or {}
            page = result.get("list") or []
            for raw in page:
                row = normalize(raw)
                rows[row["id"]] = row
            pages += 1
            fetched += len(page)
            next_cursor = result.get("nextPageCursor") or ""
            if not page or not next_cursor:
                break
            if next_cursor in seen_cursors:
                raise RuntimeError("Bybit pagination cursor did not advance")
            seen_cursors.add(next_cursor)
            cursor = next_cursor
            time.sleep(0.05)
        print(
            f"window {format_ms(window_start)} .. {format_ms(window_end)}: "
            f"pages={pages}, fetched={fetched}, unique_total={len(rows)}",
            flush=True,
        )
        window_start = window_end + 1
    return sorted(
        rows.values(), key=lambda row: (row["transactionTime"], row["id"])
    )


def copy_to_postgres(database, schema, rows):
    command = ["psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", database]
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        text=True,
    )
    if process.stdin is None:
        raise RuntimeError("failed to open psql stdin")
    stream = process.stdin
    stream.write("BEGIN;\n")
    stream.write(
        f'CREATE TEMP TABLE funding_import (LIKE {schema}.funding);\n'
    )
    stream.write(
        'COPY funding_import (id, symbol, funding, "transactionTime") '
        "FROM STDIN WITH (FORMAT csv);\n"
    )
    writer = csv.writer(stream, lineterminator="\n")
    for row in rows:
        writer.writerow(
            [row["id"], row["symbol"], row["funding"], row["transactionTime"]]
        )
    stream.write("\\.\n")
    stream.write(
        f'INSERT INTO {schema}.funding '
        '(id, symbol, funding, "transactionTime") '
        'SELECT id, symbol, funding, "transactionTime" FROM funding_import '
        "ON CONFLICT (id) DO UPDATE SET "
        "symbol = EXCLUDED.symbol, funding = EXCLUDED.funding, "
        '"transactionTime" = EXCLUDED."transactionTime";\n'
    )
    stream.write("COMMIT;\n")
    stream.close()
    return_code = process.wait()
    if return_code != 0:
        raise RuntimeError(f"psql import failed with exit code {return_code}")


def format_ms(value):
    return datetime.fromtimestamp(value / 1000, timezone.utc).isoformat()


def main():
    args = parse_args()
    if args.start_ms < 0:
        raise RuntimeError("start-ms must be non-negative")
    end_ms = args.end_ms or int(time.time() * 1000)
    if args.start_ms > end_ms:
        raise RuntimeError("start-ms must not exceed end-ms")

    session = requests.Session()
    session.trust_env = False
    session.mount("https://", SourceAddressAdapter(args.local_ip))
    rows = fetch_all(
        session,
        require_env("BYBIT_API_KEY"),
        require_env("BYBIT_API_SECRET"),
        args.start_ms,
        end_ms,
    )
    schema = STRATEGY_SCHEMAS[args.strategy]
    copy_to_postgres(args.database, schema, rows)
    print(
        f"import complete: strategy={args.strategy}, rows={len(rows)}, "
        f"range={format_ms(args.start_ms)} .. {format_ms(end_ms)}"
    )


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        raise
