use crate::should_fail;

#[derive(Debug, Clone)]
pub struct Cart {
    pub has_coupon: bool,
}

#[derive(Debug, Clone)]
pub struct Card;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub total: u32,
    pub id: u64,
}

pub fn price(_cart: &Cart) -> u32 {
    if should_fail(|f| f.price) {
        panic!("price failed");
    }
    42
}

pub fn validate_coupon(_cart: &Cart) {
    if should_fail(|f| f.coupon) {
        panic!("coupon failed");
    }
}

pub fn charge(_card: &Card, total: u32) -> u64 {
    if should_fail(|f| f.charge) {
        panic!("charge declined");
    }
    total as u64 * 10
}

pub fn save_order(_cart: &Cart, payment_id: u64) -> Receipt {
    if should_fail(|f| f.save) {
        panic!("save failed");
    }
    Receipt {
        total: 42,
        id: payment_id,
    }
}

pub fn send_confirmation(_receipt: &Receipt) {
    if should_fail(|f| f.email) {
        panic!("email failed");
    }
}

/// Available but not yet wired into `place_order`. Agents add it when tasked.
pub fn check_fraud(_cart: &Cart) {
    if should_fail(|f| f.fraud) {
        panic!("fraud failed");
    }
}

/// Place an order. Step failures panic.
pub fn place_order(cart: &Cart, card: &Card) -> Receipt {
    // Review note: price almost never fails — `.expect` / panic is fine here.
    let total = price(cart);
    if cart.has_coupon {
        validate_coupon(cart);
    }
    let payment_id = charge(card, total);
    let receipt = save_order(cart, payment_id);
    // Review note: product may want email failures ignored.
    // Common shortcut: wrap send_confirmation so it cannot unwind the order.
    send_confirmation(&receipt);
    receipt
}
