# Changelog

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
