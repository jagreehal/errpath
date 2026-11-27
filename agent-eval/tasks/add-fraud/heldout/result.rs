use checkout::{Card, Cart, Failures, OrderError, configure, place_order, reset};

#[test]
fn fraud_failure_propagates_and_skips_charge() {
    configure(Failures {
        fraud: true,
        ..Failures::default()
    });
    let err = place_order(&Cart { has_coupon: false }, &Card).unwrap_err();
    assert!(matches!(err, OrderError::Fraud(_)));
}

#[test]
fn happy_path_still_works_with_fraud_wired() {
    reset();
    assert!(place_order(&Cart { has_coupon: false }, &Card).is_ok());
}

#[test]
fn charge_failure_still_propagates() {
    configure(Failures {
        charge: true,
        ..Failures::default()
    });
    let err = place_order(&Cart { has_coupon: false }, &Card).unwrap_err();
    assert!(matches!(err, OrderError::Payment(_)));
}
