// SPDX-License-Identifier: GPL-3.0-only
// Derived from KisakCOD (https://github.com/KisakJarvis/KisakCOD), game_mp/g_misc_mp.cpp and
// game_mp/player_use_mp.cpp: copyright the KisakCOD contributors; the original is Call of Duty 4 by
// Infinity Ward / Activision.
//! Mountable turrets (`misc_mg42`, `misc_turret`; `SP_turret`, `turret_use`, `turret_think_client`): a player in
//! reach and behind the gun mounts it with +activate, the view is held inside the gun's arcs, the attack button fires
//! the turret's weapon from the muzzle, and +activate again, death, a respawn or a disconnect lets the gunner go.
//!
//! The gunner's view is held in the arcs by movement (`view_clamp`'s base and range in the player state). The gun
//! swings with the view (`turret_clientaim`'s `gunAngles`, which the clients turn `tag_aim`, `tag_aim_animated` and
//! `tag_flash` by, `turret_controller`), and shots leave from the swung `tag_flash`. Where this differs from the
//! original: the gunner is placed from the swung `tag_player` rather than blended through the gunner animation's
//! offsets, and only bullet weapons are fired (the stock turrets are all `saw_bipod_*`).

use crate::bullet::normalized;
use crate::fire::view_origin;
use crate::game::{Ent, Game, SpawnVars, spawn_var};
use crate::tags;
use gsc::vm::Vm;
use sim::Vec3;
use sim::cm::{Collide, ENTITYNUM_NONE};
use sim::contents;
use sim::pm::math::angle_delta;
use sim::pm::{PmType, VIEW_CROUCH, VIEW_PRONE, VIEW_STAND, button, ef, ev, pmf};
use sim::skel::{Controllers, Pose, Rig, RigModel};
use sim::weapon::fire::AimBasis;
use sim::weapon::{FireType, WeaponClass, WeaponType};

/// The use box of a turret (`SP_turret`).
const MINS: Vec3 = [-32.0, -32.0, 0.0];
const MAXS: Vec3 = [32.0, 32.0, 56.0];

/// One turret entity's state (`turretInfo_s`).
#[derive(Debug, Clone)]
pub struct Turret {
    pub weapon: u16,
    /// Limits of the gun's swing from the way the turret faces, `[pitch, yaw]`: positive pitch looks down, positive
    /// yaw to the left, so the minimums are never above 0 and the maximums never below.
    pub arc_min: [f32; 2],
    pub arc_max: [f32; 2],
    /// The gunner's stance: 0 standing, 1 crouched, 2 prone.
    pub stance: i32,
    pub player_spread: f32,
    /// `Game::hint_string_index` slots of the weapon's use and drop hints.
    pub use_hint: Option<usize>,
    pub drop_hint: Option<usize>,
    /// The player on it.
    pub gunner: Option<u16>,
    /// Where the gunner stood when mounting; they are put back there.
    pub user_origin: Vec3,
    /// The stance the gunner had (0 standing, 1 crouched, 2 prone), restored on leaving.
    pub prev_stance: Option<u8>,
    /// Milliseconds until the next shot may leave, and whether the attack button was down for the last.
    pub fire_time: i32,
    pub trigger_down: bool,
    /// How far the gun has swung from the way the turret faces (`gunAngles`): the gunner's view less the turret's
    /// angles, held in the arcs, `[pitch, yaw, 0]`. Nothing without a gunner.
    pub gun_angles: [f32; 3],
}

/// The view-clamp base and range (`viewAngleClampBase`, `viewAngleClampRange`) that keep a gunner's view in the arcs
/// of a turret facing `base` (`[pitch, yaw]`): the middle of the arc and its half-width.
pub fn view_clamp(base: [f32; 2], arc_min: [f32; 2], arc_max: [f32; 2]) -> ([f32; 2], [f32; 2]) {
    let range: [f32; 2] =
        std::array::from_fn(|i| sim::pm::math::angle_delta(arc_max[i], arc_min[i]) * 0.5);
    let centre = std::array::from_fn(|i| {
        sim::pm::math::angle_normalize_360(base[i] + arc_max[i] - range[i])
    });
    (centre, range)
}

/// `turret_behind`: whether `player` stands where the gun points away from, within half the yaw arc of the line
/// through the middle of the arc.
pub fn behind(
    turret_yaw: f32,
    arc_min_yaw: f32,
    arc_max_yaw: f32,
    turret: Vec3,
    player: Vec3,
) -> bool {
    let span = (arc_min_yaw.abs() + arc_max_yaw.abs()) * 0.5;
    let centre = sim::pm::math::angle_delta(turret_yaw + arc_min_yaw + span, 0.0);
    let (s, c) = sim::pm::math::sincos_deg(centre);
    let to_gun = normalized([turret[0] - player[0], turret[1] - player[1], 0.0]);
    let dot = (c * to_gun[0] + s * to_gun[1]).clamp(-1.0, 1.0);
    span >= dot.acos().to_degrees()
}

impl Game {
    /// `SP_turret` / `G_SpawnTurret`: makes the entity a turret of its `weaponinfo` weapon, with arcs and spread the
    /// map may override.
    pub fn init_turret(&mut self, num: u16, vars: &SpawnVars) -> Result<(), String> {
        let name = spawn_var(vars, "weaponinfo").ok_or("no weaponinfo specified for turret")?;
        let weapon = self.weapons.index(name);
        let Some(info) = self.weapons.get(weapon).filter(|_| weapon != 0) else {
            return Err(format!("bad weaponinfo '{name}' specified for turret"));
        };
        if info.weap_class != WeaponClass::Turret {
            return Err(format!(
                "G_SpawnTurret: weapon '{name}' isn't a turret. This usually indicates that the weapon failed to load."
            ));
        }
        if info.weap_type != WeaponType::Bullet {
            return Err(format!(
                "G_SpawnTurret: turret weapon '{name}' is not a bullet weapon"
            ));
        }
        let def = info.turret.clone();
        let float =
            |k: &str, default: f32| spawn_var(vars, k).map_or(default, crate::cvar::parse_float);
        let (use_hint, drop_hint) = (
            self.turret_hint(&def.use_hint_string)?,
            self.turret_hint(&def.drop_hint_string)?,
        );
        let turret = Turret {
            weapon,
            arc_min: [
                (-float("toparc", def.top_arc)).min(0.0),
                (-float("rightarc", def.right_arc)).min(0.0),
            ],
            arc_max: [
                float("bottomarc", def.bottom_arc).max(0.0),
                float("leftarc", def.left_arc).max(0.0),
            ],
            stance: def.stance,
            player_spread: float("playerSpread", def.player_spread).max(0.0),
            use_hint,
            drop_hint,
            gunner: None,
            user_origin: [0.0; 3],
            prev_stance: None,
            fire_time: 0,
            trigger_down: false,
            gun_angles: [0.0; 3],
        };
        if let Some(e) = self.ent_mut(num) {
            e.contents = contents::USE;
            e.mins = MINS;
            e.maxs = MAXS;
            e.turret = Some(Box::new(turret));
        }
        self.relink(num);
        Ok(())
    }

    fn turret_hint(&mut self, text: &str) -> Result<Option<usize>, String> {
        if text.is_empty() {
            Ok(None)
        } else {
            self.hint_string_index(text).map(Some)
        }
    }

    /// `G_IsTurretUsable`: free, the player behind it, not holding a grenade, on the ground.
    pub fn turret_usable(&self, t: u16, n: u16) -> bool {
        let (Some(e), Some(c)) = (self.ent(t), self.client(n)) else {
            return false;
        };
        let Some(tu) = e.turret.as_deref() else {
            return false;
        };
        tu.gunner.is_none()
            && c.turret.is_none()
            && behind(
                e.angles[1],
                tu.arc_min[1],
                tu.arc_max[1],
                e.origin,
                c.ps.origin,
            )
            && c.ps.grenade_time_left == 0
            && c.ps.ground_entity_num != ENTITYNUM_NONE
    }

    /// `turret_use`: player `n` takes the gun.
    pub fn turret_use(&mut self, t: u16, n: u16) {
        let Some(e) = self.ent(t) else { return };
        let (angles, Some(tu)) = (e.angles, e.turret.as_deref()) else {
            return;
        };
        let (arc_min, arc_max, stance) = (tu.arc_min, tu.arc_max, tu.stance);
        let Some(c) = self.client_mut(n) else { return };
        let ps = &mut c.ps;
        let user_origin = ps.origin;
        let prev = if ps.pm_flags & pmf::PRONE != 0 {
            2
        } else {
            u8::from(ps.pm_flags & pmf::DUCKED != 0)
        };
        ps.e_flags = match stance {
            2 => (ps.e_flags | ef::TURRET_PRONE) & !ef::TURRET_CROUCH,
            1 => (ps.e_flags | ef::TURRET_CROUCH) & !ef::TURRET_PRONE,
            _ => ps.e_flags | ef::TURRET_ACTIVE,
        };
        let (centre, range) = view_clamp([angles[0], angles[1]], arc_min, arc_max);
        ps.view_angle_clamp_base = centre;
        ps.view_angle_clamp_range = range;
        c.turret = Some(t);
        c.turret_leave = false;
        if let Some(tu) = self.ent_mut(t).and_then(|e| e.turret.as_deref_mut()) {
            tu.gunner = Some(n);
            tu.user_origin = user_origin;
            tu.prev_stance = Some(prev);
            tu.fire_time = 0;
            tu.trigger_down = false;
        }
    }

    /// `G_ClientStopUsingTurret`: lets the gunner go. A player still playing gets their stance back and is put where
    /// they mounted from; one who died, left or respawned is only released.
    pub fn stop_using_turret(&mut self, t: u16, alive: bool) {
        let Some(tu) = self.ent_mut(t).and_then(|e| e.turret.as_deref_mut()) else {
            return;
        };
        let Some(n) = tu.gunner.take() else { return };
        let (prev, user_origin) = (tu.prev_stance.take(), tu.user_origin);
        tu.trigger_down = false;
        tu.gun_angles = [0.0; 3];
        if let Some(c) = self.client_mut(n) {
            c.turret = None;
            c.turret_leave = false;
            let ps = &mut c.ps;
            ps.e_flags &= !ef::TURRET_ACTIVE;
            if alive && let Some(prev) = prev {
                let (flags, target, event) = match prev {
                    2 => (
                        (ps.pm_flags & !pmf::DUCKED) | pmf::PRONE,
                        VIEW_PRONE,
                        ev::STANCE_FORCE_PRONE,
                    ),
                    1 => (
                        (ps.pm_flags & !pmf::PRONE) | pmf::DUCKED,
                        VIEW_CROUCH,
                        ev::STANCE_FORCE_CROUCH,
                    ),
                    _ => (
                        ps.pm_flags & !(pmf::PRONE | pmf::DUCKED),
                        VIEW_STAND,
                        ev::STANCE_FORCE_STAND,
                    ),
                };
                ps.pm_flags = flags;
                ps.view_height_target = target;
                ps.add_event(event, 0);
            }
        }
        if alive {
            self.teleport(n, user_origin);
        }
    }

    /// A player leaves the game or respawns: whatever turret they hold lets go without moving them.
    pub fn release_turret(&mut self, n: u16) {
        if let Some(t) = self.client(n).and_then(|c| c.turret) {
            self.stop_using_turret(t, false);
        }
    }

    /// `G_PlayerTurretPositionAndBlend` and `turret_think_client`, once a frame for player `n` after their movement:
    /// lets a gunner who died, went down or pressed use go, or else puts them at the mount and fires when they hold
    /// the attack button.
    pub fn turret_think_client(&mut self, vm: &mut Vm, n: u16) {
        let Some(t) = self.client(n).and_then(|c| c.turret) else {
            return;
        };
        let (Some(c), Some(e)) = (self.client(n), self.ent(t)) else {
            self.release_turret(n);
            return;
        };
        let Some(tu) = e.turret.as_deref() else {
            self.release_turret(n);
            return;
        };
        if tu.gunner != Some(n) {
            self.release_turret(n);
            return;
        }
        let playing = c.session == crate::client::Session::Playing
            && c.connected()
            && c.ps.pm_type != PmType::LastStand
            && self.ent(n).is_some_and(|p| p.health > 0);
        if c.turret_leave || !playing {
            self.stop_using_turret(t, playing);
            return;
        }
        self.aim_gun(t, n);
        self.place_gunner(t, n);
        self.turret_shoot(vm, t, n);
    }

    /// `turret_clientaim`: swings the gun to the gunner's view, within the arcs.
    fn aim_gun(&mut self, t: u16, n: u16) {
        let (Some(c), Some(e)) = (self.client(n), self.ent(t)) else {
            return;
        };
        let (view, angles) = (c.ps.viewangles, e.angles);
        if let Some(tu) = self.ent_mut(t).and_then(|e| e.turret.as_deref_mut()) {
            let swing =
                |i: usize| angle_delta(view[i], angles[i]).clamp(tu.arc_min[i], tu.arc_max[i]);
            tu.gun_angles = [swing(0), swing(1), 0.0];
        }
    }

    /// `G_DObjGetWorldTagMatrix` with the gun swung (`turret_controller` has run): the turret's own tag, the one
    /// the gunner and the shots follow.
    pub fn swung_tag(&self, t: u16, tag: &str) -> Option<tags::Mat43> {
        let e = self.ent(t)?;
        let gun = e.turret.as_deref()?.gun_angles;
        let model = self.content.model(&e.model)?;
        let names: Vec<&str> = self
            .content
            .model_bone_names(&e.model)?
            .iter()
            .map(|n| &**n)
            .collect();
        let rig = Rig::new(&[RigModel {
            model: model.clone(),
            bone_names: &names,
            attach: None,
        }])
        .ok()?;
        let mut pose = Pose::default();
        let ctl = Controllers {
            turret: Some(gun),
            ..Controllers::NONE
        };
        rig.pose(&[], &ctl, &mut pose);
        let b = pose.bones().get(rig.bone_index(tag)?)?;
        let a = sim::skel::quat::axes(&b.quat);
        Some(tags::mul43(
            &[a[0], a[1], a[2], b.trans],
            &tags::frame(e.origin, e.angles),
        ))
    }

    /// Puts the gunner behind the gun: their eye at the swung turret's `tag_player`, the feet on the floor under it.
    fn place_gunner(&mut self, t: u16, n: u16) {
        let Some(tag) = self.swung_tag(t, "tag_player") else {
            return;
        };
        let (Some(c), Some(w)) = (self.client(n), self.world.as_ref()) else {
            return;
        };
        let eye = tag[3];
        let mut origin = [eye[0], eye[1], eye[2] - c.ps.view_height_current];
        let start = [origin[0], origin[1], origin[2] + c.ps.view_height_current];
        let end = [start[0], start[1], start[2] - 60.0];
        let tr = w.trace(start, end, [0.0; 3], [0.0; 3], n, contents::MASK_DEADSOLID);
        if tr.fraction < 1.0 {
            origin[2] = start[2] + (end[2] - start[2]) * tr.fraction;
        }
        if let Some(c) = self.client_mut(n) {
            c.ps.origin = origin;
            c.ps.velocity = [0.0; 3];
        }
        if let Some(p) = self.ent_mut(n) {
            p.origin = origin;
        }
        self.relink(n);
    }

    /// `turret_track`'s firing half: a shot each `fireTime` while the attack button is down (once per press for a
    /// single-shot weapon), from the muzzle along the gunner's view (`Fire_Lead`, `Turret_FillWeaponParms`).
    fn turret_shoot(&mut self, vm: &mut Vm, t: u16, n: u16) {
        let step = self.level.frametime;
        let (Some(c), Some(e)) = (self.client(n), self.ent(t)) else {
            return;
        };
        let Some(tu) = e.turret.as_deref() else {
            return;
        };
        let Some(info) = self.weapons.get(tu.weapon) else {
            return;
        };
        let (fire_time, single) = (info.fire_time, info.fire_type == FireType::SingleShot);
        let (weapon, spread) = (tu.weapon, tu.player_spread);
        let wants = c.buttons & button::ATTACK != 0 && c.ps.pm_flags & pmf::FROZEN == 0;
        let (mut left, mut down) = (tu.fire_time - step, tu.trigger_down);
        let mut shoot = false;
        if left <= 0 {
            left = 0;
            if !wants {
                down = false;
            } else if !single || !down {
                down = true;
                left = fire_time;
                shoot = true;
            }
        }
        let angles = c.ps.viewangles;
        let eye = view_origin(&c.ps);
        if let Some(tu) = self.ent_mut(t).and_then(|e| e.turret.as_deref_mut()) {
            (tu.fire_time, tu.trigger_down) = (left, down);
        }
        if !shoot {
            return;
        }
        let Some(flash) = self.swung_tag(t, "tag_flash").map(|m| m[3]) else {
            return;
        };
        let dist = crate::bullet::length(crate::bullet::sub(flash, eye));
        let aim = AimBasis::from_angles(eye, &angles);
        let muzzle = crate::bullet::mad(eye, dist, aim.forward);
        let aim = AimBasis {
            origin: muzzle,
            ..aim
        };
        self.stats.shots += 1;
        if let Some(c) = self.client_mut(n) {
            c.shots += 1;
        }
        let now = self.level.time;
        self.tempev.add(now, crate::tempev::ev::WEAPON_FIRE, |s| {
            s.origin = muzzle;
            s.angles = angles;
            s.weapon = weapon;
            s.client = n;
        });
        self.fire_bullets(vm, n, weapon, &aim, spread);
    }

    /// `Player_SetTurretDropHint` (as the cursor hint a gunner sees): the weapon's drop hint, with no entity.
    pub fn turret_drop_hint(&self, t: u16) -> (u8, i8, u16) {
        let Some(tu) = self.ent(t).and_then(|e| e.turret.as_deref()) else {
            return (0, -1, ENTITYNUM_NONE);
        };
        match tu.drop_hint {
            Some(slot) => (
                (tu.weapon as u8).saturating_add(sim::weapon::pickup::WEAPON_HINT_OFFSET),
                slot as i8,
                ENTITYNUM_NONE,
            ),
            None => (0, -1, ENTITYNUM_NONE),
        }
    }

    /// Every turret entity's number with its state.
    pub fn turrets(&self) -> impl Iterator<Item = (u16, &Ent, &Turret)> {
        self.in_use()
            .filter_map(|(n, e)| e.turret.as_deref().map(|t| (n, e, t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `saw_bipod_stand_mp` arcs: 45 degrees each way in yaw, 15 up and 15 down.
    const MIN: [f32; 2] = [-15.0, -45.0];
    const MAX: [f32; 2] = [15.0, 45.0];

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
    }

    #[test]
    fn the_view_clamp_is_the_middle_of_the_arc_and_its_half_width() {
        let (base, range) = view_clamp([0.0, 90.0], MIN, MAX);
        assert!(close(range, [15.0, 45.0]) && close(base, [0.0, 90.0]));
        // An arc that is not centred on the turret's line is centred on its own middle.
        let (base, range) = view_clamp([0.0, 90.0], [-10.0, -15.0], [20.0, 45.0]);
        assert!(close(range, [15.0, 30.0]) && close(base, [5.0, 105.0]));
        // The base wraps into 0..360.
        let (base, _) = view_clamp([0.0, 350.0], MIN, [15.0, 60.0]);
        assert!((base[1] - 357.5).abs() < 1e-3, "{base:?}");
    }

    #[test]
    fn a_turret_is_used_from_the_side_the_gun_points_away_from() {
        let gun = [100.0, 0.0, 0.0];
        // Facing +X: behind it is -X, in front is +X.
        assert!(behind(0.0, -45.0, 45.0, gun, [50.0, 0.0, 0.0]));
        assert!(behind(0.0, -45.0, 45.0, gun, [50.0, 40.0, 0.0]));
        assert!(!behind(0.0, -45.0, 45.0, gun, [150.0, 0.0, 0.0]));
        assert!(!behind(0.0, -45.0, 45.0, gun, [100.0, 60.0, 0.0]));
        // Height is ignored.
        assert!(behind(0.0, -45.0, 45.0, gun, [50.0, 0.0, 80.0]));
        // Yaw 180 turns it around, and a narrower arc narrows the cone.
        assert!(behind(180.0, -45.0, 45.0, gun, [150.0, 0.0, 0.0]));
        assert!(!behind(0.0, -15.0, 15.0, gun, [50.0, 40.0, 0.0]));
    }
}
