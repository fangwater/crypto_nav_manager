//! Lean account replay using the source page's FIFO, fee, funding and interest model.
use super::*;
use std::collections::BTreeMap;

mod cache;
pub use cache::{ReplayCache, ReplayStats};

#[derive(Clone, Debug, PartialEq)]
pub struct History {
    pub boundaries: BTreeMap<i64, f64>,
    pub points: Vec<(i64, f64)>,
    pub realized_boundaries: BTreeMap<i64, f64>,
    pub daily_profit_boundaries: BTreeMap<i64, f64>,
    pub final_realized_net: f64,
    pub final_daily_profit: f64,
    pub final_pnl: f64,
    pub start_pnl: f64,
}

/// Boundary values precede fills at the boundary: a Shanghai-midnight fill
/// belongs to the new day. Keep every change for drawdown, without chart sampling.
pub fn replay(
    source: PnlSourceKind,
    inputs: PnlInputs,
    start: i64,
    end: i64,
    boundaries: &[i64],
) -> Result<History> {
    let mut states = HashMap::new();
    for position in &inputs.initial_positions {
        states
            .entry(position.symbol.clone())
            .or_insert_with(|| SymbolState::for_overview(source))
            .seed_position(position)?;
    }
    let mut events =
        Vec::with_capacity(inputs.trades.len() + inputs.funding.len() + inputs.interest.len());
    events.extend(inputs.trades.into_iter().map(PnlEvent::Trade));
    events.extend(inputs.funding.into_iter().map(PnlEvent::Funding));
    events.extend(inputs.interest.into_iter().map(PnlEvent::Interest));
    events.sort_by(|a, b| {
        a.ts()
            .cmp(&b.ts())
            .then_with(|| a.rank().cmp(&b.rank()))
            .then_with(|| a.symbol().cmp(b.symbol()))
    });
    let mut wanted = boundaries
        .iter()
        .copied()
        .filter(|ts| *ts <= end)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .peekable();
    let mut result = History {
        boundaries: BTreeMap::new(),
        realized_boundaries: BTreeMap::new(),
        daily_profit_boundaries: BTreeMap::new(),
        final_realized_net: 0.0,
        final_daily_profit: 0.0,
        points: Vec::new(),
        final_pnl: 0.0,
        start_pnl: 0.0,
    };
    let mut total = 0.0;
    let mut realized_net = 0.0;
    let mut daily_profit = 0.0;
    let mut started = false;
    for event in events.iter().filter(|event| event.ts() <= end) {
        if !started && event.ts() >= start {
            result.start_pnl = total;
            started = true;
        }
        while wanted.peek().is_some_and(|ts| *ts <= event.ts()) {
            let boundary = wanted.next().unwrap();
            result.boundaries.insert(boundary, total);
            result.realized_boundaries.insert(boundary, realized_net);
            result
                .daily_profit_boundaries
                .insert(boundary, daily_profit);
        }
        let symbol = event.symbol();
        states
            .entry(symbol.into())
            .or_insert_with(|| SymbolState::for_overview(source));
        let before = states[symbol].metrics();
        let daily_before = daily_net_pnl(&states[symbol], &before);
        apply_event(&mut states, event)?;
        let after = states[symbol].metrics();
        anyhow::ensure!(
            after.unconverted_fee_count == 0 && after.unconverted_interest_count == 0,
            "account history contains unconverted charges"
        );
        total += after.total_pnl_usdt - before.total_pnl_usdt;
        realized_net += realized_net_pnl(&after) - realized_net_pnl(&before);
        daily_profit += daily_net_pnl(&states[symbol], &after) - daily_before;
        if event.ts() >= start {
            if result.points.last().is_some_and(|p| p.0 == event.ts()) {
                result.points.last_mut().unwrap().1 = total;
            } else {
                result.points.push((event.ts(), total));
            }
        }
    }
    for ts in wanted {
        result.boundaries.insert(ts, total);
        result.realized_boundaries.insert(ts, realized_net);
        result.daily_profit_boundaries.insert(ts, daily_profit);
    }
    if !started {
        result.start_pnl = total;
    }
    result.points.push((end, total));
    result.final_pnl = total;
    result.final_realized_net = realized_net;
    result.final_daily_profit = daily_profit;
    anyhow::ensure!(
        total.is_finite() && realized_net.is_finite() && daily_profit.is_finite(),
        "non-finite account history"
    );
    Ok(result)
}

// Fees are already deducted in fee_after_pnl_usdt, including opening fees.
fn realized_net_pnl(metrics: &Metrics) -> f64 {
    metrics.fee_after_pnl_usdt + metrics.funding_pnl_usdt - metrics.interest_cost_usdt
}

fn daily_net_pnl(state: &SymbolState, metrics: &Metrics) -> f64 {
    realized_net_pnl(metrics)
        - state
            .paired_closings
            .as_ref()
            .map_or(0.0, |paired| paired.pending_profit)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn market_making_summary_preserves_its_existing_amount_fifo_model() {
        let inputs = PnlInputs {
            trades: vec![
                NormalizedTrade {
                    symbol: "BTCUSDT".into(),
                    side: Side::Buy,
                    leg: PositionLeg::Futures,
                    price: 100.0,
                    amount_u: 200.0,
                    quantity: 2.0,
                    fee_usdt: Some(1.0),
                    ts: 100,
                },
                NormalizedTrade {
                    symbol: "BTCUSDT".into(),
                    side: Side::Sell,
                    leg: PositionLeg::Futures,
                    price: 120.0,
                    amount_u: 120.0,
                    quantity: 1.0,
                    fee_usdt: Some(0.5),
                    ts: 110,
                },
            ],
            funding: vec![FundingEvent {
                symbol: "BTCUSDT".into(),
                amount_usdt: 2.0,
                ts: 115,
            }],
            ..Default::default()
        };
        let history = replay(
            PnlSourceKind::MarketMaking,
            inputs.clone(),
            0,
            200,
            &[100, 110, 200],
        )
        .unwrap();
        let full = calculate(
            inputs.clone(),
            PnlCalculation {
                source: PnlSourceKind::MarketMaking,
                exchange: "bybit".into(),
                strategy_start_ms: 0,
                start_ms: 0,
                end_ms: 200,
                selected_symbols: vec![],
                max_points: 1000,
                initial_snapshot_ts_ms: None,
                skipped_initial_position_count: 0,
                principal_usdt: None,
            },
        )
        .unwrap();
        assert_eq!(history.final_pnl, full.summary.total_pnl_usdt);
        assert_eq!(
            history.final_realized_net,
            full.summary.fee_after_pnl_usdt + full.summary.funding_pnl_usdt
        );
        let quantity = replay(PnlSourceKind::FundingRate, inputs, 0, 200, &[200]).unwrap();
        assert_ne!(history.final_realized_net, quantity.final_realized_net);
    }
    #[test]
    fn daily_realized_profit_excludes_open_marks_and_counts_closes_and_charges_once() {
        let trade = |side, price, quantity, ts| NormalizedTrade {
            symbol: "BTCUSDT".into(),
            side,
            leg: PositionLeg::Spot,
            price,
            amount_u: price * quantity,
            quantity,
            fee_usdt: Some(1.0),
            ts,
        };
        let inputs = PnlInputs {
            trades: vec![
                trade(Side::Buy, 100.0, 2.0, 100),
                trade(Side::Buy, 120.0, 1.0, 101),
                trade(Side::Sell, 130.0, 1.0, 102),
            ],
            funding: vec![FundingEvent {
                symbol: "BTCUSDT".into(),
                amount_usdt: 2.0,
                ts: 103,
            }],
            interest: vec![InterestEvent {
                symbol: "BTCUSDT".into(),
                cost_usdt: Some(0.5),
                conversion_price: None,
                spot_quantity_delta: 0.0,
                ts: 104,
            }],
            ..Default::default()
        };
        let history = replay(PnlSourceKind::FundingRate, inputs, 0, 200, &[100, 102, 200]).unwrap();
        assert_eq!(history.realized_boundaries[&100], 0.0);
        // Opening another lot changes floating marks by 40, but only its fee is realized.
        assert_eq!(history.boundaries[&102], 38.0);
        assert_eq!(history.realized_boundaries[&102], -2.0);
        // FIFO close gains 30, all three fees cost 3, funding gains 2, interest costs 0.5.
        assert_eq!(history.final_realized_net, 28.5);
        assert_eq!(history.final_daily_profit, 28.5);
        assert_eq!(history.realized_boundaries[&200], 28.5);
        assert_eq!(history.final_pnl, 68.5);
        // The midnight close and later receipts belong to the new day.
        assert_eq!(
            history.final_realized_net - history.realized_boundaries[&102],
            30.5
        );
    }

    #[test]
    fn fr_day_boundary_defers_only_the_closed_quantity_of_an_open_hedge() {
        let fill = |leg, side, price, quantity, fee_usdt, ts| NormalizedTrade {
            symbol: "BTCUSDT".into(),
            side,
            leg,
            price,
            amount_u: price * quantity,
            quantity,
            fee_usdt: Some(fee_usdt),
            ts,
        };
        let inputs = PnlInputs {
            trades: vec![
                fill(PositionLeg::Spot, Side::Buy, 100., 5., 0., 1),
                fill(PositionLeg::Futures, Side::Sell, 103., 4., 0., 2),
                fill(PositionLeg::Spot, Side::Sell, 70., 5., 2., 100),
                fill(PositionLeg::Futures, Side::Buy, 72., 2., 1., 150),
                fill(PositionLeg::Futures, Side::Buy, 71., 2., 1., 200),
            ],
            funding: vec![FundingEvent {
                symbol: "BTCUSDT".into(),
                amount_usdt: 3.,
                ts: 110,
            }],
            ..Default::default()
        };
        let boundaries = &[0, 140, 180, 250];
        let history = replay(
            PnlSourceKind::FundingRate,
            inputs.clone(),
            0,
            250,
            boundaries,
        )
        .unwrap();
        // One unpaired long closes for -30, fees -2 and funding +3. The
        // four hedged units are recognized only when the short closes.
        assert_eq!(history.realized_boundaries[&140], -149.);
        assert_eq!(history.daily_profit_boundaries[&140], -29.);
        assert_eq!(history.daily_profit_boundaries[&180], -28.);
        assert_eq!(history.daily_profit_boundaries[&250], -25.);
        assert_eq!(history.final_daily_profit, history.final_realized_net);

        let mut cache = ReplayCache::default();
        let hour = 3_600_000;
        let mut inputs_at_checkpoint = inputs.clone();
        for trade in &mut inputs_at_checkpoint.trades[3..] {
            trade.ts += hour;
        }
        let (_, first) = cache
            .replay(
                "a",
                "test",
                PnlSourceKind::FundingRate,
                inputs_at_checkpoint.clone(),
                0,
                hour + 141,
                &[0, 140],
            )
            .unwrap();
        assert!(first.retained);
        let boundaries = &[0, 140, hour + 180, hour + 250];
        let end = hour + 250;
        let reference = replay(
            PnlSourceKind::FundingRate,
            inputs_at_checkpoint.clone(),
            0,
            end,
            boundaries,
        )
        .unwrap();
        let (warm, stats) = cache
            .replay(
                "a",
                "test",
                PnlSourceKind::FundingRate,
                inputs_at_checkpoint,
                0,
                end,
                boundaries,
            )
            .unwrap();
        assert!(stats.reused_events > 0);
        assert_eq!(warm, reference);
        assert_eq!(warm.final_daily_profit, -25.);
    }

    #[test]
    fn daily_boundaries_include_fees_funding_and_interest_on_the_new_day() {
        let inputs = PnlInputs {
            trades: vec![NormalizedTrade {
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                leg: PositionLeg::Spot,
                price: 100.0,
                amount_u: 100.0,
                quantity: 1.0,
                fee_usdt: Some(1.0),
                ts: 100,
            }],
            funding: vec![FundingEvent {
                symbol: "BTCUSDT".into(),
                amount_usdt: 2.0,
                ts: 101,
            }],
            interest: vec![InterestEvent {
                symbol: "BTCUSDT".into(),
                cost_usdt: Some(0.5),
                conversion_price: None,
                spot_quantity_delta: 0.0,
                ts: 102,
            }],
            ..Default::default()
        };
        let history = replay(
            PnlSourceKind::FundingRate,
            inputs.clone(),
            0,
            200,
            &[100, 101, 102, 200],
        )
        .unwrap();
        assert_eq!(history.boundaries[&100], 0.0);
        assert_eq!(history.boundaries[&101], -1.0);
        assert_eq!(history.boundaries[&102], 1.0);
        let full = calculate(
            inputs,
            PnlCalculation {
                source: PnlSourceKind::FundingRate,
                exchange: "binance".into(),
                strategy_start_ms: 0,
                start_ms: 0,
                end_ms: 200,
                selected_symbols: vec![],
                max_points: 1000,
                initial_snapshot_ts_ms: None,
                skipped_initial_position_count: 0,
                principal_usdt: None,
            },
        )
        .unwrap();
        assert_eq!(history.final_pnl, full.summary.total_pnl_usdt);
        assert_eq!(history.final_pnl, 0.5);
        assert_eq!(history.realized_boundaries, history.boundaries);
        assert_eq!(history.final_realized_net, 0.5);
    }
}
