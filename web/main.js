// SPDX-License-Identifier: GPL-3.0-or-later
// Gathers the picked install folder's files (File handles, never uploaded) and hands them to the worker.
const $ = (id) => document.getElementById(id);

// Only these are needed; the folder holds gigabytes of everything else.
const wanted = (path) => /^main\/[^/]+\.iwd$/i.test(path) || /^zone\/english\/[^/]+\.ff$/i.test(path);
const ci = (a, b) => a.toLowerCase() === b.toLowerCase();

async function walkHandle(dir, prefix, depth, out) {
  for await (const [name, h] of dir.entries()) {
    const path = prefix + name;
    if (h.kind === "file") {
      if (wanted(path)) out.push([path, await h.getFile()]);
    } else if ((depth === 0 && ci(name, "main")) || (depth === 0 && ci(name, "zone")) || (depth === 1 && prefix.toLowerCase() === "zone/" && ci(name, "english"))) {
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
    } else if ((depth === 0 && (ci(e.name, "main") || ci(e.name, "zone"))) || (depth === 1 && prefix.toLowerCase() === "zone/" && ci(e.name, "english"))) {
      await walkEntry(e, path + "/", depth + 1, out);
    }
  }
}

function fromInput(files) {
  // webkitRelativePath is "<picked folder>/<path>".
  return [...files]
    .map((f) => [f.webkitRelativePath.split("/").slice(1).join("/"), f])
    .filter(([p]) => wanted(p));
}

let worker = null;
async function start(entries) {
  if (worker) worker.terminate();
  $("err").textContent = "";
  const need = ["main/iw_00.iwd", `zone/english/${$("map").value}.ff`, "zone/english/common_mp.ff"];
  const have = new Set(entries.map(([p]) => p.toLowerCase()));
  const missing = need.filter((n) => !have.has(n));
  if (missing.length) {
    $("err").textContent = `That folder lacks ${missing.join(", ")}; pick the CoD4 install root.`;
    return;
  }
  $("stage").replaceChildren();
  const canvas = Object.assign(document.createElement("canvas"), { width: 1280, height: 720 });
  $("stage").append(canvas);
  const offscreen = canvas.transferControlToOffscreen();
  const t0 = performance.now();
  const rows = new Map();
  const state = (window.__spike = { phases: [], stats: null, error: null, started: t0 });
  const render = () => {
    const s = state.stats;
    const f = (x) => (x == null ? "-" : x.toFixed(2));
    const lines = [
      ["browser", navigator.userAgent],
      ["cross-origin isolated", String(self.crossOriginIsolated)],
      ...(s ? [
        ["backend", `${s.report.backend} (${s.report.adapter})`],
        ["BC textures on GPU", String(s.report.bc_textures)],
        ["load, click to first frame", `${s.loadMs.toFixed(0)} ms`],
        ["wasm memory (peak)", `${(s.report.wasm_memory_bytes / 2 ** 20).toFixed(0)} MiB`],
        ["frames", `${s.frames} (${s.raf})`],
        ["CPU ms/frame, last 600: p50 / p99 / max", `${f(s.cpu.p50)} / ${f(s.cpu.p99)} / ${f(s.cpu.max)}`],
        ["CPU ms/frame, all: p50 / p99 / max", `${f(s.cpuAll.p50)} / ${f(s.cpuAll.p99)} / ${f(s.cpuAll.max)}`],
        ["frame interval ms, last 600: p50 / p99 / max", `${f(s.interval.p50)} / ${f(s.interval.p99)} / ${f(s.interval.max)}`],
        ["frame interval ms, all: p50 / p99 / max", `${f(s.intervalAll.p50)} / ${f(s.intervalAll.p99)} / ${f(s.intervalAll.max)}`],
        ["GPU ms (timestamp query)", f(s.gpu_ms)],
        ["last frame", `${s.report.surfaces} surfaces, ${s.report.draws} draws, ${s.report.models} models, ${s.report.pipelines_missing} pipelines missing`],
        ["pipelines built / textures failed", `${s.report.pipelines} / ${s.report.textures_failed}`],
      ] : []),
      ...(state.log ?? []).slice(0, 8).map((l) => ["console", l.slice(0, 400)]),
      ...state.phases.map(([n, ms]) => [`load: ${n}`, `${ms.toFixed(0)} ms`]),
    ];
    $("stats").replaceChildren(...lines.map(([k, v]) => {
      const tr = document.createElement("tr");
      tr.append(Object.assign(document.createElement("th"), { textContent: k }), Object.assign(document.createElement("td"), { textContent: v }));
      return tr;
    }));
  };
  let inflight = 0;
  const tick = (now) => {
    if (worker !== w) return;
    if (inflight < 2) {
      inflight++;
      w.postMessage({ type: "tick", now });
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  const w = (worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" }));
  worker.onmessage = ({ data }) => {
    if (data.type === "ack") { inflight--; return; }
    if (data.type === "log") { (state.log ??= []).push(`${data.level}: ${data.text}`); }
    else if (data.type === "phase") state.phases.push([data.name, data.ms]);
    else if (data.type === "stats") {
      state.stats = data;
      // Run statistics only, never file content; for browsers that cannot be scripted (see serve.py).
      if (new URLSearchParams(location.search).has("report") && data.frames > 1800 && !state.reported) {
        state.reported = true;
        fetch("/report", { method: "POST", body: JSON.stringify({ ua: navigator.userAgent, phases: state.phases, log: state.log, stats: data }) });
      }
    }
    else if (data.type === "shot") {
      const still = $("still");
      still.hidden = false;
      still.getContext("2d").putImageData(new ImageData(new Uint8ClampedArray(data.pixels.buffer), data.width, data.height), 0, 0);
      state.shot = true;
    } else if (data.type === "error") { state.error = data.message; $("err").textContent = data.message; }
    render();
  };
  worker.postMessage({
    type: "start", canvas: offscreen, map: $("map").value, backend: new URLSearchParams(location.search).get("backend") ?? $("backend").value,
    paths: entries.map(([p]) => p), files: entries.map(([, f]) => f),
  }, [offscreen]);
}

$("shot").onclick = () => worker?.postMessage({ type: "capture", t: (performance.now() - window.__spike.started) / 1000 });

// `?backend=webgpu|webgl` preselects the backend, for browsers driven without a scripting API.
const wantBackend = new URLSearchParams(location.search).get("backend");
if (wantBackend) $("backend").value = wantBackend;

$("dir").onchange = (e) => start(fromInput(e.target.files));

if (window.showDirectoryPicker) {
  $("pick").hidden = false;
  $("pick").onclick = async () => {
    const root = await showDirectoryPicker({ id: "cod4", mode: "read" });
    const out = [];
    await walkHandle(root, "", 0, out);
    start(out);
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
  else await walkEntry(item.webkitGetAsEntry(), "", 0, out); // the dropped folder itself is the root
  start(out);
};
