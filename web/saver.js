// SPDX-License-Identifier: GPL-3.0-only
// Keeps the files the client downloaded from a server (a map the install lacked) in the browser's origin-private file
// system, so the next visit has them: reader.js adds them to the install it serves. They stay in this browser;
// nothing is uploaded.
//
// A save never harms what is already kept. A file is written under a name of its own (path and generation), checked
// for its size, and only then listed: the list is two files, manifest.a and manifest.b, written alternately with a
// rising `seq`, and the reader takes the newer one that parses, so a write that fails or is cut short leaves the
// older list whole. Files no list names are removed afterwards. The kept files are capped (MAX_KEPT): the least
// recently used are dropped first. Failures are posted to the page, which tells the player.

export const DOWNLOAD_DIR = "cod4e-downloads";
const MAX_KEPT = 1 << 30; // bytes kept across visits
const SLOTS = ["manifest.a", "manifest.b"];

self.onmessage = ({ data: m }) => {
  // One at a time: each save reads the list the one before it wrote.
  queue = queue.then(() => exclusive(() => (m.type === "save" ? save(m.path, m.bytes) : m.type === "touch" ? touch(m.paths) : null))).catch((e) => {
    const quota = e?.name === "QuotaExceededError";
    postMessage({ type: "error", path: m.path ?? "", message: quota ? "this browser has no room to keep it" : String(e?.message ?? e) });
  });
};
let queue = Promise.resolve();

// Another tab of this origin may be saving too: its list read, write and sweep must not interleave with ours.
const exclusive = (fn) => (navigator.locks ? navigator.locks.request(`${DOWNLOAD_DIR}-write`, fn) : fn());

async function readList(dir) {
  let best = { seq: 0, files: [] };
  for (const name of SLOTS) {
    try {
      const m = JSON.parse(await (await (await dir.getFileHandle(name)).getFile()).text());
      if (Array.isArray(m.files) && Number.isInteger(m.seq) && m.seq > best.seq) best = m;
    } catch {}
  }
  return best;
}

// Writes the list as generation `seq`, into the slot the previous generation does not occupy.
async function writeList(dir, seq, files) {
  const h = await (await dir.getFileHandle(SLOTS[seq % 2], { create: true })).createSyncAccessHandle();
  try {
    const bytes = new TextEncoder().encode(JSON.stringify({ version: 2, seq, files }));
    h.write(bytes, { at: 0 });
    h.truncate(bytes.length); // after the write: a failed write never leaves an emptied list
    h.flush();
  } finally {
    h.close();
  }
}

// Removes what no list names (an older generation, an evicted file, a write that failed).
async function sweep(dir, files) {
  const keep = new Set([...SLOTS, ...files.map((f) => f.file)]);
  for await (const name of dir.keys()) {
    if (!keep.has(name)) await dir.removeEntry(name).catch(() => {}); // one a reader still holds is left for later
  }
}

async function open() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(DOWNLOAD_DIR, { create: true });
}

async function save(path, bytes) {
  if (bytes.length > MAX_KEPT) throw new Error("it is larger than this page keeps");
  const dir = await open();
  const { seq, files: old } = await readList(dir);
  const gen = seq + 1;
  const file = `${encodeURIComponent(path)}~${gen}`;
  // Room: the least recently used go first.
  const files = old.filter((f) => f.path !== path).sort((a, b) => a.used - b.used);
  let total = bytes.length + files.reduce((n, f) => n + f.size, 0);
  while (total > MAX_KEPT && files.length) total -= files.shift().size;
  const h = await (await dir.getFileHandle(file, { create: true })).createSyncAccessHandle();
  try {
    h.write(bytes, { at: 0 });
    h.flush();
    if (h.getSize() !== bytes.length) throw new Error("the browser stored a different size");
  } catch (e) {
    h.close();
    await dir.removeEntry(file).catch(() => {});
    throw e;
  }
  h.close();
  files.push({ path, size: bytes.length, file, used: Date.now() });
  await writeList(dir, gen, files); // the file counts only from here
  await sweep(dir, files);
  postMessage({ type: "saved", path });
}

// The files this visit's install uses were used now.
async function touch(paths) {
  const dir = await open();
  const { seq, files } = await readList(dir);
  const set = new Set(paths);
  if (!files.some((f) => set.has(f.path))) return;
  for (const f of files) if (set.has(f.path)) f.used = Date.now();
  await writeList(dir, seq + 1, files);
}
