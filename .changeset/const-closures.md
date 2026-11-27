---
"cargo-errpath": minor
---

- `no-trace` accepts a handler that reads a borrowed error, as in `match &r`, when `r` itself goes on to the caller through `?`, `return`, the function's value, a local or a closure that runs.
- errpath analyses each closure stored in a `const` or `static` as a function of its own, named by the field it fills (`SCENARIOS::build`). It takes the closure's error type from the field's type, following `use` items, globs, re-exports and generic arguments.
- Reviews list the file and line of each `?` inside a macro that wraps whole items.
- `review` and `ir` skip directories tagged with `CACHEDIR.TAG`, such as Cargo's target dir.
- `check` works in a repository with no commits yet.
- Reviews describe a `loop` with no `break` as that.
