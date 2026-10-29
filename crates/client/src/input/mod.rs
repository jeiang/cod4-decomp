// SPDX-License-Identifier: GPL-3.0-only
// The command buffer follows KisakCOD (qcommon/cmd.cpp: Cmd_Wait_f, Cmd_Vstr_f, Cmd_Exec_f, Cmd_ExecFromDisk; universal/dvar_cmds.cpp: Dvar_ToggleInternal, Dvar_List_f, Dvar_Dump_f; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
mod event;
mod feedback;
mod keys;

pub use event::InputEvent;

/// The input layer's name of a physical key, for binding it from a menu.
pub fn physical_key_name(code: winit::keyboard::KeyCode) -> Option<String> {
    keys::key_name(code).map(str::to_owned)
}

/// The physical key a bind name (`w`, `escape`, `~`) is, for pressing it by name.
pub fn key_code(name: &str) -> Option<winit::keyboard::KeyCode> {
    keys::key_code(name)
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
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web_time::Instant;
use winit::event::ElementState;

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
    /// The mouse's part of the look deltas (what `CL_MouseMove` adds), so the turn-rate caps can treat it apart from
    /// key and gamepad turning.
    pub mouse_delta_yaw: f32,
    pub mouse_delta_pitch: f32,
    /// The same turning as the look deltas but without the FOV sensitivity scale of the mouse: what moves a cursor
    /// (the location selection) rather than the view.
    pub cursor_yaw: f32,
    pub cursor_pitch: f32,
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
    /// A menu waits for a key to bind: a pad button goes to [`Input::take_pad_press`] instead of the bind table.
    pub capture_pad: bool,
    pad_press: Option<String>,
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
    /// The install's files, for `exec`.
    vfs: Option<Vfs>,
    /// The command buffer behind a `wait`: what is left of the buffer that waited, then every command that came in
    /// since. `wait` is the frames still to wait (`cmd_wait`).
    backlog: VecDeque<Item>,
    wait: Option<u32>,
    /// What commands said, for the console (see [`Input::take_messages`]).
    messages: Vec<String>,
}

const MAX_EXEC_DEPTH: u32 = 8;
/// `vstr` expansions in one run of a buffer (a cvar that runs itself stops here).
const MAX_EXPANSIONS: u32 = 1024;
/// The longest name `exec` looks up in the install's files (`Cmd_Exec_f`'s buffer).
const MAX_EXEC_NAME: usize = 63;
const MAX_MESSAGES: usize = 8192;

/// One command of a buffer: how deep in `exec`s it is, the folder a disk file it came from sits in, and whether
/// the player typed it at the console (where a cvar's name alone shows it and an unknown word is said).
pub(crate) struct Item {
    pub(crate) toks: Vec<String>,
    pub(crate) depth: u32,
    pub(crate) dir: Option<Arc<Path>>,
    pub(crate) console: bool,
}

impl Item {
    pub(crate) fn new(toks: Vec<String>, depth: u32, dir: Option<Arc<Path>>) -> Self {
        Self {
            toks,
            depth,
            dir,
            console: false,
        }
    }
}

/// The commands of `text`, one buffer.
pub(crate) fn items(text: &str, depth: u32, dir: Option<&Arc<Path>>) -> VecDeque<Item> {
    text.lines()
        .flat_map(config::split_commands)
        .map(|toks| Item::new(toks, depth, dir.cloned()))
        .collect()
}

/// The text of the config kept at `path`: a file, or in a browser what the page stored under that name.
fn read_config(path: &Path) -> std::io::Result<String> {
    #[cfg(not(target_arch = "wasm32"))]
    return std::fs::read_to_string(path);
    #[cfg(target_arch = "wasm32")]
    crate::web::pref_get(&path.to_string_lossy()).ok_or_else(|| std::io::ErrorKind::NotFound.into())
}

/// Keeps `text` as the config at `path`: a file, or in a browser the page's storage under that name.
fn write_config(path: &Path, text: &str) -> std::io::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    }
    #[cfg(target_arch = "wasm32")]
    crate::web::pref_set(&path.to_string_lossy(), text)
        .map_err(|e| std::io::Error::other(format!("{e:?}")))
}

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
    pub fn new(config_dir: Option<PathBuf>, vfs: Option<Vfs>) -> Self {
        let mut i = Self::bare();
        i.vfs = vfs;
        i.load_defaults();
        i.mouse = RawMouse::new();
        i.clock = None;
        i.pad = pad::Pad::new();
        i.config_dir = config_dir.or_else(config::default_dir);
        #[cfg(not(target_arch = "wasm32"))]
        if i.config_dir.is_none() {
            tracing::warn!("no config folder found: settings and profiles will not be saved");
        }
        i.dirty = false;
        // The window takes the pointer on the first click; until then the cursor is free.
        i.captured = false;
        if std::env::var_os("COD4E_INPUT_DEBUG").is_some_and(|v| v == "1") {
            i.debug = Some(DebugCounter::new(rawmouse::Totals::default()));
        }
        i
    }

    /// [`Input::detached`] with the install's files, for what a script `exec`s.
    #[cfg(test)]
    pub(crate) fn detached_with(vfs: Vfs) -> Self {
        let mut i = Self::bare();
        i.vfs = Some(vfs);
        i.load_defaults();
        i
    }

    /// Fallback binds and cvars only; no devices, no file, no install. Focused and cursor-captured.
    pub fn detached() -> Self {
        let mut i = Self::bare();
        i.load_defaults();
        i
    }

    /// `default_mp.cfg` from the install, or the small built-in fallback without one, then the extra binds.
    fn load_defaults(&mut self) {
        let stock = self
            .vfs
            .as_ref()
            .is_some_and(|v| v.locate("default_mp.cfg").is_some());
        if stock {
            // `Com_ExecStartupConfigs`: default_mp.cfg, then language.cfg (the profile's config follows).
            self.exec_text("exec default_mp.cfg\nexec language.cfg\n", None, false, 0);
        } else {
            self.exec_text(include_str!("fallback_mp.cfg"), None, false, 0);
        }
        self.exec_text(include_str!("extra_binds.cfg"), None, false, 0);
        self.dirty = false;
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
            capture_pad: false,
            pad_press: None,
            focused: true,
            captured: true,
            prev_buttons: 0,
            config_dir: None,
            config_path: None,
            dirty: false,
            debug: None,
            fov_sensitivity_scale: 1.0,
            vfs: None,
            backlog: VecDeque::new(),
            wait: None,
            messages: Vec::new(),
        }
    }

    /// `CL_SetFOVSensitivityScale`: the mouse turns the view by this much less (or more) than the hip rate, since
    /// the view's field of view is that much narrower (or wider) than `cg_fov` (or the shell shock's look control slows
    /// it). Gamepad and key turning ignore it.
    pub fn set_fov_sensitivity_scale(&mut self, scale: f32) {
        self.fov_sensitivity_scale = scale;
    }

    /// Tell the input whether the cursor is grabbed. Mouse look only applies while it is.
    pub fn set_captured(&mut self, captured: bool) {
        if self.debug.is_some() && captured != self.captured {
            tracing::info!(captured, "input: pointer lock");
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

    /// One key press or release, from the window system or the control channel, through the bind table.
    pub fn stroke(
        &mut self,
        code: winit::keyboard::KeyCode,
        logical: Option<&winit::keyboard::Key>,
        pressed: bool,
        repeat: bool,
    ) {
        if repeat {
            return;
        }
        let named = match logical {
            Some(l) => keys::keypad_nav_name(code, l).or_else(|| keys::key_name(code)),
            None => keys::key_name(code),
        };
        if let Some(n) = named {
            self.key(n, pressed);
        }
    }

    /// A mouse button press or release, through the bind table (`mouse1`, ...).
    pub fn mouse_button(&mut self, button: winit::event::MouseButton, pressed: bool) {
        self.key(&keys::mouse_name(button), pressed);
    }

    /// The wheel turned `y` lines (positive up): a pulse of `mwheelup` or `mwheeldown`.
    pub fn wheel(&mut self, y: f32) {
        if y != 0.0 {
            self.pulse(if y > 0.0 { "mwheelup" } else { "mwheeldown" });
        }
    }

    /// Raw pointer motion, what a captured pointer's look reads.
    pub fn mouse_motion(&mut self, dx: f64, dy: f64) {
        self.mouse.winit_motion((dx, dy));
    }

    /// The window's keyboard focus, as `WindowEvent::Focused` reports it.
    pub fn focus(&mut self, focused: bool) {
        if self.debug.is_some() {
            tracing::info!(focused, "input: window focus");
        }
        self.focused = focused;
        if !focused {
            self.release_all();
        }
    }

    /// One gamepad button edge, as the pad backend would report it.
    pub fn pad_button(&mut self, name: &str, down: bool) {
        self.pad_event(name, down);
    }

    /// A gamepad stick's position (`left` or `right`), -1 to 1 on each axis.
    pub fn pad_axis(&mut self, stick: &str, dx: f32, dy: f32) {
        match stick {
            "left" => self.pad.left = (dx, dy),
            "right" => self.pad.right = (dx, dy),
            other => tracing::warn!(stick = other, "control: no such stick"),
        }
    }

    /// One engine input event, from the window or the control channel: the game's half of it (the bind table).
    ///
    /// The menus and the console read the same events first (`crate::app::dispatch_input`).
    pub fn event(&mut self, ev: &InputEvent) {
        match ev {
            InputEvent::Key {
                code,
                logical,
                state,
                repeat,
                ..
            } => self.stroke(
                *code,
                logical.as_ref(),
                *state == ElementState::Pressed,
                *repeat,
            ),
            InputEvent::Button { button, state } => {
                self.mouse_button(*button, *state == ElementState::Pressed);
            }
            InputEvent::Wheel { y } => self.wheel(*y),
            InputEvent::Motion { dx, dy } => self.mouse_motion(*dx, *dy),
            InputEvent::Pad { name, down } => self.pad_button(name, *down),
            InputEvent::PadAxis { stick, dx, dy } => self.pad_axis(stick, *dx, *dy),
            InputEvent::Focus(f) => self.focus(*f),
            // The pointer's position is the menus'; the game reads the raw motion above. Text is the console's and
            // the menus' too: nothing in the game types.
            InputEvent::Cursor { .. } | InputEvent::Text(_) => {}
        }
    }

    /// Feed winit's raw motion (used when the platform has no better source).
    pub fn device_event(&mut self, ev: &winit::event::DeviceEvent) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = ev {
            self.mouse.winit_motion(*delta);
        }
    }

    /// One gamepad button edge. While [`Input::capture_pad`] is set a press is held back for the bind menu, `pad_start`
    /// excepted: it stays the menu key that cancels the capture.
    fn pad_event(&mut self, name: &str, down: bool) {
        if self.capture_pad && down && name != "pad_start" {
            self.pad_press.get_or_insert_with(|| name.to_owned());
            return;
        }
        self.key(name, down);
    }

    /// The pad button pressed while a bind was being captured.
    pub fn take_pad_press(&mut self) -> Option<String> {
        self.pad_press.take()
    }

    /// Everything the player asked for since the previous call.
    pub fn frame(&mut self, dt: f32) -> InputFrame {
        for (name, down) in self.pad.poll() {
            if self.focused && self.cvars.bool("in_gamepad") {
                self.pad_event(name, down);
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
        // The mouse's share of the turning at the FOV-scaled sensitivity.
        let (mut mouse_yaw, mut mouse_pitch) = (0.0, 0.0);
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
                mouse_yaw -= mx * sens * cv.f32("m_yaw");
            }
            if (mlook || cv.bool("cl_freelook")) && !strafe {
                mouse_pitch += my * sens * cv.f32("m_pitch") * invert;
            } else {
                forward -= snap(my * sens * cv.f32("m_forward"));
            }
        }

        yaw += mouse_yaw;
        pitch += mouse_pitch;
        let unscale = 1.0 / self.fov_sensitivity_scale.max(1e-6) - 1.0;
        let (cursor_yaw, cursor_pitch) = (yaw + mouse_yaw * unscale, pitch + mouse_pitch * unscale);

        let dz = cv.f32("in_gamepad_deadzone");
        let (lx, ly) = pad::radial_deadzone(self.pad.left.0, self.pad.left.1, dz);
        let (rx, ry) = pad::radial_deadzone(self.pad.right.0, self.pad.right.1, dz);
        let (pad_yaw, pad_pitch) = (
            -rx * cv.f32("in_gamepad_yawrate") * dt,
            -ry * cv.f32("in_gamepad_pitchrate") * dt * invert,
        );
        yaw += pad_yaw;
        pitch += pad_pitch;

        let walk = if cv.bool("cl_run") { 1.0 } else { 0.5 };
        let axis = |v: i32| (v.clamp(-127, 127) as f32 / 127.0 * walk).clamp(-1.0, 1.0);
        let f = InputFrame {
            move_forward: (axis(forward) + ly).clamp(-1.0, 1.0),
            move_right: (axis(side) + lx).clamp(-1.0, 1.0),
            up,
            look_delta_yaw: yaw,
            look_delta_pitch: pitch,
            mouse_delta_yaw: mouse_yaw,
            mouse_delta_pitch: mouse_pitch,
            cursor_yaw: cursor_yaw + pad_yaw,
            cursor_pitch: cursor_pitch + pad_pitch,
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
            match read_config(&p) {
                Ok(text) => self.exec_text(&text, p.parent(), false, 0),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(path = %p.display(), "cannot read the profile config: {e}")
                }
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
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.config_path.clone().filter(|_| self.dirty) else {
            return Ok(());
        };
        write_config(&path, &self.config_text())?;
        self.dirty = false;
        Ok(())
    }

    /// Whether settings or binds changed since the config was read or saved.
    #[cfg(target_arch = "wasm32")]
    pub fn unsaved(&self) -> bool {
        self.dirty && self.config_path.is_some()
    }

    /// Every key bind the config holds: key name to command, in name order.
    pub fn all_binds(&self) -> impl Iterator<Item = (&str, &str)> {
        self.binds.iter().map(|(k, c)| (k.as_str(), c.as_str()))
    }

    /// Every cvar and its value, as the console's `dvarlist` would print them.
    pub fn all_cvars(&self) -> Vec<(String, String)> {
        self.cvars
            .entries(None)
            .into_iter()
            .map(|(n, v, _)| (n, v))
            .collect()
    }

    /// The lines commands have printed since the last frame (`take_messages` drains them).
    pub fn messages(&self) -> &[String] {
        &self.messages
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
        let dir: Option<Arc<Path>> = dir.map(Arc::from);
        let cmds = items(text, depth, dir.as_ref());
        self.submit(cmds, sink, None);
    }

    /// New commands join the buffer behind a `wait` instead of running ahead of it (`Cbuf_AddText`).
    fn submit(&mut self, cmds: VecDeque<Item>, sink: bool, held: Option<&mut Vec<String>>) {
        if sink && self.busy() {
            // A key's `+command` queued behind the wait: its release queues behind it too (stock sends
            // `-command <key>` through the same buffer).
            if let Some(held) = held {
                for c in &cmds {
                    let n = c.toks[0].to_ascii_lowercase();
                    if n.starts_with('+') && n != "+actionslot" {
                        held.push(format!("\0{}", &n[1..]));
                    }
                }
            }
            self.backlog.extend(cmds);
        } else {
            self.run(cmds, sink, held);
        }
    }

    /// A `wait` is pending, so new commands queue behind it.
    pub fn busy(&self) -> bool {
        self.wait.is_some() || !self.backlog.is_empty()
    }

    /// Adds commands to the buffer behind a `wait`.
    pub(crate) fn enqueue(&mut self, cmds: impl IntoIterator<Item = Item>) {
        self.backlog.extend(cmds);
    }

    /// `Cbuf_Execute` over one command buffer. `vstr` and `exec` put their commands at the front of what is left,
    /// so a `wait` anywhere in them holds the whole rest back for later frames (interactive only). With `held`, a
    /// `+command` starts a button that the caller releases (a bound key); without, it is held until its `-command`.
    fn run(&mut self, mut cmds: VecDeque<Item>, sink: bool, mut held: Option<&mut Vec<String>>) {
        let mut expansions = 0;
        while let Some(Item {
            toks, depth, dir, ..
        }) = cmds.pop_front()
        {
            let name = toks[0].to_ascii_lowercase();
            match (name.as_str(), held.as_deref_mut()) {
                ("wait", _) if sink => {
                    // `cmd_wait = atoi(arg)`: the buffer stops until that many frames have passed.
                    let n = toks.get(1).map_or(1, |a| config::atoi(a));
                    if n > 0 {
                        self.defer(n.unsigned_abs(), cmds);
                        return;
                    }
                }
                ("vstr" | "exec", _) => {
                    let exec = name == "exec";
                    expansions += 1;
                    if (exec && depth >= MAX_EXEC_DEPTH) || expansions > MAX_EXPANSIONS {
                        tracing::warn!(command = name.as_str(), depth, "too many nested commands");
                        self.say(format!("{name}: too many nested commands"));
                        if exec {
                            continue;
                        }
                        return;
                    }
                    if let Some((next, from)) = self.expand(&toks, dir.as_deref(), sink) {
                        let dir = from.map(Arc::from).or_else(|| dir.clone());
                        let depth = depth + u32::from(exec);
                        for toks in next.into_iter().rev() {
                            cmds.push_front(Item::new(toks, depth, dir.clone()));
                        }
                    }
                }
                ("+actionslot", Some(_)) => {
                    // A press, not a held button: the game selects the slot's weapon once.
                    self.pending.push(config::join(&toks).replacen('+', "", 1));
                }
                (_, Some(started)) if toks[0].starts_with('+') => {
                    let h = self.hold(&toks[0][1..]);
                    started.push(h);
                }
                _ => self.exec_tokens(&toks, sink),
            }
        }
    }

    /// Console output for the shell to print (what a command says: a missing file, a cvar list).
    pub fn say(&mut self, text: impl Into<String>) {
        if self.messages.len() < MAX_MESSAGES {
            self.messages.push(text.into());
        }
    }

    /// What the commands said since the last call, one line per entry.
    pub fn take_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.messages)
    }

    /// The commands a `vstr <cvar>` or `exec <file>` stands for: they run in its place, in the buffer it is in, so a
    /// `wait` among them holds the rest of that buffer (`Cbuf_InsertText`). Also the folder of a disk file, for the
    /// files it runs. `None` (after saying why) when there is nothing to run.
    pub fn expand(
        &mut self,
        toks: &[String],
        dir: Option<&Path>,
        sink: bool,
    ) -> Option<(Vec<Vec<String>>, Option<PathBuf>)> {
        let name = toks[0].to_ascii_lowercase();
        let (text, from) = match (name.as_str(), &toks[1..]) {
            ("vstr", [var]) => {
                let Some(text) = self.cvars.get(var).map(str::to_owned) else {
                    self.say(format!("{var} doesn't exist"));
                    return None;
                };
                (text, None)
            }
            ("vstr", _) => {
                self.say("vstr <variablename> : execute a variable command");
                return None;
            }
            ("exec", [file]) => self.read_exec(file, dir, sink)?,
            _ => {
                self.say("exec <filename> : execute a script file");
                return None;
            }
        };
        let cmds = text.lines().flat_map(config::split_commands).collect();
        Some((cmds, from))
    }

    /// `wait` met: `cmds`, the rest of the buffer, now wait (`cmd_wait`); what came in meanwhile follows it.
    pub(crate) fn defer(&mut self, frames: u32, mut cmds: VecDeque<Item>) {
        cmds.append(&mut self.backlog);
        self.backlog = cmds;
        self.wait = Some(frames.saturating_sub(1));
    }

    /// Once a frame: the buffer behind a `wait` once the wait has run out (`Cbuf_Execute` checks `cmd_wait` first).
    /// The caller runs it through its own command handler.
    pub(crate) fn take_due(&mut self) -> Option<VecDeque<Item>> {
        // `Cbuf_Execute` counts the wait down only while text is pending.
        if self.backlog.is_empty() {
            return None;
        }
        match self.wait {
            Some(n) if n > 0 => {
                self.wait = Some(n - 1);
                None
            }
            _ => {
                self.wait = None;
                Some(std::mem::take(&mut self.backlog))
            }
        }
    }

    /// [`Input::take_due`], run here: for an input with no shell on top.
    pub fn run_due(&mut self) {
        if let Some(cmds) = self.take_due() {
            self.run(cmds, true, None);
        }
    }

    /// The text of `exec <name>`: the install's files first (the IWDs hold the stock configs; a name past the
    /// original's 63 characters is cut there), then the disk (the user's own, by the path given, relative to
    /// `dir`). `.cfg` is added to a name without an extension. Also the folder a disk file sits in, for the files
    /// it runs. Says `couldn't exec` when there is none.
    fn read_exec(
        &mut self,
        name: &str,
        dir: Option<&Path>,
        sink: bool,
    ) -> Option<(String, Option<PathBuf>)> {
        let in_install = config::exec_name(&name.chars().take(MAX_EXEC_NAME).collect::<String>());
        let stock = self
            .vfs
            .as_ref()
            .and_then(|v| v.read(&in_install).ok().flatten());
        if let Some(bytes) = stock {
            return Some((String::from_utf8_lossy(&bytes).into_owned(), None));
        }
        let file = config::exec_name(name);
        if name.split(['/', '\\']).any(|p| p == "..") {
            tracing::warn!(name, "couldn't exec: path escapes the config folder");
            self.say(format!("couldn't exec {name}"));
            return None;
        }
        let path = dir.map_or_else(|| PathBuf::from(&file), |d| d.join(&file));
        match std::fs::read_to_string(&path) {
            Ok(t) => {
                if sink {
                    self.say(format!("execing {file} from disk"));
                }
                Some((t, path.parent().map(Path::to_path_buf)))
            }
            Err(e) => {
                tracing::warn!(name, path = %path.display(), "couldn't exec: {e}");
                self.say(format!("couldn't exec {name}"));
                None
            }
        }
    }

    /// `toggle <cvar> [values...]` (`Dvar_ToggleInternal`): with no values a number flips between 0 and 1; with
    /// values it takes the one after the current (numbers compare as numbers), or the first. The cvar's domain
    /// (a float's range, an enum's names) is not modelled: this store keeps text.
    fn toggle(&mut self, name: &str, values: &[&str]) {
        let Some(current) = self.cvars.get(name).map(str::to_owned) else {
            self.say(format!("toggle failed: dvar '{name}' not found."));
            return;
        };
        let same = |a: &str, b: &str| {
            a.eq_ignore_ascii_case(b)
                || matches!((a.trim().parse::<f64>(), b.trim().parse::<f64>()), (Ok(x), Ok(y)) if x == y)
        };
        let next = match values {
            [] => match current.trim().parse::<f64>() {
                Ok(0.0) => "1",
                Ok(_) => "0",
                Err(_) if current.trim().is_empty() => "1",
                Err(_) => {
                    self.say(format!(
                        "'toggle' with no arguments makes no sense for dvar '{name}'"
                    ));
                    return;
                }
            },
            [first, ..] => values[..values.len() - 1]
                .iter()
                .position(|v| same(v, &current))
                .map_or(*first, |i| values[i + 1]),
        };
        self.set_from_command(name, next, false);
    }

    /// A `set`, `toggle` or bare-name write from a console, bind, menu or config (`Dvar_SetCommand`, an external
    /// source): a read-only cvar is refused with the stock line. `true` when the value was set.
    fn set_from_command(&mut self, name: &str, value: &str, archive: bool) -> bool {
        if cvar::Cvars::read_only(name) {
            self.say(format!("{} is read only.", name.to_ascii_lowercase()));
            return false;
        }
        self.cvars.set(name, value, archive);
        self.dirty = true;
        true
    }

    /// `Dvar_Command`: a known cvar's name alone says its value, and with words after it sets it. `false` when
    /// there is no such cvar (the word is a command for someone else, or unknown).
    fn dvar_command(&mut self, name: &str, args: &[&str]) -> bool {
        let Some(value) = self.cvars.get(name).map(str::to_owned) else {
            return false;
        };
        if args.is_empty() {
            self.say(format!("\"{name}\" is \"{value}\""));
        } else {
            self.set_from_command(name, &args.join(" "), false);
        }
        true
    }

    /// `dvarlist [match]`: a line per cvar (`A` marks an archived one) and the count.
    fn dvar_list(&mut self, filter: Option<&str>) {
        let rows = self.cvars.entries(filter);
        for (name, value, archive) in &rows {
            let a = if *archive { 'A' } else { ' ' };
            self.say(format!("    {a}   {name} \"{value}\""));
        }
        self.say(format!("\n{} total dvars", rows.len()));
    }

    /// `dvardump [match]`: the same with the stock banner.
    fn dvar_dump(&mut self, filter: Option<&str>) {
        let rows = self.cvars.entries(filter);
        self.say(
            "=============================== DVAR DUMP ========================================",
        );
        for (name, value, _) in &rows {
            self.say(format!("      {name} \"{value}\""));
        }
        let n = self.cvars.entries(None).len();
        self.say(format!("\n{} total dvars\n{n} dvar indexes", rows.len()));
        self.say(
            "=============================== END DVAR DUMP =====================================",
        );
    }

    /// `sink`: unknown commands go to `pending` (interactive); off while loading files.
    fn exec_tokens(&mut self, toks: &[String], sink: bool) {
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
                self.set_from_command(n, v, name == "seta");
            }
            ("toggle", [n, values @ ..]) => self.toggle(n, values),
            ("toggle", []) => self.say("USAGE: toggle <variable> <optional value sequence>"),
            ("dvarlist", []) => self.dvar_list(None),
            ("dvarlist", [m]) => self.dvar_list(Some(m)),
            ("dvardump", []) => self.dvar_dump(None),
            ("dvardump", [m]) => self.dvar_dump(Some(m)),
            ("bind" | "unbind" | "unbindall" | "set" | "seta" | "sets" | "setu", _) => {
                // Verb only: a `set` line may carry a password.
                tracing::warn!(command = name.as_str(), args = args.len(), "bad arguments");
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
            // A bare cvar name shows or sets it (`Dvar_Command`, which `Cmd_ExecuteSingleCommand` tries after the
            // commands); the command the input layer does not run goes to the pending list, whose consumers (the
            // app, the shell, the net play) carry it out or, for a word nothing knows, forward it to the server
            // or say `Unknown command` (`CL_ForwardCommandToServer`).
            _ if sink => {
                if !crate::console::is_command(&name) && self.dvar_command(&name, &args) {
                    return;
                }
                self.pending.push(config::join(toks))
            }
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
        } else {
            // `-cmd` with no key (`IN_KeyUp`): the button is let go whoever held it, a bound key included.
            self.release_all_of(cmd);
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

    /// Lets go of `cmd` however many keys hold it.
    fn release_all_of(&mut self, cmd: &str) {
        while self.held.get(cmd).is_some_and(|h| h.count > 0) {
            self.release(cmd);
        }
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
                match c.strip_prefix('\0') {
                    Some(n) => {
                        let up = Item::new(vec![format!("-{n}")], 0, None);
                        self.submit(VecDeque::from([up]), true, None);
                    }
                    None => self.release(&c),
                }
            }
            return;
        }
        if self.down.contains_key(name) {
            return;
        }
        let mut started = Vec::new();
        if let Some(line) = self.binds.get(name).cloned() {
            let cmds = items(&line, 0, None);
            self.submit(cmds, true, Some(&mut started));
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
            tracing::info!(
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
    fn a_pad_button_pressed_while_a_bind_waits_is_the_binding_not_a_command() {
        let mut i = Input::detached();
        i.exec_line("bind pad_a \"+forward\"");
        i.capture_pad = true;
        i.pad_event("pad_a", true);
        assert_eq!(i.take_pad_press().as_deref(), Some("pad_a"));
        assert_eq!(i.take_pad_press(), None);
        assert_eq!(
            frame(&mut i).move_forward,
            0.0,
            "the press did not run its bind"
        );
        // Start still reaches the menu key that cancels, and with no bind waiting a press is a key again.
        i.pad_event("pad_start", true);
        assert_eq!(i.take_pad_press(), None);
        i.capture_pad = false;
        i.pad_event("pad_a", true);
        assert_eq!(frame(&mut i).move_forward, 1.0);
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
        i.focus(false);
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
        let vfs = Vfs::open_stock(Path::new(&root), 0).unwrap();
        let mut i = Input::bare();
        i.vfs = Some(vfs);
        i.load_defaults();
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
    fn a_fresh_profile_keeps_the_stock_melee_and_voice_keys() {
        let Some(mut i) = stock_input() else { return };
        i.use_profile(None, None);
        assert_eq!(i.bound("v"), Some("+melee"));
        assert_eq!(i.bound("z"), Some("+talk"));
        i.key("v", true);
        assert_eq!(frame(&mut i).buttons, buttons::MELEE);
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
    fn the_sensitivity_scale_touches_the_mouse_share_only() {
        let run = |scale: f32| {
            let mut i = Input::detached();
            i.exec_line("set sensitivity 2; set m_yaw 0.5; set cl_yawspeed 100");
            i.set_fov_sensitivity_scale(scale);
            i.clock = Some(1.0);
            i.key("leftarrow", true);
            i.clock = Some(1.1);
            i.mouse.winit_motion((10.0, 0.0));
            let f = i.frame(0.1);
            (f.look_delta_yaw - f.mouse_delta_yaw, f.mouse_delta_yaw)
        };
        let (key_full, mouse_full) = run(1.0);
        let (key_slow, mouse_slow) = run(0.5);
        assert!(key_full > 0.0 && mouse_full < 0.0);
        assert!((key_full - key_slow).abs() < 1e-4, "key turning ignores it");
        assert!((mouse_full * 0.5 - mouse_slow).abs() < 1e-4);
    }

    #[test]
    fn fov_sensitivity_scale_slows_mouse_look() {
        let mut i = Input::detached();
        i.exec_line("set sensitivity 2; set m_yaw 0.5; set m_pitch 0.25");
        i.set_fov_sensitivity_scale(0.5);
        i.mouse.winit_motion((10.0, 4.0));
        let f = frame(&mut i);
        assert_eq!((f.look_delta_yaw, f.look_delta_pitch), (-5.0, 1.0));
        // A cursor ignores the zoom.
        i.mouse.winit_motion((10.0, 4.0));
        let f = frame(&mut i);
        assert_eq!((f.cursor_yaw, f.cursor_pitch), (-10.0, 2.0));
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
        i.focus(false);
        assert_eq!(i.look_gate(), Some("unfocused"));
        i.focus(true);
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

    #[test]
    fn vstr_and_toggle_run_on_the_client() {
        let mut i = Input::detached();
        i.exec_line("set next \"set y 5\"; vstr next");
        assert_eq!(i.cvar("y"), Some("5"));
        i.exec_line("set t 0; toggle t");
        assert_eq!(i.cvar("t"), Some("1"));
        i.exec_line("toggle t");
        assert_eq!(i.cvar("t"), Some("0"));
        i.exec_line("set m a; toggle m a b c");
        assert_eq!(i.cvar("m"), Some("b"));
        i.exec_line("toggle m a b c");
        assert_eq!(i.cvar("m"), Some("c"));
        i.exec_line("toggle m a b c");
        assert_eq!(i.cvar("m"), Some("a"), "the last value wraps to the first");
        // Neither goes to the server.
        assert!(frame(&mut i).pending_commands.is_empty());
    }

    #[test]
    fn a_bound_vstr_chain_waits_between_commands() {
        let mut i = Input::detached();
        i.exec_line(
            "set first \"set z 1; wait; vstr second\"; set second \"set z 2; wait 2; set z 3\"",
        );
        i.exec_line("bind x \"vstr first; set after 1\"");
        i.key("x", true);
        // The rest of the buffer, `set after 1` included, waits behind the first `wait`.
        assert_eq!((i.cvar("z"), i.cvar("after")), (Some("1"), None));
        i.run_due();
        assert_eq!((i.cvar("z"), i.cvar("after")), (Some("2"), None));
        i.run_due();
        assert_eq!(i.cvar("z"), Some("2"), "wait 2 holds for two frames");
        i.run_due();
        assert_eq!((i.cvar("z"), i.cvar("after")), (Some("3"), Some("1")));
    }

    /// One buffer, as `Cbuf` has: a `wait` in an exec'd file holds what follows the `exec` too, and the message
    /// about the disk is for disk files only.
    #[test]
    fn a_wait_in_an_exec_d_file_holds_the_outer_buffer() {
        let dir = std::env::temp_dir().join(format!("cod4e-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("w.cfg");
        std::fs::write(&file, "set w_in 1\nwait\nset w_in2 1\n").unwrap();
        let mut i = Input::detached();
        i.take_messages();
        i.exec_line(&format!(
            "set w_before 1; exec \"{}\"; set w_after 1",
            file.display()
        ));
        assert_eq!(i.cvar("w_in"), Some("1"));
        assert_eq!((i.cvar("w_in2"), i.cvar("w_after")), (None, None));
        assert!(i.take_messages().iter().any(|m| m.contains("from disk")));
        i.run_due();
        assert_eq!((i.cvar("w_in2"), i.cvar("w_after")), (Some("1"), Some("1")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A path longer than the original's 63-character name buffer still reads from the disk; a self-running cvar
    /// stops instead of hanging.
    #[test]
    fn long_disk_paths_and_self_running_cvars() {
        let dir = std::env::temp_dir()
            .join(format!("cod4e-long-{}", std::process::id()))
            .join("a_directory_name_long_enough_to_pass_the_name_buffer_of_the_original");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("far.cfg"), "set far_away 1\n").unwrap();
        let mut i = Input::detached();
        i.exec_line(&format!("exec \"{}\"", dir.join("far").display()));
        assert_eq!(i.cvar("far_away"), Some("1"));
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
        i.take_messages();
        i.exec_line("set loop \"vstr loop\"; vstr loop; set done 1");
        assert_eq!(i.cvar("done"), None, "the runaway buffer is dropped");
        assert!(!i.take_messages().is_empty());
    }

    /// `-attack` after a `wait` lets go of a button a bound key started (`IN_KeyUp` with no key).
    #[test]
    fn a_deferred_minus_command_releases_the_bound_button() {
        let mut i = Input::detached();
        i.exec_line("bind x \"+attack; wait 2; -attack\"");
        i.key("x", true);
        assert_ne!(frame(&mut i).buttons & buttons::ATTACK, 0);
        i.run_due();
        i.run_due();
        assert_eq!(
            frame(&mut i).buttons & buttons::ATTACK,
            0,
            "released while x is held"
        );
        i.key("x", false);
        assert_eq!(frame(&mut i).buttons & buttons::ATTACK, 0);
    }

    /// One buffer: commands that arrive while another waits queue behind it.
    #[test]
    fn new_commands_queue_behind_a_waiting_buffer() {
        let mut i = Input::detached();
        i.exec_line("bind a \"set v 1; wait 3; set v 2\"; bind b \"set v 3\"");
        i.key("a", true);
        i.key("b", true);
        assert_eq!(i.cvar("v"), Some("1"));
        for _ in 0..3 {
            i.run_due();
        }
        assert_eq!(i.cvar("v"), Some("3"));
    }

    /// A resumed tail keeps the folder of the file it came from.
    #[test]
    fn a_resumed_exec_keeps_its_folder() {
        let dir = std::env::temp_dir().join(format!("cod4e-nest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.cfg"), "wait\nexec b\n").unwrap();
        std::fs::write(dir.join("sub/b.cfg"), "set nested_b 1\n").unwrap();
        let mut i = Input::detached();
        i.exec_line(&format!("exec \"{}\"", dir.join("sub/a").display()));
        assert_eq!(i.cvar("nested_b"), None);
        i.run_due();
        assert_eq!(i.cvar("nested_b"), Some("1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A key pressed while a `wait` is pending: its release waits in line behind its press.
    #[test]
    fn a_bound_button_pressed_during_a_wait_is_released() {
        let mut i = Input::detached();
        i.exec_line("bind a \"wait 3\"; bind w \"+forward\"");
        i.key("a", true);
        i.key("w", true);
        i.key("w", false);
        for _ in 0..4 {
            i.run_due();
        }
        frame(&mut i); // the tap shows for one frame
        assert_eq!(frame(&mut i).move_forward, 0.0, "not stuck");
    }

    #[test]
    fn a_typed_line_waits_for_the_rest() {
        let mut i = Input::detached();
        i.exec_line("set t_a 1; wait 3; set t_b 1");
        assert_eq!((i.cvar("t_a"), i.cvar("t_b")), (Some("1"), None));
        i.run_due();
        i.run_due();
        assert_eq!(i.cvar("t_b"), None);
        i.run_due();
        assert_eq!(i.cvar("t_b"), Some("1"));
    }

    #[test]
    fn toggle_says_when_the_cvar_is_not_there_and_compares_numbers() {
        let mut i = Input::detached();
        i.exec_line("toggle no_such_cvar_at_all");
        assert!(i.take_messages().iter().any(|m| m.contains("not found")));
        assert_eq!(i.cvar("no_such_cvar_at_all"), None);
        i.exec_line("set n 1.0; toggle n 1 0");
        assert_eq!(i.cvar("n"), Some("0"));
    }

    #[test]
    fn exec_adds_the_cfg_extension_and_says_when_it_cannot() {
        let dir = std::env::temp_dir().join(format!("cod4e-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.cfg"), "set from_file 7\n").unwrap();
        let mut i = Input::detached();
        i.exec_line(&format!("exec \"{}\"", dir.join("mine").display()));
        assert_eq!(i.cvar("from_file"), Some("7"));
        std::fs::remove_dir_all(&dir).unwrap();
        i.take_messages();
        i.exec_line("exec no_such_config_anywhere");
        assert!(!i.take_messages().is_empty(), "a failed exec is not silent");
    }

    #[test]
    fn failed_exec_and_bad_arguments_warn_without_the_arguments() {
        let mut i = Input::detached();
        let (_, events) = logging::capture(|| {
            i.exec_line("exec no_such_config_anywhere");
            i.exec_line("set onlyname");
        });
        assert!(events.iter().any(|e| e.level == tracing::Level::WARN
            && e.field("name") == Some("no_such_config_anywhere")));
        assert!(events.iter().any(|e| e.field("command") == Some("set")));
    }

    /// A word the input layer does not run pends for the app, which forwards it or says it is unknown; a cvar's
    /// name alone shows it and with a value sets it (`Dvar_Command`), without pending.
    #[test]
    fn an_unknown_bound_or_menu_command_pends_and_a_cvar_name_is_a_dvar_command() {
        let mut i = Input::detached();
        i.exec_line("no_such_bound_cmd_x with args");
        i.exec_line("set zz_name 4; zz_name; zz_name 7 8");
        assert_eq!(
            frame(&mut i).pending_commands,
            ["no_such_bound_cmd_x with args"]
        );
        assert_eq!(i.take_messages(), ["\"zz_name\" is \"4\""]);
        assert_eq!(i.cvar("zz_name"), Some("7 8"));
    }

    #[test]
    fn dvarlist_lists_matching_cvars() {
        let mut i = Input::detached();
        i.exec_line("set zz_a 1; set zz_b 2; set other 3; dvarlist zz_*");
        let said = i.take_messages().join("\n");
        assert!(said.contains("zz_a") && said.contains("zz_b") && !said.contains("other"));
    }

    /// The install's own configs: `exec` finds them in the IWDs, `default_mp.cfg` pulls in the controls, and the
    /// binds of a stock profile's `config_mp.cfg` run locally.
    #[test]
    fn stock_configs_exec_through_the_install() {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        let Some(vfs) = Vfs::open_stock(&root, 0)
            .ok()
            .filter(|v| v.locate("default_mp.cfg").is_some())
        else {
            eprintln!("no stock install with default_mp.cfg; skipping");
            return;
        };
        let mut i = Input::detached();
        i.vfs = Some(vfs);
        i.take_messages();
        i.exec_line("unbindall; exec default_mp; exec language");
        assert!(!i.binds.is_empty(), "default_mp.cfg binds the controls");
        assert!(
            i.take_messages().iter().all(|m| !m.contains("couldn't")),
            "every stock file was found"
        );
        let profile = std::fs::read_dir(root.join("players/profiles"))
            .into_iter()
            .flatten()
            .flatten()
            .find_map(|e| std::fs::read_to_string(e.path().join("config_mp.cfg")).ok());
        let Some(profile) = profile else {
            eprintln!("no stock profile config; skipping the binds");
            return;
        };
        i.exec_text(&profile, None, false, 0);
        // The stock profile binds Pause to `toggle cl_paused` and the vote keys to the server's commands. Multiplayer
        // registers `cl_paused` read only (`CG_RegisterDvars`): the key says so and the value stays.
        if i.bound("pause") == Some("toggle cl_paused") {
            frame(&mut i);
            i.take_messages();
            i.key("pause", true);
            assert_eq!(i.cvar("cl_paused"), Some("0"));
            assert_eq!(i.take_messages(), ["cl_paused is read only."]);
            assert!(frame(&mut i).pending_commands.is_empty());
        }
        if i.bound("f1") == Some("vote yes") {
            frame(&mut i); // what the typed `exec` handed on (`con_showchannel`, ...) is not under test
            i.key("f1", true);
            assert_eq!(frame(&mut i).pending_commands, ["vote yes"]);
        }
    }
}
