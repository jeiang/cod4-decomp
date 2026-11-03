#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/review-delta.py: `python3 scripts/review-delta.test.py` (or `just script-tests`). git is real:
a developer repo pushes main and the PR's refs/pull/7/head to a bare `origin`, and the reviewer's clone runs the
tool. `gh pr view` is a fake on PATH that answers the PR's current head."""

import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS, "review-delta.py")

FAKE_GH = f"""#!{sys.executable}
import os, sys
if sys.argv[1:3] != ["pr", "view"]:
    sys.exit(99)
print(open(os.environ["FAKE_GH_VIEW"]).read())
"""


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


class ReviewDelta(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        self.origin = os.path.join(self.tmp, "origin.git")
        self.dev = os.path.join(self.tmp, "dev")
        git(self.tmp, "init", "-q", "--bare", "-b", "main", self.origin)
        git(self.origin, "config", "uploadpack.allowAnySHA1InWant", "true")  # GitHub serves any commit by SHA
        git(self.tmp, "init", "-q", "-b", "main", self.dev)
        for k, v in (("user.email", "t@example.invalid"), ("user.name", "t"), ("commit.gpgsign", "false")):
            git(self.dev, "config", k, v)
        self.commit({"base.txt": "line\n", "README.md": "readme\n"}, "base")
        git(self.dev, "checkout", "-q", "-b", "topic")
        self.old = self.commit({"pr.txt": "one\n"}, "feat: first PR commit")
        self.push()
        bin_dir = os.path.join(self.tmp, "bin")
        os.mkdir(bin_dir)
        gh = os.path.join(bin_dir, "gh")
        with open(gh, "w") as f:
            f.write(FAKE_GH)
        os.chmod(gh, os.stat(gh).st_mode | stat.S_IXUSR)
        self.view = os.path.join(self.tmp, "view.json")
        self.env = {**os.environ, "PATH": bin_dir + os.pathsep + os.environ["PATH"], "FAKE_GH_VIEW": self.view}

    def commit(self, files, message, branch=None):
        if branch:
            git(self.dev, "checkout", "-q", branch)
        for name, text in files.items():
            with open(os.path.join(self.dev, name), "w") as f:
                f.write(text)
        git(self.dev, "add", "-A")
        git(self.dev, "commit", "-q", "-m", message)
        return git(self.dev, "rev-parse", "HEAD")

    def push(self, force=False):
        git(self.dev, "push", "-q", *(["--force"] if force else []), self.origin, "main", "topic:refs/pull/7/head")

    def advance_main(self, files=None):
        self.main_commit = self.commit(files or {"main.txt": "main\n"}, "fix: a commit on main", branch="main")
        git(self.dev, "checkout", "-q", "topic")

    def run_tool(self, old=None):
        new = git(self.dev, "rev-parse", "topic")
        with open(self.view, "w") as f:
            json.dump({"headRefOid": new, "baseRefName": "main"}, f)
        clone = os.path.join(self.tmp, "reviewer")
        if not os.path.exists(clone):
            git(self.tmp, "clone", "-q", self.origin, clone)
        return subprocess.run([sys.executable, SCRIPT, "7", old or self.old], cwd=clone, env=self.env,
                              capture_output=True, text=True)

    def ok(self, old=None):
        p = self.run_tool(old)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout

    def test_same_base_is_the_plain_diff(self):
        self.commit({"pr.txt": "two\n"}, "feat: second PR commit")
        self.push()
        out = self.ok()
        self.assertIn("--- delta: git diff old new (same base)", out)
        self.assertIn("-one\n+two\n", out)
        self.assertIn("review flags: none", out)

    def test_a_merge_from_main_drops_out(self):
        self.advance_main()
        git(self.dev, "merge", "-q", "--no-edit", "main")
        self.commit({"pr.txt": "two\n", "README.md": "readme\nmore\n"}, "feat: second PR commit")
        self.push()
        out = self.ok()
        self.assertNotIn("main.txt", out)  # main's change, merged into the branch, is not the PR's
        self.assertIn("-one\n+two\n", out)
        self.assertIn("old change replayed onto the new base", out)
        commits = out.split("--- PR commits since the old head (")[1].split("---")[0]
        self.assertTrue(commits.startswith("1)"), commits)
        self.assertIn("feat: second PR commit", commits)
        self.assertNotIn("a commit on main", commits)
        self.assertIn("--- merge commits skipped (1)", out)
        self.assertIn("review flags: README.md", out)

    def test_a_conflicting_replay_falls_back_to_an_interdiff(self):
        self.commit({"base.txt": "pr line\n"}, "feat: PR edits base")
        self.old = git(self.dev, "rev-parse", "HEAD")
        self.push()
        self.advance_main({"base.txt": "main line\n"})
        subprocess.run(["git", "merge", "-q", "--no-edit", "main"], cwd=self.dev, capture_output=True)
        self.commit({"base.txt": "resolved line\n"}, "merge main, resolved")
        self.push()
        out = self.ok()
        self.assertIn("interdiff (the replay conflicts)", out)
        self.assertIn("replay conflicts in: base.txt", out)
        self.assertIn("old PR patch: base.txt", out)

    def test_a_force_pushed_old_head_is_fetched_by_sha(self):
        self.advance_main()
        git(self.dev, "rebase", "-q", "main")
        self.commit({"pr.txt": "two\n"}, "feat: second PR commit")
        self.push(force=True)
        out = self.ok()  # the reviewer's clone never had the old head
        self.assertIn("rebased or force-pushed", out)
        self.assertIn("-one\n+two\n", out)
        self.assertNotIn("main.txt", out)

    def test_an_unknown_old_head_is_an_error(self):
        p = self.run_tool(old="0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(p.returncode, 2)
        self.assertIn("is not a commit here", p.stderr)


if __name__ == "__main__":
    unittest.main()
