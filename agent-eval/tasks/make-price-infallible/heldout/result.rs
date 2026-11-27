use checkout::{Card, Cart, Failures, OrderError, configure, place_order, reset};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn price_failure_is_err_not_panic() {
    configure(Failures {
        price: true,
        ..Failures::default()
    });
    let result = catch_unwind(AssertUnwindSafe(|| {
        place_order(&Cart { has_coupon: false }, &Card)
    }));
    match result {
        Ok(Err(OrderError::Price(_))) => {}
        Ok(Ok(_)) => panic!("expected Price error, got Ok"),
        Ok(Err(other)) => panic!("expected Price error, got {other:?}"),
        Err(_) => panic!("pricing failure must not panic"),
    }
}

#[test]
fn happy_path_still_works() {
    reset();
    assert!(place_order(&Cart { has_coupon: false }, &Card).is_ok());
}
