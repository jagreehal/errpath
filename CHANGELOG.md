# Changelog

## 0.1.0

First release.

- `cargo errpath review` compares two git refs and reports changes to error propagation, recovery and panic paths.
- `cargo errpath diagram` draws one function as a Mermaid diagram, and `cargo errpath ir` prints the analysis as versioned JSON.
- A GitHub Action posts one sticky PR comment, with inline annotations and a job summary.
- `// errpath: allow(rule) reason` comments allow a finding, and a review still shows them.
