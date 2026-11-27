# Agent notes

This is a small Rust checkout crate with **errpath** as a finish gate.

- Run `cargo test` before you finish.
- Run `cargo fmt` and `cargo clippy -- -D warnings` if you can.
- Before you hand back, run:

```bash
cargo errpath check --base HEAD
```

If that is unavailable in a dirty work tree, commit your work or run:

```bash
cargo errpath review --base <start-commit> --format json
```

## Acting on findings

| Rule | Meaning | Action |
|------|---------|--------|
| `no-panic` | New `unwrap` / `expect` / `panic!` | Propagate with `?`, handle explicitly, or `// errpath: allow(no-panic) <reason>` |
| `no-swallow` | Dropped `Result` (`let _ =`, `.ok()`, …) | Propagate, handle and record (e.g. `tracing::warn!`), or allow with reason |
| `no-trace` | Handler does not record the error | Record it, or allow with reason |
| `bad-allow` | Allow missing reason / unknown rule | Fix the comment |

Only **new** findings fail the gate. Fix them before finishing.
