// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (game_mp/g_trigger_mp.cpp, game_mp/g_active_mp.cpp, game_mp/g_main_mp.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! Triggers: what standing in a volume does (`G_TouchTriggers`), the `trigger` notifies it queues
//! (`G_Trigger`), damage volumes (`trigger_hurt`) and the volumes shots and blasts set off
//! (`trigger_damage`).
//!
//! Touching happens once per server frame for every player, not per usercmd. A touch does not
//! notify at once: it is queued, and the frame's trigger pass delivers at most one `trigger`
//! per entity, runs the scripts, and goes round again for the touches that were held back.

use gsc::{Value, Vm};
use sim::Vec3;
use sim::contents;
use sim::pm::PmType;

use crate::combat::{
    Damage, MOD_EXPLOSIVE, MOD_GRENADE, MOD_GRENADE_SPLASH, MOD_HEAD_SHOT, MOD_MELEE,
    MOD_PISTOL_BULLET, MOD_PROJECTILE_SPLASH, MOD_RIFLE_BULLET, MOD_TRIGGER_HURT, MOD_UNKNOWN,
    MODS,
};
use crate::game::{Ent, Game, TRIGGER_HURT_CONTENTS};
use crate::missile::FL_GRENADE_TOUCH_DAMAGE;

/// `pendingTriggerList` holds this many touches; later ones notify at once.
const MAX_PENDING: usize = 256;

/// `trigger_info_t`: a touch waiting for the trigger pass. The use counts tell whether either
/// entity was freed since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingTrigger {
    pub ent: u16,
    pub other: u16,
    ent_use_count: u32,
    other_use_count: u32,
}

/// `InitTriggerWait` flags: `wait <= 0` frees the entity after the first notify.
pub const SPAWN_ONCE: i32 = 0x10;
pub const SPAWN_DAMAGE_ONCE: i32 = 0x200;
/// `trigger_hurt` spawnflags.
const HURT_START_OFF: i32 = 1;
const HURT_SLOW: i32 = 0x10;
const HURT_ONCE: i32 = 0x20;

/// `trigger_damage` spawnflags that make it ignore some kinds of damage.
const TD_NO_PISTOL: i32 = 1;
const TD_NO_RIFLE: i32 = 2;
const TD_NO_PROJECTILE: i32 = 4;
const TD_NO_EXPLOSIVE: i32 = 8;
const TD_NO_SPLASH: i32 = 0x10;
const TD_NO_MELEE: i32 = 0x20;
const TD_ONLY_DIRECT: i32 = 0x100;
/// `trigger_damage` health: reset to this after every hit.
const TD_HEALTH: i32 = 32000;

/// What a trigger or item does when a player's box overlaps it (the `touch` of its entity handler).
enum Touch {
    Item,
    Multi,
    Hurt,
}

fn touch_kind(e: &Ent) -> Option<Touch> {
    if e.item.is_some() {
        return Some(Touch::Item);
    }
    match &*e.classname {
        "trigger_multiple" | "trigger_once" | "trigger_radius" | "trigger_disk" => {
            Some(Touch::Multi)
        }
        "trigger_hurt" if !e.x.hurt_off => Some(Touch::Hurt),
        _ => None,
    }
}

/// `Respond_trigger_damage`: whether this kind of damage sets the volume off. `mean` is `-1` for
/// `useby`.
fn trigger_damage_responds(spawnflags: i32, mean: i32) -> bool {
    let (grenade, projectile_splash) = (i32::from(MOD_GRENADE), i32::from(MOD_PROJECTILE_SPLASH));
    let is = |m: u8| mean == i32::from(m);
    if spawnflags & TD_NO_PISTOL != 0 && is(MOD_PISTOL_BULLET)
        || spawnflags & TD_NO_RIFLE != 0 && is(MOD_RIFLE_BULLET)
        || spawnflags & TD_NO_PROJECTILE != 0 && (grenade..=projectile_splash).contains(&mean)
        || spawnflags & TD_NO_EXPLOSIVE != 0
            && mean >= grenade
            && (mean <= projectile_splash || is(MOD_EXPLOSIVE))
        || spawnflags & TD_NO_SPLASH != 0 && (is(MOD_GRENADE_SPLASH) || is(MOD_PROJECTILE_SPLASH))
        || spawnflags & TD_NO_MELEE != 0 && is(MOD_MELEE)
    {
        return false;
    }
    spawnflags & TD_ONLY_DIRECT == 0
        || !is(MOD_UNKNOWN)
            && (mean <= i32::from(MOD_HEAD_SHOT) || mean > i32::from(MOD_TRIGGER_HURT))
}

impl Game {
    /// `G_Trigger`: queues a `trigger` notify for `t` with `other`, or sends it now when the queue is
    /// full.
    pub fn g_trigger(&mut self, vm: &mut Vm, t: u16, other: u16) {
        let (Some(te), Some(oe)) = (self.ent(t), self.ent(other)) else {
            return;
        };
        if self.level.pending_triggers.len() >= MAX_PENDING {
            let who = self.entity_value(vm, other);
            vm.notify_entity(t, "trigger", &[who]);
            return;
        }
        let p = PendingTrigger {
            ent: t,
            other,
            ent_use_count: te.use_count,
            other_use_count: oe.use_count,
        };
        self.level.pending_triggers.push(p);
    }

    /// One round of `G_RunFrame`'s trigger checks over `list`: every touch whose entities are
    /// still the same is notified, except a second one for an entity already notified this round,
    /// which stays for the next. Returns whether any stayed.
    pub fn deliver_triggers(&mut self, vm: &mut Vm, list: &mut Vec<PendingTrigger>) -> bool {
        let mut notified: Vec<u16> = Vec::new();
        let mut more = false;
        let mut kept = Vec::new();
        for p in std::mem::take(list) {
            let fresh =
                |g: &Game, n: u16, count: u32| g.ent(n).is_some_and(|e| e.use_count == count);
            if !fresh(self, p.ent, p.ent_use_count) || !fresh(self, p.other, p.other_use_count) {
                continue;
            }
            if notified.contains(&p.ent) {
                more = true;
                kept.push(p);
                continue;
            }
            notified.push(p.ent);
            let who = self.entity_value(vm, p.other);
            vm.notify_entity(p.ent, "trigger", &[who]);
        }
        *list = kept;
        more
    }

    /// `G_FreeEntityDelay`: the entity goes at the end of this frame, after the scripts that wait on
    /// it ran.
    pub fn free_entity_delayed(&mut self, n: u16) {
        let now = self.level.time;
        if let Some(e) = self.ent_mut(n) {
            e.free_at = Some(e.free_at.map_or(now, |t| t.min(now)));
        }
    }

    /// `G_TouchTriggers` for every player that clips.
    pub fn touch_all_triggers(&mut self, vm: &mut Vm) {
        for n in 0..self.clients.len() as u16 {
            if self.client(n).is_some_and(|c| !c.noclip) && self.ent(n).is_some() {
                self.touch_triggers(vm, n);
            }
        }
    }

    /// `G_TouchTriggers`: the triggers and items the player's box overlaps hear `touch` and act.
    pub fn touch_triggers(&mut self, vm: &mut Vm, n: u16) {
        let Some(c) = self.client(n) else { return };
        if c.ps.pm_type > PmType::NormalLinked {
            return;
        }
        let Some(e) = self.ent(n) else { return };
        let range = 20.0;
        let (lo, hi) = (
            [0, 1, 2].map(|i| e.origin[i] + e.mins[i] - range),
            [0, 1, 2].map(|i| e.origin[i] + e.maxs[i] + range),
        );
        let (mut bmin, mut bmax) = (
            [0, 1, 2].map(|i| c.ps.origin[i] + e.mins[i]),
            [0, 1, 2].map(|i| c.ps.origin[i] + e.maxs[i]),
        );
        crate::script::expand_bounds_to_width(&mut bmin, &mut bmax);
        let player_origin = c.ps.origin;
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let mut list = Vec::new();
        world.area_entities(lo, hi, TRIGGER_HURT_CONTENTS, |t| {
            list.push(t);
            true
        });
        for t in list {
            let Some(te) = self.ent(t) else { continue };
            let Some(kind) = touch_kind(te) else { continue };
            let over = match kind {
                Touch::Item => sim::weapon::pickup::player_touches_item(player_origin, te.origin),
                _ => crate::script::entity_contact(self, bmin, bmax, te),
            };
            if !over {
                continue;
            }
            let (me, other) = (self.entity_value(vm, n), self.entity_value(vm, t));
            vm.notify_entity(t, "touch", std::slice::from_ref(&me));
            vm.notify_entity(n, "touch", &[other]);
            match kind {
                Touch::Item => self.touch_item(vm, n, t, true),
                Touch::Multi => {
                    // `Touch_Multi`.
                    self.g_trigger(vm, t, n);
                    if self.ent(t).is_some_and(|e| e.spawnflags & SPAWN_ONCE != 0) {
                        self.free_entity_delayed(t);
                    }
                }
                Touch::Hurt => self.hurt_touch(vm, t, n),
            }
        }
    }

    /// `hurt_touch`: a damage volume hurts whoever lives in it, at most once per 50 ms (1000 ms when
    /// slow). The volume is the attacker.
    pub fn hurt_touch(&mut self, vm: &mut Vm, trigger: u16, who: u16) {
        let now = self.level.time;
        let Some(t) = self.ent(trigger) else { return };
        if !self.ent(who).is_some_and(|e| e.takedamage) || t.x.hurt_next > now {
            return;
        }
        let (flags, damage) = (t.spawnflags, t.dmg);
        self.g_trigger(vm, trigger, who);
        if let Some(t) = self.ent_mut(trigger) {
            t.x.hurt_next = now + if flags & HURT_SLOW != 0 { 1000 } else { 50 };
        }
        let mut d = Damage::new(damage, MOD_TRIGGER_HURT);
        d.attacker = Some(trigger);
        d.inflictor = Some(trigger);
        self.g_damage(vm, who, d);
        if flags & HURT_ONCE != 0
            && let Some(t) = self.ent_mut(trigger)
        {
            t.x.hurt_off = true;
        }
    }

    /// The `use` of an entity handler, run by `useby`: `trigger_hurt` switches on and off,
    /// `trigger_damage` fires as if damaged.
    pub fn use_trigger(&mut self, vm: &mut Vm, n: u16, user: u16) {
        let Some(e) = self.ent(n) else { return };
        match &*e.classname {
            "trigger_hurt" => {
                if let Some(e) = self.ent_mut(n) {
                    e.x.hurt_off = !e.x.hurt_off;
                }
            }
            "trigger_damage" => {
                let hits = e.x.damage_accumulate + 1;
                self.activate_trigger_damage(vm, n, user, hits, -1);
            }
            _ => {}
        }
    }

    /// The map's `wait`, `accumulate`, `threshold` and `START_OFF` of a trigger just spawned:
    /// the spawn functions' part that reads the entity string.
    pub fn init_trigger_spawn(
        &mut self,
        n: u16,
        wait: Option<f32>,
        accumulate: i32,
        threshold: i32,
    ) {
        let Some(e) = self.ent_mut(n) else { return };
        let no_wait = wait.is_some_and(|w| w <= 0.0);
        match &*e.classname {
            "trigger_once" => e.spawnflags |= SPAWN_ONCE,
            "trigger_multiple" | "trigger_radius" | "trigger_disk" if no_wait => {
                e.spawnflags |= SPAWN_ONCE;
            }
            "trigger_damage" => {
                e.x.damage_accumulate = accumulate;
                e.x.damage_threshold = threshold;
                if no_wait {
                    e.spawnflags |= SPAWN_DAMAGE_ONCE;
                }
            }
            "trigger_hurt" => {
                if e.dmg == 0 {
                    e.dmg = 5;
                }
                e.x.hurt_off = e.spawnflags & HURT_START_OFF != 0;
            }
            _ => {}
        }
    }

    /// `Activate_trigger_damage`: damage that gets past the threshold, the kind filter and the
    /// accumulation sets the volume off. `mean` is `-1` for `useby`.
    fn activate_trigger_damage(&mut self, vm: &mut Vm, n: u16, other: u16, damage: i32, mean: i32) {
        let Some(e) = self.ent(n) else { return };
        let (accumulate, threshold) = (e.x.damage_accumulate, e.x.damage_threshold);
        if (threshold > 0 && damage < threshold)
            || !trigger_damage_responds(e.spawnflags, mean)
            || (accumulate != 0 && TD_HEALTH - e.health < accumulate)
        {
            return;
        }
        let once = e.spawnflags & SPAWN_DAMAGE_ONCE != 0;
        if mean != -1 {
            self.g_trigger(vm, n, other);
        }
        if let Some(e) = self.ent_mut(n) {
            e.health = TD_HEALTH;
        }
        if once {
            self.free_entity_delayed(n);
        }
    }

    /// `Pain_trigger_damage` and `Die_trigger_damage`: `G_Damage` reached the volume.
    pub fn trigger_damage_hit(
        &mut self,
        vm: &mut Vm,
        n: u16,
        attacker: u16,
        damage: i32,
        mean: u8,
    ) {
        self.activate_trigger_damage(vm, n, attacker, damage, i32::from(mean));
        if let Some(e) = self.ent_mut(n)
            && e.x.damage_accumulate == 0
        {
            e.health = TD_HEALTH;
        }
    }

    /// `G_CheckHitTriggerDamage`: a shot, melee swing or missile that travelled `start` to `end`
    /// through `trigger_damage` volumes is told it hit them.
    pub fn check_hit_trigger_damage(
        &mut self,
        vm: &mut Vm,
        activator: u16,
        start: Vec3,
        end: Vec3,
        damage: i32,
        mean: u8,
    ) {
        self.hit_trigger_damage(vm, activator, start, end, damage, mean, false);
    }

    /// `G_GrenadeTouchTriggerDamage`: as above for a grenade in flight, to the volumes that asked
    /// for it (`enablegrenadetouchdamage`).
    pub fn grenade_touch_trigger_damage(
        &mut self,
        vm: &mut Vm,
        grenade: u16,
        start: Vec3,
        end: Vec3,
        damage: i32,
    ) {
        self.hit_trigger_damage(vm, grenade, start, end, damage, MOD_GRENADE, true);
    }

    #[allow(clippy::too_many_arguments)]
    fn hit_trigger_damage(
        &mut self,
        vm: &mut Vm,
        activator: u16,
        start: Vec3,
        end: Vec3,
        damage: i32,
        mean: u8,
        needs_grenade_flag: bool,
    ) {
        let Some(world) = self.world.as_ref() else {
            return;
        };
        let lo = [0, 1, 2].map(|i| start[i].min(end[i]));
        let hi = [0, 1, 2].map(|i| start[i].max(end[i]));
        let mut hit = Vec::new();
        world.area_entities(lo, hi, contents::NONSENTIENTTRIGGER, |t| {
            hit.push(t);
            true
        });
        let diff = {
            let d = crate::bullet::sub(end, start);
            crate::bullet::normalized(d)
        };
        for t in hit {
            let Some(e) = self.ent(t) else { continue };
            if &*e.classname != "trigger_damage"
                || (needs_grenade_flag && e.flags & FL_GRENADE_TOUCH_DAMAGE == 0)
                || !self
                    .world
                    .as_ref()
                    .is_some_and(|w| w.sight_trace_to_entity(t, start, end, contents::MASK_ALL))
            {
                continue;
            }
            let who = self.entity_value(vm, activator);
            vm.notify_entity(
                t,
                "damage",
                &[
                    Value::Int(damage),
                    who,
                    Value::Vector(diff),
                    Value::Vector([0.0; 3]),
                    Value::str(MODS[usize::from(mean)]),
                ],
            );
            self.trigger_damage_hit(vm, t, activator, damage, mean);
        }
    }
}
