# experiment-v1

| File | What |
|------|------|
| `runs.jsonl` | Baseline: 3 tasks × 3 cells × 2 seeds (18) |
| `pressure-silence.jsonl` | Silence-confirm with shortcut pressure × 3 cells × 3 seeds (9) |

Summarise:

```bash
python3 ../../harness/summarise.py runs.jsonl
python3 ../../harness/summarise.py pressure-silence.jsonl
```
