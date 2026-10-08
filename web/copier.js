// SPDX-License-Identifier: GPL-3.0-or-later
// Saves a copy of the files the client reads into the browser's origin-private file system (OPFS), so the next visit
// needs no folder pick (Safari and Firefox cannot remember a picked folder). The copy stays in this browser; nothing
// is uploaded. It is resumable: a file already saved at full size is skipped, and the reader only uses the copy once
// the manifest says it is complete.

const COPY_DIR = "cod4e-install";
const BLOCK = 16 << 20;

self.onmessage = async ({ data: m }) => {
  if (m.type !== "copy") return;
  try {
    await copy(m.entries);
  } catch (e) {
    const quota = e?.name === "QuotaExceededError";
    postMessage({ type: "error", quota, message: quota ? "the browser has no room for a saved copy" : String(e?.message ?? e) });
  }
};

async function copy(entries) {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(COPY_DIR, { create: true });
  const total = entries.reduce((n, [, f]) => n + f.size, 0);
  const manifest = { version: 1, complete: false, files: entries.map(([path, f]) => ({ path, size: f.size })) };
  await writeManifest(dir, manifest);
  const sync = new FileReaderSync();
  let done = 0, last = 0;
  for (const [path, file] of entries) {
    const name = encodeURIComponent(path);
    const fh = await dir.getFileHandle(name, { create: true });
    if ((await fh.getFile()).size === file.size) {
      done += file.size;
      continue;
    }
    const h = await fh.createSyncAccessHandle();
    try {
      h.truncate(0);
      for (let at = 0; at < file.size; at += BLOCK) {
        const block = new Uint8Array(sync.readAsArrayBuffer(file.slice(at, at + BLOCK)));
        h.write(block, { at });
        done += block.length;
        const now = performance.now();
        if (now - last > 250) {
          last = now;
          postMessage({ type: "progress", done, total, path });
        }
      }
      h.flush();
    } finally {
      h.close();
    }
  }
  manifest.complete = true;
  await writeManifest(dir, manifest);
  postMessage({ type: "done", total });
}

async function writeManifest(dir, manifest) {
  const h = await (await dir.getFileHandle("manifest.json", { create: true })).createSyncAccessHandle();
  try {
    const bytes = new TextEncoder().encode(JSON.stringify(manifest));
    h.truncate(0);
    h.write(bytes, { at: 0 });
    h.flush();
  } finally {
    h.close();
  }
}
