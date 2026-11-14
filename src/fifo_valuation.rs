//! Constant-time marking of the unchanged FIFO lots. A price origin avoids
//! subtracting two large absolute notionals to recover a small floating PnL.

#[derive(Clone, Debug, Default)]
struct Sum {
    value: f64,
    correction: f64,
}

impl Sum {
    fn add(&mut self, value: f64) {
        let next = self.value + value;
        self.correction += if self.value.abs() >= value.abs() {
            (self.value - next) + value
        } else {
            (value - next) + self.value
        };
        self.value = next;
    }

    fn total(&self) -> f64 {
        self.value + self.correction
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Inventory {
    origin: Option<f64>,
    units: Sum,
    offset: Sum,
}

impl Inventory {
    pub(crate) fn quantity(&mut self, price: f64, quantity: f64) {
        let origin = *self.origin.get_or_insert(price);
        self.units.add(quantity);
        self.offset.add((price - origin) * quantity);
    }

    pub(crate) fn notional(&mut self, price: f64, amount: f64) {
        let origin = *self.origin.get_or_insert(price);
        self.units.add(amount / price);
        self.offset.add((price - origin) / price * amount);
    }

    pub(crate) fn floating(&self, mark: f64) -> f64 {
        self.origin.map_or(0.0, |origin| {
            (mark - origin) * self.units.total() - self.offset.total()
        })
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
thread_local! { static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(crate) fn reference_enabled() -> bool {
    REFERENCE.with(|v| v.get())
}
#[cfg(test)]
pub(crate) fn with_reference<T>(work: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            REFERENCE.with(|v| v.set(self.0));
        }
    }
    let _reset = Reset(REFERENCE.with(|v| v.replace(true)));
    work()
}
