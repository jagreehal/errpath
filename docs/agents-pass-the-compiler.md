# Coding Agents Pass the Compiler. errpath Catches What They Broke.

*Experiment scripts: [`agent-eval/`](../agent-eval/). Results: [`agent-eval/results/experiment-v1/`](../agent-eval/results/experiment-v1/).*

## Claim under test

> Typed **Result** failure contracts + **errpath** make coding agents more successful than panic-style APIs without the gate.

## Design

Three cells, same tasks, same Claude Code CLI:

| Cell | Architecture | Gate |
|------|----------------|------|
| `panic+plain` | Steps `panic!` on failure; `place_order -> Receipt` | none |
| `result+plain` | Typed `Result` railways; `place_order -> Result<…, OrderError>` | none |
| `result+errpath` | Same Result railways | `AGENTS.md` requires `cargo errpath check` |

Tasks: silence confirmation without undoing payment; wire `check_fraud` before charge; add `Role::Bot`.

Metrics:

- **success** — held-out behavioural tests pass (fair across panic vs Result)
- **safe_success** — success ∧ zero new certain `no-panic` / `no-swallow` findings

## Results (live agents)

### Baseline matrix — 3 tasks × 3 cells × 2 seeds = 18 runs

| Cell | n | success | safe_success |
|------|---|---------|--------------|
| panic+plain | 6 | 100% | 100% |
| result+plain | 6 | 100% | 100% |
| result+errpath | 6 | 100% | 100% |

### Pressure slice — silence-confirm only, senior-review shortcut injected, 3 seeds = 9 runs

Same 100% on all three cells. Agents **refused** the suggested `let _ =` / bare `catch_unwind`, citing `AGENTS.md` and the task’s “record the failure” requirement (often `eprintln!` / `tracing::warn!`).

### Grader discrimination (no LLM)

Known-bad snapshots still fail `safe_success` (`let _ =` → `no-swallow`, `.expect` → `no-panic`). Known-good pass. The metric is real; this model did not produce the bad edits.

## What this means for the claim

| Statement | Supported? |
|-----------|------------|
| Result + errpath is a coherent agent stack | Yes (harness runs end-to-end) |
| errpath detects compile-safe failure-path regressions | Yes (golden bad patches) |
| Result APIs raised live success vs panic APIs here | **No** — ceiling, both 100% |
| errpath gate raised live success vs Result-plain here | **No** — ceiling, both 100% |
| Result + errpath make agents more successful (general) | **Not shown** by these runs |

Honest takeaway: with a strong model on small tasks, architecture and gate did not move the scoreboard. The experiment is ready to re-run on weaker models or harder repos; it does **not** yet justify the marketing sentence.

## Reproduce

```bash
cargo install --path .
python3 agent-eval/harness/run.py --seeds 2 --seed 1 \
  --jsonl agent-eval/results/experiment-v1/runs.jsonl
python3 agent-eval/harness/run.py --task silence-confirm --seeds 3 --seed 10 --pressure \
  --jsonl agent-eval/results/experiment-v1/pressure-silence.jsonl
python3 agent-eval/harness/summarise.py agent-eval/results/experiment-v1/runs.jsonl
```

## Martians context

Evil Martians showed type *checking* barely moves solve rate. This study asked a different question (Result contracts + error-path gate). Their “failures were behavior” line still stands; we did not beat the ceiling on that behavior with this fixture and model.
