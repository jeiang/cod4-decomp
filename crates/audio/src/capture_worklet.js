// SPDX-License-Identifier: GPL-3.0-only
// The AudioWorklet half of the browser microphone: posts each 128-frame quantum of the input, folded to mono, to
// the main thread, which reduces it to the voice codec's rate (see capture.rs).
class Cod4eMic extends AudioWorkletProcessor {
  process(inputs) {
    const channels = inputs[0];
    if (channels && channels.length) {
      const n = channels[0].length;
      const mono = new Float32Array(n);
      for (const ch of channels) {
        for (let i = 0; i < n; i++) mono[i] += ch[i];
      }
      if (channels.length > 1) {
        for (let i = 0; i < n; i++) mono[i] /= channels.length;
      }
      this.port.postMessage(mono, [mono.buffer]);
    }
    return true;
  }
}

registerProcessor("cod4e-mic", Cod4eMic);
