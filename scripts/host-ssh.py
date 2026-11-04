#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Detector-free SSH with a per-user, cross-worktree master. See docs/host-notes.md."""
import fcntl
import getopt
import hashlib
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time


def run(args):
    # Accept SSH's transport arguments, including those supplied by scp -S / rsync -e.
    opts, rest = getopt.getopt(args, "46aqTtvxyp:l:i:o:")
    if not rest:
        raise ValueError("usage: host-ssh.py [ssh options] host [command ...]")
    flags = []
    for name, value in opts:
        if name == "-o" and value.split("=", 1)[0].lower() in {
            "controlmaster", "controlpath", "controlpersist", "proxycommand", "proxyjump"}:
            raise ValueError("connection policy belongs in COD4E_LINUX_SSH_CONFIG")
        flags.append(name)
        if value:
            flags.append(value)
    host, command = rest[0], rest[1:]
    config = os.path.realpath(os.environ.get("COD4E_LINUX_SSH_CONFIG", "/dev/null"))
    base = ["ssh", "-F", config, "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
            "-o", "ServerAliveInterval=10", "-o", "ServerAliveCountMax=3"]
    # Only evaluate the explicitly selected config: default ssh -G can run Match exec.
    effective = subprocess.run([*base, *flags, "-G", host], check=True, capture_output=True).stdout
    identity = b"\n".join(line for line in effective.splitlines() if line.split(b" ", 1)[0] in {
        b"hostname", b"user", b"port", b"identityfile", b"identityagent", b"proxycommand", b"proxyjump"})
    key = hashlib.sha256(os.fsencode(config) + b"\0" + identity).hexdigest()[:24]
    root = Path("/tmp", f"cod4e-ssh-{os.getuid()}")
    try:
        root.mkdir(mode=0o700)
    except FileExistsError:
        pass
    info = root.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError(f"unsafe SSH control directory: {root}")
    socket = root / (key + ".sock")
    common = [*base, *flags, "-o", f"ControlPath={socket}", "-o", "ControlPersist=600"]
    # A file lock, unlike a mkdir lock, is released even if the creator crashes.
    with open(root / (key + ".lock"), "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        check = subprocess.run([*common, "-O", "check", host], stdin=subprocess.DEVNULL,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if check.returncode:
            socket.unlink(missing_ok=True)
            # The background master must not retain a follower's output pipes:
            # subprocess capture would otherwise wait for ControlPersist expiry.
            master_log = root / (key + ".master.log")
            with open(master_log, "w") as output:
                started = subprocess.run([*common, "-o", "ControlMaster=yes", "-Nf", host],
                                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=output)
            sys.stderr.write(master_log.read_text())
            if started.returncode:
                return started.returncode
    # Bound concurrent session channels below sshd's default MaxSessions=10.
    channels = [open(root / f"{key}.{n}.channel", "a") for n in range(8)]
    try:
        while True:
            for channel in channels:
                try:
                    fcntl.flock(channel, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    continue
            else:
                time.sleep(.1)
                continue
            break
        # A dead master must NOT silently fall back to a new direct connection.
        # OpenSSH closes inherited descriptors at startup. A separate keeper must
        # own the token while waiting for SSH, even after the caller is SIGKILLed.
        keeper = os.fork()
        if keeper:
            for token in channels:
                token.close()
            def forward(sig, _frame):
                try:
                    os.kill(keeper, sig)
                except ProcessLookupError:
                    pass
            for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
                signal.signal(sig, forward)
            _, status = os.waitpid(keeper, 0)
            result = os.waitstatus_to_exitcode(status)
            return result if result >= 0 else 128 - result
        try:
            for token in channels:
                if token is not channel:
                    token.close()
            child = subprocess.Popen([*common, "-o", "ControlMaster=no", "-o", "ProxyCommand=false", host, *command])
            def forward(sig, _frame):
                child.send_signal(sig)
            for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
                signal.signal(sig, forward)
            status = child.wait()
            os._exit(status if status >= 0 else 128 - status)
        except OSError as error:
            print(f"host-ssh: {error}", file=sys.stderr, flush=True)
            os._exit(255)
    finally:
        for channel in channels:
            channel.close()


if __name__ == "__main__":
    try:
        sys.exit(run(sys.argv[1:]))
    except (ValueError, OSError, getopt.GetoptError) as error:
        print(f"host-ssh: {error}", file=sys.stderr)
        sys.exit(255)
    except subprocess.CalledProcessError as error:
        if error.stderr:
            sys.stderr.buffer.write(error.stderr)
        sys.exit(error.returncode)
