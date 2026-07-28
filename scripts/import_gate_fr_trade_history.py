#!/usr/bin/env python3
"""Import normalized Gate FR trades from account-history CSV files."""

import argparse
import csv
import glob
import re
import subprocess
from decimal import Decimal, InvalidOperation
from pathlib import Path


SUPPORTED_STRATEGY = "gate_fr_arb01"
EXCLUDED_LEGACY_SYMBOLS = {"BTCUSDT", "ETHUSDT", "BNBUSDT", "DOTUSDT"}


def parse_args():
    parser = argparse.ArgumentParser(
        description="Backfill Gate FR trades from normalized account-history CSVs"
    )
    parser.add_argument("--strategy", choices=(SUPPORTED_STRATEGY,), required=True)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--start-ms", type=int, required=True)
    parser.add_argument("--end-ms", type=int, required=True)
    parser.add_argument("--database", default="crypto_nav_manager")
    return parser.parse_args()


def psql_lines(database, sql):
    output = subprocess.check_output(
        ["psql", "-X", "-A", "-t", "-d", database, "-c", sql],
        text=True,
    )
    return [line for line in output.splitlines() if line]


def strategy_schema(database, strategy):
    rows = psql_lines(
        database,
        "SELECT db_schema FROM strategy_envs "
        f"WHERE slug = '{strategy}'",
    )
    if rows != ["gate_fr_arb01"]:
        raise RuntimeError(f"unexpected strategy schema: {rows!r}")
    return rows[0]


def strategy_symbols(database, schema):
    if not re.fullmatch(r"[a-z][a-z0-9_]*", schema):
        raise RuntimeError(f"invalid PostgreSQL schema: {schema!r}")
    symbols = set(
        psql_lines(database, f"SELECT DISTINCT symbol FROM {schema}.trades")
    )
    return symbols - EXCLUDED_LEGACY_SYMBOLS


def decimal_text(value, field, absolute=False, optional=False):
    text = "" if value is None else str(value).strip()
    if optional and not text:
        return None
    try:
        number = Decimal(text)
    except InvalidOperation as error:
        raise RuntimeError(f"invalid {field}: {text!r}") from error
    if absolute:
        number = abs(number)
    return format(number, "f")


def normalize(row):
    venue = row.get("venue", "").lower()
    if venue == "gatespot":
        market = "spot"
    elif venue == "gateswap":
        market = "swap"
    else:
        raise RuntimeError(f"unsupported Gate venue: {venue!r}")
    symbol = row.get("symbol", "").upper()
    trade_id = row.get("trade_id", "")
    order_id = row.get("order_id", "")
    side = row.get("side", "").lower()
    role = row.get("role", "").lower()
    ts = int(row.get("ts", "0"))
    if not symbol or not trade_id or not order_id or ts <= 0:
        raise RuntimeError(f"invalid Gate trade identity: {row!r}")
    if side not in {"buy", "sell"} or role not in {"maker", "taker"}:
        raise RuntimeError(f"invalid Gate side/role: {side!r}/{role!r}")
    return {
        "market": market,
        "symbol": symbol,
        "trade_id": trade_id,
        "order_id": order_id,
        "side": side,
        "liquidity_role": role,
        "price": decimal_text(row.get("price"), "price"),
        "quantity": decimal_text(row.get("qty"), "qty", absolute=True),
        "quote_quantity": decimal_text(row.get("amountu"), "amountu", absolute=True),
        "fee_amount": decimal_text(row.get("fee_raw", "0"), "fee_raw", absolute=True),
        "fee_asset": row.get("fee_ccy", "").upper() or "USDT",
        "fee_usdt": decimal_text(
            row.get("fee_usdt"), "fee_usdt", absolute=True, optional=True
        ),
        "realized_pnl": decimal_text(
            row.get("realized_pnl_raw"),
            "realized_pnl_raw",
            optional=True,
        ),
        "event_time_ms": ts,
    }


def load_rows(data_dir, symbols, start_ms, end_ms):
    rows = {}
    scanned = 0
    excluded_legacy = 0
    paths = sorted(glob.glob(str(data_dir / "*.csv")))
    if not paths:
        raise RuntimeError(f"no CSV files found in {data_dir}")
    for path in paths:
        with open(path, newline="") as source:
            for raw in csv.DictReader(source):
                scanned += 1
                ts = int(raw.get("ts", "0"))
                symbol = raw.get("symbol", "").upper()
                if not start_ms <= ts < end_ms:
                    continue
                if symbol in EXCLUDED_LEGACY_SYMBOLS:
                    excluded_legacy += 1
                    continue
                if symbol not in symbols:
                    continue
                row = normalize(raw)
                key = (row["market"], row["symbol"], row["trade_id"])
                rows[key] = row
    print(
        f"scanned={scanned}, unique_import={len(rows)}, "
        f"excluded_legacy={excluded_legacy}"
    )
    return sorted(
        rows.values(),
        key=lambda row: (row["event_time_ms"], row["symbol"], row["trade_id"]),
    )


def copy_to_postgres(database, schema, rows):
    process = subprocess.Popen(
        ["psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", database],
        stdin=subprocess.PIPE,
        text=True,
    )
    if process.stdin is None:
        raise RuntimeError("failed to open psql stdin")
    stream = process.stdin
    columns = (
        "market,symbol,trade_id,order_id,side,liquidity_role,price,quantity,"
        "quote_quantity,fee_amount,fee_asset,fee_usdt,realized_pnl,event_time_ms"
    )
    names = columns.split(",")
    stream.write("BEGIN;\n")
    stream.write(f"CREATE TEMP TABLE trades_import (LIKE {schema}.trades);\n")
    stream.write(f"COPY trades_import ({columns}) FROM STDIN WITH (FORMAT csv);\n")
    writer = csv.writer(stream, lineterminator="\n")
    for row in rows:
        writer.writerow([row[name] for name in names])
    stream.write("\\.\n")
    stream.write(
        f"INSERT INTO {schema}.trades ({columns}) "
        f"SELECT {columns} FROM trades_import "
        "ON CONFLICT (market,symbol,trade_id) DO UPDATE SET "
        "order_id=EXCLUDED.order_id,side=EXCLUDED.side,"
        "liquidity_role=EXCLUDED.liquidity_role,price=EXCLUDED.price,"
        "quantity=EXCLUDED.quantity,quote_quantity=EXCLUDED.quote_quantity,"
        "fee_amount=EXCLUDED.fee_amount,fee_asset=EXCLUDED.fee_asset,"
        "fee_usdt=EXCLUDED.fee_usdt,realized_pnl=EXCLUDED.realized_pnl,"
        "event_time_ms=EXCLUDED.event_time_ms;\n"
    )
    stream.write("COMMIT;\n")
    stream.close()
    if process.wait() != 0:
        raise RuntimeError("psql Gate trade import failed")


def main():
    args = parse_args()
    if args.start_ms <= 0 or args.end_ms <= args.start_ms:
        raise RuntimeError("invalid import time range")
    schema = strategy_schema(args.database, args.strategy)
    symbols = strategy_symbols(args.database, schema)
    rows = load_rows(args.data_dir, symbols, args.start_ms, args.end_ms)
    copy_to_postgres(args.database, schema, rows)
    print(
        f"Gate trade import complete: strategy={args.strategy}, "
        f"start_ms={args.start_ms}, end_ms={args.end_ms}, rows={len(rows)}"
    )


if __name__ == "__main__":
    main()
