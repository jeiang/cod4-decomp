// SPDX-License-Identifier: GPL-3.0-or-later
// The browser's datagram transport to a cod4e server: WebTransport, carrying the same datagrams a UDP client
// exchanges. The framing is the server's (crates/net/src/wt.rs): a datagram that fits is one WebTransport datagram;
// a longer one (up to 8192 bytes) is one unidirectional stream carrying exactly its bytes, ended by FIN; both forms
// are accepted from the server. The wasm client polls this synchronously (`recv`) and never blocks on it.

const MAX_DATAGRAM = 1200; // what browsers allow; the session may allow less
const MAX_MESSAGE = 8192;
const QUEUE_LIMIT = 512; // received messages kept for the client; older arrivals beyond it are dropped, like a full socket

const fromBase64 = (b64) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

export function createTransport() {
  let session = null;
  let writer = null;
  let state = "idle"; // idle, connecting, open, closed
  let error = "";
  const inbox = [];
  let limit = MAX_DATAGRAM;
  const stats = { sent: 0, received: 0, streamsOut: 0, streamsIn: 0, dropped: 0 };

  const push = (bytes) => {
    if (inbox.length >= QUEUE_LIMIT) return void stats.dropped++;
    inbox.push(bytes);
    stats.received++;
  };

  async function readDatagrams(s) {
    const reader = s.datagrams.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return;
      push(value);
    }
  }

  async function readStream(stream) {
    const reader = stream.getReader();
    const parts = [];
    let n = 0;
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      n += value.length;
      if (n > MAX_MESSAGE) return void reader.cancel(); // longer than any message: discard
      parts.push(value);
    }
    const all = new Uint8Array(n);
    let at = 0;
    for (const p of parts) { all.set(p, at); at += p.length; }
    stats.streamsIn++;
    push(all);
  }

  async function readStreams(s) {
    const streams = s.incomingUnidirectionalStreams.getReader();
    for (;;) {
      const { value, done } = await streams.read();
      if (done) return;
      readStream(value).catch(() => {});
    }
  }

  return {
    stats,
    // `target` is "host:port" or an https URL; `hash` the base64 SHA-256 of a self-signed certificate (or "").
    open(target, hash) {
      if (session) session.close();
      inbox.length = 0;
      state = "connecting";
      error = "";
      const url = /^https:\/\//.test(target) ? target : `https://${target}/`;
      const options = hash ? { serverCertificateHashes: [{ algorithm: "sha-256", value: fromBase64(hash) }] } : {};
      try {
        session = new WebTransport(url, options);
      } catch (e) {
        state = "closed";
        error = String(e?.message ?? e);
        return;
      }
      const s = session;
      s.ready.then(() => {
        if (s !== session) return;
        // Safari 27 has the older spec's createWritable() and no `writable`.
        writer = (s.datagrams.writable ?? s.datagrams.createWritable()).getWriter();
        limit = Math.min(MAX_DATAGRAM, s.datagrams.maxDatagramSize || MAX_DATAGRAM);
        state = "open";
        readDatagrams(s).catch(() => {});
        readStreams(s).catch(() => {});
      }).catch((e) => {
        if (s !== session) return;
        state = "closed";
        error = String(e?.message ?? e);
      });
      s.closed.then(() => { if (s === session) state = "closed"; }).catch((e) => {
        if (s === session) { state = "closed"; error ||= String(e?.message ?? e); }
      });
    },
    close() {
      session?.close();
      session = null;
      state = "closed";
    },
    state: () => state,
    error: () => error,
    // Fire and forget: loss is normal. Before the session is open the datagram is dropped.
    send(bytes) {
      if (state !== "open" || !session) return;
      stats.sent++;
      if (bytes.length <= limit) {
        writer.write(bytes.slice()).catch(() => {});
      } else if (bytes.length <= MAX_MESSAGE) {
        stats.streamsOut++;
        session.createUnidirectionalStream().then(async (stream) => {
          const w = stream.getWriter();
          await w.write(bytes.slice());
          await w.close();
        }).catch(() => {});
      }
    },
    // The next received message copied into dst, as its length; -1 when there is none.
    recv(dst) {
      const m = inbox.shift();
      if (!m) return -1;
      const n = Math.min(m.length, dst.length);
      dst.set(m.subarray(0, n));
      return n;
    },
  };
}
