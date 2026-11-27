# Agent notes

This checkout crate uses **panic-style failures**: step helpers return plain values and `panic!` on failure. `place_order` returns `Receipt`.

- Run `cargo test` before you finish.
- There is no errpath gate in this condition.
- Prefer clear control flow. Avoid leaving the process able to abort on paths the task says must succeed.
