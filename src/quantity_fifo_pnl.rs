//! Quantity-based FIFO PnL matching for intra and funding-rate spot/futures.
//! Open lots are matched in base-asset units so price differences cannot
//! create a synthetic residual position.
use crate::fifo_pnl::{FifoPnlError, Side};
use crate::fifo_valuation::Inventory;
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
    long_inventory: Inventory,
    short_inventory: Inventory,
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
            Side::Buy => close_fifo(
                &mut self.shorts,
                &mut self.short_inventory,
                price,
                quantity,
                -1.0,
            ),
            Side::Sell => close_fifo(
                &mut self.longs,
                &mut self.long_inventory,
                price,
                quantity,
                1.0,
            ),
        };
        if remaining_quantity > 0.0 {
            let lot = Lot {
                entry_price: price,
                quantity: remaining_quantity,
            };
            match side {
                Side::Buy => {
                    self.longs.push_back(lot);
                    self.long_inventory.quantity(price, remaining_quantity);
                }
                Side::Sell => {
                    self.shorts.push_back(lot);
                    self.short_inventory.quantity(price, remaining_quantity);
                }
            }
        }

        self.gross_realized_pnl += realized_pnl;
        self.cumulative_fees += fee;
        Ok(())
    }

    pub fn snapshot(&self, bid: f64, ask: f64) -> Result<QuantityPnlSnapshot, FifoPnlError> {
        validate_positive("bid", bid)?;
        validate_positive("ask", ask)?;

        let floating_pnl = self.long_inventory.floating(ask) - self.short_inventory.floating(bid);
        #[cfg(test)]
        let floating_pnl = if crate::fifo_valuation::reference_enabled() {
            self.longs
                .iter()
                .map(|l| (ask - l.entry_price) * l.quantity)
                .chain(
                    self.shorts
                        .iter()
                        .map(|l| (l.entry_price - bid) * l.quantity),
                )
                .sum()
        } else {
            floating_pnl
        };
        let realized_pnl = self.gross_realized_pnl - self.cumulative_fees;

        Ok(QuantityPnlSnapshot {
            gross_realized_pnl: self.gross_realized_pnl,
            cumulative_fees: self.cumulative_fees,
            realized_pnl,
            floating_pnl,
            total_pnl: realized_pnl + floating_pnl,
        })
    }

    pub(crate) fn allocated_bytes(&self) -> usize {
        (self.longs.capacity() + self.shorts.capacity()) * std::mem::size_of::<Lot>()
    }
}

fn close_fifo(
    lots: &mut VecDeque<Lot>,
    inventory: &mut Inventory,
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
        inventory.quantity(lot.entry_price, -matched_quantity);
        quantity -= matched_quantity;
        lot.quantity -= matched_quantity;

        if lot.quantity == 0.0 {
            lots.pop_front();
        }
    }

    if lots.is_empty() {
        inventory.clear();
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

    fn reference_floating(pnl: &QuantityFifoPnl, bid: f64, ask: f64) -> f64 {
        pnl.longs
            .iter()
            .map(|l| (ask - l.entry_price) * l.quantity)
            .chain(
                pnl.shorts
                    .iter()
                    .map(|l| (l.entry_price - bid) * l.quantity),
            )
            .sum()
    }

    #[test]
    fn aggregate_marks_match_lot_scan_through_partial_closes_and_reversals() {
        let mut pnl = QuantityFifoPnl::default();
        let mut seed = 919_u64;
        for index in 0..20_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let side = if index % 127 < 90 {
                Side::Buy
            } else {
                Side::Sell
            };
            let price = 100.0 + (seed % 10_000) as f64 / 1000.0;
            let quantity = (1 + (seed >> 32) % 100) as f64 / 17.0;
            pnl.apply_fill(side, price, quantity, -0.0001).unwrap();
            let bid = 104.0 + (index % 17) as f64 / 13.0;
            let expected = reference_floating(&pnl, bid, bid + 0.01);
            let actual = pnl.snapshot(bid, bid + 0.01).unwrap().floating_pnl;
            assert!(
                (actual - expected).abs() <= 1e-8 + expected.abs() * 1e-11,
                "index={index}, actual={actual}, expected={expected}"
            );
        }
        for side in [Side::Sell, Side::Buy] {
            pnl.apply_fill(side, 105.0, 1_000_000.0, 0.0).unwrap();
            let expected = reference_floating(&pnl, 104.0, 106.0);
            assert!((pnl.snapshot(104.0, 106.0).unwrap().floating_pnl - expected).abs() < 1e-8);
        }
    }

    #[test]
    fn large_prices_small_pnl_and_flat_inventory_do_not_accumulate_cost_cancellation() {
        let mut pnl = QuantityFifoPnl::default();
        for index in 0..10_000 {
            pnl.apply_fill(Side::Buy, 1e9 + (index % 10) as f64 / 100.0, 0.0003, 0.0)
                .unwrap();
        }
        let expected = reference_floating(&pnl, 1e9 + 0.04, 1e9 + 0.06);
        assert!(
            (pnl.snapshot(1e9 + 0.04, 1e9 + 0.06).unwrap().floating_pnl - expected).abs() < 1e-10
        );
        let quantities = pnl.longs.iter().map(|l| l.quantity).collect::<Vec<_>>();
        for quantity in quantities {
            pnl.apply_fill(Side::Sell, 1e9, quantity, 0.0).unwrap();
        }
        assert!(pnl.longs.is_empty() && pnl.shorts.is_empty());
        assert_eq!(pnl.snapshot(1e9, 1e9).unwrap().floating_pnl, 0.0);
    }

    #[test]
    #[ignore = "release-mode performance measurement"]
    fn benchmark_growing_inventory_marking() {
        use std::{hint::black_box, time::Instant};
        for count in [10_000, 20_000] {
            let mut pnl = QuantityFifoPnl::default();
            let start = Instant::now();
            for index in 0..count {
                pnl.apply_fill(Side::Buy, 100.0 + (index % 17) as f64 * 0.01, 0.001, 0.0)
                    .unwrap();
                black_box(pnl.snapshot(100.1, 100.2).unwrap());
            }
            let optimized = start.elapsed();
            let mut reference = QuantityFifoPnl::default();
            let start = Instant::now();
            for index in 0..count {
                reference
                    .apply_fill(Side::Buy, 100.0 + (index % 17) as f64 * 0.01, 0.001, 0.0)
                    .unwrap();
                black_box(reference_floating(&reference, 100.1, 100.2));
            }
            println!(
                "lots={count} optimized_us={} reference_us={}",
                optimized.as_micros(),
                start.elapsed().as_micros()
            );
        }
    }
}
