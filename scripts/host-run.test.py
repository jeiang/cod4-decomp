#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/host-run.sh's host queue and its Chromium check: `python3 scripts/host-run.test.py` (or `just
script-tests`; needs `flock`, which the dev shell provides). The "host" is a temporary home directory on this machine
with a stand-in GC-rooted Chromium; each run gets its own fake checkout (a copy of host-run.sh and find-chromium.sh
with stand-in display, network and web scripts), `cargo` is a stand-in that records its calls, and the stand-in
harness prints `started <FAKE_ID>` and runs until the test creates release-<FAKE_ID>. PATH keeps no directory that
holds a real Chromium."""

import importlib.util
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
HOST_RUN = os.path.join(SCRIPTS, "host-run.sh")
FIND_CHROMIUM = os.path.join(SCRIPTS, "find-chromium.sh")
CHROMIUM_NAMES = ("chromium", "chromium-browser", "google-chrome", "google-chrome-stable")
WEB_STAGES = "web-client web-join web-download web-match"
STAGE_SPEC = importlib.util.spec_from_file_location("stages_for", os.path.join(SCRIPTS, "stages-for.py"))
STAGES_FOR = importlib.util.module_from_spec(STAGE_SPEC)
STAGE_SPEC.loader.exec_module(STAGES_FOR)

PASS_THROUGH = """#!/usr/bin/env bash
shift
[ "${1:-}" = -- ] && shift
exec "$@"
"""
HARNESS = """#!/usr/bin/env bash
echo "started $FAKE_ID" >> "$FAKE_EVENTS"
echo "$FAKE_ID ${COD4E_CHROME:-none}" >> "$FAKE_DIR/chrome"
for _ in $(seq 600); do
  [ -e "$FAKE_DIR/release-$FAKE_ID" ] && exit 0
  sleep 0.1
done
exit 9
"""


def executable(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)


@unittest.skipIf(shutil.which("flock") is None, "needs flock (run inside `nix develop`)")
class HostQueue(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = tmp.name
        self.home = os.path.join(self.dir, "home")
        self.root = os.path.join(self.home, "Projects", "cod4e")
        self.bin = os.path.join(self.dir, "bin")
        self.events = os.path.join(self.dir, "events")
        os.makedirs(self.root)
        executable(os.path.join(self.bin, "cargo"), '#!/usr/bin/env bash\necho "$FAKE_ID $*" >> "$FAKE_DIR/cargo"\n')
        self.chromium = os.path.join(self.root, "bin", "chromium", "bin", "chromium")
        executable(self.chromium, "#!/usr/bin/env bash\nexit 0\n")
        self.path = os.pathsep.join([self.bin] + [d for d in os.environ["PATH"].split(os.pathsep) if d and not any(
            os.path.exists(os.path.join(d, n)) for n in CHROMIUM_NAMES)])
        for n in (1, 2, 3):
            executable(os.path.join(self.root, f"slot-{n}", "target", "release", "cod4e-harness"), HARNESS)
        self.procs = []
        self.addCleanup(self.stop_all)

    def stop_all(self):
        for p in self.procs:
            if p.poll() is None:
                p.kill()
                p.wait()
            p.stderr_file.close()

    def env(self, **extra):
        env = dict(os.environ, HOME=self.home, PATH=self.path,
                   FAKE_EVENTS=self.events, FAKE_DIR=self.dir, COD4E_HOST_WAIT_REPORT="1")
        for k in ("COD4E_SLOT", "COD4E_HOST_WAIT_TIMEOUT", "COD4E_BUILD_HASH"):
            env.pop(k, None)
        env.update(extra)
        return env

    def checkout(self, name):
        c = os.path.join(self.dir, "run-" + name)
        os.makedirs(os.path.join(c, "scripts"))
        shutil.copy(HOST_RUN, os.path.join(c, "scripts", "host-run.sh"))
        executable(os.path.join(c, "scripts", "virtual-display.sh"), PASS_THROUGH)
        executable(os.path.join(c, "scripts", "netns.sh"), PASS_THROUGH)
        shutil.copy(FIND_CHROMIUM, os.path.join(c, "scripts", "find-chromium.sh"))
        executable(os.path.join(c, "web", "build.sh"), '#!/usr/bin/env bash\necho "$FAKE_ID" >> "$FAKE_DIR/web"\n')
        executable(os.path.join(c, "target", "release", "cod4e-harness"), HARNESS)
        return c

    def start(self, name, *args, **env):
        c = self.checkout(name)
        err = open(os.path.join(self.dir, name + ".err"), "w+")
        p = subprocess.Popen(["bash", "scripts/host-run.sh", "--no-checks", *args], cwd=c, stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=err,
                             env=self.env(FAKE_ID=name, COD4E_BUILD_HASH="hash-" + name, **env))
        p.stderr_file = err
        p.name = name
        p.checkout = c
        self.procs.append(p)
        return p

    def stderr(self, p):
        with open(p.stderr_file.name) as f:
            return f.read()

    def started(self):
        if not os.path.exists(self.events):
            return []
        with open(self.events) as f:
            return [line.split()[1] for line in f]

    def until(self, what, cond, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if cond():
                return
            time.sleep(0.1)
        self.fail(f"timed out waiting for {what}; started={self.started()}; stderr: " +
                  "; ".join(f"{p.name}: {self.stderr(p)!r}" for p in self.procs))

    def release(self, p, status=0):
        open(os.path.join(self.dir, "release-" + p.name), "w").close()
        self.assertEqual(p.wait(timeout=30), status, self.stderr(p))

    def queue(self):
        r = subprocess.run(["bash", HOST_RUN, "--queue"], env=self.env(), stdin=subprocess.DEVNULL,
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout

    def test_serial_runs_queue_in_order_and_report_what_they_wait_on(self):
        a = self.start("a", "--", "--stage", "client-load")
        self.until("a to run", lambda: self.started() == ["a"])
        b = self.start("b", "--")
        self.until("b to report", lambda: "queue position 2 of 2" in self.stderr(b))
        c = self.start("c", "--")
        self.until("c to report", lambda: "queue position 3 of 3" in self.stderr(c))
        err = self.stderr(b)
        self.assertRegex(err, rf"(?m)^host-run: waiting \d+s for serial run PID {a.pid}; queue position 2 of 2 ")
        self.assertIn(f"host-run:   ahead: PID {a.pid} serial hash-a, running for", err)
        self.assertIn("(--stage client-load)", err)
        self.assertIn("gives up after 21600s of waiting", err)
        # Reports repeat every COD4E_HOST_WAIT_REPORT seconds.
        self.until("b to report again", lambda: len(re.findall(r"^host-run: waiting \d+s", self.stderr(b), re.M)) >= 3)
        q = self.queue()
        lines = [line for line in q.splitlines() if "PID" in line]
        self.assertEqual(len(lines), 3, q)
        self.assertIn(f"PID {a.pid} serial hash-a, running for", lines[0])
        self.assertIn(f"PID {b.pid} serial hash-b, waiting for serial run PID {a.pid} for", lines[1])
        self.assertIn(f"PID {c.pid} serial hash-c, waiting for serial run PID {a.pid} for", lines[2])
        self.release(a)
        self.until("b to run", lambda: self.started() == ["a", "b"])
        # c waits behind b: for its turn, or for b's pending marker if b got there first.
        self.until("c to wait behind b", lambda: re.search(
            rf"for (its serial turn|serial run PID {b.pid}); queue position 2 of 2 .*\n"
            rf"host-run:   ahead: PID {b.pid} serial hash-b, ", self.stderr(c)))
        time.sleep(1)
        self.assertEqual(self.started(), ["a", "b"])
        self.release(b)
        self.until("c to run", lambda: self.started() == ["a", "b", "c"])
        self.release(c)
        self.assertEqual(self.queue(), "no registered host runs\n")
        self.assertFalse(os.path.exists(os.path.join(self.root, ".serial-pending")))

    def test_a_run_that_waits_too_long_gives_up_with_status_76(self):
        a = self.start("a", "--")
        self.until("a to run", lambda: self.started() == ["a"])
        b = self.start("b", "--", COD4E_HOST_WAIT_TIMEOUT="2")
        self.assertEqual(b.wait(timeout=30), 76, self.stderr(b))
        err = self.stderr(b)
        m = re.search(r"^host-run: gave up after (\d+)s waiting for serial run PID (\d+) \(queue position 2 of 2, "
                      r"COD4E_HOST_WAIT_TIMEOUT=2\); nothing ran after this point$", err, re.M)
        self.assertIsNotNone(m, err)
        self.assertGreaterEqual(int(m.group(1)), 2)
        self.assertEqual(int(m.group(2)), a.pid)
        with open(os.path.join(b.checkout, ".host-run-gave-up")) as f:
            self.assertEqual(f.read(), m.group(0) + "\n")
        self.assertEqual(self.started(), ["a"])
        self.assertNotIn(f"PID {b.pid}", self.queue())
        self.release(a)
        self.assertFalse(os.path.exists(os.path.join(a.checkout, ".host-run-gave-up")))

    def test_slot_runs_wait_for_their_slot_and_for_a_pending_serial_run(self):
        a = self.start("a", "--", COD4E_SLOT="1")
        self.until("a to run", lambda: self.started() == ["a"])
        b = self.start("b", "--", COD4E_SLOT="1")
        self.until("b to report", lambda: "for slot 1; queue position 2 of 2" in self.stderr(b))
        self.assertIn(f"ahead: PID {a.pid} slot 1 hash-a, running for", self.stderr(b))
        # Other slots run alongside.
        c = self.start("c", "--", COD4E_SLOT="2")
        self.until("c to run", lambda: "c" in self.started())
        # A serial run waits for the stage lock the slot runs hold, and keeps new slot runs from starting.
        s = self.start("s", "--")
        self.until("s to wait", lambda: "for the stage lock (exclusive, for the serial run); queue position 3 of 3"
                   in self.stderr(s))
        d = self.start("d", "--", COD4E_SLOT="3")
        self.until("d to report", lambda: f"for serial run PID {s.pid}; queue position 2 of 2" in self.stderr(d))
        self.release(a)
        self.until("b to wait for the serial run", lambda: f"for serial run PID {s.pid}" in self.stderr(b))
        self.release(c)
        self.until("s to run", lambda: self.started()[-1] == "s")
        self.release(s)
        self.until("b and d to run", lambda: {"b", "d"} <= set(self.started()))
        self.release(b)
        self.release(d)
        self.assertEqual(self.started()[:3], ["a", "c", "s"])

    def test_entries_of_dead_or_reused_pids_are_not_runs(self):
        q = os.path.join(self.root, "queue")
        os.makedirs(q)
        # A reused PID: this test's own process, with another start time; and a PID that no longer runs.
        dead = subprocess.Popen(["true"])
        dead.wait()
        for name, pid in (("00000000000000001-1", os.getpid()), ("00000000000000002-2", dead.pid)):
            with open(os.path.join(q, name), "w") as f:
                f.write(f"pid={pid}\nstart=Thu Jan  1 00:00:00 1970\nkind=serial\nstate=running\nsince=0\n")
        a = self.start("a", "--")
        self.until("a to run", lambda: self.started() == ["a"])
        self.assertNotIn("waiting", self.stderr(a))
        self.release(a)
        self.assertEqual(os.listdir(q), [])
        self.assertEqual(self.queue(), "no registered host runs\n")

    def test_wait_settings_must_be_whole_seconds(self):
        for env in ({"COD4E_HOST_WAIT_TIMEOUT": "1h"}, {"COD4E_HOST_WAIT_REPORT": "0"}):
            with self.subTest(env=env):
                p = self.start("x" + str(len(self.procs)), "--", **env)
                self.assertEqual(p.wait(timeout=30), 2)
                self.assertIn("must be a whole number of seconds", self.stderr(p))
        self.assertEqual(self.started(), [])

    def lines(self, name):
        path = os.path.join(self.dir, name)
        if not os.path.exists(path):
            return []
        with open(path) as f:
            return f.read().splitlines()

    def finish(self, name, *args, **env):
        """Runs host-run.sh to its end (its harness, if it gets that far, exits at once); returns the process."""
        open(os.path.join(self.dir, "release-" + name), "w").close()
        p = self.start(name, *args, **env)
        p.wait(timeout=60)
        return p

    def test_without_chromium_a_run_selecting_web_stages_refuses_to_start(self):
        shutil.rmtree(os.path.join(self.root, "bin"))
        refused = (("web", ["--", "--stage", "web-client"], "web-client"),
                   ("webmixed", ["--", "--stage", "client-load,web-match"], "web-match"),
                   ("suite", ["--"], WEB_STAGES),
                   ("slot", ["--", "--stage", "web-join"], "web-join"),
                   ("partial", ["--", "--allow-skip", "web-client"], "web-join web-download web-match"))
        for name, args, stages in refused:
            with self.subTest(args=args):
                p = self.finish(name, *args, **({"COD4E_SLOT": "1"} if name == "slot" else {}))
                self.assertEqual(p.returncode, 78, self.stderr(p))
                self.assertIn(f"host-run: no Chromium for {stages} on this host; run `just chromium-install`",
                              self.stderr(p))
                self.assertNotIn(name, [line.split()[0] for line in self.lines("cargo")])
        self.assertEqual(self.started(), [])
        self.assertEqual(self.queue(), "no registered host runs\n")
        runs = (("checks", []),
                ("native", ["--", "--stage", "client-load"]),
                ("allowed", ["--", "--stage", "client-gfx", "--allow-skip", "web-client"]),
                ("script", ["--", "--stage", "client-gfx", "--script", "scenarios/web-x.cfg"]),
                ("allskip", ["--", "--allow-skip", "web-client,web-join", "--allow-skip", "web-download,web-match"]))
        for name, args in runs:
            with self.subTest(args=args):
                p = self.finish(name, *args)
                self.assertEqual(p.returncode, 0, self.stderr(p))
                self.assertNotIn("Chromium", self.stderr(p))
        p = self.finish("exec", "--exec", "--", "true", COD4E_SLOT="2")
        self.assertEqual(p.returncode, 0, self.stderr(p))
        self.assertEqual(self.started(), ["native", "allowed", "script", "allskip"])
        self.assertEqual(self.lines("chrome"), ["native none", "allowed none", "script none", "allskip none"])
        self.assertEqual(self.lines("web"), ["allskip"])
        self.assertIn("checks build --workspace", self.lines("cargo"))

    def test_the_rooted_chromium_is_passed_to_the_harness(self):
        for name, args in (("web", ["--", "--stage", "web-client"]), ("native", ["--", "--stage", "client-load"])):
            p = self.finish(name, *args)
            self.assertEqual(p.returncode, 0, self.stderr(p))
        self.assertEqual(self.lines("chrome"), [f"web {self.chromium}", f"native {self.chromium}"])
        self.assertEqual(self.lines("web"), ["web"])

    def test_preflight_does_not_recognize_forms_the_harness_rejects(self):
        shutil.rmtree(os.path.join(self.root, "bin"))
        # A valid native selection suppresses the default suite. Unsupported arguments must not add web stages
        # to preflight or trigger a web build. The stand-in harness ignores arguments; the real harness rejects them.
        for name, args in (("equals", ["--stage=web-client"]),
                           ("glob", ["--stage", "web-*"]),
                           ("prefix", ["--stage", "web-client-extra"])):
            p = self.finish(name, "--", "--stage", "client-load", *args)
            self.assertEqual(p.returncode, 0, self.stderr(p))
        self.assertEqual(self.lines("web"), [])
        # An unsupported equals-form allow-skip must not authorize a real web selection.
        p = self.finish("skip-equals", "--", "--stage", "web-client", "--allow-skip=web-client")
        self.assertEqual(p.returncode, 78, self.stderr(p))

    def test_preflight_web_stages_match_the_real_head(self):
        shutil.rmtree(os.path.join(self.root, "bin"))
        repo = os.path.dirname(SCRIPTS)

        def assert_contract(cwd, name):
            expected = [s.name for s in STAGES_FOR.stage_facts("HEAD", cwd=cwd)
                        if s.default and STAGES_FOR.matches_stage("@web", s, None)]
            p = self.finish(name, "--")
            self.assertEqual(p.returncode, 78, self.stderr(p))
            self.assertIn(f"no Chromium for {' '.join(expected)} on this host;", self.stderr(p))
            for i, stage in enumerate(expected):
                p = self.finish(f"{name}-{i}", "--", "--stage", stage)
                self.assertEqual(p.returncode, 78, self.stderr(p))
                self.assertIn(f"no Chromium for {stage} on this host;", self.stderr(p))

        assert_contract(repo, "real-head")
        # Negative control: a fifth default stage using the existing browser implementation must break the same
        # contract. Build a temporary git snapshot so stage_facts reads it exactly as it reads the real HEAD.
        scratch = os.path.join(self.dir, "fifth-web-stage")
        stage_dir = os.path.join(scratch, "crates", "harness", "src")
        shutil.copytree(os.path.join(repo, "crates", "harness", "src", "stages"),
                        os.path.join(stage_dir, "stages"))
        stage_rs = STAGES_FOR.git("show", "HEAD:" + STAGES_FOR.STAGE_RS, cwd=repo)
        extra = ('    StageDef {\n        name: "web-fifth",\n        default: true,\n'
                 '        kind: Kind::Builtin(crate::stages::web_client::run),\n    },\n')
        end = stage_rs.index("\n];", stage_rs.index("pub static STAGES"))
        with open(os.path.join(stage_dir, "stage.rs"), "w") as f:
            f.write(stage_rs[:end] + "\n" + extra + stage_rs[end:])
        STAGES_FOR.git("init", "-q", cwd=scratch)
        STAGES_FOR.git("add", ".", cwd=scratch)
        STAGES_FOR.git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                       "-c", "commit.gpgsign=false", "commit", "-qm", "Add fifth browser stage", cwd=scratch)
        with self.assertRaisesRegex(AssertionError, "web-fifth"):
            assert_contract(scratch, "negative-control")


if __name__ == "__main__":
    unittest.main()
