// SPDX-License-Identifier: GPL-3.0-only
//! What the weapon state machine asks the game to do: fire bullets, swing the knife, throw
//! grenades, and the script notifies that follow (`HandleClientEvent`, `FireWeapon`,
//! `FireWeaponMelee`, `G_UseOffHand`).
//!
//! Shots leave the player's view origin along [`Client::fire_angles`]: the view angles with the view's effects
//! (a hit's kick, a scope's sway) and, aimed down sights, the gun's angles (idle, bob, recoil, sway). The view kick
//! of a shot is applied by clients, so the server only advances the gun's recoil spring.

use gsc::Vm;
use sim::Vec3;
use sim::cm::{ENTITYNUM_NONE, ENTITYNUM_WORLD};
use sim::pm::{PlayerState, math};
use sim::weapon::damage::melee_damage;
use sim::weapon::fire::{AimBasis, aim_spread_degrees};
use sim::weapon::{WeaponClass, WeaponEvent, WeaponOut, WeaponParams, WeaponType};

use crate::bullet::{
    BulletHit, BulletParams, MASK_SHOT_CLIENT, SURF_NOIMPACT, ShotTrace, dot, lerp, mad, normalized,
};
use crate::combat::{Damage, MOD_GRENADE, MOD_MELEE, MOD_SUICIDE};
use crate::game::Game;

/// The knife's trace pattern: the centre, then the four corners of the swing box.
const MELEE_OFFSETS: [[f32; 2]; 5] = [
    [0.0, 0.0],
    [1.0, 1.0],
    [-1.0, 1.0],
    [1.0, -1.0],
    [-1.0, -1.0],
];

/// `G_GetPlayerViewOrigin` (`BG_GetPlayerViewOrigin`): the eye with its bob and lean, where shots start.
pub fn view_origin(ps: &PlayerState) -> Vec3 {
    sim::pm::bob::view_origin(ps, ps.command_time)
}

impl Game {
    /// Acts on the events one command's `PM_Weapon` raised.
    pub fn weapon_events(&mut self, vm: &mut Vm, n: u16, out: &WeaponOut) {
        for ev in out.events() {
            match *ev {
                WeaponEvent::Fire { weapon, .. } => self.fire_weapon(vm, n, weapon),
                WeaponEvent::Melee { weapon } => self.fire_melee(vm, n, weapon),
                WeaponEvent::OffhandThrow {
                    weapon, fuse_left, ..
                } => {
                    let Some(aim) = self
                        .client(n)
                        .map(|c| AimBasis::from_angles(view_origin(&c.ps), &c.fire_angles()))
                    else {
                        continue;
                    };
                    self.throw_grenade(vm, n, weapon, &aim, fuse_left);
                }
                WeaponEvent::GrenadeSuicide { weapon } => self.grenade_suicide(vm, n, weapon),
                WeaponEvent::Detonate { .. } => vm.notify_entity(n, "detonate", &[]),
                WeaponEvent::ReloadAmmoAdded { .. } => vm.notify_entity(n, "reload", &[]),
                _ => {}
            }
        }
    }

    /// `FireWeapon`.
    fn fire_weapon(&mut self, vm: &mut Vm, n: u16, weapon: u16) {
        vm.notify_entity(n, "weapon_fired", &[]);
        let time = self.level.time;
        if let Some(c) = self.client_mut(n) {
            c.last_fire_time = time;
        }
        let Some(info) = self.weapons.get(weapon) else {
            return;
        };
        let (weap_type, class) = (info.weap_type, info.weap_class);
        let Some(c) = self.client(n) else { return };
        let aim_angles = c.fire_angles();
        let aim = AimBasis::from_angles(view_origin(&c.ps), &aim_angles);
        let spread = aim_spread_degrees(info, &c.ps, &WeaponParams::default());
        let fuse_left = c.ps.grenade_time_left;
        self.stats.shots += 1;
        if let Some(c) = self.client_mut(n) {
            c.shots += 1;
        }
        let (now, eye, angles) = (self.level.time, aim.origin, aim_angles);
        self.tempev.add(now, crate::tempev::ev::WEAPON_FIRE, |s| {
            s.origin = eye;
            s.angles = angles;
            s.weapon = weapon;
            s.client = n;
        });
        match weap_type {
            WeaponType::Bullet => self.fire_bullets(vm, n, weapon, &aim, spread),
            WeaponType::Grenade => self.throw_grenade(vm, n, weapon, &aim, fuse_left),
            WeaponType::Projectile if class == WeaponClass::Grenade => {
                self.fire_grenade_launcher(vm, n, weapon, &aim)
            }
            WeaponType::Projectile => self.fire_rocket(n, weapon, &aim, spread),
            WeaponType::Binoculars => {}
        }
        self.kick_gun(n, weapon);
    }

    /// `BG_WeaponFireRecoil` for the gun's recoil spring (`G_PlayerEvent`): the shot has left, now the gun kicks.
    fn kick_gun(&mut self, n: u16, weapon: u16) {
        let rolls: [f32; 4] = std::array::from_fn(|_| self.rand() as f32 / u32::MAX as f32);
        let mut rolls = rolls.into_iter().cycle();
        let (Some(info), Some(c)) = (
            self.weapons.get(weapon),
            self.clients.get_mut(usize::from(n)),
        ) else {
            return;
        };
        let r = sim::weapon::fire::fire_recoil(info, &c.ps, || rolls.next().unwrap_or(0.0));
        c.gun.state.kick(r.gun_kick);
    }

    /// `Bullet_Fire` and the damage it deals.
    pub(crate) fn fire_bullets(
        &mut self,
        vm: &mut Vm,
        n: u16,
        weapon: u16,
        aim: &AimBasis,
        spread: f32,
    ) {
        self.ensure_player_anims();
        let penetration = self.penetration_table();
        let params = WeaponParams::default();
        let mut hits: Vec<BulletHit> = Vec::new();
        {
            let (Some(info), Some(c)) = (self.weapons.get(weapon), self.client(n)) else {
                return;
            };
            let p = BulletParams {
                attacker: n,
                info,
                perks: c.ps.perks,
                params: &params,
                penetration: &penetration,
                friendly_fire: self.cvars.int("scr_friendlyfire") != 0,
            };
            self.bullet_hits(&p, aim, spread, self.level.time, &mut hits);
        }
        self.apply_bullet_hits(vm, n, weapon, hits);
    }

    /// The impacts and damage of the bullets `shooter` (a player or a vehicle) fired.
    pub(crate) fn apply_bullet_hits(
        &mut self,
        vm: &mut Vm,
        shooter: u16,
        weapon: u16,
        hits: Vec<BulletHit>,
    ) {
        // The shot was judged against the bodies as they were `psTimeOffset` ago; the killcam starts that much back.
        let time_offset = self.lag_time.map_or(0, |t| self.level.time - t);
        for h in hits {
            if h.exit {
                self.bullet_impact_event(shooter, weapon, &h);
                continue;
            }
            self.check_hit_trigger_damage(vm, shooter, h.start, h.point, h.damage, h.mean);
            self.bullet_impact_event(shooter, weapon, &h);
            if !h.damageable {
                continue;
            }
            if self.is_accurate_hit(h.target, shooter) {
                self.stats.hits += 1;
                if let Some(c) = self.client_mut(shooter) {
                    c.hits += 1;
                }
            }
            let mut d = Damage::new(h.damage, h.mean);
            d.inflictor = Some(shooter);
            d.attacker = Some(shooter);
            d.dir = Some(h.dir);
            d.point = Some(h.point);
            d.flags = h.flags;
            d.weapon = u32::from(weapon);
            d.hitloc = h.hitloc;
            d.time_offset = time_offset;
            self.g_damage(vm, h.target, d);
        }
    }

    /// Tells the clients where a bullet struck and what it struck.
    fn bullet_impact_event(&mut self, shooter: u16, weapon: u16, h: &BulletHit) {
        let hit_flesh = h.target != ENTITYNUM_WORLD && self.is_client(h.target);
        if h.no_impact {
            return;
        }
        let surface = if hit_flesh { 7 } else { h.surface };
        let now = self.level.time;
        self.tempev.add(now, crate::tempev::ev::BULLET_IMPACT, |s| {
            s.origin = h.point;
            s.angles = crate::tempev::dir_to_angles(h.normal);
            s.event_parm = surface;
            s.weapon = weapon;
            s.client = shooter;
            s.eflags = if h.exit {
                crate::tempev::IMPACT_EXIT
            } else {
                0
            };
        });
    }

    /// `Weapon_Throw_Grenade`: the grenade leaves the hand with the weapon's throw speed along
    /// the view, carrying the thrower's own motion along that direction.
    fn throw_grenade(&mut self, vm: &mut Vm, n: u16, weapon: u16, aim: &AimBasis, fuse_left: i32) {
        let Some(info) = self.weapons.get(weapon) else {
            return;
        };
        let mut toss = mad([0.0; 3], info.projectile_speed as f32, aim.forward);
        toss[2] += info.projectile_speed_up as f32;
        if info.projectile_speed_forward != 0 {
            let flat = normalized([aim.forward[0], aim.forward[1], 0.0]);
            let s = info.projectile_speed_forward as f32;
            toss[0] += s * flat[0];
            toss[1] += s * flat[1];
        }
        let velocity = self.client(n).map_or([0.0; 3], |c| c.ps.velocity);
        let dir = normalized(toss);
        let inherit = mad([0.0; 3], dot(velocity, dir), dir);
        if let Some(c) = self.client_mut(n) {
            c.ps.grenade_time_left = 0;
        }
        if let Err(e) =
            self.launch_grenade(vm, n, weapon, aim.origin, toss, inherit, true, fuse_left)
        {
            self.print(format!("G_FireGrenade: {e}\n"));
        }
    }

    /// `Weapon_GrenadeLauncher_Fire`.
    fn fire_grenade_launcher(&mut self, vm: &mut Vm, n: u16, weapon: u16, aim: &AimBasis) {
        let Some(info) = self.weapons.get(weapon) else {
            return;
        };
        let mut toss = mad([0.0; 3], info.projectile_speed as f32, aim.forward);
        toss[2] += info.projectile_speed_up as f32;
        let velocity = self.client(n).map_or([0.0; 3], |c| c.ps.velocity);
        let dir = normalized(toss);
        let inherit = mad([0.0; 3], dot(velocity, dir), dir);
        if let Err(e) = self.launch_grenade(vm, n, weapon, aim.origin, toss, inherit, false, 0) {
            self.print(format!("G_FireGrenade: {e}\n"));
        }
    }

    /// `Weapon_RocketLauncher_Fire`: the rocket is scattered within `spread` degrees and the
    /// launcher kicks its holder back.
    fn fire_rocket(&mut self, n: u16, weapon: u16, aim: &AimBasis, spread: f32) {
        let offset = spread.to_radians().tan() * 16.0;
        let theta = self.random_f32() * 360.0;
        let r = self.random_f32();
        let (s, c) = math::sincos_deg(theta);
        let mut dir = mad([0.0; 3], 16.0, aim.forward);
        dir = mad(dir, r * c * offset, aim.right);
        dir = mad(dir, r * s * offset, aim.up);
        if let Err(e) = self.launch_rocket(n, weapon, aim.origin, normalized(dir)) {
            self.print(format!("G_FireRocket: {e}\n"));
        }
        if let Some(c) = self.client_mut(n) {
            c.ps.velocity = mad(c.ps.velocity, -64.0, aim.forward);
        }
    }

    /// `FireWeaponMelee` and `Weapon_Melee_internal`: the knife swing hits the nearest thing
    /// within reach, trying the centre line and then the corners of the swing box.
    fn fire_melee(&mut self, vm: &mut Vm, n: u16, weapon: u16) {
        self.ensure_player_anims();
        let Some(info) = self.weapons.get(weapon) else {
            return;
        };
        let params = WeaponParams::default();
        let (range, width, height) = (params.melee_range, params.melee_width, params.melee_height);
        let _ = info;
        let Some(c) = self.client(n) else { return };
        let aim = AimBasis::from_angles(view_origin(&c.ps), &c.ps.viewangles);
        let origin = aim.origin;
        let reach = mad(origin, range, aim.forward);
        let at = |o: [f32; 2], from: Vec3| {
            mad(mad(from, width * o[0], aim.right), height * o[1], aim.up)
        };
        let trace = |g: &Game, start: Vec3, end: Vec3| {
            g.shot_trace(start, end, n, ENTITYNUM_NONE, MASK_SHOT_CLIENT, false)
        };
        let count = if width > 0.0 || height > 0.0 { 5 } else { 1 };
        let mut found: Option<(ShotTrace, Vec3)> = None;
        let mut centre_line = None;
        for (i, o) in MELEE_OFFSETS[..count].iter().enumerate() {
            let end = at(*o, reach);
            let t = trace(self, origin, end);
            if i == 0 {
                centre_line = Some(lerp(origin, end, t.fraction));
            }
            if t.surface_flags & SURF_NOIMPACT == 0 && t.fraction != 1.0 {
                found = Some((t, lerp(origin, end, t.fraction)));
                break;
            }
        }
        if found.is_none() {
            for i in [1usize, 3] {
                if i + 1 >= count {
                    break;
                }
                let (start, end) = (at(MELEE_OFFSETS[i], reach), at(MELEE_OFFSETS[i + 1], reach));
                let t = trace(self, start, end);
                if t.surface_flags & SURF_NOIMPACT == 0 && !t.start_solid && t.fraction != 1.0 {
                    found = Some((t, lerp(start, end, t.fraction)));
                    break;
                }
            }
        }
        // `G_CheckHitTriggerDamage` along the centre line, hit or not.
        let rand = self.rand();
        let info = self.weapons.get(weapon).expect("weapon looked up above");
        let damage = melee_damage(info, rand);
        if let Some(end) = centre_line {
            self.check_hit_trigger_damage(vm, n, origin, end, damage, MOD_MELEE);
        }
        let Some((t, point)) = found else { return };
        if t.hit == ENTITYNUM_WORLD || !self.ent(t.hit).is_some_and(|e| e.takedamage) {
            return;
        }
        let mut d = Damage::new(damage, MOD_MELEE);
        d.inflictor = Some(n);
        d.attacker = Some(n);
        d.dir = Some(aim.forward);
        d.point = Some(point);
        d.weapon = u32::from(weapon);
        d.hitloc = t.hitloc;
        self.g_damage(vm, t.hit, d);
    }

    /// `EV_GRENADE_SUICIDE`: a grenade cooked off in the hand kills its holder, credited to
    /// whoever the grenade was taken from when it was thrown back.
    fn grenade_suicide(&mut self, vm: &mut Vm, n: u16, weapon: u16) {
        let Some(e) = self.ent(n) else { return };
        if e.flags & 3 != 0 {
            return;
        }
        let owner = self
            .client(n)
            .map(|c| c.ps.throw_back_grenade_owner)
            .filter(|o| *o != ENTITYNUM_NONE);
        let attacker = owner.unwrap_or(n);
        let mut d = Damage::new(
            100_000,
            if owner.is_some() {
                MOD_GRENADE
            } else {
                MOD_SUICIDE
            },
        );
        d.inflictor = Some(attacker);
        d.attacker = Some(attacker);
        d.weapon = u32::from(weapon);
        if let Some(e) = self.ent_mut(n) {
            e.health = 0;
        }
        self.player_die(vm, n, &d, 100_000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shots_start_from_the_bobbing_eye_not_the_still_one() {
        let still = PlayerState {
            origin: [10.0, 20.0, 30.0],
            view_height_current: 60.0,
            view_height_target: sim::pm::VIEW_STAND,
            ..PlayerState::default()
        };
        assert_eq!(view_origin(&still), [10.0, 20.0, 90.0]);
        let running = PlayerState {
            velocity: [190.0, 0.0, 0.0],
            bob_cycle: 40,
            ..still
        };
        assert_ne!(view_origin(&running)[2], 90.0);
    }
}
