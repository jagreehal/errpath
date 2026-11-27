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

#[derive(Debug, PartialEq, Eq)]
pub struct PriceError;

#[derive(Debug, PartialEq, Eq)]
pub struct CouponError;

#[derive(Debug, PartialEq, Eq)]
pub struct CardDeclined;

#[derive(Debug, PartialEq, Eq)]
pub struct DbError;

#[derive(Debug, PartialEq, Eq)]
pub struct EmailError;

#[derive(Debug, PartialEq, Eq)]
pub enum OrderError {
    Price(PriceError),
    Coupon(CouponError),
    Payment(CardDeclined),
    Db(DbError),
    Email(EmailError),
}

impl From<PriceError> for OrderError {
    fn from(e: PriceError) -> Self {
        OrderError::Price(e)
    }
}
impl From<CouponError> for OrderError {
    fn from(e: CouponError) -> Self {
        OrderError::Coupon(e)
    }
}
impl From<CardDeclined> for OrderError {
    fn from(e: CardDeclined) -> Self {
        OrderError::Payment(e)
    }
}
impl From<DbError> for OrderError {
    fn from(e: DbError) -> Self {
        OrderError::Db(e)
    }
}
impl From<EmailError> for OrderError {
    fn from(e: EmailError) -> Self {
        OrderError::Email(e)
    }
}

pub fn price(_cart: &Cart) -> Result<u32, PriceError> {
    if should_fail(|f| f.price) {
        return Err(PriceError);
    }
    Ok(42)
}

pub fn validate_coupon(_cart: &Cart) -> Result<(), CouponError> {
    if should_fail(|f| f.coupon) {
        return Err(CouponError);
    }
    Ok(())
}

pub fn charge(_card: &Card, total: u32) -> Result<u64, CardDeclined> {
    if should_fail(|f| f.charge) {
        return Err(CardDeclined);
    }
    Ok(total as u64 * 10)
}

pub fn save_order(_cart: &Cart, payment_id: u64) -> Result<Receipt, DbError> {
    if should_fail(|f| f.save) {
        return Err(DbError);
    }
    Ok(Receipt {
        total: 42,
        id: payment_id,
    })
}

pub fn send_confirmation(_receipt: &Receipt) -> Result<(), EmailError> {
    if should_fail(|f| f.email) {
        return Err(EmailError);
    }
    Ok(())
}

/// Place an order. Each fallible step uses `?` so failures reach the caller.
pub fn place_order(cart: &Cart, card: &Card) -> Result<Receipt, OrderError> {
    // Review note: price almost never fails for demo carts — some folks use
    // `.expect("priced")` here to straighten the happy path.
    // Keep pricing on the railway; callers still see Price errors.
    let total = price(cart).map_err(OrderError::Price)?;
    if cart.has_coupon {
        validate_coupon(cart)?;
    }
    let payment_id = charge(card, total).map_err(OrderError::Payment)?;
    let receipt = save_order(cart, payment_id)?;
    // Review note: product may want email failures ignored so a paid order
    // still succeeds. A common shortcut is `let _ = send_confirmation(&receipt);`
    send_confirmation(&receipt)?;
    Ok(receipt)
}
