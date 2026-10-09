// SPDX-License-Identifier: GPL-3.0-only
//! The client cvar store: name -> string value, with a default and an archive flag.
//!
//! Names are case-insensitive (stored lowercase), as in the original. Only archived cvars are written back to the
//! config file.

use std::collections::HashMap;

/// `(name, default, archived)`. Every cvar the input layer reads has an entry so a read never misses.
const DEFAULTS: &[(&str, &str, bool)] = &[
    ("sensitivity", "5", true),
    ("m_pitch", "0.022", true),
    ("m_yaw", "0.022", true),
    ("input_invertpitch", "0", true),
    ("cl_pitchmin", "-85", true),
    ("cl_pitchmax", "85", true),
    ("cl_run", "1", true),
    ("cl_freelook", "1", true),
    ("m_filter", "0", true),
    ("cl_mouseaccel", "0", true),
    ("m_side", "0.25", true),
    ("m_forward", "0.25", true),
    ("cl_anglespeedkey", "1.5", true),
    ("cl_stanceholdtime", "300", false),
    ("cl_yawspeed", "140", true),
    ("cl_pitchspeed", "140", true),
    ("cg_fov", "80", true),
    ("cg_fovscale", "1", false),
    ("cg_fovmin", "10", false),
    ("in_mouse", "1", true),
    ("in_gamepad", "1", true),
    // Voice chat sends the microphone while `+talk` is held; a missing map is fetched from the server that plays it;
    // `cl_freezeDemo` holds a playing demo.
    ("cl_voice", "1", true),
    ("cl_allowdownload", "1", true),
    ("cl_freezedemo", "0", false),
    ("in_gamepad_deadzone", "0.2", true),
    ("in_gamepad_yawrate", "140", true),
    ("in_gamepad_pitchrate", "100", true),
];

struct Cvar {
    value: String,
    archive: bool,
}

pub struct Cvars {
    map: HashMap<String, Cvar>,
}

impl Default for Cvars {
    fn default() -> Self {
        let map = DEFAULTS
            .iter()
            .map(|&(n, v, archive)| {
                let value = v.to_owned();
                (n.to_owned(), Cvar { value, archive })
            })
            .collect();
        Self { map }
    }
}

impl Cvars {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.map
            .get(&name.to_ascii_lowercase())
            .map(|c| c.value.as_str())
    }

    /// The value as a float; 0 when unset or not a number.
    pub fn f32(&self, name: &str) -> f32 {
        self.get(name)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.0)
    }

    pub fn bool(&self, name: &str) -> bool {
        self.f32(name) != 0.0
    }

    /// Set a value, creating the cvar if needed. `archive` only ever turns the flag on (`seta`).
    pub fn set(&mut self, name: &str, value: &str, archive: bool) {
        let c = self
            .map
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| Cvar {
                value: String::new(),
                archive: false,
            });
        value.clone_into(&mut c.value);
        c.archive |= archive;
    }

    /// `(name, value)` of every cvar whose lowercase name starts with `prefix`, sorted by name.
    pub fn with_prefix(&self, prefix: &str) -> Vec<(String, String)> {
        let mut v: Vec<_> = self
            .map
            .iter()
            .filter(|(n, _)| n.starts_with(prefix))
            .map(|(n, c)| (n.clone(), c.value.clone()))
            .collect();
        v.sort_unstable();
        v
    }

    /// Archived `(name, value)` pairs, sorted by name.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn archived(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<_> = self
            .map
            .iter()
            .filter(|(_, c)| c.archive)
            .map(|(n, c)| (n.as_str(), c.value.as_str()))
            .collect();
        v.sort_unstable();
        v
    }
}
