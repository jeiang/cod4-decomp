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

/// What a registered variable holds and the values it accepts (`dvarType_t` with its `DvarLimits`). A value is
/// read as the type (`Dvar_StringToValue`), refused outside the range (`Dvar_ValueInDomain`) and kept as the type
/// prints it (`Dvar_ValueToString`: `"1"`/`"0"`, `%i`, `%g`), KisakCOD `universal/dvar.cpp:396-440,479,1848`.
/// An enum (`dedicated`) is an `Int` over its indexes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Kind {
    Bool,
    Int(i32, i32),
    Float(f32, f32),
    /// Two floats, `"x y"`, each in the range.
    Vec2(f32, f32),
    #[default]
    String,
}

impl Kind {
    /// `value` as this type stores it, or `None` when it is outside the range.
    pub fn read(self, value: &str) -> Option<String> {
        let floats = |v: &str| {
            let mut w = v.split_whitespace().map(parse_float);
            [w.next().unwrap_or(0.0), w.next().unwrap_or(0.0)]
        };
        match self {
            Kind::String => Some(value.to_owned()),
            Kind::Bool => Some(if parse_int(value) != 0 { "1" } else { "0" }.to_owned()),
            Kind::Int(lo, hi) => {
                let n = parse_int(value);
                (lo..=hi).contains(&n).then(|| n.to_string())
            }
            Kind::Float(lo, hi) => {
                let x = parse_float(value);
                (lo..=hi).contains(&x).then(|| fmt_g(x))
            }
            Kind::Vec2(lo, hi) => {
                let [x, y] = floats(value);
                ((lo..=hi).contains(&x) && (lo..=hi).contains(&y))
                    .then(|| format!("{} {}", fmt_g(x), fmt_g(y)))
            }
        }
    }

    /// `value` as this type stores it, held to the range (`Dvar_ClampValueToDomain`): what a value set before the
    /// registration becomes when the variable is registered.
    fn clamp(self, value: &str) -> String {
        match self {
            Kind::String | Kind::Bool => self.read(value).unwrap_or_default(),
            Kind::Int(lo, hi) => parse_int(value).clamp(lo, hi).to_string(),
            Kind::Float(lo, hi) => fmt_g(parse_float(value).clamp(lo, hi)),
            Kind::Vec2(lo, hi) => {
                let mut w = value.split_whitespace().map(parse_float);
                let mut c = || fmt_g(w.next().unwrap_or(0.0).clamp(lo, hi));
                let x = c();
                format!("{x} {}", c())
            }
        }
    }
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
    /// The type and range a new value must have (checked for every write that is not the engine's own).
    pub kind: Kind,
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

    /// Registers `name` as a string with a default. A value set earlier is kept; flags are merged.
    pub fn register(&mut self, name: &str, default: &str, flags: u32) {
        self.register_kind(name, Kind::String, default, flags);
    }

    /// Registers `name` with its type and range (`Dvar_Register*`). A value set earlier is kept, read as the type
    /// and held to the range (`Dvar_MakeExplicitType`); flags are merged.
    pub fn register_kind(&mut self, name: &str, kind: Kind, default: &str, flags: u32) {
        let default = kind.read(default).unwrap_or_else(|| default.to_owned());
        let default = default.as_str();
        match self.vars.get_mut(&key(name)) {
            Some(v) => {
                v.default = default.to_owned();
                v.flags |= flags;
                v.registered = true;
                v.name = name.to_owned();
                v.kind = kind;
                v.value = if flags & ROM != 0 {
                    default.to_owned()
                } else {
                    kind.clamp(&v.value)
                };
                if let Some(l) = v.latched.take() {
                    v.latched = Some(kind.clamp(&l));
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
                        kind,
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

    /// The value as an integer. A Float or Vec2 dvar is read as its float and truncated, as the engine's
    /// `(int)dvar->current.value` does (KisakCOD `game_mp/g_active_mp.cpp:1153` for `g_gravity`): its stored `%g`
    /// text can be `1e+06`, which `atoi` would read as 1. Any other is read as `atoi`. Scripts' `getdvarint` reads
    /// the string with `atoi` (`game_mp/g_scr_main_mp.cpp:890-899`), not through this.
    pub fn int(&self, name: &str) -> i32 {
        let Some(v) = self.get(name) else {
            return 0;
        };
        match v.kind {
            Kind::Float(..) | Kind::Vec2(..) => parse_float(&v.value) as i32,
            _ => parse_int(&v.value),
        }
    }

    pub fn float(&self, name: &str) -> f32 {
        parse_float(self.string(name))
    }

    pub fn bool(&self, name: &str) -> bool {
        self.int(name) != 0
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
        let unchanged = self.vars.get(&key(name)).is_some_and(|v| v.value == value);
        let r = self.try_set(name, value, external);
        match &r {
            // The original prints the refusal too; one that would not change the value (the command line's `set` lines
            // run again after boot, `+set fs_game` among them) loses nothing.
            Err(why @ (Refused::ReadOnly(_) | Refused::WriteProtected(_))) if unchanged => {
                logging::dedup!(
                    INFO,
                    "refused",
                    cvar = name,
                    external,
                    "cvar set refused: {why}"
                );
            }
            Err(why) => {
                logging::dedup!(
                    WARN,
                    "refused",
                    cvar = name,
                    external,
                    "cvar set refused: {why}"
                );
            }
            Ok(()) => {}
        }
        r
    }

    fn try_set(&mut self, name: &str, value: &str, external: bool) -> Result<(), Refused> {
        let cheats = self.bool("sv_cheats");
        let v = self.vars.entry(key(name)).or_insert_with(|| Cvar {
            name: name.to_owned(),
            value: String::new(),
            default: String::new(),
            flags: 0,
            latched: None,
            kind: Kind::String,
            registered: false,
        });
        if v.flags & ROM != 0 {
            return Err(Refused::ReadOnly(v.name.clone()));
        }
        if v.flags & INIT != 0 {
            return Err(Refused::WriteProtected(v.name.clone()));
        }
        let Some(value) = v.kind.read(value) else {
            return Err(Refused::NotValid {
                name: v.name.clone(),
                value: value.to_owned(),
            });
        };
        if external && v.flags & CHEAT != 0 && !cheats {
            return Err(Refused::CheatProtected(v.name.clone()));
        }
        Self::store(&mut self.modified, v, &value, false);
        apply_logging(name, &value);
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
            kind: Kind::String,
            registered: false,
        });
        Self::store(&mut self.modified, v, value, true);
        apply_logging(name, value);
    }

    fn store(modified: &mut u32, v: &mut Cvar, value: &str, force: bool) {
        if v.flags & LATCH != 0 && !force && v.registered {
            // A latched write goes to the pending value unless it is already that, even when it equals the one
            // running: the pending value is what the next map applies, so a config that lowered a dvar and a later
            // `set` back to the running value must not leave the lower one waiting (`Dvar_SetLatchedValue`,
            // `dvar.cpp:1485-1521`, KisakCOD).
            if v.latched.as_deref() != Some(value) {
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

    /// Adds flags; the variable counts as changed for them, so a newly published one (`CODINFO` from a script's
    /// `makedvarserverinfo`) goes out at the next frame.
    pub fn add_flags(&mut self, name: &str, flags: u32) {
        if let Some(v) = self.vars.get_mut(&key(name)) {
            v.flags |= flags;
            self.modified |= flags;
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

    /// The value `name` takes at the next map load, when that is not the value it has now.
    ///
    /// Stock reports the difference only when the two values differ (`dvar.cpp:1392-1396`, KisakCOD), and only a
    /// difference changes the next map, so a latched value equal to the running one is not pending.
    pub fn pending(&self, name: &str) -> Option<&str> {
        let v = self.get(name)?;
        v.latched.as_deref().filter(|l| *l != v.value)
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

/// `developer` and `log_filter` steer the log level (see `logging`).
fn apply_logging(name: &str, value: &str) {
    if name.eq_ignore_ascii_case("developer") {
        logging::set_developer(i64::from(parse_int(value)));
    } else if name.eq_ignore_ascii_case("log_filter")
        && let Err(e) = logging::set_filter(value)
    {
        tracing::warn!(filter = value, "log_filter does not parse: {e}");
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

/// `%g`: six significant digits, trailing zeros dropped, an exponent below 1e-4 or from 1e6 on.
pub fn fmt_g(x: f32) -> String {
    let x = f64::from(x);
    if x == 0.0 || !x.is_finite() {
        return if x == 0.0 {
            "0".to_owned()
        } else {
            x.to_string()
        };
    }
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        }
    };
    let sci = format!("{x:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        trim(format!("{:.*}", (5 - exp) as usize, x))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa.to_owned()), exp.abs())
    }
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
    fn a_latched_write_replaces_the_pending_value_even_with_the_running_one() {
        // `default_mp_gamesettings.cfg` lowers `sv_maxclients` from 32; a later `set sv_maxclients 32` must leave
        // nothing waiting, and the map keeps 32 (`Dvar_SetLatchedValue`, `dvar.cpp:1485-1521`, KisakCOD).
        let mut c = Cvars::new();
        c.register("sv_maxclients", "32", ARCHIVE | SERVERINFO | LATCH);
        c.set("sv_maxclients", "24");
        assert_eq!(c.string("sv_maxclients"), "32", "latched, not applied");
        assert_eq!(c.pending("sv_maxclients"), Some("24"));
        c.set("sv_maxclients", "32");
        assert_eq!(c.pending("sv_maxclients"), None);
        c.apply_latched();
        assert_eq!(c.string("sv_maxclients"), "32");
        c.set("sv_maxclients", "40");
        assert_eq!(c.pending("sv_maxclients"), Some("40"));
        c.apply_latched();
        assert_eq!(c.int("sv_maxclients"), 40);
    }

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
    fn a_refused_set_warns_with_the_variable() {
        let mut c = Cvars::new();
        c.register("logtest_rom", "", ROM);
        let (ok, events) = logging::capture(|| c.set("logtest_rom", "x"));
        assert!(!ok);
        let w = events.iter().find(|e| e.level == tracing::Level::WARN);
        assert_eq!(w.and_then(|e| e.field("cvar")), Some("logtest_rom"));
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
        c.register_kind("sv_fps", Kind::Int(10, 1000), "30", 0);
        assert!(matches!(
            c.set_external("sv_fps", "5"),
            Err(Refused::NotValid { .. })
        ));
        assert!(!c.set("sv_fps", "1001"));
        assert!(c.set("sv_fps", "60"));
        assert_eq!(c.int("sv_fps"), 60);
        c.register_kind("g_voteAbstainWeight", Kind::Float(0.0, 1.0), "0.5", 0);
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
    fn a_typed_value_is_kept_as_its_type_prints_it() {
        let mut c = Cvars::new();
        c.register_kind("sv_voice", Kind::Bool, "0", 0);
        assert!(c.set("sv_voice", "7"));
        assert_eq!(c.string("sv_voice"), "1", "Dvar_StringToBool: atoi != 0");
        assert!(c.set("sv_voice", "x"));
        assert_eq!(c.string("sv_voice"), "0");
        c.register_kind("sv_reconnectlimit", Kind::Int(0, 1800), "3", 0);
        assert!(c.set("sv_reconnectlimit", "60s"));
        assert_eq!(c.string("sv_reconnectlimit"), "60", "%i of atoi");
        c.register_kind("g_gravity", Kind::Float(1.0, f32::MAX), "800", 0);
        assert!(c.set("g_gravity", "0800.50"));
        assert_eq!(c.string("g_gravity"), "800.5", "%g of atof");
        c.register_kind(
            "bg_bobAmplitudeProne",
            Kind::Vec2(0.0, 1.0),
            "0.02 0.005",
            0,
        );
        assert!(!c.set("bg_bobAmplitudeProne", "0.5 2"));
        assert!(c.set("bg_bobAmplitudeProne", "0.5"));
        assert_eq!(c.string("bg_bobAmplitudeProne"), "0.5 0");
    }

    #[test]
    fn a_value_set_before_registration_is_read_as_the_type_and_held_to_the_range() {
        let mut c = Cvars::new();
        c.set("sv_maxclients", "99");
        c.set("g_antilag", "yes");
        c.register_kind("sv_maxclients", Kind::Int(1, 64), "32", 0);
        c.register_kind("g_antilag", Kind::Bool, "1", 0);
        assert_eq!(c.string("sv_maxclients"), "64");
        assert_eq!(c.string("g_antilag"), "0");
    }

    #[test]
    fn an_integer_read_of_a_float_dvar_takes_its_exponent_form_whole() {
        let mut c = Cvars::new();
        c.register_kind("g_gravity", Kind::Float(1.0, f32::MAX), "800", 0);
        assert!(c.set("g_gravity", "1000000"));
        assert_eq!(c.string("g_gravity"), "1e+06");
        assert_eq!(c.int("g_gravity"), 1_000_000);
        assert!(c.set("g_gravity", "799.9"));
        assert_eq!(c.int("g_gravity"), 799, "truncated, as (int) casts");
        c.set("loose", "1e+06");
        assert_eq!(c.int("loose"), 1, "an untyped variable is read as atoi");
    }

    #[test]
    fn fmt_g_prints_as_printf_does() {
        for (x, want) in [
            (0.0, "0"),
            (0.5, "0.5"),
            (800.0, "800"),
            (15000.0, "15000"),
            (0.007, "0.007"),
            (1e-8, "1e-08"),
            (123456.0, "123456"),
            (1234567.0, "1.23457e+06"),
            (-2.5, "-2.5"),
            (f32::MAX, "3.40282e+38"),
        ] {
            assert_eq!(fmt_g(x), want, "{x}");
        }
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
