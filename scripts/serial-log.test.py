#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/serial-log.sh: `python3 scripts/serial-log.test.py` (or `just script-tests`). git is real (a
clone of a bare `origin`); `just` is a fake on PATH that prints a harness-like run and exits with $FAKE_RC."""

import glob
import os
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS, "serial-log.sh")

FAKE_JUST = """#!/usr/bin/env bash
echo "args=$*|checks=$COD4E_LINUX_CHECKS"
echo "building on the host" >&2
echo "[1/2] asset-load: Passed (1.0 s)"
echo "[2/2] net-info: Failed (2.0 s)"
echo "noise [3/3] not-a-progress-line: x"
printf '\\nbundle: /tmp/b.zip\\nlocal bundle: bundles/b.zip\\n'
exit "${FAKE_RC:-0}"
"""


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


class SerialLog(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        origin = os.path.join(tmp.name, "origin.git")
        dev = os.path.join(tmp.name, "dev")
        self.clone = os.path.join(tmp.name, "clone")
        git(tmp.name, "init", "-q", "--bare", "-b", "main", origin)
        git(tmp.name, "init", "-q", "-b", "main", dev)
        for k, v in (("user.email", "t@example.invalid"), ("user.name", "t"), ("commit.gpgsign", "false")):
            git(dev, "config", k, v)
        with open(os.path.join(dev, "a.txt"), "w") as f:
            f.write("a\n")
        git(dev, "add", "-A")
        git(dev, "commit", "-q", "-m", "base")
        git(dev, "push", "-q", origin, "main")
        git(tmp.name, "clone", "-q", origin, self.clone)
        # A commit the clone has not fetched yet: serial-log fetches before it resolves.
        with open(os.path.join(dev, "a.txt"), "w") as f:
            f.write("b\n")
        git(dev, "commit", "-q", "-am", "next")
        git(dev, "push", "-q", origin, "main")
        self.sha = git(dev, "rev-parse", "HEAD")
        bin_dir = os.path.join(tmp.name, "bin")
        os.mkdir(bin_dir)
        just = os.path.join(bin_dir, "just")
        with open(just, "w") as f:
            f.write(FAKE_JUST)
        os.chmod(just, os.stat(just).st_mode | stat.S_IXUSR)
        self.env = {**os.environ, "PATH": bin_dir + os.pathsep + os.environ["PATH"]}
        self.env.pop("COD4E_LINUX_CHECKS", None)

    def run_tool(self, *args, **env):
        return subprocess.run(["bash", SCRIPT, *args], cwd=self.clone, env={**self.env, **env},
                              capture_output=True, text=True)

    def the_log(self):
        logs = glob.glob(os.path.join(self.clone, "bundles/serial-logs/*.log"))
        self.assertEqual(len(logs), 1, logs)
        with open(logs[0]) as f:
            return os.path.relpath(logs[0], self.clone), f.read()

    def test_the_whole_run_and_its_status_are_kept(self):
        p = self.run_tool("origin/main", "asset-load,net-info", FAKE_RC="1", COD4E_LINUX_CHECKS="1")
        self.assertEqual(p.returncode, 1)
        path, log = self.the_log()
        self.assertRegex(path, rf"^bundles/serial-logs/serial-{self.sha[:12]}-\d{{8}}T\d{{6}}Z\.log$")
        self.assertTrue(log.startswith(f"sha={self.sha}\nstages=asset-load,net-info\nchecks=1\n"), log)
        self.assertIn(f"args=serial {self.sha} asset-load,net-info|checks=1\n", log)
        self.assertIn("building on the host\n", log)
        self.assertTrue(log.endswith("exit=1\n"), log)
        self.assertEqual(p.stdout.splitlines()[1:], [
            "[1/2] asset-load: Passed (1.0 s)", "[2/2] net-info: Failed (2.0 s)",
            "bundle: /tmp/b.zip", "local bundle: bundles/b.zip", f"exit=1 log={path}"])

    def test_the_default_suite_without_host_checks(self):
        p = self.run_tool(self.sha[:10])
        self.assertEqual(p.returncode, 0, p.stderr)
        _, log = self.the_log()
        self.assertIn("stages=<default suite>\nchecks=0\n", log)
        self.assertIn(f"args=serial {self.sha} |checks=0\n", log)

    def test_an_unknown_ref_runs_nothing(self):
        p = self.run_tool("no-such-ref")
        self.assertEqual(p.returncode, 2)
        self.assertFalse(os.path.exists(os.path.join(self.clone, "bundles")))


if __name__ == "__main__":
    unittest.main()
