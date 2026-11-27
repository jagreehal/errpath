use checkout::{Card, Cart, Failures, configure, place_order, reset};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn fraud_failure_aborts_before_charge() {
    configure(Failures {
        fraud: true,
        ..Failures::default()
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        place_order(&Cart { has_coupon: false }, &Card)
    }));
    assert!(result.is_err(), "fraud failure must abort");
}

#[test]
fn happy_path_still_works_with_fraud_wired() {
    reset();
    let receipt = place_order(&Cart { has_coupon: false }, &Card);
    assert_eq!(receipt.total, 42);
}
