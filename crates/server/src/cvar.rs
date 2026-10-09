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
/// `DVAR_INIT`: set only on the command line or by the engine; the console refuses it.
pub const INIT: u32 = 16;
pub const LATCH: u32 = 32;
pub const ROM: u32 = 64;
pub const CHEAT: u32 = 128;
/// `DVAR_CODINFO`: published to the clients as a key/value pair in the `CODINFO` configstrings.
pub const CODINFO: u32 = 256;

/// The values a variable accepts (`DvarLimits`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Limit {
    Int(i32, i32),
    Float(f32, f32),
}

/// Why [`Cvars::set_checked`] refused a value, worded as the original's console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    ReadOnly(String),
    WriteProtected(String),
    CheatProtected(String),
    NotValid { name: String, value: String },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::ReadOnly(n) => write!(f, "{n} is read only."),
            Refused::WriteProtected(n) => write!(f, "{n} is write protected."),
            Refused::CheatProtected(n) => write!(f, "{n} is cheat protected."),
            Refused::NotValid { name, value } => {
                write!(f, "'{value}' is not a valid value for dvar '{name}'")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cvar {
    pub name: String,
    pub value: String,
    pub default: String,
    pub flags: u32,
    /// A `LATCH` variable takes a new value at the next map load.
    pub latched: Option<String>,
    /// The range a new value must be in (checked for every write that is not the engine's own).
    pub limit: Option<Limit>,
    /// Registered by the engine (as opposed to created by a bare `set`).
    pub registered: bool,
}

#[derive(Default)]
pub struct Cvars {
    vars: BTreeMap<String, Cvar>,
    /// The flags of every variable whose value changed since [`Cvars::take_modified`] (`dvar_modifiedFlags`).
    modified: u32,
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
                        limit: None,
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

    /// Gives `name` the range of values it accepts.
    pub fn set_limit(&mut self, name: &str, limit: Limit) {
        if let Some(v) = self.vars.get_mut(&key(name)) {
            v.limit = Some(limit);
        }
    }

    /// Sets a value from a script or the engine's own commands. `ROM` and `INIT` variables and values outside
    /// the range refuse; `LATCH` variables keep the old value until [`Cvars::apply_latched`]. Returns whether the
    /// value was accepted.
    pub fn set(&mut self, name: &str, value: &str) -> bool {
        self.set_checked(name, value, false).is_ok()
    }

    /// Like [`Cvars::set`] for a person at the console, the command line, an rcon or a passed vote: a `CHEAT`
    /// variable also refuses while `sv_cheats` is off, and the refusal says why.
    pub fn set_external(&mut self, name: &str, value: &str) -> Result<(), Refused> {
        self.set_checked(name, value, true)
    }

    fn set_checked(&mut self, name: &str, value: &str, external: bool) -> Result<(), Refused> {
        let cheats = self.bool("sv_cheats");
        let v = self.vars.entry(key(name)).or_insert_with(|| Cvar {
            name: name.to_owned(),
            value: String::new(),
            default: String::new(),
            flags: 0,
            latched: None,
            limit: None,
            registered: false,
        });
        if v.flags & ROM != 0 {
            return Err(Refused::ReadOnly(v.name.clone()));
        }
        if v.flags & INIT != 0 {
            return Err(Refused::WriteProtected(v.name.clone()));
        }
        let in_range = match v.limit {
            Some(Limit::Int(lo, hi)) => (lo..=hi).contains(&parse_int(value)),
            Some(Limit::Float(lo, hi)) => (lo..=hi).contains(&parse_float(value)),
            None => true,
        };
        if !in_range {
            return Err(Refused::NotValid {
                name: v.name.clone(),
                value: value.to_owned(),
            });
        }
        if external && v.flags & CHEAT != 0 && !cheats {
            return Err(Refused::CheatProtected(v.name.clone()));
        }
        Self::store(&mut self.modified, v, value, false);
        Ok(())
    }

    /// Like [`Cvars::set`] but also writes `ROM` and `INIT` variables and ignores the range (the engine's own writes).
    pub fn force(&mut self, name: &str, value: &str) {
        let v = self.vars.entry(key(name)).or_insert_with(|| Cvar {
            name: name.to_owned(),
            value: String::new(),
            default: String::new(),
            flags: 0,
            latched: None,
            limit: None,
            registered: false,
        });
        Self::store(&mut self.modified, v, value, true);
    }

    fn store(modified: &mut u32, v: &mut Cvar, value: &str, force: bool) {
        if v.flags & LATCH != 0 && !force && v.registered {
            if v.value != value {
                v.latched = Some(value.to_owned());
            }
            return;
        }
        if v.value != value {
            *modified |= v.flags;
        }
        v.value = value.to_owned();
        v.latched = None;
    }

    /// The flags of the variables changed since the last call (`dvar_modifiedFlags`), and forgets them.
    pub fn take_modified(&mut self) -> u32 {
        std::mem::take(&mut self.modified)
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
                if v.value != l {
                    self.modified |= v.flags;
                }
                v.value = l;
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cvar> {
        self.vars.values()
    }

    /// The name and value of each variable with `flag` set.
    pub fn info_pairs(&self, flag: u32) -> impl Iterator<Item = (&str, &str)> {
        self.vars
            .values()
            .filter(move |v| v.flags & flag != 0)
            .map(|v| (v.name.as_str(), v.value.as_str()))
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
    fn init_and_out_of_range_values_refuse_and_a_cheat_variable_waits_for_sv_cheats() {
        let mut c = Cvars::new();
        c.register("sv_cheats", "1", SYSTEMINFO | INIT);
        assert_eq!(
            c.set_external("sv_cheats", "0"),
            Err(Refused::WriteProtected("sv_cheats".into()))
        );
        assert!(!c.set("sv_cheats", "0"), "a script cannot either");
        c.register("sv_fps", "30", 0);
        c.set_limit("sv_fps", Limit::Int(10, 1000));
        assert!(matches!(
            c.set_external("sv_fps", "5"),
            Err(Refused::NotValid { .. })
        ));
        assert!(!c.set("sv_fps", "1001"));
        assert!(c.set("sv_fps", "60"));
        assert_eq!(c.int("sv_fps"), 60);
        c.register("g_voteAbstainWeight", "0.5", 0);
        c.set_limit("g_voteAbstainWeight", Limit::Float(0.0, 1.0));
        assert!(!c.set("g_voteAbstainWeight", "1.5"));
        c.register("pickupPrints", "0", CHEAT);
        c.force("sv_cheats", "0");
        assert_eq!(
            c.set_external("pickupPrints", "1"),
            Err(Refused::CheatProtected("pickupPrints".into()))
        );
        assert!(
            c.set("pickupPrints", "1"),
            "the scripts set cheat variables whatever sv_cheats says"
        );
        c.force("sv_cheats", "1");
        assert_eq!(c.set_external("pickupPrints", "0"), Ok(()));
    }

    #[test]
    fn the_flags_of_a_changed_variable_are_remembered_until_taken() {
        let mut c = Cvars::new();
        c.register("sv_hostname", "a", SERVERINFO);
        c.register("g_speed", "190", 0);
        c.set("g_speed", "200");
        assert_eq!(c.take_modified(), 0);
        c.set("sv_hostname", "a");
        assert_eq!(c.take_modified(), 0, "the same value is no change");
        c.set("sv_hostname", "b");
        assert_eq!(c.take_modified(), SERVERINFO);
        assert_eq!(c.take_modified(), 0);
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
