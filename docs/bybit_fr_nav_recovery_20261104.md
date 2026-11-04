# Bybit FR arb01 history recovery, 2026-11-04

The enabled SG account `bybit_fr_arb01` was absent from NAV's recurring
history selection. Live account facts continued refreshing, but all three
successful history watermarks stopped at 2026-10-30 03:00 UTC. A curve marked
complete did not establish exchange-history freshness.

Commit `060251e` restores the account to the existing 15-minute history worker.
The repair was built locally and deployed to jp-meta's existing
`/home/ubuntu/crypto_nav_manager`; only `crypto-nav-manager.service` restarted
(PID 2632968). All seven migration-bearing binaries were replaced together,
without adding migrations. Formatting, focused Cargo checks, 18 worker tests,
and the release build passed. Nginx `/nav/`, `/nav-api/health`, and emitted
assets were checked. The next regular collection slot is November 4 08:00 UTC
(16:00 Shanghai), then every 15 minutes.

The existing history CLI repaired the gap with a 60-minute overlap through
2026-11-04 07:44:50.146 UTC. Trade rows increased from 23,241 to 24,264;
funding rows increased from 476 to 860; interest remained at 16. Every dataset
watermark reached that end. Latest trade: November 3 03:17:53.091 UTC;
latest funding: November 4 04:00 UTC. No new interest was reported.
Initial snapshots, source identity, trading settings, and trading processes
were preserved. Verification retained all 17 protected SG processes and 21
configuration hashes, and all 132 protected jp-meta processes and 114 hashes.
Recoverable account-schema, cursor, and executable backups remain host-only at
`maintenance-backups/bybit-fr-nav-20261104T074416Z/`.

## PnL attribution

All figures below use the existing production FIFO API at the fixed end above,
in USDT. The monitoring curve measures realized trading PnL after fees plus
actual funding minus interest; it excludes open-position floating PnL.
Positive fee contribution below means net commission rebates.

| Window | Trading realized before fees | Fee contribution | Actual funding | Interest cost | Realized net |
| --- | ---: | ---: | ---: | ---: | ---: |
| Since October 30 03:00 UTC gap | 21.596653 | 1.779059 | -91.479652 | 0 | -68.103940 |
| Last seven days | -171.857966 | 2.720710 | -72.917494 | 0 | -242.054750 |
| Last three days | 42.649040 | 0.368451 | 30.591674 | 0 | 73.609165 |
| Last 24 hours | 0 | 0 | 13.632136 | 0 | 13.632136 |

Since the gap, WETUSDT alone paid 146.724540 USDT in funding; other symbols
received 55.244887 USDT net. Its October 31 Shanghai 12:00, 16:00, and 20:00
debits were 42.43152639, 34.11404658, and 26.59159394 USDT. The live Viz
still showed approximately 73,040.365795 WET spot against a 73,038 WET
perpetual short, about 3,983 USDT on each leg. Delta hedging retains funding
exposure: a negative funding rate charges the short. No WET fills occurred
after October 29 13:01:11.278 UTC in the repaired account history.

Daily realized net on Shanghai dates was -76.076067 on October 31,
-36.948143 on November 1, +11.470568 on November 2, +53.432438 on November 3,
and +9.145066 on November 4 through the fixed cutoff. Daily queries use
half-open calendar days, implemented with an inclusive end one millisecond
before the next midnight; their components sum to the all-history report.
Every fee and interest was convertible. Temporary audit sessions were removed.

Seven-day floating PnL changed by +297.361427 USDT, making NAV's total PnL
estimate +55.306677 USDT. Those floating marks use the existing latest-fill
valuation, rather than current exchange equity. Since the immutable snapshot
at 2026-10-23 10:45:19.622 UTC, realized net is -520.937396 USDT; the largest
symbol detractors are BASED (-148.209160), WET (-140.149934), RECALL
(-68.339717), and MERL (-57.070250). Commission rebates partly offset losses.

The NAV detail and live overview read the repaired history after the NAV
restart; today's realized profit is +9.145066 USDT. CTA Manager's saved
four-hour monitor rebuilds at its next normal 16:00 Shanghai boundary.
No CTA Manager restart or direct Manager RocksDB write was performed.
