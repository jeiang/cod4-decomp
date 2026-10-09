// SPDX-License-Identifier: GPL-3.0-only
//! Entity linking, model attachments and player corpses (`G_EntLinkTo`, `G_EntUnlink`,
//! `G_EntAttach`, `G_GeneralLink`, `PlayerCmd_ClonePlayer`).
//!
//! A linked entity keeps a fixed offset from its parent (or from one of the parent's tags) and
//! follows it every frame. Tags are rest-pose bones (see [`crate::tags`]); the follow ignores
//! animations the parent plays. Attachments and hidden parts are recorded on the entity and
//! only matter to tag lookups and to clients (they are not simulated server-side).

use std::rc::Rc;
use std::sync::Arc;

use gsc::Vm;
use net::entity::EntityState;
use sim::Vec3;
use sim::cm::Collide;
use sim::contents;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType};
use sim::traj::Trajectory;

use crate::client::{Conn, Session, Team};
use crate::content::Content;
use crate::delta;
use crate::game::{Ent, EntKind, Game};
use crate::playeranim::LegsWire;
use crate::tags::{self, IDENTITY, Mat43, Skeleton};
use sim::pm::math;

/// `FL_SUPPORTS_LINKTO` in `ent->flags`.
pub const FL_SUPPORTS_LINKTO: i32 = 0x1000;
/// Models one entity can carry (`attachModelNames`).
pub const MAX_ATTACH: usize = net::entity::MAX_ATTACH;
/// First entity number of the player corpse ring and its size (`G_SpawnPlayerClone`).
pub const CORPSE_BASE: usize = 64;
pub const CORPSES: usize = 8;

/// Where a tag is in the bones of `models`, the model of an entity followed by the attachments before the one that
/// hangs from it, as an [`EntityState::attach`] tag: 0 for no tag (the model's origin), else the index of the last bone
/// of that name, plus one (the bone a `Rig` hangs the attachment from). A tag no model has hangs from the origin too.
pub fn tag_wire(content: &Content, models: &[&str], tag: &str) -> u16 {
    if tag.is_empty() {
        return 0;
    }
    let names = || {
        models.iter().flat_map(|m| {
            content
                .model_bone_names(m)
                .into_iter()
                .flat_map(|b| b.iter())
        })
    };
    names()
        .enumerate()
        .filter(|(_, n)| n.eq_ignore_ascii_case(tag))
        .last()
        .map_or(0, |(i, _)| (i + 1).min(255) as u16)
}

/// The tag name a [`tag_wire`] value stands for among the bones of `models`; `None` for the model's origin.
pub fn tag_name(content: &Content, models: &[&str], wire: u16) -> Option<Arc<str>> {
    let i = usize::from(wire.checked_sub(1)?);
    models
        .iter()
        .flat_map(|m| {
            content
                .model_bone_names(m)
                .into_iter()
                .flat_map(|b| b.iter())
        })
        .nth(i)
        .cloned()
}

#[derive(Debug, Clone)]
pub struct Link {
    pub parent: u16,
    /// Lowercase tag of the parent, `None` for the parent's origin.
    pub tag: Option<Rc<str>>,
    /// The entity's frame relative to the parent tag (`tagInfo->axis`).
    pub axis: Mat43,
}

#[derive(Debug, Clone)]
pub struct Attach {
    pub model: Rc<str>,
    /// Lowercase; empty attaches at the model origin.
    pub tag: Rc<str>,
    pub ignore_collision: bool,
}

/// A player corpse after `clonePlayer`: falls to the ground, stays solid to bullets.
#[derive(Debug, Clone)]
pub struct Corpse {
    pub velocity: Vec3,
    pub falling: bool,
    /// Level time at which the body stops being a clone-in-progress (`BodyEnd`).
    pub end_time: i32,
    /// The death animation `getcorpseanim` reports.
    pub anim: Rc<str>,
    /// The client the body was, its team and the legs clip it died in, for the clients that draw it.
    pub client: u16,
    pub team: Team,
    pub legs: LegsWire,
    /// Level time of the clone, and the low byte of [`Level::corpses_made`] it was.
    pub start_time: i32,
    pub serial: u8,
}

/// State of the entity builtins that does not belong to the engine fields of [`Ent`].
#[derive(Debug, Clone)]
pub struct EntExtra {
    pub link: Option<Link>,
    pub attached: Vec<Attach>,
    /// `s.partBits`: hidden bones by DObj bone index, most significant bit first.
    pub hide_bits: [u32; 4],
    /// Trigger state: hint string table index (`None` is the empty hint), cursor hint
    /// (`-1` inherits), `usetriggerrequirelookat`, team filter and the claiming client.
    pub hint: Option<usize>,
    pub cursor_hint: i32,
    pub require_look_at: bool,
    pub trigger_team: Team,
    pub claimed_by: Option<u16>,
    pub corpse: Option<Corpse>,
    /// `startragdoll` was called; ragdolls themselves are a client-side simulation.
    pub ragdoll: bool,
    /// `physicslaunch(point, force)`: the launch the clients simulate.
    pub physics_launch: Option<(Vec3, Vec3)>,
    /// `spawnplane`: the player the plane belongs to; the clients mark it on the compass.
    pub plane_owner: Option<u16>,
    /// `trigger_hurt`: not hurting (`START_OFF`, a spent `ONCE`, or toggled by `useby`) and the level
    /// time before which it hurts nobody again (`item[0].index`).
    pub hurt_off: bool,
    pub hurt_next: i32,
    /// `trigger_damage`: `accumulate` and `threshold` of the map.
    pub damage_accumulate: i32,
    pub damage_threshold: i32,
}

impl Default for EntExtra {
    fn default() -> Self {
        Self {
            link: None,
            attached: Vec::new(),
            hide_bits: [0; 4],
            hint: None,
            cursor_hint: 0,
            require_look_at: false,
            trigger_team: Team::Free,
            claimed_by: None,
            corpse: None,
            ragdoll: false,
            physics_launch: None,
            plane_owner: None,
            hurt_off: false,
            hurt_next: 0,
            damage_accumulate: 0,
            damage_threshold: 0,
        }
    }
}

/// Why `linkto` failed, in the order the original diagnoses it.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkError {
    ParentHasNoModel,
    ParentModelInvalid(String),
    NoSuchTag(String, String),
    Other,
}

impl LinkError {
    pub fn message(&self) -> String {
        match self {
            LinkError::ParentHasNoModel => {
                "failed to link entity since parent has no model".to_owned()
            }
            LinkError::ParentModelInvalid(m) => {
                format!("failed to link entity since parent model '{m}' is invalid")
            }
            LinkError::NoSuchTag(t, m) => format!(
                "failed to link entity since tag '{t}' does not exist in parent model '{m}'"
            ),
            LinkError::Other => "failed to link entity".to_owned(),
        }
    }
}

impl Game {
    pub fn supports_linkto(&self, n: u16) -> bool {
        self.ent(n)
            .is_some_and(|e| e.kind == EntKind::Client || e.flags & FL_SUPPORTS_LINKTO != 0)
    }

    pub fn is_linked(&self, n: u16) -> bool {
        self.ent(n).is_some_and(|e| e.x.link.is_some())
    }

    /// `SV_DObjExists`: the entity's model is a skeletal model the server knows.
    pub fn dobj_exists(&self, n: u16) -> bool {
        self.skeleton_of(n).is_some()
    }

    fn skeleton_of(&self, n: u16) -> Option<Arc<Skeleton>> {
        let e = self.ent(n)?;
        if e.model.is_empty() {
            return None;
        }
        self.content.skeleton(&e.model).cloned()
    }

    /// The bones of the entity's DObj in index order: its model, then each attachment.
    pub fn dobj_models(&self, n: u16) -> Vec<(Rc<str>, Arc<Skeleton>)> {
        let Some(e) = self.ent(n) else {
            return Vec::new();
        };
        let Some(primary) = self.skeleton_of(n) else {
            return Vec::new();
        };
        let mut out = vec![(e.model.clone(), primary)];
        for a in &e.x.attached {
            if let Some(s) = self.content.skeleton(&a.model) {
                out.push((a.model.clone(), s.clone()));
            }
        }
        out
    }

    /// `G_DObjGetLocalTagMatrix`: the rest pose of `tag` in the entity's model space, looking
    /// in its model and then in what is attached to it.
    pub fn local_tag(&self, n: u16, tag: &str) -> Option<Mat43> {
        let primary = self.skeleton_of(n)?;
        if let Some(m) = primary.tag(tag) {
            return Some(*m);
        }
        let e = self.ent(n)?;
        for a in &e.x.attached {
            let Some(s) = self.content.skeleton(&a.model) else {
                continue;
            };
            let Some(m) = s.tag(tag) else { continue };
            let base = if a.tag.is_empty() {
                IDENTITY
            } else {
                *primary.tag(&a.tag).unwrap_or(&IDENTITY)
            };
            return Some(tags::mul43(m, &base));
        }
        None
    }

    /// `G_DObjGetWorldTagMatrix`.
    pub fn world_tag(&self, n: u16, tag: &str) -> Option<Mat43> {
        let local = self.local_tag(n, tag)?;
        let e = self.ent(n)?;
        Some(tags::mul43(&local, &tags::frame(e.origin, e.angles)))
    }

    /// `G_CalcTagParentAxis`: the frame the children of `parent` hang from.
    fn tag_parent_frame(&self, parent: u16, tag: Option<&str>) -> Option<Mat43> {
        let p = self.ent(parent)?;
        let frame = tags::frame(p.origin, p.angles);
        match tag {
            None => Some(frame),
            Some(t) => Some(tags::mul43(&self.local_tag(parent, t)?, &frame)),
        }
    }

    /// `G_EntLinkTo` / `G_EntLinkToWithOffset`. `offset` is `(origin, angles)` relative to
    /// the parent tag; without it the entity keeps its current world position.
    pub fn link_to(
        &mut self,
        n: u16,
        parent: u16,
        tag: Option<&str>,
        offset: Option<(Vec3, Vec3)>,
    ) -> Result<(), LinkError> {
        self.unlink(n);
        let tag: Option<Rc<str>> = tag
            .filter(|t| !t.is_empty())
            .map(|t| t.to_ascii_lowercase().into());
        if let Some(t) = &tag
            && (!self.dobj_exists(parent) || self.local_tag(parent, t).is_none())
        {
            return Err(self.diagnose(parent, Some(t)));
        }
        let mut up = Some(parent);
        while let Some(p) = up {
            if p == n {
                return Err(self.diagnose(parent, None));
            }
            up = self
                .ent(p)
                .and_then(|e| e.x.link.as_ref())
                .map(|l| l.parent);
        }
        let pframe = self
            .tag_parent_frame(parent, tag.as_deref())
            .ok_or(LinkError::Other)?;
        let axis = match offset {
            Some((origin, angles)) => {
                let a = tags::angles_to_axis(angles);
                [a[0], a[1], a[2], origin]
            }
            None => {
                let e = self.ent(n).ok_or(LinkError::Other)?;
                tags::mul43(&tags::frame(e.origin, e.angles), &tags::inverse43(&pframe))
            }
        };
        if let Some(e) = self.ent_mut(n) {
            e.x.link = Some(Link { parent, tag, axis });
        }
        if let Some(c) = self.client_mut(n) {
            c.ps.pm_type = match c.ps.pm_type {
                PmType::Normal => PmType::NormalLinked,
                PmType::Dead => PmType::DeadLinked,
                t => t,
            };
        }
        Ok(())
    }

    /// The error `ScrCmd_LinkTo` reports after `G_EntLinkTo` returned 0.
    fn diagnose(&self, parent: u16, tag: Option<&str>) -> LinkError {
        if !self.dobj_exists(parent) {
            return match self.ent(parent) {
                Some(e) if !e.model.is_empty() => {
                    LinkError::ParentModelInvalid(e.model.to_string())
                }
                _ => LinkError::ParentHasNoModel,
            };
        }
        match tag {
            Some(t) if self.local_tag(parent, t).is_none() => {
                let model = self
                    .ent(parent)
                    .map_or(String::new(), |e| e.model.to_string());
                LinkError::NoSuchTag(t.to_owned(), model)
            }
            _ => LinkError::Other,
        }
    }

    /// `G_EntUnlink`: the entity stays where it is.
    pub fn unlink(&mut self, n: u16) {
        let Some(e) = self.ent_mut(n) else { return };
        if e.x.link.take().is_none() {
            return;
        }
        if let Some(c) = self.client(n) {
            let angles = [c.ps.viewangles[0], c.ps.viewangles[1], 0.0];
            self.set_client_view_angle(n, angles);
            if let Some(e) = self.ent_mut(n) {
                e.angles[0] = 0.0;
            }
        }
        if let Some(c) = self.client_mut(n) {
            c.ps.pm_type = match c.ps.pm_type {
                PmType::NormalLinked => PmType::Normal,
                PmType::DeadLinked => PmType::Dead,
                t => t,
            };
        }
    }

    /// `G_EntUnlinkFree`: unlinks the entity and everything linked to it.
    pub fn unlink_all(&mut self, n: u16) {
        self.unlink(n);
        let children: Vec<u16> = self
            .in_use()
            .filter(|(_, e)| e.x.link.as_ref().is_some_and(|l| l.parent == n))
            .map(|(c, _)| c)
            .collect();
        for c in children {
            self.unlink(c);
        }
    }

    /// `G_GeneralLink` and the linked branch of `G_RunClient`: moves the entity to where its
    /// parent's tag now is. The parent is brought up to date first.
    pub fn follow_link(&mut self, vm: &mut Vm, n: u16) {
        let Some(link) = self.ent(n).and_then(|e| e.x.link.clone()) else {
            return;
        };
        if self.ent(link.parent).is_none() {
            self.unlink(n);
            return;
        }
        self.run_mover(vm, link.parent);
        let Some(pframe) = self.tag_parent_frame(link.parent, link.tag.as_deref()) else {
            self.unlink(n);
            return;
        };
        if let Some(c) = self.client_mut(n) {
            if c.noclip {
                return;
            }
            c.ps.pm_type = if c.session == Session::Dead {
                PmType::DeadLinked
            } else {
                PmType::NormalLinked
            };
            let origin = tags::transform43(link.axis[3], &pframe);
            c.ps.origin = origin;
            self.set_ent_pose(n, origin, None);
            return;
        }
        let world = tags::mul43(&link.axis, &pframe);
        self.set_ent_pose(
            n,
            world[3],
            Some(tags::axis_to_angles(&[world[0], world[1], world[2]])),
        );
    }

    /// `G_SetOrigin` / `G_SetAngle`: the entity is where it is, not moving.
    fn set_ent_pose(&mut self, n: u16, origin: Vec3, angles: Option<Vec3>) {
        if let Some(e) = self.ent_mut(n) {
            e.origin = origin;
            e.mv.pos.tr = Trajectory::stationary(origin);
            if let Some(a) = angles {
                e.angles = a;
                e.mv.ang.tr = Trajectory::stationary(a);
            }
        }
        self.relink(n);
    }

    /// Fills the attachments and hidden bones of `s`, the state of entity `n`: what a client needs to build the entity's
    /// model from its own copy of the zones. A model the clients were never told of cannot be drawn and is left out.
    pub fn net_attachments(&self, n: u16, s: &mut EntityState) {
        let Some(e) = self.ent(n) else { return };
        s.part_bits = e.x.hide_bits;
        let mut models: Vec<&str> = vec![&e.model];
        let mut slot = 0;
        for a in &e.x.attached {
            let index = self.models.find(&a.model);
            if index != 0 {
                s.set_attach(slot, index as u16, tag_wire(&self.content, &models, &a.tag));
                slot += 1;
                models.push(&a.model);
            }
        }
    }

    /// `G_EntAttach`; `false` when the table is full or the model is unknown.
    pub fn attach_model(&mut self, n: u16, model: &str, tag: &str, ignore_collision: bool) -> bool {
        if self.content.model(model).is_none() {
            return false;
        }
        let Some(e) = self.ent_mut(n) else {
            return false;
        };
        if e.x.attached.len() >= MAX_ATTACH {
            return false;
        }
        e.x.attached.push(Attach {
            model: model.to_ascii_lowercase().into(),
            tag: tag.to_ascii_lowercase().into(),
            ignore_collision,
        });
        true
    }

    /// `G_EntDetach`; `false` when nothing matched.
    pub fn detach_model(&mut self, n: u16, model: &str, tag: &str) -> bool {
        let Some(e) = self.ent_mut(n) else {
            return false;
        };
        let i =
            e.x.attached.iter().position(|a| {
                a.model.eq_ignore_ascii_case(model) && a.tag.eq_ignore_ascii_case(tag)
            });
        match i {
            Some(i) => {
                e.x.attached.remove(i);
                true
            }
            None => false,
        }
    }

    /// `G_GetFreePlayerCorpseIndex`: the first unused corpse slot; with all of them used, the one whose body lies
    /// farthest from the first player, freed.
    fn free_corpse_slot(&mut self, vm: &mut Vm) -> usize {
        let slot = |i: usize| CORPSE_BASE + i;
        if let Some(i) = (0..CORPSES).find(|i| self.ent(slot(*i) as u16).is_none()) {
            return slot(i);
        }
        let from = (0..self.max_clients as u16)
            .find_map(|n| self.client(n).filter(|c| c.conn == Conn::Connected))
            .map(|c| c.ps.origin)
            .unwrap_or_default();
        let dist = |i: usize| {
            let o = self.ent(slot(i) as u16).map_or(from, |e| e.origin);
            (0..3).map(|k| (o[k] - from[k]).powi(2)).sum::<f32>()
        };
        let far = (0..CORPSES)
            .max_by(|a, b| dist(*a).total_cmp(&dist(*b)))
            .unwrap_or(0);
        self.free_entity(vm, slot(far) as u16);
        slot(far)
    }

    /// `PlayerCmd_ClonePlayer`: a corpse entity at the player's place with its velocity. It
    /// falls until it lands; clients animate it. Returns the corpse's entity number.
    pub fn clone_player(
        &mut self,
        vm: &mut Vm,
        n: u16,
        duration_ms: i32,
        anim: &str,
    ) -> Option<u16> {
        let src = self.ent(n)?.clone();
        let c = self.client(n)?;
        let (origin, velocity, yaw) = (c.ps.origin, c.ps.velocity, c.ps.viewangles[1]);
        let (team, legs) = (c.team, c.pose.legs_wire());
        let slot = self.free_corpse_slot(vm);
        self.level.corpses_made += 1;
        let mut e = Ent::new(EntKind::Plain, "");
        e.model = src.model.clone();
        e.origin = origin;
        e.angles = [0.0, yaw, 0.0];
        e.mins = src.mins;
        e.maxs = src.maxs;
        e.contents = contents::CORPSE | contents::ACTOR;
        e.x.attached = src.x.attached.clone();
        e.x.hide_bits = src.x.hide_bits;
        let max = self
            .cvars
            .get("g_clonePlayerMaxVelocity")
            .map_or(80.0, |_| self.cvars.float("g_clonePlayerMaxVelocity"));
        let mut v = velocity;
        for a in v.iter_mut().take(2) {
            *a = a.min(max);
        }
        e.x.corpse = Some(Corpse {
            velocity: v,
            falling: true,
            end_time: self.level.time + duration_ms,
            anim: anim.into(),
            client: n,
            team,
            legs,
            start_time: self.level.time,
            serial: self.level.corpses_made as u8,
        });
        if self.ents.len() <= slot {
            self.ents.resize(slot + 1, None);
        }
        self.ents[slot] = Some(e);
        self.level.num_entities = self.level.num_entities.max(slot + 1);
        self.relink(slot as u16);
        Some(slot as u16)
    }

    /// `GScr_PlaceSpawnPoint`: raises the entity by up to 128 units, then drops it to the
    /// floor with the player hull. The ground entity flag the original sets on what it lands
    /// on is not set here: a spawned player finds its ground on its first command, which is when movers start carrying it.
    pub fn place_spawn_point(&mut self, n: u16) {
        let Some(origin) = self.ent(n).map(|e| e.origin) else {
            return;
        };
        let Some(w) = self.world.as_ref() else { return };
        let mask = contents::MASK_PLAYERSOLID;
        let lerp = |a: Vec3, b: Vec3, f: f32| {
            [
                a[0] + (b[0] - a[0]) * f,
                a[1] + (b[1] - a[1]) * f,
                a[2] + (b[2] - a[2]) * f,
            ]
        };
        let up = [origin[0], origin[1], origin[2] + 128.0];
        let t = w.trace(origin, up, PLAYER_MINS, PLAYER_MAXS, n, mask);
        let top = lerp(origin, up, t.fraction);
        let down = [top[0], top[1], top[2] - 262_144.0];
        let t = w.trace(top, down, PLAYER_MINS, PLAYER_MAXS, n, mask);
        let floor = lerp(top, down, t.fraction);
        if w.trace(floor, floor, PLAYER_MINS, PLAYER_MAXS, n, mask)
            .all_solid
        {
            self.print(format!(
                "WARNING: Spawn point entity {n} is in solid at ({}, {}, {})\n",
                origin[0] as i32, origin[1] as i32, origin[2] as i32
            ));
        }
        self.set_ent_pose(n, floor, None);
    }

    /// How far the death animation moves the body this frame, forward and to the left (`G_GetAnimDeltaForCorpse`).
    fn corpse_delta(&self, c: &Corpse) -> Vec3 {
        let (Some(m), Some(a)) = (
            self.content.root_motion(&c.anim),
            self.content.anim(&c.anim),
        ) else {
            return [0.0; 3];
        };
        let at = |t: i32| ((t - c.start_time) as f32 * 0.001 / a.length.max(1e-3)).clamp(0.0, 1.0);
        let now = self.level.time;
        delta::rel_delta(m, at(now - self.level.frametime), at(now)).1
    }

    /// `G_RunCorpseMove`: a falling body drops under gravity; a landed one follows the death animation's root motion
    /// and falls again if that carries it off its footing. A body that lands on a slope tilts to it and one that
    /// strikes a wall or a steep face is pushed off it.
    pub fn run_corpse(&mut self, vm: &mut Vm, n: u16) {
        let now = self.level.time;
        let dt = self.level.frametime as f32 * 0.001;
        let gravity = self.cvars.float("g_gravity");
        let Some(e) = self.ent(n) else { return };
        let Some(c) = e.x.corpse.as_ref() else { return };
        if now >= c.end_time && e.contents & contents::ACTOR != 0 {
            if let Some(e) = self.ent_mut(n) {
                e.contents = contents::CORPSE;
            }
            self.relink(n);
            return;
        }
        let (mins, maxs, start, angles) = (e.mins, e.maxs, e.origin, e.angles);
        let (falling, mut v) = (c.falling, c.velocity);
        let delta = if falling {
            [0.0; 3]
        } else {
            self.corpse_delta(c)
        };
        let moving = !falling && math::dot(&delta, &delta) > 1.0;
        if !falling && !moving {
            return;
        }
        let mut end = start;
        if falling {
            v[2] -= gravity * dt;
            end = math::mad(&end, dt, &v);
        }
        if moving {
            let (fwd, right, _) = math::angle_vectors(&[0.0, angles[1], 0.0]);
            end = math::mad(&end, delta[0], &fwd);
            end = math::mad(&end, -delta[1], &right);
        }
        let Some(w) = self.world.as_ref() else { return };
        let mask = contents::MASK_DEADSOLID;
        let mut t = w.trace(start, end, mins, maxs, n, mask);
        let mut at = math::lerp(&start, &end, t.fraction);
        let (mut falling_now, mut velocity, mut tilt) = (falling, v, None);
        if t.fraction == 1.0 {
            if moving {
                // Carried off its footing: it falls from here at the speed it was moving.
                let below = [at[0], at[1], at[2] - 1.0];
                let g = w.trace(at, below, mins, maxs, n, mask);
                if g.fraction == 1.0 && !g.all_solid {
                    falling_now = true;
                    velocity = [delta[0] / dt, delta[1] / dt, 0.0];
                    let (fwd, right, _) = math::angle_vectors(&[0.0, angles[1], 0.0]);
                    velocity = math::mad(
                        &math::mad(&[0.0; 3], velocity[0], &fwd),
                        -velocity[1],
                        &right,
                    );
                }
            }
        } else if w.point_contents(at, sim::cm::ENTITYNUM_NONE, contents::NODROP) & contents::NODROP
            != 0
        {
            self.free_entity(vm, n);
            return;
        } else if falling {
            if t.all_solid {
                // Started inside something: look again from a little higher, ignoring player clips.
                let up = [start[0], start[1], start[2] + 32.0];
                t = w.trace(up, end, mins, maxs, n, mask & !contents::PLAYERCLIP);
                if !t.all_solid {
                    at = math::lerp(&up, &end, t.fraction);
                }
            }
            velocity = [0.0; 3];
            if t.all_solid || t.normal[2] > 0.0 {
                falling_now = false;
                if !t.all_solid {
                    // `G_BounceCorpse`: the body's up is the surface's normal, its forward still toward its yaw.
                    let (fwd, _, _) = math::angle_vectors(&[0.0, angles[1], 0.0]);
                    let mut left = math::cross(&t.normal, &fwd);
                    math::normalize(&mut left);
                    let mut f = math::cross(&left, &t.normal);
                    math::normalize(&mut f);
                    tilt = Some(tags::axis_to_angles(&[f, left, t.normal]));
                }
            } else {
                at = math::mad(&at, 1.0, &t.normal);
            }
        }
        if let Some(e) = self.ent_mut(n) {
            e.origin = at;
            e.mv.pos.tr = Trajectory::stationary(at);
            if let Some(a) = tilt {
                e.angles = a;
            }
            if let Some(c) = e.x.corpse.as_mut() {
                c.falling = falling_now;
                c.velocity = velocity;
            }
        }
        self.relink(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Content;
    use crate::cvar::Cvars;
    use gsc::{Builtins, Options, compile};

    fn vm() -> Vm {
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .expect("compiles");
        Vm::new(prog).expect("loads")
    }

    fn game() -> Game {
        let mut g = Game::new(Cvars::new(), Content::default());
        g.max_clients = 8;
        g
    }

    fn origin_ent(g: &mut Game, origin: Vec3, angles: Vec3) -> u16 {
        let mut e = Ent::new(EntKind::Plain, "script_origin");
        e.origin = origin;
        e.angles = angles;
        e.flags |= FL_SUPPORTS_LINKTO;
        g.spawn(e).unwrap()
    }

    fn near(a: Vec3, b: Vec3) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-2)
    }

    #[test]
    fn a_linked_entity_keeps_its_offset_when_the_parent_moves_and_turns() {
        let mut g = game();
        let mut vm = vm();
        let parent = origin_ent(&mut g, [100.0, 0.0, 0.0], [0.0; 3]);
        let child = origin_ent(&mut g, [110.0, 0.0, 5.0], [0.0; 3]);
        g.link_to(child, parent, None, None).unwrap();
        {
            let p = g.ent_mut(parent).unwrap();
            p.origin = [100.0, 50.0, 0.0];
            p.angles = [0.0, 90.0, 0.0];
        }
        g.run_mover(&mut vm, child);
        let c = g.ent(child).unwrap();
        // 10 units ahead of the parent, which now faces +Y.
        assert!(near(c.origin, [100.0, 60.0, 5.0]), "{:?}", c.origin);
        assert!((c.angles[1] - 90.0).abs() < 1e-2, "{:?}", c.angles);
    }

    #[test]
    fn an_explicit_offset_replaces_the_current_position() {
        let mut g = game();
        let mut vm = vm();
        let parent = origin_ent(&mut g, [0.0, 0.0, 0.0], [0.0, 90.0, 0.0]);
        let child = origin_ent(&mut g, [999.0, 999.0, 999.0], [0.0; 3]);
        g.link_to(
            child,
            parent,
            None,
            Some(([20.0, 0.0, 0.0], [0.0, 0.0, 0.0])),
        )
        .unwrap();
        g.run_mover(&mut vm, child);
        assert!(near(g.ent(child).unwrap().origin, [0.0, 20.0, 0.0]));
    }

    #[test]
    fn chains_follow_the_root_even_when_the_child_runs_first() {
        let mut g = game();
        let mut vm = vm();
        let a = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        let b = origin_ent(&mut g, [10.0, 0.0, 0.0], [0.0; 3]);
        let c = origin_ent(&mut g, [20.0, 0.0, 0.0], [0.0; 3]);
        g.link_to(c, b, None, None).unwrap();
        g.link_to(b, a, None, None).unwrap();
        g.ent_mut(a).unwrap().origin = [0.0, 0.0, 100.0];
        g.run_mover(&mut vm, c);
        assert!(near(g.ent(b).unwrap().origin, [10.0, 0.0, 100.0]));
        assert!(near(g.ent(c).unwrap().origin, [20.0, 0.0, 100.0]));
    }

    #[test]
    fn linking_into_a_cycle_is_refused() {
        let mut g = game();
        let a = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        let b = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        g.link_to(a, b, None, None).unwrap();
        // The original reports the failure with the same text as a model-less parent.
        assert_eq!(
            g.link_to(b, a, None, None),
            Err(LinkError::ParentHasNoModel)
        );
        assert!(!g.is_linked(b));
    }

    #[test]
    fn unlinking_leaves_the_entity_in_place_and_freeing_the_parent_unlinks_children() {
        let mut g = game();
        let mut vm = vm();
        let parent = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        let child = origin_ent(&mut g, [5.0, 0.0, 0.0], [0.0; 3]);
        g.link_to(child, parent, None, None).unwrap();
        g.ent_mut(parent).unwrap().origin = [0.0, 0.0, 50.0];
        g.run_mover(&mut vm, child);
        g.unlink(child);
        g.ent_mut(parent).unwrap().origin = [0.0, 0.0, 99.0];
        g.run_mover(&mut vm, child);
        assert!(near(g.ent(child).unwrap().origin, [5.0, 0.0, 50.0]));
        g.link_to(child, parent, None, None).unwrap();
        g.free_entity(&mut vm, parent);
        assert!(!g.is_linked(child));
    }

    #[test]
    fn linking_to_a_tag_needs_a_model_with_that_tag() {
        let mut g = game();
        let parent = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        let child = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        assert_eq!(
            g.link_to(child, parent, Some("tag_flash"), None),
            Err(LinkError::ParentHasNoModel)
        );
        g.ent_mut(parent).unwrap().model = "nope".into();
        assert_eq!(
            g.link_to(child, parent, Some("tag_flash"), None)
                .unwrap_err()
                .message(),
            "failed to link entity since parent model 'nope' is invalid"
        );
    }

    #[test]
    fn attachments_detach_in_order_and_respect_the_limit() {
        let mut g = game();
        let e = origin_ent(&mut g, [0.0; 3], [0.0; 3]);
        // Unknown models cannot be attached.
        assert!(!g.attach_model(e, "nope", "tag_a", false));
        g.ent_mut(e).unwrap().x.attached = (0..MAX_ATTACH)
            .map(|i| Attach {
                model: format!("m{i}").into(),
                tag: "t".into(),
                ignore_collision: false,
            })
            .collect();
        assert!(g.detach_model(e, "M3", "T"));
        assert!(!g.detach_model(e, "m3", "t"));
        let names: Vec<_> = g
            .ent(e)
            .unwrap()
            .x
            .attached
            .iter()
            .map(|a| a.model.to_string())
            .collect();
        assert_eq!(&names[..4], ["m0", "m1", "m2", "m4"]);
        assert_eq!(names.len(), MAX_ATTACH - 1);
    }

    #[test]
    fn with_every_corpse_slot_used_the_body_farthest_from_the_first_player_is_replaced() {
        let mut g = game();
        let mut vm = vm();
        g.clients = (0..8)
            .map(|n| {
                let mut c = crate::client::Client::new(n, false, String::new());
                c.conn = Conn::Free;
                c
            })
            .collect();
        g.ents.resize(8, None);
        let n = g.connect_client(&mut vm, false, "p").expect("slot");
        let mut slots = Vec::new();
        for i in 0..CORPSES {
            // Bodies at 100, 200 ... along x, so the last made is the farthest.
            g.client_mut(n).unwrap().ps.origin = [100.0 * (i + 1) as f32, 0.0, 0.0];
            slots.push(g.clone_player(&mut vm, n, 0, "a").expect("corpse"));
        }
        assert_eq!(
            slots.iter().map(|s| usize::from(*s)).collect::<Vec<_>>(),
            (CORPSE_BASE..CORPSE_BASE + CORPSES).collect::<Vec<_>>()
        );
        let first = g.ent(slots[0]).unwrap().x.corpse.as_ref().unwrap().serial;
        g.client_mut(n).unwrap().ps.origin = [0.0; 3];
        let ninth = g.clone_player(&mut vm, n, 0, "a").expect("corpse");
        assert_eq!(ninth, slots[CORPSES - 1], "the farthest slot is reused");
        let serial = g.ent(ninth).unwrap().x.corpse.as_ref().unwrap().serial;
        assert_ne!(serial, first);
        assert!(g.ent(slots[0]).is_some(), "the nearest body stays");
    }
}
