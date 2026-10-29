// SPDX-License-Identifier: GPL-3.0-only
//! What the control channel's requests ask of the running client, and how each is answered.
//!
//! The requests name the player's own paths: `key` presses a bind, `text` types into the focused field, `exec` runs a
//! console line as Enter does, `viewpos` places the camera the way `setviewpos` does, and a capture saves the frame
//! the window is showing. The state document is the game's real state, so an assertion says what should be true
//! rather than what was drawn.

use crate::input::InputEvent;
use devctrl::{Outcome, Request, lookup, reply};
use serde_json::{Map, Value, json};
use winit::event::{ElementState, MouseButton};
use winit::keyboard::{Key, KeyCode, NamedKey};

/// The doors the requests open into the game. The app implements it (`crates/client/src/app.rs`); a test can drive
/// [`handle`] with a fake.
pub trait Control {
    /// Frames drawn so far.
    fn frames(&self) -> u64;

    /// Seconds since request `seq` was first offered; starts its clock.
    fn waited(&mut self, seq: u64) -> f64;

    /// Frames left of a `step` request, after counting this one.
    fn step_left(&mut self, seq: u64, frames: u64) -> u64;

    /// The state document, narrowed to `what` when it names a part.
    fn state(&mut self, what: Option<&str>) -> Value;

    /// The keys bound to `command` (`+forward`, `togglemenu`).
    fn binds_of(&self, command: &str) -> Vec<String>;

    fn cvar(&self, name: &str) -> Option<String>;

    /// Sets a cvar the way the console's `set` does.
    fn set_cvar(&mut self, name: &str, value: &str) -> Result<(), String>;

    /// Queues one input event into the same queue the window's events enter.
    fn inject(&mut self, ev: InputEvent);

    /// Runs one console line as Enter does: echoed, then executed.
    fn exec(&mut self, line: &str);

    /// Saves the frame on screen. `dir` overrides the run's output directory. `None` while not between frames: a
    /// capture is taken after the next present, so the request waits.
    fn capture(&mut self, dir: Option<&str>, base64: bool) -> Option<Result<Value, String>>;

    /// Places the camera (`setviewpos`): the player's own origin, and angles in degrees.
    fn view(&mut self, origin: Option<[f32; 3]>, angles: Option<[f32; 3]>)
    -> Result<Value, String>;

    /// Asks the listen server this client runs for its state, or runs a line on it. `None` until the server answers:
    /// it answers between its own frames.
    fn server(&mut self, seq: u64, req: &Map<String, Value>) -> Option<Result<Value, String>>;

    /// Ends the run as `quit` does.
    fn quit(&mut self);
}

/// Answers one request. `Wait` means the request stays at the front of the channel's queue and is offered again next
/// frame: a wait, a capture between frames, or a step with frames left.
pub fn handle<H: Control + ?Sized>(h: &mut H, req: &Request) -> Outcome {
    match req.cmd.as_str() {
        "capture" => {
            return match h.capture(req.str("dir"), req.bool("base64").unwrap_or(false)) {
                None => Outcome::Wait,
                Some(r) => Outcome::Done(reply(&req.id, r)),
            };
        }
        "wait" => return wait(h, req),
        "assert" => {
            let Some(spec) = expectation(req) else {
                return Outcome::Done(reply(
                    &req.id,
                    Err("assert needs the expected state".to_owned()),
                ));
            };
            return Outcome::Done(check(h, &req.id, spec));
        }
        "step" => {
            let frames = req.u64("frames").unwrap_or(1).max(1);
            return match h.step_left(req.seq, frames) {
                0 => Outcome::Done(reply(&req.id, Ok(json!({"steps": frames})))),
                _ => Outcome::Wait,
            };
        }
        // A listen server answers between its own frames: the request waits for its reply.
        "server" => {
            let id = req.id.clone();
            return match h.server(req.seq, &req.args) {
                None => Outcome::Wait,
                Some(r) => Outcome::Done(reply(&id, r)),
            };
        }
        _ => {}
    }
    Outcome::Done(reply(&req.id, answer(h, req)))
}

/// The requests answered in one look at the game.
fn answer<H: Control + ?Sized>(h: &mut H, req: &Request) -> Result<Value, String> {
    match req.cmd.as_str() {
        "ping" => Ok(json!({"client": "cod4e", "frames": h.frames()})),
        "state" => {
            let what = req
                .str("what")
                .or_else(|| req.str("part"))
                .or_else(|| req.str("of"));
            Ok(h.state(what))
        }
        "cvar" => {
            let name = req.str("name").ok_or("cvar needs a name")?;
            match req.str("value") {
                None => Ok(json!({"name": name, "value": h.cvar(name)})),
                Some(value) => {
                    h.set_cvar(name, value)?;
                    Ok(json!({"name": name, "value": h.cvar(name)}))
                }
            }
        }
        "cvars" => {
            // The section is the variables themselves, not the document that names it.
            let all = match h.state(Some("cvars")) {
                Value::Object(mut m) => m.remove("cvars").unwrap_or(Value::Null),
                v => v,
            };
            match req.str("filter") {
                None => Ok(all),
                Some(f) => {
                    let f = f.to_ascii_lowercase();
                    let mut out = Map::new();
                    if let Value::Object(m) = all {
                        for (k, v) in m {
                            if k.to_ascii_lowercase().contains(&f) {
                                out.insert(k, v);
                            }
                        }
                    }
                    Ok(Value::Object(out))
                }
            }
        }
        "binds" => Ok(h.state(Some("binds"))),
        "key" => inject_key(h, req),
        "text" => {
            let text = req.str("text").ok_or("text needs the text")?;
            h.inject(InputEvent::Text(text.to_owned()));
            Ok(json!({"typed": text.chars().count()}))
        }
        "mouse" => {
            if let (Some(x), Some(y)) = (req.f32("x"), req.f32("y")) {
                h.inject(InputEvent::Cursor { x, y });
                Ok(json!({"cursor": [x, y]}))
            } else if let (Some(dx), Some(dy)) = (req.number("dx"), req.number("dy")) {
                h.inject(InputEvent::Motion { dx, dy });
                Ok(json!({"motion": [dx, dy]}))
            } else {
                Err("mouse needs x and y (a position) or dx and dy (a turn)".to_owned())
            }
        }
        "button" => {
            let name = req.str("name").ok_or("button needs a name")?;
            let button =
                mouse_button(name).ok_or_else(|| format!("no mouse button is named {name}"))?;
            let (down, up) = edges(req.str("action"))?;
            for (state, on) in [(ElementState::Pressed, down), (ElementState::Released, up)] {
                if on {
                    h.inject(InputEvent::Button { button, state });
                }
            }
            Ok(json!({"name": name}))
        }
        "wheel" => {
            let y = req.f32("y").ok_or("wheel needs y")?;
            h.inject(InputEvent::Wheel { y });
            Ok(json!({"wheel": y}))
        }
        "pad" => {
            if let (Some(stick), Some(dx), Some(dy)) =
                (req.str("stick"), req.f32("x"), req.f32("y"))
            {
                h.inject(InputEvent::PadAxis {
                    stick: stick.to_owned(),
                    dx,
                    dy,
                });
                return Ok(json!({"stick": stick}));
            }
            let name = req
                .str("name")
                .or_else(|| req.str("button"))
                .ok_or("pad needs a button's name or a stick with x and y")?;
            let (down, up) = edges(req.str("action"))?;
            for (pressed, on) in [(true, down), (false, up)] {
                if on {
                    h.inject(InputEvent::Pad {
                        name: name.to_owned(),
                        down: pressed,
                    });
                }
            }
            Ok(json!({"pad": name}))
        }
        "exec" => {
            let line = req.str("line").ok_or("exec needs the line")?;
            h.exec(line);
            Ok(json!({"line": line}))
        }
        "viewpos" => {
            let origin = vec3(req.value("origin"));
            let angles = vec3(req.value("angles"));
            if origin.is_none() && angles.is_none() {
                return Ok(
                    json!({"set": false, "origin": lookup(&h.state(Some("player")), "player.origin").cloned().unwrap_or(Value::Null)}),
                );
            }
            h.view(origin, angles)
        }

        "quit" => {
            h.quit();
            Ok(json!({"quitting": true}))
        }
        other => Err(format!("no such command: {other}")),
    }
}

/// The state a `wait` or an `assert` expects.
fn expectation<'a>(req: &'a Request) -> Option<&'a Value> {
    req.value("state")
        .or_else(|| req.value("until"))
        .or_else(|| req.value("expect"))
}

/// The values of the expected paths, for a reply that says what was seen as well as what was wanted.
fn observed(doc: &Value, spec: &Value) -> Value {
    let Value::Object(want) = spec else {
        return json!({});
    };
    let mut out = Map::new();
    for path in want.keys() {
        out.insert(
            path.clone(),
            lookup(&doc, path).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}

/// `wait`: the request holds until the state document answers `state`, or the timeout runs out.
fn wait<H: Control + ?Sized>(h: &mut H, req: &Request) -> Outcome {
    let Some(spec) = expectation(req) else {
        return Outcome::Done(reply(
            &req.id,
            Err("wait needs the state to wait for".to_owned()),
        ));
    };
    let timeout_ms = req.number("timeout_ms").unwrap_or(5000.0);
    let verdict = devctrl::matches(&h.state(None), spec);
    match verdict {
        Ok(()) => {
            let waited_ms = (h.waited(req.seq) * 1000.0).round() as u64;
            let seen = observed(&h.state(None), spec);
            Outcome::Done(reply(
                &req.id,
                Ok(json!({"waited_ms": waited_ms, "observed": seen})),
            ))
        }
        Err(mismatches) => {
            if h.waited(req.seq) * 1000.0 < timeout_ms {
                return Outcome::Wait;
            }
            let seen = observed(&h.state(None), spec);
            Outcome::Done(
                devctrl::Reply::failed(
                    req.id.clone(),
                    format!("timed out after {timeout_ms} ms: {}", mismatches.join("; ")),
                )
                .set("timeout", true)
                .set("observed", seen),
            )
        }
    }
}

/// `assert`: the state document answers the expectation now.
fn check<H: Control + ?Sized>(h: &mut H, id: &Value, spec: &Value) -> devctrl::Reply {
    match devctrl::matches(&h.state(None), spec) {
        Ok(()) => devctrl::Reply::ok(id.clone()).set("observed", observed(&h.state(None), spec)),
        Err(mismatches) => devctrl::Reply::failed(id.clone(), mismatches.join("; "))
            .set("observed", observed(&h.state(None), spec))
            .set("mismatches", mismatches),
    }
}

/// `key`: presses a key by name, or the keys bound to a command (`+forward`), down, up or both.
fn inject_key<H: Control + ?Sized>(h: &mut H, req: &Request) -> Result<Value, String> {
    let action = req.str("action").unwrap_or("tap");
    edges(Some(action))?;
    if let Some(command) = req.str("bind") {
        let keys = h.binds_of(command);
        if keys.is_empty() {
            return Err(format!("nothing is bound to {command}"));
        }
        for key in &keys {
            press(h, key, action)?;
        }
        return Ok(json!({"bind": command, "keys": keys}));
    }
    let name = req
        .str("name")
        .ok_or("key needs a name or a bind")?
        .to_owned();
    press(h, &name, action)?;
    Ok(json!({"name": name, "action": action}))
}

/// The states an action presses: down, up, or both in order.
fn edges(action: Option<&str>) -> Result<(bool, bool), String> {
    match action.unwrap_or("tap") {
        "down" | "press" => Ok((true, false)),
        "up" | "release" => Ok((false, true)),
        "tap" | "click" => Ok((true, true)),
        other => Err(format!("no such action: {other} (down, up or tap)")),
    }
}

/// Queues one key, button or pad press by name: `escape`, `w`, `mouse1`, `pad_a`.
fn press<H: Control + ?Sized>(h: &mut H, name: &str, action: &str) -> Result<(), String> {
    let code = key_code(name);
    let button = mouse_button(name);
    let pad = name.starts_with("pad_");
    if code.is_none() && button.is_none() && !pad {
        return Err(format!(
            "no key, mouse button or pad is named {name} (see the `keyname` request)"
        ));
    }
    let (down, up) = edges(Some(action))?;
    for (state, on) in [(ElementState::Pressed, down), (ElementState::Released, up)] {
        if !on {
            continue;
        }
        match (code, button) {
            (Some(code), _) => h.inject(InputEvent::Key {
                code,
                logical: logical_key(name),
                text: text_of(name).map(str::to_owned),
                state,
                repeat: false,
            }),
            (None, Some(button)) => h.inject(InputEvent::Button { button, state }),
            (None, None) => h.inject(InputEvent::Pad {
                name: name.to_owned(),
                down: state == ElementState::Pressed,
            }),
        }
    }
    Ok(())
}

/// The physical key a bind name is, for pressing it by name (`w`, `escape`, `~`).
fn key_code(name: &str) -> Option<KeyCode> {
    crate::input::key_code(name)
}

/// The mouse button a request's name is: the engine's names, and the short ones people type.
fn mouse_button(name: &str) -> Option<MouseButton> {
    Some(match name.to_ascii_lowercase().as_str() {
        "mouse1" | "left" | "lmb" => MouseButton::Left,
        "mouse2" | "right" | "rmb" => MouseButton::Right,
        "mouse3" | "middle" | "mmb" => MouseButton::Middle,
        "mouse4" => MouseButton::Back,
        "mouse5" => MouseButton::Forward,
        _ => return None,
    })
}

/// The logical key a name is, for the keys a menu reads by name (arrows, enter, escape) and for characters.
fn logical_key(name: &str) -> Option<Key> {
    if let Some(named) = named_key(name) {
        return Some(Key::Named(named));
    }
    match text_of(name)? {
        " " => Some(Key::Named(NamedKey::Space)),
        text => Some(Key::Character(text.into())),
    }
}

/// The named key a bind name is (`up`, `enter`, `escape`, ...).
fn named_key(name: &str) -> Option<NamedKey> {
    Some(match name.to_ascii_lowercase().as_str() {
        "up" | "arrowup" => NamedKey::ArrowUp,
        "down" | "arrowdown" => NamedKey::ArrowDown,
        "left" | "arrowleft" => NamedKey::ArrowLeft,
        "right" | "arrowright" => NamedKey::ArrowRight,
        "enter" => NamedKey::Enter,
        "escape" | "esc" => NamedKey::Escape,
        "tab" => NamedKey::Tab,
        "space" => NamedKey::Space,
        "backspace" => NamedKey::Backspace,
        "delete" | "del" => NamedKey::Delete,
        "insert" | "ins" => NamedKey::Insert,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" | "pgup" => NamedKey::PageUp,
        "pagedown" | "pgdn" => NamedKey::PageDown,
        _ => return None,
    })
}

/// The character a key types, if it types one: `w` types `w`, `semicolon` types `;`.
fn text_of(name: &str) -> Option<&'static str> {
    const ONE: [&str; 36] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
    ];
    let lower = name.to_ascii_lowercase();
    if lower.len() == 1 {
        let c = lower.as_bytes()[0];
        if let Some(s) = ONE.iter().find(|s| s.as_bytes()[0] == c) {
            return Some(s);
        }
    }
    Some(match lower.as_str() {
        "space" => " ",
        "semicolon" => ";",
        "apostrophe" => "'",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "minus" => "-",
        "equal" | "equals" => "=",
        "leftbracket" => "[",
        "rightbracket" => "]",
        "grave" => "`",
        _ => return None,
    })
}

/// A request's `[x, y, z]`.
fn vec3(v: Option<&Value>) -> Option<[f32; 3]> {
    let a = v?.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some([
        a[0].as_f64()? as f32,
        a[1].as_f64()? as f32,
        a[2].as_f64()? as f32,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control that records what the requests did and answers a small state document.
    #[derive(Default)]
    struct Fake {
        events: Vec<InputEvent>,
        lines: Vec<String>,
        captured: usize,
        waited: u64,
        steps: u64,
        quit: bool,
    }

    impl Control for Fake {
        fn frames(&self) -> u64 {
            42
        }
        fn waited(&mut self, _: u64) -> f64 {
            self.waited as f64
        }
        fn step_left(&mut self, _: u64, frames: u64) -> u64 {
            self.steps += 1;
            frames.saturating_sub(self.steps)
        }
        fn state(&mut self, what: Option<&str>) -> Value {
            let doc = json!({
                "player": {"health": 100, "origin": [1.0, 2.0, 3.0]},
                "menu": {"open": ["main"]},
                "cvars": {"cg_fov": "80", "in_mouse": "1"},
            });
            match what {
                None => doc,
                Some(w) => json!({w: doc.get(w).cloned().unwrap_or(Value::Null)}),
            }
        }
        fn binds_of(&self, command: &str) -> Vec<String> {
            match command {
                "+forward" => vec!["w".to_owned()],
                _ => Vec::new(),
            }
        }
        fn cvar(&self, name: &str) -> Option<String> {
            (name == "cg_fov").then(|| "80".to_owned())
        }
        fn set_cvar(&mut self, name: &str, value: &str) -> Result<(), String> {
            self.lines.push(format!("set {name} {value}"));
            Ok(())
        }
        fn inject(&mut self, ev: InputEvent) {
            self.events.push(ev);
        }
        fn exec(&mut self, line: &str) {
            self.lines.push(line.to_owned());
        }
        fn capture(&mut self, _: Option<&str>, _: bool) -> Option<Result<Value, String>> {
            self.captured += 1;
            Some(Ok(json!({"path": "out/shot.png"})))
        }
        fn view(
            &mut self,
            origin: Option<[f32; 3]>,
            angles: Option<[f32; 3]>,
        ) -> Result<Value, String> {
            Ok(json!({"origin": origin, "angles": angles}))
        }
        fn server(&mut self, _: u64, _: &Map<String, Value>) -> Option<Result<Value, String>> {
            Some(Err("no listen server".to_owned()))
        }
        fn quit(&mut self) {
            self.quit = true;
        }
    }

    fn run(f: &mut Fake, line: &str) -> Value {
        let req = Request::parse(line, 1, 1).unwrap();
        match handle(f, &req) {
            Outcome::Done(r) => serde_json::from_str(&r.line()).unwrap(),
            Outcome::Wait => json!({"wait": true}),
        }
    }

    #[test]
    fn a_key_request_queues_the_events_the_window_would() {
        let mut f = Fake::default();
        let reply = run(
            &mut f,
            r#"{"id":1,"cmd":"key","name":"escape","action":"tap"}"#,
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(f.events.len(), 2);
        match &f.events[0] {
            InputEvent::Key { code, state, .. } => {
                assert_eq!(*code, KeyCode::Escape);
                assert_eq!(*state, ElementState::Pressed);
            }
            other => panic!("{other:?}"),
        }
        // A button name goes through the button path, a bind through the keys bound to it.
        run(&mut f, r#"{"id":2,"cmd":"key","name":"mouse1"}"#);
        assert!(matches!(f.events[2], InputEvent::Button { .. }));
        let reply = run(
            &mut f,
            r#"{"id":3,"cmd":"key","bind":"+forward","action":"down"}"#,
        );
        assert_eq!(reply["keys"], json!(["w"]));
        assert_eq!(reply["ok"], json!(true));
        let bad = run(&mut f, r#"{"id":4,"cmd":"key","name":"nope"}"#);
        assert_eq!(bad["ok"], json!(false));
        assert!(bad["error"].as_str().unwrap().contains("no key"));
    }

    #[test]
    fn text_exec_and_cvars_take_the_players_paths() {
        let mut f = Fake::default();
        run(&mut f, r#"{"id":1,"cmd":"text","text":"bots 3"}"#);
        assert!(matches!(&f.events[0], InputEvent::Text(t) if t == "bots 3"));
        run(&mut f, r#"{"id":2,"cmd":"exec","line":"bots 3"}"#);
        assert_eq!(f.lines, ["bots 3"]);
        let reply = run(&mut f, r#"{"id":3,"cmd":"cvar","name":"cg_fov"}"#);
        assert_eq!(reply["value"], json!("80"));
        run(
            &mut f,
            r#"{"id":4,"cmd":"cvar","name":"cg_fov","value":"90"}"#,
        );
        assert_eq!(f.lines[1], "set cg_fov 90");
        let filtered = run(&mut f, r#"{"id":5,"cmd":"cvars","filter":"fov"}"#);
        assert_eq!(filtered, json!({"cg_fov": "80"}));
    }

    #[test]
    fn waits_asserts_and_steps_hold_the_queue_until_true() {
        let mut f = Fake::default();
        let first = run(
            &mut f,
            r#"{"id":1,"cmd":"wait","state":{"player.health":100},"timeout_ms":1000}"#,
        );
        assert_eq!(first["ok"], json!(true));
        assert_eq!(first["waited_ms"].as_f64(), Some(0.0));
        // Not yet true and the clock has not run out: the request waits.
        let held = run(
            &mut f,
            r#"{"id":2,"cmd":"wait","state":{"player.health":101},"timeout_ms":1000}"#,
        );
        assert_eq!(held["wait"], json!(true));
        // The clock ran out: a failed reply that shows what was seen.
        f.waited = 2;
        let timed_out = run(
            &mut f,
            r#"{"id":3,"cmd":"wait","state":{"player.health":101},"timeout_ms":1000}"#,
        );
        assert_eq!(timed_out["ok"], json!(false));
        assert_eq!(timed_out["timeout"], json!(true));
        assert_eq!(timed_out["observed"]["player.health"].as_f64(), Some(100.0));
        let asserted = run(
            &mut f,
            r#"{"id":4,"cmd":"assert","state":{"player.health":{"gt":99}}}"#,
        );
        assert_eq!(asserted["ok"], json!(true));
        let failed = run(
            &mut f,
            r#"{"id":5,"cmd":"assert","state":{"menu.open.0":"class"}}"#,
        );
        assert_eq!(failed["ok"], json!(false));
        assert_eq!(failed["observed"]["menu.open.0"], json!("main"));
        assert!(failed["error"].as_str().unwrap().contains("menu.open.0"));
        // A step of two frames takes two offers plus the one that finds none left.
        assert_eq!(
            run(&mut f, r#"{"id":6,"cmd":"step","frames":2}"#)["wait"],
            json!(true)
        );
        assert_eq!(
            run(&mut f, r#"{"id":7,"cmd":"step","frames":2}"#)["ok"],
            json!(true)
        );
    }

    #[test]
    fn a_capture_answers_and_quit_ends_the_run() {
        let mut f = Fake::default();
        let shot = run(&mut f, r#"{"id":1,"cmd":"capture"}"#);
        assert_eq!(shot["path"], json!("out/shot.png"));
        assert_eq!(f.captured, 1);
        assert_eq!(
            run(&mut f, r#"{"id":2,"cmd":"quit"}"#)["quitting"],
            json!(true)
        );
        assert!(f.quit);
        assert_eq!(
            run(&mut f, r#"{"id":3,"cmd":"nonsense"}"#)["ok"],
            json!(false)
        );
        // The pointer and the wheel go through the same events as the window's.
        run(&mut f, r#"{"id":4,"cmd":"mouse","x":10,"y":20}"#);
        run(&mut f, r#"{"id":5,"cmd":"mouse","dx":5,"dy":-3}"#);
        run(&mut f, r#"{"id":6,"cmd":"wheel","y":1}"#);
        assert_eq!(f.events.len(), 3);
        assert!(matches!(
            f.events[0],
            InputEvent::Cursor { x: 10.0, y: 20.0 }
        ));
        assert!(matches!(
            f.events[1],
            InputEvent::Motion { dx: 5.0, dy: -3.0 }
        ));
        assert!(matches!(f.events[2], InputEvent::Wheel { y: 1.0 }));
    }

    #[test]
    fn every_key_name_a_request_may_use_resolves() {
        for name in [
            "escape",
            "enter",
            "tab",
            "up",
            "down",
            "left",
            "right",
            "space",
            "backspace",
            "a",
            "z",
            "0",
            "9",
            "semicolon",
            "slash",
            "grave",
        ] {
            assert!(key_code(name).is_some(), "{name}");
            assert!(logical_key(name).is_some(), "{name}");
        }
        assert!(matches!(
            logical_key("up"),
            Some(Key::Named(NamedKey::ArrowUp))
        ));
        assert!(matches!(logical_key("a"), Some(Key::Character(_))));
        assert_eq!(text_of("w"), Some("w"));
        assert_eq!(text_of("semicolon"), Some(";"));
        assert_eq!(text_of("escape"), None);
    }
}
