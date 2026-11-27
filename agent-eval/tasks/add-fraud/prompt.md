# Task: add a fraud check before charging

`check_fraud` already exists in this crate but is not called from `place_order`.

## Requirements

1. Call `check_fraud(cart)` after pricing (and coupon validation when applicable) and **before** `charge`.
2. If fraud fails, the order must not charge, save, or send email. The failure must surface to the caller the same way other step failures do in this crate.
3. Happy path and existing failure modes (price, charge, etc.) must keep working.
4. Run `cargo test` and leave the tree compiling.

Edit only what you need under `src/`.
