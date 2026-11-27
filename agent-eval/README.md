# errpath agent eval — Result types × errpath

Rust-only experiment: do **typed Result railways** and/or **errpath** make coding agents more successful than panic-style APIs?

## Cells

| Cell | Fixture | Gate |
|------|---------|------|
| `panic+plain` | `fixtures/panic` | none |
| `result+plain` | `fixtures/result` | none |
| `result+errpath` | `fixtures/result` | errpath check in `AGENTS.md` |

## Run

```bash
cargo install --path ../..   # cargo-errpath on PATH
python3 harness/run.py --seeds 2 --seed 1
python3 harness/run.py --task silence-confirm --pressure --seeds 3 --seed 10 \
  --jsonl results/experiment-v1/pressure-silence.jsonl
python3 harness/summarise.py results/experiment-v1/runs.jsonl
```

## Latest headline

Baseline 18 runs + 9 pressure silence runs: **all cells 100% success / safe_success** with the local Claude Code default model. Grader still fails known-bad swallow/panic snapshots. See [`../docs/agents-pass-the-compiler.md`](../docs/agents-pass-the-compiler.md).
