#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Serve web/ on localhost with the cross-origin isolation headers (COOP + COEP) the install read bridge needs.

The page reads the user's install folder in the browser and uploads nothing; this server hands out the page and the
wasm module. Two development aids, both off unless asked for:

  --install DIR   also serve the files the client reads under /install/ (with an index.json), so automated browser
                  runs can read the install over HTTP without a folder picker (open the page with ?dev-install=/install).
                  Never use this on a public host.
  POST /report    the page posts its run statistics (numbers only, no file content) with ?report; printed here.
"""
import argparse
import http.server
import json
import os
import re
import sys
from pathlib import Path

# The files the client reads (the same rule as web/main.js).
ZONES = {"code_post_gfx", "code_post_gfx_mp", "localized_code_post_gfx_mp", "ui_mp", "common_mp", "localized_common_mp"}


def wanted(rel: str) -> bool:
    low = rel.lower()
    if low == "localization.txt" or re.fullmatch(r"main/[^/]+\.iwd", low):
        return True
    m = re.fullmatch(r"zone/english/([^/]+)\.ff", low)
    if not m:
        return False
    zone = m.group(1)
    return zone in ZONES or bool(re.fullmatch(r"mp_[a-z0-9_]+", zone) and not zone.endswith("_load"))


def install_index(root: Path):
    out = []
    for dirpath, _, files in os.walk(root):
        for name in files:
            rel = (Path(dirpath) / name).relative_to(root).as_posix()
            if rel.count("/") <= 2 and wanted(rel):
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
        if "/install/" not in self.path:
            super().log_message(fmt, *args)

    def do_POST(self):
        if self.path != "/report":
            return self.send_error(404)
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        print("REPORT", body.decode("utf-8", "replace"), flush=True)
        self.send_response(204)
        self.end_headers()

    def do_GET(self):
        if self.path.startswith("/install/") and self.install:
            return self.get_install()
        super().do_GET()

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
