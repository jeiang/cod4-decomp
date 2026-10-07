// SPDX-License-Identifier: GPL-3.0-or-later
// Runs the wasm renderer: file reads (FileReaderSync) and the OffscreenCanvas both live in a worker.
import init, { Spike } from "./pkg/web.js";

// wgpu reports validation errors through the console, which the page cannot see from a worker.
for (const level of ["warn", "error"]) {
  const orig = console[level].bind(console);
  console[level] = (...a) => { orig(...a); postMessage({ type: "log", level, text: a.map(String).join(" ") }); };
}

const WINDOW = 600; // frames in the "recent" statistics

const pct = (xs, p) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
const summary = (xs) => ({ n: xs.length, p50: pct(xs, 50), p99: pct(xs, 99), max: pct(xs, 100) });

// The page's requestAnimationFrame drives the frames: Safari never fires one inside a worker.
let spike = null, capturing = false, ticker = null;

self.onmessage = async ({ data }) => {
  if (data.type === "capture" && spike && !capturing) {
    capturing = true; // frame() cannot run while capture() holds the renderer
    try {
      const pixels = await spike.capture(data.t);
      postMessage({ type: "shot", pixels, width: 1280, height: 720 }, [pixels.buffer]);
    } catch (e) {
      postMessage({ type: "error", message: String(e?.stack ?? e) });
    }
    capturing = false;
  }
  if (data.type === "tick") {
    try {
      ticker?.(data.now);
    } catch (e) {
      postMessage({ type: "error", message: `frame: ${e?.stack ?? e}` });
    }
    postMessage({ type: "ack" });
  }
  if (data.type !== "start") return;
  const t0 = performance.now();
  try {
    const wasm = await init();
    const compile = performance.now() - t0;
    postMessage({ type: "phase", name: "fetch + compile wasm", ms: compile });
    spike = await Spike.load(
      data.canvas, data.paths, data.files, data.map, data.backend,
      (name, ms) => postMessage({ type: "phase", name, ms }),
    );
    const loadMs = performance.now() - t0;
    const cpu = [], interval = [];
    let start = null, last = null, frames = 0, lastPost = 0, worst = 0;
    ticker = (now) => {
      start ??= now;
      const ms = capturing ? 0 : spike.frame((now - start) / 1000);
      if (last !== null) interval.push(now - last);
      last = now;
      cpu.push(ms);
      worst = Math.max(worst, ms);
      frames++;
      if (now - lastPost > 500) {
        lastPost = now;
        const report = JSON.parse(spike.report());
        postMessage({
          type: "stats",
          loadMs, frames, raf: "page rAF",
          cpu: summary(cpu.slice(-WINDOW)), cpuAll: summary(cpu.slice(1)),
          interval: summary(interval.slice(-WINDOW)), intervalAll: summary(interval),
          gpu_ms: report.gpu_timestamps ? report.gpu_ms : null,
          report,
        });
      }
    };
  } catch (e) {
    postMessage({ type: "error", message: String(e?.stack ?? e) });
  }
};
