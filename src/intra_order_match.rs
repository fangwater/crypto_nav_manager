use anyhow::{Result, bail};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "buy" => Ok(Self::Buy),
            "sell" => Ok(Self::Sell),
            _ => bail!("unsupported order side {value:?}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }

    fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchingState {
    Pending,
    Completed,
    Netted,
    Mixed,
}

impl MatchingState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Netted => "netted",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MarginOrder {
    pub fkey: i64,
    pub symbol: String,
    pub side: Side,
    pub cts: i64,
    pub open_uts: i64,
    pub fts: Option<i64>,
    pub close_count: i64,
    pub price: f64,
    pub amount: f64,
    pub range: f64,
    pub tlen: Option<f64>,
    pub open_fill_amount: f64,
    pub remaining_amount: f64,
    pub camount: f64,
    pub netted_amount: f64,
    pub close_notional: f64,
    pub matching_state: MatchingState,
    pub open_source_ts_us: i64,
}

impl MarginOrder {
    pub fn cprice(&self) -> Option<f64> {
        (self.camount > 0.0).then(|| self.close_notional / self.camount)
    }

    pub fn holding(&self) -> i64 {
        self.open_uts - self.cts
    }

    pub fn holding_close(&self) -> Option<i64> {
        self.fts.map(|fts| fts - self.open_uts)
    }

    pub fn pnlu(&self) -> Option<f64> {
        let close = self.cprice()?;
        match self.side {
            Side::Buy if close != 0.0 => Some((close - self.price) / close),
            Side::Sell if self.price != 0.0 => Some((self.price - close) / self.price),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FuturesOrder {
    pub client_order_id: i64,
    pub main_fkey: Option<i64>,
    pub symbol: String,
    pub side: Side,
    pub create_ts_us: i64,
    pub update_ts_us: i64,
    pub source_ts_us: i64,
    pub amount: f64,
    pub cprice: Option<f64>,
    pub event_count: i64,
}

#[derive(Clone, Debug)]
pub enum MatchEvent {
    Margin(MarginOrder),
    Futures(FuturesOrder),
}

impl MatchEvent {
    fn sort_key(&self) -> (i64, i64, u8, i64) {
        match self {
            Self::Margin(order) => (order.open_uts, order.open_source_ts_us, 0, order.fkey),
            Self::Futures(order) => (
                order.create_ts_us,
                order.source_ts_us,
                1,
                order.client_order_id,
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HedgeResult {
    pub order: FuturesOrder,
    pub allocated_amount: f64,
    pub unallocated_amount: f64,
    pub anchor_matched: bool,
}

#[derive(Debug)]
pub struct MatchEngine {
    epsilon: f64,
    orders: BTreeMap<i64, MarginOrder>,
    queues: BTreeMap<(String, Side), VecDeque<i64>>,
}

impl MatchEngine {
    pub fn new(mut pending: Vec<MarginOrder>, epsilon: f64) -> Result<Self> {
        if !epsilon.is_finite() || epsilon < 0.0 {
            bail!("matching epsilon must be finite and non-negative");
        }
        pending.sort_by_key(|order| (order.open_uts, order.open_source_ts_us, order.fkey));
        let mut engine = Self {
            epsilon,
            orders: BTreeMap::new(),
            queues: BTreeMap::new(),
        };
        for mut order in pending {
            if order.remaining_amount <= epsilon {
                bail!(
                    "non-pending order {} loaded into pending snapshot",
                    order.fkey
                );
            }
            order.matching_state = MatchingState::Pending;
            let key = (order.symbol.clone(), order.side);
            let fkey = order.fkey;
            if engine.orders.insert(fkey, order).is_some() {
                bail!("duplicate pending Margin fkey {fkey}");
            }
            engine.queues.entry(key).or_default().push_back(fkey);
        }
        Ok(engine)
    }

    pub fn apply(&mut self, mut events: Vec<MatchEvent>) -> Result<Vec<HedgeResult>> {
        events.sort_by_key(MatchEvent::sort_key);
        let mut hedges = Vec::new();
        for event in events {
            match event {
                MatchEvent::Margin(order) => self.apply_margin(order)?,
                MatchEvent::Futures(order) => hedges.push(self.apply_futures(order)?),
            }
        }
        Ok(hedges)
    }

    pub fn orders(&self) -> impl Iterator<Item = &MarginOrder> {
        self.orders.values()
    }

    pub fn into_orders(self) -> Vec<MarginOrder> {
        self.orders.into_values().collect()
    }

    fn apply_margin(&mut self, mut order: MarginOrder) -> Result<()> {
        if !order.open_fill_amount.is_finite() || order.open_fill_amount <= self.epsilon {
            bail!("Margin order {} has no real fill", order.fkey);
        }
        if self.orders.contains_key(&order.fkey) {
            bail!("duplicate Margin fkey {}", order.fkey);
        }
        order.remaining_amount = order.open_fill_amount;
        order.matching_state = MatchingState::Pending;
        let fkey = order.fkey;
        let symbol = order.symbol.clone();
        let side = order.side;
        self.orders.insert(fkey, order);

        let opposite_key = (symbol.clone(), side.opposite());
        loop {
            let Some(other_fkey) = self
                .queues
                .get(&opposite_key)
                .and_then(|queue| queue.front().copied())
            else {
                break;
            };
            let new_remaining = self.orders[&fkey].remaining_amount;
            if new_remaining <= self.epsilon {
                break;
            }
            let other_remaining = self.orders[&other_fkey].remaining_amount;
            let quantity = new_remaining.min(other_remaining);
            {
                let other = self.orders.get_mut(&other_fkey).unwrap();
                other.remaining_amount =
                    clamp_zero(other.remaining_amount - quantity, self.epsilon);
                other.netted_amount += quantity;
                refresh_state(other, self.epsilon);
            }
            {
                let current = self.orders.get_mut(&fkey).unwrap();
                current.remaining_amount =
                    clamp_zero(current.remaining_amount - quantity, self.epsilon);
                current.netted_amount += quantity;
                refresh_state(current, self.epsilon);
            }
            if self.orders[&other_fkey].remaining_amount <= self.epsilon {
                self.queues.get_mut(&opposite_key).unwrap().pop_front();
            }
        }

        if self.orders[&fkey].remaining_amount > self.epsilon {
            self.queues
                .entry((symbol, side))
                .or_default()
                .push_back(fkey);
        }
        Ok(())
    }

    fn apply_futures(&mut self, order: FuturesOrder) -> Result<HedgeResult> {
        if !order.amount.is_finite() || order.amount < -self.epsilon {
            bail!(
                "Futures order {} has invalid fill amount",
                order.client_order_id
            );
        }
        let queue_key = (order.symbol.clone(), order.side.opposite());
        let mut remaining = order.amount.max(0.0);
        let mut anchor_matched = false;
        while remaining > self.epsilon {
            let Some(fkey) = self
                .queues
                .get(&queue_key)
                .and_then(|queue| queue.front().copied())
            else {
                break;
            };
            let available = self.orders[&fkey].remaining_amount;
            let quantity = remaining.min(available);
            let margin = self.orders.get_mut(&fkey).unwrap();
            margin.remaining_amount = clamp_zero(margin.remaining_amount - quantity, self.epsilon);
            margin.camount += quantity;
            if let Some(price) = order.cprice {
                margin.close_notional += quantity * price;
            }
            margin.fts = Some(
                margin
                    .fts
                    .map_or(order.update_ts_us, |fts| fts.max(order.update_ts_us)),
            );
            margin.close_count += order.event_count;
            refresh_state(margin, self.epsilon);
            anchor_matched |= order.main_fkey == Some(fkey);
            remaining = clamp_zero(remaining - quantity, self.epsilon);
            if margin.remaining_amount <= self.epsilon {
                self.queues.get_mut(&queue_key).unwrap().pop_front();
            }
        }
        Ok(HedgeResult {
            allocated_amount: order.amount - remaining,
            unallocated_amount: remaining,
            anchor_matched,
            order,
        })
    }
}

fn clamp_zero(value: f64, epsilon: f64) -> f64 {
    if value.abs() <= epsilon { 0.0 } else { value }
}

fn refresh_state(order: &mut MarginOrder, epsilon: f64) {
    order.matching_state = if order.remaining_amount > epsilon {
        MatchingState::Pending
    } else if order.camount > epsilon && order.netted_amount > epsilon {
        MatchingState::Mixed
    } else if order.camount > epsilon {
        MatchingState::Completed
    } else {
        MatchingState::Netted
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn margin(fkey: i64, side: Side, timestamp: i64, amount: f64) -> MatchEvent {
        MatchEvent::Margin(MarginOrder {
            fkey,
            symbol: "BTCUSDT".to_string(),
            side,
            cts: timestamp - 10,
            open_uts: timestamp,
            fts: None,
            close_count: 0,
            price: 100.0,
            amount,
            range: 2.0,
            tlen: Some(1_000.0),
            open_fill_amount: amount,
            remaining_amount: amount,
            camount: 0.0,
            netted_amount: 0.0,
            close_notional: 0.0,
            matching_state: MatchingState::Pending,
            open_source_ts_us: timestamp + 1,
        })
    }

    fn futures(id: i64, side: Side, timestamp: i64, amount: f64) -> MatchEvent {
        MatchEvent::Futures(FuturesOrder {
            client_order_id: id,
            main_fkey: Some(2),
            symbol: "BTCUSDT".to_string(),
            side,
            create_ts_us: timestamp,
            update_ts_us: timestamp + 5,
            source_ts_us: timestamp + 10,
            amount,
            cprice: Some(101.0),
            event_count: 2,
        })
    }

    fn events() -> Vec<MatchEvent> {
        vec![
            futures(20, Side::Sell, 40, 2.5),
            margin(1, Side::Buy, 10, 1.0),
            margin(2, Side::Buy, 20, 2.0),
            margin(3, Side::Sell, 30, 0.5),
        ]
    }

    fn signature(engine: &MatchEngine) -> Vec<(i64, String, i64, i64, i64)> {
        engine
            .orders()
            .map(|order| {
                (
                    order.fkey,
                    order.matching_state.as_str().to_string(),
                    (order.remaining_amount * 1e8).round() as i64,
                    (order.camount * 1e8).round() as i64,
                    (order.netted_amount * 1e8).round() as i64,
                )
            })
            .collect()
    }

    #[test]
    fn released_events_are_chunk_invariant() {
        let mut one_shot = MatchEngine::new(Vec::new(), 1e-8).unwrap();
        one_shot.apply(events()).unwrap();
        let expected = signature(&one_shot);

        let mut canonical = events();
        canonical.sort_by_key(MatchEvent::sort_key);
        for size in 1..=canonical.len() {
            let mut incremental = MatchEngine::new(Vec::new(), 1e-8).unwrap();
            for chunk in canonical.chunks(size) {
                incremental.apply(chunk.to_vec()).unwrap();
            }
            assert_eq!(signature(&incremental), expected, "chunk size {size}");
        }
    }

    #[test]
    fn changing_endpoints_preserve_final_state() {
        let mut canonical = events();
        canonical.sort_by_key(MatchEvent::sort_key);
        let mut incremental = MatchEngine::new(Vec::new(), 1e-8).unwrap();
        for endpoint in 1..=canonical.len() {
            incremental
                .apply(vec![canonical[endpoint - 1].clone()])
                .unwrap();
        }

        let mut one_shot = MatchEngine::new(Vec::new(), 1e-8).unwrap();
        one_shot.apply(canonical).unwrap();
        assert_eq!(signature(&incremental), signature(&one_shot));
    }

    #[test]
    fn futures_consumes_margin_fifo_and_keeps_anchor_as_audit() {
        let mut engine = MatchEngine::new(Vec::new(), 1e-8).unwrap();
        let hedges = engine
            .apply(vec![
                margin(1, Side::Buy, 10, 1.0),
                margin(2, Side::Buy, 20, 2.0),
                futures(20, Side::Sell, 30, 2.5),
            ])
            .unwrap();
        assert!(hedges[0].anchor_matched);
        assert_eq!(hedges[0].unallocated_amount, 0.0);
        let orders = engine.orders().collect::<Vec<_>>();
        assert_eq!(orders[0].matching_state, MatchingState::Completed);
        assert_eq!(orders[1].remaining_amount, 0.5);
    }

    #[test]
    fn opposite_margin_fills_net_before_a_later_hedge() {
        let mut engine = MatchEngine::new(Vec::new(), 1e-8).unwrap();
        engine
            .apply(vec![
                margin(1, Side::Buy, 10, 1.0),
                margin(2, Side::Sell, 20, 0.25),
            ])
            .unwrap();
        let first = engine.orders().next().unwrap();
        assert_eq!(first.remaining_amount, 0.75);
        assert_eq!(first.netted_amount, 0.25);
        assert_eq!(
            engine.orders().nth(1).unwrap().matching_state,
            MatchingState::Netted
        );
    }
}
