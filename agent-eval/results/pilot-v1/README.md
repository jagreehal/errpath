# pilot-v1 results

| File | Contents |
|------|----------|
| `grader-validation.jsonl` | Known-good / known-bad snapshots vs grader expectations |
| `smoke-agent.jsonl` | First live Claude smoke (`silence-confirm` / `plain`) |
| `agent-runs.jsonl` | Mini live matrix: 3 tasks × `{plain, errpath-check}` × seed 1 |

Re-run grader validation:

```bash
python3 agent-eval/harness/run.py --mode validate-grader
```

Summarise agent rows:

```bash
python3 agent-eval/harness/summarise.py agent-eval/results/pilot-v1/agent-runs.jsonl
```
