//! Visible smoke tests. Held-out task tests are applied by the grader after the agent finishes.
use checkout::{configure, place_order, reset, Card, Cart, Failures, OrderError};

#[test]
fn happy_path() {
    reset();
    let receipt = place_order(&Cart { has_coupon: false }, &Card).unwrap();
    assert_eq!(receipt.total, 42);
}

#[test]
fn price_failure_propagates() {
    configure(Failures {
        price: true,
        ..Failures::default()
    });
    let err = place_order(&Cart { has_coupon: false }, &Card).unwrap_err();
    assert!(matches!(err, OrderError::Price(_)));
}
