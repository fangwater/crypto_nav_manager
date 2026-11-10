# OKX MM 83777 NAV and CTA overview

`okex_mm_83777` replaces the retired `okex_mm_alpha` in the scheduled history
collector and authenticated account-overview export. Its enabled catalog record
keeps its own schema, account credentials, configured start time and NAV FIFO
model. The collector reads derivatives fills and funding only; it does not query
MM borrowing interest or perform RocksDB reconciliation for this environment.

The existing 15-minute UTC schedule now includes this account, with the usual
per-exchange stagger. History status expects both trades and funding. CTA Manager
discovers the account through the existing NAV HTTP provider, preserves the
`account_kind=mm` attribution and existing `lgq` visibility boundary, and links
to `/nav/#/strategies/okex_mm_83777`. No Exec source or strategy binding is added.

Backfill uses `sync_history --strategy okex_mm_83777 --dataset all --full` from
the existing catalog start. Exchange-native identifiers make repeated imports
idempotent. Deposits and withdrawals do not become trading PnL. Trading processes
and their account settings are independent of this NAV-only rollout.

Deployed and verified on 2026-11-10 at 09:30–09:35 UTC, code commit `6c0b270`.
The local release binary was uploaded to an explicit staging path, SHA-256
checked and atomically installed. Only `crypto-nav-manager.service` restarted;
PID 483448 retains CPU24. CTA Manager PID3909484 and 193 other sampled live
processes retained their executable identities and CPU masks. No migrations,
strategy registrations, grants, trading binaries or trading settings changed.

The bootstrap imported 184 fills, starting at 2026-11-10 08:03:22.706 UTC,
and zero funding rows, with successful independent trades/funding watermarks
through 09:25:24.981 UTC. BTC/ETH fee-rate snapshots were also collected. The
service scheduled the next automatic account scan for 09:45 UTC. NAV retains
the factual negative maker fee cost (rebates), counted once in its existing
amount FIFO. The checked detail reported 184 trades, 9,106.27224 USDT turnover,
-0.455313612 USDT fee cost and 1.52973675105 USDT total PnL.

Authenticated HTTP checks verified NAV detail and CTA overview visibility for
`lgq`, `collection_status=live`, the source-scoped CTA NAV response, and both
public SPA pages plus their emitted JavaScript assets. The unrelated `prc`
user cannot read the NAV detail (404) or CTA source NAV (403). All four
short-lived diagnostic sessions were deleted after verification.

NAV's latest monitoring export had 28 complete points. CTA preserves its
existing closed four-hour grid: the checked 27-point curve ends at 08:00 UTC,
before the first live fill at 08:03, so that curve correctly still ends at zero.
The current overview/detail already includes the day's PnL; the 12:00 UTC
four-hour point will include the later trading history. This does not alter
sampling or manufacture interim historical observations.

Private rollback and evidence remain in
`/home/ubuntu/crypto_nav_manager/maintenance-backups/okex_mm_onboard_20261110/`.
Validation: 215 library tests, 19 history-normalization tests, formatting,
release build, integration freshness, permissions and gateway checks passed.
