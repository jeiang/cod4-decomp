// SPDX-License-Identifier: GPL-3.0-only
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
mod feedback;
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
pub use feedback::{Feedback, Seen, scan_own};
pub use rawmouse::RawMouse;

use assets::vfs::Vfs;
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
    pub const TEMP_STANCE: u32 = button::TEMP_STANCE as u32;
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
    /// Seconds down in the frame so far, for the intervals already over.
    accum: f64,
    /// When the interval still running began (`Some` exactly while `count > 0`).
    since: Option<f64>,
}

/// What a held command was asked as: the name [`Input::hold`] may have swapped for a press that is not a button.
fn requested(held: &str) -> &str {
    match held {
        "gostand_stand" => "gostand",
        "moveup_stance" => "moveup",
        h => h,
    }
}

/// `+command` -> button bits. Commands not listed have none.
fn button_bits(cmd: &str) -> u32 {
    use buttons::*;
    match cmd {
        "attack" => ATTACK,
        // Both ADS throws also hold THROW (`IN_ToggleADS_Throw_Down`, `IN_Speed_Throw_Down`); the ADS bit itself is
        // decided in `frame`.
        "speed_throw" | "toggleads_throw" => THROW,
        "reload" => RELOAD,
        "activate" => USE,
        "usereload" => USE_RELOAD,
        "melee" => MELEE,
        "frag" => FRAG,
        "smoke" => SMOKE,
        "gostand" | "moveup" => JUMP,
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

/// Held commands consumed as axes, modifiers or stance keys rather than buttons.
const CONSUMED_CMDS: &[&str] = &[
    "forward",
    "back",
    "moveleft",
    "moveright",
    "left",
    "right",
    "lookup",
    "lookdown",
    "speed",
    "strafe",
    "mlook",
    "stance",
    "prone",
    "movedown",
    "gostand_stand",
    "moveup_stance",
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
    /// `usingAds`: ADS toggled on by `+toggleads_throw` / `toggleads`; the held speed key flips it for as long as it is down.
    using_ads: bool,
    /// The stance the stance commands latched: `buttons::PRONE`, `buttons::CROUCH` or 0. Forced-stance events, a
    /// respawn and standing up reset it.
    stance: u32,
    /// `+stance` held: the stance it started from and when.
    stance_hold: Option<(u32, f64)>,
    /// The cgame says the player cannot look or turn (`PMF_FROZEN`).
    frozen: bool,
    /// The clock [`Input::now`] reads: real time, or a fixed value for a detached input.
    start: Instant,
    clock: Option<f64>,
    last_frame: f64,
    /// The previous frame's mouse motion, for `m_filter`.
    prev_mouse: (f32, f32),
    pending: Vec<String>,
    /// Wheel "keys" to release after the next frame has seen them.
    wheel: Vec<String>,
    mouse: RawMouse,
    pad: pad::Pad,
    focused: bool,
    captured: bool,
    prev_buttons: u32,
    /// The user's folder, and the file [`Input::save`] writes (the active profile's `config_mp.cfg` there).
    config_dir: Option<PathBuf>,
    config_path: Option<PathBuf>,
    dirty: bool,
    debug: Option<DebugCounter>,
    /// See [`Input::set_fov_sensitivity_scale`].
    fov_sensitivity_scale: f32,
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
    /// Execute the install's `default_mp.cfg` (through `vfs`, as the original does at startup). The profile's own
    /// `config_mp.cfg` follows through [`Input::use_profile`]. `config_dir` is the user's folder (or the platform
    /// default, see [`config::default_dir`]). Opens the raw mouse and gamepad backends; either failing is logged,
    /// not fatal.
    pub fn new(config_dir: Option<PathBuf>, vfs: Option<&Vfs>) -> Self {
        let mut i = Self::bare();
        i.load_defaults(vfs);
        i.mouse = RawMouse::new();
        i.clock = None;
        i.pad = pad::Pad::new();
        i.config_dir = config_dir.or_else(config::default_dir);
        i.dirty = false;
        // The window takes the pointer on the first click; until then the cursor is free.
        i.captured = false;
        if std::env::var_os("COD4E_INPUT_DEBUG").is_some_and(|v| v == "1") {
            i.debug = Some(DebugCounter::new(rawmouse::Totals::default()));
        }
        i
    }

    /// Fallback binds and cvars only; no devices, no file, no install. Focused and cursor-captured.
    pub fn detached() -> Self {
        let mut i = Self::bare();
        i.load_defaults(None);
        i
    }

    /// `default_mp.cfg` from the install, or the small built-in fallback without one, then the extra binds.
    fn load_defaults(&mut self, vfs: Option<&Vfs>) {
        let stock = vfs.is_some_and(|v| self.exec_vfs(v, "default_mp.cfg", 0));
        if !stock {
            self.exec_text(include_str!("fallback_mp.cfg"), None, false, 0);
        }
        self.exec_text(include_str!("extra_binds.cfg"), None, false, 0);
        self.dirty = false;
    }

    /// `exec <name>` through the search path; `false` if the file is not there. Unknown commands in it are skipped,
    /// as when the original loads a config with commands another build lacks.
    fn exec_vfs(&mut self, vfs: &Vfs, name: &str, depth: u32) -> bool {
        let Ok(Some(bytes)) = vfs.read(name) else {
            return false;
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            for toks in config::split_commands(line) {
                match (toks[0].to_ascii_lowercase().as_str(), &toks[1..]) {
                    ("exec", [file]) if depth < MAX_EXEC_DEPTH => {
                        if !self.exec_vfs(vfs, file, depth + 1) {
                            eprintln!("couldn't exec {file}");
                        }
                    }
                    _ => self.exec_tokens(&toks, None, false, depth),
                }
            }
        }
        true
    }

    fn bare() -> Self {
        Self {
            cvars: Cvars::default(),
            binds: BTreeMap::new(),
            down: HashMap::new(),
            held: HashMap::new(),
            using_ads: false,
            stance: 0,
            stance_hold: None,
            frozen: false,
            start: Instant::now(),
            clock: Some(0.0),
            last_frame: 0.0,
            prev_mouse: (0.0, 0.0),
            pending: Vec::new(),
            wheel: Vec::new(),
            mouse: RawMouse::detached(),
            pad: pad::Pad::none(),
            focused: true,
            captured: true,
            prev_buttons: 0,
            config_dir: None,
            config_path: None,
            dirty: false,
            debug: None,
            fov_sensitivity_scale: 1.0,
        }
    }

    /// `CL_SetFOVSensitivityScale`: the mouse turns the view by this much less (or more) than the hip rate, since
    /// the view's field of view is that much narrower (or wider) than `cg_fov`. Gamepad and key turning ignore it.
    pub fn set_fov_sensitivity_scale(&mut self, scale: f32) {
        self.fov_sensitivity_scale = scale;
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
                    && let Some(n) =
                        keys::keypad_nav_name(c, &event.logical_key).or_else(|| keys::key_name(c))
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

        // `CL_KeyState`: how much of the time since the last frame each axis key was down.
        let now = self.now();
        let interval = now - std::mem::replace(&mut self.last_frame, now);
        let [fwd, back, left_m, right_m, turn_l, turn_r, look_u, look_d] = [
            "forward",
            "back",
            "moveleft",
            "moveright",
            "left",
            "right",
            "lookup",
            "lookdown",
        ]
        .map(|c| self.key_state(c, now, interval));

        let held = |s: &Self, c: &str| s.held.get(c).is_some_and(|h| h.count > 0 || h.tapped);
        let (prone_held, down_held) = (held(self, "prone"), held(self, "movedown"));
        let (strafe, mlook) = (self.active("strafe"), self.active("mlook"));
        let speed_key = self.active("speed") || self.active("speed_throw");

        // Buttons: the held commands, then stance and ADS the way `CL_KeyMove` settles them.
        let mut buttons = 0;
        let mut held_other = Vec::new();
        for (name, h) in &self.held {
            if h.count > 0 || h.tapped {
                let bits = button_bits(name);
                buttons |= bits;
                if bits == 0 && !CONSUMED_CMDS.contains(&name.as_str()) {
                    held_other.push(name.clone());
                }
            }
        }
        held_other.sort_unstable();
        if prone_held || down_held {
            // A held key asks for the stance only while it is down; the latched one is left alone.
            buttons &= !(buttons::PRONE | buttons::CROUCH);
            buttons |= if prone_held {
                buttons::PRONE
            } else {
                buttons::CROUCH
            } | buttons::TEMP_STANCE;
        } else {
            self.stance_hold_update(now);
            buttons |= self.stance;
        }
        if speed_key != self.using_ads {
            buttons |= buttons::ADS;
        }
        if self.active("back") {
            buttons &= !buttons::SPRINT;
        }
        let up = f32::from(
            i8::from(held(self, "gostand") || held(self, "gostand_stand") || held(self, "moveup"))
                - i8::from(down_held),
        );
        let cv = &self.cvars;
        let sens = cv.f32("sensitivity");
        let invert = if cv.bool("input_invertpitch") {
            -1.0
        } else {
            1.0
        };

        // Keyboard movement: whole `127ths`, as the original's chars.
        let q = |f: f32| (f * 127.0) as i32;
        let mut forward = q(fwd) - q(back);
        let mut side = q(right_m) - q(left_m);
        let strafing = strafe && buttons & buttons::SPRINT == 0;
        if strafing {
            side += q(turn_r) - q(turn_l);
        }

        let speed = dt
            * if speed_key {
                cv.f32("cl_anglespeedkey")
            } else {
                1.0
            };
        let (mut yaw, mut pitch) = (0.0, 0.0);
        // The filter's sample buffer rotates every frame, frozen or not.
        let (mut mx, mut my) = (mdx as f32, mdy as f32);
        let (px, py) = std::mem::replace(&mut self.prev_mouse, (mx, my));
        if !self.frozen {
            if !strafe {
                yaw += (turn_l - turn_r) * cv.f32("cl_yawspeed") * speed;
            }
            pitch += (look_d - look_u) * cv.f32("cl_pitchspeed") * speed;

            // `CL_MouseMove`: optional two-sample filter, acceleration by speed, then yaw/pitch or strafe/walk.
            if cv.bool("m_filter") {
                (mx, my) = ((mx + px) * 0.5, (my + py) * 0.5);
            }
            let msec = dt * 1000.0;
            let rate = if msec > 0.0 { mx.hypot(my) / msec } else { 0.0 };
            let sens = (rate * cv.f32("cl_mouseaccel") + sens) * self.fov_sensitivity_scale;
            let snap = |f: f32| f.round() as i32;
            if strafe {
                side += snap(mx * sens * cv.f32("m_side"));
            } else {
                yaw -= mx * sens * cv.f32("m_yaw");
            }
            if (mlook || cv.bool("cl_freelook")) && !strafe {
                pitch += my * sens * cv.f32("m_pitch") * invert;
            } else {
                forward -= snap(my * sens * cv.f32("m_forward"));
            }
        }

        let dz = cv.f32("in_gamepad_deadzone");
        let (lx, ly) = pad::radial_deadzone(self.pad.left.0, self.pad.left.1, dz);
        let (rx, ry) = pad::radial_deadzone(self.pad.right.0, self.pad.right.1, dz);
        yaw -= rx * cv.f32("in_gamepad_yawrate") * dt;
        pitch -= ry * cv.f32("in_gamepad_pitchrate") * dt * invert;

        let walk = if cv.bool("cl_run") { 1.0 } else { 0.5 };
        let axis = |v: i32| (v.clamp(-127, 127) as f32 / 127.0 * walk).clamp(-1.0, 1.0);
        let f = InputFrame {
            move_forward: (axis(forward) + ly).clamp(-1.0, 1.0),
            move_right: (axis(side) + lx).clamp(-1.0, 1.0),
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

    /// `CL_KeyState`: the fraction of the last frame that `cmd` was down, 0..=1. Resets the count.
    fn key_state(&mut self, cmd: &str, now: f64, interval: f64) -> f32 {
        let Some(h) = self.held.get_mut(cmd) else {
            return 0.0;
        };
        let mut down = std::mem::take(&mut h.accum);
        if let Some(since) = h.since {
            down += now - since;
            h.since = Some(now);
        }
        if interval <= 0.0 {
            // No time passed (a stopped clock): down means fully down.
            return f32::from(h.count > 0 || h.tapped);
        }
        (down / interval).clamp(0.0, 1.0) as f32
    }

    /// Run console-style input: `bind`, `set`, `+attack`, `weapnext`, ... Several commands may share a line with `;`.
    /// Commands that are not the input layer's own land in the next frame's `pending_commands`.
    pub fn exec_line(&mut self, line: &str) {
        self.exec_text(line, None, true, 0);
    }

    /// The user's folder, where the profiles live; `None` without one.
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_dir.clone()
    }

    /// Takes a profile's settings: runs `read` (its `config_mp.cfg`, the user's copy or else the install's; the stock
    /// `default_mp.cfg` has already run) and makes `save` the file [`Input::save`] writes. A profile without a file
    /// keeps what is set now.
    pub fn use_profile(&mut self, read: Option<PathBuf>, save: Option<PathBuf>) {
        if let Some(p) = read {
            match std::fs::read_to_string(&p) {
                Ok(text) => self.exec_text(&text, p.parent(), false, 0),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!("cannot read {}: {e}", p.display()),
            }
        }
        self.config_path = save;
        self.dirty = false;
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
            // The stance commands set (goprone, gocrouch) or step (the rest) the latched stance, as the original's.
            ("goprone", []) if sink => self.set_stance(buttons::PRONE),
            ("gocrouch", []) if sink => self.set_stance(buttons::CROUCH),
            ("togglecrouch", []) if sink => self.toggle_stance(buttons::CROUCH),
            ("toggleprone", []) if sink => self.toggle_stance(buttons::PRONE),
            ("lowerstance", []) if sink => self.lower_stance(),
            ("raisestance", []) if sink => self.raise_stance(),
            ("toggleads", []) if sink => self.using_ads = !self.using_ads,
            ("leaveads", []) if sink => self.using_ads = false,
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

    /// `CL_SetStance`: the stance the cgame (or a command) puts the client in. Ignored while `+prone` / `+movedown`
    /// is held, which send their own temporary stance.
    fn set_stance(&mut self, stance: u32) {
        if !self.temp_stance_held() {
            self.stance = stance;
        }
    }

    /// `CL_ToggleStance`.
    fn toggle_stance(&mut self, preferred: u32) {
        if !self.temp_stance_held() {
            self.stance = if self.stance == preferred {
                0
            } else {
                preferred
            };
        }
    }

    /// `+prone` or `+movedown` is down (`IN_IsTempStanceKeyActive`).
    fn temp_stance_held(&self) -> bool {
        self.active("prone") || self.active("movedown")
    }

    /// Down now, not merely tapped since the last frame.
    fn active(&self, cmd: &str) -> bool {
        self.held.get(cmd).is_some_and(|h| h.count > 0)
    }

    /// `IN_LowerStance`: stand -> crouch -> prone.
    fn lower_stance(&mut self) {
        if !self.temp_stance_held() {
            self.stance = match self.stance {
                0 => buttons::CROUCH,
                _ => buttons::PRONE,
            };
        }
    }

    /// `IN_RaiseStance`: prone -> crouch -> stand.
    fn raise_stance(&mut self) {
        if !self.temp_stance_held() {
            self.stance = if self.stance == buttons::PRONE {
                buttons::CROUCH
            } else {
                0
            };
        }
    }

    /// `+stance`: crouch now; held past `cl_stanceHoldTime` it goes prone (or stands, from prone) at the next frame.
    fn stance_down(&mut self) {
        if !self.temp_stance_held() {
            self.stance_hold = Some((self.stance, self.now()));
            self.stance = buttons::CROUCH;
        }
    }

    /// `-stance`: a short press from crouch stands up again.
    fn stance_up(&mut self) {
        if !self.temp_stance_held() {
            if self
                .stance_hold
                .is_some_and(|(from, _)| from == buttons::CROUCH)
            {
                self.stance = 0;
            }
            self.stance_hold = None;
        }
    }

    /// `CL_StanceButtonUpdate`.
    fn stance_hold_update(&mut self, now: f64) {
        let hold = f64::from(self.cvars.f32("cl_stanceholdtime")) / 1000.0;
        if let Some((from, since)) = self.stance_hold
            && now - since >= hold
        {
            self.stance = if from == buttons::PRONE {
                0
            } else {
                buttons::PRONE
            };
            self.stance_hold = None;
        }
    }

    /// What the cgame learned from the player state: forced stances, ADS resets, the frozen flag.
    pub fn apply(&mut self, fb: &Feedback) {
        for &s in &fb.stances {
            self.set_stance(s);
        }
        if fb.leave_ads {
            self.using_ads = false;
        }
        self.frozen = fb.frozen;
    }

    /// The input layer's clock in seconds.
    fn now(&self) -> f64 {
        self.clock
            .unwrap_or_else(|| self.start.elapsed().as_secs_f64())
    }

    /// `+cmd` / `-cmd` typed rather than bound: held until the matching `-cmd`.
    fn console_hold(&mut self, cmd: &str, press: bool) {
        if press {
            let held = self.hold(cmd);
            self.down.entry(String::new()).or_default().push(held);
        } else if let Some(started) = self.down.get_mut("")
            && let Some(i) = started.iter().position(|h| requested(h) == cmd)
        {
            let held = started.swap_remove(i);
            self.release(&held);
        }
    }

    /// Starts `cmd`; returns the name actually held (what [`Input::release`] must be given). Some presses do
    /// something other than hold a button: a `+gostand` while latched in a stance stands up instead of jumping.
    fn hold(&mut self, cmd: &str) -> String {
        let temp = self.temp_stance_held();
        let held = match cmd {
            "toggleads_throw" => {
                self.using_ads = !self.using_ads;
                cmd
            }
            "speed" | "speed_throw" => {
                self.using_ads = false;
                cmd
            }
            "gostand" if self.stance != 0 => {
                self.set_stance(0);
                "gostand_stand"
            }
            "moveup" if temp => "moveup_stance",
            "moveup" if self.stance != 0 => {
                self.stance = if self.stance == buttons::PRONE {
                    buttons::CROUCH
                } else {
                    0
                };
                "moveup_stance"
            }
            "stance" => {
                self.stance_down();
                cmd
            }
            _ => cmd,
        };
        let now = self.now();
        let h = self.held.entry(held.to_owned()).or_default();
        if h.count == 0 {
            h.since = Some(now);
        }
        h.count += 1;
        h.tapped = true;
        held.to_owned()
    }

    fn release(&mut self, cmd: &str) {
        let now = self.now();
        let Some(h) = self.held.get_mut(cmd) else {
            return;
        };
        h.count = h.count.saturating_sub(1);
        if h.count == 0 {
            if let Some(s) = h.since.take() {
                h.accum += now - s;
            }
            match cmd {
                "stance" => self.stance_up(),
                // `IN_SpeedUp` / `IN_Speed_Throw_Up`: letting go also ends a toggled ADS.
                "speed" | "speed_throw" => self.using_ads = false,
                _ => {}
            }
        }
    }

    /// A key (or button) changed state. Repeats are ignored: the key is already down.
    pub(crate) fn key(&mut self, name: &str, down: bool) {
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
                    let held = self.hold(c);
                    started.push(held);
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
        i.exec_line("bind ctrl \"+movedown\"; bind c \"+movedown\"");
        i.key("ctrl", true);
        i.key("c", true);
        i.key("ctrl", false);
        let f = frame(&mut i);
        assert_eq!(
            (f.buttons, f.up),
            (buttons::CROUCH | buttons::TEMP_STANCE, -1.0)
        );
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
        // The tap also pressed THROW for one frame (`IN_ToggleADS_Throw_Down`).
        assert_eq!(frame(&mut i).buttons, buttons::ADS | buttons::THROW);
        assert_eq!(frame(&mut i).buttons, buttons::ADS);
        i.key("k", true);
        i.key("k", false);
        // Toggled off; the tap still shows THROW for its frame.
        assert_eq!(frame(&mut i).buttons, buttons::THROW);
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

    /// An input that executed the install's `default_mp.cfg`; `None` (skip) without `COD4_PATH`.
    fn stock_input() -> Option<Input> {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return None;
        };
        let install = server::content::Install::open(Path::new(&root)).unwrap();
        let mut i = Input::bare();
        i.load_defaults(Some(&install.vfs));
        Some(i)
    }

    /// The d-pad HUD and the Controls menu look keys up by the exact command `+actionslot N`; the keys are whatever
    /// the install's own config binds.
    #[test]
    fn action_slots_come_from_the_stock_config_and_rebind() {
        let Some(mut i) = stock_input() else { return };
        for slot in 1..=4 {
            let keys = i.binding_keys(&format!("+actionslot {slot}"));
            assert_eq!(keys.len(), 1, "slot {slot}: {keys:?}");
        }
        let key6 = i.binding_keys("+actionslot 4")[0].to_owned();
        i.key(&key6, true);
        assert_eq!(frame(&mut i).pending_commands, ["actionslot 4"]);
        // The use key the cursor hint shows, and the stock stance toggles.
        assert_eq!(i.binding_keys("+activate")[0], "f");
        assert_eq!(i.bound("ctrl"), Some("goprone"));
        // What the menu's bind capture executes.
        let key1 = i.binding_keys("+actionslot 1")[0].to_owned();
        i.exec_line(&format!("unbind \"{key1}\""));
        i.exec_line("bind \"h\" \"+actionslot 1\"");
        assert_eq!(i.binding_keys("+actionslot 1"), ["h"]);
        // Binds and cvars the profile config writes back are the stock ones plus the changes.
        let text = i.config_text();
        let mut b = Input::bare();
        b.exec_text(&text, None, false, 0);
        assert_eq!(b.binding_keys("+actionslot 1"), ["h"]);
    }

    /// The grenade slots' bind labels and the throw buttons come from the install's own config.
    #[test]
    fn stock_config_binds_the_grenade_keys() {
        let Some(mut i) = stock_input() else { return };
        // The stock keys must be there; the gamepad extras and key order are incidental.
        let frag = i.binding_keys("+frag");
        assert!(frag.contains(&"g") && frag.contains(&"mouse3"), "{frag:?}");
        assert!(i.binding_keys("+smoke").contains(&"4"));
        i.key("g", true);
        assert_eq!(frame(&mut i).buttons, buttons::FRAG);
        i.key("g", false);
        i.key("4", true);
        assert_eq!(frame(&mut i).buttons, buttons::SMOKE);
    }

    #[test]
    fn stock_stance_keys_set_the_latch_and_jump_stands() {
        let Some(mut i) = stock_input() else { return };
        i.key("ctrl", true);
        i.key("ctrl", false);
        assert_eq!(frame(&mut i).buttons, buttons::PRONE);
        assert_eq!(frame(&mut i).buttons, buttons::PRONE);
        i.key("c", true);
        i.key("c", false);
        assert_eq!(frame(&mut i).buttons, buttons::CROUCH);
        // goprone / gocrouch set the stance; pressing again does not toggle it back.
        i.key("c", true);
        i.key("c", false);
        assert_eq!(frame(&mut i).buttons, buttons::CROUCH);
        // Space from a latched stance stands up and does not jump.
        i.key("space", true);
        assert_eq!(frame(&mut i).buttons, 0);
        i.key("space", false);
        i.key("space", true);
        assert_eq!(frame(&mut i).buttons, buttons::JUMP);
    }

    /// The stock MOUSE2 and the gamepad trigger: an ADS throw holds THROW as well as ADS.
    #[test]
    fn ads_throw_binds_hold_throw_and_ads() {
        let mut i = Input::detached();
        i.exec_line("bind mouse2 \"+speed_throw\"; bind mouse3 \"+toggleads_throw\"");
        i.key("mouse2", true);
        assert_eq!(frame(&mut i).buttons, buttons::ADS | buttons::THROW);
        i.key("mouse2", false);
        assert_eq!(frame(&mut i).buttons, 0);
        // Toggle: THROW only while the key is down, ADS until toggled off again.
        i.key("mouse3", true);
        assert_eq!(frame(&mut i).buttons, buttons::ADS | buttons::THROW);
        i.key("mouse3", false);
        assert_eq!(frame(&mut i).buttons, buttons::ADS);
        // Holding the speed key while toggled on flips ADS off.
        i.key("mouse2", true);
        // `IN_SpeedDown` clears the toggle, and the held speed key then asks for ADS itself.
        assert_eq!(frame(&mut i).buttons, buttons::ADS | buttons::THROW);
        i.key("mouse2", false);
        assert_eq!(frame(&mut i).buttons, 0);
        i.key("mouse3", true);
        i.key("mouse3", false);
        frame(&mut i);
        i.apply(&Feedback {
            leave_ads: true,
            ..Feedback::default()
        });
        assert_eq!(frame(&mut i).buttons, 0);
    }

    #[test]
    fn fov_sensitivity_scale_slows_mouse_look() {
        let mut i = Input::detached();
        i.exec_line("set sensitivity 2; set m_yaw 0.5; set m_pitch 0.25");
        i.set_fov_sensitivity_scale(0.5);
        i.mouse.winit_motion((10.0, 4.0));
        let f = frame(&mut i);
        assert_eq!((f.look_delta_yaw, f.look_delta_pitch), (-5.0, 1.0));
    }

    #[test]
    fn held_prone_and_movedown_send_a_temporary_stance_and_leave_the_latch() {
        let mut i = Input::detached();
        i.exec_line("gocrouch; bind z \"+prone\"; bind x \"+movedown\"");
        i.key("z", true);
        assert_eq!(frame(&mut i).buttons, buttons::PRONE | buttons::TEMP_STANCE);
        i.key("z", false);
        assert_eq!(frame(&mut i).buttons, buttons::CROUCH);
        i.key("x", true);
        assert_eq!(
            frame(&mut i).buttons,
            buttons::CROUCH | buttons::TEMP_STANCE
        );
        // Forced stances are ignored while a temporary stance key is down.
        i.apply(&Feedback {
            stances: vec![0],
            ..Feedback::default()
        });
        i.key("x", false);
        assert_eq!(frame(&mut i).buttons, buttons::CROUCH);
    }

    /// Sprinting out of prone (or a ladder) raises `EV_STANCE_FORCE_STAND`: the latch must give way, so the next
    /// `goprone` goes prone again.
    #[test]
    fn a_forced_stance_resets_the_latch() {
        let mut i = Input::detached();
        i.exec_line("goprone");
        assert_eq!(frame(&mut i).buttons, buttons::PRONE);
        i.apply(&Feedback {
            stances: vec![0],
            ..Feedback::default()
        });
        assert_eq!(frame(&mut i).buttons, 0);
        i.exec_line("goprone");
        assert_eq!(frame(&mut i).buttons, buttons::PRONE);
        i.apply(&Feedback {
            stances: vec![buttons::CROUCH],
            ..Feedback::default()
        });
        assert_eq!(frame(&mut i).buttons, buttons::CROUCH);
    }

    #[test]
    fn stance_commands_step_toggle_and_hold() {
        let mut i = Input::detached();
        let stance = |i: &mut Input| frame(i).buttons;
        i.exec_line("lowerstance");
        assert_eq!(stance(&mut i), buttons::CROUCH);
        i.exec_line("lowerstance");
        i.exec_line("lowerstance");
        assert_eq!(stance(&mut i), buttons::PRONE);
        i.exec_line("raisestance");
        assert_eq!(stance(&mut i), buttons::CROUCH);
        i.exec_line("togglecrouch");
        assert_eq!(stance(&mut i), 0);
        i.exec_line("toggleprone");
        assert_eq!(stance(&mut i), buttons::PRONE);
        i.exec_line("toggleprone");
        assert_eq!(stance(&mut i), 0);
        // +stance: a short press from standing crouches and a second one stands; a long one goes prone.
        i.exec_line("+stance");
        assert_eq!(stance(&mut i), buttons::CROUCH);
        i.exec_line("-stance");
        assert_eq!(stance(&mut i), buttons::CROUCH);
        i.exec_line("+stance");
        i.exec_line("-stance");
        assert_eq!(stance(&mut i), 0);
        i.exec_line("+stance");
        i.clock = Some(0.31);
        assert_eq!(stance(&mut i), buttons::PRONE);
        i.exec_line("-stance");
        assert_eq!(stance(&mut i), buttons::PRONE);
    }

    #[test]
    fn keyboard_axes_follow_the_fraction_of_the_frame_a_key_was_down() {
        let mut i = Input::detached();
        i.clock = Some(1.0);
        frame(&mut i);
        // Down for the whole of the next frame.
        i.key("w", true);
        i.clock = Some(1.1);
        assert_eq!(frame(&mut i).move_forward, 1.0);
        // Down for the first quarter of the frame, then released.
        i.clock = Some(1.2);
        i.key("w", false);
        i.clock = Some(1.3);
        let f = frame(&mut i);
        assert_eq!(
            f.move_forward,
            63.0 / 127.0,
            "the key was down half of 1.1..1.3, in whole 127ths"
        );
        // A tap with no time on the clock does nothing once time passes.
        i.key("d", true);
        i.key("d", false);
        i.clock = Some(1.4);
        assert_eq!(frame(&mut i).move_right, 0.0);
        // Turning is fractional too.
        i.exec_line("set cl_yawspeed 100");
        i.key("leftarrow", true);
        i.clock = Some(1.45);
        i.key("leftarrow", false);
        i.clock = Some(1.5);
        let yaw = i.frame(0.1).look_delta_yaw;
        assert!((yaw - 100.0 * 0.1 * 0.5).abs() < 1e-3, "{yaw}");
    }

    #[test]
    fn sprint_is_ignored_while_back_is_held() {
        let mut i = Input::detached();
        i.key("shift", true);
        assert_eq!(frame(&mut i).buttons, buttons::SPRINT | buttons::BREATH);
        i.key("s", true);
        assert_eq!(frame(&mut i).buttons, buttons::BREATH);
    }

    #[test]
    fn strafe_mouse_acceleration_filter_and_freelook() {
        let mut i = Input::detached();
        i.exec_line("set sensitivity 1; set m_yaw 1; set m_pitch 1");
        // Acceleration: faster motion turns further (rate = counts per millisecond).
        i.exec_line("set cl_mouseaccel 1");
        i.mouse.winit_motion((10.0, 0.0));
        let slow = i.frame(0.1).look_delta_yaw;
        i.mouse.winit_motion((10.0, 0.0));
        let fast = i.frame(0.01).look_delta_yaw;
        assert!(fast < slow, "{fast} {slow}");
        i.exec_line("set cl_mouseaccel 0");
        // Filter: the previous frame's motion is averaged in.
        i.exec_line("set m_filter 1");
        frame(&mut i);
        i.mouse.winit_motion((10.0, 0.0));
        assert_eq!(i.frame(0.01).look_delta_yaw, -5.0);
        i.exec_line("set m_filter 0");
        // freelook off: vertical motion walks instead of looking.
        i.exec_line("set cl_freelook 0");
        i.mouse.winit_motion((0.0, 40.0));
        let f = frame(&mut i);
        assert_eq!(f.look_delta_pitch, 0.0);
        assert!(f.move_forward < 0.0, "{}", f.move_forward);
        i.exec_line("set cl_freelook 1; bind mouse4 \"+strafe\"");
        // +strafe: horizontal motion strafes and yaw stays.
        i.key("mouse4", true);
        i.mouse.winit_motion((40.0, 0.0));
        let f = frame(&mut i);
        assert_eq!(f.look_delta_yaw, 0.0);
        assert!(f.move_right > 0.0);
    }

    #[test]
    fn releasing_the_speed_key_ends_a_toggled_ads() {
        let mut i = Input::detached();
        i.exec_line("bind mouse3 \"+toggleads_throw\"; bind mouse2 \"+speed_throw\"");
        i.key("mouse3", true);
        i.key("mouse3", false);
        frame(&mut i);
        assert!(i.using_ads);
        i.key("mouse2", true);
        i.key("mouse2", false);
        assert!(!i.using_ads);
    }

    #[test]
    fn the_mouse_filter_keeps_rotating_while_frozen() {
        let mut i = Input::detached();
        i.exec_line("set sensitivity 1; set m_yaw 1; set m_filter 1");
        i.apply(&Feedback {
            frozen: true,
            ..Feedback::default()
        });
        i.mouse.winit_motion((10.0, 0.0));
        frame(&mut i);
        i.apply(&Feedback::default());
        frame(&mut i);
        // The frozen frame's motion is no longer a sample two frames on.
        i.mouse.winit_motion((10.0, 0.0));
        assert_eq!(frame(&mut i).look_delta_yaw, -5.0);
    }

    #[test]
    fn a_frozen_player_cannot_look() {
        let mut i = Input::detached();
        i.apply(&Feedback {
            frozen: true,
            ..Feedback::default()
        });
        i.mouse.winit_motion((10.0, 10.0));
        i.key("leftarrow", true);
        let f = frame(&mut i);
        assert_eq!((f.look_delta_yaw, f.look_delta_pitch), (0.0, 0.0));
        i.apply(&Feedback::default());
        i.mouse.winit_motion((10.0, 10.0));
        assert_ne!(frame(&mut i).look_delta_yaw, 0.0);
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
    fn the_profile_config_runs_after_the_defaults_and_saves_to_its_own_file() {
        let dir = std::env::temp_dir().join(format!("cod4e-input-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (theirs, mine) = (dir.join("install.cfg"), dir.join("users/p/config_mp.cfg"));
        std::fs::write(
            &theirs,
            "unbindall\nbind w \"+profile\"\nseta sensitivity 4\n",
        )
        .unwrap();
        let mut i = Input::detached();
        assert_eq!(i.bound("w"), Some("+forward"), "the defaults came first");
        i.use_profile(Some(theirs.clone()), Some(mine.clone()));
        assert_eq!(
            (i.bound("w"), i.cvar("sensitivity")),
            (Some("+profile"), Some("4"))
        );
        i.save().unwrap();
        assert!(!mine.exists(), "reading a profile changes nothing to write");
        i.exec_line("seta sensitivity 6");
        i.save().unwrap();
        assert!(
            std::fs::read_to_string(&mine)
                .unwrap()
                .contains("seta sensitivity \"6\"")
        );
        assert!(
            std::fs::read_to_string(&theirs)
                .unwrap()
                .contains("sensitivity 4"),
            "the install's file is never written"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        let mut i = Input::new(
            Some(std::env::temp_dir().join("cod4e-no-such-config.cfg")),
            None,
        );
        assert_eq!(i.look_gate(), Some("pointer free"));
        i.mouse.winit_motion((10.0, 10.0));
        assert_eq!(frame(&mut i).look_delta_yaw, 0.0);
    }
}
