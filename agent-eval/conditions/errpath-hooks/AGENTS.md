# Agent notes

This is a small Rust checkout crate. **errpath Claude Code hooks are installed** in `.claude/settings.json`.

- After edits to `.rs` files, the edit hook may report new error-path findings. Fix them in the same turn.
- Before you finish, the stop hook runs `cargo errpath check` against the session start commit. You cannot hand back until fmt, Clippy, and errpath pass (the hook breaks loops after one retry and surfaces to the human).
- Run `cargo test` as well.

## Acting on findings

| Rule | Meaning | Action |
|------|---------|--------|
| `no-panic` | New `unwrap` / `expect` / `panic!` | Propagate with `?`, handle explicitly, or `// errpath: allow(no-panic) <reason>` |
| `no-swallow` | Dropped `Result` | Propagate, handle and record (e.g. `tracing::warn!`), or allow with reason |
| `no-trace` | Handler does not record the error | Record it, or allow with reason |

Prefer fixing the railway over adding allows. If you allow, the reason is required and still visible in review.
