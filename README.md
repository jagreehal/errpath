# errpath

See how a Rust PR changes error propagation, recovery and panic paths.

`errpath` compares two git refs and reports what changed about how each function can fail. It reads syntax only, so it needs no build, and it works on plain Rust with no library to adopt.

You hear nothing when failure paths stay the same. A rename, a reformat or a new comment produces no output.

## Why

A dependency update changes one line:

```diff
-    builder.build()
+    builder.build().unwrap()
```

The PR title says nothing about it, and it compiles. Run over that commit, `errpath` reports:

```
## Error path review: moderate risk

1 function changed, 1 new finding.

### New findings

- `no-panic` `builder.build.unwrap can panic` at `src/dns/resolver.rs:3`

### Changed error paths

#### `dns::resolver::new_resolver`

- added panic path `builder.build.unwrap`
```

Clippy lints a single `unwrap`, and those lints are worth keeping. `errpath` shows what a change did to the ways your code can fail. It reports only what that change introduced, so it works on a codebase with years of existing `unwrap`s.

## Install

```bash
cargo install cargo-errpath
```

## Use

```bash
# What changed between two refs
cargo errpath review --base main

# The same, as JSON, with collapsed diagrams
cargo errpath review --base main --head my-branch --format json -o review.json
cargo errpath review --base main --diagrams collapsed

# Limit the review to some paths
cargo errpath review --base main crates/billing

# One function as a Mermaid diagram
cargo errpath diagram src/transfer.rs transfer

# The analysis behind all of this, as versioned JSON
cargo errpath ir src
```

`review` reads the two refs from git, not your working tree, and starts from their merge base, so work merged to `main` after you branched isn't counted as yours. When it sees no change it prints nothing to stdout, and one line to stderr saying what it compared and what it couldn't analyse.

## GitHub Action

```yaml
name: errpath

on:
  pull_request:

permissions:
  contents: read
  pull-requests: write

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
      - uses: jagreehal/errpath@v0.1.0
```

Every review goes in the job summary, and new findings are annotated in the Files changed tab. By default only moderate and high risk get a PR comment, and that one comment is updated as the PR changes. If a later push drops below the threshold, the comment is removed.

| Input | Default | |
| --- | --- | --- |
| `comment-on` | `moderate` | Lowest risk that gets a comment: `low`, `moderate` or `high` |
| `fail-on` | `never` | Fail the job at this risk or above: `never`, `moderate` or `high` |
| `comment` | `true` | Post a PR comment at all |
| `annotations` | `true` | Annotate new findings inline |
| `diagrams` | `collapsed` | Diagrams in the comment: `open` or `collapsed` |
| `path` | | Paths to review, space separated |
| `base`, `head` | the PR's | Refs to compare |
| `version` | the action's tag | `cargo-errpath` version to install |
| `cli` | | A command to run instead of installing |

It outputs `risk`, `changed` and `report`, the path of the JSON report.

A fork PR's token can't comment, so its review stays in the job summary and annotations. We'd leave `fail-on` at `never` until you've seen a few weeks of its output on your code.

## What it reports

Each function's exits are compared between the two refs. Moved and renamed functions are matched up, so moving code isn't reported as new.

| Change | Risk |
| --- | --- |
| An error is certainly dropped (`let _ = commit();`, `commit().ok();`) | High |
| A new panic path (`unwrap`, `expect`, `panic!`, `todo!`) | Moderate |
| An error may be dropped, but the callee's type can't be seen (`let _ = tx.send(1);`) | Moderate |
| A step moved in or out of a branch or loop, or a condition changed | Moderate |
| An error conversion changed (`map_err(Top::A)` to `map_err(Top::B)`) | Moderate |
| A declared return type changed | Moderate |
| Exits reordered | Moderate |
| `break`, `continue` or an endless `loop` added, removed or retargeted | Moderate |
| A step, failure exit or early return added or removed | Low |
| `.context("...")` text changed, or `match` arms changed | Low |

New functions and deleted functions don't produce a report on their own, unless a new one has a finding. They aren't a change to an existing failure path.

Treat risk as a starting point. You decide whether a change matters from the code around it.

## Allowing a finding

Some dropped errors are deliberate. Say why, on the line above or at the end of the line:

```rust
// errpath: allow(no-swallow) receiver hung up during shutdown
let _ = tx.send(1);

let n = commit().unwrap(); // errpath: allow(no-panic) checked at startup
```

The reason is required. An allow comment with no reason, or naming an unknown rule, is ignored and reported as `bad-allow`. A review still shows every new allow comment, so a finding can't be silenced unseen, and removing one makes the finding new again.

## Limits

`errpath` reads syntax. Each result states how much it analysed, so read a silent review together with that coverage line.

- **Macros.** A `?` inside a macro call is counted but not placed. Functions wrapped entirely in a macro, like `cfg_rt! { .. }`, aren't analysed at all.
- **Types.** Errors are named from signatures in the same file. On real crates that covers about a quarter of steps. The rest show the enclosing function's error, or "unknown error".
- **Implicit conversions.** A change inside `impl From<A> for B` doesn't appear at the `?` that uses it.
- **Data flow.** A loop with nothing fallible can set a variable that a later `if` fails on. The two aren't connected.
- **Identical calls.** Steps are identified by callee, not arguments, so swapping two `read(r)?` calls that differ only in their arguments looks the same.
- **Diverging calls.** A call to a `fn() -> !` isn't known to stop the path. `while true {}` isn't treated as endless either.
- **Recovery.** `if let Err(e) = a() { .. }` handles an error without a step, so it isn't tracked as an exit.
- **Test code.** `#[test]`, `#[cfg(test)]` and code under a package's `tests/`, `benches/` and `examples/` are skipped. Code behind `cfg(any(test, feature = "..."))` is analysed, because a feature name doesn't prove it's test-only.

These edits produce a report even though behaviour stays the same:

- Renaming a variable that shadows a parameter, as in `let now = now.as_secs();`.
- Renaming a variable captured by name in a string, as in `format!("{ca}")`.
- A closure parameter renamed in a condition, rather than in an error conversion.
- A trailing comma after a turbofish call, as in `f::<T>(x,)`.

## How it's tested

Besides unit tests, a mutation test applies hundreds of small edits to real syntax and checks each one. Edits to failure paths must be reported, and edits that leave them alone must stay silent. To score your own code:

```bash
MUTATE_PATH=path/to/src cargo test real_code -- --ignored --nocapture
```

On a sample of 2,353 mutants from four popular crates, 2,350 got the right answer. The other 3 are renames of shadowing variables. Mutants cover the kinds of edit we wrote operators for, so read that score as coverage of those edits.

Replayed over the last 30 commits of three popular crates, it reported on 39 of 90 commits: 15 low risk, 24 moderate and none high. The default `comment-on` would post 24 PR comments for those 90 commits. We read the reports by hand and most showed real changes to failure paths. We haven't graded them formally.

## JSON

`cargo errpath review --format json` gives you `risk`, `changed`, `markdown`, `newFindings`, `new_allows`, `functions`, `coverage`, `notes` and more. `cargo errpath ir` gives the analysis itself. Both carry a `schema_version`.

## Agent skill

```bash
npx skills add jagreehal/errpath --skill errpath
```

The [skill](skills/errpath/SKILL.md) teaches a coding agent to run a review and act on its findings.

## License

Apache-2.0
