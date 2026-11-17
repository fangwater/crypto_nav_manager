//! Current-position entry-cost attribution, independent of the NAV PnL ledger.
//! FIFO is per leg in base units. Cross-leg pairing is an inventory convention,
//! not an assertion that two fills share a parent order.

use anyhow::{Result, bail};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

pub const MINUTE_MS: i64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Leg {
    Spot,
    Futures,
}

#[derive(Clone, Debug)]
pub struct Fill {
    pub id: String,
    pub order_id: String,
    pub ts: i64,
    pub leg: Leg,
    /// Buy = 1; sell = -1.
    pub side: i8,
    pub price: f64,
    pub quantity: f64,
    /// Positive cost, negative rebate; None is not zero.
    pub fee_usdt: Option<f64>,
    pub liquidity: String,
}

#[derive(Clone, Debug)]
pub struct Seed {
    pub ts: i64,
    pub leg: Leg,
    pub signed_quantity: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lot {
    pub id: String,
    pub order_id: String,
    pub ts: i64,
    pub leg: Leg,
    pub side: i8,
    pub price: Option<f64>,
    pub quantity: f64,
    pub fee_usdt: Option<f64>,
    pub liquidity: String,
}

impl Lot {
    fn take(&mut self, quantity: f64) -> Self {
        let mut part = self.clone();
        part.quantity = quantity;
        part.fee_usdt = self.fee_usdt.map(|fee| fee * quantity / self.quantity);
        self.fee_usdt = self.fee_usdt.zip(part.fee_usdt).map(|(a, b)| a - b);
        self.quantity -= quantity;
        part
    }
}

#[derive(Clone, Debug, Default)]
pub struct Inventory {
    spot: VecDeque<Lot>,
    futures: VecDeque<Lot>,
    pub trade_count: usize,
}

impl Inventory {
    pub fn seed(&mut self, seed: Seed) -> Result<()> {
        if !seed.signed_quantity.is_finite() || seed.ts < 0 {
            bail!("invalid initial quantity or timestamp");
        }
        if seed.signed_quantity == 0.0 {
            return Ok(());
        }
        self.apply_lot(Lot {
            id: "initial-position".into(),
            order_id: String::new(),
            ts: seed.ts,
            leg: seed.leg,
            side: if seed.signed_quantity > 0.0 { 1 } else { -1 },
            price: None,
            quantity: seed.signed_quantity.abs(),
            fee_usdt: None,
            liquidity: "unknown".into(),
        });
        Ok(())
    }

    /// Call in deterministic chronological order, including pre-window fills.
    pub fn apply(&mut self, fill: Fill) -> Result<()> {
        if !matches!(fill.side, -1 | 1)
            || !fill.price.is_finite()
            || fill.price <= 0.0
            || !fill.quantity.is_finite()
            || fill.quantity <= 0.0
            || fill.ts < 0
            || fill.fee_usdt.is_some_and(|fee| !fee.is_finite())
        {
            bail!("invalid FR entry-cost fill {}", fill.id);
        }
        self.trade_count += 1;
        self.apply_lot(Lot {
            id: fill.id,
            order_id: fill.order_id,
            ts: fill.ts,
            leg: fill.leg,
            side: fill.side,
            price: Some(fill.price),
            quantity: fill.quantity,
            fee_usdt: fill.fee_usdt,
            liquidity: fill.liquidity,
        });
        Ok(())
    }

    fn apply_lot(&mut self, mut incoming: Lot) {
        let queue = match incoming.leg {
            Leg::Spot => &mut self.spot,
            Leg::Futures => &mut self.futures,
        };
        while incoming.quantity > 0.0 {
            let Some(oldest) = queue.front_mut() else {
                break;
            };
            if oldest.side == incoming.side {
                break;
            }
            let quantity = oldest.quantity.min(incoming.quantity);
            oldest.take(quantity);
            incoming.take(quantity);
            if oldest.quantity == 0.0 {
                queue.pop_front();
            }
        }
        if incoming.quantity > 0.0 {
            queue.push_back(incoming);
        }
    }

    pub fn quantities(&self) -> (f64, f64) {
        let signed = |q: &VecDeque<Lot>| q.iter().map(|l| l.quantity * f64::from(l.side)).sum();
        (signed(&self.spot), signed(&self.futures))
    }

    pub fn oldest_pair_time(&self) -> Option<i64> {
        let s = self.spot.front()?;
        let f = self.futures.front()?;
        (s.side != f.side).then_some(s.ts.min(f.ts))
    }

    pub fn report(self, premiums: &BTreeMap<i64, f64>) -> Report {
        let (spot_quantity, futures_quantity) = self.quantities();
        let mut spot = self.spot;
        let mut futures = self.futures;
        let reference = Reference::new(premiums);
        let mut pairs = Vec::new();
        while let (Some(s), Some(f)) = (spot.front_mut(), futures.front_mut()) {
            if s.side == f.side {
                break;
            }
            let quantity = s.quantity.min(f.quantity);
            let s_part = s.take(quantity);
            let f_part = f.take(quantity);
            if s.quantity == 0.0 {
                spot.pop_front();
            }
            if f.quantity == 0.0 {
                futures.pop_front();
            }
            pairs.push(Pair::new(s_part, f_part, &reference));
        }
        let mut summary = Totals::default();
        let mut minutes: BTreeMap<i64, Totals> = BTreeMap::new();
        for pair in &pairs {
            summary.add(pair);
            minutes
                .entry(minute(pair.completed_at_ms))
                .or_default()
                .add(pair);
        }
        let minutes = minutes
            .into_iter()
            .map(|(ts, totals)| Minute { ts, totals })
            .collect();
        Report {
            spot_quantity,
            futures_quantity,
            net_quantity: spot_quantity + futures_quantity,
            trade_count: self.trade_count,
            summary,
            minutes,
            pairs,
            unmatched: spot.into_iter().chain(futures).collect(),
        }
    }
}

pub fn minute(ts: i64) -> i64 {
    ts.div_euclid(MINUTE_MS) * MINUTE_MS
}

struct Reference {
    times: Vec<i64>,
    sums: Vec<f64>,
}

impl Reference {
    fn new(premiums: &BTreeMap<i64, f64>) -> Self {
        let mut times = Vec::new();
        let mut sums = vec![0.0];
        for (&ts, &value) in premiums {
            if ts >= 0 && ts % MINUTE_MS == 0 && value.is_finite() {
                times.push(ts);
                sums.push(sums.last().unwrap() + value);
            }
        }
        Self { times, sums }
    }

    fn average(&self, start: i64, end: i64) -> (Option<f64>, usize, usize) {
        let start = minute(start);
        let end = minute(end);
        let expected = ((end - start) / MINUTE_MS + 1) as usize;
        let lo = self.times.partition_point(|&t| t < start);
        let hi = self.times.partition_point(|&t| t <= end);
        let found = hi - lo;
        let value = (found == expected).then(|| (self.sums[hi] - self.sums[lo]) / expected as f64);
        (value, found, expected)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pair {
    pub spot: Lot,
    pub futures: Lot,
    pub quantity: f64,
    pub direction: &'static str,
    pub completed_at_ms: i64,
    pub lag_ms: i64,
    pub cross_minute: bool,
    pub notional_usdt: Option<f64>,
    /// Direction-adjusted entry spread; positive is favorable to this position.
    pub entry_spread_usdt: Option<f64>,
    pub fee_usdt: Option<f64>,
    pub premium_rate: Option<f64>,
    pub reference_minutes: usize,
    pub expected_minutes: usize,
    pub reference_spread_usdt: Option<f64>,
    /// Includes waiting and benchmark differences; NOT identified pure slippage.
    pub execution_deviation_usdt: Option<f64>,
    pub estimated_cost_usdt: Option<f64>,
}

impl Pair {
    fn new(spot: Lot, futures: Lot, reference: &Reference) -> Self {
        let quantity = spot.quantity;
        let direction = f64::from(spot.side);
        let start = spot.ts.min(futures.ts);
        let end = spot.ts.max(futures.ts);
        let (premium_rate, reference_minutes, expected_minutes) = reference.average(start, end);
        let notional_usdt = spot.price.map(|s| quantity * s);
        let entry_spread_usdt = spot
            .price
            .zip(futures.price)
            .map(|(s, f)| direction * quantity * (f - s));
        let fee_usdt = spot.fee_usdt.zip(futures.fee_usdt).map(|(s, f)| s + f);
        let reference_spread_usdt = notional_usdt
            .zip(premium_rate)
            .map(|(n, p)| direction * n * p);
        let execution_deviation_usdt = reference_spread_usdt
            .zip(entry_spread_usdt)
            .map(|(r, a)| r - a);
        let estimated_cost_usdt = execution_deviation_usdt.zip(fee_usdt).map(|(d, f)| d + f);
        Self {
            direction: if spot.side == 1 {
                "positive"
            } else {
                "reverse"
            },
            quantity,
            completed_at_ms: end,
            lag_ms: end - start,
            cross_minute: minute(start) != minute(end),
            notional_usdt,
            entry_spread_usdt,
            fee_usdt,
            premium_rate,
            reference_minutes,
            expected_minutes,
            reference_spread_usdt,
            execution_deviation_usdt,
            estimated_cost_usdt,
            spot,
            futures,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub pair_count: usize,
    pub quantity: f64,
    pub priced_quantity: f64,
    pub reference_covered_quantity: f64,
    pub cost_covered_quantity: f64,
    pub cross_minute_quantity: f64,
    pub max_lag_ms: i64,
    pub notional_usdt: Option<f64>,
    pub entry_spread_usdt: Option<f64>,
    pub fee_usdt: Option<f64>,
    pub reference_spread_usdt: Option<f64>,
    pub execution_deviation_usdt: Option<f64>,
    pub estimated_cost_usdt: Option<f64>,
    pub estimated_cost_bps: Option<f64>,
}

impl Default for Totals {
    fn default() -> Self {
        Self {
            pair_count: 0,
            quantity: 0.0,
            priced_quantity: 0.0,
            reference_covered_quantity: 0.0,
            cost_covered_quantity: 0.0,
            cross_minute_quantity: 0.0,
            max_lag_ms: 0,
            notional_usdt: Some(0.0),
            entry_spread_usdt: Some(0.0),
            fee_usdt: Some(0.0),
            reference_spread_usdt: Some(0.0),
            execution_deviation_usdt: Some(0.0),
            estimated_cost_usdt: Some(0.0),
            estimated_cost_bps: None,
        }
    }
}

impl Totals {
    fn add(&mut self, pair: &Pair) {
        let add = |a: Option<f64>, b: Option<f64>| a.zip(b).map(|(a, b)| a + b);
        self.pair_count += 1;
        self.quantity += pair.quantity;
        if pair.entry_spread_usdt.is_some() {
            self.priced_quantity += pair.quantity;
        }
        if pair.premium_rate.is_some() {
            self.reference_covered_quantity += pair.quantity;
        }
        if pair.estimated_cost_usdt.is_some() {
            self.cost_covered_quantity += pair.quantity;
        }
        if pair.cross_minute {
            self.cross_minute_quantity += pair.quantity;
        }
        self.max_lag_ms = self.max_lag_ms.max(pair.lag_ms);
        self.notional_usdt = add(self.notional_usdt, pair.notional_usdt);
        self.entry_spread_usdt = add(self.entry_spread_usdt, pair.entry_spread_usdt);
        self.fee_usdt = add(self.fee_usdt, pair.fee_usdt);
        self.reference_spread_usdt = add(self.reference_spread_usdt, pair.reference_spread_usdt);
        self.execution_deviation_usdt =
            add(self.execution_deviation_usdt, pair.execution_deviation_usdt);
        self.estimated_cost_usdt = add(self.estimated_cost_usdt, pair.estimated_cost_usdt);
        self.estimated_cost_bps = self
            .estimated_cost_usdt
            .zip(self.notional_usdt)
            .filter(|(_, n)| *n > 0.0)
            .map(|(c, n)| c / n * 10_000.0);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Minute {
    pub ts: i64,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub spot_quantity: f64,
    pub futures_quantity: f64,
    pub net_quantity: f64,
    pub trade_count: usize,
    pub summary: Totals,
    pub minutes: Vec<Minute>,
    pub pairs: Vec<Pair>,
    pub unmatched: Vec<Lot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(id: &str, ts: i64, leg: Leg, side: i8, quantity: f64, price: f64, fee: f64) -> Fill {
        Fill {
            id: id.into(),
            order_id: id.into(),
            ts,
            leg,
            side,
            quantity,
            price,
            fee_usdt: Some(fee),
            liquidity: "maker".into(),
        }
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-8, "{a} != {b}");
    }

    #[test]
    fn entry_cost_is_reference_minus_fill_plus_actual_fee() {
        let mut book = Inventory::default();
        book.apply(fill("s", 1, Leg::Spot, 1, 10.0, 100.02, 0.1))
            .unwrap();
        book.apply(fill("f", 2, Leg::Futures, -1, 10.0, 100.08, 0.1))
            .unwrap();
        let report = book.report(&BTreeMap::from([(0, 0.001)]));
        near(report.summary.estimated_cost_usdt.unwrap(), 0.6002);
        near(report.summary.entry_spread_usdt.unwrap(), 0.6);
        near(report.net_quantity, 0.0);
    }

    #[test]
    fn cross_minute_futures_first_averages_each_required_minute() {
        let mut book = Inventory::default();
        book.apply(fill("f", 1, Leg::Futures, -1, 2.0, 100.0, 0.0))
            .unwrap();
        book.apply(fill("s", 120_001, Leg::Spot, 1, 2.0, 100.0, 0.0))
            .unwrap();
        let report = book.report(&BTreeMap::from([
            (0, 0.001),
            (60_000, 0.002),
            (120_000, 0.003),
        ]));
        near(report.pairs[0].premium_rate.unwrap(), 0.002);
        near(report.summary.estimated_cost_usdt.unwrap(), 0.4);
        assert!(report.pairs[0].cross_minute);
        assert_eq!(report.minutes[0].ts, 120_000);
    }

    #[test]
    fn partial_close_keeps_oldest_remaining_cost_and_prorates_fees() {
        let mut book = Inventory::default();
        for f in [
            fill("s1", 1, Leg::Spot, 1, 2.0, 100.0, 2.0),
            fill("s2", 2, Leg::Spot, 1, 3.0, 110.0, 3.0),
            fill("f", 3, Leg::Futures, -1, 5.0, 112.0, 5.0),
            fill("sc", 4, Leg::Spot, -1, 3.0, 120.0, 9.0),
            fill("fc", 5, Leg::Futures, 1, 3.0, 121.0, 9.0),
        ] {
            book.apply(f).unwrap();
        }
        let r = book.report(&BTreeMap::from([(0, 0.0)]));
        near(r.summary.quantity, 2.0);
        near(r.summary.fee_usdt.unwrap(), 4.0);
        assert_eq!(r.pairs[0].spot.id, "s2");
        near(r.summary.entry_spread_usdt.unwrap(), 4.0);
        near(r.summary.estimated_cost_usdt.unwrap(), 0.0);
    }

    #[test]
    fn reversal_and_rebate_have_correct_sign_without_false_neutrality() {
        let mut book = Inventory::default();
        for f in [
            fill("s", 1, Leg::Spot, 1, 2.0, 100.0, 2.0),
            fill("sr", 2, Leg::Spot, -1, 5.0, 101.0, -5.0),
            fill("f", 3, Leg::Futures, 1, 2.0, 100.0, 0.0),
        ] {
            book.apply(f).unwrap();
        }
        let r = book.report(&BTreeMap::from([(0, 0.0)]));
        near(r.summary.entry_spread_usdt.unwrap(), 2.0);
        near(r.summary.fee_usdt.unwrap(), -2.0);
        near(r.summary.estimated_cost_usdt.unwrap(), -4.0);
        near(r.net_quantity, -1.0);
        near(r.unmatched[0].quantity, 1.0);
    }

    #[test]
    fn missing_middle_minute_invalidates_cost_instead_of_interpolating() {
        let mut book = Inventory::default();
        book.apply(fill("s", 1, Leg::Spot, 1, 1.0, 100.0, 0.0))
            .unwrap();
        book.apply(fill("f", 120_001, Leg::Futures, -1, 1.0, 100.0, 0.0))
            .unwrap();
        let r = book.report(&BTreeMap::from([(0, 0.001), (120_000, 0.003)]));
        assert_eq!(r.pairs[0].reference_minutes, 2);
        assert_eq!(r.pairs[0].expected_minutes, 3);
        assert_eq!(r.summary.estimated_cost_usdt, None);
        near(r.summary.cost_covered_quantity, 0.0);
    }

    #[test]
    fn unknown_initial_price_and_unknown_fees_are_not_zero_cost() {
        let mut book = Inventory::default();
        book.seed(Seed {
            ts: 0,
            leg: Leg::Spot,
            signed_quantity: 2.0,
        })
        .unwrap();
        let mut future = fill("f", 1, Leg::Futures, -1, 2.0, 100.0, 0.0);
        future.fee_usdt = None;
        book.apply(future).unwrap();
        let r = book.report(&BTreeMap::from([(0, 0.001)]));
        near(r.summary.quantity, 2.0);
        assert_eq!(r.summary.estimated_cost_usdt, None);
        assert_eq!(r.summary.entry_spread_usdt, None);
        assert_eq!(r.summary.fee_usdt, None);
    }
}
