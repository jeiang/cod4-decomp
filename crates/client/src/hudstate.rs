// SPDX-License-Identifier: GPL-3.0-or-later
//! What the owner-draw HUD pieces read: facts of the player and the match, filled each frame from the network
//! play, and the pure logic over them (fades, the low-health pulse, ammo thresholds).
//!
//! The original keeps this in its client globals next to the snapshot; here it is plain owned data on
//! [`crate::shell::GameFacts`], so the drawing never reaches into the network layer. Times are the shell's
//! millisecond clock.

use crate::compass::MapInfo;
use std::collections::HashMap;

/// `CG_FadeColor`: alpha of something shown at `start` for `total` ms that fades out over the last `fade` ms;
/// `None` when it has not been shown (`start` of 0) or is over.
pub fn fade_color(now: i32, start: i32, total: i32, fade: i32) -> Option<f32> {
    if start == 0 {
        return None;
    }
    let t = now - start;
    if t >= total {
        return None;
    }
    let left = total - t;
    Some(if left < fade && fade > 0 {
        left as f32 / fade as f32
    } else {
        1.0
    })
}

/// `CG_FadeHudMenu`: the alpha of a HUD piece whose `hud_fade_*` dvar is `secs` and which was last shown at
/// `start`. A dvar of 0 never fades.
pub fn fade_hud(secs: f32, start: i32, now: i32) -> f32 {
    if secs == 0.0 {
        return 1.0;
    }
    fade_color(now, start, (secs * 1000.0).round() as i32, 700).unwrap_or(0.0)
}

/// `CG_CalcPlayerHealth`: the health bar's fill, 0 when dead or when the server has not said.
pub fn health_fraction(health: i32, max_health: i32, dead: bool) -> f32 {
    if health == 0 || max_health == 0 || dead {
        return 0.0;
    }
    (health as f32 / max_health as f32).clamp(0.0, 1.0)
}

/// `CG_CheckPlayerForLowAmmoSpecific`: the stock is a fifth of the most the player can carry or less.
pub fn low_ammo(stock: i32, player_max: i32) -> bool {
    let (cur, max) = (stock.min(999), player_max.min(999));
    max >= 0 && max as f32 * 0.2 >= cur as f32
}

/// `CG_CheckPlayerForLowClipSpecific`: the magazine is at or under the weapon's warning fraction.
pub fn low_clip(clip: i32, clip_size: i32, threshold: f32) -> bool {
    if clip < 0 {
        return false;
    }
    let (cur, full) = (clip.min(999), clip_size.min(999));
    full > 0 && full as f32 * threshold >= cur as f32
}

/// The `hud_healthOverlay_*` dvars (cheat protected, so only their stock values exist here).
pub struct OverlayParams {
    pub pulse_start: f32,
    pub phase_one_ms: i32,
    pub phase_two_mult: f32,
    pub phase_two_ms: i32,
    pub phase_three_mult: f32,
    pub phase_three_ms: i32,
    pub end_alpha: f32,
    pub end_ms: i32,
    pub regen_pause_ms: i32,
}

impl Default for OverlayParams {
    fn default() -> Self {
        Self {
            pulse_start: 0.55,
            phase_one_ms: 150,
            phase_two_mult: 0.7,
            phase_two_ms: 320,
            phase_three_mult: 0.6,
            phase_three_ms: 400,
            end_alpha: 0.0,
            end_ms: 700,
            regen_pause_ms: 8000,
        }
    }
}

/// Peak of each of the four pulses after a hit.
const PULSE_MAGS: [f32; 4] = [1.0, 0.8, 0.6, 0.3];

/// The low-health overlay's pulse (`CG_PulseLowHealthOverlay`, `CG_FadeLowHealthOverlay`): after a hit that leaves
/// the player under `pulse_start` the overlay ramps up, eases down in two steps and repeats at lower peaks until the
/// regeneration pause is over, then fades out.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    hurt: bool,
    from: f32,
    to: f32,
    duration: i32,
    phase: u8,
    index: usize,
    pulse_time: i32,
    last_hit: i32,
    old_health: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            hurt: false,
            from: 0.0,
            to: 0.0,
            duration: 0,
            phase: 0,
            index: 0,
            pulse_time: 0,
            last_hit: 0,
            old_health: 1.0,
        }
    }
}

impl Overlay {
    /// Forgets everything (a respawn: `CG_ResetLowHealthOverlay`).
    pub fn reset(&mut self, p: &OverlayParams) {
        *self = Self {
            to: p.end_alpha,
            ..Self::default()
        };
    }

    /// Advances the pulse for the health fraction seen at `now`.
    pub fn pulse(&mut self, now: i32, health: f32, p: &OverlayParams) {
        if self.old_health > health && p.pulse_start > health {
            self.last_hit = now;
            self.index = 0;
        }
        self.old_health = health;
        if self.duration + self.pulse_time > now || !(p.pulse_start > health || self.hurt) {
            return;
        }
        self.hurt = true;
        self.pulse_time = now;
        self.from = self.to;
        if self.index >= PULSE_MAGS.len() {
            self.hurt = false;
            self.to = p.end_alpha;
            self.duration = p.end_ms;
            self.phase = 0;
            return;
        }
        let mag = PULSE_MAGS[self.index];
        match self.phase {
            0 => {
                self.to = mag.clamp(0.0, 1.0);
                self.duration = p.phase_one_ms;
                self.phase = 1;
            }
            1 => {
                self.to = (p.phase_two_mult * mag).clamp(0.0, 1.0);
                self.duration = p.phase_two_ms;
                self.phase = 2;
            }
            _ => {
                self.to = (p.phase_three_mult * mag).clamp(0.0, 1.0);
                self.duration = p.phase_three_ms;
                self.phase = 0;
                let pulse_len = p.phase_one_ms + p.phase_two_ms + p.phase_three_ms;
                if now >= p.regen_pause_ms + self.last_hit - 3 * pulse_len {
                    self.index += 1;
                }
            }
        }
    }

    /// The overlay's alpha at `now`.
    pub fn alpha(&self, now: i32) -> f32 {
        let t = (now - self.pulse_time).max(0);
        if self.duration <= 0 || t >= self.duration {
            self.to
        } else {
            self.from + (self.to - self.from) * (t as f32 / self.duration as f32)
        }
    }
}

/// How the weapon's magazine is drawn (`ammoCounterClip`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Counter {
    #[default]
    None,
    Magazine,
    ShortMagazine,
    Shotgun,
    Rocket,
    Beltfed,
}

impl Counter {
    /// From the weapon file's value; the alternate-weapon style is resolved to the other weapon's before this.
    pub fn from_raw(v: i32) -> Self {
        match v {
            1 => Self::Magazine,
            2 => Self::ShortMagazine,
            3 => Self::Shotgun,
            4 => Self::Rocket,
            5 => Self::Beltfed,
            _ => Self::None,
        }
    }

    /// The image of one round and its size and step in virtual units, `(material, w, h, step_x, step_y)`.
    pub fn bullet(self) -> Option<(&'static str, f32, f32)> {
        match self {
            Self::None => None,
            Self::Magazine => Some(("ammo_counter_bullet", 4.0, 8.0)),
            Self::ShortMagazine => Some(("ammo_counter_riflebullet", 32.0, 8.0)),
            Self::Shotgun => Some(("ammo_counter_shotgunshell", 16.0, 8.0)),
            Self::Rocket => Some(("ammo_counter_rocket", 64.0, 16.0)),
            Self::Beltfed => Some(("ammo_counter_beltbullet", 8.0, 4.0)),
        }
    }
}

/// The weapon the player has up.
#[derive(Clone, Debug, Default)]
pub struct WeaponFacts {
    pub index: u16,
    /// Localize key of the name and of the fire mode (empty for none).
    pub display: String,
    pub mode: String,
    /// Rounds in the magazine; -1 for a weapon without one.
    pub clip: i32,
    /// Rounds in reserve; -1 for none.
    pub stock: i32,
    pub low_ammo: bool,
    pub low_clip: bool,
    /// The weapon whose rounds the magazine graphic shows (the alternate weapon for an under-barrel attachment).
    pub counter: Counter,
    pub counter_clip: i32,
    pub counter_clip_size: i32,
    pub counter_low: bool,
    /// Reserve text of the ammo-stock display: `None` when the weapon hides it.
    pub stock_shown: Option<i32>,
    pub stock_low: bool,
    pub icon: Option<String>,
    /// 0 square, 1 two to one, 2 four to one.
    pub icon_ratio: i32,
    /// The reload warning applies: some stock left to reload with, and the weapon is empty-ish.
    pub can_reload: bool,
    /// The magazine is exactly empty.
    pub empty: bool,
    pub clip_size: i32,
    /// The low-ammo warning stays quiet (reloading, on a turret).
    pub hide_warning: bool,
    /// The weapon cannot be used while prone.
    pub blocks_prone: bool,
}

/// One of the two grenade slots next to the ammo.
#[derive(Clone, Debug, Default)]
pub struct OffhandFacts {
    pub icon: Option<String>,
    pub ammo: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stance {
    #[default]
    Stand,
    Crouch,
    Prone,
}

/// Another player as the compass knows them.
#[derive(Clone, Debug, Default)]
pub struct Actor {
    pub friendly: bool,
    pub pos: [f32; 2],
    pub yaw: f32,
    /// When the entity was last in the snapshot, the shell's clock.
    pub last_update: i32,
    /// When it last fired (an enemy is shown at that spot for a while), 0 if never.
    pub fire_time: i32,
    pub fire_pos: [f32; 2],
    pub event_seq: u8,
}

/// Everything the owner-draws read.
/// One action slot (`setActionSlot`) for the d-pad pieces.
#[derive(Clone, Debug, Default)]
pub struct SlotFacts {
    /// `sim::pm::action_slot` type.
    pub kind: u8,
    pub usable: bool,
    /// The slot's weapon is the one held.
    pub active: bool,
    /// The weapon's d-pad icon material and its ratio (0 square, 1 two to one, 2 four to one).
    pub icon: Option<String>,
    pub icon_ratio: i32,
    pub ammo: i32,
}

#[derive(Clone, Debug, Default)]
pub struct HudFacts {
    /// A live match is being played; the pieces draw nothing otherwise.
    pub live: bool,
    /// The shell's clock at the last fill.
    pub now: i32,
    pub pm_dead: bool,
    pub spectator: bool,
    /// The server's health of the shown player and the cap.
    pub health: i32,
    pub max_health: i32,
    pub weapon: Option<WeaponFacts>,
    pub frag: Option<OffhandFacts>,
    pub second: Option<OffhandFacts>,
    /// Flash grenade is the second slot's weapon, else smoke.
    pub second_is_flash: bool,
    pub stance: Stance,
    pub prone_blocked_end: i32,
    pub weapon_disabled: bool,
    /// Sprint budget left and the full budget, ms.
    pub sprint_left: i32,
    pub sprint_max: i32,
    pub sprinting: bool,
    pub mantle_hint: bool,
    /// The player is picking a point on the map (an airstrike): the full-screen map shows.
    pub selecting_location: bool,
    pub breath_hint: bool,
    /// The four action slots as the d-pad shows them.
    pub slots: [SlotFacts; 4],
    /// The crosshair hint (`cursorHint`), its raw text from the use-trigger strings, and when it was last on.
    pub cursor_hint: u8,
    pub cursor_hint_text: String,
    pub cursor_hint_time: i32,
    /// "No ammo" style hint: the localize key and when it appeared.
    pub invalid_cmd: Option<(&'static str, i32)>,
    /// The player's own client number, and whether their team is known (a spectator has no friends).
    pub own_client: u16,
    pub team_known: bool,
    pub origin: [f32; 3],
    /// View yaw in degrees.
    pub yaw: f32,
    pub map: Option<MapInfo>,
    pub actors: HashMap<u16, Actor>,
    // Fade starts, the shell's clock; 0 is "not shown yet".
    pub health_fade: i32,
    pub stance_fade: i32,
    pub sprint_fade: i32,
    pub ammo_fade: i32,
    pub offhand_fade: i32,
    pub compass_fade: i32,
    pub weapon_select_time: i32,
    pub overlay: Overlay,
    /// Last drawn health fraction of the bar's red trail, and the last time the bar pulsed.
    pub bar_trail: f32,
    pub bar_trail_delay: i32,
    pub last_bar_pulse: i32,
    pub last_clip_flash: i32,
    // What the fill compares against to notice a change.
    pub prev_health: i32,
    pub prev_stance: Option<Stance>,
    pub prev_weapon: u16,
    pub prev_event_seq: u8,
    pub prev_sprint_left: i32,
    /// Weapon, magazine and reserve.
    pub prev_ammo: (u16, i32, i32),
    pub prev_offhand: (i32, i32),
    pub prev_origin: [f32; 3],
    pub prev_dead: bool,
    /// How often each owner-draw piece drew since the match began, for the run report.
    pub drawn: std::collections::BTreeMap<i32, u32>,
    /// Objective marks the compass and the full map drew since the match began.
    pub objective_marks: u32,
    /// The most objectives the server listed and the most alpha the compass drew them with.
    pub objectives_listed: u32,
    pub objectives_alpha: f32,
}

impl HudFacts {
    /// The facts as the run report shows them.
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "live": self.live,
            "health": self.health,
            "max_health": self.max_health,
            "weapon": self.weapon.as_ref().map(|w| serde_json::json!({
                "name": w.display, "clip": w.clip, "stock": w.stock, "counter": format!("{:?}", w.counter),
                "icon": w.icon,
            })),
            "frag": self.frag.as_ref().map(|o| o.ammo),
            "second": self.second.as_ref().map(|o| o.ammo),
            "stance": format!("{:?}", self.stance),
            "sprint": [self.sprint_left, self.sprint_max],
            "map": self.map.as_ref().map(|m| &m.material),
            "friendlies": self.actors.values().filter(|a| a.friendly).count(),
            "enemies": self.actors.values().filter(|a| !a.friendly).count(),
            "overlay_alpha": self.overlay.alpha(self.now),
            "drawn": self.drawn,
            "objective_marks": self.objective_marks,
            "objectives_listed": self.objectives_listed,
            "objectives_alpha": self.objectives_alpha,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_holds_then_ramps_down_over_its_last_700_ms() {
        assert_eq!(fade_color(5000, 0, 2000, 700), None);
        assert_eq!(fade_color(1000, 1000, 2000, 700), Some(1.0));
        assert_eq!(fade_color(2300, 1000, 2000, 700), Some(1.0));
        let mid = fade_color(2650, 1000, 2000, 700).unwrap();
        assert!((mid - 350.0 / 700.0).abs() < 1e-6, "{mid}");
        assert_eq!(fade_color(3000, 1000, 2000, 700), None);
    }

    #[test]
    fn a_hud_dvar_of_zero_never_fades_and_otherwise_follows_the_last_show() {
        assert_eq!(fade_hud(0.0, 0, 99_999), 1.0);
        assert_eq!(fade_hud(2.0, 0, 100), 0.0);
        assert_eq!(fade_hud(2.0, 100, 1000), 1.0);
        assert_eq!(fade_hud(2.0, 100, 5000), 0.0);
    }

    #[test]
    fn health_is_a_fraction_that_is_zero_for_the_dead_and_the_unknown() {
        assert_eq!(health_fraction(50, 100, false), 0.5);
        assert_eq!(health_fraction(150, 100, false), 1.0);
        assert_eq!(health_fraction(-5, 100, false), 0.0);
        assert_eq!(health_fraction(50, 0, false), 0.0);
        assert_eq!(health_fraction(50, 100, true), 0.0);
    }

    #[test]
    fn low_ammo_is_a_fifth_of_what_can_be_carried() {
        assert!(low_ammo(12, 60));
        assert!(!low_ammo(13, 60));
        assert!(low_ammo(0, 0));
        assert!(low_clip(5, 30, 0.2));
        assert!(!low_clip(7, 30, 0.2));
        assert!(!low_clip(-1, 30, 0.2));
        assert!(!low_clip(0, 0, 0.2));
    }

    fn run(o: &mut Overlay, p: &OverlayParams, from: i32, to: i32, health: f32) -> Vec<(i32, f32)> {
        (from..=to)
            .step_by(10)
            .map(|t| {
                o.pulse(t, health, p);
                (t, o.alpha(t))
            })
            .collect()
    }

    fn peak(s: &[(i32, f32)], from: i32, to: i32) -> f32 {
        s.iter()
            .filter(|&&(t, _)| (from..to).contains(&t))
            .map(|&(_, a)| a)
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_hit_pulses_at_full_strength_until_the_regeneration_pause_then_steps_down_and_ends() {
        let p = OverlayParams::default();
        let mut o = Overlay::default();
        o.reset(&p);
        // Healthy: nothing shows.
        assert!(
            run(&mut o, &p, 100, 400, 1.0)
                .iter()
                .all(|&(_, a)| a == 0.0)
        );
        // Hit down to 30% at t=500 and stay there.
        let s = run(&mut o, &p, 500, 14_000, 0.3);
        assert!(s[0].1 < 0.1, "starts from nothing: {}", s[0].1);
        assert!((peak(&s, 500, 1400) - 1.0).abs() < 0.05);
        // Between pulses it dips but is still lit (phases two and three fade to 0.7 and 0.6 of the peak).
        let dip = s
            .iter()
            .filter(|&&(t, _)| (1000..1400).contains(&t))
            .map(|&(_, a)| a)
            .fold(1.0, f32::min);
        assert!(dip > 0.5, "{dip}");
        // Still at full strength well into the pause, weaker pulses after it, nothing at the end.
        assert!((peak(&s, 3000, 5000) - 1.0).abs() < 0.05);
        assert!(
            (peak(&s, 6500, 7400) - 0.8).abs() < 0.05,
            "{}",
            peak(&s, 6500, 7400)
        );
        assert!(peak(&s, 8400, 9400) < 0.65);
        assert!(
            s.iter()
                .filter(|&&(t, _)| t > 11_000)
                .all(|&(_, a)| a == 0.0)
        );
    }

    #[test]
    fn a_respawn_clears_a_running_pulse() {
        let p = OverlayParams::default();
        let mut o = Overlay::default();
        run(&mut o, &p, 0, 600, 0.3);
        assert!(o.alpha(600) > 0.0);
        o.reset(&p);
        assert_eq!(o.alpha(610), 0.0);
        assert_eq!(o.alpha(10_000), 0.0);
    }
}
