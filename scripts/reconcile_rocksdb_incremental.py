#!/usr/bin/env python3
"""Incrementally reconcile strategy-center fills with PostgreSQL trades."""

import argparse
import csv
import json
import math
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

import pyarrow.parquet as pq


REPO_ROOT = Path(__file__).resolve().parents[1]
SUPPORTED = (
    "binance-intra-arb01",
    "bybit-intra-arb01",
    "bybit-intra-arb02",
)
REMOTE = {"bybit-intra-arb01", "bybit-intra-arb02"}
VENUE_MARKET = {
    "BinanceMargin": "spot",
    "BinanceFutures": "swap",
    "BybitMargin": "spot",
    "BybitFutures": "swap",
}
SID_MARKET = {"1": "spot", "0": "swap"}


def parse_args():
    parser = argparse.ArgumentParser(
        description="Incrementally compare PG exchange trades with center RocksDB fills"
    )
    parser.add_argument("--strategy", action="append", choices=SUPPORTED)
    parser.add_argument("--database", default="crypto_nav_manager")
    parser.add_argument("--database-url")
    parser.add_argument("--settlement-gap-minutes", type=int, default=10)
    parser.add_argument("--overlap-minutes", type=int, default=5)
    parser.add_argument("--baseline-minutes", type=int, default=60)
    parser.add_argument("--end-ms", type=int)
    parser.add_argument("--qty-epsilon", type=float, default=1e-8)
    parser.add_argument("--skip-sync", action="store_true")
    parser.add_argument("--ssh-host", default="sg")
    parser.add_argument("--base-dir", default="/home/ubuntu")
    parser.add_argument("--mkt-signal-root", default="/home/ubuntu/mkt_signal")
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--keep-remote", action="store_true")
    parser.add_argument(
        "--cleanup-on-success",
        action="store_true",
        help="Remove the report directory after every requested strategy aligns",
    )
    return parser.parse_args()


def run(command, capture=False, check=True):
    print("+", shlex.join(map(str, command)), flush=True)
    result = subprocess.run(
        list(map(str, command)),
        check=check,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    )
    return result.stdout.strip() if capture else ""


def psql(args, sql):
    command = ["psql", "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1"]
    command += [args.database_url] if args.database_url else ["-d", args.database]
    return run(command + ["-c", sql], capture=True)


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def start_status(args, strategy, run_id, candidate_end_ms):
    psql(
        args,
        "INSERT INTO rocksdb_alignment_status "
        "(strategy_slug,state,phase,progress_percent,run_id,started_at,updated_at,"
        "completed_at,candidate_end_ms,scan_start_ms,pg_success_end_ms,actual_end_ms,"
        "group_count,mismatch_count,pg_event_count,local_event_count,message) VALUES ("
        f"{literal(strategy)},'running','preparing',5,{literal(run_id)},"
        f"CURRENT_TIMESTAMP,CURRENT_TIMESTAMP,NULL,{candidate_end_ms},"
        "NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL) "
        "ON CONFLICT (strategy_slug) DO UPDATE SET "
        "state='running',phase='preparing',progress_percent=5,"
        f"run_id={literal(run_id)},started_at=CURRENT_TIMESTAMP,"
        "updated_at=CURRENT_TIMESTAMP,completed_at=NULL,"
        f"candidate_end_ms={candidate_end_ms},scan_start_ms=NULL,"
        "pg_success_end_ms=NULL,actual_end_ms=NULL,group_count=NULL,"
        "mismatch_count=NULL,pg_event_count=NULL,local_event_count=NULL,message=NULL",
    )


def update_status(args, strategy, state, phase, progress, **values):
    assignments = [
        f"state={literal(state)}",
        f"phase={literal(phase)}",
        f"progress_percent={progress}",
        "updated_at=CURRENT_TIMESTAMP",
    ]
    for column, value in values.items():
        assignments.append(
            f"{column}={literal(value) if isinstance(value, str) else value}"
        )
    if state in ("succeeded", "mismatch", "failed"):
        assignments.append("completed_at=CURRENT_TIMESTAMP")
    psql(
        args,
        "UPDATE rocksdb_alignment_status SET "
        + ",".join(assignments)
        + f" WHERE strategy_slug={literal(strategy)}",
    )


def checkpoint(args, strategy):
    row = psql(
        args,
        "SELECT c.aligned_from_ms::text || '|' || "
        "c.verified_through_ms::text || '|' || w.success_end_ms::text "
        "FROM rocksdb_alignment_checkpoints c "
        "JOIN history_sync_watermarks w ON w.strategy_slug=c.strategy_slug "
        "AND w.dataset='trades' "
        f"WHERE c.strategy_slug={literal(strategy)}",
    )
    fields = row.split("|")
    if len(fields) != 3:
        raise RuntimeError(f"missing checkpoint or PG trades watermark: {strategy}")
    return tuple(map(int, fields))


def rfc3339_us(timestamp_us):
    seconds, micros = divmod(timestamp_us, 1_000_000)
    instant = datetime.fromtimestamp(seconds, timezone.utc)
    return instant.strftime("%Y-%m-%dT%H:%M:%S") + f".{micros:06d}Z"


def sync_trades(args, strategy, start_ms, end_ms):
    command = [
        REPO_ROOT / "target/release/sync_history",
        "--strategy", strategy,
        "--dataset", "trades",
        "--start-ms", start_ms,
        "--end-ms", end_ms,
    ]
    if args.database_url:
        command += ["--database-url", args.database_url]
    run(command)


def export_pg(args, strategy, start_ms, end_ms, root):
    command = [
        REPO_ROOT / "target/release/export_history",
        "--strategy", strategy,
        "--dataset", "trades",
        "--start-ms", start_ms,
        "--end-ms", end_ms,
        "--output-dir", root,
    ]
    if args.database_url:
        command += ["--database-url", args.database_url]
    run(command)
    return root / strategy


def locate_export(root):
    files = sorted(root.glob("*/uniform_orders.parquet"))
    if len(files) != 1:
        raise RuntimeError(f"expected one order export below {root}, found {len(files)}")
    directory = files[0].parent
    if not (directory / "trade_updates_unmatched.parquet").is_file():
        raise RuntimeError(f"missing unmatched trade export below {directory}")
    return directory


def export_orders(args, strategy, start_ms, end_ms, root):
    exporter = Path(args.mkt_signal_root) / "target/release/order_export"
    common = [
        "--base-dir", args.base_dir,
        "--env-name", strategy,
        "--start", rfc3339_us(start_ms * 1_000),
        "--end", rfc3339_us(end_ms * 1_000 + 999),
    ]
    if strategy not in REMOTE:
        run([exporter] + common + ["--output-root", root])
        return locate_export(root)

    token = uuid.uuid4().hex
    remote_root = f"/tmp/crypto_nav_rocksdb_reconcile/{token}"
    remote_binary = f"{remote_root}/order_export"
    remote_output = f"{remote_root}/output"
    run(["ssh", args.ssh_host, "mkdir", "-p", remote_output])
    run(["scp", exporter, f"{args.ssh_host}:{remote_binary}"])
    try:
        run(["ssh", args.ssh_host, "chmod", "700", remote_binary])
        command = [remote_binary] + common + ["--output-root", remote_output]
        run(["ssh", args.ssh_host, shlex.join(map(str, command))])
        root.mkdir(parents=True, exist_ok=True)
        run(["scp", "-r", f"{args.ssh_host}:{remote_output}/.", root])
    finally:
        if not args.keep_remote:
            run(
                ["ssh", args.ssh_host, "rm", "-rf", "--", remote_root],
                check=False,
            )
    return locate_export(root)


def group_key(market, symbol, side):
    symbol = str(symbol).strip().upper().replace("-", "")
    side = str(side).strip().lower()
    if not symbol or side not in ("buy", "sell"):
        raise RuntimeError(f"invalid symbol/side: {symbol!r}/{side!r}")
    return market, symbol, side


def empty_stat():
    return {"qty": 0.0, "events": 0, "orders": set()}


def add_stat(groups, key, qty, order_id):
    value = groups[key]
    value["qty"] += qty
    value["events"] += 1
    value["orders"].add(str(order_id))


def pg_groups(directory, start_ms, end_ms):
    groups = defaultdict(empty_stat)
    seen = {}
    for path in sorted(directory.glob("trades_*.csv")):
        with path.open(newline="") as stream:
            for row in csv.DictReader(stream):
                timestamp = int(row["ts"])
                if not start_ms <= timestamp <= end_ms:
                    continue
                market = SID_MARKET.get(row["sid"])
                if market is None:
                    raise RuntimeError(f"unsupported PG sid: {row['sid']!r}")
                key = group_key(market, row["symbol"], row["side"])
                qty = float(row["qty"])
                if qty <= 0:
                    continue
                trade_key = (market, key[1], row["id"])
                fingerprint = (key, row["orderId"], qty)
                if trade_key in seen:
                    if seen[trade_key] != fingerprint:
                        raise RuntimeError(f"conflicting PG duplicate: {trade_key}")
                    continue
                seen[trade_key] = fingerprint
                add_stat(groups, key, qty, row["orderId"])
    return dict(groups)


def parquet_rows(path, columns):
    table = pq.read_table(path, columns=columns)
    values = [table.column(name).to_pylist() for name in columns]
    return zip(*values)


def uniform_groups(path, start_us, end_us, epsilon):
    groups = defaultdict(empty_stat)
    client_ids = defaultdict(set)
    columns = [
        "update_ts", "symbol", "trading_venue", "side",
        "amount_update", "client_order_id",
    ]
    for timestamp, symbol, venue, side, qty, client_id in parquet_rows(path, columns):
        if timestamp is None or not start_us <= timestamp <= end_us:
            continue
        qty = float(qty)
        if qty < -epsilon:
            raise RuntimeError(f"negative uniform amount_update: {qty}")
        if qty <= epsilon:
            continue
        market = VENUE_MARKET.get(str(venue))
        if market is None:
            raise RuntimeError(f"unsupported venue: {venue!r}")
        key = group_key(market, symbol, side)
        add_stat(groups, key, qty, client_id)
        client_ids[key].add(int(client_id))
    return dict(groups), dict(client_ids)


def unmatched_groups(path, start_us, end_us, epsilon, uniform_ids):
    series = {}
    columns = [
        "event_time", "trade_time", "symbol", "order_id", "client_order_id",
        "side", "trading_venue", "cumulative_filled_quantity",
    ]
    for event_ts, trade_ts, symbol, order_id, client_id, side, venue, qty in parquet_rows(path, columns):
        timestamp = int(trade_ts) if trade_ts and int(trade_ts) > 0 else int(event_ts)
        if timestamp > end_us:
            continue
        market = VENUE_MARKET.get(str(venue))
        if market is None:
            raise RuntimeError(f"unsupported venue: {venue!r}")
        group = group_key(market, symbol, side)
        key = market, group[1], str(order_id)
        entry = series.setdefault(
            key, {"group": group, "client_ids": set(), "observations": []}
        )
        if entry["group"] != group:
            raise RuntimeError(f"inconsistent unmatched order side: {key}")
        entry["client_ids"].add(int(client_id))
        entry["observations"].append((timestamp, float(qty)))

    groups = defaultdict(empty_stat)
    represented = unmatched_only = 0
    for order_id, entry in series.items():
        observations = sorted(entry["observations"])
        baseline = max((qty for ts, qty in observations if ts < start_us), default=0.0)
        current = [(ts, qty) for ts, qty in observations if start_us <= ts <= end_us]
        if not current:
            continue
        qty = max([baseline] + [value for _, value in current]) - baseline
        if qty < -epsilon:
            raise RuntimeError(f"negative unmatched cumulative delta: {order_id}")
        if qty <= epsilon:
            continue
        group = entry["group"]
        if entry["client_ids"] & uniform_ids.get(group, set()):
            represented += 1
            continue
        add_stat(groups, group, qty, order_id[2])
        groups[group]["events"] += len(current) - 1
        unmatched_only += 1
    return dict(groups), represented, unmatched_only


def compare(pg, uniform, unmatched, epsilon):
    rows = []
    for key in sorted(set(pg) | set(uniform) | set(unmatched)):
        p = pg.get(key, empty_stat())
        u = uniform.get(key, empty_stat())
        x = unmatched.get(key, empty_stat())
        local_qty = u["qty"] + x["qty"]
        difference = local_qty - p["qty"]
        rows.append({
            "market": key[0], "symbol": key[1], "side": key[2],
            "pg_events": p["events"], "pg_orders": len(p["orders"]),
            "pg_qty": p["qty"],
            "uniform_events": u["events"], "uniform_orders": len(u["orders"]),
            "uniform_qty": u["qty"],
            "unmatched_events": x["events"], "unmatched_orders": len(x["orders"]),
            "unmatched_qty": x["qty"], "local_qty": local_qty,
            "qty_diff": difference,
            "status": "MATCH" if abs(difference) <= epsilon else "MISMATCH",
        })
    return rows


def write_csv(path, rows):
    with path.open("w", newline="") as stream:
        if rows:
            writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)


def reconcile(args, strategy, candidate_end_ms, report_root, run_id):
    start_status(args, strategy, run_id, candidate_end_ms)
    aligned_from, previous_end, _ = checkpoint(args, strategy)
    scan_start = max(
        aligned_from,
        ((previous_end - args.overlap_minutes * 60_000) // 60_000) * 60_000,
    )
    update_status(
        args, strategy, "running", "loading_watermark", 10,
        scan_start_ms=scan_start,
    )
    sync_error = None
    if not args.skip_sync:
        update_status(args, strategy, "running", "syncing_trades", 15)
        try:
            sync_trades(args, strategy, scan_start, candidate_end_ms)
        except subprocess.CalledProcessError as error:
            sync_error = f"sync_history exited with {error.returncode}"

    _, _, pg_watermark = checkpoint(args, strategy)
    actual_end = min(candidate_end_ms, pg_watermark)
    if actual_end < scan_start:
        raise RuntimeError(
            f"PG watermark {pg_watermark} is before scan start {scan_start}"
        )

    root = report_root / strategy
    root.mkdir(parents=True, exist_ok=True)
    update_status(
        args, strategy, "running", "exporting_pg", 30,
        pg_success_end_ms=pg_watermark,
        actual_end_ms=actual_end,
    )
    pg_dir = export_pg(args, strategy, scan_start, actual_end, root / "pg")
    export_start = max(
        aligned_from, scan_start - args.baseline_minutes * 60_000
    )
    update_status(args, strategy, "running", "exporting_orders", 50)
    order_dir = export_orders(
        args, strategy, export_start, actual_end, root / "orders"
    )

    update_status(args, strategy, "running", "comparing", 85)
    start_us, end_us = scan_start * 1_000, actual_end * 1_000 + 999
    pg = pg_groups(pg_dir, scan_start, actual_end)
    uniform, client_ids = uniform_groups(
        order_dir / "uniform_orders.parquet", start_us, end_us, args.qty_epsilon
    )
    unmatched, represented, unmatched_only = unmatched_groups(
        order_dir / "trade_updates_unmatched.parquet",
        start_us, end_us, args.qty_epsilon, client_ids,
    )
    groups = compare(pg, uniform, unmatched, args.qty_epsilon)
    mismatches = [row for row in groups if row["status"] == "MISMATCH"]
    aligned = not mismatches
    advanced = aligned and actual_end > previous_end
    if advanced:
        psql(
            args,
            "UPDATE rocksdb_alignment_checkpoints SET "
            f"verified_through_ms=GREATEST(verified_through_ms,{actual_end}), "
            "verified_at=CURRENT_TIMESTAMP "
            f"WHERE strategy_slug={literal(strategy)}",
        )
    write_csv(root / "groups.csv", groups)
    summary = {
        "strategy": strategy,
        "aligned": aligned,
        "checkpoint_advanced": advanced,
        "sync_error": sync_error,
        "aligned_from_ms": aligned_from,
        "previous_verified_through_ms": previous_end,
        "scan_start_ms": scan_start,
        "candidate_end_ms": candidate_end_ms,
        "pg_success_end_ms": pg_watermark,
        "actual_end_ms": actual_end,
        "settlement_gap_minutes": args.settlement_gap_minutes,
        "overlap_minutes": args.overlap_minutes,
        "baseline_minutes": args.baseline_minutes,
        "group_count": len(groups),
        "mismatched_group_count": len(mismatches),
        "pg_event_count": sum(value["events"] for value in pg.values()),
        "uniform_event_count": sum(value["events"] for value in uniform.values()),
        "unmatched_represented_order_count": represented,
        "unmatched_only_order_count": unmatched_only,
        "pg_qty": sum(value["qty"] for value in pg.values()),
        "local_qty": sum(value["qty"] for value in uniform.values())
        + sum(value["qty"] for value in unmatched.values()),
    }
    update_status(
        args,
        strategy,
        "succeeded" if aligned else "mismatch",
        "complete",
        100,
        group_count=len(groups),
        mismatch_count=len(mismatches),
        pg_event_count=summary["pg_event_count"],
        local_event_count=summary["uniform_event_count"],
        message="全部分组匹配" if aligned else f"{len(mismatches)} 个分组存在差异",
    )
    (root / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n"
    )
    print(
        f"{strategy}: aligned={aligned} groups={len(groups)} "
        f"mismatches={len(mismatches)} end={actual_end} advanced={advanced}",
        flush=True,
    )
    return summary


def main():
    args = parse_args()
    for name in ("settlement_gap_minutes", "overlap_minutes", "baseline_minutes"):
        if getattr(args, name) < 0:
            raise RuntimeError(f"--{name.replace('_', '-')} must be non-negative")
    if not math.isfinite(args.qty_epsilon) or args.qty_epsilon < 0:
        raise RuntimeError("--qty-epsilon must be finite and non-negative")
    now_gap_ms = int(time.time() * 1_000) - args.settlement_gap_minutes * 60_000
    candidate_end_ms = min(args.end_ms or now_gap_ms, now_gap_ms)
    if args.work_dir:
        report_root = args.work_dir.resolve()
        report_root.mkdir(parents=True, exist_ok=True)
    else:
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        parent = Path(tempfile.gettempdir()) / "crypto_nav_rocksdb_reconcile"
        parent.mkdir(parents=True, exist_ok=True)
        report_root = Path(tempfile.mkdtemp(prefix=stamp + "-", dir=parent))
    print(f"report_root={report_root}", flush=True)
    print(f"candidate_end_ms={candidate_end_ms}", flush=True)

    summaries = []
    failed = False
    for strategy in args.strategy or SUPPORTED:
        run_id = uuid.uuid4().hex
        try:
            summary = reconcile(
                args, strategy, candidate_end_ms, report_root, run_id
            )
            failed |= not summary["aligned"] or summary["sync_error"] is not None
        except Exception as error:
            failed = True
            summary = {"strategy": strategy, "aligned": False, "error": str(error)}
            try:
                update_status(
                    args, strategy, "failed", "complete", 100,
                    message=str(error)[:1000],
                )
            except Exception as status_error:
                print(
                    f"{strategy}: status update failed: {status_error}",
                    file=sys.stderr,
                    flush=True,
                )
            root = report_root / strategy
            root.mkdir(parents=True, exist_ok=True)
            (root / "summary.json").write_text(
                json.dumps(summary, indent=2, sort_keys=True) + "\n"
            )
            print(f"{strategy}: ERROR: {error}", file=sys.stderr, flush=True)
        summaries.append(summary)
    (report_root / "summary.json").write_text(
        json.dumps(summaries, indent=2, sort_keys=True) + "\n"
    )
    print(f"summary={report_root / 'summary.json'}", flush=True)
    if args.cleanup_on_success and not failed:
        shutil.rmtree(report_root)
        print(f"removed_success_report={report_root}", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
