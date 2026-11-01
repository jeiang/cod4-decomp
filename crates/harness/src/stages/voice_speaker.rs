// SPDX-License-Identifier: GPL-3.0-only
//! A second person on a running server who joins a team and talks: a real UDP client (no window) that answers the
//! join menus, sends commands, and sends a coded tone as voice frames ([`net::voice`]) for the server to relay like
//! any player's. Client stages use it to make the driven client hear someone.

use assets::vfs::Vfs;
use net::UdpTransport;
use net::client::NetClient;
use net::entity::etype;
use net::ui::AutoJoin;
use net::voice::{Encoder, FRAME_SAMPLES};
use sim::pm::UserCmd;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The speaker's player name, which the talker icons must show.
pub(super) const NAME: &str = "voicebot";
/// No client number yet.
const NONE: u32 = u32::MAX;

pub(super) struct Speaker {
    stop: Arc<AtomicBool>,
    own: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Speaker {
    /// Joins `addr` (an `sv_pure` server of the stock install at `install`) and talks until dropped.
    pub(super) fn start(addr: SocketAddr, install: &Path) -> Result<Self, String> {
        let vfs =
            Vfs::open_stock(install, 0).map_err(|e| format!("cannot open the install: {e}"))?;
        let pure = assets::vfs::pure_digest(&vfs.pure_iwds(install));
        let t = UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .map_err(|e| format!("cannot bind the speaker's socket: {e}"))?;
        let mut c = NetClient::new(t, addr, NAME, "", 6400);
        c.set_pure(pure);
        let stop = Arc::new(AtomicBool::new(false));
        let own = Arc::new(AtomicU32::new(NONE));
        let (s, o) = (stop.clone(), own.clone());
        let thread = std::thread::spawn(move || talk(c, &s, &o));
        Ok(Self {
            stop,
            own,
            thread: Some(thread),
        })
    }

    /// The speaker's client number once it has a body.
    pub(super) fn client(&self) -> Option<u16> {
        let n = self.own.load(Ordering::Relaxed);
        (n != NONE).then_some(n as u16)
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A 400 Hz tone at 8 kHz, a frame at a time.
fn tone(frame: u64) -> [i16; FRAME_SAMPLES] {
    let mut pcm = [0i16; FRAME_SAMPLES];
    for (i, s) in pcm.iter_mut().enumerate() {
        let n = frame * FRAME_SAMPLES as u64 + i as u64;
        let phase = n as f32 * 400.0 / net::voice::RATE as f32 * std::f32::consts::TAU;
        *s = (phase.sin() * 8000.0) as i16;
    }
    pcm
}

fn talk(mut c: NetClient<UdpTransport>, stop: &AtomicBool, own: &AtomicU32) {
    let mut join = AutoJoin::default();
    let mut enc = Encoder::default();
    let mut cmd_time = 0;
    let mut sent = 0u64;
    let mut next_voice = None;
    // One voice frame is 20 ms of audio: they go out on that clock, as a microphone gives them.
    while !stop.load(Ordering::Relaxed) {
        c.pump(Duration::from_millis(20));
        let events = c.ui().map(|ui| ui.drain_events()).unwrap_or_default();
        for ev in &events {
            if let Some(a) = join.step(ev) {
                c.command(&a);
            }
        }
        let Some(s) = c.latest() else { continue };
        let n = s.ps.client_num;
        let alive = s.entity(n).is_some_and(|e| e.etype == etype::PLAYER);
        if alive {
            own.store(u32::from(n), Ordering::Relaxed);
        }
        let now = c.now_ms();
        let Some(st) = c.snaps.server_time(now) else {
            continue;
        };
        cmd_time = (cmd_time + 1).max(st);
        if own.load(Ordering::Relaxed) != NONE {
            let due = next_voice.get_or_insert(now);
            while *due <= now {
                c.send_voice(enc.encode(&tone(sent)));
                sent += 1;
                *due += 20;
            }
        }
        c.send_cmd(UserCmd {
            server_time: cmd_time,
            ..UserCmd::default()
        });
    }
    c.disconnect();
    for _ in 0..5 {
        c.pump(Duration::from_millis(10));
    }
}
