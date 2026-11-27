# Agent notes

This checkout crate uses **typed `Result` error railways** (`place_order -> Result<Receipt, OrderError>`).

- **Hard rule:** do not use `let _ =` on a `Result`, `.ok()`, `unwrap`, or `expect` unless you add
  `// errpath: allow(rule) <reason>` on the previous line.
- Prefer `?` or `if let Err(e) = ... { tracing::warn!(...); }`.
- Run `cargo test` before you finish.
- Before you hand back, run:

```bash
cargo errpath check --base HEAD
```

or `cargo errpath review --base <start-commit> --format json`.

## Acting on findings

| Rule | Action |
|------|--------|
| `no-panic` | Propagate with `?`, handle explicitly, or `// errpath: allow(no-panic) <reason>` |
| `no-swallow` | Propagate, handle and record (`tracing::warn!`), or allow with reason |
| `no-trace` | Record the error, or allow with reason |
