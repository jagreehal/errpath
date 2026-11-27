use checkout::{configure, place_order, reset, Card, Cart, Failures};
use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn happy_path() {
    reset();
    let receipt = place_order(&Cart { has_coupon: false }, &Card);
    assert_eq!(receipt.total, 42);
}

#[test]
fn price_failure_panics() {
    configure(Failures {
        price: true,
        ..Failures::default()
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        place_order(&Cart { has_coupon: false }, &Card)
    }));
    assert!(result.is_err(), "price failure should panic");
}
