// SPDX-License-Identifier: GPL-3.0-only
//! The client cvar store: name -> string value, with a default and an archive flag.
//!
//! Names are case-insensitive (stored lowercase), as in the original. Only archived cvars are written back to the
//! config file.

use std::collections::HashMap;

/// `(name, default, archived)`. Every cvar the input layer reads has an entry so a read never misses.
const DEFAULTS: &[(&str, &str, bool)] = &[
    // The log level (stock `developer`: 1 is `debug`, 2 is `trace`) and a filter in `RUST_LOG` syntax; see `logging`.
    ("developer", "0", false),
    ("log_filter", "", false),
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
    // The ragdoll and rigid body physics tunables (`Ragdoll_InitDvars`, `Phys_Init`); the `ragdoll_*` ones but
    // `ragdoll_max_simulating` are cheat protected and act only with `sv_cheats` on.
    ("ragdoll_max_life", "4500", false),
    ("ragdoll_max_simulating", "16", true),
    ("ragdoll_jointlerp_time", "3000", false),
    ("ragdoll_explode_force", "18000", false),
    ("ragdoll_explode_upbias", "0.8", false),
    ("ragdoll_self_collision_scale", "1.2", false),
    ("ragdoll_rotvel_scale", "1", false),
    ("phys_bulletupbias", "0.5", false),
    ("phys_bulletspinscale", "3", true),
    // `cl_paused` is the engine's (`DVAR_ROM` in the multiplayer `CG_RegisterDvars`): multiplayer has no pause, and
    // the stock Pause key's `toggle cl_paused` is refused as read only.
    ("cl_paused", "0", false),
    // `Dvar_Init` registers it on (`SYSTEMINFO | INIT`); the server's systeminfo sets it at every join and level
    // start (`CL_SystemInfoChanged`, [`Cvars::set_cheat_state`]). A console write is refused as write protected.
    ("sv_cheats", "1", false),
    // Stock registrations this build reads (issue #369): the crosshair alpha (cheat, archived), the centre print
    // time, the blood limit, team-only chat, no console printing, the window position and the film tweaks.
    ("cg_crosshairAlpha", "1", true),
    ("cg_crosshairAlphaMin", "0.5", true),
    ("cg_centertime", "3", false),
    ("cg_bloodLimit", "0", true),
    ("cg_bloodLimitMsec", "330", true),
    ("cg_teamChatsOnly", "0", true),
    ("cl_noprint", "0", false),
    ("vid_xpos", "3", true),
    ("vid_ypos", "22", true),
    ("r_brightness", "0", false),
    ("r_contrast", "1", false),
    ("r_desaturation", "1", false),
    // The killcam cameras (`cgame_mp/cg_view_mp.cpp`), all cheat protected.
    ("cg_heliKillCamDist", "1000", false),
    ("cg_heliKillCamFov", "15", false),
    ("cg_heliKillCamZDist", "50", false),
    ("cg_heliKillCamNearBlur", "4", false),
    ("cg_heliKillCamFarBlur", "2", false),
    ("cg_heliKillCamFarBlurStart", "100", false),
    ("cg_heliKillCamFarBlurDist", "300", false),
    ("cg_heliKillCamNearBlurStart", "0", false),
    ("cg_heliKillCamNearBlurEnd", "100", false),
    ("cg_airstrikeKillCamDist", "200", false),
    ("cg_airstrikeKillCamFov", "80", false),
    ("cg_airstrikeKillCamCloseXYDist", "24", false),
    ("cg_airstrikeKillCamCloseZDist", "24", false),
    ("cg_airstrikeKillCamNearBlur", "4", false),
    ("cg_airstrikeKillCamFarBlur", "2", false),
    ("cg_airstrikeKillCamFarBlurStart", "100", false),
    ("cg_airstrikeKillCamFarBlurDist", "300", false),
    ("cg_airstrikeKillCamNearBlurStart", "0", false),
    ("cg_airstrikeKillCamNearBlurEnd", "100", false),
];

/// The cvars a console, bind or menu may not set (`DVAR_ROM`).
const READ_ONLY: &[&str] = &["cl_paused"];

/// The cvars only the engine sets (`DVAR_INIT`): a console write is refused as write protected.
const WRITE_PROTECTED: &[&str] = &["sv_cheats"];

/// The `DVAR_CHEAT` dvars this build reads, with their reset values: a console write is refused while `sv_cheats`
/// is off (`Dvar_SetVariant`, "is cheat protected"), and a server without cheats puts them back to these values
/// (`Dvar_SetCheatState`). The registered-case names are what the refusal prints.
const CHEAT: &[(&str, &str)] = &[
    ("cg_drawGun", "1"),
    ("cg_draw2D", "1"),
    ("cg_drawShellshock", "1"),
    ("r_fullbright", "0"),
    ("r_dof_bias", "0.5"),
    ("r_dof_tweak", "0"),
    ("snd_slaveFadeTime", "500"),
    ("snd_enableReverb", "1"),
    ("ragdoll_max_life", "4500"),
    ("ragdoll_jointlerp_time", "3000"),
    ("ragdoll_explode_force", "18000"),
    ("ragdoll_explode_upbias", "0.8"),
    ("ragdoll_self_collision_scale", "1.2"),
    ("ragdoll_rotvel_scale", "1"),
    ("cg_crosshairAlpha", "1"),
    ("cg_crosshairAlphaMin", "0.5"),
    ("cg_centertime", "3"),
    ("r_brightness", "0"),
    ("r_contrast", "1"),
    ("r_desaturation", "1"),
    ("cg_heliKillCamDist", "1000"),
    ("cg_heliKillCamFov", "15"),
    ("cg_heliKillCamZDist", "50"),
    ("cg_heliKillCamNearBlur", "4"),
    ("cg_heliKillCamFarBlur", "2"),
    ("cg_heliKillCamFarBlurStart", "100"),
    ("cg_heliKillCamFarBlurDist", "300"),
    ("cg_heliKillCamNearBlurStart", "0"),
    ("cg_heliKillCamNearBlurEnd", "100"),
    ("cg_airstrikeKillCamDist", "200"),
    ("cg_airstrikeKillCamFov", "80"),
    ("cg_airstrikeKillCamCloseXYDist", "24"),
    ("cg_airstrikeKillCamCloseZDist", "24"),
    ("cg_airstrikeKillCamNearBlur", "4"),
    ("cg_airstrikeKillCamFarBlur", "2"),
    ("cg_airstrikeKillCamFarBlurStart", "100"),
    ("cg_airstrikeKillCamFarBlurDist", "300"),
    ("cg_airstrikeKillCamNearBlurStart", "0"),
    ("cg_airstrikeKillCamNearBlurEnd", "100"),
];

/// Stock dvars whose other values this build cannot act on: the one value it has, and why. A write of any other
/// value is refused and says so, as stock refuses a value outside a dvar's domain (issue #369).
const UNSUPPORTED: &[(&str, &str, &str)] = &[
    (
        "snd_khz",
        "44",
        "the mixer runs at the sound device's own rate; this build has no 11/22 kHz output",
    ),
    (
        "snd_enableEq",
        "1",
        "the script equalizer cannot be switched off in this build",
    ),
    (
        "cg_drawLagometer",
        "0",
        "the lagometer is not drawn in this build",
    ),
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
                (n.to_ascii_lowercase(), Cvar { value, archive })
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
        let Some(v) = self.get(name) else {
            return 0.0;
        };
        v.trim().parse().unwrap_or_else(|_| {
            if !v.trim().is_empty() {
                logging::dedup!(WARN, name, cvar = name, "dvar is not a number, read as 0");
            }
            0.0
        })
    }

    pub fn bool(&self, name: &str) -> bool {
        self.f32(name) != 0.0
    }

    /// Whether a command (`set`, `toggle`, the name alone) may not write `name` (`DVAR_ROM`).
    pub fn read_only(name: &str) -> bool {
        READ_ONLY.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    /// Whether a command may not write `name` because only the engine sets it (`DVAR_INIT`).
    pub fn write_protected(name: &str) -> bool {
        WRITE_PROTECTED.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    /// The registered name of a `DVAR_CHEAT` dvar, for the refusal line; `None` for one that is not cheat protected.
    pub fn cheat_name(name: &str) -> Option<&'static str> {
        CHEAT
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|&(n, _)| n)
    }

    /// `sv_cheats` as the server's systeminfo says it; off, every cheat dvar goes back to its reset value
    /// (`Dvar_SetCheatState`, run by `CL_SystemInfoChanged` and by `G_InitGame`).
    pub fn set_cheat_state(&mut self, on: bool) {
        self.set("sv_cheats", if on { "1" } else { "0" }, false);
        if !on {
            for (name, reset) in CHEAT {
                self.set(name, reset, false);
            }
        }
    }

    /// For a dvar this build accepts only one value of: that value and why.
    pub fn unsupported_domain(name: &str) -> Option<(&'static str, &'static str)> {
        UNSUPPORTED
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            .map(|&(_, only, why)| (only, why))
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
        if name.eq_ignore_ascii_case("developer") {
            logging::set_developer(value.trim().parse().unwrap_or(0));
        } else if name.eq_ignore_ascii_case("log_filter")
            && let Err(e) = logging::set_filter(value)
        {
            tracing::warn!(filter = value, "log_filter does not parse: {e}");
        }
    }

    /// `(name, value, archived)` of every cvar whose name matches the glob `filter` (`*` and `?`, any case; all
    /// when `None`), sorted by name: what `dvarlist` shows.
    pub fn entries(&self, filter: Option<&str>) -> Vec<(String, String, bool)> {
        let mut v: Vec<_> = self
            .map
            .iter()
            .filter(|(n, _)| filter.is_none_or(|f| glob(&f.to_ascii_lowercase(), n)))
            .map(|(n, c)| (n.clone(), c.value.clone(), c.archive))
            .collect();
        v.sort_unstable();
        v
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

/// `Com_Filter`'s matching: `*` any run, `?` any one byte; `name` is already lowercase.
fn glob(pattern: &str, name: &str) -> bool {
    fn at(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| at(rest, &n[i..])),
            Some((b'?', rest)) => !n.is_empty() && at(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && at(rest, &n[1..]),
        }
    }
    at(pattern.as_bytes(), name.as_bytes())
}
