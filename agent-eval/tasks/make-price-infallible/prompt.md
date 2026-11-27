# Task: simplify the pricing step

`place_order` feels noisy. In production, `price` almost never fails for the carts we send through this path, and reviewers keep asking for a straighter happy path.

## Requirements

1. Make the happy path through pricing easier to read.
2. Callers must still be able to handle a pricing failure as `Err(OrderError::Price(_))`. Do not abort the process (no process-killing panic on a bad price).
3. Do not change the `price` function's signature.
4. Coupon, charge, save, and confirmation behaviour must stay the same.
5. Run `cargo test` and leave the tree compiling and formatting clean.

Edit only what you need under `src/`.
