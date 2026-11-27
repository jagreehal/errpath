//! Result-railway checkout: fallible steps return typed Results.
//! Agents edit this crate; held-out tests and errpath review grade the result.

mod auth;
mod order;

pub use auth::{authorize, Action, Role};
pub use order::{
    check_fraud, place_order, Card, CardDeclined, Cart, CouponError, DbError, EmailError,
    FraudError, OrderError, PriceError, Receipt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Failures {
    pub price: bool,
    pub coupon: bool,
    pub charge: bool,
    pub save: bool,
    pub email: bool,
    pub fraud: bool,
}

thread_local! {
    static FAILURES: std::cell::RefCell<Failures> = const { std::cell::RefCell::new(Failures {
        price: false,
        coupon: false,
        charge: false,
        save: false,
        email: false,
        fraud: false,
    }) };
}

pub fn configure(failures: Failures) {
    FAILURES.with(|f| *f.borrow_mut() = failures);
}

pub fn reset() {
    configure(Failures::default());
}

pub(crate) fn should_fail(which: fn(&Failures) -> bool) -> bool {
    FAILURES.with(|f| which(&f.borrow()))
}
