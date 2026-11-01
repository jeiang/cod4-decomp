#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Serve web/ on localhost with the cross-origin isolation headers (COOP + COEP) the install read bridge needs.

The page reads the user's install folder in the browser and uploads nothing; this server hands out the page and the
wasm module. Two development aids, both off unless asked for:

  --install DIR   also serve the files the client reads under /install/ (with an index.json), so automated browser
                  runs can read the install over HTTP without a folder picker (open the page with ?dev-install=/install).
                  Never use this on a public host. Safari cannot use it: the reader worker's synchronous request is
                  served through the page's main thread, which is blocked inside the wasm client, so the page never
                  starts. In Safari pick the install folder once and use the saved copy (no ?dev-install).
  POST /report    the page posts its run statistics (numbers only, no file content) with ?report; printed here.
  /control        the dev control channel's bridge (docs/control-channel.md): the harness POSTs one request line to
                  /control and gets the reply as the response (504 after 30 s without one); the page, opened with
                  ?control, long-polls GET /control/next for the next request and POSTs the reply to /control/reply.
                  Without a page polling, every request times out: the bridge does nothing on its own.
"""
from __future__ import annotations

import argparse
import http.server
import json
import os
import re
import sys
import threading
import time
from collections import deque
from pathlib import Path

# How long a request waits for the page's reply before 504, and how long the page's poll waits for a request.
CONTROL_REPLY_TIMEOUT = 30.0
CONTROL_POLL_TIMEOUT = 10.0


class Control:
    """The request queue between the harness (POST /control) and the page (GET /control/next, POST /control/reply).

    A request is a JSON object with an `id`; the page's reply echoes it, which is how the two are paired."""

    def __init__(self):
        self.cond = threading.Condition()
        self.queue: deque[str] = deque()
        self.replies: dict[object, str] = {}

    def ask(self, line: str, timeout: float) -> str | None:
        try:
            key = json.loads(line).get("id")
        except (ValueError, AttributeError):
            return None
        with self.cond:
            self.queue.append(line)
            self.cond.notify_all()
            deadline = time.monotonic() + timeout
            while key not in self.replies:
                left = deadline - time.monotonic()
                if left <= 0:
                    # Nobody answered: the request is withdrawn so a page that starts later does not run it.
                    try:
                        self.queue.remove(line)
                    except ValueError:
                        pass
                    return None
                self.cond.wait(left)
            return self.replies.pop(key)

    def next(self, timeout: float) -> str | None:
        with self.cond:
            deadline = time.monotonic() + timeout
            while not self.queue:
                left = deadline - time.monotonic()
                if left <= 0:
                    return None
                self.cond.wait(left)
            return self.queue.popleft()

    def reply(self, line: str) -> bool:
        try:
            key = json.loads(line).get("id")
        except (ValueError, AttributeError):
            return False
        with self.cond:
            self.replies[key] = line
            self.cond.notify_all()
        return True


control = Control()

# The files the client reads (the same rule as web/install.js): the install's language is line 1 of localization.txt.
LANGUAGES = ["english", "french", "german", "italian", "spanish", "british", "russian", "polish", "korean",
             "taiwanese", "japanese", "chinese", "thai", "leet", "czech"]
ZONES = {"code_post_gfx", "code_post_gfx_mp", "localized_code_post_gfx_mp", "ui_mp", "common_mp", "localized_common_mp"}


def language_of(root: Path) -> str:
    for name in os.listdir(root):
        if name.lower() == "localization.txt":
            lines = (root / name).read_text(errors="replace").splitlines()
            first = lines[0].strip().lower() if lines else ""
            return first if first in LANGUAGES else "english"
    return "english"


def wanted(rel: str, language: str) -> bool:
    low = rel.lower()
    if low == "localization.txt" or re.fullmatch(r"main/[^/]+\.iwd", low):
        return True
    m = re.fullmatch(rf"zone/{language}/([^/]+)\.ff", low)
    if not m:
        return False
    zone = m.group(1)
    return zone in ZONES or bool(re.fullmatch(r"mp_[a-z0-9_]+", zone) and not zone.endswith("_load"))


def install_index(root: Path):
    language = language_of(root)
    out = []
    for dirpath, _, files in os.walk(root):
        for name in files:
            rel = (Path(dirpath) / name).relative_to(root).as_posix()
            if rel.count("/") <= 2 and wanted(rel, language):
                out.append({"path": rel, "size": (Path(dirpath) / name).stat().st_size})
    return sorted(out, key=lambda e: e["path"])


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }
    install: Path | None = None

    def log_message(self, fmt, *args):
        if "/install/" not in self.path and "/control" not in self.path:
            super().log_message(fmt, *args)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode("utf-8", "replace")
        if self.path == "/report":
            print("REPORT", body, flush=True)
            return self.send_text(204, "")
        if self.path == "/control":
            reply = control.ask(body.strip(), CONTROL_REPLY_TIMEOUT)
            if reply is None:
                return self.send_text(504, "no reply from the page (is it open with ?control?)\n")
            return self.send_text(200, reply + "\n", "application/json")
        if self.path == "/control/reply":
            return self.send_text(204 if control.reply(body.strip()) else 400, "")
        self.send_error(404)

    def do_GET(self):
        if self.path.startswith("/install/") and self.install:
            return self.get_install()
        if self.path == "/control/next":
            line = control.next(CONTROL_POLL_TIMEOUT)
            if line is None:
                return self.send_text(204, "")
            return self.send_text(200, line + "\n", "application/json")
        super().do_GET()

    def send_text(self, status: int, text: str, content_type: str = "text/plain"):
        body = text.encode()
        self.send_response(status)
        if body:
            self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def get_install(self):
        rel = self.path[len("/install/"):].split("?")[0]
        if rel == "index.json":
            body = json.dumps(install_index(self.install)).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            return self.wfile.write(body)
        from urllib.parse import unquote

        path = (self.install / unquote(rel)).resolve()
        if not path.is_file() or self.install.resolve() not in path.parents:
            return self.send_error(404)
        size = path.stat().st_size
        m = re.match(r"bytes=(\d+)-(\d*)$", self.headers.get("Range", ""))
        start, end = (int(m.group(1)), int(m.group(2) or size - 1)) if m else (0, size - 1)
        end = min(end, size - 1)
        self.send_response(206 if m else 200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1))
        if m:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        with open(path, "rb") as f:
            f.seek(start)
            left = end - start + 1
            while left:
                chunk = f.read(min(left, 1 << 20))
                if not chunk:
                    break
                self.wfile.write(chunk)
                left -= len(chunk)

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("port", nargs="?", type=int, default=8080)
    ap.add_argument("--install", type=Path, help="serve this install directory under /install/ (development only)")
    args = ap.parse_args()
    Handler.install = args.install
    root = Path(__file__).parent
    handler = lambda *a, **k: Handler(*a, directory=str(root), **k)
    # localhost is a secure context, which WebGPU requires.
    with http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler) as srv:
        print(f"http://localhost:{args.port}/  (COOP/COEP on)", flush=True)
        srv.serve_forever()


if __name__ == "__main__":
    sys.exit(main())
