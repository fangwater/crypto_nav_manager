//! Daily realized attribution for FR hedges, using the actual venue FIFO closes.
use super::{PositionLeg, Side};
use std::collections::VecDeque;

#[derive(Clone, Debug, Default)]
struct LegInventory {
    side: Option<Side>,
    paired: f64,
    unpaired: f64,
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
    pub(super) realized: f64,
}

impl PairedClosings {
    fn inventory(&mut self, leg: PositionLeg) -> &mut LegInventory {
        match leg {
            PositionLeg::Spot => &mut self.spot,
            PositionLeg::Futures => &mut self.futures,
        }
    }

    pub(super) fn open(&mut self, leg: PositionLeg, side: Side, quantity: f64) {
        if quantity == 0.0 {
            return;
        }
        let inventory = self.inventory(leg);
        inventory.side = Some(side);
        inventory.unpaired += quantity;
        if self.spot.side != self.futures.side {
            // Both legs take the oldest unpaired quantity. Paired inventory
            // is therefore a FIFO prefix in each leg.
            let paired = self.spot.unpaired.min(self.futures.unpaired);
            for inventory in [&mut self.spot, &mut self.futures] {
                inventory.unpaired -= paired;
                inventory.paired += paired;
            }
        }
    }

    pub(super) fn close(&mut self, leg: PositionLeg, quantity: f64, pnl_per_unit: f64) {
        let inventory = self.inventory(leg);
        let mut paired = inventory.paired.min(quantity);
        let unpaired = quantity - paired;
        inventory.paired -= paired;
        inventory.unpaired = (inventory.unpaired - unpaired).max(0.0);
        self.realized += unpaired * pnl_per_unit;

        // Both legs close paired quantities in the same FIFO order. At most
        // one leg can be ahead, so only its unmatched realized pieces queue.
        if self.pending_leg != Some(leg) {
            while paired > 0.0 {
                let Some(pending) = self.pending.front_mut() else {
                    break;
                };
                let matched = pending.quantity.min(paired);
                self.realized += matched * (pnl_per_unit + pending.pnl_per_unit);
                pending.quantity -= matched;
                paired -= matched;
                if pending.quantity == 0.0 {
                    self.pending.pop_front();
                }
            }
        }
        if paired > 0.0 {
            self.pending_leg = Some(leg);
            self.pending.push_back(PendingClose {
                quantity: paired,
                pnl_per_unit,
            });
        }
    }

    pub(super) fn allocated_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.pending.capacity() * std::mem::size_of::<PendingClose>()
    }
}
