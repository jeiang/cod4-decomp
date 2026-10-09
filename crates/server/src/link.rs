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
use sim::Vec3;
use sim::cm::Collide;
use sim::contents;
use sim::pm::{PLAYER_MAXS, PLAYER_MINS, PmType};
use sim::traj::Trajectory;

use crate::client::{Session, Team};
use crate::game::{Ent, EntKind, Game};
use crate::tags::{self, IDENTITY, Mat43, Skeleton};

/// `FL_SUPPORTS_LINKTO` in `ent->flags`.
pub const FL_SUPPORTS_LINKTO: i32 = 0x1000;
/// Models one entity can carry (`attachModelNames`).
pub const MAX_ATTACH: usize = 19;
/// First entity number of the player corpse ring and its size (`G_SpawnPlayerClone`).
pub const CORPSE_BASE: usize = 64;
pub const CORPSES: usize = 8;

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
        let (origin, velocity) = (c.ps.origin, c.ps.velocity);
        let slot = CORPSE_BASE + self.level.next_corpse % CORPSES;
        self.level.next_corpse += 1;
        self.free_entity(vm, slot as u16);
        let mut e = Ent::new(EntKind::Plain, "");
        e.model = src.model.clone();
        e.origin = origin;
        e.angles = src.angles;
        e.mins = src.mins;
        e.maxs = src.maxs;
        e.contents = contents::CORPSE | contents::ACTOR;
        e.x.attached = src.x.attached.clone();
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
    /// on only matters to pushers, which this server does not run for players yet.
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

    /// `G_RunCorpse`: gravity until the body lands, then it only waits for `BodyEnd`.
    pub fn run_corpse(&mut self, n: u16) {
        let now = self.level.time;
        let dt = self.level.frametime as f32 * 0.001;
        let gravity = self.cvars.float("g_gravity");
        let Some(e) = self.ent_mut(n) else { return };
        let Some(c) = e.x.corpse.as_mut() else { return };
        let (mins, maxs) = (e.mins, e.maxs);
        if now >= c.end_time && e.contents & contents::ACTOR != 0 {
            e.contents = contents::CORPSE;
            self.relink(n);
            return;
        }
        if !c.falling {
            return;
        }
        c.velocity[2] -= gravity * dt;
        let (start, v) = (e.origin, c.velocity);
        let end = [
            start[0] + v[0] * dt,
            start[1] + v[1] * dt,
            start[2] + v[2] * dt,
        ];
        let Some(w) = self.world.as_ref() else { return };
        let t = w.trace(start, end, mins, maxs, n, contents::MASK_DEADSOLID);
        let at = [
            start[0] + (end[0] - start[0]) * t.fraction,
            start[1] + (end[1] - start[1]) * t.fraction,
            start[2] + (end[2] - start[2]) * t.fraction,
        ];
        if let Some(e) = self.ent_mut(n) {
            e.origin = at;
            e.mv.pos.tr = Trajectory::stationary(at);
            if t.fraction < 1.0
                && let Some(c) = e.x.corpse.as_mut()
            {
                c.velocity = [0.0; 3];
                c.falling = false;
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
}
