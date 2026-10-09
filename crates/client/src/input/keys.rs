// SPDX-License-Identifier: GPL-3.0-only
//! Key names as used in `bind` lines. Physical keys, so binds follow position, not layout.
//!
//! Either physical Shift, Ctrl or Alt is the one key `shift` / `ctrl` / `alt`, as the original's window procedure maps them.
//! `;` is `semicolon` since it separates commands.

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

pub fn key_name(code: KeyCode) -> Option<&'static str> {
    use KeyCode::*;
    Some(match code {
        KeyA => "a",
        KeyB => "b",
        KeyC => "c",
        KeyD => "d",
        KeyE => "e",
        KeyF => "f",
        KeyG => "g",
        KeyH => "h",
        KeyI => "i",
        KeyJ => "j",
        KeyK => "k",
        KeyL => "l",
        KeyM => "m",
        KeyN => "n",
        KeyO => "o",
        KeyP => "p",
        KeyQ => "q",
        KeyR => "r",
        KeyS => "s",
        KeyT => "t",
        KeyU => "u",
        KeyV => "v",
        KeyW => "w",
        KeyX => "x",
        KeyY => "y",
        KeyZ => "z",
        Digit0 => "0",
        Digit1 => "1",
        Digit2 => "2",
        Digit3 => "3",
        Digit4 => "4",
        Digit5 => "5",
        Digit6 => "6",
        Digit7 => "7",
        Digit8 => "8",
        Digit9 => "9",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        Space => "space",
        Enter => "enter",
        Escape => "escape",
        Tab => "tab",
        Backspace => "backspace",
        ShiftLeft | ShiftRight => "shift",
        ControlLeft | ControlRight => "ctrl",
        AltLeft | AltRight => "alt",
        SuperLeft => "super",
        SuperRight => "rsuper",
        ArrowUp => "uparrow",
        ArrowDown => "downarrow",
        ArrowLeft => "leftarrow",
        ArrowRight => "rightarrow",
        Insert => "ins",
        Delete => "del",
        Home => "home",
        End => "end",
        PageUp => "pgup",
        PageDown => "pgdn",
        CapsLock => "capslock",
        Pause => "pause",
        ScrollLock => "scrolllock",
        NumLock => "kp_numlock",
        Minus => "-",
        Equal => "=",
        BracketLeft => "[",
        BracketRight => "]",
        Semicolon => "semicolon",
        Quote => "'",
        Comma => ",",
        Period => ".",
        Slash => "/",
        Backslash => "\\",
        Backquote => "~",
        Numpad0 => "kp_0",
        Numpad1 => "kp_1",
        Numpad2 => "kp_2",
        Numpad3 => "kp_3",
        Numpad4 => "kp_4",
        Numpad5 => "kp_5",
        Numpad6 => "kp_6",
        Numpad7 => "kp_7",
        Numpad8 => "kp_8",
        Numpad9 => "kp_9",
        NumpadEnter => "kp_enter",
        NumpadAdd => "kp_plus",
        NumpadSubtract => "kp_minus",
        NumpadMultiply => "kp_multiply",
        NumpadDivide => "kp_slash",
        NumpadDecimal => "kp_del",
        NumpadEqual => "kp_equals",
        F13 => "f13",
        F14 => "f14",
        F15 => "f15",
        F16 => "f16",
        F17 => "f17",
        F18 => "f18",
        F19 => "f19",
        F20 => "f20",
        F21 => "f21",
        F22 => "f22",
        F23 => "f23",
        F24 => "f24",
        _ => return None,
    })
}

/// The keypad key with NumLock off: the window system reports the physical key as `Numpad7` but the logical key as
/// `Home`; the original names it `kp_home`. `None` for anything else.
pub fn keypad_nav_name(code: KeyCode, logical: &winit::keyboard::Key) -> Option<&'static str> {
    use winit::keyboard::{Key, NamedKey};
    let Key::Named(n) = logical else { return None };
    Some(match (code, n) {
        (KeyCode::Numpad7, NamedKey::Home) => "kp_home",
        (KeyCode::Numpad8, NamedKey::ArrowUp) => "kp_uparrow",
        (KeyCode::Numpad9, NamedKey::PageUp) => "kp_pgup",
        (KeyCode::Numpad4, NamedKey::ArrowLeft) => "kp_leftarrow",
        (KeyCode::Numpad5, NamedKey::Clear) => "kp_5",
        (KeyCode::Numpad6, NamedKey::ArrowRight) => "kp_rightarrow",
        (KeyCode::Numpad1, NamedKey::End) => "kp_end",
        (KeyCode::Numpad2, NamedKey::ArrowDown) => "kp_downarrow",
        (KeyCode::Numpad3, NamedKey::PageDown) => "kp_pgdn",
        (KeyCode::Numpad0, NamedKey::Insert) => "kp_ins",
        (KeyCode::NumpadDecimal, NamedKey::Delete) => "kp_del",
        _ => return None,
    })
}

/// The id the original's UI localizes a key by: `Q` for a character key, `KEY_MOUSE1`, `KEY_CTRL`, ... otherwise.
/// Gamepad buttons have no stock string and keep their upper-case name.
pub fn key_id(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    if name.chars().count() == 1 || name.starts_with("pad_") {
        upper
    } else {
        format!("KEY_{upper}")
    }
}

pub fn mouse_name(b: MouseButton) -> String {
    match b {
        MouseButton::Left => "mouse1".into(),
        MouseButton::Right => "mouse2".into(),
        MouseButton::Middle => "mouse3".into(),
        MouseButton::Back => "mouse4".into(),
        MouseButton::Forward => "mouse5".into(),
        MouseButton::Other(n) => format!("mouse{}", u32::from(n) + 6),
    }
}

/// Gamepad button names, shared with `bind`. Positional (south/east/west/north), Xbox lettering.
pub fn pad_name(b: gilrs::Button) -> Option<&'static str> {
    use gilrs::Button::*;
    Some(match b {
        South => "pad_a",
        East => "pad_b",
        West => "pad_x",
        North => "pad_y",
        LeftTrigger => "pad_lb",
        RightTrigger => "pad_rb",
        LeftTrigger2 => "pad_lt",
        RightTrigger2 => "pad_rt",
        Select => "pad_back",
        Start => "pad_start",
        LeftThumb => "pad_ls",
        RightThumb => "pad_rs",
        DPadUp => "pad_dpad_up",
        DPadDown => "pad_dpad_down",
        DPadLeft => "pad_dpad_left",
        DPadRight => "pad_dpad_right",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ids_follow_the_original_names() {
        assert_eq!(key_id("q"), "Q");
        assert_eq!(key_id("7"), "7");
        assert_eq!(key_id("mouse1"), "KEY_MOUSE1");
        assert_eq!(key_id("ctrl"), "KEY_CTRL");
        assert_eq!(key_id("semicolon"), "KEY_SEMICOLON");
        assert_eq!(key_id("pad_a"), "PAD_A");
    }

    /// Either physical modifier is the one named key, so the stock sprint and stance binds answer to both.
    #[test]
    fn both_physical_modifiers_are_one_key() {
        use KeyCode::*;
        for (left, right, name) in [
            (ShiftLeft, ShiftRight, "shift"),
            (ControlLeft, ControlRight, "ctrl"),
            (AltLeft, AltRight, "alt"),
        ] {
            assert_eq!(key_name(left), Some(name));
            assert_eq!(key_name(right), Some(name));
        }
        assert_eq!(key_name(Pause), Some("pause"));
        assert_eq!(key_name(F13), Some("f13"));
    }

    #[test]
    fn keypad_with_numlock_off_has_the_navigation_names() {
        use winit::keyboard::{Key, NamedKey};
        let home = Key::Named(NamedKey::Home);
        assert_eq!(keypad_nav_name(KeyCode::Numpad7, &home), Some("kp_home"));
        assert_eq!(keypad_nav_name(KeyCode::Home, &home), None);
        assert_eq!(
            keypad_nav_name(KeyCode::Numpad7, &Key::Character("7".into())),
            None
        );
    }
}
