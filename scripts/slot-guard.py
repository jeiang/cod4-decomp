#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Slot leases for the Linux test host, installed apart from the ref under test.

Usage:
  slot-guard version
  slot-guard status [slot...]
  slot-guard acquire <slot> <owner> [--task <text>] [--replace]
  slot-guard release <slot> [--force]
  slot-guard run [--protocol <n>] <slot> -- <command...>

A lease names who may run in a slot; the token that proves it is read from COD4E_SLOT_TOKEN (never from the command
line, which other users of the host see in `ps`). `acquire` with no token in the environment makes one and prints it
alone on stdout; a coordinator may instead hand out its own token (16 or more printable characters). The lease file
keeps only the token's SHA-256.

`run` checks the token against the lease and takes the slot's run lock in one critical section, then runs the
command with the run lock inherited, so the lease cannot be released or reassigned while the command or anything it
left running is alive, even after the guard itself was killed. A second run under the same lease is refused rather
than queued. The kernel drops the run lock when the last process holding it exits, so a crash leaves no live run
behind; the lease itself stays until its owner releases it or the coordinator replaces it (`--replace`/`--force`,
refused while a run is live). The guard never signals a run it did not start; it forwards SIGINT, SIGTERM and SIGHUP
to its own command and exits with that command's status (128+signal when it was killed by one).

The guard does not take scripts/host-run.sh's slot, stage or serial locks; those keep their meaning. It protects only
launches that go through it: a client that starts host-run.sh directly (scripts/remote-linux.sh before the guard, or
without COD4E_LINUX_SLOT_TOKEN) bypasses the lease and is still serialised by host-run.sh's own slot lock alone.

State lives in COD4E_SLOT_GUARD_DIR (default ~/Projects/cod4e/slot-guard). Exit status: 0 done, 2 usage, 75 refused
(no lease, wrong token, slot leased to someone else, run live) or the command's own status for `run`.
"""

import fcntl
import hashlib
import hmac
import json
import os
import re
import secrets
import signal
import subprocess
import sys
import tempfile
import time

VERSION = 1
EX_USAGE = 2
EX_REFUSED = 75
SLOT_RE = re.compile(r"[A-Za-z0-9_-]{1,32}")
TOKEN_RE = re.compile(r"[!-~]{16,256}")
TOKEN_ENV = "COD4E_SLOT_TOKEN"


class Refused(Exception):
    pass


class Usage(Exception):
    pass


def state_dir():
    d = os.environ.get("COD4E_SLOT_GUARD_DIR") or os.path.expanduser("~/Projects/cod4e/slot-guard")
    os.makedirs(d, mode=0o700, exist_ok=True)
    return d


def check_slot(slot):
    if not SLOT_RE.fullmatch(slot):
        raise Usage(f"slot name '{slot}' must be 1-32 letters, digits, '_' or '-'")
    return slot


def check_text(what, text, limit):
    if not text or len(text) > limit or any(ord(c) < 32 or ord(c) == 127 for c in text):
        raise Usage(f"{what} must be 1-{limit} characters with no control characters")
    return text


def env_token():
    token = os.environ.get(TOKEN_ENV, "")
    if token and not TOKEN_RE.fullmatch(token):
        raise Usage(f"{TOKEN_ENV} must be 16-256 printable characters without spaces")
    return token


def digest(token):
    return hashlib.sha256(token.encode()).hexdigest()


class State:
    """The short critical section shared by every lease change and run start, on <dir>/state.lock."""

    def __init__(self):
        self.dir = state_dir()

    def __enter__(self):
        self.fd = os.open(os.path.join(self.dir, "state.lock"), os.O_RDWR | os.O_CREAT, 0o600)
        fcntl.flock(self.fd, fcntl.LOCK_EX)
        return self

    def __exit__(self, *exc):
        os.close(self.fd)

    def path(self, slot, kind):
        return os.path.join(self.dir, f"slot-{slot}.{kind}")

    def read(self, slot, kind):
        try:
            with open(self.path(slot, kind)) as f:
                return json.load(f)
        except FileNotFoundError:
            return None

    def write(self, slot, kind, value):
        fd, tmp = tempfile.mkstemp(dir=self.dir, prefix=f".slot-{slot}.{kind}.")
        try:
            with os.fdopen(fd, "w") as f:
                json.dump(value, f, sort_keys=True)
                f.write("\n")
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, self.path(slot, kind))
        except BaseException:
            os.unlink(tmp)
            raise

    def remove(self, slot, kind):
        try:
            os.unlink(self.path(slot, kind))
        except FileNotFoundError:
            pass

    def open_run_lock(self, slot):
        return os.open(self.path(slot, "run"), os.O_RDWR | os.O_CREAT, 0o600)

    def run_live(self, slot):
        """True while some process holds the slot's run lock. Called only inside the critical section, where no run
        can start, so the probe's own brief hold is never seen by another guard."""
        fd = self.open_run_lock(slot)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        finally:
            os.close(fd)
        return False

    def describe_run(self, slot):
        info = self.read(slot, "run-info") or {}
        if not info:
            return "a run is live (no run record)"
        return (f"a run is live: guard PID {info.get('pid')}, command PID {info.get('child')}, owner "
                f"{info.get('owner')}, started {info.get('started')}, in {info.get('cwd')}")


def describe_lease(lease):
    task = f" for {lease['task']}" if lease.get("task") else ""
    return f"leased to {lease['owner']}{task} since {lease['issued_at']}"


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def token_matches(lease, token):
    return bool(token) and hmac.compare_digest(lease["token_sha256"], digest(token))


def cmd_acquire(args):
    replace = "--replace" in args
    args = [a for a in args if a != "--replace"]
    task = ""
    if "--task" in args:
        i = args.index("--task")
        if i + 1 >= len(args):
            raise Usage("--task needs a value")
        task = check_text("task", args[i + 1], 200)
        del args[i:i + 2]
    if len(args) != 2:
        raise Usage("acquire <slot> <owner> [--task <text>] [--replace]")
    slot = check_slot(args[0])
    owner = check_text("owner", args[1], 64)
    token = env_token()
    generated = not token
    if generated:
        token = secrets.token_urlsafe(24)
    with State() as st:
        lease = st.read(slot, "lease")
        if lease and token_matches(lease, token):
            print(f"slot {slot} already {describe_lease(lease)} under this token", file=sys.stderr)
            return 0
        if lease and not replace:
            raise Refused(f"slot {slot} is {describe_lease(lease)}; the coordinator may --replace it while no run is "
                          f"live")
        if lease and st.run_live(slot):
            raise Refused(f"slot {slot} is {describe_lease(lease)} and {st.describe_run(slot)}; not reassigning it")
        st.write(slot, "lease", {"version": VERSION, "slot": slot, "owner": owner, "task": task,
                                 "token_sha256": digest(token), "issued_at": now()})
    print(f"slot {slot} leased to {owner}", file=sys.stderr)
    if generated:
        print(token)
    return 0


def cmd_release(args):
    force = "--force" in args
    args = [a for a in args if a != "--force"]
    if len(args) != 1:
        raise Usage("release <slot> [--force]")
    slot = check_slot(args[0])
    token = env_token()
    with State() as st:
        lease = st.read(slot, "lease")
        if not lease:
            print(f"slot {slot} has no lease", file=sys.stderr)
            return 0
        if not force and not token_matches(lease, token):
            raise Refused(f"slot {slot} is {describe_lease(lease)}; {TOKEN_ENV} does not match its token")
        if st.run_live(slot):
            raise Refused(f"slot {slot} is {describe_lease(lease)} and {st.describe_run(slot)}; not releasing it")
        st.remove(slot, "lease")
    print(f"slot {slot} released", file=sys.stderr)
    return 0


def cmd_status(args):
    with State() as st:
        slots = [check_slot(s) for s in args]
        if not slots:
            names = {m.group(1) for f in os.listdir(st.dir) if (m := re.fullmatch(r"slot-(.+)\.(lease|run)", f))}
            slots = sorted(names)
        if not slots:
            print("no slot leases")
        for slot in slots:
            lease = st.read(slot, "lease")
            text = describe_lease(lease) if lease else "not leased"
            run = st.describe_run(slot) if st.run_live(slot) else "no guarded run"
            print(f"slot {slot}: {text}; {run}")
    return 0


def cmd_run(args):
    protocol = VERSION
    if args[:1] == ["--protocol"]:
        if len(args) < 2 or not args[1].isdigit():
            raise Usage("--protocol needs a number")
        protocol = int(args[1])
        args = args[2:]
    if protocol > VERSION:
        raise Refused(f"slot-guard {VERSION} is older than the protocol {protocol} this client needs; reinstall it "
                      f"(just slot-guard-install)")
    if len(args) < 3 or args[1] != "--":
        raise Usage("run [--protocol <n>] <slot> -- <command...>")
    slot = check_slot(args[0])
    command = args[2:]
    token = env_token()
    if not token:
        raise Refused(f"{TOKEN_ENV} is not set; a guarded run needs the slot's lease token")
    # Signals that arrive before the command starts are held and delivered to it once it has.
    child = []
    pending = []

    def forward(signum, _frame):
        if child:
            child[0].send_signal(signum)
        else:
            pending.append(signum)

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, forward)
    with State() as st:
        lease = st.read(slot, "lease")
        if not lease:
            raise Refused(f"slot {slot} has no lease; ask the coordinator for one")
        if not token_matches(lease, token):
            raise Refused(f"slot {slot} is {describe_lease(lease)}; {TOKEN_ENV} does not match its token")
        lock = st.open_run_lock(slot)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(lock)
            raise Refused(f"slot {slot} is {describe_lease(lease)} and {st.describe_run(slot)}; wait for it instead "
                          f"of queueing a second run") from None
        # Above the fds scripts/host-run.sh opens with `exec N>` (6-9), so its redirections never close the child's
        # copy of the run lock.
        held = fcntl.fcntl(lock, fcntl.F_DUPFD, 100)
        os.close(lock)
        env = dict(os.environ)
        del env[TOKEN_ENV]
        env["COD4E_SLOT_OWNER"] = lease["owner"]
        try:
            child.append(subprocess.Popen(command, env=env, pass_fds=(held,)))
        except OSError as e:
            os.close(held)
            print(f"slot-guard: cannot start {command[0]}: {e}", file=sys.stderr)
            return 127
        # The command is running and holds the slot: a failed write (a full disk) must not end the guard under it, or
        # the caller would report the run over while it goes on. Say so and wait for the command as usual.
        try:
            st.write(slot, "run-info", {"pid": os.getpid(), "child": child[0].pid, "owner": lease["owner"],
                                        "started": now(), "cwd": os.getcwd()})
        except OSError as e:
            print(f"slot-guard: slot {slot} run started (pid {child[0].pid}) but its run-info was not written ({e}); "
                  f"status will not name this run", file=sys.stderr)
    for signum in pending:
        child[0].send_signal(signum)
    status = child[0].wait()
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, signal.SIG_DFL)
    with State() as st:
        info = st.read(slot, "run-info")
        if info and info.get("pid") == os.getpid():
            st.remove(slot, "run-info")
    os.close(held)
    return 128 - status if status < 0 else status


def main(argv):
    commands = {"acquire": cmd_acquire, "release": cmd_release, "status": cmd_status, "run": cmd_run}
    try:
        if argv[:1] == ["version"] and len(argv) == 1:
            print(f"slot-guard {VERSION}")
            return 0
        if not argv or argv[0] not in commands:
            raise Usage(__doc__.split("\n\n")[1])
        return commands[argv[0]](argv[1:])
    except Usage as e:
        print(f"slot-guard: {e}", file=sys.stderr)
        return EX_USAGE
    except Refused as e:
        print(f"slot-guard: {e}", file=sys.stderr)
        return EX_REFUSED


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
