// SPDX-License-Identifier: GPL-3.0-only
// The install reader. The wasm module runs on the page's main thread (winit owns the canvas there) and needs
// synchronous ranged reads of the install; a File or an OPFS file can only be read synchronously in a worker. This
// worker answers the page's requests through two SharedArrayBuffers (see bridge.js for the other side):
//
//   ctl  Int32 [0] state (IDLE, REQUEST, DONE, FAILED), [1] file id, [2] byte count; Float64 at byte 16: offset
//   data bytes read (or, after a failure, the UTF-8 error message)
//
// Sources: "files" (File objects picked this session), "opfs" (the copy made by copier.js) and "http" (a development
// server's `/install/`, for automated runs only: see web/serve.py).

const IDLE = 0, REQUEST = 1, DONE = 2, FAILED = 3;
export const COPY_DIR = "cod4e-install";

let sources = [];

// A source is { size, read(offset, dst) } filling dst (a Uint8Array over the shared data buffer) completely.

function fileSource(file) {
  const sync = new FileReaderSync();
  return {
    size: file.size,
    read(offset, dst) {
      dst.set(new Uint8Array(sync.readAsArrayBuffer(file.slice(offset, offset + dst.length))));
    },
  };
}

async function opfsSources() {
  const root = await navigator.storage.getDirectory();
  let dir;
  try {
    dir = await root.getDirectoryHandle(COPY_DIR);
  } catch {
    return null;
  }
  let manifest;
  try {
    manifest = JSON.parse(await (await (await dir.getFileHandle("manifest.json")).getFile()).text());
  } catch {
    return null;
  }
  if (!manifest.complete) return null;
  const out = [];
  for (const { path, size } of manifest.files) {
    const handle = await (await dir.getFileHandle(encodeURIComponent(path))).createSyncAccessHandle();
    if (handle.getSize() !== size) throw new Error(`the saved copy of ${path} is damaged`);
    let scratch = null; // used when this browser refuses a shared buffer
    out.push({
      path, size,
      read(offset, dst) {
        if (!scratch) {
          try { return void handle.read(dst, { at: offset }); } catch { scratch = new Uint8Array(dst.length); }
        }
        if (scratch.length < dst.length) scratch = new Uint8Array(dst.length);
        handle.read(scratch.subarray(0, dst.length), { at: offset });
        dst.set(scratch.subarray(0, dst.length));
      },
    });
  }
  return out;
}

function httpSources(base, index) {
  return index.map(({ path, size }) => {
    const url = `${base}/${path.split("/").map(encodeURIComponent).join("/")}`;
    return {
      path, size,
      read(offset, dst) {
        const x = new XMLHttpRequest();
        x.open("GET", url, false);
        x.responseType = "arraybuffer";
        x.setRequestHeader("Range", `bytes=${offset}-${offset + dst.length - 1}`);
        x.send();
        if (x.status !== 206 && !(x.status === 200 && dst.length === size)) throw new Error(`${url}: HTTP ${x.status}`);
        dst.set(new Uint8Array(x.response));
      },
    };
  });
}

function serve(ctl, data) {
  const i32 = new Int32Array(ctl), f64 = new Float64Array(ctl, 16, 1), bytes = new Uint8Array(data);
  const text = new TextEncoder();
  for (;;) {
    Atomics.wait(i32, 0, IDLE);
    if (Atomics.load(i32, 0) !== REQUEST) continue;
    let state = DONE;
    try {
      const src = sources[i32[1]];
      if (!src) throw new Error(`no install file ${i32[1]}`);
      src.read(f64[0], bytes.subarray(0, i32[2]));
    } catch (e) {
      const msg = text.encode(String(e?.message ?? e)).subarray(0, 1024);
      bytes.set(msg);
      i32[2] = msg.length;
      state = FAILED;
    }
    Atomics.store(i32, 0, state);
    Atomics.notify(i32, 0);
  }
}

self.onmessage = async ({ data: m }) => {
  if (m.type !== "open") return;
  try {
    if (m.source === "files") {
      sources = m.entries.map(([path, file]) => ({ path, ...fileSource(file) }));
    } else if (m.source === "opfs") {
      const s = await opfsSources();
      if (!s) return postMessage({ type: "none" });
      sources = s;
    } else if (m.source === "http") {
      const index = await (await fetch(`${m.base}/index.json`)).json();
      sources = httpSources(m.base, index);
    } else throw new Error(`unknown source ${m.source}`);
    postMessage({ type: "ready", files: sources.map(({ path, size }) => [path, size]) });
  } catch (e) {
    return postMessage({ type: "error", message: String(e?.message ?? e) });
  }
  serve(m.ctl, m.data); // never returns: this worker only answers reads from here on
};
