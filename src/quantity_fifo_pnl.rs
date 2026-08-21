//! Quantity-based FIFO PnL matching for intra and funding-rate spot/futures.
//! Open lots are matched in base-asset units so price differences cannot
//! create a synthetic residual position.
use crate::fifo_pnl::{FifoPnlError, Side};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Lot {
    entry_price: f64,
    quantity: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuantityPnlSnapshot {
    pub gross_realized_pnl: f64,
    pub cumulative_fees: f64,
    pub realized_pnl: f64,
    pub floating_pnl: f64,
    pub total_pnl: f64,
}

#[derive(Clone, Debug, Default)]
pub struct QuantityFifoPnl {
    longs: VecDeque<Lot>,
    shorts: VecDeque<Lot>,
    gross_realized_pnl: f64,
    cumulative_fees: f64,
}

impl QuantityFifoPnl {
    pub fn apply_fill(
        &mut self,
        side: Side,
        price: f64,
        quantity: f64,
        fee: f64,
    ) -> Result<(), FifoPnlError> {
        validate_positive("price", price)?;
        validate_positive("quantity", quantity)?;
        if !fee.is_finite() {
            return Err(FifoPnlError::InvalidFee(fee));
        }

        let (realized_pnl, remaining_quantity) = match side {
            Side::Buy => close_fifo(&mut self.shorts, price, quantity, -1.0),
            Side::Sell => close_fifo(&mut self.longs, price, quantity, 1.0),
        };
        if remaining_quantity > 0.0 {
            let lot = Lot {
                entry_price: price,
                quantity: remaining_quantity,
            };
            match side {
                Side::Buy => self.longs.push_back(lot),
                Side::Sell => self.shorts.push_back(lot),
            }
        }

        self.gross_realized_pnl += realized_pnl;
        self.cumulative_fees += fee;
        Ok(())
    }

    pub fn snapshot(&self, bid: f64, ask: f64) -> Result<QuantityPnlSnapshot, FifoPnlError> {
        validate_positive("bid", bid)?;
        validate_positive("ask", ask)?;

        let floating_pnl = self
            .longs
            .iter()
            .map(|lot| (ask - lot.entry_price) * lot.quantity)
            .chain(
                self.shorts
                    .iter()
                    .map(|lot| (lot.entry_price - bid) * lot.quantity),
            )
            .sum::<f64>();
        let realized_pnl = self.gross_realized_pnl - self.cumulative_fees;

        Ok(QuantityPnlSnapshot {
            gross_realized_pnl: self.gross_realized_pnl,
            cumulative_fees: self.cumulative_fees,
            realized_pnl,
            floating_pnl,
            total_pnl: realized_pnl + floating_pnl,
        })
    }
}

fn close_fifo(
    lots: &mut VecDeque<Lot>,
    close_price: f64,
    mut quantity: f64,
    direction: f64,
) -> (f64, f64) {
    let mut realized_pnl = 0.0;

    while quantity > 0.0 {
        let Some(lot) = lots.front_mut() else {
            break;
        };
        let matched_quantity = quantity.min(lot.quantity);
        realized_pnl += direction * (close_price - lot.entry_price) * matched_quantity;
        quantity -= matched_quantity;
        lot.quantity -= matched_quantity;

        if lot.quantity == 0.0 {
            lots.pop_front();
        }
    }

    (realized_pnl, quantity)
}

fn validate_positive(field: &'static str, value: f64) -> Result<(), FifoPnlError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(FifoPnlError::InvalidPositiveValue { field, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "actual={actual}, expected={expected}"
        );
    }

    #[test]
    fn equal_quantity_at_different_notionals_closes_without_a_residual_lot() {
        let mut pnl = QuantityFifoPnl::default();
        pnl.apply_fill(Side::Buy, 1.0, 100.0, 0.0).unwrap();
        pnl.apply_fill(Side::Sell, 2.0, 100.0, 0.0).unwrap();

        let snapshot = pnl.snapshot(3.0, 3.0).unwrap();

        assert_close(snapshot.gross_realized_pnl, 100.0);
        assert_close(snapshot.floating_pnl, 0.0);
        assert_close(snapshot.total_pnl, 100.0);
        assert!(pnl.longs.is_empty());
        assert!(pnl.shorts.is_empty());
    }

    #[test]
    fn closes_oldest_quantity_first() {
        let mut pnl = QuantityFifoPnl::default();
        pnl.apply_fill(Side::Buy, 100.0, 10.0, 0.0).unwrap();
        pnl.apply_fill(Side::Buy, 110.0, 20.0, 0.0).unwrap();
        pnl.apply_fill(Side::Sell, 120.0, 15.0, 0.0).unwrap();

        let snapshot = pnl.snapshot(115.0, 115.0).unwrap();

        assert_close(snapshot.gross_realized_pnl, 250.0);
        assert_close(snapshot.floating_pnl, 75.0);
        assert_close(snapshot.total_pnl, 325.0);
        assert_eq!(pnl.longs.len(), 1);
        assert_close(pnl.longs.front().unwrap().quantity, 15.0);
    }

    #[test]
    fn subtracts_fees_from_realized_and_total_pnl() {
        let mut pnl = QuantityFifoPnl::default();
        pnl.apply_fill(Side::Buy, 100.0, 10.0, 2.0).unwrap();
        pnl.apply_fill(Side::Sell, 110.0, 4.0, 1.0).unwrap();

        let snapshot = pnl.snapshot(104.0, 106.0).unwrap();

        assert_close(snapshot.gross_realized_pnl, 40.0);
        assert_close(snapshot.cumulative_fees, 3.0);
        assert_close(snapshot.realized_pnl, 37.0);
        assert_close(snapshot.floating_pnl, 36.0);
        assert_close(snapshot.total_pnl, 73.0);
    }
}
