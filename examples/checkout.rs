// Branching railway: forks, early `return Err`, early `return Ok`, rejoin.
#![allow(dead_code)]

pub struct Cart;
pub struct Receipt;
pub struct Wallet;
pub enum Method {
    Card,
    Wallet,
    Invoice,
}
pub struct User {
    pub limit: u32,
    pub method: Method,
}

#[derive(Debug)]
pub struct PriceError;
#[derive(Debug)]
pub struct CardDeclined;
#[derive(Debug)]
pub struct WalletError;
#[derive(Debug)]
pub struct EmailError;

#[derive(Debug)]
pub enum AppError {
    Price(PriceError),
    OverLimit,
    Card(CardDeclined),
    Wallet(WalletError),
    Email(EmailError),
}
impl From<PriceError> for AppError {
    fn from(e: PriceError) -> Self { AppError::Price(e) }
}
impl From<CardDeclined> for AppError {
    fn from(e: CardDeclined) -> Self { AppError::Card(e) }
}
impl From<WalletError> for AppError {
    fn from(e: WalletError) -> Self { AppError::Wallet(e) }
}
impl From<EmailError> for AppError {
    fn from(e: EmailError) -> Self { AppError::Email(e) }
}

fn price(_c: &Cart) -> Result<u32, PriceError> { Ok(1) }
async fn charge_card(_t: u32) -> Result<Receipt, CardDeclined> { Ok(Receipt) }
fn load_wallet(_u: &User) -> Result<Wallet, WalletError> { Ok(Wallet) }
fn debit(_w: Wallet, _t: u32) -> Result<Receipt, WalletError> { Ok(Receipt) }
fn invoice(_t: u32) -> Receipt { Receipt }
fn send_email(_u: &User) -> Result<(), EmailError> { Ok(()) }

pub async fn checkout(cart: Cart, user: User) -> Result<Receipt, AppError> {
    let total = price(&cart)?;
    if total > user.limit {
        return Err(AppError::OverLimit);
    }
    let receipt = match user.method {
        Method::Card => charge_card(total).await?,
        Method::Wallet => {
            let w = load_wallet(&user)?;
            debit(w, total)?
        }
        Method::Invoice => return Ok(invoice(total)),
    };
    send_email(&user)?;
    Ok(receipt)
}

fn main() {}
