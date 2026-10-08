// SPDX-License-Identifier: GPL-3.0-only
//! Console variables (the original calls them dvars).
//!
//! Names are case-insensitive. A variable set before it is registered (`+set` on the command
//! line, a cfg, or script `setdvar`) keeps its value when the engine registers it later, as in
//! the original.

use std::collections::BTreeMap;

pub const ARCHIVE: u32 = 1;
pub const USERINFO: u32 = 2;
pub const SERVERINFO: u32 = 4;
pub const SYSTEMINFO: u32 = 8;
pub const LATCH: u32 = 32;
pub const ROM: u32 = 64;
pub const CHEAT: u32 = 128;

#[derive(Debug, Clone)]
pub struct Cvar {
    pub name: String,
    pub value: String,
    pub default: String,
    pub flags: u32,
    /// A `LATCH` variable takes a new value at the next map load.
    pub latched: Option<String>,
    /// Registered by the engine (as opposed to created by a bare `set`).
    pub registered: bool,
}

#[derive(Default)]
pub struct Cvars {
    vars: BTreeMap<String, Cvar>,
}

fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl Cvars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `name` with a default. A value set earlier is kept; flags are merged.
    pub fn register(&mut self, name: &str, default: &str, flags: u32) {
        match self.vars.get_mut(&key(name)) {
            Some(v) => {
                v.default = default.to_owned();
                v.flags |= flags;
                v.registered = true;
                v.name = name.to_owned();
                if flags & ROM != 0 {
                    v.value = default.to_owned();
                }
            }
            None => {
                self.vars.insert(
                    key(name),
                    Cvar {
                        name: name.to_owned(),
                        value: default.to_owned(),
                        default: default.to_owned(),
                        flags,
                        latched: None,
                        registered: true,
                    },
                );
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&Cvar> {
        self.vars.get(&key(name))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.vars.contains_key(&key(name))
    }

    /// The value, or `""` for an unknown variable.
    pub fn string(&self, name: &str) -> &str {
        self.get(name).map_or("", |v| v.value.as_str())
    }

    pub fn int(&self, name: &str) -> i32 {
        parse_int(self.string(name))
    }

    pub fn float(&self, name: &str) -> f32 {
        parse_float(self.string(name))
    }

    pub fn bool(&self, name: &str) -> bool {
        self.int(name) != 0
    }

    /// Sets a value from the console, a cfg or a script. `ROM` variables refuse; `LATCH`
    /// variables keep the old value until [`Cvars::apply_latched`]. Returns whether the
    /// value was accepted.
    pub fn set(&mut self, name: &str, value: &str) -> bool {
        self.set_inner(name, value, false)
    }

    /// Like [`Cvars::set`] but also writes `ROM` variables (the engine's own writes).
    pub fn force(&mut self, name: &str, value: &str) {
        self.set_inner(name, value, true);
    }

    fn set_inner(&mut self, name: &str, value: &str, force: bool) -> bool {
        let v = self.vars.entry(key(name)).or_insert_with(|| Cvar {
            name: name.to_owned(),
            value: String::new(),
            default: String::new(),
            flags: 0,
            latched: None,
            registered: false,
        });
        if v.flags & ROM != 0 && !force {
            return false;
        }
        if v.flags & LATCH != 0 && !force && v.registered {
            if v.value != value {
                v.latched = Some(value.to_owned());
            }
            return true;
        }
        v.value = value.to_owned();
        v.latched = None;
        true
    }

    pub fn add_flags(&mut self, name: &str, flags: u32) {
        if let Some(v) = self.vars.get_mut(&key(name)) {
            v.flags |= flags;
        }
    }

    /// Applies every latched value (map load).
    pub fn apply_latched(&mut self) {
        for v in self.vars.values_mut() {
            if let Some(l) = v.latched.take() {
                v.value = l;
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cvar> {
        self.vars.values()
    }

    /// `\key\value` pairs of the variables with `flag` set, as the server info strings.
    pub fn info_string(&self, flag: u32) -> String {
        let mut s = String::new();
        for v in self.vars.values().filter(|v| v.flags & flag != 0) {
            s.push('\\');
            s.push_str(&v.name);
            s.push('\\');
            s.push_str(&v.value);
        }
        s
    }

    /// `seta` lines for the archived variables (`config_mp.cfg` body).
    pub fn archive_lines(&self) -> Vec<String> {
        self.vars
            .values()
            .filter(|v| v.flags & ARCHIVE != 0)
            .map(|v| format!("seta {} \"{}\"", v.name, v.value))
            .collect()
    }
}

/// `atoi`: leading integer, 0 when there is none.
pub fn parse_int(s: &str) -> i32 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse().unwrap_or(0)
}

/// `atof`: leading float, 0 when there is none.
pub fn parse_float(s: &str) -> f32 {
    let t = s.trim_start();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_exp = false;
    let b = t.as_bytes();
    while end < b.len() {
        let c = b[end];
        let ok = c.is_ascii_digit()
            || ((c == b'-' || c == b'+') && (end == 0 || matches!(b[end - 1], b'e' | b'E')))
            || (c == b'.' && !seen_dot && !seen_exp)
            || ((c == b'e' || c == b'E') && !seen_exp && end > 0);
        if !ok {
            break;
        }
        seen_dot |= c == b'.';
        seen_exp |= c == b'e' || c == b'E';
        end += 1;
    }
    // Back off a dangling exponent or sign.
    while end > 0 && !b[end - 1].is_ascii_digit() && b[end - 1] != b'.' {
        end -= 1;
    }
    t[..end].parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_before_register_survives_and_latch_waits() {
        let mut c = Cvars::new();
        c.set("G_GameType", "tdm");
        c.register("g_gametype", "war", LATCH);
        assert_eq!(c.string("g_gametype"), "tdm");
        c.set("g_gametype", "dm");
        assert_eq!(c.string("g_gametype"), "tdm");
        c.apply_latched();
        assert_eq!(c.string("g_gametype"), "dm");
    }

    #[test]
    fn rom_refuses_script_writes_but_not_engine_writes() {
        let mut c = Cvars::new();
        c.register("mapname", "", ROM | SERVERINFO);
        assert!(!c.set("mapname", "x"));
        c.force("mapname", "mp_crash");
        assert_eq!(c.info_string(SERVERINFO), "\\mapname\\mp_crash");
    }

    #[test]
    fn atoi_atof_prefixes() {
        assert_eq!(parse_int("12abc"), 12);
        assert_eq!(parse_int("-3"), -3);
        assert_eq!(parse_int("x"), 0);
        assert_eq!(parse_float("1.5e2x"), 150.0);
        assert_eq!(parse_float("0.05"), 0.05);
        assert_eq!(parse_float("-"), 0.0);
        assert_eq!(parse_float("1e"), 1.0);
    }
}
