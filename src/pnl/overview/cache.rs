//! Disposable, bounded checkpoints. Validate normalized historical inputs on
//! every refresh, including old corrections, before reusing any FIFO state.
use super::*;
use sha2::{Digest, Sha256};

const OVERLAP_MS: i64 = 3_600_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 32;

#[derive(Debug)]
pub struct ReplayStats {
    pub total_events: usize,
    pub reused_events: usize,
    pub replayed_events: usize,
    pub retained: bool,
}

pub struct ReplayCache {
    entries: BTreeMap<String, Checkpoint>,
    clock: u64,
    max_bytes: usize,
}

impl Default for ReplayCache {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            clock: 0,
            max_bytes: MAX_BYTES,
        }
    }
}

struct Checkpoint {
    cutoff: i64,
    window_start: i64,
    fingerprint: [u8; 32],
    state: State,
    used: u64,
    bytes: usize,
}

#[derive(Clone)]
struct State {
    symbols: HashMap<String, SymbolState>,
    history: History,
    total: f64,
    realized: f64,
    started: bool,
}

impl State {
    fn new(source: PnlSourceKind, positions: &[InitialPosition]) -> Result<Self> {
        let mut symbols = HashMap::new();
        for position in positions {
            symbols
                .entry(position.symbol.clone())
                .or_insert_with(|| SymbolState::for_source(source))
                .seed_position(position)?;
        }
        Ok(Self {
            symbols,
            history: History {
                boundaries: BTreeMap::new(),
                points: Vec::new(),
                realized_boundaries: BTreeMap::new(),
                final_realized_net: 0.0,
                final_pnl: 0.0,
                start_pnl: 0.0,
            },
            total: 0.0,
            realized: 0.0,
            started: false,
        })
    }

    fn prepare(&mut self, start: i64, wanted: &BTreeSet<i64>) {
        if let Some((_, value)) = self.history.points.iter().rev().find(|(ts, _)| *ts < start) {
            self.history.start_pnl = *value;
        }
        self.history.points.retain(|(ts, _)| *ts >= start);
        self.started = !self.history.points.is_empty();
        self.history.boundaries.retain(|ts, _| wanted.contains(ts));
        self.history
            .realized_boundaries
            .retain(|ts, _| wanted.contains(ts));
    }

    fn record_boundaries(
        &mut self,
        through: i64,
        wanted: &mut std::iter::Peekable<impl Iterator<Item = i64>>,
    ) {
        while wanted.peek().is_some_and(|ts| *ts <= through) {
            let ts = wanted.next().unwrap();
            self.history.boundaries.insert(ts, self.total);
            self.history.realized_boundaries.insert(ts, self.realized);
        }
    }

    fn apply(&mut self, source: PnlSourceKind, event: &PnlEvent, start: i64) -> Result<()> {
        if !self.started && event.ts() >= start {
            self.history.start_pnl = self.total;
            self.started = true;
        }
        let symbol = event.symbol();
        self.symbols
            .entry(symbol.into())
            .or_insert_with(|| SymbolState::for_source(source));
        let before = self.symbols[symbol].metrics();
        apply_event(&mut self.symbols, event)?;
        let after = self.symbols[symbol].metrics();
        anyhow::ensure!(
            after.unconverted_fee_count == 0 && after.unconverted_interest_count == 0,
            "account history contains unconverted charges"
        );
        self.total += after.total_pnl_usdt - before.total_pnl_usdt;
        self.realized += realized_net_pnl(&after) - realized_net_pnl(&before);
        if event.ts() >= start {
            if self
                .history
                .points
                .last()
                .is_some_and(|p| p.0 == event.ts())
            {
                self.history.points.last_mut().unwrap().1 = self.total;
            } else {
                self.history.points.push((event.ts(), self.total));
            }
        }
        Ok(())
    }

    fn finish(mut self, end: i64) -> Result<History> {
        if !self.started {
            self.history.start_pnl = self.total;
        }
        anyhow::ensure!(
            self.total.is_finite() && self.realized.is_finite(),
            "non-finite account history"
        );
        self.history.points.push((end, self.total));
        self.history.final_pnl = self.total;
        self.history.final_realized_net = self.realized;
        Ok(self.history)
    }

    fn bytes(&self) -> usize {
        // Conservative heap accounting; no event history is retained. The FIFO
        // capacities count even when partially emptied. Entries over budget are
        // simply not cached, without dropping positions or report points.
        self.symbols.capacity() * (std::mem::size_of::<SymbolState>() + 64)
            + self
                .symbols
                .iter()
                .map(|(symbol, s)| {
                    symbol.capacity()
                        + s.spot_fifo.allocated_bytes()
                        + s.futures_fifo.allocated_bytes()
                })
                .sum::<usize>()
            + self.history.points.capacity() * std::mem::size_of::<(i64, f64)>()
            + (self.history.boundaries.len() + self.history.realized_boundaries.len()) * 64
    }
}

impl ReplayCache {
    pub fn retain_accounts(&mut self, accounts: &HashSet<String>) {
        self.entries.retain(|key, _| accounts.contains(key));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replay(
        &mut self,
        account: &str,
        context: &str,
        source: PnlSourceKind,
        inputs: PnlInputs,
        start: i64,
        end: i64,
        boundaries: &[i64],
    ) -> Result<(History, ReplayStats)> {
        let cutoff = end.saturating_sub(OVERLAP_MS);
        let wanted = boundaries
            .iter()
            .copied()
            .filter(|ts| *ts <= end)
            .collect::<BTreeSet<_>>();
        let mut events =
            Vec::with_capacity(inputs.trades.len() + inputs.funding.len() + inputs.interest.len());
        events.extend(inputs.trades.into_iter().map(PnlEvent::Trade));
        events.extend(inputs.funding.into_iter().map(PnlEvent::Funding));
        events.extend(inputs.interest.into_iter().map(PnlEvent::Interest));
        events.retain(|e| e.ts() <= end);
        events.sort_by(|a, b| {
            a.ts()
                .cmp(&b.ts())
                .then_with(|| a.rank().cmp(&b.rank()))
                .then_with(|| a.symbol().cmp(b.symbol()))
        });
        let seed = seed(context, source, &inputs.initial_positions);
        let mut digest = seed.clone();
        let mut reused = 0;
        let previous = self.entries.remove(account).filter(|c| {
            c.cutoff <= cutoff
                && c.window_start <= start
                && wanted
                    .iter()
                    .filter(|ts| **ts <= c.cutoff)
                    .all(|ts| c.state.history.boundaries.contains_key(ts))
        });
        let mut state = if let Some(previous) = previous {
            let count = events.partition_point(|e| e.ts() < previous.cutoff);
            for event in &events[..count] {
                hash_event(&mut digest, event);
            }
            if <[u8; 32]>::from(digest.clone().finalize()) == previous.fingerprint {
                reused = count;
                previous.state
            } else {
                digest = seed;
                State::new(source, &inputs.initial_positions)?
            }
        } else {
            State::new(source, &inputs.initial_positions)?
        };
        state.prepare(start, &wanted);
        let mut pending = wanted
            .iter()
            .copied()
            .filter(|ts| !state.history.boundaries.contains_key(ts))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        let prefix_end = events.partition_point(|e| e.ts() < cutoff);
        for event in &events[reused..prefix_end] {
            state.record_boundaries(event.ts(), &mut pending);
            state.apply(source, event, start)?;
            hash_event(&mut digest, event);
        }
        state.record_boundaries(cutoff, &mut pending);
        let bytes = state.bytes() + account.len() + std::mem::size_of::<Checkpoint>() + 128;
        let retained = bytes <= self.max_bytes;
        let mut checkpoint = Some(state);
        let mut tail = if retained {
            checkpoint.as_ref().unwrap().clone()
        } else {
            checkpoint.take().unwrap()
        };
        for event in &events[prefix_end..] {
            tail.record_boundaries(event.ts(), &mut pending);
            tail.apply(source, event, start)?;
        }
        tail.record_boundaries(end, &mut pending);
        let history = tail.finish(end)?;
        if retained {
            self.clock = self.clock.wrapping_add(1);
            while self.entries.len() >= MAX_ACCOUNTS
                || self.entries.values().map(|v| v.bytes).sum::<usize>() + bytes > self.max_bytes
            {
                let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, v)| v.used)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                self.entries.remove(&oldest);
            }
            self.entries.insert(
                account.into(),
                Checkpoint {
                    cutoff,
                    window_start: start,
                    fingerprint: digest.finalize().into(),
                    state: checkpoint.unwrap(),
                    used: self.clock,
                    bytes,
                },
            );
        }
        Ok((
            history,
            ReplayStats {
                total_events: events.len(),
                reused_events: reused,
                replayed_events: events.len() - reused,
                retained,
            },
        ))
    }
}

fn string(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}
fn number(hash: &mut Sha256, value: f64) {
    hash.update(value.to_bits().to_le_bytes());
}
fn optional(hash: &mut Sha256, value: Option<f64>) {
    hash.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        number(hash, value);
    }
}
fn seed(context: &str, source: PnlSourceKind, positions: &[InitialPosition]) -> Sha256 {
    let mut hash = Sha256::new();
    string(&mut hash, context);
    hash.update([source as u8]);
    hash.update((positions.len() as u64).to_le_bytes());
    for p in positions {
        string(&mut hash, &p.symbol);
        number(&mut hash, p.spot_quantity);
        number(&mut hash, p.futures_quantity);
        number(&mut hash, p.mark_price);
    }
    hash
}
fn hash_event(hash: &mut Sha256, event: &PnlEvent) {
    hash.update(event.ts().to_le_bytes());
    hash.update([event.rank()]);
    string(hash, event.symbol());
    match event {
        PnlEvent::Trade(t) => {
            hash.update([t.side as u8, t.leg as u8]);
            number(hash, t.price);
            number(hash, t.quantity);
            number(hash, t.amount_u);
            optional(hash, t.fee_usdt);
        }
        PnlEvent::Funding(f) => number(hash, f.amount_usdt),
        PnlEvent::Interest(i) => {
            optional(hash, i.cost_usdt);
            optional(hash, i.conversion_price);
            number(hash, i.spot_quantity_delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HOUR: i64 = 3_600_000;

    fn inputs(end: i64) -> PnlInputs {
        PnlInputs {
            trades: (0..=end / 60_000)
                .map(|n| NormalizedTrade {
                    symbol: if n % 3 == 0 { "BTCUSDT" } else { "ETHUSDT" }.into(),
                    side: if n % 17 < 11 { Side::Buy } else { Side::Sell },
                    leg: if n % 7 == 0 {
                        PositionLeg::Spot
                    } else {
                        PositionLeg::Futures
                    },
                    price: 100.0 + (n % 19) as f64 / 7.0,
                    amount_u: 5.0,
                    quantity: 0.05,
                    fee_usdt: Some(-0.0001),
                    ts: n * 60_000,
                })
                .collect(),
            funding: (0..=end / HOUR)
                .map(|n| FundingEvent {
                    symbol: "BTCUSDT".into(),
                    amount_usdt: -0.01,
                    ts: n * HOUR,
                })
                .collect(),
            interest: (0..=end / HOUR)
                .map(|n| InterestEvent {
                    symbol: "ETHUSDT".into(),
                    cost_usdt: Some(0.001),
                    conversion_price: Some(101.0),
                    spot_quantity_delta: -0.00001,
                    ts: n * HOUR,
                })
                .collect(),
            initial_positions: vec![InitialPosition {
                symbol: "BTCUSDT".into(),
                spot_quantity: 0.1,
                futures_quantity: -0.2,
                mark_price: 100.0,
            }],
        }
    }

    fn bounds(end: i64) -> Vec<i64> {
        (0..=end / HOUR).map(|n| n * HOUR).chain([end]).collect()
    }

    fn compare(
        cache: &mut ReplayCache,
        data: PnlInputs,
        source: PnlSourceKind,
        end: i64,
    ) -> ReplayStats {
        let start = end - 4 * HOUR;
        let expected =
            super::super::replay(source, data.clone(), start, end, &bounds(end)).unwrap();
        let (actual, stats) = cache
            .replay("a", "scope", source, data, start, end, &bounds(end))
            .unwrap();
        assert_eq!(actual, expected);
        stats
    }

    #[test]
    fn warm_replay_matches_cold_for_append_overlap_window_shift_and_both_fifo_models() {
        for source in [PnlSourceKind::FundingRate, PnlSourceKind::MarketMaking] {
            let mut cache = ReplayCache::default();
            assert_eq!(
                compare(&mut cache, inputs(8 * HOUR), source, 8 * HOUR).reused_events,
                0
            );
            for minutes in [5, 10, 30, 60, 90, 180] {
                let end = 8 * HOUR + minutes * 60_000;
                let stats = compare(&mut cache, inputs(end), source, end);
                assert!(stats.reused_events > 300);
                assert!(stats.replayed_events < stats.total_events);
            }
        }
    }

    #[test]
    fn old_corrections_deletions_insertions_and_fee_changes_invalidate_checkpoint() {
        for change in 0..6 {
            let mut cache = ReplayCache::default();
            compare(
                &mut cache,
                inputs(8 * HOUR),
                PnlSourceKind::FundingRate,
                8 * HOUR,
            );
            let end = 8 * HOUR + 5 * 60_000;
            let mut data = inputs(end);
            match change {
                0 => data.trades[1].price += 1.0,
                1 => data.trades[1].fee_usdt = Some(-0.5),
                2 => {
                    data.trades.remove(1);
                }
                3 => {
                    let mut t = data.trades[1].clone();
                    t.quantity = 0.001;
                    data.trades.push(t);
                }
                4 => data.funding[1].amount_usdt += 1.0,
                _ => data.interest[1].spot_quantity_delta -= 0.00001,
            }
            assert_eq!(
                compare(&mut cache, data, PnlSourceKind::FundingRate, end).reused_events,
                0
            );
        }
    }

    #[test]
    fn late_events_inside_overlap_are_replayed_without_invalidating_older_state() {
        let mut cache = ReplayCache::default();
        compare(
            &mut cache,
            inputs(8 * HOUR),
            PnlSourceKind::FundingRate,
            8 * HOUR,
        );
        let mut data = inputs(8 * HOUR + 5 * 60_000);
        let mut late = data.trades.last().unwrap().clone();
        late.ts = 7 * HOUR + 1;
        late.quantity = 0.003;
        data.trades.push(late);
        assert!(
            compare(
                &mut cache,
                data,
                PnlSourceKind::FundingRate,
                8 * HOUR + 5 * 60_000
            )
            .reused_events
                > 300
        );
    }

    #[test]
    fn initial_positions_source_and_context_changes_cannot_reuse_old_fifo() {
        for change in 0..3 {
            let mut cache = ReplayCache::default();
            compare(
                &mut cache,
                inputs(8 * HOUR),
                PnlSourceKind::FundingRate,
                8 * HOUR,
            );
            let mut data = inputs(8 * HOUR);
            let mut source = PnlSourceKind::FundingRate;
            let mut context = "scope";
            match change {
                0 => data.initial_positions[0].spot_quantity = 0.3,
                1 => source = PnlSourceKind::MarketMaking,
                _ => context = "new schema or anchor",
            }
            let expected =
                super::super::replay(source, data.clone(), 4 * HOUR, 8 * HOUR, &bounds(8 * HOUR))
                    .unwrap();
            let (actual, stats) = cache
                .replay(
                    "a",
                    context,
                    source,
                    data,
                    4 * HOUR,
                    8 * HOUR,
                    &bounds(8 * HOUR),
                )
                .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(stats.reused_events, 0);
        }
    }

    #[test]
    fn missing_historical_boundary_or_backwards_window_uses_full_replay() {
        let mut cache = ReplayCache::default();
        compare(
            &mut cache,
            inputs(8 * HOUR),
            PnlSourceKind::FundingRate,
            8 * HOUR,
        );
        let mut requested = bounds(8 * HOUR);
        requested.push(1);
        let data = inputs(8 * HOUR);
        let expected = super::super::replay(
            PnlSourceKind::FundingRate,
            data.clone(),
            4 * HOUR,
            8 * HOUR,
            &requested,
        )
        .unwrap();
        let (actual, stats) = cache
            .replay(
                "a",
                "scope",
                PnlSourceKind::FundingRate,
                data,
                4 * HOUR,
                8 * HOUR,
                &requested,
            )
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(stats.reused_events, 0);
        assert_eq!(
            compare(
                &mut cache,
                inputs(6 * HOUR),
                PnlSourceKind::FundingRate,
                6 * HOUR
            )
            .reused_events,
            0
        );
    }

    #[test]
    fn budget_eviction_and_errors_never_return_incomplete_history() {
        let mut cache = ReplayCache {
            max_bytes: 1,
            ..Default::default()
        };
        assert!(
            !compare(
                &mut cache,
                inputs(8 * HOUR),
                PnlSourceKind::FundingRate,
                8 * HOUR
            )
            .retained
        );
        assert!(cache.entries.is_empty());
        cache.max_bytes = MAX_BYTES;
        compare(
            &mut cache,
            inputs(8 * HOUR),
            PnlSourceKind::FundingRate,
            8 * HOUR,
        );
        let mut invalid = inputs(8 * HOUR);
        invalid.trades[1].fee_usdt = None;
        assert!(
            cache
                .replay(
                    "a",
                    "scope",
                    PnlSourceKind::FundingRate,
                    invalid,
                    4 * HOUR,
                    8 * HOUR,
                    &bounds(8 * HOUR)
                )
                .is_err()
        );
        assert!(cache.entries.is_empty());
        compare(
            &mut cache,
            inputs(8 * HOUR),
            PnlSourceKind::FundingRate,
            8 * HOUR,
        );
        cache.retain_accounts(&HashSet::new());
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn account_state_is_isolated_and_lru_eviction_falls_back_to_full_history() {
        let mut cache = ReplayCache::default();
        let end = 8 * HOUR;
        let a = inputs(end);
        cache
            .replay(
                "a",
                "scope",
                PnlSourceKind::FundingRate,
                a.clone(),
                4 * HOUR,
                end,
                &bounds(end),
            )
            .unwrap();
        let mut b = a.clone();
        b.initial_positions[0].spot_quantity = 10.0;
        for t in &mut b.trades {
            t.quantity *= 2.0;
        }
        let b_expected = super::super::replay(
            PnlSourceKind::FundingRate,
            b.clone(),
            4 * HOUR,
            end,
            &bounds(end),
        )
        .unwrap();
        let (actual, _) = cache
            .replay(
                "b",
                "scope",
                PnlSourceKind::FundingRate,
                b.clone(),
                4 * HOUR,
                end,
                &bounds(end),
            )
            .unwrap();
        assert_eq!(actual, b_expected);
        assert!(
            compare(&mut cache, a.clone(), PnlSourceKind::FundingRate, end).reused_events > 300
        );
        let mut eviction = ReplayCache::default();
        compare(&mut eviction, a.clone(), PnlSourceKind::FundingRate, end);
        eviction.max_bytes = eviction.entries["a"].bytes;
        let (_, stats) = eviction
            .replay(
                "b",
                "scope",
                PnlSourceKind::FundingRate,
                a.clone(),
                4 * HOUR,
                end,
                &bounds(end),
            )
            .unwrap();
        assert!(stats.retained);
        assert!(eviction.entries.values().map(|e| e.bytes).sum::<usize>() <= eviction.max_bytes);
        assert!(!eviction.entries.contains_key("a"));
        assert_eq!(
            compare(&mut eviction, a, PnlSourceKind::FundingRate, end).reused_events,
            0
        );
    }
}
