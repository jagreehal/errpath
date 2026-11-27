# Agent notes

This checkout crate uses **typed `Result` error railways** (`place_order -> Result<Receipt, OrderError>`, step functions return `Result`).

- Prefer `?` and explicit `match` / `if let Err` over `unwrap` / `expect` / `let _ =`.
- Run `cargo test` before you finish.
- There is no errpath gate in this condition.
