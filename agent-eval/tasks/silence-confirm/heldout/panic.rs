use checkout::{Card, Cart, Failures, configure, place_order};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn email_failure_still_returns_receipt() {
    configure(Failures {
        email: true,
        ..Failures::default()
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        place_order(&Cart { has_coupon: false }, &Card)
    }));
    let receipt = result.expect("paid order must not panic when email fails");
    assert_eq!(receipt.total, 42);
}

#[test]
fn charge_failure_still_aborts() {
    configure(Failures {
        charge: true,
        ..Failures::default()
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        place_order(&Cart { has_coupon: false }, &Card)
    }));
    assert!(result.is_err(), "charge failure must still abort");
}
