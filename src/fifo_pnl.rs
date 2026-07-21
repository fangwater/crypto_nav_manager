//! Amount-based FIFO PnL matching used by the Liang Torch notebooks.
//!
//! Each open lot stores Liang Torch's `amountu`, not asset quantity. Closing
//! PnL is the entry-to-exit return multiplied by the matched `amountu`.

use std::collections::VecDeque;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Lot {
    entry_price: f64,
    amount_u: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillResult {
    /// PnL returned by Liang Torch's Opentable for this fill, before fees.
    pub realized_pnl: f64,
    pub matched_amount_u: f64,
    pub opened_amount_u: f64,
    /// Cumulative realized PnL after subtracting cumulative fees.
    pub cumulative_realized_pnl: f64,
    pub net_open_amount_u: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PnlSnapshot {
    pub gross_realized_pnl: f64,
    pub cumulative_fees: f64,
    pub realized_pnl: f64,
    pub floating_pnl: f64,
    pub total_pnl: f64,
    pub long_amount_u: f64,
    pub short_amount_u: f64,
    pub net_open_amount_u: f64,
}

#[derive(Debug, Error, PartialEq)]
pub enum FifoPnlError {
    #[error("{field} must be finite and greater than zero, got {value}")]
    InvalidPositiveValue { field: &'static str, value: f64 },
    #[error("fee must be finite, got {0}")]
    InvalidFee(f64),
}

#[derive(Clone, Debug, Default)]
pub struct FifoPnl {
    longs: VecDeque<Lot>,
    shorts: VecDeque<Lot>,
    long_amount_u: f64,
    short_amount_u: f64,
    gross_realized_pnl: f64,
    cumulative_fees: f64,
}

impl FifoPnl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one fill in timestamp order.
    ///
    /// `amount_u` must be the notebook's `amountu` value. A buy closes the
    /// oldest short lots first; a sell closes the oldest long lots first.
    /// Positive `fee` values are costs, matching the notebook CSV convention.
    pub fn apply_fill(
        &mut self,
        side: Side,
        price: f64,
        amount_u: f64,
        fee: f64,
    ) -> Result<FillResult, FifoPnlError> {
        validate_positive("price", price)?;
        validate_positive("amount_u", amount_u)?;
        if !fee.is_finite() {
            return Err(FifoPnlError::InvalidFee(fee));
        }

        let (realized_pnl, remaining_amount_u) = match side {
            Side::Buy => close_fifo(&mut self.shorts, price, amount_u, -1.0),
            Side::Sell => close_fifo(&mut self.longs, price, amount_u, 1.0),
        };
        let matched_amount_u = amount_u - remaining_amount_u;

        match side {
            Side::Buy => self.short_amount_u -= matched_amount_u,
            Side::Sell => self.long_amount_u -= matched_amount_u,
        }
        if remaining_amount_u > 0.0 {
            let lot = Lot {
                entry_price: price,
                amount_u: remaining_amount_u,
            };
            match side {
                Side::Buy => {
                    self.longs.push_back(lot);
                    self.long_amount_u += remaining_amount_u;
                }
                Side::Sell => {
                    self.shorts.push_back(lot);
                    self.short_amount_u += remaining_amount_u;
                }
            }
        }

        self.gross_realized_pnl += realized_pnl;
        self.cumulative_fees += fee;

        Ok(FillResult {
            realized_pnl,
            matched_amount_u,
            opened_amount_u: remaining_amount_u,
            cumulative_realized_pnl: self.gross_realized_pnl - self.cumulative_fees,
            net_open_amount_u: self.net_open_amount_u(),
        })
    }

    /// Uses the same marking rule as Opentable.get_float_profit:
    /// long lots use ask, short lots use bid.
    pub fn floating_pnl(&self, bid: f64, ask: f64) -> Result<f64, FifoPnlError> {
        validate_positive("bid", bid)?;
        validate_positive("ask", ask)?;

        let long_pnl = self
            .longs
            .iter()
            .map(|lot| (ask - lot.entry_price) / lot.entry_price * lot.amount_u)
            .sum::<f64>();
        let short_pnl = self
            .shorts
            .iter()
            .map(|lot| (lot.entry_price - bid) / lot.entry_price * lot.amount_u)
            .sum::<f64>();
        Ok(long_pnl + short_pnl)
    }

    pub fn mark_pnl(&self, mark_price: f64) -> Result<f64, FifoPnlError> {
        self.floating_pnl(mark_price, mark_price)
    }

    pub fn snapshot(&self, bid: f64, ask: f64) -> Result<PnlSnapshot, FifoPnlError> {
        let floating_pnl = self.floating_pnl(bid, ask)?;
        let realized_pnl = self.gross_realized_pnl - self.cumulative_fees;
        let long_amount_u = self.long_amount_u();
        let short_amount_u = self.short_amount_u();

        Ok(PnlSnapshot {
            gross_realized_pnl: self.gross_realized_pnl,
            cumulative_fees: self.cumulative_fees,
            realized_pnl,
            floating_pnl,
            total_pnl: realized_pnl + floating_pnl,
            long_amount_u,
            short_amount_u,
            net_open_amount_u: long_amount_u - short_amount_u,
        })
    }

    pub fn long_amount_u(&self) -> f64 {
        self.long_amount_u
    }

    pub fn short_amount_u(&self) -> f64 {
        self.short_amount_u
    }

    pub fn net_open_amount_u(&self) -> f64 {
        self.long_amount_u() - self.short_amount_u()
    }

    pub fn open_lot_count(&self) -> usize {
        self.longs.len() + self.shorts.len()
    }
}

fn close_fifo(
    lots: &mut VecDeque<Lot>,
    close_price: f64,
    mut amount_u: f64,
    direction: f64,
) -> (f64, f64) {
    let mut realized_pnl = 0.0;

    while amount_u > 0.0 {
        let Some(lot) = lots.front_mut() else {
            break;
        };
        let matched = amount_u.min(lot.amount_u);
        realized_pnl += direction * (close_price - lot.entry_price) / lot.entry_price * matched;
        amount_u -= matched;
        lot.amount_u -= matched;

        if lot.amount_u == 0.0 {
            lots.pop_front();
        }
    }

    (realized_pnl, amount_u)
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

    #[derive(Clone, Copy)]
    struct ReferenceLot {
        side: Side,
        price: f64,
        amount_u: f64,
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "actual={actual}, expected={expected}"
        );
    }

    #[test]
    fn sells_close_oldest_long_amount_first() {
        let mut pnl = FifoPnl::new();
        pnl.apply_fill(Side::Buy, 100.0, 1_000.0, 0.0).unwrap();
        pnl.apply_fill(Side::Buy, 110.0, 2_200.0, 0.0).unwrap();

        let fill = pnl.apply_fill(Side::Sell, 120.0, 1_500.0, 0.0).unwrap();

        assert_close(fill.realized_pnl, 200.0 + 500.0 / 11.0);
        assert_close(fill.matched_amount_u, 1_500.0);
        assert_close(fill.opened_amount_u, 0.0);
        assert_close(pnl.long_amount_u(), 1_700.0);
        assert_eq!(pnl.open_lot_count(), 1);
        assert_close(pnl.mark_pnl(115.0).unwrap(), 850.0 / 11.0);
    }

    #[test]
    fn buys_close_oldest_short_amount_first() {
        let mut pnl = FifoPnl::new();
        pnl.apply_fill(Side::Sell, 100.0, 1_000.0, 0.0).unwrap();
        pnl.apply_fill(Side::Sell, 90.0, 1_800.0, 0.0).unwrap();

        let fill = pnl.apply_fill(Side::Buy, 80.0, 1_500.0, 0.0).unwrap();

        assert_close(fill.realized_pnl, 200.0 + 500.0 / 9.0);
        assert_close(pnl.short_amount_u(), 1_300.0);
        assert_eq!(pnl.open_lot_count(), 1);
        assert_close(pnl.mark_pnl(85.0).unwrap(), 650.0 / 9.0);
    }

    #[test]
    fn fill_can_close_and_reverse_in_one_step() {
        let mut pnl = FifoPnl::new();
        pnl.apply_fill(Side::Buy, 100.0, 1_000.0, 0.0).unwrap();

        let fill = pnl.apply_fill(Side::Sell, 110.0, 1_600.0, 0.0).unwrap();

        assert_close(fill.realized_pnl, 100.0);
        assert_close(fill.matched_amount_u, 1_000.0);
        assert_close(fill.opened_amount_u, 600.0);
        assert_close(fill.net_open_amount_u, -600.0);
        assert_close(pnl.short_amount_u(), 600.0);
    }

    #[test]
    fn snapshot_matches_notebook_realized_plus_floating_formula() {
        let mut pnl = FifoPnl::new();
        pnl.apply_fill(Side::Buy, 100.0, 1_000.0, 2.0).unwrap();
        pnl.apply_fill(Side::Sell, 110.0, 400.0, 1.0).unwrap();

        let snapshot = pnl.snapshot(104.0, 106.0).unwrap();

        assert_close(snapshot.gross_realized_pnl, 40.0);
        assert_close(snapshot.cumulative_fees, 3.0);
        assert_close(snapshot.realized_pnl, 37.0);
        assert_close(snapshot.floating_pnl, 36.0);
        assert_close(snapshot.total_pnl, 73.0);
        assert_close(snapshot.long_amount_u, 600.0);
        assert_close(snapshot.net_open_amount_u, 600.0);
    }

    #[test]
    fn rejects_invalid_external_values() {
        let mut pnl = FifoPnl::new();
        assert!(matches!(
            pnl.apply_fill(Side::Buy, 0.0, 100.0, 0.0),
            Err(FifoPnlError::InvalidPositiveValue { field: "price", .. })
        ));
        assert!(matches!(
            pnl.apply_fill(Side::Buy, 100.0, -1.0, 0.0),
            Err(FifoPnlError::InvalidPositiveValue {
                field: "amount_u",
                ..
            })
        ));
        assert!(matches!(
            pnl.apply_fill(Side::Buy, 100.0, 1.0, f64::NAN),
            Err(FifoPnlError::InvalidFee(_))
        ));
    }

    #[test]
    fn deque_engine_matches_notebook_list_scanning() {
        let fills = [
            (Side::Buy, 100.0, 1_000.0),
            (Side::Buy, 105.0, 700.0),
            (Side::Sell, 110.0, 600.0),
            (Side::Sell, 95.0, 1_500.0),
            (Side::Buy, 90.0, 800.0),
            (Side::Buy, 120.0, 900.0),
            (Side::Sell, 125.0, 500.0),
        ];
        let mut pnl = FifoPnl::new();
        let mut reference = Vec::new();

        for (index, (side, price, amount_u)) in fills.into_iter().enumerate() {
            let actual = pnl.apply_fill(side, price, amount_u, 0.0).unwrap();
            let expected = reference_apply(&mut reference, side, price, amount_u);
            assert_close(actual.realized_pnl, expected);

            let bid = 92.0 + index as f64;
            let ask = bid + 0.5;
            assert_close(
                pnl.floating_pnl(bid, ask).unwrap(),
                reference_floating(&reference, bid, ask),
            );
            assert_close(
                pnl.net_open_amount_u(),
                reference
                    .iter()
                    .map(|lot| match lot.side {
                        Side::Buy => lot.amount_u,
                        Side::Sell => -lot.amount_u,
                    })
                    .sum(),
            );
        }
    }

    fn reference_apply(lots: &mut Vec<ReferenceLot>, side: Side, price: f64, amount_u: f64) -> f64 {
        let mut remaining = amount_u;
        let mut realized = 0.0;
        let mut next = Vec::with_capacity(lots.len() + 1);

        for mut lot in lots.drain(..) {
            if lot.side == side || remaining == 0.0 {
                next.push(lot);
                continue;
            }

            let matched = remaining.min(lot.amount_u);
            realized += match lot.side {
                Side::Buy => (price - lot.price) / lot.price * matched,
                Side::Sell => (lot.price - price) / lot.price * matched,
            };
            remaining -= matched;
            lot.amount_u -= matched;
            if lot.amount_u > 0.0 {
                next.push(lot);
            }
        }

        if remaining > 0.0 {
            next.push(ReferenceLot {
                side,
                price,
                amount_u: remaining,
            });
        }
        *lots = next;
        realized
    }

    fn reference_floating(lots: &[ReferenceLot], bid: f64, ask: f64) -> f64 {
        lots.iter()
            .map(|lot| match lot.side {
                Side::Buy => (ask - lot.price) / lot.price * lot.amount_u,
                Side::Sell => (lot.price - bid) / lot.price * lot.amount_u,
            })
            .sum()
    }
}
