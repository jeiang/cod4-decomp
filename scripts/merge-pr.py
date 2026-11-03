#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Squash-merge a PR only at the head that was validated: `just merge-pr <n> <sha>` (dry run unless CONFIRM=yes).

Each gate fails closed, in this order, with its own exit status:
  2  <sha> does not resolve to a commit (after fetching the base and the PR head), or gh/git failed
  3  the PR's head is not <sha> (evidence for another head does not apply), or the PR is not open
  4  the `windows` check is missing or did not pass, or any other check is neither passed nor skipped (pending
     counts as not passed)
  5  `mergeable` is not MERGEABLE: UNKNOWN is polled every MERGE_PR_POLL seconds (default 10) for up to
     MERGE_PR_WAIT seconds (default 180), and still UNKNOWN after that stops too
  6  `gh pr merge` failed
Then it prints `git diff --stat origin/<base>...<sha>` and the files the PR deletes. Without CONFIRM=yes it stops
there (exit 0, nothing merged); with it, it runs `gh pr merge <n> --squash --match-head-commit <sha>`, so GitHub
refuses the merge if the head moved meanwhile, fetches, and fast-forwards this checkout only when it is on <base>.
"""

import json
import os
import subprocess
import sys
import time


class Stop(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def run(cmd, check=True):
    p = subprocess.run(cmd, capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if check and p.returncode != 0:
        raise Stop(2, f"{' '.join(cmd)} failed (exit {p.returncode}): {p.stderr.strip()[:500]}")
    return p


def git(*args, check=True):
    return run(["git", *args], check=check)


def gh_json(*args):
    p = run(["gh", *args], check=False)
    try:
        return json.loads(p.stdout)
    except ValueError:
        raise Stop(2, f"gh {' '.join(args)} printed no JSON (exit {p.returncode}): {p.stderr.strip()[:500]}")


def pr_view(pr, fields):
    d = gh_json("pr", "view", pr, "--json", fields)
    if not isinstance(d, dict):
        raise Stop(2, f"gh pr view {pr} printed no object")
    return d


def checks_gate(pr):
    try:
        checks = gh_json("pr", "checks", pr, "--json", "name,state,bucket")
    except Stop as e:
        raise Stop(4, f"STOP: {e}")
    if not isinstance(checks, list) or not all(isinstance(c, dict) for c in checks):
        raise Stop(4, "gh pr checks printed no list of checks")
    for c in checks:
        print(f"check {c.get('name')} {c.get('state')} {c.get('bucket')}")
    windows = [c for c in checks if c.get("name") == "windows"]
    if not windows:
        raise Stop(4, "STOP: no windows check")
    if any(c.get("bucket") != "pass" for c in windows):
        raise Stop(4, f"STOP: windows is {', '.join(str(c.get('bucket')) for c in windows)}, not pass")
    bad = [c for c in checks if c.get("bucket") not in ("pass", "skipping")]
    if bad:
        raise Stop(4, "STOP: not passed: " + ", ".join(f"{c.get('name')} ({c.get('bucket')})" for c in bad))


def mergeable_gate(pr):
    wait = float(os.environ.get("MERGE_PR_WAIT", "180"))
    poll = float(os.environ.get("MERGE_PR_POLL", "10"))
    deadline = time.monotonic() + wait
    while True:
        m = pr_view(pr, "mergeable").get("mergeable")
        print(f"mergeable={m}")
        if m != "UNKNOWN" or time.monotonic() >= deadline:
            break
        time.sleep(poll)
    if m != "MERGEABLE":
        raise Stop(5, f"STOP: mergeable={m}")


def merge(pr, want):
    facts = pr_view(pr, "headRefOid,baseRefName,state")
    head, base = facts.get("headRefOid"), facts.get("baseRefName")
    if not head or not base:
        raise Stop(2, "gh pr view gave no headRefOid or baseRefName")
    git("fetch", "-q", "origin", base, f"pull/{pr}/head")
    sha = git("rev-parse", "--verify", "--quiet", f"{want}^{{commit}}", check=False).stdout.strip()
    if not sha:
        raise Stop(2, f"STOP: {want} is not a commit here")
    if head != sha:
        raise Stop(3, f"STOP: head {head} != validated {sha}")
    if facts.get("state") != "OPEN":
        raise Stop(3, f"STOP: PR is {facts.get('state')}")
    checks_gate(pr)
    mergeable_gate(pr)
    print(f"--- git diff --stat origin/{base}...{sha}")
    print(git("diff", "--stat", f"origin/{base}...{sha}").stdout, end="")
    print("--- deleted files")
    print(git("diff", "--diff-filter=D", "--name-only", f"origin/{base}...{sha}").stdout or "(none)\n", end="")
    if os.environ.get("CONFIRM") != "yes":
        print("DRY RUN: nothing merged; rerun with CONFIRM=yes after reading the stat")
        return 0
    p = run(["gh", "pr", "merge", pr, "--squash", "--match-head-commit", sha], check=False)
    print(p.stdout, end="")
    if p.returncode != 0:
        raise Stop(6, f"STOP: gh pr merge failed (exit {p.returncode}): {p.stderr.strip()[:500]}")
    git("fetch", "-q", "origin", base)
    branch = git("symbolic-ref", "--quiet", "--short", "HEAD", check=False).stdout.strip()
    if branch == base:
        git("merge", "--ff-only", "-q", f"origin/{base}")
    print(f"merged; origin/{base} is now " + git("log", "--oneline", "-1", f"origin/{base}").stdout.strip())
    return 0


def main(argv):
    if len(argv) != 2 or not argv[0].isdigit():
        print("usage: merge-pr.py <pr number> <validated sha>", file=sys.stderr)
        return 2
    try:
        return merge(argv[0], argv[1])
    except Stop as e:
        print(e, file=sys.stderr)
        return e.code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
