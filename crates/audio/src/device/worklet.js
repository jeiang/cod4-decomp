// SPDX-License-Identifier: GPL-3.0-or-later
// The AudioWorklet half of the browser output; the protocol is documented in transport.rs.
// processorOptions: { capacity, ring?: SharedArrayBuffer, ctrl?: SharedArrayBuffer }. Without `ring` the
// processor keeps its own ring and fills it from posted Float32Array chunks (interleaved L R).
const WRITE = 0, READ = 1, UNDERRUNS = 2;
const REPORT_EVERY = 8; // quanta between status messages (~21 ms at 48 kHz)

class Cod4eMixer extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const o = options.processorOptions;
    this.mask = o.capacity - 1;
    this.ring = o.ring ? new Float32Array(o.ring) : new Float32Array(o.capacity * 2);
    this.ctrl = o.ctrl ? new Int32Array(o.ctrl) : new Int32Array(4);
    this.started = false;
    this.peak = 0;
    this.crossings = 0;
    this.last = 0;
    this.quanta = 0;
    this.port.onmessage = (e) => this.push(e.data);
  }

  push(chunk) {
    const w = Atomics.load(this.ctrl, WRITE);
    const free = this.mask + 1 - ((w - Atomics.load(this.ctrl, READ)) | 0);
    const n = Math.min(chunk.length >> 1, free);
    for (let f = 0; f < n; f++) {
      const slot = ((w + f) & this.mask) * 2;
      this.ring[slot] = chunk[2 * f];
      this.ring[slot + 1] = chunk[2 * f + 1];
    }
    Atomics.store(this.ctrl, WRITE, (w + n) | 0);
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    const l = out[0], r = out[1] || out[0];
    const n = l.length;
    const w = Atomics.load(this.ctrl, WRITE);
    if (!this.started && w === 0) return true;
    this.started = true;
    const rd = Atomics.load(this.ctrl, READ);
    const take = Math.min((w - rd) | 0, n);
    let peak = this.peak, last = this.last, crossings = this.crossings;
    for (let f = 0; f < take; f++) {
      const slot = ((rd + f) & this.mask) * 2;
      const a = this.ring[slot], b = this.ring[slot + 1];
      l[f] = a;
      r[f] = b;
      const m = Math.max(Math.abs(a), Math.abs(b));
      if (m > peak) peak = m;
      if (a !== 0) {
        if (last !== 0 && (a > 0) !== (last > 0)) crossings++;
        last = a;
      }
    }
    this.peak = peak; this.last = last; this.crossings = crossings;
    if (take < n) {
      // The output buffers start zeroed; an underrun leaves the rest silent.
      Atomics.store(this.ctrl, UNDERRUNS, (Atomics.load(this.ctrl, UNDERRUNS) + 1) | 0);
    }
    Atomics.store(this.ctrl, READ, (rd + take) | 0);
    if (++this.quanta % REPORT_EVERY === 0) {
      this.port.postMessage([
        Atomics.load(this.ctrl, READ) >>> 0,
        Atomics.load(this.ctrl, UNDERRUNS) >>> 0,
        this.peak,
        this.crossings,
      ]);
    }
    return true;
  }
}

registerProcessor("cod4e-mixer", Cod4eMixer);
