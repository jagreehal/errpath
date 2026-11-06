#!/usr/bin/env python3
"""Runs the real shell steps of action.yml against real git repos, with a fake gh.

Usage: cargo build && python3 tests/action_steps.py
"""
import json, os, re, shutil, stat, subprocess, sys, tempfile
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
ACTION = yaml.safe_load((ROOT / "action.yml").read_text())
BIN = ROOT / "target/debug/cargo-errpath"
DEFAULTS = {k: str(v.get("default", "")) for k, v in ACTION["inputs"].items()}
FAKE_GH = """#!/bin/bash
echo "$*" >> "$GH_LOG"
case "$*" in
  *--paginate*) [ -n "${EXISTING:-}" ] && echo "$EXISTING" ;;
  *) [ -n "${GH_FAIL:-}" ] && exit 1 ;;
esac
exit 0
"""

CLEAN = "fn a() -> Result<u32, E> { Ok(1) }\nfn f() -> Result<u32, E> { a()?; Ok(1) }\n"
LOW = "fn a() -> Result<u32, E> { Ok(1) }\nfn f() -> Result<u32, E> { a()?; a()?; Ok(1) }\n"
MODERATE = "fn a() -> Result<u32, E> { Ok(1) }\nfn f() -> Result<u32, E> { a()?; a().unwrap(); Ok(1) }\n"
HIGH = "fn a() -> Result<u32, E> { Ok(1) }\nfn f() -> Result<u32, E> { let _ = a(); a()?; Ok(1) }\n"


def git(cwd, *args):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", *args], cwd=cwd, check=True, capture_output=True)


def sha(cwd, ref):
    return subprocess.run(["git", "rev-parse", ref], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


def repo(tmp, after, name="src/lib.rs"):
    work = tmp / "repo"
    (work / "src").mkdir(parents=True)
    git(work, "init", "-q", "-b", "main")
    (work / "src/lib.rs").write_text(CLEAN)
    git(work, "add", ".")
    git(work, "commit", "-qm", "base")
    base = sha(work, "HEAD")
    (work / name).write_text(after)
    git(work, "add", ".")
    git(work, "commit", "-qm", "change")
    return work, base, sha(work, "HEAD")


def run(after, inputs=None, existing="", gh_fail=False, name="src/lib.rs", pr=True):
    """Runs every step but Install. Returns log, summary, stdout and exit code."""
    with tempfile.TemporaryDirectory() as t:
        tmp = Path(t)
        work, base, head = repo(tmp, after, name)
        fake = tmp / "bin"
        fake.mkdir()
        (fake / "gh").write_text(FAKE_GH)
        (fake / "gh").chmod((fake / "gh").stat().st_mode | stat.S_IEXEC)
        values = {**DEFAULTS, "cli": str(BIN), "github-token": "t", **(inputs or {})}
        github = {"event.pull_request.base.sha": base if pr else "", "event.pull_request.head.sha": head if pr else "", "event.before": base, "repository": "o/r", "event.pull_request.number": "7", "action_ref": ""}
        outputs, stdout, code = {}, "", 0
        summary = tmp / "summary"
        summary.write_text("")
        log = tmp / "gh.log"
        log.write_text("")

        def value(expression):
            kind, _, rest = expression.strip().partition(".")
            if kind == "inputs":
                return values.get(rest, "")
            if kind == "github":
                return github.get(rest, "")
            if kind == "steps":
                step, _, key = rest.partition(".outputs.")
                return outputs.get(step, {}).get(key, "")
            raise ValueError(expression)

        for step in ACTION["runs"]["steps"]:
            if step["name"] == "Install errpath":
                continue
            condition = step.get("if", "")
            if "inputs.comment ==" in condition and (values["comment"] != "true" or not pr):
                continue
            if "inputs.annotations ==" in condition and values["annotations"] != "true":
                continue
            out_file = tmp / "output"
            out_file.write_text("")
            env = {k: re.sub(r"\$\{\{(.*?)\}\}", lambda m: value(m.group(1)), str(v)) for k, v in step.get("env", {}).items()}
            env.update(PATH=f"{fake}:{os.environ['PATH']}", GITHUB_OUTPUT=str(out_file), GITHUB_STEP_SUMMARY=str(summary), RUNNER_TEMP=str(tmp), GH_LOG=str(log), EXISTING=existing, HOME=str(tmp))
            if gh_fail:
                env["GH_FAIL"] = "1"
            p = subprocess.run(["bash", "-e", "-c", step["run"]], cwd=work, env=env, capture_output=True, text=True)
            stdout += p.stdout
            outputs[step.get("id", "")] = dict(l.split("=", 1) for l in out_file.read_text().splitlines() if "=" in l)
            if p.returncode:
                code = p.returncode
                break
        return log.read_text(), summary.read_text(), stdout, code, outputs.get("review", {})


def posts(log):
    return [l for l in log.splitlines() if "--paginate" not in l]


failures = []


def check(name, condition, detail=""):
    print(("ok   " if condition else "FAIL ") + name)
    if not condition:
        failures.append(f"{name}: {detail}")


log, summary, out, code, review = run(MODERATE)
check("moderate posts one comment", len(posts(log)) == 1 and "issues/7/comments -F body=@" in posts(log)[0], log)
check("moderate annotates", "::warning file=src/lib.rs,line=2,title=errpath no-panic::" in out, out)
check("moderate goes in the summary", "Error path review: moderate risk" in summary, summary)
check("outputs are set", review.get("risk") == "moderate" and review.get("changed") == "true", review)
check("a review doesn't fail the job by default", code == 0, code)

log, summary, out, code, _ = run(LOW)
check("low stays out of the PR", posts(log) == [], log)
check("low still reaches the summary", "Error path review: low risk" in summary, summary)

log, *_ = run(LOW, {"comment-on": "low"})
check("comment-on low posts low", len(posts(log)) == 1, log)

log, *_ = run(MODERATE, {"comment-on": "high"})
check("comment-on high skips moderate", posts(log) == [], log)

log, *_ = run(HIGH, {"comment-on": "high"})
check("comment-on high posts high", len(posts(log)) == 1, log)

log, *_ = run(MODERATE, existing="99")
check("an existing comment is updated", len(posts(log)) == 1 and "-X PATCH repos/o/r/issues/comments/99" in posts(log)[0], log)

log, *_ = run(LOW, existing="99")
check("a stale comment is deleted below the threshold", len(posts(log)) == 1 and "-X DELETE repos/o/r/issues/comments/99" in posts(log)[0], log)

log, summary, _, code, review = run(CLEAN + "// a note\n", existing="99")
check("no change deletes a stale comment", "-X DELETE" in log, log)
check("no change says what it compared", "no failure-path change seen in" in summary and "not analysed" in summary, summary)
check("no change sets changed=false", review.get("changed") == "false", review)

log, *_ = run(CLEAN + "// a note\n")
check("no change and no comment means no API writes", posts(log) == [], log)

_, _, _, code, _ = run(MODERATE, {"fail-on": "moderate"})
check("fail-on moderate fails at moderate", code == 1, code)
_, _, _, code, _ = run(LOW, {"fail-on": "moderate"})
check("fail-on moderate passes low", code == 0, code)
_, _, _, code, _ = run(MODERATE, {"fail-on": "high"})
check("fail-on high passes moderate", code == 0, code)
_, _, out, code, _ = run(HIGH, {"fail-on": "high"})
check("fail-on high fails at high", code == 1, code)
check("a certain drop annotates as an error", "::error file=src/lib.rs" in out, out)

_, _, out, code, _ = run(MODERATE, {"comment-on": "nonsense"})
check("a bad comment-on is rejected", code != 0, out)
_, _, out, code, _ = run(MODERATE, {"fail-on": "low"})
check("a bad fail-on is rejected", code != 0, out)

log, _, out, code, _ = run(MODERATE, gh_fail=True)
check("a read-only token only warns", code == 0 and "::warning::errpath could not update" in out, out + log)

log, *_ = run(MODERATE, {"comment": "false"})
check("comment: false writes nothing", log == "", log)

_, _, out, _, _ = run(MODERATE, {"annotations": "false"})
check("annotations: false stays quiet", "::warning" not in out, out)

log, summary, _, code, _ = run(MODERATE, pr=False)
check("a push reviews without a PR", code == 0 and log == "" and "moderate risk" in summary, log + summary)

_, _, out, _, _ = run("fn f() { x().unwrap(); }\n", name="src/a,b%c.rs")
check("annotation properties are escaped", "file=src/a%2Cb%25c.rs" in out, out)

_, summary, _, code, _ = run(MODERATE, {"path": "src/nothing"})
check("path limits the review", code == 0 and "no failure-path change" in summary, summary)

if failures:
    print("\n" + "\n".join(failures))
    sys.exit(1)
print("\nall checks passed")
