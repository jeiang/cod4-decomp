#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Print what a PR's own change did between two heads, without main's changes: `just review-delta <n> <old-sha>`.

For re-reviews. The new head is the PR's current head; each head's base is its merge base with origin/<base>.
- Equal bases: the delta is `git diff <old> <new>`.
- Main merged in (or the branch rebased) in between: the old head's own change is replayed onto the new base
  (`git merge-tree --write-tree --merge-base=<old base> <new base> <old>`) and the delta is that tree against the new
  head, so main's changes drop out and conflict resolutions made in the PR stay in.
- If that replay conflicts: an interdiff, file by file, of the old PR patch (old base..old) against the new one
  (new base..new), said as such.
It lists the commits the PR added (`git rev-list --no-merges <new> ^<old> ^<new base>`; merge commits over the same
range are named as skipped), the delta's stat, a "review flags" line for delta changes to logcheck.rs, stage.rs,
stage modules, scenarios and README.md, and the diff. Read only: it fetches (the base, the PR head, and <old-sha> by
SHA when it is not here, into FETCH_HEAD) and never writes a ref, the index or the worktree. Exit 2 on a failure.
"""

import difflib
import json
import subprocess
import sys

FLAGS = (
    ("logcheck.rs", lambda p: p == "crates/harness/src/logcheck.rs"),
    ("stage.rs", lambda p: p == "crates/harness/src/stage.rs"),
    ("stage modules", lambda p: p.startswith("crates/harness/src/stages/")),
    ("scenarios", lambda p: p.startswith("crates/harness/scenarios/")),
    ("README.md", lambda p: p == "README.md"),
)


class Fail(Exception):
    pass


def run(cmd, ok=(0,)):
    p = subprocess.run(cmd, capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if p.returncode not in ok:
        raise Fail(f"{' '.join(cmd)} failed (exit {p.returncode}): {p.stderr.strip()[:500]}")
    return p


def git(*args, ok=(0,)):
    if args[0] == "diff":
        # A plain patch whatever the user's config says (an external diff tool, colour).
        args = ("diff", "--no-ext-diff", "--no-color", *args[1:])
    return run(["git", "-c", "color.ui=never", *args], ok=ok)


def resolve(ref):
    return git("rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}", ok=(0, 1)).stdout.strip()


def lines(text):
    return [line for line in text.split("\n") if line]


def interdiff(b_old, old, b_new, new):
    """The files whose PR patch changed, and the unified diff of their old and new patches."""
    files = sorted(set(lines(git("diff", "--name-only", b_old, old).stdout))
                   | set(lines(git("diff", "--name-only", b_new, new).stdout)))
    changed, out = [], []
    for f in files:
        a = git("diff", b_old, old, "--", f).stdout.splitlines(keepends=True)
        b = git("diff", b_new, new, "--", f).stdout.splitlines(keepends=True)
        if a != b:
            changed.append(f)
            out.extend(difflib.unified_diff(a, b, f"old PR patch: {f}", f"new PR patch: {f}"))
    return changed, "".join(out)


def delta(pr, old_ref):
    facts = json.loads(run(["gh", "pr", "view", pr, "--json", "headRefOid,baseRefName"]).stdout or "null")
    if not isinstance(facts, dict) or not facts.get("headRefOid") or not facts.get("baseRefName"):
        raise Fail(f"gh pr view {pr} gave no headRefOid or baseRefName")
    new, base = facts["headRefOid"], facts["baseRefName"]
    git("fetch", "-q", "origin", base, f"pull/{pr}/head")
    old = resolve(old_ref)
    if not old:
        # A force-pushed head is no longer reachable from the PR's ref; GitHub still serves it by SHA.
        git("fetch", "-q", "origin", old_ref, ok=(0, 1, 128))
        old = resolve(old_ref)
    if not old:
        raise Fail(f"{old_ref} is not a commit here and `git fetch origin {old_ref}` did not bring it")
    if not resolve(new):
        raise Fail(f"the PR head {new} is not here after fetching pull/{pr}/head")
    b_old = git("merge-base", f"origin/{base}", old).stdout.strip()
    b_new = git("merge-base", f"origin/{base}", new).stdout.strip()
    print(f"PR #{pr}: old head {old} (base {b_old[:12]}), new head {new} (base {b_new[:12]})")
    if git("merge-base", "--is-ancestor", old, new, ok=(0, 1)).returncode:
        print("the old head is not in the new head's history: the branch was rebased or force-pushed")
    rng = [new, f"^{old}", f"^{b_new}"]
    commits = lines(git("log", "--no-merges", "--format=%h %s", *rng).stdout)
    merges = lines(git("log", "--merges", "--format=%h %s", *rng).stdout)
    print(f"--- PR commits since the old head ({len(commits)})")
    for c in commits:
        print(c)
    if merges:
        print(f"--- merge commits skipped ({len(merges)}): " + "; ".join(merges))
    if b_old == b_new:
        how, frm, conflict = "git diff old new (same base)", old, None
    else:
        p = git("merge-tree", "--write-tree", "--name-only", f"--merge-base={b_old}", b_new, old, ok=(0, 1))
        out = p.stdout.split("\n")
        if p.returncode == 0:
            how, frm, conflict = "old change replayed onto the new base, against the new head", out[0], None
        else:
            conflict = sorted({f for f in out[1:out.index("") if "" in out else len(out)] if f})
            how, frm = "interdiff (the replay conflicts)", None
    print(f"--- delta: {how}")
    if frm is None:
        print("replay conflicts in: " + (", ".join(conflict) or "(unnamed)"))
        changed, text = interdiff(b_old, old, b_new, new)
    else:
        print(git("diff", "--stat", frm, new).stdout, end="")
        changed = lines(git("diff", "--name-only", frm, new).stdout)
        text = git("diff", frm, new).stdout
    flags = [name for name, hit in FLAGS if any(hit(p) for p in changed)]
    print("review flags: " + (", ".join(flags) if flags else "none"))
    print("--- diff")
    print(text or "(no change)\n", end="")
    return 0


def main(argv):
    if len(argv) != 2 or not argv[0].isdigit():
        print("usage: review-delta.py <pr number> <old head sha>", file=sys.stderr)
        return 2
    try:
        return delta(argv[0], argv[1])
    except (Fail, ValueError) as e:
        print(f"review-delta: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
