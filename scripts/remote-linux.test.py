#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/remote-linux.sh's logs, exit status, bundle names and slot-guard launch:
`python3 scripts/remote-linux.test.py` (or `just script-tests`). The "host" is a temporary home directory on this
machine: `ssh` on PATH runs its command with bash there, and `nix develop -c scripts/host-run.sh` runs a stand-in
host run that prints to both streams, writes a bundle and out/ and exits with FAKE_STATUS."""

import glob
import os
import re
import shlex
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REMOTE = os.path.join(SCRIPTS, "remote-linux.sh")
GUARD = os.path.join(SCRIPTS, "slot-guard.py")
TOKEN = "lease-token-0123456789abcdef"

SSH = """#!/usr/bin/env bash
if [ -n "${FAKE_SSH_FAIL:-}" ]; then echo "ssh: connect to host $1: Connection refused" >&2; exit 255; fi
shift
export HOME=$FAKE_HOME
exec bash -c "$*"
"""
NIX = """#!/usr/bin/env bash
[ "$1 $2 $3" = "develop -c scripts/host-run.sh" ] || exit 98
shift 3
exec "$FAKE_HOST_RUN" "$@"
"""
HOST_RUN = """#!/usr/bin/env bash
{
  printf 'arg=%s\\n' "$@"
  printf 'COD4E_BUILD_HASH=%s\\n' "$COD4E_BUILD_HASH"
  printf 'COD4E_SLOT=%s\\n' "${COD4E_SLOT:-}"
  printf 'COD4E_SLOT_OWNER=%s\\n' "${COD4E_SLOT_OWNER:-}"
  printf 'COD4E_SLOT_TOKEN=%s\\n' "${COD4E_SLOT_TOKEN:-}"
  printf 'shipped=%s\\n' "$(cat version.txt)"
} > "$FAKE_RECORD"
echo "host stdout line"
echo "host stderr line" >&2
mkdir -p bundles out
echo zip > bundles/cod4e-run-1.zip
echo result > out/result.txt
exit "${FAKE_STATUS:-0}"
"""


def executable(path, text):
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)


class RemoteLinux(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        self.repo = os.path.join(self.tmp, "repo")
        self.home = os.path.join(self.tmp, "home")
        self.bin = os.path.join(self.tmp, "bin")
        self.record = os.path.join(self.tmp, "record")
        for d in (self.repo, self.bin, os.path.join(self.home, "Projects", "cod4e")):
            os.makedirs(d)
        executable(os.path.join(self.bin, "ssh"), SSH)
        executable(os.path.join(self.bin, "nix"), NIX)
        executable(os.path.join(self.tmp, "host-run"), HOST_RUN)
        # The temp repo must not depend on the host's git config. Signing is turned off on the command line too: a
        # NixOS host's git carries `commit.gpgsign` in a config file GIT_CONFIG_NOSYSTEM does not turn off.
        self.git_env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        git = ["git", "-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"]
        subprocess.run(["git", "init", "-q", self.repo], check=True, env=self.git_env)
        with open(os.path.join(self.repo, "version.txt"), "w") as f:
            f.write("v1\n")
        subprocess.run([*git, "-C", self.repo, "add", "version.txt"], check=True, env=self.git_env)
        subprocess.run([*git, "-C", self.repo, "commit", "-q", "-m", "v1"], check=True, env=self.git_env)
        self.commit = subprocess.run(["git", "-C", self.repo, "rev-parse", "HEAD"], check=True, capture_output=True,
                                     text=True).stdout.strip()

    def remote(self, *args, **env):
        full = dict(self.git_env, PATH=self.bin + os.pathsep + os.environ["PATH"], COD4E_LINUX_HOST="testhost",
                    FAKE_HOME=self.home, FAKE_HOST_RUN=os.path.join(self.tmp, "host-run"), FAKE_RECORD=self.record)
        for k in ("COD4E_LINUX_SLOT", "COD4E_LINUX_SLOT_TOKEN", "COD4E_LINUX_EXEC", "COD4E_LINUX_CHECKS",
                  "COD4E_SLOT_GUARD_DIR", "COD4E_SLOT_TOKEN"):
            full.pop(k, None)
        for k, v in env.items():
            if v is None:
                full.pop(k, None)
            else:
                full[k] = v
        return subprocess.run([REMOTE, *args], cwd=self.repo, env=full, stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=120)

    def log_of(self, r):
        m = re.search(r"^log: (bundles/logs/remote-\S+)$", r.stderr, re.M)
        self.assertIsNotNone(m, r.stderr)
        with open(os.path.join(self.repo, m.group(1))) as f:
            return m.group(1), f.read()

    def recorded(self):
        with open(self.record) as f:
            return f.read()

    def install_guard(self):
        b = os.path.join(self.home, "Projects", "cod4e", "bin")
        os.makedirs(b)
        executable(os.path.join(b, "slot-guard"),
                   f'#!/usr/bin/env bash\nexec {shlex.quote(sys.executable)} {shlex.quote(GUARD)} "$@"\n')
        env = dict(os.environ, HOME=self.home, COD4E_SLOT_TOKEN=TOKEN)
        env.pop("COD4E_SLOT_GUARD_DIR", None)
        subprocess.run([sys.executable, GUARD, "acquire", "2", "worker-a"], env=env, check=True, capture_output=True)

    def test_success_keeps_streams_log_and_this_runs_bundles(self):
        r = self.remote("--", "--stage", "a b")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("host stdout line", r.stdout)
        self.assertNotIn("host stderr line", r.stdout)
        self.assertIn("host stderr line", r.stderr)
        self.assertIn("local bundle: bundles/cod4e-run-1.zip", r.stdout)
        _, log = self.log_of(r)
        for line in ("host stdout line", "host stderr line", "local bundle: bundles/cod4e-run-1.zip",
                     f"commit={self.commit}", "host_status=0"):
            self.assertIn(line, log)
        self.assertTrue(log.endswith("exit=0\n"), log)
        rec = self.recorded()
        self.assertIn("arg=--\narg=--stage\narg=a b\n", rec)
        self.assertIn(f"COD4E_BUILD_HASH={self.commit[:12]}\n", rec)
        self.assertIn("shipped=v1\n", rec)
        self.assertTrue(os.path.exists(os.path.join(self.repo, "out", "result.txt")))
        self.assertEqual(glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*")), [])

    def test_host_failure_keeps_its_status_bundles_and_cleanup(self):
        r = self.remote("--", FAKE_STATUS="3")
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertIn("local bundle: bundles/cod4e-run-1.zip", r.stdout)
        _, log = self.log_of(r)
        self.assertIn("host_status=3", log)
        self.assertTrue(log.endswith("exit=3\n"), log)
        self.assertEqual(glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*")), [])

    def test_early_failures_are_logged_with_their_status(self):
        cases = (((["no-such-ref", "--"]), {}, 128, "no-such-ref"),
                 (["--"], {"COD4E_LINUX_HOST": None}, 1, "set COD4E_LINUX_HOST"),
                 (["--"], {"FAKE_SSH_FAIL": "1"}, 255, "Connection refused"),
                 (["--"], {"COD4E_LINUX_SLOT_TOKEN": TOKEN}, 2, "needs COD4E_LINUX_SLOT"))
        for args, env, status, message in cases:
            with self.subTest(args=args, env=env):
                r = self.remote(*args, **env)
                self.assertEqual(r.returncode, status, r.stderr)
                _, log = self.log_of(r)
                self.assertIn(message, log)
                self.assertTrue(log.endswith(f"exit={status}\n"), log)
        self.assertFalse(os.path.exists(self.record))

    def test_each_invocation_has_its_own_log(self):
        logs = {self.log_of(self.remote("--"))[0] for _ in range(3)}
        self.assertEqual(len(logs), 3)
        self.assertEqual(len(os.listdir(os.path.join(self.repo, "bundles", "logs"))), 3)

    def test_slot_bundles_are_named_for_the_slot(self):
        r = self.remote("--", COD4E_LINUX_SLOT="2", COD4E_LINUX_CHECKS="0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("local bundle: bundles/slot-2-cod4e-run-1.zip", r.stdout)
        self.assertTrue(os.path.exists(os.path.join(self.repo, "bundles", "slot-2-cod4e-run-1.zip")))
        rec = self.recorded()
        self.assertIn("COD4E_SLOT=2\n", rec)
        self.assertIn("arg=--no-checks\n", rec)
        self.assertIn("COD4E_SLOT_OWNER=\n", rec)

    def test_guarded_run_needs_the_installed_guard_and_the_lease_token(self):
        r = self.remote("--", COD4E_LINUX_SLOT="2", COD4E_LINUX_SLOT_TOKEN=TOKEN)
        self.assertEqual(r.returncode, 75, r.stderr)
        self.assertIn("slot-guard is not installed", r.stderr)
        self.assertFalse(os.path.exists(self.record))
        self.install_guard()
        wrong = "wrong-token-0123456789abcdef"
        r = self.remote("--", COD4E_LINUX_SLOT="2", COD4E_LINUX_SLOT_TOKEN=wrong)
        self.assertEqual(r.returncode, 75, r.stderr)
        self.assertIn("leased to worker-a", r.stderr)
        self.assertFalse(os.path.exists(self.record))
        _, log = self.log_of(r)
        self.assertNotIn(wrong, log)
        self.assertTrue(log.endswith("exit=75\n"), log)
        r = self.remote("--", COD4E_LINUX_SLOT="2", COD4E_LINUX_SLOT_TOKEN=TOKEN, FAKE_STATUS="4")
        self.assertEqual(r.returncode, 4, r.stderr)
        rec = self.recorded()
        self.assertIn("COD4E_SLOT_OWNER=worker-a\n", rec)
        self.assertIn("COD4E_SLOT_TOKEN=\n", rec)
        _, log = self.log_of(r)
        self.assertNotIn(TOKEN, log + r.stdout + r.stderr)
        self.assertIn("guarded=yes", log)
        self.assertEqual(glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*")), [])


if __name__ == "__main__":
    unittest.main()
