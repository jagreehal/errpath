//! Panic-style checkout: failures abort the process instead of returning Result.
//! Used to compare against the Result-railway fixture in the agent eval.

mod auth;
mod order;

pub use auth::{authorize, Action, Role};
pub use order::{check_fraud, place_order, Card, Cart, Receipt};

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
