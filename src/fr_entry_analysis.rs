//! Liquidity mix and premium evolution for the current FIFO position.
//! The last matched fill is an observed boundary, not a target-completion claim.
use crate::fr_entry_cost::{Fill, Leg, Lot, MINUTE_MS, Report, minute};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiquidityBucket {
    pub fill_count: usize,
    pub quantity: f64,
    pub notional_usdt: Option<f64>,
    pub fee_usdt: Option<f64>,
    pub unconverted_fee_parts: usize,
    #[serde(skip)]
    ids: HashSet<String>,
}

impl Default for LiquidityBucket {
    fn default() -> Self {
        Self {
            fill_count: 0,
            quantity: 0.0,
            notional_usdt: Some(0.0),
            fee_usdt: Some(0.0),
            unconverted_fee_parts: 0,
            ids: HashSet::new(),
        }
    }
}

impl LiquidityBucket {
    fn add(&mut self, lot: &Lot) {
        self.ids.insert(lot.id.clone());
        self.fill_count = self.ids.len();
        self.quantity += lot.quantity;
        self.notional_usdt = self
            .notional_usdt
            .zip(lot.price)
            .map(|(n, p)| n + lot.quantity * p);
        self.fee_usdt = self.fee_usdt.zip(lot.fee_usdt).map(|(a, b)| a + b);
        if lot.fee_usdt.is_none() {
            self.unconverted_fee_parts += 1;
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegLiquidity {
    pub maker: LiquidityBucket,
    pub taker: LiquidityBucket,
    pub unknown: LiquidityBucket,
    pub total: LiquidityBucket,
    pub maker_notional_pct: Option<f64>,
    pub taker_notional_pct: Option<f64>,
    pub unknown_notional_pct: Option<f64>,
}

impl LegLiquidity {
    fn add(&mut self, lot: &Lot) {
        self.total.add(lot);
        match lot.liquidity.to_ascii_lowercase().as_str() {
            "maker" => self.maker.add(lot),
            "taker" => self.taker.add(lot),
            _ => self.unknown.add(lot),
        }
        let pct = |n: Option<f64>| {
            n.zip(self.total.notional_usdt)
                .filter(|(_, t)| *t > 0.0)
                .map(|(n, t)| 100.0 * n / t)
        };
        self.maker_notional_pct = pct(self.maker.notional_usdt);
        self.taker_notional_pct = pct(self.taker.notional_usdt);
        self.unknown_notional_pct = pct(self.unknown.notional_usdt);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiquidityMix {
    pub spot: LegLiquidity,
    pub futures: LegLiquidity,
}

impl LiquidityMix {
    fn add(&mut self, lot: &Lot) {
        match lot.leg {
            Leg::Spot => self.spot.add(lot),
            Leg::Futures => self.futures.add(lot),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BasisPoint {
    pub ts: i64,
    pub premium_bps: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub event_ms: i64,
    pub reference_minute_ms: i64,
    pub premium_bps: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BasisEvolution {
    pub start: Option<Milestone>,
    pub last_pair: Option<Milestone>,
    pub as_of: Milestone,
    pub is_scenario: bool,
    pub build_change_bps: Option<f64>,
    pub after_pair_change_bps: Option<f64>,
    pub total_change_bps: Option<f64>,
    pub covered_minutes: usize,
    pub expected_minutes: usize,
    pub points: Vec<BasisPoint>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryAnalysis {
    pub paired_liquidity: LiquidityMix,
    pub window_liquidity: Option<LiquidityMix>,
    pub spot_vwap: Option<f64>,
    pub futures_vwap: Option<f64>,
    /// Raw F - S, not direction-adjusted.
    pub fill_spread: Option<f64>,
    pub fill_spread_bps: Option<f64>,
    pub execution_deviation_bps: Option<f64>,
    pub deviation_assessment: &'static str,
    pub basis: BasisEvolution,
}

pub fn calculate(
    report: &Report,
    fills: &[Fill],
    premiums: &BTreeMap<i64, f64>,
    as_of_ms: i64,
    is_scenario: bool,
) -> EntryAnalysis {
    let mut paired_liquidity = LiquidityMix::default();
    for p in &report.pairs {
        paired_liquidity.add(&p.spot);
        paired_liquidity.add(&p.futures);
    }
    let prices_known = !report.pairs.is_empty()
        && report
            .pairs
            .iter()
            .all(|p| p.spot.price.is_some() && p.futures.price.is_some());
    let start_ms = prices_known.then(|| {
        report
            .pairs
            .iter()
            .map(|p| p.spot.ts.min(p.futures.ts))
            .min()
            .unwrap()
    });
    let last_pair_ms = report
        .pairs
        .iter()
        .filter(|p| p.spot.price.is_some() || p.futures.price.is_some())
        .map(|p| p.completed_at_ms)
        .max();
    let mut window_liquidity = None;
    if let Some((start, end)) = start_ms.zip(last_pair_ms) {
        let mut mix = LiquidityMix::default();
        for f in fills.iter().filter(|f| f.ts >= start && f.ts <= end) {
            mix.add(&Lot {
                id: f.id.clone(),
                order_id: f.order_id.clone(),
                ts: f.ts,
                leg: f.leg,
                side: f.side,
                price: Some(f.price),
                quantity: f.quantity,
                fee_usdt: f.fee_usdt,
                liquidity: f.liquidity.clone(),
            });
        }
        window_liquidity = Some(mix);
    }
    let vwap = |leg: &LegLiquidity| {
        leg.total
            .notional_usdt
            .filter(|_| leg.total.quantity > 0.0)
            .map(|n| n / leg.total.quantity)
    };
    let spot_vwap = vwap(&paired_liquidity.spot);
    let futures_vwap = vwap(&paired_liquidity.futures);
    let fill_spread = spot_vwap.zip(futures_vwap).map(|(s, f)| f - s);
    let fill_spread_bps = fill_spread.zip(spot_vwap).map(|(b, s)| b / s * 10_000.0);
    let execution_deviation_bps = report
        .summary
        .execution_deviation_usdt
        .zip(report.summary.notional_usdt)
        .filter(|(_, n)| *n > 0.0)
        .map(|(d, n)| d / n * 10_000.0);
    let deviation_assessment = match execution_deviation_bps {
        Some(d) if d > 0.0001 => "adverse",
        Some(d) if d < -0.0001 => "favorable",
        Some(_) => "at_reference",
        None => "unknown",
    };
    let point = |event_ms: i64, reference_minute_ms: i64| Milestone {
        event_ms,
        reference_minute_ms,
        premium_bps: premiums
            .get(&reference_minute_ms)
            .filter(|p| p.is_finite())
            .map(|p| p * 10_000.0),
    };
    let start = start_ms.map(|t| point(t, minute(t)));
    let last_pair = last_pair_ms.map(|t| point(t, minute(t)));
    // "Now" must be a fully closed minute available at the requested as-of time.
    let as_of = point(as_of_ms, minute(as_of_ms) - MINUTE_MS);
    let change = |a: Option<&Milestone>, b: Option<&Milestone>| {
        if is_scenario {
            return None;
        }
        a.zip(b)
            .filter(|(a, b)| b.reference_minute_ms >= a.reference_minute_ms)
            .and_then(|(a, b)| a.premium_bps.zip(b.premium_bps))
            .map(|(a, b)| b - a)
    };
    let mut points = Vec::new();
    let mut expected_minutes = 0;
    let mut covered_minutes = 0;
    if let Some(start) = &start {
        let begin = start.reference_minute_ms;
        let end = as_of.reference_minute_ms;
        if begin <= end {
            expected_minutes = ((end - begin) / MINUTE_MS + 1) as usize;
            let mut previous = begin - MINUTE_MS;
            for (&ts, &rate) in premiums.range(begin..=end).filter(|(_, r)| r.is_finite()) {
                // Explicit null points break the line at missing minutes, without
                // materializing months of missing samples or interpolating them.
                if ts > previous + MINUTE_MS {
                    points.push(BasisPoint {
                        ts: previous + MINUTE_MS,
                        premium_bps: None,
                    });
                }
                points.push(BasisPoint {
                    ts,
                    premium_bps: Some(rate * 10_000.0),
                });
                previous = ts;
                covered_minutes += 1;
            }
            if previous < end {
                points.push(BasisPoint {
                    ts: end,
                    premium_bps: None,
                });
            }
        }
    }
    let basis = BasisEvolution {
        build_change_bps: change(start.as_ref(), last_pair.as_ref()),
        after_pair_change_bps: change(last_pair.as_ref(), Some(&as_of)),
        total_change_bps: change(start.as_ref(), Some(&as_of)),
        start,
        last_pair,
        as_of,
        is_scenario,
        covered_minutes,
        expected_minutes,
        points,
    };
    EntryAnalysis {
        paired_liquidity,
        window_liquidity,
        spot_vwap,
        futures_vwap,
        fill_spread,
        fill_spread_bps,
        execution_deviation_bps,
        deviation_assessment,
        basis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fr_entry_cost::{Inventory, Seed};
    fn fill(id: &str, ts: i64, leg: Leg, q: f64, price: f64, role: &str, fee: f64) -> Fill {
        Fill {
            id: id.into(),
            order_id: id.into(),
            ts,
            leg,
            side: if leg == Leg::Spot { 1 } else { -1 },
            quantity: q,
            price,
            fee_usdt: Some(fee),
            liquidity: role.into(),
        }
    }
    fn run(fills: &[Fill], p: &BTreeMap<i64, f64>, now: i64) -> EntryAnalysis {
        let mut inv = Inventory::default();
        for f in fills {
            inv.apply(f.clone()).unwrap();
        }
        calculate(&inv.report(p), fills, p, now, false)
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-8, "{a} != {b}");
    }

    #[test]
    fn amount_weighted_liquidity_is_not_fill_count_and_splits_are_not_double_counted() {
        let fills = [
            fill("s1", 1, Leg::Spot, 9.0, 100.0, "maker", -0.9),
            fill("s2", 2, Leg::Spot, 1.0, 100.0, "taker", 0.2),
            fill("f1", 3, Leg::Futures, 4.0, 101.0, "maker", 0.4),
            fill("f2", 4, Leg::Futures, 6.0, 101.0, "unknown", 0.6),
        ];
        let a = run(&fills, &BTreeMap::from([(0, 0.01)]), 120000);
        near(a.paired_liquidity.spot.maker_notional_pct.unwrap(), 90.0);
        assert_eq!(a.paired_liquidity.spot.maker.fill_count, 1);
        near(a.paired_liquidity.spot.total.fee_usdt.unwrap(), -0.7);
        near(
            a.paired_liquidity.futures.unknown_notional_pct.unwrap(),
            60.0,
        );
        near(a.fill_spread.unwrap(), 1.0);
    }

    #[test]
    fn separates_build_drift_from_post_pair_drift_and_does_not_claim_completion() {
        let fills = [
            fill("s", 60001, Leg::Spot, 1.0, 100.0, "maker", 0.0),
            fill("f", 180001, Leg::Futures, 1.0, 100.0, "taker", 0.0),
        ];
        let p = BTreeMap::from([
            (60000, 0.001),
            (120000, 0.002),
            (180000, 0.003),
            (240000, 0.004),
            (300000, 0.002),
        ]);
        let a = run(&fills, &p, 360000);
        near(a.basis.build_change_bps.unwrap(), 20.0);
        near(a.basis.after_pair_change_bps.unwrap(), -10.0);
        near(a.basis.total_change_bps.unwrap(), 10.0);
        assert_eq!(a.basis.last_pair.unwrap().event_ms, 180001);
        assert_eq!(a.basis.as_of.reference_minute_ms, 300000);
    }

    #[test]
    fn missing_anchor_and_unknown_fee_remain_unknown_and_trend_breaks_at_gaps() {
        let mut s = fill("s", 1, Leg::Spot, 1.0, 100.0, "maker", 0.0);
        s.fee_usdt = None;
        let a = run(
            &[s, fill("f", 120001, Leg::Futures, 1.0, 101.0, "taker", 0.0)],
            &BTreeMap::from([(0, 0.001), (180000, 0.003)]),
            240000,
        );
        assert_eq!(a.paired_liquidity.spot.total.fee_usdt, None);
        assert_eq!(a.basis.build_change_bps, None);
        assert_eq!(a.basis.after_pair_change_bps, None);
        assert!(
            a.basis
                .points
                .iter()
                .any(|p| p.ts == 60000 && p.premium_bps.is_none())
        );
        assert_eq!(a.basis.covered_minutes, 2);
        assert_eq!(a.basis.expected_minutes, 4);
    }

    #[test]
    fn unknown_initial_cost_does_not_invent_a_build_start() {
        let mut book = Inventory::default();
        book.seed(Seed {
            ts: 0,
            leg: Leg::Spot,
            signed_quantity: 1.0,
        })
        .unwrap();
        let f = fill("f", 60001, Leg::Futures, 1.0, 100.0, "maker", 0.0);
        book.apply(f.clone()).unwrap();
        let a = calculate(
            &book.report(&BTreeMap::new()),
            &[f],
            &BTreeMap::new(),
            120000,
            false,
        );
        assert!(a.basis.start.is_none());
        assert!(a.window_liquidity.is_none());
        assert_eq!(a.basis.last_pair.unwrap().event_ms, 60001);
    }

    #[test]
    fn partial_closes_preserve_window_turnover_but_only_retained_cost_in_paired_mix() {
        let mut fills = vec![
            fill("s", 1, Leg::Spot, 10.0, 100.0, "maker", 1.0),
            fill("f", 2, Leg::Futures, 10.0, 101.0, "maker", 1.0),
        ];
        let mut close = fill("sc", 60001, Leg::Spot, 5.0, 102.0, "taker", 3.0);
        close.side = -1;
        fills.push(close);
        let mut close = fill("fc", 60002, Leg::Futures, 5.0, 103.0, "taker", 3.0);
        close.side = 1;
        fills.push(close);
        fills.push(fill("s2", 120001, Leg::Spot, 2.0, 104.0, "taker", 0.4));
        fills.push(fill("f2", 120002, Leg::Futures, 2.0, 105.0, "taker", 0.4));
        let a = run(&fills, &BTreeMap::new(), 180000);
        near(a.paired_liquidity.spot.total.quantity, 7.0);
        near(a.window_liquidity.unwrap().spot.total.quantity, 17.0);
    }

    #[test]
    fn a_manual_constant_is_not_evidence_of_unchanged_market_basis() {
        let fills = [
            fill("s", 1, Leg::Spot, 1.0, 100.0, "maker", 0.0),
            fill("f", 2, Leg::Futures, 1.0, 101.0, "maker", 0.0),
        ];
        let p = BTreeMap::from([(0, 0.0), (60000, 0.0)]);
        let mut book = Inventory::default();
        for f in &fills {
            book.apply(f.clone()).unwrap();
        }
        let a = calculate(&book.report(&p), &fills, &p, 120000, true);
        assert!(a.basis.is_scenario);
        assert!(a.basis.total_change_bps.is_none());
    }
}
