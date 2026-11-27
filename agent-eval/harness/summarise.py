#!/usr/bin/env python3
"""Summarise experiment JSONL by cell (architecture+gate) and task."""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("jsonl", type=Path)
    args = p.parse_args()
    if not args.jsonl.is_file():
        print(f"missing {args.jsonl}", file=sys.stderr)
        return 1

    by_cell: dict[str, list[dict]] = defaultdict(list)
    by_cell_task: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for line in args.jsonl.read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if "cell" not in row:
            continue
        by_cell[row["cell"]].append(row)
        by_cell_task[(row["cell"], row["task"])].append(row)

    if not by_cell:
        print("no experiment rows")
        return 0

    print("## By cell")
    print(f"{'cell':16} n  success  safe_success  errpath_clean")
    for cell, rows in sorted(by_cell.items()):
        n = len(rows)
        success = sum(1 for r in rows if r.get("grade", {}).get("success")) / n
        safe = sum(1 for r in rows if r.get("grade", {}).get("safe_success")) / n
        errp = sum(1 for r in rows if r.get("grade", {}).get("errpath_pass")) / n
        print(f"{cell:16} {n:2}  {success:7.0%}  {safe:12.0%}  {errp:13.0%}")

    print("\n## By cell × task")
    print(f"{'cell':16} {'task':18} n  success  safe")
    for (cell, task), rows in sorted(by_cell_task.items()):
        n = len(rows)
        success = sum(1 for r in rows if r.get("grade", {}).get("success")) / n
        safe = sum(1 for r in rows if r.get("grade", {}).get("safe_success")) / n
        print(f"{cell:16} {task:18} {n:2}  {success:7.0%}  {safe:4.0%}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
