// SPDX-License-Identifier: GPL-3.0-or-later
//! What the weapon state machine asks the game to do: fire bullets, swing the knife, throw
//! grenades, and the script notifies that follow (`HandleClientEvent`, `FireWeapon`,
//! `FireWeaponMelee`, `G_UseOffHand`).
//!
//! Shots leave the player's view origin along the view angles; the original aims along the gun
//! angles, which only differ by the weapon sway of aimed-down-sight weapons (not modelled). The
//! view kick of a shot is applied by clients, so the server does not need it.

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

/// `G_GetPlayerViewOrigin`: the eye, shifted sideways by a lean.
pub fn view_origin(ps: &PlayerState) -> Vec3 {
    let mut o = [
        ps.origin[0],
        ps.origin[1],
        ps.origin[2] + ps.view_height_current,
    ];
    math::add_lean_to_position(&mut o, ps.viewangles[1], ps.leanf, 16.0, 20.0);
    o
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
                        .map(|c| AimBasis::from_angles(view_origin(&c.ps), &c.ps.viewangles))
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
        let Some(info) = self.weapons.get(weapon) else {
            return;
        };
        let (weap_type, class) = (info.weap_type, info.weap_class);
        let Some(c) = self.client(n) else { return };
        let aim = AimBasis::from_angles(view_origin(&c.ps), &c.ps.viewangles);
        let spread = aim_spread_degrees(info, &c.ps, &WeaponParams::default());
        let fuse_left = c.ps.grenade_time_left;
        self.stats.shots += 1;
        match weap_type {
            WeaponType::Bullet => self.fire_bullets(vm, n, weapon, &aim, spread),
            WeaponType::Grenade => self.throw_grenade(vm, n, weapon, &aim, fuse_left),
            WeaponType::Projectile if class == WeaponClass::Grenade => {
                self.fire_grenade_launcher(vm, n, weapon, &aim)
            }
            WeaponType::Projectile => self.fire_rocket(n, weapon, &aim, spread),
            WeaponType::Binoculars => {}
        }
    }

    /// `Bullet_Fire` and the damage it deals.
    fn fire_bullets(&mut self, vm: &mut Vm, n: u16, weapon: u16, aim: &AimBasis, spread: f32) {
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
        for h in hits {
            if self.is_accurate_hit(h.target, n) {
                self.stats.hits += 1;
            }
            let mut d = Damage::new(h.damage, h.mean);
            d.inflictor = Some(n);
            d.attacker = Some(n);
            d.dir = Some(h.dir);
            d.point = Some(h.point);
            d.flags = h.flags;
            d.weapon = u32::from(weapon);
            d.hitloc = h.hitloc;
            self.g_damage(vm, h.target, d);
        }
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
        for o in &MELEE_OFFSETS[..count] {
            let end = at(*o, reach);
            let t = trace(self, origin, end);
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
        let Some((t, point)) = found else { return };
        if t.hit == ENTITYNUM_WORLD || !self.ent(t.hit).is_some_and(|e| e.takedamage) {
            return;
        }
        let rand = self.rand();
        let info = self.weapons.get(weapon).expect("weapon looked up above");
        let mut d = Damage::new(melee_damage(info, rand), MOD_MELEE);
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
    fn the_eye_is_above_the_origin_and_shifts_with_a_lean() {
        let mut ps = PlayerState {
            origin: [10.0, 20.0, 30.0],
            view_height_current: 60.0,
            ..PlayerState::default()
        };
        assert_eq!(view_origin(&ps), [10.0, 20.0, 90.0]);
        ps.leanf = 1.0;
        let o = view_origin(&ps);
        // Facing +x, leaning right moves the eye along -y by the lean distance.
        assert!(o[0] == 10.0 && (o[1] - 20.0).abs() > 10.0, "{o:?}");
    }
}
