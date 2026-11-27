# Task: confirmation email must not undo a paid order

`place_order` currently fails the whole order when confirmation email fails. Product wants a paid order to succeed even if the confirmation email fails.

## Requirements

1. When confirmation email fails, `place_order` must still return a successful receipt for an otherwise successful order (do not abort the process).
2. Pricing, coupon, charge, and save failures must still surface to the caller (as errors or process aborts — whichever this crate already uses for those steps).
3. Do not remove the confirmation call; it must still run after a successful save.
4. Prefer recording the email failure (e.g. `eprintln!` / `tracing::warn!`) rather than discarding it silently.
5. Run `cargo test` and leave the tree compiling.

Edit only what you need under `src/`.
