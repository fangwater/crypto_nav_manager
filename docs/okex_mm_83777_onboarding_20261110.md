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
