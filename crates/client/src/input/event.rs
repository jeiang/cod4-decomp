// SPDX-License-Identifier: GPL-3.0-only
//! One input event as the engine sees it, wherever it came from.
//!
//! The window system's events are translated into these at the point winit delivers them ([`crate::app`]); the control
//! channel's injected ones are built directly. Both go into the same queue and reach the menus, the console and the
//! game through the same code, so a synthetic key press is the player's key press and nothing else.
//!
//! winit's own `KeyEvent` cannot be built outside winit (its platform half is private), which is why the queue holds
//! this smaller type: the fields the engine reads, and no more.

use winit::event::{ElementState, MouseButton};
use winit::keyboard::{Key, KeyCode};

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// A key went down or up. `logical` and `text` are what it types, for the menus and the console.
    Key {
        code: KeyCode,
        logical: Option<Key>,
        text: Option<String>,
        state: ElementState,
        repeat: bool,
    },
    /// Text typed into the console, chat or an edit field, one character at a time as a keyboard would.
    Text(String),
    /// The pointer moved to this position in window pixels (the menus hit-test it).
    Cursor {
        x: f32,
        y: f32,
    },
    /// A mouse button changed, or the wheel turned (`Wheel` carries `y` lines, positive up).
    Button {
        button: MouseButton,
        state: ElementState,
    },
    Wheel {
        y: f32,
    },
    /// Raw motion: what a captured pointer's look reads (`DeviceEvent::MouseMotion`).
    Motion {
        dx: f64,
        dy: f64,
    },
    /// A gamepad button: `pad_a`, `pad_start`, ... as the bind table names them.
    Pad {
        name: String,
        down: bool,
    },
    /// A gamepad stick: `left` or `right`, each axis -1 to 1 (`dx` right, `dy` down).
    PadAxis {
        stick: String,
        dx: f32,
        dy: f32,
    },
    /// The window took or lost the keyboard's focus.
    Focus(bool),
}

impl InputEvent {
    /// A key press or release by physical key, with the text it types when `text` is given.
    pub fn key(code: KeyCode, state: ElementState) -> Self {
        Self::Key {
            code,
            logical: None,
            text: None,
            state,
            repeat: false,
        }
    }
}
