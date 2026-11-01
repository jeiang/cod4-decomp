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
    // time, the blood limit, team-only chat, no console printing and the window position.
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
    // KisakCOD (GPL-3.0), gfx_d3d/r_dvars.cpp, R_RegisterDvars: renderer defaults and flags.
    // `DVAR_SAVED`, like `DVAR_ARCHIVE`, keeps ordinary `set` writes in the config.
    ("r_filmUseTweaks", "0", false),
    ("r_filmTweakEnable", "0", true),
    ("r_filmTweakContrast", "1.4", true),
    ("r_filmTweakBrightness", "0", true),
    ("r_filmTweakDesaturation", "0.2", true),
    ("r_filmTweakInvert", "0", true),
    ("r_filmTweakDarkTint", "0.7 0.85 1", true),
    ("r_filmTweakLightTint", "1.1 1.05 0.85", true),
    ("r_glow_allowed", "1", true),
    ("r_glow_allowed_script_forced", "0", true),
    ("r_glow", "1", false),
    ("r_glowUseTweaks", "0", false),
    ("r_glowTweakEnable", "0", false),
    ("r_glowTweakRadius0", "5", false),
    ("r_glowTweakBloomIntensity0", "1", false),
    ("r_glowTweakBloomCutoff", "0.5", false),
    ("r_glowTweakBloomDesaturation", "0", false),
    ("r_znear", "4", false),
    ("r_znear_depthhack", "0.1", false),
    ("r_zfar", "0", false),
    ("r_outdoor", "1", true),
    ("r_outdoorFeather", "8", true),
    ("r_outdoorAwayBias", "32", true),
    ("r_outdoorDownBias", "0", true),
    ("r_sun_from_dvars", "0", false),
    ("r_lightTweakSunLight", "1", false),
    ("r_lightTweakSunColor", "0 1 0 1", false),
    ("r_lightTweakSunDiffuseColor", "0 0 1 1", false),
    ("r_lightTweakSunDirection", "0 0 0", true),
    ("r_lightTweakAmbient", "0.1", false),
    ("r_lightTweakAmbientColor", "1 0 0 1", false),
    ("r_lightTweakDiffuseFraction", "0.5", false),
    ("r_blur", "0", false),
    // R_RegisterSunDvars, gfx_d3d/r_sky.cpp: the source set for `r_sun_from_dvars`, all `DVAR_NOFLAG`.
    ("r_sunsprite_shader", "sun", false),
    ("r_sunsprite_size", "16", false),
    ("r_sunflare_shader", "sun_flare", false),
    ("r_sunflare_min_size", "0", false),
    ("r_sunflare_min_angle", "45", false),
    ("r_sunflare_max_size", "2500", false),
    ("r_sunflare_max_angle", "2", false),
    ("r_sunflare_max_alpha", "1", false),
    ("r_sunflare_fadein", "1", false),
    ("r_sunflare_fadeout", "1", false),
    ("r_sunblind_min_angle", "30", false),
    ("r_sunblind_max_angle", "5", false),
    ("r_sunblind_max_darken", "0.75", false),
    ("r_sunblind_fadein", "0.5", false),
    ("r_sunblind_fadeout", "3", false),
    ("r_sunglare_min_angle", "30", false),
    ("r_sunglare_max_angle", "5", false),
    ("r_sunglare_max_lighten", "0.75", false),
    ("r_sunglare_fadein", "0.5", false),
    ("r_sunglare_fadeout", "3", false),
    ("r_sun_fx_position", "0 0 0", false),
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
    // `CG_CompassRegisterDvars`: the multiplayer minimap's scale, `DVAR_ARCHIVE | DVAR_CHEAT | DVAR_SAVED` (3500 in
    // single player).
    ("compassMaxRange", "2500", true),
];

/// The cvars a console, bind or menu may not set (`DVAR_ROM`).
const READ_ONLY: &[&str] = &[
    "cl_paused",
    // `DVAR_ROM | DVAR_AUTOEXEC` in R_RegisterDvars.
    "r_lightTweakAmbient",
    "r_lightTweakAmbientColor",
    "r_lightTweakDiffuseFraction",
    "r_lightTweakSunDiffuseColor",
];

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
    ("clientSideEffects", "1"),
    ("cg_crosshairAlpha", "1"),
    ("cg_crosshairAlphaMin", "0.5"),
    ("cg_centertime", "3"),
    ("r_brightness", "0"),
    ("r_contrast", "1"),
    ("r_desaturation", "1"),
    ("r_filmUseTweaks", "0"),
    ("r_glow", "1"),
    ("r_glowUseTweaks", "0"),
    ("r_glowTweakEnable", "0"),
    ("r_glowTweakRadius0", "5"),
    ("r_glowTweakBloomIntensity0", "1"),
    ("r_glowTweakBloomCutoff", "0.5"),
    ("r_glowTweakBloomDesaturation", "0"),
    ("r_znear", "4"),
    ("r_znear_depthhack", "0.1"),
    ("r_zfar", "0"),
    ("r_sun_from_dvars", "0"),
    ("r_lightTweakSunLight", "1"),
    ("r_lightTweakSunColor", "0 1 0 1"),
    ("r_lightTweakSunDirection", "0 0 0"),
    ("r_blur", "0"),
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
    ("compassMaxRange", "2500"),
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
    reset: Option<String>,
    /// The value it was registered with: the default, or the first value of one a command created
    /// (`Dvar_RegisterVariant` with an external source keeps it as the reset value).
    registered: String,
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
                (
                    n.to_ascii_lowercase(),
                    Cvar {
                        registered: value.clone(),
                        value,
                        archive,
                        reset: None,
                    },
                )
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
        self.f32_or(name, 0.0)
    }

    /// The value as a float, or `default` when unset or not a number.
    pub fn f32_or(&self, name: &str, default: f32) -> f32 {
        let Some(v) = self.get(name) else {
            return default;
        };
        v.trim().parse().unwrap_or_else(|_| {
            if !v.trim().is_empty() {
                logging::dedup!(WARN, name, cvar = name, "dvar is not a number, read as 0");
            }
            default
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
                if let Some(c) = self.map.get_mut(&name.to_ascii_lowercase()) {
                    c.reset.as_deref().unwrap_or(reset).clone_into(&mut c.value);
                } else {
                    self.set(name, reset, false);
                }
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
                reset: None,
                registered: value.to_owned(),
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

    /// Engine-only world registration: replace the current value and its cheat reset value without changing
    /// archive flags. Map lighting uses this when stock re-registers its dvars from the world's sun settings.
    pub fn set_reset(&mut self, name: &str, value: &str) {
        let c = self
            .map
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| Cvar {
                value: String::new(),
                archive: false,
                reset: None,
                registered: value.to_owned(),
            });
        value.clone_into(&mut c.value);
        value.clone_into(c.reset.get_or_insert_with(String::new));
    }

    /// What `reset <name>` restores (`Dvar_Reset`): the engine's reset value, a cheat dvar's, else the value it was
    /// registered with. `None` for no such dvar.
    pub fn reset_value(&self, name: &str) -> Option<&str> {
        let c = self.map.get(&name.to_ascii_lowercase())?;
        Some(
            c.reset
                .as_deref()
                .or_else(|| {
                    CHEAT
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(name))
                        .map(|&(_, r)| r)
                })
                .unwrap_or(&c.registered),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Input;

    #[test]
    fn renderer_defaults_and_flags_are_registered_before_any_assignment() {
        let cvars = Cvars::default();
        // Independent expectations from KisakCOD gfx_d3d/r_dvars.cpp, R_RegisterDvars.
        // name, default, saved/archive, cheat, read only
        for (name, default, archive, cheat, read_only) in [
            ("r_brightness", "0", false, true, false),
            ("r_contrast", "1", false, true, false),
            ("r_desaturation", "1", false, true, false),
            ("r_filmUseTweaks", "0", false, true, false),
            ("r_filmTweakEnable", "0", true, false, false),
            ("r_filmTweakContrast", "1.4", true, false, false),
            ("r_filmTweakBrightness", "0", true, false, false),
            ("r_filmTweakDesaturation", "0.2", true, false, false),
            ("r_filmTweakInvert", "0", true, false, false),
            ("r_filmTweakDarkTint", "0.7 0.85 1", true, false, false),
            ("r_filmTweakLightTint", "1.1 1.05 0.85", true, false, false),
            ("r_glow_allowed", "1", true, false, false),
            ("r_glow_allowed_script_forced", "0", true, false, false),
            ("r_glow", "1", false, true, false),
            ("r_glowUseTweaks", "0", false, true, false),
            ("r_glowTweakEnable", "0", false, true, false),
            ("r_glowTweakRadius0", "5", false, true, false),
            ("r_glowTweakBloomIntensity0", "1", false, true, false),
            ("r_glowTweakBloomCutoff", "0.5", false, true, false),
            ("r_glowTweakBloomDesaturation", "0", false, true, false),
            ("r_znear", "4", false, true, false),
            ("r_znear_depthhack", "0.1", false, true, false),
            ("r_zfar", "0", false, true, false),
            ("r_outdoor", "1", true, false, false),
            ("r_outdoorFeather", "8", true, false, false),
            ("r_outdoorAwayBias", "32", true, false, false),
            ("r_outdoorDownBias", "0", true, false, false),
            ("r_sun_from_dvars", "0", false, true, false),
            ("r_lightTweakSunLight", "1", false, true, false),
            ("r_lightTweakSunColor", "0 1 0 1", false, true, false),
            ("r_lightTweakSunDiffuseColor", "0 0 1 1", false, false, true),
            ("r_lightTweakSunDirection", "0 0 0", true, true, false),
            ("r_lightTweakAmbient", "0.1", false, false, true),
            ("r_lightTweakAmbientColor", "1 0 0 1", false, false, true),
            ("r_lightTweakDiffuseFraction", "0.5", false, false, true),
            ("r_blur", "0", false, true, false),
            // KisakCOD cgame/cg_compass.cpp, CG_CompassRegisterDvars (multiplayer).
            ("compassMaxRange", "2500", true, true, false),
        ] {
            assert_eq!(cvars.get(name), Some(default), "{name}");
            assert_eq!(Cvars::cheat_name(name).is_some(), cheat, "{name}");
            assert_eq!(Cvars::read_only(name), read_only, "{name}");
            let entries = cvars.entries(Some(name));
            assert_eq!(entries.len(), 1, "{name}");
            assert_eq!(entries[0].2, archive, "{name}");
        }
    }

    #[test]
    fn sun_flare_override_sources_have_fresh_stock_defaults_and_no_flags() {
        let cvars = Cvars::default();
        // KisakCOD gfx_d3d/r_sky.cpp, R_RegisterSunDvars.
        for (name, default) in [
            ("r_sunsprite_shader", "sun"),
            ("r_sunsprite_size", "16"),
            ("r_sunflare_shader", "sun_flare"),
            ("r_sunflare_min_size", "0"),
            ("r_sunflare_min_angle", "45"),
            ("r_sunflare_max_size", "2500"),
            ("r_sunflare_max_angle", "2"),
            ("r_sunflare_max_alpha", "1"),
            ("r_sunflare_fadein", "1"),
            ("r_sunflare_fadeout", "1"),
            ("r_sunblind_min_angle", "30"),
            ("r_sunblind_max_angle", "5"),
            ("r_sunblind_max_darken", "0.75"),
            ("r_sunblind_fadein", "0.5"),
            ("r_sunblind_fadeout", "3"),
            ("r_sunglare_min_angle", "30"),
            ("r_sunglare_max_angle", "5"),
            ("r_sunglare_max_lighten", "0.75"),
            ("r_sunglare_fadein", "0.5"),
            ("r_sunglare_fadeout", "3"),
            ("r_sun_fx_position", "0 0 0"),
        ] {
            assert_eq!(cvars.get(name), Some(default), "{name}");
            assert_eq!(Cvars::cheat_name(name), None, "{name}");
            assert!(!Cvars::read_only(name), "{name}");
            assert!(!Cvars::write_protected(name), "{name}");
            let entries = cvars.entries(Some(name));
            assert_eq!(entries.len(), 1, "{name}");
            assert!(!entries[0].2, "{name}");
        }
    }

    #[test]
    fn renderer_cheat_commands_are_refused_and_reset_to_registered_defaults() {
        let mut input = Input::detached();
        for (name, changed, default) in [
            ("r_brightness", "0.5", "0"),
            ("r_contrast", "2", "1"),
            ("r_desaturation", "0.5", "1"),
            ("r_filmUseTweaks", "1", "0"),
            ("r_glow", "0", "1"),
            ("r_glowUseTweaks", "1", "0"),
            ("r_glowTweakEnable", "1", "0"),
            ("r_glowTweakRadius0", "7", "5"),
            ("r_glowTweakBloomIntensity0", "2", "1"),
            ("r_glowTweakBloomCutoff", "0.25", "0.5"),
            ("r_glowTweakBloomDesaturation", "0.5", "0"),
            ("r_znear", "8", "4"),
            ("r_znear_depthhack", "0.2", "0.1"),
            ("r_zfar", "4096", "0"),
            ("r_sun_from_dvars", "1", "0"),
            ("r_lightTweakSunLight", "2", "1"),
            ("r_lightTweakSunColor", "1 0 0 1", "0 1 0 1"),
            ("r_lightTweakSunDirection", "10 20 30", "0 0 0"),
            ("r_blur", "4", "0"),
            ("compassMaxRange", "10000", "2500"),
        ] {
            input.cvars.set_cheat_state(true);
            assert_eq!(input.cvar(name), Some(default), "{name}");
            input.exec_line(&format!("set {name} \"{changed}\""));
            assert_eq!(input.cvar(name), Some(changed), "{name}");
            input.cvars.set_cheat_state(false);
            assert_eq!(input.cvar(name), Some(default), "{name}");
            input.take_messages();
            input.exec_line(&format!("set {name} \"{changed}\""));
            assert_eq!(input.cvar(name), Some(default), "{name}");
            assert!(
                input
                    .take_messages()
                    .iter()
                    .any(|message| message.contains("is cheat protected")),
                "{name}"
            );
        }
    }

    #[test]
    fn renderer_saved_flags_archive_ordinary_set_commands() {
        let mut input = Input::detached();
        for (name, changed) in [
            ("r_filmTweakEnable", "1"),
            ("r_filmTweakContrast", "1.8"),
            ("r_filmTweakBrightness", "0.25"),
            ("r_filmTweakDesaturation", "0.5"),
            ("r_filmTweakInvert", "1"),
            ("r_filmTweakDarkTint", "1 1 1"),
            ("r_filmTweakLightTint", "1 1 1"),
            ("r_glow_allowed", "0"),
            ("r_glow_allowed_script_forced", "1"),
            ("r_outdoor", "0"),
            ("r_outdoorFeather", "16"),
            ("r_outdoorAwayBias", "64"),
            ("r_outdoorDownBias", "8"),
            ("r_lightTweakSunDirection", "10 20 30"),
        ] {
            input.exec_line(&format!("set {name} \"{changed}\""));
            assert!(
                input
                    .cvars
                    .archived()
                    .contains(&(name.to_ascii_lowercase().as_str(), changed)),
                "{name}"
            );
        }
    }

    #[test]
    fn renderer_read_only_lighting_refuses_console_writes() {
        let mut input = Input::detached();
        for (name, default) in [
            ("r_lightTweakAmbient", "0.1"),
            ("r_lightTweakAmbientColor", "1 0 0 1"),
            ("r_lightTweakDiffuseFraction", "0.5"),
            ("r_lightTweakSunDiffuseColor", "0 0 1 1"),
        ] {
            input.exec_line(&format!("set {name} 0"));
            assert_eq!(input.cvar(name), Some(default), "{name}");
            assert!(
                input
                    .take_messages()
                    .iter()
                    .any(|message| message.contains("is read only")),
                "{name}"
            );
        }
    }

    #[test]
    fn world_lighting_reset_survives_cheat_writes_without_changing_archive_flags() {
        let mut cvars = Cvars::default();
        cvars.set_reset("r_lightTweakSunLight", "1.75");
        cvars.set_reset("r_lightTweakSunDirection", "10 20 30");
        assert_eq!(cvars.get("r_lightTweakSunLight"), Some("1.75"));
        cvars.set("r_lightTweakSunLight", "3", false);
        cvars.set("r_lightTweakSunDirection", "40 50 60", false);
        cvars.set("r_blur", "8", false);
        cvars.set_cheat_state(false);
        assert_eq!(cvars.get("r_lightTweakSunLight"), Some("1.75"));
        assert_eq!(cvars.get("r_lightTweakSunDirection"), Some("10 20 30"));
        assert_eq!(cvars.get("r_blur"), Some("0"));
        assert!(
            !cvars
                .archived()
                .iter()
                .any(|(name, _)| *name == "r_lighttweaksunlight")
        );
        assert!(
            cvars
                .archived()
                .contains(&("r_lighttweaksundirection", "10 20 30"))
        );
        // The next world replaces the previous world's reset, not only its current value.
        cvars.set_reset("r_lightTweakSunLight", "0.5");
        cvars.set("r_lightTweakSunLight", "2", false);
        cvars.set_cheat_state(false);
        assert_eq!(cvars.get("r_lightTweakSunLight"), Some("0.5"));
    }
}
