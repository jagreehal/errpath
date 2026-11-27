#!/usr/bin/env python3
"""Grade a checkout worktree: architecture-specific held-out tests + errpath review."""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

EVAL_ROOT = Path(__file__).resolve().parents[1]
BLOCKING_RULES = {"no-panic", "no-swallow"}


def run(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, env=os.environ.copy(), text=True, capture_output=True)


def install_heldout(workdir: Path, task: str, architecture: str) -> None:
    src = EVAL_ROOT / "tasks" / task / "heldout" / f"{architecture}.rs"
    if not src.is_file():
        raise FileNotFoundError(src)
    dest = workdir / "tests" / "heldout.rs"
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)


def cargo_test(workdir: Path) -> tuple[bool, str]:
    proc = run(["cargo", "test", "--", "--test-threads=1"], cwd=workdir)
    out = (proc.stdout or "") + (proc.stderr or "")
    return proc.returncode == 0, out


def errpath_review(workdir: Path, base: str) -> tuple[dict | None, str]:
    proc = run(
        ["cargo", "errpath", "review", "--base", base, "--format", "json"],
        cwd=workdir,
    )
    text = (proc.stdout or "").strip()
    err = (proc.stderr or "").strip()
    # A failed run or missing output is no review, never a clean one.
    if proc.returncode != 0 or not text:
        return None, f"errpath review exited {proc.returncode}\n{text}\n{err}"
    try:
        return json.loads(text), err
    except json.JSONDecodeError:
        return None, f"invalid errpath JSON\n{text}\n{err}"


def blocking_findings(review: dict) -> list[dict]:
    out = []
    for item in review.get("newFindings") or review.get("new_findings") or []:
        finding = item.get("finding", item)
        if (
            finding.get("rule") in BLOCKING_RULES
            and finding.get("confidence") == "certain"
        ):
            out.append(item if "finding" in item else {"finding": finding})
    return out


def grade(workdir: Path, task: str, architecture: str, base: str) -> dict:
    install_heldout(workdir, task, architecture)
    tests_ok, test_out = cargo_test(workdir)
    review, review_err = errpath_review(workdir, base)
    if review is None:
        return {
            "behavior_pass": tests_ok,
            "errpath_pass": False,
            "success": tests_ok,
            "safe_success": False,
            "blocking_findings": [],
            "new_allows": [],
            "error": review_err,
            "test_output_tail": test_out[-4000:],
        }
    blocking = blocking_findings(review)
    errpath_pass = len(blocking) == 0
    return {
        "behavior_pass": tests_ok,
        "errpath_pass": errpath_pass,
        # Primary: held-out behaviour (fair across panic vs Result).
        "success": tests_ok,
        # Secondary: behaviour + no new certain panic/swallow (Result+errpath claim).
        "safe_success": bool(tests_ok and errpath_pass),
        "blocking_findings": blocking,
        "new_allows": review.get("new_allows") or review.get("newAllows") or [],
        "risk": review.get("risk"),
        "changed": review.get("changed"),
        "review_stderr": review_err,
        "test_output_tail": test_out[-4000:],
    }


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--workdir", type=Path, required=True)
    p.add_argument("--task", required=True)
    p.add_argument("--architecture", choices=["result", "panic"], required=True)
    p.add_argument("--base", required=True)
    p.add_argument("--json-out", type=Path)
    args = p.parse_args()
    result = grade(args.workdir.resolve(), args.task, args.architecture, args.base)
    text = json.dumps(result, indent=2)
    if args.json_out:
        args.json_out.write_text(text + "\n")
    print(text)
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
