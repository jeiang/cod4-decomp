// SPDX-License-Identifier: GPL-3.0-or-later
//! Player input: keys, mouse, wheel and gamepad resolved through the bind table into one [`InputFrame`] per frame.
//!
//! Decoupled from networking: the netplay layer turns an `InputFrame` into a usercmd. Commands keep their original
//! names (`+forward`, `+attack`, `weapnext`, ...). A `+cmd` is *held* while any key bound to it is down; a tap shorter
//! than a frame still shows for one frame. Any other bound command is handed over once, in
//! [`InputFrame::pending_commands`], except the config commands (`bind`, `set`, `exec`, ...) which run here.
//!
//! Angle convention (the original's): positive yaw turns left, positive pitch looks down. `look_delta_*` are degrees.

pub mod config;
mod cvar;
mod keys;

/// The input layer's name of a physical key, for binding it from a menu.
pub fn physical_key_name(code: winit::keyboard::KeyCode) -> Option<String> {
    keys::key_name(code).map(str::to_owned)
}

/// The input layer's name of a mouse button.
pub fn mouse_key_name(b: winit::event::MouseButton) -> String {
    keys::mouse_name(b)
}

/// The id the original's UI localizes a key by (`KEY_MOUSE1`).
pub use keys::key_id;
mod pad;
mod rawmouse;
#[cfg(not(target_arch = "wasm32"))]
pub mod selftest;

pub use cvar::Cvars;
pub use rawmouse::RawMouse;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use web_time::Instant;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::PhysicalKey;

/// `usercmd.buttons` bits: the sim crate's, as `u32`.
pub mod buttons {
    use sim::pm::button;
    pub const ATTACK: u32 = button::ATTACK as u32;
    pub const SPRINT: u32 = button::SPRINT as u32;
    pub const MELEE: u32 = button::MELEE as u32;
    pub const USE: u32 = button::USE as u32;
    pub const RELOAD: u32 = button::RELOAD as u32;
    pub const USE_RELOAD: u32 = button::USE_RELOAD as u32;
    pub const LEAN_LEFT: u32 = button::LEAN_LEFT as u32;
    pub const LEAN_RIGHT: u32 = button::LEAN_RIGHT as u32;
    pub const PRONE: u32 = button::PRONE as u32;
    pub const CROUCH: u32 = button::CROUCH as u32;
    pub const JUMP: u32 = button::JUMP as u32;
    pub const ADS: u32 = button::ADS as u32;
    pub const BREATH: u32 = button::BREATH as u32;
    pub const FRAG: u32 = button::FRAG as u32;
    pub const SMOKE: u32 = button::SMOKE as u32;
    pub const NIGHTVISION: u32 = button::NIGHTVISION as u32;
    pub const THROW: u32 = button::THROW as u32;
}

/// What the player asked for this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputFrame {
    /// -1 (back) ..= 1 (forward).
    pub move_forward: f32,
    /// -1 (left) ..= 1 (right).
    pub move_right: f32,
    /// -1 (down: `+movedown`) ..= 1 (up: `+gostand`/`+moveup`); for free-fly use.
    pub up: f32,
    /// Degrees, positive = turn left. Scaled by sensitivity, `m_yaw`, rates and `dt`.
    pub look_delta_yaw: f32,
    /// Degrees, positive = look down.
    pub look_delta_pitch: f32,
    /// [`buttons`] held this frame.
    pub buttons: u32,
    /// Bits that went from up to down since the previous frame.
    pub pressed: u32,
    /// Bits that went from down to up since the previous frame.
    pub released: u32,
    /// Names (no `+`) of held `+commands` with no button bit, e.g. `scores`.
    pub held_other: Vec<String>,
    /// Non-`+` commands run since the previous frame, in order (`weapnext`, `say`, `togglemenu`, ...).
    pub pending_commands: Vec<String>,
}

impl InputFrame {
    /// An explicit `quit` command. Never Escape: that is `togglemenu`.
    pub fn quit(&self) -> bool {
        self.pending_commands.iter().any(|c| c == "quit")
    }

    /// `togglefullscreen` ran (F11).
    pub fn toggle_fullscreen(&self) -> bool {
        self.pending_commands
            .iter()
            .any(|c| c == "togglefullscreen")
    }

    /// `togglemenu` ran (Escape, gamepad start): release the pointer, and open the menu where there is one.
    pub fn toggle_menu(&self) -> bool {
        self.pending_commands.iter().any(|c| c == "togglemenu")
    }
}

#[derive(Default)]
struct Held {
    count: u32,
    /// Pressed since the last frame, even if already released.
    tapped: bool,
}

/// `+command` -> button bits. Commands not listed have none.
fn button_bits(cmd: &str) -> u32 {
    use buttons::*;
    match cmd {
        "attack" => ATTACK,
        "speed_throw" => ADS,
        "reload" => RELOAD,
        "activate" => USE,
        "usereload" => USE_RELOAD,
        "melee" => MELEE,
        "frag" => FRAG,
        "smoke" => SMOKE,
        "gostand" | "moveup" => JUMP,
        "movedown" => CROUCH,
        "prone" => PRONE,
        "sprint" => SPRINT,
        "breath_sprint" => SPRINT | BREATH,
        "holdbreath" => BREATH,
        "leanleft" => LEAN_LEFT,
        "leanright" => LEAN_RIGHT,
        "throw" => THROW,
        "nightvision" => NIGHTVISION,
        _ => 0,
    }
}

/// Held commands consumed as axes or toggles rather than buttons.
const AXIS_CMDS: &[&str] = &[
    "forward",
    "back",
    "moveleft",
    "moveright",
    "left",
    "right",
    "lookup",
    "lookdown",
    "toggleads_throw",
];

/// `COD4E_INPUT_DEBUG=1`: one line per second, see [`Input::count_debug`].
struct DebugCounter {
    last: Instant,
    base: rawmouse::Totals,
    sum: (f64, f64),
    frames: u32,
    /// Frames whose mouse motion the gate threw away, and the reason of the last.
    dropped: u32,
    why: &'static str,
}

pub struct Input {
    pub cvars: Cvars,
    binds: BTreeMap<String, String>,
    /// Keys currently down -> the `+commands` their press started (so release stops exactly those).
    down: HashMap<String, Vec<String>>,
    held: HashMap<String, Held>,
    ads_toggle: bool,
    pending: Vec<String>,
    /// Wheel "keys" to release after the next frame has seen them.
    wheel: Vec<String>,
    mouse: RawMouse,
    pad: pad::Pad,
    focused: bool,
    captured: bool,
    prev_buttons: u32,
    config_path: Option<PathBuf>,
    dirty: bool,
    debug: Option<DebugCounter>,
}

const MAX_EXEC_DEPTH: u32 = 8;

impl DebugCounter {
    fn new(base: rawmouse::Totals) -> Self {
        Self {
            last: Instant::now(),
            base,
            sum: (0.0, 0.0),
            frames: 0,
            dropped: 0,
            why: "",
        }
    }
}

impl Input {
    /// Load the embedded default binds, then `config_path` (or the platform default, see [`config::default_path`])
    /// if it exists. Opens the raw mouse and gamepad backends; either failing is logged, not fatal.
    pub fn new(config_path: Option<PathBuf>) -> Self {
        let mut i = Self::detached();
        i.mouse = RawMouse::new();
        i.pad = pad::Pad::new();
        i.config_path = config_path.or_else(config::default_path);
        if let Some(p) = i.config_path.clone() {
            match std::fs::read_to_string(&p) {
                Ok(text) => i.exec_text(&text, p.parent(), false, 0),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!("cannot read {}: {e}", p.display()),
            }
        }
        i.dirty = false;
        // The window takes the pointer on the first click; until then the cursor is free.
        i.captured = false;
        if std::env::var_os("COD4E_INPUT_DEBUG").is_some_and(|v| v == "1") {
            i.debug = Some(DebugCounter::new(rawmouse::Totals::default()));
        }
        i
    }

    /// Default binds and cvars only; no devices, no file. Focused and cursor-captured.
    pub fn detached() -> Self {
        let mut i = Self {
            cvars: Cvars::default(),
            binds: BTreeMap::new(),
            down: HashMap::new(),
            held: HashMap::new(),
            ads_toggle: false,
            pending: Vec::new(),
            wheel: Vec::new(),
            mouse: RawMouse::detached(),
            pad: pad::Pad::none(),
            focused: true,
            captured: true,
            prev_buttons: 0,
            config_path: None,
            dirty: false,
            debug: None,
        };
        i.exec_text(include_str!("default_mp.cfg"), None, false, 0);
        i.dirty = false;
        i
    }

    /// Tell the input whether the cursor is grabbed. Mouse look only applies while it is.
    pub fn set_captured(&mut self, captured: bool) {
        if self.debug.is_some() && captured != self.captured {
            eprintln!(
                "input: pointer lock {}",
                if captured { "taken" } else { "released" }
            );
        }
        self.captured = captured;
    }

    /// Why this frame's mouse motion is thrown away, if it is.
    fn look_gate(&self) -> Option<&'static str> {
        if !self.focused {
            Some("unfocused")
        } else if !self.captured {
            Some("pointer free")
        } else if !self.cvars.bool("in_mouse") {
            Some("in_mouse 0")
        } else {
            None
        }
    }

    pub fn window_event(&mut self, ev: &WindowEvent) {
        match ev {
            WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                if let PhysicalKey::Code(c) = event.physical_key
                    && let Some(n) = keys::key_name(c)
                {
                    self.key(n, event.state == ElementState::Pressed);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.key(&keys::mouse_name(*button), *state == ElementState::Pressed);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                if y != 0.0 {
                    self.pulse(if y > 0.0 { "mwheelup" } else { "mwheeldown" });
                }
            }
            WindowEvent::Focused(f) => {
                if self.debug.is_some() {
                    eprintln!("input: window {}", if *f { "focused" } else { "unfocused" });
                }
                self.focused = *f;
                if !*f {
                    self.release_all();
                }
            }
            _ => {}
        }
    }

    /// Feed winit's raw motion (used when the platform has no better source).
    pub fn device_event(&mut self, ev: &winit::event::DeviceEvent) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = ev {
            self.mouse.winit_motion(*delta);
        }
    }

    /// Everything the player asked for since the previous call.
    pub fn frame(&mut self, dt: f32) -> InputFrame {
        for (name, down) in self.pad.poll() {
            if self.focused && self.cvars.bool("in_gamepad") {
                self.key(name, down);
            }
        }
        let raw = self.mouse.drain();
        let gate = self.look_gate();
        self.count_debug(raw, gate);
        let (mdx, mdy) = if gate.is_none() { raw } else { (0.0, 0.0) };
        let cv = &self.cvars;
        let sens = cv.f32("sensitivity");
        let invert = if cv.bool("input_invertpitch") {
            -1.0
        } else {
            1.0
        };
        let held = |c: &str| self.held.get(c).is_some_and(|h| h.count > 0 || h.tapped);
        let axis = |pos: &str, neg: &str| f32::from(i8::from(held(pos)) - i8::from(held(neg)));

        let mut yaw = -(mdx as f32) * sens * cv.f32("m_yaw");
        let mut pitch = mdy as f32 * sens * cv.f32("m_pitch") * invert;
        yaw += axis("left", "right") * cv.f32("cl_yawspeed") * dt;
        pitch += axis("lookdown", "lookup") * cv.f32("cl_pitchspeed") * dt;

        let dz = cv.f32("in_gamepad_deadzone");
        let (lx, ly) = pad::radial_deadzone(self.pad.left.0, self.pad.left.1, dz);
        let (rx, ry) = pad::radial_deadzone(self.pad.right.0, self.pad.right.1, dz);
        yaw -= rx * cv.f32("in_gamepad_yawrate") * dt;
        pitch -= ry * cv.f32("in_gamepad_pitchrate") * dt * invert;

        let walk = if cv.bool("cl_run") { 1.0 } else { 0.5 };
        let mut buttons = self.ads_toggle as u32 * buttons::ADS;
        let mut held_other = Vec::new();
        for (name, h) in &self.held {
            if h.count > 0 || h.tapped {
                let bits = button_bits(name);
                buttons |= bits;
                if bits == 0 && !AXIS_CMDS.contains(&name.as_str()) {
                    held_other.push(name.clone());
                }
            }
        }
        held_other.sort_unstable();
        let up =
            f32::from(i8::from(held("gostand") || held("moveup")) - i8::from(held("movedown")));
        let f = InputFrame {
            move_forward: (axis("forward", "back") * walk + ly).clamp(-1.0, 1.0),
            move_right: (axis("moveright", "moveleft") * walk + lx).clamp(-1.0, 1.0),
            up,
            look_delta_yaw: yaw,
            look_delta_pitch: pitch,
            buttons,
            pressed: buttons & !self.prev_buttons,
            released: self.prev_buttons & !buttons,
            held_other,
            pending_commands: std::mem::take(&mut self.pending),
        };
        self.prev_buttons = buttons;
        for h in self.held.values_mut() {
            h.tapped = false;
        }
        for w in std::mem::take(&mut self.wheel) {
            self.key(&w, false);
        }
        f
    }

    /// Run console-style input: `bind`, `set`, `+attack`, `weapnext`, ... Several commands may share a line with `;`.
    /// Commands that are not the input layer's own land in the next frame's `pending_commands`.
    pub fn exec_line(&mut self, line: &str) {
        self.exec_text(line, None, true, 0);
    }

    /// The folder of the config file (where the player profile lives too); `None` without one.
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
    }

    /// Cvar value as text.
    pub fn cvar(&self, name: &str) -> Option<&str> {
        self.cvars.get(name)
    }

    /// Set a cvar; it is written back on [`Self::save`] only if it was archived (`seta`, or a default archived one).
    pub fn set_cvar(&mut self, name: &str, value: &str) {
        self.cvars.set(name, value, false);
        self.dirty = true;
    }

    /// `(min, max)` view pitch in degrees (positive = down).
    #[allow(dead_code)] // netplay integration API
    pub fn pitch_limits(&self) -> (f32, f32) {
        (self.cvars.f32("cl_pitchmin"), self.cvars.f32("cl_pitchmax"))
    }

    /// The config as text: archived cvars, then `unbindall` and every bind.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn config_text(&self) -> String {
        let mut s = String::from("// generated by cod4e\n");
        for (n, v) in self.cvars.archived() {
            s += &format!("seta {n} {}\n", config::quote(v));
        }
        s += "unbindall\n";
        for (k, c) in &self.binds {
            s += &format!("bind {} {}\n", config::quote(k), config::quote(c));
        }
        s
    }

    /// Write the config file, if anything changed since it was loaded.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.config_path.clone().filter(|_| self.dirty) else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, self.config_text())?;
        self.dirty = false;
        Ok(())
    }

    /// Keys bound to `command`, in name order.
    pub fn binding_keys(&self, command: &str) -> Vec<&str> {
        self.binds
            .iter()
            .filter(|(_, c)| c.eq_ignore_ascii_case(command))
            .map(|(k, _)| k.as_str())
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn bound(&self, key: &str) -> Option<&str> {
        self.binds
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn exec_text(&mut self, text: &str, dir: Option<&Path>, sink: bool, depth: u32) {
        for line in text.lines() {
            for toks in config::split_commands(line) {
                self.exec_tokens(&toks, dir, sink, depth);
            }
        }
    }

    /// `sink`: unknown commands go to `pending` (interactive); off while loading files.
    fn exec_tokens(&mut self, toks: &[String], dir: Option<&Path>, sink: bool, depth: u32) {
        let args: Vec<&str> = toks[1..].iter().map(String::as_str).collect();
        let name = toks[0].to_ascii_lowercase();
        match (name.as_str(), args.as_slice()) {
            ("bind", [key, cmd]) => {
                self.binds
                    .insert(key.to_ascii_lowercase(), (*cmd).to_owned());
                self.dirty = true;
            }
            ("unbind", [key]) => {
                self.binds.remove(&key.to_ascii_lowercase());
                self.dirty = true;
            }
            ("unbindall", []) => {
                self.binds.clear();
                self.dirty = true;
            }
            ("set" | "seta" | "sets" | "setu", [n, v, ..]) => {
                self.cvars.set(n, v, name == "seta");
                self.dirty = true;
            }
            ("exec", [file]) if depth < MAX_EXEC_DEPTH => {
                let path = dir.map_or_else(|| PathBuf::from(file), |d| d.join(file));
                match std::fs::read_to_string(&path) {
                    Ok(t) => self.exec_text(&t, path.parent(), sink, depth + 1),
                    Err(e) => eprintln!("exec {}: {e}", path.display()),
                }
            }
            ("bind" | "unbind" | "unbindall" | "set" | "seta" | "sets" | "setu" | "exec", _) => {
                eprintln!("bad arguments: {}", config::join(toks));
            }
            ("+actionslot", _) if sink => {
                self.pending.push(config::join(toks).replacen('+', "", 1))
            }
            ("-actionslot", _) => {}
            _ if name.starts_with('+') && sink => self.console_hold(&name[1..], true),
            _ if name.starts_with('-') && sink => self.console_hold(&name[1..], false),
            _ if sink => self.pending.push(config::join(toks)),
            _ => {}
        }
    }

    /// `+cmd` / `-cmd` typed rather than bound: held until the matching `-cmd`.
    fn console_hold(&mut self, cmd: &str, press: bool) {
        let started = self.down.entry(String::new()).or_default();
        if press {
            started.push(cmd.to_owned());
            self.hold(cmd);
        } else if let Some(i) = started.iter().position(|c| c == cmd) {
            started.swap_remove(i);
            self.release(cmd);
        }
    }

    fn hold(&mut self, cmd: &str) {
        if cmd == "toggleads_throw" {
            self.ads_toggle = !self.ads_toggle;
        }
        let h = self.held.entry(cmd.to_owned()).or_default();
        h.count += 1;
        h.tapped = true;
    }

    fn release(&mut self, cmd: &str) {
        if let Some(h) = self.held.get_mut(cmd) {
            h.count = h.count.saturating_sub(1);
        }
    }

    /// A key (or button) changed state. Repeats are ignored: the key is already down.
    fn key(&mut self, name: &str, down: bool) {
        if !down {
            for c in self.down.remove(name).unwrap_or_default() {
                self.release(&c);
            }
            return;
        }
        if self.down.contains_key(name) {
            return;
        }
        let mut started = Vec::new();
        if let Some(line) = self.binds.get(name).cloned() {
            for toks in config::split_commands(&line) {
                if toks[0] == "+actionslot" {
                    // A press, not a held button: the game selects the slot's weapon once.
                    self.pending.push(config::join(&toks).replacen('+', "", 1));
                } else if let Some(c) = toks[0].strip_prefix('+') {
                    self.hold(c);
                    started.push(c.to_owned());
                } else {
                    self.exec_tokens(&toks, None, true, 0);
                }
            }
        }
        self.down.insert(name.to_owned(), started);
    }

    /// A key with no held state: pressed now, released after the next frame has seen it.
    fn pulse(&mut self, name: &str) {
        self.key(name, false);
        self.key(name, true);
        self.wheel.push(name.to_owned());
    }

    /// Lets go of every key, button and stick: the state the input holds when the events that would end it will go
    /// elsewhere (a menu takes the window's events while it is open).
    pub fn release_all(&mut self) {
        let keys: Vec<String> = self
            .down
            .keys()
            .filter(|k| !k.is_empty())
            .cloned()
            .collect();
        for k in keys {
            self.key(&k, false);
        }
        for h in self.held.values_mut() {
            h.tapped = false;
        }
        self.pad.left = (0.0, 0.0);
        self.pad.right = (0.0, 0.0);
    }

    /// Once a second: per-source event counts, the look gate, focus, lock and the GCMouse devices.
    fn count_debug(&mut self, raw: (f64, f64), gate: Option<&'static str>) {
        let Some(d) = self.debug.as_mut() else { return };
        d.frames += 1;
        d.sum.0 += raw.0;
        d.sum.1 += raw.1;
        if let (Some(why), true) = (gate, raw != (0.0, 0.0)) {
            d.dropped += 1;
            d.why = why;
        }
        let el = d.last.elapsed().as_secs_f64();
        if el >= 1.0 {
            let (t, b) = (self.mouse.totals(), d.base);
            let per = |n: u64| n as f64 / el;
            eprintln!(
                "input: src={} gc/s={:.0} winit/s={:.0} used/s={:.0} sum=({:+.1},{:+.1}) look={} \
                 focused={} pointer_lock={} frames={} mice={} {}",
                self.mouse.source(),
                per(t.gc - b.gc),
                per(t.winit - b.winit),
                per(t.used - b.used),
                d.sum.0,
                d.sum.1,
                if d.dropped > 0 {
                    format!("dropped({}) in {} frames", d.why, d.dropped)
                } else {
                    "ok".into()
                },
                self.focused,
                self.captured,
                d.frames,
                self.mouse.devices(),
                self.mouse.describe_devices(),
            );
            *d = DebugCounter::new(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(i: &mut Input) -> InputFrame {
        i.frame(0.01)
    }

    #[test]
    fn plus_commands_hold_release_and_edge() {
        let mut i = Input::detached();
        i.key("mouse1", true);
        let f = frame(&mut i);
        assert_eq!(f.buttons, buttons::ATTACK);
        assert_eq!(f.pressed, buttons::ATTACK);
        let f = frame(&mut i);
        assert_eq!((f.buttons, f.pressed, f.released), (buttons::ATTACK, 0, 0));
        i.key("mouse1", true); // key repeat while down
        i.key("mouse1", false);
        let f = frame(&mut i);
        assert_eq!((f.buttons, f.released), (0, buttons::ATTACK));
    }

    #[test]
    fn tap_within_one_frame_still_registers() {
        let mut i = Input::detached();
        i.key("r", true);
        i.key("r", false);
        let f = frame(&mut i);
        assert_eq!(f.buttons, buttons::RELOAD);
        assert_eq!(frame(&mut i).buttons, 0);
    }

    #[test]
    fn two_keys_one_command_hold_until_both_up() {
        let mut i = Input::detached();
        i.key("ctrl", true);
        i.key("c", true);
        i.key("ctrl", false);
        let f = frame(&mut i);
        assert_eq!((f.buttons, f.up), (buttons::CROUCH, -1.0));
        i.key("c", false);
        assert_eq!(frame(&mut i).buttons, 0);
    }

    #[test]
    fn rebinding_while_down_releases_the_original_command() {
        let mut i = Input::detached();
        i.key("w", true);
        assert_eq!(frame(&mut i).move_forward, 1.0);
        i.exec_line("bind w \"+back\"");
        i.key("w", false);
        let f = frame(&mut i);
        assert_eq!(f.move_forward, 0.0);
    }

    #[test]
    fn modifiers_are_separate_keys_and_axes_combine() {
        let mut i = Input::detached();
        i.key("shift", true);
        i.key("w", true);
        i.key("d", true);
        let f = frame(&mut i);
        assert_eq!(f.buttons, buttons::SPRINT | buttons::BREATH);
        assert_eq!((f.move_forward, f.move_right), (1.0, 1.0));
        i.key("rshift", true); // unbound by default
        i.key("shift", false);
        assert_eq!(frame(&mut i).buttons, 0);
        i.key("w", true); // already down: still forward
        i.key("s", true);
        assert_eq!(frame(&mut i).move_forward, 0.0);
    }

    #[test]
    fn wheel_pulses_one_frame_and_queues_commands() {
        let mut i = Input::detached();
        i.pulse("mwheelup");
        i.pulse("mwheelup");
        i.pulse("mwheeldown");
        let f = frame(&mut i);
        assert_eq!(f.pending_commands, ["weapnext", "weapnext", "weapprev"]);
        assert!(frame(&mut i).pending_commands.is_empty());
        i.exec_line("bind mwheelup \"+reload\"");
        i.pulse("mwheelup");
        assert_eq!(frame(&mut i).buttons, buttons::RELOAD);
        assert_eq!(frame(&mut i).buttons, 0);
    }

    #[test]
    fn console_hold_and_toggle_ads() {
        let mut i = Input::detached();
        i.exec_line("+frag; +smoke");
        let f = frame(&mut i);
        assert_eq!(f.buttons, buttons::FRAG | buttons::SMOKE);
        i.exec_line("-frag");
        assert_eq!(frame(&mut i).buttons, buttons::SMOKE);
        i.exec_line("-smoke");
        i.exec_line("bind k \"+toggleads_throw\"");
        i.key("k", true);
        i.key("k", false);
        assert_eq!(frame(&mut i).buttons, buttons::ADS);
        assert_eq!(frame(&mut i).buttons, buttons::ADS);
        i.key("k", true);
        i.key("k", false);
        assert_eq!(frame(&mut i).buttons, 0);
    }

    #[test]
    fn unknown_plus_commands_report_held_and_unknown_commands_pend() {
        let mut i = Input::detached();
        i.key("tab", true);
        assert_eq!(frame(&mut i).held_other, ["scores"]);
        i.key("tab", false);
        assert!(frame(&mut i).held_other.is_empty());
        i.exec_line("say \"hello world\"; weapnext");
        assert_eq!(
            frame(&mut i).pending_commands,
            ["say \"hello world\"", "weapnext"]
        );
    }

    #[test]
    fn mouse_look_scales_and_gates() {
        let mut i = Input::detached();
        i.exec_line("set sensitivity 2; set m_yaw 0.5; set m_pitch 0.25");
        i.mouse.winit_motion((10.0, 4.0));
        let f = frame(&mut i);
        assert_eq!((f.look_delta_yaw, f.look_delta_pitch), (-10.0, 2.0));
        i.set_captured(false);
        i.mouse.winit_motion((10.0, 4.0));
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
        i.set_captured(true);
        i.exec_line("set input_invertpitch 1");
        i.mouse.winit_motion((0.0, 4.0));
        assert_eq!(frame(&mut i).look_delta_pitch, -2.0);
        i.exec_line("set in_mouse 0");
        i.mouse.winit_motion((9.0, 9.0));
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
    }

    #[test]
    fn keyboard_look_uses_rates() {
        let mut i = Input::detached();
        i.exec_line("set cl_yawspeed 100");
        i.key("leftarrow", true);
        let f = i.frame(0.5);
        assert_eq!(f.look_delta_yaw, 50.0);
    }

    #[test]
    fn walk_halves_keyboard_axes() {
        let mut i = Input::detached();
        i.exec_line("set cl_run 0");
        i.key("w", true);
        assert_eq!(frame(&mut i).move_forward, 0.5);
    }

    #[test]
    fn blur_releases_everything() {
        let mut i = Input::detached();
        i.key("w", true);
        i.key("mouse1", true);
        i.window_event(&WindowEvent::Focused(false));
        let f = frame(&mut i);
        assert_eq!((f.buttons, f.move_forward), (0, 0.0));
        i.mouse.winit_motion((5.0, 5.0));
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
    }

    /// The d-pad HUD and the Controls menu look keys up by the exact command `+actionslot N`.
    #[test]
    fn action_slots_have_stock_default_keys_and_rebind() {
        let mut i = Input::detached();
        for (slot, key) in [(1, "n"), (2, "7"), (3, "5"), (4, "6")] {
            assert_eq!(i.binding_keys(&format!("+actionslot {slot}")), [key]);
        }
        i.key("6", true);
        assert_eq!(frame(&mut i).pending_commands, ["actionslot 4"]);
        // What the menu's bind capture executes.
        i.exec_line("unbind \"n\"");
        i.exec_line("bind \"h\" \"+actionslot 1\"");
        assert_eq!(i.binding_keys("+actionslot 1"), ["h"]);
    }

    #[test]
    fn config_round_trips_binds_and_archived_cvars() {
        let mut a = Input::detached();
        a.exec_line("unbind w; bind \"semicolon\" \"say \\\"hi; there\\\"\"; seta sensitivity 7.5; seta my_name \"a \\\"b\\\" c\"; set tmp 1");
        let text = a.config_text();
        assert!(!text.contains("tmp"));
        let mut b = Input::detached();
        b.exec_text(&text, None, false, 0);
        assert_eq!(b.config_text(), text);
        assert_eq!(b.bound("w"), None);
        assert_eq!(b.bound("semicolon"), Some("say \"hi; there\""));
        assert_eq!(b.cvar("sensitivity"), Some("7.5"));
        assert_eq!(b.cvar("MY_NAME"), Some("a \"b\" c"));
        assert_eq!(b.cvar("tmp"), None);
    }

    #[test]
    fn exec_loads_relative_file_and_save_writes_only_when_dirty() {
        let dir = std::env::temp_dir().join(format!("cod4e-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("extra.cfg"),
            "bind p \"+reload\"\nseta sensitivity 3\n",
        )
        .unwrap();
        let cfg = dir.join("config_mp.cfg");
        std::fs::write(&cfg, "exec extra.cfg\n").unwrap();
        let mut i = Input::detached();
        i.config_path = Some(cfg.clone());
        i.exec_text(
            &std::fs::read_to_string(&cfg).unwrap(),
            Some(&dir),
            false,
            0,
        );
        assert_eq!(
            (i.bound("p"), i.cvar("sensitivity")),
            (Some("+reload"), Some("3"))
        );
        i.dirty = false;
        i.save().unwrap();
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "exec extra.cfg\n");
        i.set_cvar("sensitivity", "4");
        i.save().unwrap();
        assert!(
            std::fs::read_to_string(&cfg)
                .unwrap()
                .contains("seta sensitivity \"4\"")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exec_recursion_is_bounded() {
        let dir = std::env::temp_dir().join(format!("cod4e-input-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("loop.cfg"), "exec loop.cfg\n").unwrap();
        let mut i = Input::detached();
        i.exec_text("exec loop.cfg", Some(&dir), false, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn escape_opens_the_menu_and_never_quits() {
        let mut i = Input::detached();
        i.key("escape", true);
        let f = frame(&mut i);
        i.key("escape", false);
        assert!(f.toggle_menu() && !f.quit(), "{:?}", f.pending_commands);
        i.exec_line("quit");
        let f = frame(&mut i);
        assert!(f.quit() && !f.toggle_menu());
        i.key("pad_start", true);
        assert!(frame(&mut i).toggle_menu());
    }

    #[test]
    fn mouse_look_gate_names_why_motion_is_dropped() {
        let mut i = Input::detached();
        assert_eq!(i.look_gate(), None);
        i.set_captured(false);
        assert_eq!(i.look_gate(), Some("pointer free"));
        i.set_captured(true);
        i.window_event(&WindowEvent::Focused(false));
        assert_eq!(i.look_gate(), Some("unfocused"));
        i.window_event(&WindowEvent::Focused(true));
        i.exec_line("set in_mouse 0");
        assert_eq!(i.look_gate(), Some("in_mouse 0"));
    }

    /// Escape, then a click: motion made while the pointer was free is not replayed when it is taken again.
    #[test]
    fn released_pointer_drops_motion_and_a_click_resumes_it() {
        let mut i = Input::detached();
        i.set_captured(false);
        i.mouse.winit_motion((50.0, 50.0));
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
        i.set_captured(true);
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
        i.mouse.winit_motion((10.0, 0.0));
        assert_ne!(frame(&mut i).look_delta_yaw, 0.0);
    }

    #[test]
    fn real_devices_start_with_the_pointer_free() {
        let mut i = Input::new(Some(std::env::temp_dir().join("cod4e-no-such-config.cfg")));
        assert_eq!(i.look_gate(), Some("pointer free"));
        i.mouse.winit_motion((10.0, 10.0));
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
    }
}
