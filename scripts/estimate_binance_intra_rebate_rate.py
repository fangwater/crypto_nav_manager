#!/usr/bin/env python3
"""Infer the hourly Binance Spot MM rebate rate from PostgreSQL history."""

import argparse
import json
import re
import subprocess
from datetime import datetime, timezone
from decimal import Decimal


HOUR_MS = 3_600_000
DEFAULT_STRATEGY = "binance-intra-arb01"
DEFAULT_DATABASE = "crypto_nav_manager"


def parse_args():
    parser = argparse.ArgumentParser(
        description="Compare one complete Spot Maker trade hour with its next-hour rebate"
    )
    parser.add_argument("--strategy", default=DEFAULT_STRATEGY)
    parser.add_argument("--database", default=DEFAULT_DATABASE)
    parser.add_argument(
        "--hour",
        help="UTC trade hour, for example 2026-07-22T05:00:00Z; defaults to latest complete match",
    )
    return parser.parse_args()


def psql_json(database, sql):
    output = subprocess.check_output(
        ["psql", "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-d", database, "-c", sql],
        text=True,
    ).strip()
    if not output:
        return None
    return json.loads(output)


def quote_literal(value):
    return "'" + value.replace("'", "''") + "'"


def load_schema(database, strategy):
    row = psql_json(
        database,
        "SELECT json_build_object('schema',db_schema,'exchange',exchange,"
        "'strategy_kind',strategy_kind)::text FROM strategy_envs WHERE slug="
        + quote_literal(strategy),
    )
    if row is None:
        raise RuntimeError(f"strategy not found: {strategy}")
    if row["exchange"] != "binance" or row["strategy_kind"] != "intra_exchange":
        raise RuntimeError(f"strategy is not Binance intra: {strategy}")
    schema = row["schema"]
    if not re.fullmatch(r"[a-z][a-z0-9_]*", schema):
        raise RuntimeError(f"unsafe PostgreSQL schema: {schema!r}")
    return schema


def parse_hour(value):
    if value is None:
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    parsed = parsed.astimezone(timezone.utc)
    if parsed.minute or parsed.second or parsed.microsecond:
        raise RuntimeError("--hour must be aligned to an exact UTC hour")
    return int(parsed.timestamp() * 1000)


def load_hour(database, schema, requested_hour_ms):
    hour_filter = ""
    if requested_hour_ms is not None:
        hour_filter = f"AND rebate_hours.hour_start_ms = {requested_hour_ms}"
    sql = f"""
WITH max_trade AS (
    SELECT (MAX(event_time_ms) / {HOUR_MS}) * {HOUR_MS} AS complete_cutoff_ms
    FROM {schema}.trades
    WHERE market = 'spot' AND liquidity_role = 'maker'
),
rebate_hours AS (
    SELECT
        (event_time_ms / {HOUR_MS}) * {HOUR_MS} - {HOUR_MS} AS hour_start_ms,
        SUM(amount_usdt) AS rebate_usdt,
        MIN(event_time_ms) AS rebate_posted_ms,
        MIN(description) AS description,
        COUNT(*) AS rebate_record_count
    FROM {schema}.rebates
    WHERE amount_usdt IS NOT NULL
      AND description LIKE 'Spot MM Rebate-Spot%'
    GROUP BY 1
),
matched AS (
    SELECT
        rebate_hours.hour_start_ms,
        rebate_hours.rebate_usdt,
        rebate_hours.rebate_posted_ms,
        rebate_hours.description,
        rebate_hours.rebate_record_count,
        COUNT(*) AS trade_count,
        COUNT(DISTINCT trades.symbol) AS symbol_count,
        SUM(COALESCE(trades.quote_quantity, trades.price * trades.quantity)) AS maker_notional,
        SUM(CASE WHEN trades.side = 'buy'
                 THEN COALESCE(trades.quote_quantity, trades.price * trades.quantity)
                 ELSE 0 END) AS buy_notional,
        SUM(CASE WHEN trades.side = 'sell'
                 THEN COALESCE(trades.quote_quantity, trades.price * trades.quantity)
                 ELSE 0 END) AS sell_notional
    FROM rebate_hours
    CROSS JOIN max_trade
    JOIN {schema}.trades AS trades
      ON trades.market = 'spot'
     AND trades.liquidity_role = 'maker'
     AND trades.event_time_ms >= rebate_hours.hour_start_ms
     AND trades.event_time_ms < rebate_hours.hour_start_ms + {HOUR_MS}
    WHERE rebate_hours.hour_start_ms + {HOUR_MS} <= max_trade.complete_cutoff_ms
      {hour_filter}
    GROUP BY
        rebate_hours.hour_start_ms,
        rebate_hours.rebate_usdt,
        rebate_hours.rebate_posted_ms,
        rebate_hours.description,
        rebate_hours.rebate_record_count
    ORDER BY rebate_hours.hour_start_ms DESC
    LIMIT 1
)
SELECT json_build_object(
    'hour_start_ms', hour_start_ms,
    'rebate_posted_ms', rebate_posted_ms,
    'description', description,
    'rebate_record_count', rebate_record_count,
    'trade_count', trade_count,
    'symbol_count', symbol_count,
    'maker_notional', maker_notional::text,
    'buy_notional', buy_notional::text,
    'sell_notional', sell_notional::text,
    'rebate_usdt', rebate_usdt::text
)::text
FROM matched
"""
    return psql_json(database, sql)


def utc_text(timestamp_ms):
    return datetime.fromtimestamp(timestamp_ms / 1000, timezone.utc).strftime(
        "%Y-%m-%d %H:%M:%S UTC"
    )


def main():
    args = parse_args()
    schema = load_schema(args.database, args.strategy)
    requested_hour_ms = parse_hour(args.hour)
    row = load_hour(args.database, schema, requested_hour_ms)
    if row is None:
        target = args.hour or "the latest complete hour"
        raise RuntimeError(f"no matched Spot Maker trades and rebate for {target}")

    hour_start_ms = int(row["hour_start_ms"])
    maker_notional = Decimal(row["maker_notional"])
    rebate_usdt = Decimal(row["rebate_usdt"])
    if maker_notional <= 0:
        raise RuntimeError("Spot Maker notional must be positive")
    expected_description = datetime.fromtimestamp(
        hour_start_ms / 1000, timezone.utc
    ).strftime("Spot MM Rebate-Spot %y-%m-%d %H:00")
    if row["description"] != expected_description:
        raise RuntimeError(
            f"rebate hour mismatch: expected {expected_description!r}, got {row['description']!r}"
        )

    rate = rebate_usdt / maker_notional
    print(f"strategy={args.strategy}")
    print(
        f"trade_window={utc_text(hour_start_ms)} .. "
        f"{utc_text(hour_start_ms + HOUR_MS)}"
    )
    print(f"trade_count={row['trade_count']}")
    print(f"symbol_count={row['symbol_count']}")
    print(f"spot_maker_notional_usdt={maker_notional}")
    print(f"buy_notional_usdt={Decimal(row['buy_notional'])}")
    print(f"sell_notional_usdt={Decimal(row['sell_notional'])}")
    print(f"rebate_posted={utc_text(int(row['rebate_posted_ms']))}")
    print(f"rebate_usdt={rebate_usdt}")
    print(f"implied_rate={rate}")
    print(f"implied_rate_percent={rate * Decimal(100)}")
    print(f"implied_rate_bps={rate * Decimal(10000)}")


if __name__ == "__main__":
    main()
