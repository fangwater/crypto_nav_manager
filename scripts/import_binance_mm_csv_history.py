#!/usr/bin/env python3
"""Import Binance MM notebook trade CSVs into the normalized PostgreSQL table."""

import argparse
import csv
import glob
import re
import subprocess
from decimal import Decimal, InvalidOperation
from pathlib import Path


SUPPORTED_STRATEGY = "binance_mm_alpha"
EXPECTED_SCHEMA = "binance_mm_alpha"
EXPECTED_COLUMNS = (
    "symbol",
    "id",
    "orderId",
    "side",
    "price",
    "qty",
    "amountu",
    "fees",
    "ts",
    "ttype",
    "positionSide",
    "realizedPnl",
)
DATABASE_COLUMNS = (
    "market",
    "symbol",
    "trade_id",
    "order_id",
    "side",
    "liquidity_role",
    "price",
    "quantity",
    "quote_quantity",
    "fee_amount",
    "fee_asset",
    "fee_usdt",
    "realized_pnl",
    "event_time_ms",
)


def parse_args():
    parser = argparse.ArgumentParser(
        description="Import Binance MM notebook CSVs into normalized trades"
    )
    parser.add_argument("--strategy", choices=(SUPPORTED_STRATEGY,), required=True)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--end-ms", type=int)
    parser.add_argument("--database", default="crypto_nav_manager")
    return parser.parse_args()


def psql_lines(database, sql):
    output = subprocess.check_output(
        ["psql", "-X", "-A", "-t", "-d", database, "-c", sql],
        text=True,
    )
    return [line for line in output.splitlines() if line]


def strategy_storage(database, strategy):
    rows = psql_lines(
        database,
        "SELECT db_schema || '|' || st_ms::text || '|' || "
        "COALESCE(csv_output_dir, '') FROM strategy_envs "
        f"WHERE slug = '{strategy}'",
    )
    if len(rows) != 1:
        raise RuntimeError(f"strategy not found in PostgreSQL: {strategy}")
    schema, start_ms, data_dir = rows[0].split("|", 2)
    if schema != EXPECTED_SCHEMA or not re.fullmatch(r"[a-z][a-z0-9_]*", schema):
        raise RuntimeError(f"unexpected PostgreSQL schema: {schema!r}")
    return schema, int(start_ms), Path(data_dir) if data_dir else None


def validate_storage(database, schema):
    actual = psql_lines(
        database,
        "SELECT column_name FROM information_schema.columns "
        f"WHERE table_schema = '{schema}' AND table_name = 'trades' "
        "ORDER BY ordinal_position",
    )
    if tuple(actual) != DATABASE_COLUMNS:
        raise RuntimeError(
            f"{schema}.trades is not the normalized standard schema: {actual!r}"
        )


def decimal_text(value, field, *, positive=False, nonnegative=False, optional=False):
    text = "" if value is None else str(value).strip()
    if optional and not text:
        return None
    try:
        number = Decimal(text)
    except InvalidOperation as error:
        raise RuntimeError(f"invalid {field}: {text!r}") from error
    if not number.is_finite():
        raise RuntimeError(f"non-finite {field}: {text!r}")
    if positive and number <= 0:
        raise RuntimeError(f"non-positive {field}: {text!r}")
    if nonnegative and number < 0:
        raise RuntimeError(f"negative {field}: {text!r}")
    return format(number, "f")


def normalize(raw, path, line_number):
    symbol = str(raw.get("symbol", "")).strip().upper().replace("-", "")
    trade_id = str(raw.get("id", "")).strip()
    order_id = str(raw.get("orderId", "")).strip()
    side = str(raw.get("side", "")).strip().lower()
    role = str(raw.get("ttype", "")).strip().lower()
    position_side = str(raw.get("positionSide", "")).strip().upper()
    try:
        event_time_ms = int(raw.get("ts", "0"))
    except ValueError as error:
        raise RuntimeError(f"invalid ts: {raw.get('ts')!r}") from error
    if not symbol or not trade_id or not order_id or event_time_ms <= 0:
        raise RuntimeError("missing trade identity or timestamp")
    if side not in {"buy", "sell"} or role not in {"maker", "taker"}:
        raise RuntimeError(f"invalid side/role: {side!r}/{role!r}")
    if position_side != "BOTH":
        raise RuntimeError(f"unsupported positionSide: {position_side!r}")

    try:
        fees = decimal_text(raw.get("fees"), "fees")
        return {
            "market": "usdm_futures",
            "symbol": symbol,
            "trade_id": trade_id,
            "order_id": order_id,
            "side": side,
            "liquidity_role": role,
            "price": decimal_text(raw.get("price"), "price", positive=True),
            "quantity": decimal_text(raw.get("qty"), "qty", positive=True),
            "quote_quantity": decimal_text(
                raw.get("amountu"), "amountu", nonnegative=True
            ),
            # The notebook converted commission into USDT before writing `fees`.
            "fee_amount": fees,
            "fee_asset": "USDT",
            "fee_usdt": fees,
            "realized_pnl": decimal_text(
                raw.get("realizedPnl"), "realizedPnl", optional=True
            ),
            "event_time_ms": event_time_ms,
        }
    except RuntimeError as error:
        raise RuntimeError(f"{path}:{line_number}: {error}") from error


def load_rows(data_dir, start_ms, end_ms):
    paths = sorted(glob.glob(str(data_dir / "trades_*.csv")))
    if not paths:
        raise RuntimeError(f"no trades_*.csv files found in {data_dir}")
    rows = {}
    scanned = 0
    before_start = 0
    at_or_after_end = 0
    duplicates = 0
    for path in paths:
        with open(path, newline="") as source:
            reader = csv.DictReader(source)
            if tuple(reader.fieldnames or ()) != EXPECTED_COLUMNS:
                raise RuntimeError(f"unexpected CSV header in {path}: {reader.fieldnames!r}")
            for line_number, raw in enumerate(reader, 2):
                scanned += 1
                row = normalize(raw, path, line_number)
                ts = row["event_time_ms"]
                if ts < start_ms:
                    before_start += 1
                    continue
                if end_ms is not None and ts >= end_ms:
                    at_or_after_end += 1
                    continue
                key = (row["market"], row["symbol"], row["trade_id"])
                previous = rows.get(key)
                if previous is not None:
                    duplicates += 1
                    if previous != row:
                        raise RuntimeError(f"conflicting duplicate trade: {key!r}")
                rows[key] = row
    ordered = sorted(
        rows.values(),
        key=lambda row: (row["event_time_ms"], row["symbol"], row["trade_id"]),
    )
    print(
        f"files={len(paths)}, scanned={scanned}, unique_import={len(ordered)}, "
        f"duplicates={duplicates}, before_start={before_start}, "
        f"at_or_after_end={at_or_after_end}",
        flush=True,
    )
    return ordered


def copy_to_postgres(database, schema, rows):
    process = subprocess.Popen(
        ["psql", "-X", "-v", "ON_ERROR_STOP=1", "-d", database],
        stdin=subprocess.PIPE,
        text=True,
    )
    if process.stdin is None:
        raise RuntimeError("failed to open psql stdin")
    stream = process.stdin
    columns = ",".join(DATABASE_COLUMNS)
    stream.write("BEGIN;\n")
    stream.write(f"CREATE TEMP TABLE trades_import (LIKE {schema}.trades);\n")
    stream.write(f"COPY trades_import ({columns}) FROM STDIN WITH (FORMAT csv);\n")
    writer = csv.writer(stream, lineterminator="\n")
    for row in rows:
        writer.writerow([row[name] for name in DATABASE_COLUMNS])
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
        raise RuntimeError("psql Binance MM CSV import failed")


def main():
    args = parse_args()
    schema, start_ms, configured_data_dir = strategy_storage(
        args.database, args.strategy
    )
    data_dir = args.data_dir or configured_data_dir
    if data_dir is None:
        raise RuntimeError("no CSV data directory configured")
    if args.end_ms is not None and args.end_ms <= start_ms:
        raise RuntimeError("end-ms must be greater than strategy st_ms")
    validate_storage(args.database, schema)
    rows = load_rows(data_dir, start_ms, args.end_ms)
    copy_to_postgres(args.database, schema, rows)
    if rows:
        print(
            f"Binance MM CSV import complete: strategy={args.strategy}, "
            f"rows={len(rows)}, start_ms={rows[0]['event_time_ms']}, "
            f"end_ms={rows[-1]['event_time_ms']}",
            flush=True,
        )
    else:
        print(f"Binance MM CSV import complete: strategy={args.strategy}, rows=0")


if __name__ == "__main__":
    main()
