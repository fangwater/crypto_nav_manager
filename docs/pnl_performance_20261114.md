# NAV FIFO performance repair — 2026-11-14

The RBF publication investigation found independent NAV reconstruction competing
with CTA Manager on physical CPU24. Captured NAV instruction addresses resolved
to `QuantityFifoPnl::snapshot`, which scanned every open lot after every fill.
Growing one-sided inventories could therefore produce quadratic marking work.

## Calculation changes

Both existing FIFO models retain their original lot matching, realized PnL,
fees and bid/ask conventions. Quantity FIFO now maintains open quantity and
entry-price offsets; amount FIFO maintains the corresponding amount/price
weights and offsets. Compensated sums and a fixed price origin reduce numerical
cancellation when large notionals have small PnL. Empty inventories reset their
aggregates. Marking is constant time; closing still consumes the factual FIFO
lots in the same order. MM remains amount based and FR remains quantity based.

The overview keeps disposable per-account checkpoints before a one-hour overlap.
Every refresh still reads and normalizes the complete input range and checks a
SHA-256 fingerprint of the complete historical prefix. A matching prefix reuses
FIFO and cumulative results, replaying the overlap and newly arrived events.
Any older insertion, deletion, price/fee/funding/interest correction, initial
position change, source/context change or missing historical boundary triggers
full replay. A backwards range also invalidates reuse. Failures retain the last
good externally served reconstruction; missing charges remain unavailable.

The cache retains no raw event copy. It is limited to32 accounts and64 MiB of
conservative heap accounting, including allocated FIFO capacity and retained
curve points; over-budget entries use full replay and other entries evict by
LRU. Historical four-hour monitoring boundaries remain, while fine-grained
drawdown points keep the moving46-day window. Live observations and historical
replay remain separate. No database schema or durable source record changes.

This is incremental FIFO replay, not incremental SQL loading: complete input
validation deliberately remains so old record corrections cannot silently
invalidate PnL. SQL loading, normalization and sorting can still cost CPU.

## Verification

The test-only original per-lot formulas provide an independent valuation oracle.
Randomized fills, partial closes, reversals, rebates, flat resets and large prices
with small PnL are compared against those formulas. Checkpoint tests compare
every point and boundary with full replay, including historical corrections,
late records, moving windows, source/initial-state changes and memory eviction.

`nav_pnl_audit` explicitly exports normalized inputs over a read-only PostgreSQL
pool. It starts no service, runs no migrations and makes no exchange request.
Its files contain private positions and must stay outside Git in mode0600.
An ignored offline release test compares original marking, optimized full replay
and a five-minute incremental replay on that exact frozen file. Realized totals
and realized boundaries must match exactly; floating curves use a tight numeric
tolerance, with measured maximum differences reported. Warm/full optimized
results must match exactly.

Production capture:12 accounts, end1794670320000 ms (2026-11-14 15:32 UTC),
309955091 bytes, SHA-256
`86aea34e5046ec055366c773e0b370b954f2f56114709c01e0d9bd54b1cc8e1a`.
The first capture used saved/derived conversions. A second read-only capture
for the three Binance accounts subscribed to existing public IPC marks, with no
REST bootstrap, at the same cutoff. The complete merged frozen file has SHA-256
`a1c11ffb472b7575e6d6114d4eab9b3f280e574daf0e0417f7f7d40c7431ed06`.
All12 accounts passed original/full/incremental comparison. Realized totals and
all realized boundaries were identical. The largest floating-curve difference
was0.000000003827 USDT. Warm optimized histories matched full optimized histories
exactly, including every retained point and requested boundary.

The aggregate local fixture timings were3481 ms for the original marking
formula,822 ms for optimized full replay and380 ms for validated incremental
replay. Gate FR02 measured1003/151/75 ms and Binance FR04 measured984/150/89 ms.
These measurements exclude SQL and include input cloning in the full replay
paths; they do not claim equivalent HTTP or production latency reductions.

Formatting, all-target Cargo check and all276 default tests passed (225 library
and51 binary tests, with two explicit ignored audits/benchmarks). One unchanged
history-timeout test failed once in an earlier parallel run, then passed in
isolation and in the later complete suite; its code was not modified.

A synthetic release measurement with20,000 growing open lots and a mark after
each fill measured633 µs with aggregation versus184389 µs for the original
per-lot formula. This roughly291-fold result isolates the marking workload;
it is not a claimed end-to-end service speedup.

## Deployment boundary

Only independent NAV binaries and its own CPU affinity drop-in are eligible
for replacement. The jp-meta drop-in assigns NAV to audited physical CPU35,
leaving CTA on24 and Redis on28. Recheck all users' live thread affinities
before applying it. Do not change any trading process, binding or setting.
Existing NAV child tools inherit the new affinity after the service restart;
already-running finite children may keep their previous mask until they exit.

The frontend, credentials and host-specific environment files remain unchanged.
Build locally from the configured NVMe target directory. Preserve rollback
binaries and the old CPU drop-in, upload to explicit staging paths, verify
SHA-256, then replace atomically and restart only `crypto-nav-manager.service`.
Keep all migration-embedding utility binaries consistent with the service.
