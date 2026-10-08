// SPDX-License-Identifier: GPL-3.0-or-later
// The page's side of the install read bridge (the reader worker's side is reader.js): a synchronous ranged read that
// the wasm module calls. The wasm module owns the main thread and cannot await, and the reader worker is the only
// place a File or an OPFS file can be read synchronously, so the request goes through shared memory and this thread
// waits for the answer.

const IDLE = 0, REQUEST = 1, DONE = 2, FAILED = 3;
const DATA_BYTES = 4 << 20;

export function createBridge() {
  const ctl = new SharedArrayBuffer(32);
  const data = new SharedArrayBuffer(DATA_BYTES);
  const i32 = new Int32Array(ctl), f64 = new Float64Array(ctl, 16, 1), bytes = new Uint8Array(data);
  const utf8 = new TextDecoder();
  // Atomics.wait is refused on some main threads; spinning is the portable wait, and the answer is microseconds away.
  let canWait = true;
  const wait = () => {
    if (canWait) {
      try { return void Atomics.wait(i32, 0, REQUEST, 1); } catch { canWait = false; }
    }
  };
  return {
    ctl, data,
    // Fills dst (a Uint8Array view into wasm memory) from `offset` of install file `id`.
    read(id, offset, dst) {
      for (let done = 0; done < dst.length;) {
        const n = Math.min(dst.length - done, DATA_BYTES);
        i32[1] = id;
        i32[2] = n;
        f64[0] = offset + done;
        Atomics.store(i32, 0, REQUEST);
        Atomics.notify(i32, 0);
        while (Atomics.load(i32, 0) === REQUEST) wait();
        const state = Atomics.load(i32, 0);
        if (state === FAILED) {
          const msg = utf8.decode(bytes.slice(0, i32[2]));
          Atomics.store(i32, 0, IDLE);
          throw new Error(msg);
        }
        dst.set(bytes.subarray(0, n), done);
        Atomics.store(i32, 0, IDLE);
        done += n;
      }
    },
  };
}
