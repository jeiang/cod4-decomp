#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/merge-pr.py: `python3 scripts/merge-pr.test.py` (or `just script-tests`). git is real: a bare
`origin` in a temporary directory holds main and the PR's refs/pull/7/head. `gh` is a fake on PATH that answers from
a JSON state file and records every call."""

import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS, "merge-pr.py")

FAKE_GH = f"""#!{sys.executable}
import json, os, sys
path = os.environ["FAKE_GH_STATE"]
state = json.load(open(path))
with open(os.environ["FAKE_GH_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\\n")
args = sys.argv[1:]
if args[:2] == ["pr", "view"]:
    if args[-1] == "mergeable":
        seq = state["mergeable"]
        print(json.dumps({{"mergeable": seq[0]}}))
        if len(seq) > 1:
            state["mergeable"] = seq[1:]
            json.dump(state, open(path, "w"))
    else:
        print(json.dumps(state["view"]))
elif args[:2] == ["pr", "checks"]:
    print(state["checks"] if isinstance(state["checks"], str) else json.dumps(state["checks"]))
elif args[:2] == ["pr", "merge"]:
    sys.exit(state.get("merge_exit", 0))
else:
    sys.exit(99)
"""

PASSING = [{"name": "windows", "state": "SUCCESS", "bucket": "pass"},
           {"name": "GitGuardian Security Checks", "state": "SUCCESS", "bucket": "pass"}]


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


class MergePr(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        origin = os.path.join(self.tmp, "origin.git")
        dev = os.path.join(self.tmp, "dev")
        self.clone = os.path.join(self.tmp, "clone")
        git(self.tmp, "init", "-q", "--bare", "-b", "main", origin)
        git(self.tmp, "init", "-q", "-b", "main", dev)
        for k, v in (("user.email", "t@example.invalid"), ("user.name", "t"), ("commit.gpgsign", "false")):
            git(dev, "config", k, v)
        for name in ("keep.txt", "gone.txt"):
            with open(os.path.join(dev, name), "w") as f:
                f.write(name + "\n")
        git(dev, "add", "-A")
        git(dev, "commit", "-q", "-m", "base")
        git(dev, "checkout", "-q", "-b", "topic")
        git(dev, "rm", "-q", "gone.txt")
        with open(os.path.join(dev, "new.txt"), "w") as f:
            f.write("new\n")
        git(dev, "add", "-A")
        git(dev, "commit", "-q", "-m", "feat: the change")
        self.head = git(dev, "rev-parse", "HEAD")
        git(dev, "push", "-q", origin, "main", "topic:refs/pull/7/head")
        git(self.tmp, "clone", "-q", origin, self.clone)
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        gh = os.path.join(bin_dir, "gh")
        with open(gh, "w") as f:
            f.write(FAKE_GH)
        os.chmod(gh, os.stat(gh).st_mode | stat.S_IXUSR)
        self.state = os.path.join(self.tmp, "state.json")
        self.log = os.path.join(self.tmp, "gh.log")
        self.env = {**os.environ, "PATH": bin_dir + os.pathsep + os.environ["PATH"], "FAKE_GH_STATE": self.state,
                    "FAKE_GH_LOG": self.log, "MERGE_PR_POLL": "0.01", "MERGE_PR_WAIT": "0.2"}
        self.env.pop("CONFIRM", None)
        self.set_state()

    def set_state(self, head=None, checks=PASSING, mergeable=("MERGEABLE",), merge_exit=0, state="OPEN"):
        with open(self.state, "w") as f:
            json.dump({"view": {"headRefOid": head or self.head, "baseRefName": "main", "state": state},
                       "checks": checks, "mergeable": list(mergeable), "merge_exit": merge_exit}, f)

    def run_tool(self, sha=None, confirm=False):
        env = {**self.env, **({"CONFIRM": "yes"} if confirm else {})}
        return subprocess.run([sys.executable, SCRIPT, "7", sha or self.head], cwd=self.clone, env=env,
                              capture_output=True, text=True)

    def gh_calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return f.read().splitlines()

    def merged(self):
        return [c for c in self.gh_calls() if c.startswith("pr merge")]

    def test_dry_run_prints_the_stat_and_deletions_and_merges_nothing(self):
        p = self.run_tool()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("--- git diff --stat origin/main...", p.stdout)
        self.assertIn("new.txt", p.stdout)
        self.assertIn("--- deleted files\ngone.txt\n", p.stdout)
        self.assertIn("DRY RUN", p.stdout)
        self.assertEqual(self.merged(), [])

    def test_confirm_merges_at_exactly_the_validated_head(self):
        p = self.run_tool(sha=self.head[:10], confirm=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.merged(), [f"pr merge 7 --squash --match-head-commit {self.head}"])

    def test_another_head_stops(self):
        self.set_state(head="1" * 40)
        p = self.run_tool(confirm=True)
        self.assertEqual(p.returncode, 3)
        self.assertIn("!= validated", p.stderr)
        self.assertEqual(self.merged(), [])

    def test_a_closed_pr_stops(self):
        self.set_state(state="MERGED")
        self.assertEqual(self.run_tool(confirm=True).returncode, 3)
        self.assertEqual(self.merged(), [])

    def test_an_unknown_sha_stops(self):
        self.assertEqual(self.run_tool(sha="no-such-ref", confirm=True).returncode, 2)
        self.assertEqual(self.merged(), [])

    def test_checks_that_did_not_pass_stop(self):
        cases = {
            "windows missing": PASSING[1:],
            "windows failed": [{"name": "windows", "state": "FAILURE", "bucket": "fail"}, PASSING[1]],
            "windows pending": [{"name": "windows", "state": "IN_PROGRESS", "bucket": "pending"}, PASSING[1]],
            "other failed": [PASSING[0], {"name": "GitGuardian Security Checks", "state": "FAILURE", "bucket": "fail"}],
            "not json": "no checks reported on the 'topic' branch",
        }
        for name, checks in cases.items():
            with self.subTest(name):
                self.set_state(checks=checks)
                p = self.run_tool(confirm=True)
                self.assertEqual(p.returncode, 4, p.stderr)
                self.assertEqual(self.merged(), [])

    def test_a_skipped_check_besides_windows_passes(self):
        self.set_state(checks=[PASSING[0], {"name": "optional", "state": "SKIPPED", "bucket": "skipping"}])
        self.assertEqual(self.run_tool(confirm=True).returncode, 0)

    def test_unknown_mergeable_is_waited_for(self):
        self.set_state(mergeable=("UNKNOWN", "UNKNOWN", "MERGEABLE"))
        p = self.run_tool(confirm=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.count("mergeable=UNKNOWN"), 2)
        self.assertEqual(len(self.merged()), 1)

    def test_not_mergeable_stops(self):
        for m in ("CONFLICTING", "UNKNOWN"):  # UNKNOWN: still unknown when the wait runs out
            with self.subTest(m):
                self.set_state(mergeable=(m,))
                p = self.run_tool(confirm=True)
                self.assertEqual(p.returncode, 5, p.stderr)
                self.assertIn(f"mergeable={m}", p.stderr)
                self.assertEqual(self.merged(), [])

    def test_a_failed_merge_is_reported(self):
        self.set_state(merge_exit=1)
        self.assertEqual(self.run_tool(confirm=True).returncode, 6)


if __name__ == "__main__":
    unittest.main()
