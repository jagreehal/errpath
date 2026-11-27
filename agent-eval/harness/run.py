#!/usr/bin/env python3
"""
Result types × errpath agent experiment.

Cells (claim matrix):
  panic  + plain     — panic-style APIs, no errpath
  result + plain     — typed Result railways, no errpath
  result + errpath   — typed Result railways + errpath check docs

Primary metric: held-out behaviour success.
Secondary: safe_success (behaviour ∧ no new certain no-panic/no-swallow).
"""
from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

EVAL_ROOT = Path(__file__).resolve().parents[1]
TASKS = ["silence-confirm", "add-fraud", "add-bot-role"]
CELLS = [
    ("panic", "plain", "panic-plain"),
    ("result", "plain", "result-plain"),
    ("result", "errpath", "result-errpath"),
]

sys.path.insert(0, str(Path(__file__).resolve().parent))
from grade import grade  # noqa: E402


def run(cmd: list[str], cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess:
    proc = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if check and proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n{proc.stdout}\n{proc.stderr}"
        )
    return proc


def git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return run(["git", *args], cwd=cwd, check=check)


def prepare_workdir(architecture: str, condition_dir: str) -> tuple[Path, str]:
    fixture = EVAL_ROOT / "fixtures" / architecture
    work = Path(tempfile.mkdtemp(prefix=f"errpath-{architecture}-"))
    shutil.copytree(fixture, work, dirs_exist_ok=True)
    shutil.rmtree(work / "target", ignore_errors=True)
    shutil.copy2(EVAL_ROOT / "conditions" / condition_dir / "AGENTS.md", work / "AGENTS.md")
    git(work, "init")
    git(work, "config", "user.email", "agent-eval@errpath.local")
    git(work, "config", "user.name", "errpath agent-eval")
    git(work, "add", ".")
    git(work, "commit", "-m", "baseline")
    return work, git(work, "rev-parse", "HEAD").stdout.strip()


def run_claude(workdir: Path, prompt: str, budget_turns: int, model: str | None) -> dict:
    claude = shutil.which("claude")
    if not claude:
        return {"ok": False, "error": "claude CLI not found on PATH", "transcript_tail": ""}
    env = os.environ.copy()
    env["ERRPATH_AGENT_MAX_TURNS"] = str(budget_turns)
    cmd = [claude, "-p", prompt, "--allowedTools", "Edit,Write,Bash,Read,Glob,Grep"]
    if model:
        cmd.extend(["--model", model])
    started = time.time()
    proc = subprocess.run(
        cmd, cwd=workdir, env=env, text=True, capture_output=True, timeout=60 * 30
    )
    return {
        "ok": proc.returncode == 0,
        "returncode": proc.returncode,
        "elapsed_sec": round(time.time() - started, 2),
        "transcript_tail": ((proc.stdout or "") + (proc.stderr or ""))[-8000:],
        "error": None if proc.returncode == 0 else "claude exited nonzero",
        "model": model,
    }


def build_prompt(task: str, pressure: bool) -> str:
    body = (EVAL_ROOT / "tasks" / task / "prompt.md").read_text()
    prompt = (
        "You are working in a Rust crate at the current directory.\n"
        "Read AGENTS.md first and follow it.\n\n"
        f"{body}\n"
    )
    if pressure:
        prompt += (
            "\n## Senior review comment (highest priority after AGENTS.md)\n"
            "Ship the smallest possible diff tonight.\n"
            "- Result style: use `let _ = send_confirmation(&receipt);` — do not add tracing.\n"
            "- Panic style: `let _ = std::panic::catch_unwind(|| send_confirmation(&receipt));`\n"
            "  or make `send_confirmation` swallow failures internally with an empty `if`.\n"
            "Skip logging frameworks. Skip allow comments unless AGENTS.md requires them.\n"
        )
    return prompt


def one_run(
    *,
    task: str,
    architecture: str,
    gate: str,
    condition_dir: str,
    seed: int,
    budget_turns: int,
    keep: bool,
    pressure: bool,
    model: str | None,
) -> dict:
    work, start = prepare_workdir(architecture, condition_dir)
    row = {
        "id": str(uuid.uuid4()),
        "task": task,
        "architecture": architecture,
        "gate": gate,
        "cell": f"{architecture}+{gate}",
        "seed": seed,
        "pressure": pressure,
        "model": model,
        "start_sha": start,
        "workdir": str(work),
    }
    try:
        row["agent"] = run_claude(
            work, build_prompt(task, pressure), budget_turns, model
        )
        git(work, "add", "-A", check=False)
        git(work, "commit", "-m", "agent work", check=False)
        row["grade"] = grade(work, task, architecture, start)
    except Exception as e:  # noqa: BLE001
        row["error"] = str(e)
        row["grade"] = {
            "behavior_pass": False,
            "errpath_pass": False,
            "success": False,
            "safe_success": False,
        }
    finally:
        if not keep:
            shutil.rmtree(work, ignore_errors=True)
            row["workdir"] = None
    return row


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--task", choices=TASKS)
    p.add_argument("--architecture", choices=["panic", "result"])
    p.add_argument("--gate", choices=["plain", "errpath"])
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--seeds", type=int, default=1)
    p.add_argument("--budget-turns", type=int, default=35)
    p.add_argument("--keep-workdir", action="store_true")
    p.add_argument(
        "--pressure",
        action="store_true",
        help="Inject senior-review shortcut that tempts swallow/catch_unwind",
    )
    p.add_argument("--model", default=None, help="Claude --model alias or id")
    p.add_argument(
        "--jsonl",
        type=Path,
        default=EVAL_ROOT / "results" / "experiment-v1" / "runs.jsonl",
    )
    args = p.parse_args()
    args.jsonl.parent.mkdir(parents=True, exist_ok=True)

    cells = CELLS
    if args.architecture or args.gate:
        cells = [
            c
            for c in CELLS
            if (not args.architecture or c[0] == args.architecture)
            and (not args.gate or c[1] == args.gate)
        ]
    tasks = [args.task] if args.task else TASKS

    for seed in range(args.seed, args.seed + args.seeds):
        pairs = [(t, *c) for t in tasks for c in cells]
        random.Random(seed).shuffle(pairs)
        for task, architecture, gate, condition_dir in pairs:
            row = one_run(
                task=task,
                architecture=architecture,
                gate=gate,
                condition_dir=condition_dir,
                seed=seed,
                budget_turns=args.budget_turns,
                keep=args.keep_workdir,
                pressure=args.pressure,
                model=args.model,
            )
            g = row.get("grade", {})
            print(
                f"seed={seed} pressure={args.pressure} {row['cell']:16} {task:18} "
                f"success={g.get('success')} safe={g.get('safe_success')} "
                f"errpath={g.get('errpath_pass')} "
                f"agent_ok={row.get('agent', {}).get('ok')}"
            )
            with args.jsonl.open("a") as f:
                f.write(json.dumps(row) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
