//! Defer FR closing PnL while an event has left part of a hedge unbalanced.
use super::{PositionLeg, Side};
use std::collections::VecDeque;

#[derive(Clone, Debug, Default)]
struct LegInventory {
    side: Option<Side>,
    quantity: f64,
}

#[derive(Clone, Debug)]
struct PendingClose {
    quantity: f64,
    pnl_per_unit: f64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct PairedClosings {
    spot: LegInventory,
    futures: LegInventory,
    pending_leg: Option<PositionLeg>,
    pending: VecDeque<PendingClose>,
    pub(super) pending_profit: f64,
}

impl PairedClosings {
    fn inventory(&mut self, leg: PositionLeg) -> &mut LegInventory {
        match leg {
            PositionLeg::Spot => &mut self.spot,
            PositionLeg::Futures => &mut self.futures,
        }
    }

    fn hedged_quantity(&self) -> f64 {
        match (self.spot.side, self.futures.side) {
            (Some(spot), Some(futures)) if spot != futures => {
                self.spot.quantity.min(self.futures.quantity)
            }
            _ => 0.0,
        }
    }

    pub(super) fn open(&mut self, leg: PositionLeg, side: Side, quantity: f64) {
        if quantity == 0.0 {
            return;
        }
        let hedged_before = self.hedged_quantity();
        let inventory = self.inventory(leg);
        if inventory.side == Some(side) {
            inventory.quantity += quantity;
        } else {
            inventory.side = Some(side);
            inventory.quantity = quantity;
        }
        if self.pending_leg == Some(leg) {
            // Reopening the first leg can restore the hedge before the other
            // leg closes. Release those closes once the imbalance is gone.
            self.release((self.hedged_quantity() - hedged_before).max(0.0));
        }
    }

    pub(super) fn close(&mut self, leg: PositionLeg, quantity: f64, pnl_per_unit: f64) {
        let hedged_before = self.hedged_quantity();
        let unmatched = if self.pending_leg.is_some() && self.pending_leg != Some(leg) {
            self.release(quantity)
        } else {
            quantity
        };
        let inventory = self.inventory(leg);
        inventory.quantity = (inventory.quantity - quantity).max(0.0);
        // A close that only trims excess inventory leaves the hedge intact.
        let deferred = unmatched.min((hedged_before - self.hedged_quantity()).max(0.0));
        if deferred > 0.0 {
            self.pending_leg = Some(leg);
            self.pending_profit += deferred * pnl_per_unit;
            self.pending.push_back(PendingClose {
                quantity: deferred,
                pnl_per_unit,
            });
        }
    }

    fn release(&mut self, mut quantity: f64) -> f64 {
        while quantity > 0.0 {
            let Some(pending) = self.pending.front_mut() else {
                break;
            };
            let matched = pending.quantity.min(quantity);
            self.pending_profit -= matched * pending.pnl_per_unit;
            pending.quantity -= matched;
            quantity -= matched;
            if pending.quantity == 0.0 {
                self.pending.pop_front();
            }
        }
        if self.pending.is_empty() {
            self.pending_leg = None;
            self.pending_profit = 0.0;
        }
        quantity
    }

    pub(super) fn allocated_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.pending.capacity() * std::mem::size_of::<PendingClose>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PositionLeg::{Futures, Spot};

    #[test]
    fn restoring_the_hedge_releases_prior_pnl_without_waiting_for_a_full_exit() {
        let mut closes = PairedClosings::default();
        closes.open(Spot, Side::Buy, 12.0);
        closes.open(Futures, Side::Sell, 10.0);
        closes.close(Spot, 2.0, -30.0);
        assert_eq!(closes.pending_profit, 0.0);

        closes.close(Spot, 3.0, -40.0);
        assert_eq!(closes.pending_profit, -120.0);
        closes.open(Futures, Side::Sell, 1.0);
        assert_eq!(closes.pending_profit, -120.0);
        closes.open(Spot, Side::Buy, 2.0);
        assert_eq!(closes.pending_profit, -40.0);
        closes.close(Futures, 1.0, 41.0);
        assert_eq!(closes.pending_profit, 0.0);

        // A separate excess contract remains after that hedge has settled.
        closes.close(Futures, 1.0, 42.0);
        assert_eq!(closes.pending_profit, 0.0);
    }
}
