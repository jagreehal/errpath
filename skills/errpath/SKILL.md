---
name: errpath
description: >
  Reviews how a Rust change affects error propagation, recovery and panic
  paths with cargo errpath. Use when reviewing a Rust PR or branch, checking a
  change for new unwraps or dropped errors, handling errpath findings or allow
  comments, or setting up the errpath GitHub Action.
---

# errpath

`cargo errpath` compares two git refs and reports what changed about how each function can fail. It reads syntax, so it needs no build.

```bash
cargo install cargo-errpath
```

## Review a change

Commit first: `review` reads refs from git and ignores the working tree.

```bash
cargo errpath review --base main                                # markdown
cargo errpath review --base main --format json -o review.json   # for tools
cargo errpath review --base main crates/billing                 # some paths
```

No stdout means failure paths stayed the same. Read the stderr line too: it says what `errpath` compared and what it skipped.

In JSON, read `risk`, `changed`, `newFindings`, `new_allows`, `functions`, `coverage` and `notes`.

## Act on findings

| Rule | Meaning | Action |
| --- | --- | --- |
| `no-panic` | The change added a panic path | Return the error, or allow it with the invariant that holds |
| `no-swallow` | The change dropped an error | Handle or propagate it, or allow it with the reason |
| `bad-allow` | An allow comment has no reason or an unknown rule | Fix the comment |

Treat risk as a prompt to read the surrounding code. High means an error is certainly dropped. Moderate covers new panics, possible drops, moved steps, and changed conversions, return types, order or jumps. Low covers added or removed steps and exits, `.context` text and `match` arms.

## Allow a finding

Put the comment on the line above or at the end of the line. The reason is required, and reviews still show new allows.

```rust
// errpath: allow(no-swallow) receiver hung up during shutdown
let _ = tx.send(1);

let n = commit().unwrap(); // errpath: allow(no-panic) checked at startup
```

Add an allow only when you can state the reason. Ask the author when you can't.

## Draw one function

```bash
cargo errpath diagram src/transfer.rs transfer   # Mermaid
cargo errpath ir src                             # versioned JSON
```

Each `?` becomes a step with an exit to its error. Error names come from callee signatures anywhere in the same crate, and other steps show the enclosing function's error. Review comments include the changed function's diagram.

## GitHub Action

```yaml
permissions:
  contents: read
  pull-requests: write

steps:
  - uses: actions/checkout@v7
    with:
      fetch-depth: 0
  - uses: jagreehal/errpath@v0.2.0
```

Keep `fail-on` at `never` until the team has read a few weeks of output. `comment-on` sets the lowest risk that gets a PR comment, and defaults to `moderate`.

## Limits

`errpath` sees syntax only: code inside macros, the receiver type of a method call and `impl From` bodies stay out of view. Pair it with tests and Clippy. The README lists each limit.
