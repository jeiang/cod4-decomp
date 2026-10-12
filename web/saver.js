// SPDX-License-Identifier: GPL-3.0-only
// Keeps the files the client downloaded from a server (a map the install lacked) in the browser's origin-private file
// system, so the next visit has them: reader.js adds them to the install it serves. They stay in this browser;
// nothing is uploaded. The manifest lists the files that are whole, so a save cut short is never read.

export const DOWNLOAD_DIR = "cod4e-downloads";

self.onmessage = async ({ data: m }) => {
  if (m.type !== "save") return;
  try {
    await save(m.path, m.bytes);
  } catch (e) {
    postMessage({ type: "error", path: m.path, message: String(e?.message ?? e) });
  }
};

// One file at a time: the manifest is rewritten after each, and the messages arrive in order.
let queue = Promise.resolve();

function save(path, bytes) {
  queue = queue.then(() => write(path, bytes));
  return queue;
}

async function write(path, bytes) {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(DOWNLOAD_DIR, { create: true });
  let files = [];
  try {
    files = JSON.parse(await (await (await dir.getFileHandle("manifest.json")).getFile()).text()).files;
  } catch {}
  const h = await (await dir.getFileHandle(encodeURIComponent(path), { create: true })).createSyncAccessHandle();
  try {
    h.truncate(0);
    h.write(bytes, { at: 0 });
    h.flush();
  } finally {
    h.close();
  }
  files = files.filter((f) => f.path !== path);
  files.push({ path, size: bytes.length });
  const mh = await (await dir.getFileHandle("manifest.json", { create: true })).createSyncAccessHandle();
  try {
    const text = new TextEncoder().encode(JSON.stringify({ version: 1, files }));
    mh.truncate(0);
    mh.write(text, { at: 0 });
    mh.flush();
  } finally {
    mh.close();
  }
  postMessage({ type: "saved", path });
}
