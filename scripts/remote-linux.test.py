#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/remote-linux.sh's logs, exit status, bundle names and slot-guard launch:
`python3 scripts/remote-linux.test.py` (or `just script-tests`). The "host" is a temporary home directory on this
machine: `ssh` on PATH runs its command with bash there, and `nix develop -c scripts/host-run.sh` runs a stand-in
host run that prints to both streams, writes a bundle and out/ and exits with FAKE_STATUS."""

import glob
import os
from pathlib import Path
import re
import signal
import shlex
import stat
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REMOTE = os.path.join(SCRIPTS, "remote-linux.sh")
GUARD = os.path.join(SCRIPTS, "slot-guard.py")
TOKEN = "lease-token-0123456789abcdef"

SSH = f"""#!{sys.executable}
import json, os, signal, subprocess, sys, time
# Match OpenSSH's closefrom(3): the session cannot retain a passed token FD.
os.closerange(3, 1024)
args = sys.argv[1:]
with open(os.environ["FAKE_RECORD"] + ".ssh", "a") as log:
    log.write(json.dumps(args) + "\\n")
options = []
config = None
control = None
mode = None
while args and args[0].startswith("-"):
    flag = args.pop(0)
    if flag in ("-o", "-F", "-O", "-p", "-l", "-i"):
        value = args.pop(0)
        if flag == "-o":
            options.append(value)
            if value.startswith("ControlPath="):
                control = value.split("=", 1)[1]
        elif flag == "-F":
            config = value
        elif flag == "-O":
            mode = value
    elif flag == "-G":
        print("hostname " + args[0] + "\\nuser test\\nport 22")
        sys.exit(0)
    elif flag == "-Nf":
        mode = "master"
if mode == "check":
    alive = os.path.exists(control)
    if alive and os.environ.get("FAKE_MASTER_DIES"):
        os.unlink(control)
    sys.exit(0 if alive else 255)
if mode == "master":
    time.sleep(.03)
    open(control, "w").close()
    with open(os.environ["FAKE_RECORD"] + ".masters", "a") as log:
        log.write(control + "\\n")
    sys.exit(0)
if os.environ.get("FAKE_SSH_FAIL"):
    print("ssh: connect to host " + args[0] + ": Connection refused", file=sys.stderr)
    sys.exit(255)
if control and not os.path.exists(control):
    print("ssh: master disappeared; fallback disabled", file=sys.stderr)
    sys.exit(255)
os.environ["HOME"] = os.environ["FAKE_HOME"]
cmd = " ".join(args[1:])
if os.environ.get("FAKE_COMMAND_DELAY"):
    import fcntl
    counter = os.environ["FAKE_RECORD"] + ".active"
    with open(counter, "a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        lock.seek(0); active = int(lock.read() or "0") + 1
        lock.seek(0); lock.truncate(); lock.write(str(active)); lock.flush()
        with open(counter + ".seen", "a") as seen:
            seen.write(str(active) + "\\n")
    time.sleep(float(os.environ["FAKE_COMMAND_DELAY"]))
    with open(counter, "r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        active = int(lock.read()) - 1
        lock.seek(0); lock.truncate(); lock.write(str(active))
if ".host-transport poll " in cmd and os.environ.get("FAKE_POLL_FAIL"):
    print("ssh: final poll diagnostic", file=sys.stderr)
    sys.exit(int(os.environ["FAKE_POLL_FAIL"]))
drop = os.environ.get("FAKE_DROP")
marker = os.environ["FAKE_RECORD"] + ".dropped"
matches = ((drop == "follow" and ".host-transport poll " in cmd and ".host-transport poll 0 0" not in cmd)
           or (drop == "start" and ".host-transport start" in cmd)
           or (drop == "collect" and ".host-transport collect" in cmd))
if matches and not os.path.exists(marker):
    if drop == "follow":
        while not os.path.exists(os.environ["FAKE_RECORD"]):
            time.sleep(.01)
    result = subprocess.run(["bash", "-c", cmd], stdout=subprocess.PIPE)
    open(marker, "w").write(cmd)
    sys.stdout.buffer.write(result.stdout[:len(result.stdout) // 2])
    sys.stdout.buffer.flush()
    sys.exit(255)
os.execvp("bash", ["bash", "-c", cmd])
"""
SETSID = f"""#!{sys.executable}
import os, sys
os.setsid()
os.execvp(sys.argv[1], sys.argv[1:])
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
  printf 'COD4E_HOST_WAIT_TIMEOUT=%s\\n' "${COD4E_HOST_WAIT_TIMEOUT:-}"
  printf 'COD4E_HOST_WAIT_REPORT=%s\\n' "${COD4E_HOST_WAIT_REPORT:-}"
  printf 'shipped=%s\\n' "$(cat version.txt)"
} > "$FAKE_RECORD"
echo start >> "$FAKE_RECORD.starts"
echo "host stdout line"
echo "host stderr line" >&2
/bin/sleep "${FAKE_DELAY:-0}"
echo "host final stdout"
echo "host final stderr" >&2
mkdir -p bundles out
echo zip > bundles/cod4e-run-1.zip
echo result > out/result.txt
[ -z "${FAKE_GAVE_UP:-}" ] || echo "host-run: gave up after 9s waiting for slot 2 (queue position 2 of 2)" > .host-run-gave-up
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
        executable(os.path.join(self.bin, "setsid"), SETSID)
        executable(os.path.join(self.bin, "nix"), NIX)
        executable(os.path.join(self.bin, "sleep"),
                   f'#!{sys.executable}\nimport os, sys, time\n'
                   'with open(os.environ["FAKE_RECORD"] + ".sleeps", "a") as f: f.write(sys.argv[1] + "\\n")\n'
                   'time.sleep(.05)\n')
        self.config = os.path.realpath(os.path.join(self.tmp, "ssh-config"))
        Path(self.config).write_text("")
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

    def remote_env(self, **env):
        full = dict(self.git_env, PATH=self.bin + os.pathsep + os.environ["PATH"], COD4E_LINUX_HOST="testhost",
                    FAKE_HOME=self.home, FAKE_HOST_RUN=os.path.join(self.tmp, "host-run"), FAKE_RECORD=self.record,
                    COD4E_LINUX_SSH_CONFIG=self.config)
        for k in ("COD4E_LINUX_SLOT", "COD4E_LINUX_SLOT_TOKEN", "COD4E_LINUX_EXEC", "COD4E_LINUX_CHECKS",
                  "COD4E_SLOT_GUARD_DIR", "COD4E_SLOT_TOKEN", "COD4E_HOST_WAIT_TIMEOUT", "COD4E_HOST_WAIT_REPORT"):
            full.pop(k, None)
        for k, v in env.items():
            if v is None:
                full.pop(k, None)
            else:
                full[k] = v
        return full

    def remote(self, *args, **env):
        return subprocess.run([REMOTE, *args], cwd=self.repo, env=self.remote_env(**env), stdin=subprocess.DEVNULL,
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

    def test_wait_settings_are_passed_on_and_a_give_up_is_named_last(self):
        r = self.remote("--", COD4E_HOST_WAIT_TIMEOUT="0", COD4E_HOST_WAIT_REPORT="60")
        self.assertEqual(r.returncode, 0, r.stderr)
        rec = self.recorded()
        self.assertIn("COD4E_HOST_WAIT_TIMEOUT=0\nCOD4E_HOST_WAIT_REPORT=60\n", rec)
        self.assertNotIn("gave up", r.stderr)
        r = self.remote("--", COD4E_HOST_WAIT_TIMEOUT="6h")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("COD4E_HOST_WAIT_TIMEOUT must be a whole number of seconds", r.stderr)
        r = self.remote("--", FAKE_STATUS="76", FAKE_GAVE_UP="1")
        self.assertEqual(r.returncode, 76, r.stderr)
        lines = r.stderr.splitlines()
        self.assertEqual(lines[-2], "remote: host run gave up waiting: gave up after 9s waiting for slot 2 "
                                    "(queue position 2 of 2)")
        # A command's own status 76 is not a give-up.
        r = self.remote("--", FAKE_STATUS="76")
        self.assertEqual(r.returncode, 76, r.stderr)
        self.assertNotIn("gave up", r.stderr)

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

    def test_killed_follower_connection_reconnects_without_losing_run_output_or_status(self):
        r = self.remote("--", FAKE_DROP="follow", FAKE_DELAY="3", FAKE_STATUS="37")
        self.assertEqual(r.returncode, 37, r.stderr)
        self.assertTrue(os.path.exists(self.record + ".dropped"))
        self.assertIn("reconnecting", r.stderr)
        for text, stream in (("host stdout line", r.stdout), ("host final stdout", r.stdout),
                             ("host stderr line", r.stderr), ("host final stderr", r.stderr)):
            self.assertEqual(stream.count(text), 1, stream)
        self.assertIn("local bundle: bundles/cod4e-run-1.zip", r.stdout)
        self.assertEqual(Path(self.record + ".starts").read_text(), "start\n")
        self.assertTrue(self.log_of(r)[1].endswith("exit=37\n"))

    def test_launch_acknowledgement_and_collection_connection_can_be_lost(self):
        for phase in ("start", "collect"):
            with self.subTest(phase=phase):
                r = self.remote("--", FAKE_DROP=phase, FAKE_STATUS="255")
                self.assertEqual(r.returncode, 255, r.stderr)
                self.assertIn("host_status=255", self.log_of(r)[1])
                self.assertIn("local bundle:", r.stdout)
                self.assertTrue(os.path.exists(self.record + ".dropped"))
                os.unlink(self.record + ".dropped")
        self.assertEqual(Path(self.record + ".starts").read_text(), "start\nstart\n")

    def test_poll_backoff_is_bounded_and_keeps_last_error_and_run(self):
        r = self.remote("--", FAKE_POLL_FAIL="255")
        self.assertEqual(r.returncode, 255, r.stderr)
        self.assertIn("reconnect attempts exhausted", r.stderr)
        self.assertIn("ssh: final poll diagnostic", r.stderr)
        self.assertEqual(len(glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*"))), 1)
        waits = [float(n) for n in Path(self.record + ".sleeps").read_text().splitlines() if float(n) >= 1]
        self.assertEqual(len(waits), 4, waits)
        for wait, base in zip(waits, (5, 10, 20, 30)):
            self.assertGreaterEqual(wait, base)
            self.assertLess(wait, base + 1)

    def test_non_transport_error_is_not_retried(self):
        r = self.remote("--", FAKE_POLL_FAIL="42")
        self.assertEqual(r.returncode, 42, r.stderr)
        self.assertNotIn("reconnecting", r.stderr)
        self.assertTrue(all(float(n) < 1 for n in Path(self.record + ".sleeps").read_text().splitlines()))

    def test_poll_interval_is_five_seconds(self):
        r = self.remote("--", FAKE_DELAY="1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("5", Path(self.record + ".sleeps").read_text().splitlines())

    def test_shared_detector_free_options(self):
        import json
        r = self.remote("--")
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = [json.loads(n) for n in Path(self.record + ".ssh").read_text().splitlines()]
        paths = set()
        for args in calls:
            self.assertEqual(args[:2], ["-F", self.config])
            for option in ("BatchMode=yes", "ConnectTimeout=10", "ServerAliveInterval=10",
                           "ServerAliveCountMax=3"):
                self.assertIn(option, args)
            paths.update(n for n in args if n.startswith("ControlPath="))
            if any(".host-transport poll " in n for n in args):
                self.assertIn("ControlMaster=no", args)
                self.assertIn("ProxyCommand=false", args)
        self.assertEqual(len(paths), 1)
        self.assertEqual(len(Path(self.record + ".masters").read_text().splitlines()), 1)
        self.assertTrue(any("ControlPersist=600" in args for args in calls))

    def test_concurrent_callers_share_master_and_recreate_after_death(self):
        helper = os.path.join(SCRIPTS, "host-ssh.py")
        env = self.remote_env()
        calls = [subprocess.Popen([sys.executable, helper, "testhost", "true"], env=env)
                 for _ in range(4)]
        for p in calls:
            self.assertEqual(p.wait(timeout=15), 0)
        masters = Path(self.record + ".masters").read_text().splitlines()
        self.assertEqual(len(masters), 1)
        os.unlink(masters[0])
        r = subprocess.run([sys.executable, helper, "-t", "testhost", "true"], env=env)
        self.assertEqual(r.returncode, 0)
        self.assertEqual(Path(self.record + ".masters").read_text().splitlines(), masters * 2)

    def test_master_dying_before_command_fails_closed_without_replay(self):
        helper = os.path.join(SCRIPTS, "host-ssh.py")
        args = [sys.executable, helper, "testhost", "true"]
        self.assertEqual(subprocess.run(args, env=self.remote_env()).returncode, 0)
        r = subprocess.run(args, env=self.remote_env(FAKE_MASTER_DIES="1"), capture_output=True, text=True)
        self.assertEqual(r.returncode, 255)
        self.assertIn("fallback disabled", r.stderr)
        self.assertEqual(len(Path(self.record + ".masters").read_text().splitlines()), 1)
        self.assertEqual(subprocess.run(args, env=self.remote_env()).returncode, 0)
        self.assertEqual(len(Path(self.record + ".masters").read_text().splitlines()), 2)

    def test_command_channel_budget(self):
        helper = os.path.join(SCRIPTS, "host-ssh.py")
        env = self.remote_env(FAKE_COMMAND_DELAY=".5")
        calls = [subprocess.Popen([sys.executable, helper, "testhost", "true"], env=env) for _ in range(12)]
        for p in calls:
            self.assertEqual(p.wait(timeout=15), 0)
        counts = [int(n) for n in Path(self.record + ".active.seen").read_text().splitlines()]
        self.assertLessEqual(max(counts), 8)
        self.assertGreater(max(counts), 1)

    def test_killed_wrapper_keeps_token_until_fd_closing_ssh_exits(self):
        helper = os.path.join(SCRIPTS, "host-ssh.py")
        env = self.remote_env(FAKE_COMMAND_DELAY="2")
        args = [sys.executable, helper, "testhost", "true"]
        calls = [subprocess.Popen(args, env=env) for _ in range(8)]
        active = Path(self.record + ".active")
        self.wait_for(lambda: active.exists() and active.read_text() == "8")
        calls[0].kill()
        calls[0].wait(timeout=5)
        ninth = subprocess.Popen(args, env=env)
        time.sleep(.25)
        self.assertIsNone(ninth.poll())
        self.assertEqual(active.read_text(), "8")
        for p in calls[1:]:
            self.assertEqual(p.wait(timeout=10), 0)
        self.assertEqual(ninth.wait(timeout=10), 0)
        counts = [int(n) for n in Path(self.record + ".active.seen").read_text().splitlines()]
        self.assertEqual(max(counts), 8)

    def wait_for(self, predicate):
        end = time.monotonic() + 15
        while time.monotonic() < end:
            if predicate():
                return
            time.sleep(.02)
        self.fail("timed out waiting for detached host run")

    def test_killed_mac_follower_leaves_guarded_run_and_attach_recovers(self):
        self.install_guard()
        p = subprocess.Popen([REMOTE, "--"], cwd=self.repo,
                             env=self.remote_env(COD4E_LINUX_SLOT="2", COD4E_LINUX_SLOT_TOKEN=TOKEN,
                                                 FAKE_DELAY="8", FAKE_STATUS="19"),
                             stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: p.poll() is None and p.kill())
        self.wait_for(lambda: os.path.exists(self.record))
        run = glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*"))[0]
        # TERM stops the local follower only; the private detached session and its lease survive it.
        p.terminate()
        stdout, stderr = p.communicate(timeout=15)
        self.assertEqual(p.returncode, 143, stderr)
        self.assertIn("host run keeps its lease and locks", stderr)
        self.assertTrue(os.path.isdir(run))
        denied = self.remote("--", COD4E_LINUX_SLOT="2", COD4E_LINUX_SLOT_TOKEN=TOKEN)
        self.assertEqual(denied.returncode, 75, denied.stderr)
        self.wait_for(lambda: os.path.exists(os.path.join(run, ".host-status")))
        self.assertEqual(Path(run, ".host-status").read_text(), "19\n")
        r = self.remote("--attach", "~/Projects/cod4e/" + os.path.basename(run))
        self.assertEqual(r.returncode, 19, r.stderr)
        self.assertIn("host final stdout", r.stdout)
        self.assertIn("host final stderr", r.stderr)
        self.assertIn("local bundle: bundles/slot-2-cod4e-run-1.zip", r.stdout)
        self.assertNotIn(TOKEN, stdout + stderr + r.stdout + r.stderr + self.log_of(r)[1])
        self.assertFalse(os.path.exists(run))

    def test_sigkill_and_ctrl_c_leave_run_recoverable(self):
        for sig in (signal.SIGKILL, signal.SIGINT):
            with self.subTest(signal=sig):
                p = subprocess.Popen([REMOTE, "--"], cwd=self.repo,
                                     env=self.remote_env(FAKE_DELAY="3", FAKE_STATUS="23"),
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     text=True, start_new_session=True)
                self.addCleanup(lambda p=p: p.poll() is None and os.killpg(p.pid, signal.SIGKILL))
                self.wait_for(lambda: os.path.exists(self.record))
                run = glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*"))[0]
                os.killpg(p.pid, sig)
                p.communicate(timeout=15)
                self.assertEqual(p.returncode, -sig if sig == signal.SIGKILL else 130)
                self.assertTrue(os.path.isdir(run))
                r = self.remote("--attach", "~/Projects/cod4e/" + os.path.basename(run))
                self.assertEqual(r.returncode, 23, r.stderr)
                self.assertIn("local bundle:", r.stdout)
                self.assertFalse(os.path.exists(run))
                os.unlink(self.record)

    def test_attach_reports_dead_supervisor_and_rejects_untrusted_paths(self):
        run = os.path.join(self.home, "Projects", "cod4e", "run-dead")
        os.makedirs(run)
        # Capture a genuine generated transport by interrupting a live run, then simulate an abnormal host death.
        p = subprocess.Popen([REMOTE, "--"], cwd=self.repo, env=self.remote_env(FAKE_DELAY="3"),
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.wait_for(lambda: os.path.exists(self.record))
        live = next(d for d in glob.glob(os.path.join(self.home, "Projects", "cod4e", "run-*")) if d != run)
        p.terminate()
        p.communicate(timeout=15)
        for name in (".host-transport", ".host-slot", ".host-stdout", ".host-stderr"):
            with open(os.path.join(live, name), "rb") as source, open(os.path.join(run, name), "wb") as dest:
                dest.write(source.read())
        with open(os.path.join(run, ".host-identity"), "w") as f:
            f.write("99999999\nnot-a-start-time\n99999999\n")
        r = self.remote("--attach", "~/Projects/cod4e/run-dead")
        self.assertEqual(r.returncode, 255, r.stderr)
        self.assertIn("gone without an exit status", r.stderr)
        self.assertTrue(os.path.isdir(run))
        self.wait_for(lambda: os.path.exists(os.path.join(live, ".host-status")))
        r = self.remote("--attach", "~/Projects/cod4e/" + os.path.basename(live))
        self.assertEqual(r.returncode, 0, r.stderr)
        r = self.remote("--attach", "~/Projects/cod4e/run-x;echo bad")
        self.assertEqual(r.returncode, 2, r.stderr)


if __name__ == "__main__":
    unittest.main()
