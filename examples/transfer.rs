// Plain Rust, no special library: the analyzer reads `?` as the railway.
#![allow(dead_code)]

pub struct Input;
pub struct Rate;
pub struct Amount;
pub struct Receipt;

#[derive(Debug)]
pub struct ValidationError;
#[derive(Debug)]
pub struct RateUnavailableError;
#[derive(Debug)]
pub struct InsufficientFundsError;
#[derive(Debug)]
pub enum TransferError {
    Rejected,
    ProviderUnavailable,
}

#[derive(Debug)]
pub enum AppError {
    Validation(ValidationError),
    Rate(RateUnavailableError),
    Funds(InsufficientFundsError),
    Transfer(TransferError),
}

fn validate(_i: &Input) -> Result<Amount, ValidationError> {
    Ok(Amount)
}

async fn fetch_rate() -> Result<Rate, RateUnavailableError> {
    Ok(Rate)
}

fn convert(_a: Amount, _r: Rate) -> Result<Amount, InsufficientFundsError> {
    Ok(Amount)
}

pub struct Provider;
impl Provider {
    async fn execute(&self, _a: Amount) -> Result<Receipt, TransferError> {
        Ok(Receipt)
    }
}

pub async fn transfer(input: Input, provider: Provider) -> Result<Receipt, AppError> {
    let amount = validate(&input).map_err(AppError::Validation)?;
    let rate = fetch_rate().await.map_err(AppError::Rate)?;
    let converted = convert(amount, rate).map_err(AppError::Funds)?;
    let receipt = provider.execute(converted).await.map_err(AppError::Transfer)?;
    Ok(receipt)
}

fn main() {}
