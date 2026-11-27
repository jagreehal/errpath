use checkout::{Card, Cart, Failures, OrderError, configure, place_order};

#[test]
fn email_failure_still_returns_ok() {
    configure(Failures {
        email: true,
        ..Failures::default()
    });
    let receipt = place_order(&Cart { has_coupon: false }, &Card)
        .expect("paid order must succeed when email fails");
    assert_eq!(receipt.total, 42);
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

#[test]
fn save_failure_still_propagates() {
    configure(Failures {
        save: true,
        ..Failures::default()
    });
    let err = place_order(&Cart { has_coupon: false }, &Card).unwrap_err();
    assert!(matches!(err, OrderError::Db(_)));
}
