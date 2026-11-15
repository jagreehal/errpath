---
"cargo-errpath": minor
---

- `cargo errpath check` runs rustfmt, Clippy with `-D warnings`, errpath, and cargo-deny and cargo-shear when you have them installed. It fails on new findings errpath is certain of and lists the possible ones.
- `cargo errpath init` writes a pull request workflow and Claude Code hooks, and suggests autotel when nothing in the repository exports OpenTelemetry.
- `cargo errpath hook start|edit|stop` keeps a coding agent's work passing `check`: one file after each edit, and every changed Cargo workspace before the agent finishes.
- The `no-trace` rule flags an error handler that records nothing OpenTelemetry can see. Every path through the handler has to hand the error on, panic, emit a `tracing` event, record on a span, or call autotel.
- `review` reads the work tree when you leave out `--head`, accepts a tree such as the empty tree as `--base`, and reads each ref's files in one `git cat-file --batch`.
- Only a certainly dropped error rates as high risk.
