// SPDX-License-Identifier: GPL-3.0-only
//! The harness's side of the client's dev control channel (#378): a real client started with `--control`, and the
//! requests a stage makes of it over the loopback socket, one line of JSON each. A stage that drives a match through
//! this module presses the player's own keys and buttons (the bind table, the console's field, the menus' pointer)
//! and reads the state document, so what it asserts is what the player sees and does (#376), not a hook's word.
//!
//! [`Driven`] is one such client. [`Walker`] is the stage's own player in a match: it reads the player's and the
//! entities' positions and turns toward the nearest living enemy with the mouse (or the arrow keys when the pointer
//! could not be locked), walks with the forward key and fires with the attack button, as a person's hands would.
use crate::stages::client_session::profile_args;
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A client the stage started with `--control 0`, and the socket its channel answers on.
pub struct Driven {
    child: Child,
    sock: BufReader<TcpStream>,
    id: u64,
    /// The directory the run writes to (`--out`): captures land here.
    pub dir: PathBuf,
    /// `host:port` of the channel.
    pub addr: String,
    /// Every step the stage took, with its outcome: the `steps` of the run's report.
    pub steps: Vec<Value>,
    began: Instant,
}

impl Driven {
    /// Starts the client with `args` (its own flags: `--listen`, `--bots`, `--duration`, ...) and the harness's
    /// window, sound and profile flags, waits for its channel to open and connects to it. Output lands in `dir`.
    pub fn start(client: &Path, install: &Path, dir: &Path, args: &[&str]) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let addr_file = dir.join("control.addr");
        let _ = fs::remove_file(&addr_file);
        let log = File::create(dir.join("client.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(client)
            .arg("--install")
            .arg(install)
            .args(["--size", "1280x720", "--no-sound"])
            .args(args)
            .args(["--control", "0", "--control-file"])
            .arg(&addr_file)
            .args(profile_args(&dir.join("config")))
            .arg("--out")
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let addr = loop {
            if let Ok(s) = fs::read_to_string(&addr_file)
                && let Some(port) = s.lines().next().and_then(|l| l.split(':').nth(1))
            {
                break format!("127.0.0.1:{port}");
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!(
                    "the client exited {status} before opening its control channel (see {}/client.log)",
                    dir.display()
                ));
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the client did not open its control channel".to_owned());
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        let sock = match TcpStream::connect(&addr) {
            Ok(s) => {
                s.set_read_timeout(Some(Duration::from_secs(240)))
                    .map_err(|e| e.to_string())?;
                BufReader::new(s)
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the channel at {addr} did not answer: {e}"));
            }
        };
        Ok(Self {
            child,
            sock,
            id: 0,
            dir: dir.to_owned(),
            addr,
            steps: Vec::new(),
            began: Instant::now(),
        })
    }

    /// One request and its reply: `Err` with the channel's message when the reply says `ok: false`.
    pub fn ask(&mut self, req: Value) -> Result<Value, String> {
        let reply = self.exchange(req)?;
        match reply["ok"].as_bool() {
            Some(true) => Ok(reply),
            _ => Err(reply["error"]
                .as_str()
                .unwrap_or("the request failed")
                .to_owned()),
        }
    }

    /// Sends one request line and reads its reply line, whatever it says.
    fn exchange(&mut self, mut req: Value) -> Result<Value, String> {
        self.id += 1;
        req["id"] = json!(self.id);
        let line = format!("{req}\n");
        self.sock
            .get_mut()
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut back = String::new();
        self.sock.read_line(&mut back).map_err(|e| e.to_string())?;
        if back.is_empty() {
            return Err("the channel closed".to_owned());
        }
        serde_json::from_str(&back).map_err(|e| format!("{e}: {back}"))
    }

    /// One section of the state document (`player`, `entities`, `hud`, ...), as the section's own object.
    pub fn state(&mut self, what: &str) -> Result<Value, String> {
        let mut doc = self.ask(json!({"cmd": "state", "what": what}))?;
        Ok(doc[what].take())
    }

    /// The value at a dotted path of the state document.
    pub fn lookup(&mut self, path: &str) -> Result<Value, String> {
        let section = path.split('.').next().unwrap_or(path);
        let doc = self.ask(json!({"cmd": "state", "what": section}))?;
        Ok(devctrl::lookup(&doc, path).cloned().unwrap_or(Value::Null))
    }

    /// One `wait`: `Some(reply)` once the state matches, `None` when the clock ran out first.
    pub fn wait(&mut self, state: &Value, timeout: Duration) -> Result<Option<Value>, String> {
        let reply = self.exchange(
            json!({"cmd": "wait", "state": state, "timeout_ms": timeout.as_millis() as u64}),
        )?;
        if reply["ok"].as_bool() == Some(true) {
            return Ok(Some(reply));
        }
        if reply["timeout"].as_bool() == Some(true) {
            return Ok(None);
        }
        Err(reply["error"]
            .as_str()
            .unwrap_or("the wait failed")
            .to_owned())
    }

    /// Waits up to `limit` for the state to match; the failure names what was observed last.
    pub fn until(&mut self, state: Value, limit: Duration, what: &str) -> Result<Value, String> {
        match self.wait(&state, limit)? {
            Some(reply) => Ok(reply),
            None => {
                let seen = self.observed(&state)?;
                Err(format!(
                    "{what}: not within {} s; observed {seen}",
                    limit.as_secs()
                ))
            }
        }
    }

    /// The values of the paths an expectation names, for a failure that says what was seen.
    fn observed(&mut self, state: &Value) -> Result<Value, String> {
        let mut out = serde_json::Map::new();
        if let Value::Object(m) = state {
            for path in m.keys() {
                out.insert(path.clone(), self.lookup(path)?);
            }
        }
        Ok(Value::Object(out))
    }

    /// Lets the client draw `frames` frames.
    pub fn frames(&mut self, frames: u64) -> Result<(), String> {
        self.ask(json!({"cmd": "step", "frames": frames}))?;
        Ok(())
    }

    /// Lets the clock run for `d`, the client drawing meanwhile.
    pub fn pause(&mut self, d: Duration) -> Result<(), String> {
        // A wait on a state nothing answers is the channel's clock: it costs frames, not a sleep of the stage.
        let _ = self.wait(&json!({"client.frames": {"lt": 0}}), d)?;
        Ok(())
    }

    /// Presses (`down`), releases (`up`) or taps the keys bound to `command` (`+forward`, `togglemenu`).
    pub fn bind(&mut self, command: &str, action: &str) -> Result<(), String> {
        self.ask(json!({"cmd": "key", "bind": command, "action": action}))?;
        Ok(())
    }

    /// Presses, releases or taps a key by its name (`w`, `escape`, `enter`).
    pub fn key(&mut self, name: &str, action: &str) -> Result<(), String> {
        self.ask(json!({"cmd": "key", "name": name, "action": action}))?;
        Ok(())
    }

    /// Presses or releases a mouse button (`mouse1`).
    pub fn button(&mut self, name: &str, action: &str) -> Result<(), String> {
        self.ask(json!({"cmd": "button", "name": name, "action": action}))?;
        Ok(())
    }

    /// Opens the console with its key, types `line`, sends it with Enter and closes the console again.
    pub fn console(&mut self, line: &str) -> Result<(), String> {
        self.key("backquote", "tap")?;
        self.until(
            json!({"console.open": true}),
            Duration::from_secs(5),
            "the console did not open",
        )?;
        self.ask(json!({"cmd": "text", "text": line}))?;
        self.key("enter", "tap")?;
        self.key("backquote", "tap")?;
        self.until(
            json!({"console.open": false}),
            Duration::from_secs(5),
            "the console did not close",
        )?;
        Ok(())
    }

    /// Types a chat line as a person does: the chat bind opens the field, the text goes in, Enter sends it.
    pub fn chat(&mut self, line: &str, team: bool) -> Result<(), String> {
        let bind = if team {
            "chatmodeteam"
        } else {
            "chatmodepublic"
        };
        self.bind(bind, "tap")?;
        self.until(
            json!({"console.open": true}),
            Duration::from_secs(5),
            "the chat field did not open",
        )?;
        self.ask(json!({"cmd": "text", "text": line}))?;
        self.key("enter", "tap")?;
        Ok(())
    }

    /// Clicks the visible item of the top menu named or labelled `want` (case-insensitive; `a|b` takes the first
    /// alternative that shows) where the state document put it: the pointer moves to the centre of its rectangle
    /// and the left button goes down and up. Returns the point clicked.
    pub fn click(&mut self, want: &str) -> Result<[f32; 2], String> {
        let menu = self.state("menu")?;
        let top = menu["open"]
            .as_array()
            .and_then(|o| o.last())
            .and_then(Value::as_str)
            .ok_or("no menu is open")?
            .to_owned();
        let items = menu["items"].as_array().cloned().unwrap_or_default();
        let found = want.split('|').find_map(|w| {
            items.iter().find(|it| {
                it["menu"].as_str() == Some(top.as_str())
                    && it["visible"].as_bool() == Some(true)
                    && (it["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(w))
                        || it["text"]
                            .as_str()
                            .is_some_and(|t| !t.is_empty() && t.eq_ignore_ascii_case(w)))
            })
        });
        let Some(it) = found else {
            let shown: Vec<String> = items
                .iter()
                .filter(|it| it["visible"].as_bool() == Some(true))
                .map(|it| {
                    let (n, t) = (
                        it["name"].as_str().unwrap_or_default(),
                        it["text"].as_str().unwrap_or_default(),
                    );
                    if t.is_empty() {
                        n.to_owned()
                    } else {
                        t.to_owned()
                    }
                })
                .filter(|s| !s.is_empty())
                .collect();
            return Err(format!(
                "{top} shows no item {want:?}; it shows {}",
                shown.join(", ")
            ));
        };
        let r = it["rect"].as_array().cloned().unwrap_or_default();
        if r.len() != 4 {
            return Err(format!("{want} has no rectangle: {it}"));
        }
        let at = [
            r[0].as_f64().unwrap_or(0.0) as f32 + r[2].as_f64().unwrap_or(0.0) as f32 / 2.0,
            r[1].as_f64().unwrap_or(0.0) as f32 + r[3].as_f64().unwrap_or(0.0) as f32 / 2.0,
        ];
        self.ask(json!({"cmd": "mouse", "x": at[0], "y": at[1]}))?;
        // The menu sees the move on one frame and the press on the next, as a person's pointer settles first.
        self.frames(1)?;
        self.button("mouse1", "down")?;
        self.frames(1)?;
        self.button("mouse1", "up")?;
        Ok(at)
    }

    /// Captures the frame on screen as `<name>.png` in the run's directory.
    pub fn capture(&mut self, name: &str) -> Result<PathBuf, String> {
        let shot = self.ask(json!({"cmd": "capture"}))?;
        let from = shot["path"]
            .as_str()
            .ok_or_else(|| format!("no capture path: {shot}"))?;
        let to = self.dir.join(format!("{name}.png"));
        fs::rename(from, &to).map_err(|e| format!("{from} -> {}: {e}", to.display()))?;
        let bytes = fs::metadata(&to).map_err(|e| e.to_string())?.len();
        if bytes < 1024 {
            return Err(format!("{} is {bytes} bytes", to.display()));
        }
        Ok(to)
    }

    /// Runs one named step and records its outcome; the error names the step.
    pub fn step<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let began = self.began.elapsed().as_secs_f64();
        let r = f(self);
        let secs = self.began.elapsed().as_secs_f64() - began;
        let note = match &r {
            Ok(_) => String::new(),
            Err(e) => e.clone(),
        };
        self.steps
            .push(json!({"step": name, "ok": r.is_ok(), "note": note, "secs": secs}));
        r.map_err(|e| format!("{name}: {e}"))
    }

    /// Writes the steps and the state sections `parts` as `<file>` in the run's directory, and returns the report.
    pub fn report(&mut self, file: &str, parts: &[&str]) -> Result<Value, String> {
        let mut report = json!({"steps": self.steps});
        for p in parts {
            report[*p] = self.state(p)?;
        }
        let path = self.dir.join(file);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(report)
    }

    /// Ends the run as `quit` does and waits for the process: its exit status, or `None` for a client that did not
    /// leave within `grace` and was killed.
    pub fn finish(mut self, grace: Duration) -> Option<ExitStatus> {
        let _ = self.ask(json!({"cmd": "quit"}));
        let end = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(s)) => return Some(s),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return None;
                }
            }
        }
    }
}

/// How the walker turns: the mouse, or the arrow keys once a mouse turn proved to do nothing (the pointer could
/// not be locked, so the look gate drops the motion).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    Untried,
    Mouse,
    Keys,
}

/// The stage's player in a match: a person's hands on the keys, reading the screen's own state.
pub struct Walker {
    turn: Turn,
    /// Degrees per mouse count: `sensitivity * m_yaw` and `sensitivity * m_pitch`.
    per_count: [f32; 2],
    held: Vec<&'static str>,
    last: Option<[f32; 3]>,
    stuck: u32,
    /// Ticks left of a turn away from a wall, and its direction.
    evade: (u32, f32),
    /// How many ticks turned the view with the mouse, with the keys, and fired.
    pub mouse_ticks: u32,
    pub key_ticks: u32,
    pub shots: u32,
    pub ticks: u32,
}

/// Degrees in `[-180, 180)`.
fn wrap(deg: f32) -> f32 {
    (deg + 180.0).rem_euclid(360.0) - 180.0
}

impl Walker {
    /// Reads the sensitivity the mouse turns at.
    pub fn new(c: &mut Driven) -> Result<Self, String> {
        let cvar = |c: &mut Driven, name: &str, default: f32| -> Result<f32, String> {
            let v = c.ask(json!({"cmd": "cvar", "name": name}))?;
            Ok(v["value"]
                .as_str()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(default))
        };
        let sens = cvar(c, "sensitivity", 5.0)?;
        let per_count = [
            sens * cvar(c, "m_yaw", 0.022)?,
            sens * cvar(c, "m_pitch", 0.022)?,
        ];
        Ok(Self {
            turn: Turn::Untried,
            per_count,
            held: Vec::new(),
            last: None,
            stuck: 0,
            evade: (0, 1.0),
            mouse_ticks: 0,
            key_ticks: 0,
            shots: 0,
            ticks: 0,
        })
    }

    fn hold(&mut self, c: &mut Driven, bind: &'static str, down: bool) -> Result<(), String> {
        let is = self.held.contains(&bind);
        if down && !is {
            c.bind(bind, "down")?;
            self.held.push(bind);
        } else if !down && is {
            c.bind(bind, "up")?;
            self.held.retain(|b| *b != bind);
        }
        Ok(())
    }

    /// Lets go of every key the walker holds.
    pub fn stop(&mut self, c: &mut Driven) -> Result<(), String> {
        for bind in std::mem::take(&mut self.held) {
            c.bind(bind, "up")?;
        }
        Ok(())
    }

    /// One tick of the walker, a few times a second: turns toward the nearest living enemy (or away from the wall
    /// the player is stuck on), walks, and fires when the sights are on an enemy in range. Does nothing while the
    /// player is dead, in the killcam, or has a menu or the console up.
    pub fn tick(&mut self, c: &mut Driven) -> Result<(), String> {
        self.ticks += 1;
        let player = c.state("player")?;
        let hud = c.state("hud")?;
        let busy = hud["live"]["killcam"].as_bool() == Some(true)
            || hud["open_menus"].as_array().is_some_and(|m| !m.is_empty())
            || player["alive"].as_bool() != Some(true);
        if busy || c.lookup("console.open")? == Value::Bool(true) {
            return self.stop(c);
        }
        let own = player["client"].as_u64().unwrap_or(u64::MAX);
        let team = player["team"].as_u64().unwrap_or(0);
        let origin = vec3(&player["origin"]);
        let angles = vec3(&player["angles"]);
        let eye = [origin[0], origin[1], origin[2] + 60.0];
        let ents = c.state("entities")?;
        let mut nearest: Option<(f32, [f32; 3])> = None;
        for e in ents["list"].as_array().into_iter().flatten() {
            if e["kind"].as_str() != Some("player")
                || e["actor"].as_bool() == Some(true)
                || e["dead"].as_bool() == Some(true)
                || e["client"].as_u64() == Some(own)
                || e["team"].as_u64().is_none_or(|t| t == team)
            {
                continue;
            }
            let at = vec3(&e["origin"]);
            let at = [at[0], at[1], at[2] + 50.0];
            let d = dist(eye, at);
            if nearest.is_none_or(|(n, _)| d < n) {
                nearest = Some((d, at));
            }
        }
        // Stuck when barely moving while walking.
        if let Some(l) = self.last {
            let moved = dist([origin[0], origin[1], 0.0], [l[0], l[1], 0.0]);
            self.stuck = if moved < 8.0 && self.held.contains(&"+forward") {
                self.stuck + 1
            } else {
                0
            };
        }
        self.last = Some(origin);
        if self.stuck >= 3 && self.evade.0 == 0 {
            self.evade = (
                4,
                if self.ticks.is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                },
            );
            self.stuck = 0;
        }
        let (mut yaw_err, mut pitch_err, mut fire) = (0.0_f32, 0.0_f32, false);
        if self.evade.0 > 0 {
            self.evade.0 -= 1;
            yaw_err = 30.0 * self.evade.1;
        } else if let Some((d, at)) = nearest {
            let (dx, dy, dz) = (at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]);
            let want_yaw = dy.atan2(dx).to_degrees();
            let want_pitch = -dz.atan2(dx.hypot(dy)).to_degrees();
            yaw_err = wrap(want_yaw - angles[1]);
            pitch_err = want_pitch - angles[0];
            fire = d < 900.0 && yaw_err.abs() < 4.0 && pitch_err.abs() < 4.0;
        }
        self.turn_by(c, yaw_err, pitch_err, angles)?;
        self.hold(c, "+forward", true)?;
        // Pulled on one tick and released on the next: a single-shot weapon fires once per pull.
        let pull = fire && !self.held.contains(&"+attack");
        self.shots += u32::from(pull);
        self.hold(c, "+attack", pull)?;
        Ok(())
    }

    /// Turns the view by the errors: the mouse's counts, or the arrow keys held until the next tick.
    fn turn_by(
        &mut self,
        c: &mut Driven,
        yaw_err: f32,
        pitch_err: f32,
        angles: [f32; 3],
    ) -> Result<(), String> {
        if self.turn != Turn::Keys {
            // Yaw grows to the left; a count to the right takes from it. Pitch grows downward with the counts.
            let dx = -yaw_err / self.per_count[0].max(1e-4);
            let dy = pitch_err / self.per_count[1].max(1e-4);
            if yaw_err.abs() >= 2.0 || pitch_err.abs() >= 2.0 {
                c.ask(json!({"cmd": "mouse", "dx": dx.clamp(-2000.0, 2000.0), "dy": dy.clamp(-2000.0, 2000.0)}))?;
                if self.turn == Turn::Untried && yaw_err.abs() >= 10.0 {
                    // The first real turn tells whether the look gate takes the mouse at all.
                    c.frames(3)?;
                    let after = vec3(&c.state("player")?["angles"]);
                    self.turn = if wrap(after[1] - angles[1]).abs() > 1.0 {
                        Turn::Mouse
                    } else {
                        Turn::Keys
                    };
                }
                if self.turn != Turn::Keys {
                    self.mouse_ticks += 1;
                }
            }
            if self.turn != Turn::Keys {
                return Ok(());
            }
        }
        self.key_ticks += 1;
        // `cl_yawspeed` is 140 degrees a second: a key held a tick turns some 20 degrees.
        self.hold(c, "+left", yaw_err > 6.0)?;
        self.hold(c, "+right", yaw_err < -6.0)?;
        self.hold(c, "+lookdown", pitch_err > 6.0)?;
        self.hold(c, "+lookup", pitch_err < -6.0)?;
        Ok(())
    }

    /// Whether the mouse or the arrow keys turned the view, for the stage's notes.
    pub fn turning(&self) -> &'static str {
        match self.turn {
            Turn::Untried => "never turned",
            Turn::Mouse => "the mouse",
            Turn::Keys => {
                "the arrow keys (the pointer could not be locked, so the mouse was dropped)"
            }
        }
    }

    /// Drives the player until the state matches or `limit` runs out: a short wait, then a tick, over and over.
    pub fn drive_until(
        &mut self,
        c: &mut Driven,
        state: Value,
        limit: Duration,
        what: &str,
    ) -> Result<Value, String> {
        let end = Instant::now() + limit;
        loop {
            if let Some(r) = c.wait(&state, Duration::from_millis(150))? {
                self.stop(c)?;
                return Ok(r);
            }
            if Instant::now() > end {
                self.stop(c)?;
                let seen = c.observed(&state)?;
                return Err(format!(
                    "{what}: not within {} s; observed {seen}",
                    limit.as_secs()
                ));
            }
            self.tick(c)?;
        }
    }
}

fn vec3(v: &Value) -> [f32; 3] {
    let f = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    [f(0), f(1), f(2)]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_wrap_to_the_short_way_round() {
        assert_eq!(wrap(190.0), -170.0);
        assert_eq!(wrap(-190.0), 170.0);
        assert_eq!(wrap(45.0), 45.0);
        assert_eq!(wrap(-180.0), -180.0);
    }
}
