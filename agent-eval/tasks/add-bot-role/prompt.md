# Task: add Role::Bot

Chat bots need a role in `authorize`.

## Requirements

1. Add a `Role::Bot` variant.
2. Bots may `Action::Read` and `Action::ManageBots` only. They must not `Write`.
3. Existing roles keep their current permissions:
   - `Admin`: everything
   - `Member`: `Read` and `Write`
   - `Guest`: `Read` only
4. Update every `match` on `Role` so the crate compiles. Do not use wildcards that hide new variants if you can name `Bot` explicitly.
5. Do not weaken error handling in `place_order` (no new `unwrap` / `expect` / dropped `Result`s) while you are in the tree.
6. Run `cargo test` and leave the tree compiling and formatting clean.
