#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Serve web/ on localhost with the cross-origin isolation headers (COOP + COEP) a threaded wasm build needs.

The page reads the user's install folder in the browser and uploads nothing; this server only hands out the
page and the wasm module.
"""
import http.server
import sys
from pathlib import Path


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }

    def do_POST(self):
        # `?report` on the page posts its run statistics (numbers only, no file content) so a browser I cannot script
        # still reports them: they are printed here.
        if self.path != "/report":
            return self.send_error(404)
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        print("REPORT", body.decode("utf-8", "replace"), flush=True)
        self.send_response(204)
        self.end_headers()

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
    root = Path(__file__).parent
    handler = lambda *a, **k: Handler(*a, directory=str(root), **k)
    # localhost is a secure context, which WebGPU requires.
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as srv:
        print(f"http://localhost:{port}/  (COOP/COEP on)", flush=True)
        srv.serve_forever()
