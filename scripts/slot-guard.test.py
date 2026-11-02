#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/slot-guard.py, run as separate processes against a temporary state directory:
`python3 scripts/slot-guard.test.py` (or `just script-tests`)."""

import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest

GUARD = os.path.join(os.path.dirname(os.path.abspath(__file__)), "slot-guard.py")
TOKEN_A = "token-a-0123456789abcdef"
TOKEN_B = "token-b-0123456789abcdef"


class SlotGuard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = os.path.join(self.tmp.name, "state")
        self.procs = []
        self.addCleanup(self.reap)

    def reap(self):
        for p in self.procs:
            if p.poll() is None:
                p.kill()
                p.wait()

    def env(self, token=None):
        env = dict(os.environ, COD4E_SLOT_GUARD_DIR=self.dir)
        env.pop("COD4E_SLOT_TOKEN", None)
        if token is not None:
            env["COD4E_SLOT_TOKEN"] = token
        return env

    def guard(self, *args, token=None):
        return subprocess.run([sys.executable, GUARD, *args], env=self.env(token), capture_output=True, text=True,
                              timeout=60)

    def start(self, *args, token=None):
        p = subprocess.Popen([sys.executable, GUARD, *args], env=self.env(token), stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True)
        self.procs.append(p)
        return p

    def path(self, name):
        return os.path.join(self.tmp.name, name)

    def wait_for(self, name, timeout=30):
        deadline = time.monotonic() + timeout
        while not os.path.exists(self.path(name)):
            if time.monotonic() > deadline:
                self.fail(f"{name} never appeared")
            time.sleep(0.02)
        with open(self.path(name)) as f:
            return f.read()

    def held_command(self, name):
        """A command that records its PID in <name>.pid and runs until <name>.stop exists."""
        code = ("import os, sys, time\n"
                f"open({self.path(name + '.tmp')!r}, 'w').write(str(os.getpid()))\n"
                f"os.replace({self.path(name + '.tmp')!r}, {self.path(name + '.pid')!r})\n"
                f"while not os.path.exists({self.path(name + '.stop')!r}): time.sleep(0.02)\n")
        return [sys.executable, "-c", code]

    def stop(self, name):
        open(self.path(name + ".stop"), "w").close()

    def wait_idle(self, slot):
        """Waits for the kernel to drop a killed process's run lock (kill(2) returns before the process is gone)."""
        deadline = time.monotonic() + 30
        while "no guarded run" not in self.guard("status", slot).stdout:
            self.assertLess(time.monotonic(), deadline, "the run lock outlived its processes")
            time.sleep(0.05)

    def lease(self, slot="1", owner="worker-a", token=TOKEN_A):
        r = self.guard("acquire", slot, owner, "--task", "issue 1", token=token)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_acquire_without_token_prints_one_that_runs(self):
        r = self.guard("acquire", "2", "worker-a")
        self.assertEqual(r.returncode, 0, r.stderr)
        token = r.stdout.strip()
        self.assertGreaterEqual(len(token), 16)
        lease_text = open(os.path.join(self.dir, "slot-2.lease")).read()
        self.assertNotIn(token, lease_text)
        r = self.guard("run", "2", "--", sys.executable, "-c", "pass", token=token)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_competing_token_is_refused_and_same_token_is_idempotent(self):
        self.lease()
        r = self.guard("acquire", "1", "worker-b", token=TOKEN_B)
        self.assertEqual(r.returncode, 75)
        self.assertIn("leased to worker-a for issue 1", r.stderr)
        self.assertEqual(self.guard("acquire", "1", "worker-a", token=TOKEN_A).returncode, 0)
        ran = self.path("ran")
        r = self.guard("run", "1", "--", sys.executable, "-c", f"open({ran!r}, 'w')", token=TOKEN_B)
        self.assertEqual(r.returncode, 75)
        self.assertFalse(os.path.exists(ran))

    def test_run_needs_a_matching_lease(self):
        r = self.guard("run", "1", "--", sys.executable, "-c", "pass", token=TOKEN_A)
        self.assertEqual(r.returncode, 75)
        self.assertIn("no lease", r.stderr)
        self.lease()
        r = self.guard("run", "1", "--", sys.executable, "-c", "pass")
        self.assertEqual(r.returncode, 75)
        self.assertIn("COD4E_SLOT_TOKEN is not set", r.stderr)

    def test_run_keeps_the_command_status_and_hides_the_token(self):
        self.lease()
        code = "import os, sys; print(os.environ.get('COD4E_SLOT_TOKEN'), os.environ['COD4E_SLOT_OWNER']); sys.exit(7)"
        r = subprocess.run([sys.executable, GUARD, "run", "1", "--", sys.executable, "-c", code],
                           env=self.env(TOKEN_A), capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 7, r.stderr)
        self.assertEqual(r.stdout.strip(), "None worker-a")

    def test_live_run_blocks_release_reassignment_and_a_second_run(self):
        self.lease()
        run = self.start("run", "1", "--", *self.held_command("a"), token=TOKEN_A)
        self.wait_for("a.pid")
        refused = ((["release", "1"], TOKEN_B), (["release", "1"], TOKEN_A), (["release", "1", "--force"], None),
                   (["acquire", "1", "worker-b", "--replace"], TOKEN_B),
                   (["run", "1", "--", sys.executable, "-c", "pass"], TOKEN_A))
        for args, token in refused:
            r = self.guard(*args, token=token)
            self.assertEqual(r.returncode, 75, (args, r.stderr))
        status = self.guard("status", "1")
        self.assertIn("leased to worker-a", status.stdout)
        self.assertIn("a run is live", status.stdout)
        self.assertNotIn(TOKEN_A, status.stdout + status.stderr)
        self.stop("a")
        self.assertEqual(run.wait(timeout=30), 0)
        self.assertIn("no guarded run", self.guard("status", "1").stdout)
        self.assertEqual(self.guard("release", "1", token=TOKEN_B).returncode, 75)
        self.assertEqual(self.guard("release", "1", token=TOKEN_A).returncode, 0)
        self.assertIn("not leased", self.guard("status", "1").stdout)

    def test_concurrent_acquires_have_one_winner(self):
        tokens = [f"token-{i:02d}-0123456789abcdef" for i in range(12)]
        procs = [self.start("acquire", "3", f"worker-{i}", token=t) for i, t in enumerate(tokens)]
        codes = sorted(p.wait(timeout=60) for p in procs)
        self.assertEqual(codes, [0] + [75] * (len(tokens) - 1))

    def test_concurrent_runs_under_one_lease_start_one_command(self):
        self.lease()
        count, stop = self.path("count"), self.path("stop")
        code = (f"import os, time\nfd = os.open({count!r}, os.O_WRONLY | os.O_CREAT | os.O_APPEND)\n"
                f"os.write(fd, b'x')\nwhile not os.path.exists({stop!r}): time.sleep(0.02)\n")
        procs = [self.start("run", "1", "--", sys.executable, "-c", code, token=TOKEN_A) for _ in range(8)]
        # The winner runs until every other guard has been refused.
        deadline = time.monotonic() + 60
        while sum(p.poll() is not None for p in procs) < 7:
            self.assertLess(time.monotonic(), deadline, "a competing run was not refused")
            time.sleep(0.02)
        open(stop, "w").close()
        codes = sorted(p.wait(timeout=60) for p in procs)
        self.assertEqual(codes, [0] + [75] * 7)
        self.assertEqual(open(count).read(), "x")

    def test_killed_guard_keeps_the_slot_until_its_command_exits(self):
        self.lease()
        run = self.start("run", "1", "--", *self.held_command("a"), token=TOKEN_A)
        pid = int(self.wait_for("a.pid"))
        run.kill()
        run.wait(timeout=30)
        # The command is still alive and still holds the run lock: no reassignment, and it is not killed.
        r = self.guard("acquire", "1", "worker-b", "--replace", token=TOKEN_B)
        self.assertEqual(r.returncode, 75, r.stderr)
        os.kill(pid, 0)
        self.stop("a")
        deadline = time.monotonic() + 30
        while self.guard("acquire", "1", "worker-b", "--replace", token=TOKEN_B).returncode != 0:
            self.assertLess(time.monotonic(), deadline, "the run lock outlived its command")
            time.sleep(0.05)
        r = self.guard("run", "1", "--", sys.executable, "-c", "pass", token=TOKEN_B)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_crash_of_guard_and_command_frees_the_slot(self):
        self.lease()
        run = self.start("run", "1", "--", *self.held_command("a"), token=TOKEN_A)
        pid = int(self.wait_for("a.pid"))
        run.kill()
        run.wait(timeout=30)
        os.kill(pid, signal.SIGKILL)
        self.wait_idle("1")
        self.assertEqual(self.guard("run", "1", "--", sys.executable, "-c", "pass", token=TOKEN_A).returncode, 0)
        self.assertEqual(self.guard("release", "1", "--force").returncode, 0)

    def test_terminate_reaches_the_command(self):
        self.lease()
        run = self.start("run", "1", "--", *self.held_command("a"), token=TOKEN_A)
        self.wait_for("a.pid")
        run.send_signal(signal.SIGTERM)
        self.assertEqual(run.wait(timeout=30), 128 + signal.SIGTERM)
        self.assertIn("no guarded run", self.guard("status", "1").stdout)

    def test_newer_protocol_and_bad_input_are_refused(self):
        self.lease()
        r = self.guard("run", "--protocol", "2", "1", "--", sys.executable, "-c", "pass", token=TOKEN_A)
        self.assertEqual(r.returncode, 75)
        self.assertIn("older than the protocol 2", r.stderr)
        self.assertEqual(self.guard("version").stdout, "slot-guard 1\n")
        self.assertEqual(self.guard("acquire", "../1", "worker-a", token=TOKEN_A).returncode, 2)
        self.assertEqual(self.guard("acquire", "1", "worker-a", token="short").returncode, 2)
        self.assertEqual(self.guard("run", "1", sys.executable, token=TOKEN_A).returncode, 2)


if __name__ == "__main__":
    unittest.main()
