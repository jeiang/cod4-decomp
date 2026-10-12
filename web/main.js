// SPDX-License-Identifier: GPL-3.0-only
// The page: finds the user's install (a saved copy in this browser, or a folder the user picks), keeps a copy for
// next time, and starts the wasm client. The files are read in this browser only; nothing is uploaded.
import { createBridge } from "./bridge.js";
import { createTransport } from "./transport.js";
import { choose, isLanguageDir, wanted } from "./install.js";

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);

// Which files of the install the client reads, and in which language: see install.js.
const ci = (s) => s.toLowerCase();

// ---- picking ------------------------------------------------------------------------------------------------

const isDirName = (name, depth, prefix) =>
  (depth === 0 && (ci(name) === "main" || ci(name) === "zone")) ||
  (depth === 1 && ci(prefix) === "zone/" && isLanguageDir(name));

async function walkHandle(dir, prefix, depth, out) {
  for await (const [name, h] of dir.entries()) {
    const path = prefix + name;
    if (h.kind === "file") {
      if (wanted(path)) out.push([path, await h.getFile()]);
    } else if (isDirName(name, depth, prefix)) {
      await walkHandle(h, path + "/", depth + 1, out);
    }
  }
}

const readAll = (reader) => new Promise((res, rej) => {
  const all = [];
  const next = () => reader.readEntries((es) => (es.length ? (all.push(...es), next()) : res(all)), rej);
  next();
});

async function walkEntry(dir, prefix, depth, out) {
  for (const e of await readAll(dir.createReader())) {
    const path = prefix + e.name;
    if (e.isFile) {
      if (wanted(path)) out.push([path, await new Promise((res, rej) => e.file(res, rej))]);
    } else if (isDirName(e.name, depth, prefix)) {
      await walkEntry(e, path + "/", depth + 1, out);
    }
  }
}

// <input webkitdirectory>: webkitRelativePath is "<picked folder>/<path>".
const fromInput = (files) =>
  [...files].map((f) => [f.webkitRelativePath.split("/").slice(1).join("/"), f]).filter(([p]) => wanted(p));

const PREF = "cod4e:"; // localStorage key prefix of the settings

// ---- state ----------------------------------------------------------------------------------------------------

const bridge = createBridge();
const wire = createTransport();
// A closed or reloaded tab ends its session at once; the server frees the player's slot when it sees it close.
addEventListener("pagehide", () => wire.close());
let reader = null;
let copier = null;
let saver = null;
let storageFree = -1;
let noticeTimer = 0;

// A few words over the game for the player, which go away on their own.
function notice(text) {
  $("progress").textContent = text;
  $("progress").hidden = !text;
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => { $("progress").hidden = true; }, 8000);
}

// The worker that keeps downloaded files; what it cannot keep it says, and the player is told.
function getSaver() {
  if (!saver) {
    saver = new Worker(new URL("./saver.js", import.meta.url), { type: "module" });
    saver.onmessage = ({ data }) => {
      if (data.type === "error") notice(`Could not keep ${data.path || "a downloaded file"} for next time: ${data.message}`);
    };
  }
  return saver;
}
const savedFiles = []; // the downloaded files this visit has kept (the run report lists them)
let install = null; // [[path, size], ...] once a reader is ready

const status = (text, cls = "") => {
  $("status").textContent = text;
  $("status").className = cls;
};

function newReader() {
  reader?.terminate();
  reader = new Worker(new URL("./reader.js", import.meta.url), { type: "module" });
  return reader;
}

function ask(worker, message, transfer) {
  return new Promise((resolve) => {
    worker.onmessage = ({ data }) => resolve(data);
    worker.postMessage(message, transfer);
  });
}

async function openInstall(source, extra) {
  const r = await ask(newReader(), { type: "open", source, ctl: bridge.ctl, data: bridge.data, ...extra });
  if (r.type === "ready") {
    install = r.files;
    // The kept downloads this visit uses count as used, so the least recently used are the ones dropped for room.
    if (r.downloads?.length) getSaver().postMessage({ type: "touch", paths: r.downloads });
    return true;
  }
  if (r.type === "error") status(r.message, "bad");
  return false;
}

function ready(how) {
  $("play").disabled = false;
  status(how);
  if (params.has("autostart")) play();
}

async function pick(found) {
  if (!found.length) return status("That folder has none of the game's files. Pick the Call of Duty 4 install folder.", "bad");
  // The install's language is in its localization.txt; the client reads that language's zones.
  const loc = found.find(([p]) => ci(p) === "localization.txt");
  const { language, entries, missing } = choose(found, loc ? await loc[1].text() : "");
  if (missing.length) return status(`That folder lacks ${missing.join(", ")}. Pick the Call of Duty 4 install folder.`, "bad");
  found = entries;
  if (!(await openInstall("files", { entries: found }))) return;
  ready(`Install found (${found.length} files, ${language}).`);
  if ($("keep").checked) keepCopy(found);
}

function keepCopy(found) {
  if (!navigator.storage?.getDirectory) return;
  navigator.storage.persist?.().catch(() => {});
  copier?.terminate();
  copier = new Worker(new URL("./copier.js", import.meta.url), { type: "module" });
  const total = found.reduce((n, [, f]) => n + f.size, 0);
  copier.onmessage = ({ data }) => {
    if (data.type === "progress") {
      $("copy").textContent = `Saving a copy in this browser: ${(data.done / 2 ** 30).toFixed(2)} of ${(total / 2 ** 30).toFixed(2)} GiB`;
    } else if (data.type === "done") {
      $("copy").textContent = "A copy is saved in this browser; next time you will not need to pick the folder.";
      $("forget").hidden = false;
    } else if (data.type === "error") {
      $("copy").textContent = data.quota
        ? "This browser has no room for a saved copy, so you will pick the folder each visit."
        : `Saving a copy failed: ${data.message}`;
    }
  };
  copier.postMessage({ type: "copy", entries: found });
}

// ---- starting ---------------------------------------------------------------------------------------------------

let overlay = {};
const frames = { count: 0, last: 0, worst: 0 };

function clientArgs() {
  const a = [];
  const add = (flag, v) => v && a.push(flag, v);
  add("--map", $("map").value.trim());
  add("--backend", $("backend").value);
  add("--connect", $("server").value.trim());
  add("--name", $("name").value.trim());
  if (params.has("flythrough")) a.push("--flythrough", "--duration", "100000");
  if (params.has("args")) a.push(...params.get("args").split(/\s+/).filter(Boolean));
  return a;
}

function showOverlay() {
  const rows = Object.entries(overlay).map(([k, v]) => `${k}: ${v}`);
  $("overlay").textContent = rows.join("\n");
}

async function play() {
  if (!install) return;
  try {
    const { quota, usage } = await navigator.storage.estimate();
    if (quota) storageFree = Math.max(0, quota - (usage ?? 0));
  } catch {}
  $("play").disabled = true;
  $("setup").hidden = true;
  $("cod4e-canvas").hidden = false;
  const started = performance.now();
  const t = {};
  globalThis.cod4 = {
    read_into: (id, offset, dst) => bridge.read(id, offset, dst),
    net_open: (target) => wire.open(target, $("cert").value.trim()),
    net_send: (bytes) => wire.send(bytes),
    net_recv: (dst) => wire.recv(dst),
    net_state: () => wire.state(),
    net_error: () => wire.error(),
    net_close: () => wire.close(),
    config: () => JSON.stringify({ args: clientArgs(), files: install }),
    // The settings (a config_mp.cfg per profile) are kept in this browser, written the moment the client changes them.
    pref_get: (key) => {
      try { return localStorage.getItem(PREF + key) ?? undefined; } catch { return undefined; }
    },
    pref_set: (key, value) => localStorage.setItem(PREF + key, value),
    download_progress: (text) => {
      $("progress").textContent = text;
      $("progress").hidden = !text;
    },
    // A file the client downloaded from the server it joins (a map): kept for the next visit (saver.js).
    // What the browser will still let this page store (-1: it does not say), for the client to check before a download.
    storage_free: () => storageFree,
    notice,
    save_download: (path, bytes) => {
      savedFiles.push(path);
      const saver = getSaver();
      const copy = bytes.slice(); // the view is of wasm memory, which lives on after this call
      saver.postMessage({ type: "save", path, bytes: copy }, [copy.buffer]);
    },
    fatal: (message) => {
      $("setup").hidden = false;
      $("cod4e-canvas").hidden = true;
      $("play").disabled = false;
      status(message, "bad");
      $("overlay").hidden = true;
      window.__cod4 = { ...window.__cod4, error: message };
    },
    overlay: (json) => {
      overlay = JSON.parse(json);
      t.firstOverlay ??= performance.now() - started;
      overlay["load, play click to first frame (ms)"] = Math.round(t.firstOverlay);
      overlay["net"] = `${wire.state()} sent ${wire.stats.sent} received ${wire.stats.received} streams out/in ${wire.stats.streamsOut}/${wire.stats.streamsIn}`;
      overlay["page"] = navigator.userAgent;
      // The worklet node exists as soon as the sound starts but plays nothing until the AudioContext is resumed by a
      // gesture: the hint shows while a started output's frames_played stands still.
      const played = /frames_played (\d+)/.exec(String(overlay.sound ?? ""));
      const now = played ? Number(played[1]) : null;
      $("sound-hint").hidden = now === null || now !== t.played;
      t.played = now;
      showOverlay();
      window.__cod4 = { ...window.__cod4, overlay, startedAt: started };
      // Joined to a server: the run counts from the moment the server put the player in the world.
      if (params.has("connect") && overlay["net phase"] === "spawned") {
        t.spawnedFrames ??= overlay.frames ?? 0;
        t.spawnedMs ??= Math.round(performance.now() - started);
      }
      overlay["spawned at (ms)"] = t.spawnedMs ?? "";
      const reportFrames = params.has("connect") ? (t.spawnedFrames ?? Infinity) : 0;
      if (params.has("report") && !t.reported && (overlay.frames ?? 0) - reportFrames >= Number(params.get("report") || 600)) {
        t.reported = true;
        fetch("/report", { method: "POST", body: JSON.stringify({ ua: navigator.userAgent, overlay, saved: savedFiles }) }).catch(() => {});
      }
    },
  };
  $("overlay").hidden = !params.has("debug");
  if (!params.has("debug")) {
    $("overlay").hidden = false;
    setTimeout(() => { if (!overlayPinned) $("overlay").hidden = true; }, 8000);
  }
  try {
    const mod = await import("./pkg/cod4e.js");
    await mod.default();
  } catch (e) {
    globalThis.cod4.fatal(`the client could not start: ${e?.message ?? e}`);
  }
}

let overlayPinned = params.has("debug");
addEventListener("keydown", (e) => {
  if (e.code === "F3") {
    e.preventDefault();
    overlayPinned = true;
    $("overlay").hidden = !$("overlay").hidden;
  }
});

// ---- boot ---------------------------------------------------------------------------------------------------------

async function boot() {
  const webgpu = Boolean(navigator.gpu);
  $("gpu").textContent = webgpu ? "WebGPU is available; it will be used." : "No WebGPU in this browser; WebGL2 will be used.";
  if (params.has("map")) $("map").value = params.get("map");
  if (params.has("backend")) $("backend").value = params.get("backend");
  if (params.has("connect")) $("server").value = params.get("connect");
  if (params.has("cert")) $("cert").value = params.get("cert");
  // A server started with --wt-info writes {"url", "certHashSha256Base64"}; ?wt-info=<where the page can fetch it>.
  if (params.has("wt-info")) {
    try {
      const info = await (await fetch(params.get("wt-info"))).json();
      $("server").value = info.url;
      $("cert").value = info.certHashSha256Base64 ?? "";
    } catch (e) {
      status(`cannot read ${params.get("wt-info")}: ${e?.message ?? e}`, "bad");
    }
  }
  if (params.has("name")) $("name").value = params.get("name");
  // Automated runs can start with settings already kept: ?seed=<JSON object of settings key to config text>.
  if (params.has("seed")) {
    for (const [key, text] of Object.entries(JSON.parse(params.get("seed")))) localStorage.setItem(PREF + key, text);
  }
  $("play").onclick = play;
  if (!self.crossOriginIsolated) {
    return status("This page must be served with cross-origin isolation headers (COOP and COEP); see web/serve.py.", "bad");
  }
  $("dir").onchange = (e) => pick(fromInput(e.target.files));
  if (window.showDirectoryPicker) {
    $("browse").hidden = false;
    $("browse").onclick = async () => {
      try {
        const root = await showDirectoryPicker({ id: "cod4", mode: "read" });
        const out = [];
        await walkHandle(root, "", 0, out);
        pick(out);
      } catch (e) {
        if (e?.name !== "AbortError") status(String(e?.message ?? e), "bad");
      }
    };
  }
  const drop = $("drop");
  drop.ondragover = (e) => { e.preventDefault(); drop.classList.add("over"); };
  drop.ondragleave = () => drop.classList.remove("over");
  drop.ondrop = async (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    const item = e.dataTransfer.items[0];
    const out = [];
    const handle = item.getAsFileSystemHandle ? await item.getAsFileSystemHandle() : null;
    if (handle?.kind === "directory") await walkHandle(handle, "", 0, out);
    else await walkEntry(item.webkitGetAsEntry(), "", 0, out);
    pick(out);
  };
  if (navigator.storage?.getDirectory) {
    $("forget-downloads").hidden = false;
    $("forget-downloads").onclick = async () => {
      try { await (await navigator.storage.getDirectory()).removeEntry("cod4e-downloads", { recursive: true }); } catch {}
      status("The maps downloaded to this browser are deleted. Reload the page to use the install only.");
    };
  }
  $("forget").onclick = async () => {
    reader?.terminate();
    install = null;
    $("play").disabled = true;
    for (const dir of ["cod4e-install", "cod4e-downloads"]) {
      try { await (await navigator.storage.getDirectory()).removeEntry(dir, { recursive: true }); } catch {}
    }
    $("forget").hidden = true;
    $("copy").textContent = "";
    status("The saved copy and the maps downloaded to this browser are deleted. Pick the install folder again.");
  };
  // Automated runs read the install from a development server instead of a picker (web/serve.py --install).
  if (params.has("dev-install")) {
    if (await openInstall("http", { base: params.get("dev-install") })) ready("Install read from the development server.");
    return;
  }
  if (navigator.storage?.getDirectory && await openInstall("opfs")) {
    $("forget").hidden = false;
    return ready("Using the copy saved in this browser.");
  }
  status("Pick your Call of Duty 4 install folder. It is read in this browser and never uploaded.");
}

boot();
