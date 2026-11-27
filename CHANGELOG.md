# Changelog

## 0.5.0

- `no-trace` accepts a handler that reads a borrowed error, as in `match &r`, when `r` itself goes on to the caller through `?`, `return`, the function's value, a local or a closure that runs.
- errpath analyses each closure stored in a `const` or `static` as a function of its own, named by the field it fills (`SCENARIOS::build`). It takes the closure's error type from the field's type, following `use` items, globs, re-exports and generic arguments.
- Reviews list the file and line of each `?` inside a macro that wraps whole items.
- `review` and `ir` skip directories tagged with `CACHEDIR.TAG`, such as Cargo's target dir.
- `check` works in a repository with no commits yet.
- Reviews describe a `loop` with no `break` as that.

## 0.4.0

- `cargo errpath check` runs rustfmt, Clippy with `-D warnings`, errpath, and cargo-deny and cargo-shear when you have them installed. It fails on new findings errpath is certain of and lists the possible ones.
- `cargo errpath init` writes a pull request workflow and Claude Code hooks, and suggests autotel when nothing in the repository exports OpenTelemetry.
- `cargo errpath hook start|edit|stop` keeps a coding agent's work passing `check`: one file after each edit, and every changed Cargo workspace before the agent finishes.
- The `no-trace` rule flags an error handler that records nothing OpenTelemetry can see. Every path through the handler has to hand the error on, panic, emit a `tracing` event, record on a span, or call autotel.
- `review` reads the work tree when you leave out `--head`, accepts a tree such as the empty tree as `--base`, and reads each ref's files in one `git cat-file --batch`.
- Only a certainly dropped error rates as high risk.

## 0.3.0

- Errors handled in place, such as `if let Err(e) = a() { log(e) }`, appear in diagrams as handled steps. Reviews rate one added, removed, or changed to catch a different error as moderate risk.
- A returned `match` that hands its error back (`err => err`) shows as a step with an error exit. A guard on that arm forks the diagram, so reviews catch a changed guard.
- PR comment diagrams carry a one-line caption saying they show error paths.
- The README walks through an agent fixing the two-line PR.

## 0.2.0

- Each `?` now takes its error name from the called function's signature anywhere in the same crate, so more steps show the callee's own error. A library and its binaries resolve as separate crates.
- The GitHub Action setup examples use `actions/checkout@v7`.
- The README shows real `errpath` diagrams as images, so they appear in GitHub Mobile. It also has animated failure paths, a worked pull request review, examples for loops, `match`, early returns and async closures, and a linked interactive page for stepping through the diagrams.

## 0.1.0

First release.

- `cargo errpath review` compares two git refs and reports changes to error propagation, recovery and panic paths.
- `cargo errpath diagram` draws one function as a Mermaid diagram, and `cargo errpath ir` prints the analysis as versioned JSON.
- A GitHub Action posts one sticky PR comment, with inline annotations and a job summary.
- `// errpath: allow(rule) reason` comments allow a finding, and a review still shows them.
