// SPDX-License-Identifier: GPL-3.0-only
//! The client's effects: [`ClientEvent`]s become effect playbacks (impact sprites and decals, explosions, blood), the
//! playback runs on the server clock against the static map, and every frame yields the sprites, decals, models and
//! sounds to draw.
//!
//! Which effect an impact plays is the original's: the weapon's impact type picks a row of the impact table, the
//! surface type a column; flesh has its own four columns (body or head, fatal or not).

use crate::decal::{self, Placement};
use crate::events::{ClientEvent, Events};
use crate::viewmodel::ViewTags;
use assets::zone::fx::{FxEffectDef, FxImpactTable};
use assets::zone::gfx::Material;
use assets::zone::gfxworld::GfxWorld;
use assets::zone::weapon::WeaponDef;
use fx::{Camera, Draws, Frame, Frustum, Fx, Library, SoundPlay};
use glam::Vec3;
use net::entity::{EntityState, etype};
use render::{DynMesh, DynVertex, ModelInstance, ModelKind};
use server::content::Content;
use sim::cm::Collide;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Rows of the impact table by `WeaponDef::impact_type` (the original's `g_TypeName` order: small, large, shotgun, AP,
/// grenade bounce, grenade explode, rocket explode, dud, each small, large and shotgun and AP with a normal and an exit
/// row).
const ROW_BY_IMPACT_TYPE: [Option<usize>; 9] = [
    None,
    Some(0),
    Some(2),
    Some(6),
    Some(4),
    Some(8),
    Some(9),
    Some(10),
    Some(11),
];
/// `WeaponDef::impact_type` of the dud row.
const IMPACT_TYPE_DUD: i32 = 8;
const SURFACE_FLESH: u8 = 7;
const FLESH_BODY_NONFATAL: usize = 0;
const FLESH_BODY_FATAL: usize = 1;
/// How high above a player's feet blood appears.
const CHEST: f32 = 40.0;

/// Whether a weapon's explosion also plays the impact table's blast for the surface (a flashbang's and a dud's do
/// not): `weapProjExplosion_t` 2 and 4.
pub fn explodes_on_surface(d: &WeaponDef) -> bool {
    !matches!(d.proj_explosion, 2 | 4)
}

/// What the world looks like to a particle: solid map geometry.
pub(crate) struct Tracer<'a>(pub(crate) &'a dyn Collide);

impl fx::World for Tracer<'_> {
    fn trace(&self, a: Vec3, b: Vec3, mins: Vec3, maxs: Vec3) -> Option<(f32, Vec3)> {
        let t = self.0.trace(
            a.to_array(),
            b.to_array(),
            mins.to_array(),
            maxs.to_array(),
            sim::cm::ENTITYNUM_NONE,
            sim::contents::SOLID,
        );
        (t.fraction < 1.0).then(|| (t.fraction, Vec3::from(t.normal)))
    }

    /// Particles stop at windows and the sky box too (and at the item clip, for those that ask), and go on moving
    /// when they start inside something: `FX_UpdateElementPosition_CollidingStep`'s mask and `FX_TraceHitSomething`.
    fn trace_particle(
        &self,
        a: Vec3,
        b: Vec3,
        mins: Vec3,
        maxs: Vec3,
        item_clip: bool,
    ) -> Option<(f32, Vec3)> {
        let mask = sim::contents::SOLID
            | sim::contents::GLASS
            | sim::contents::SKY
            | if item_clip {
                sim::contents::ITEMCLIP
            } else {
                0
            };
        let t = self.0.trace(
            a.to_array(),
            b.to_array(),
            mins.to_array(),
            maxs.to_array(),
            sim::cm::ENTITYNUM_NONE,
            mask,
        );
        (t.fraction < 1.0 && !t.start_solid && !t.all_solid)
            .then(|| (t.fraction, Vec3::from(t.normal)))
    }
}

/// What the effects see of the map: its collision for the particles that bounce, its light grid for the elements
/// that take their colour from the light they stand in.
struct FxWorld<'a> {
    tracer: Tracer<'a>,
    map: &'a GfxWorld,
}

impl fx::World for FxWorld<'_> {
    fn trace(&self, a: Vec3, b: Vec3, mins: Vec3, maxs: Vec3) -> Option<(f32, Vec3)> {
        self.tracer.trace(a, b, mins, maxs)
    }

    fn trace_particle(
        &self,
        a: Vec3,
        b: Vec3,
        mins: Vec3,
        maxs: Vec3,
        item_clip: bool,
    ) -> Option<(f32, Vec3)> {
        self.tracer.trace_particle(a, b, mins, maxs, item_clip)
    }

    fn lighting(&self, p: Vec3) -> [u8; 3] {
        render::lightgrid::average_lighting(
            self.map,
            p.to_array(),
            &render::lightgrid::LightingEnv::none(),
        )
    }
}

/// A decal's clipped geometry, kept for as long as the decal lives.
struct Mark {
    material: Arc<Material>,
    verts: Vec<DynVertex>,
}

pub struct Effects {
    fx: Fx,
    impact: Option<Arc<FxImpactTable>>,
    world: Arc<GfxWorld>,
    marks: HashMap<u64, Mark>,
    /// The trail effect playing on each missile entity, by entity number.
    missiles: HashMap<u16, u64>,
    /// A `--fx-demo` effect with a trail, swept around a circle so the trail has a path: the handle, the circle's
    /// centre and axes, and when it started.
    /// The server time of the last update.
    clock: i32,
    sweep: Option<(u64, Frame, i32)>,
    /// The script effect entities, by entity number.
    world_fx: HashMap<u16, WorldFx>,
    world_fx_stamp: u32,
    /// The client number of this player and the gun in their hands, for the first-person flash.
    own: u16,
    view: Option<ViewTags>,
    /// The camera's horizontal field of view in radians and its aspect ratio, for the view volume that the effects
    /// cull against; `None` culls nothing.
    projection: Option<(f32, f32)>,
    /// What was played, by kind; for the report.
    pub played: BTreeMap<&'static str, u64>,
    /// Names of effects an event asked for that the content lacks.
    pub missing: HashSet<String>,
}

/// A frame at `origin` facing `forward` with `up` above it.
fn oriented(origin: Vec3, forward: [f32; 3], up: [f32; 3]) -> Frame {
    let (f, u) = (Vec3::from(forward), Vec3::from(up));
    Frame {
        origin,
        axis: [f, u.cross(f), u],
    }
}

/// What a script effect entity (`spawnfx`, `playloopedfx`) has done on this client.
struct WorldFx {
    /// The looping effect's handle.
    handle: Option<u64>,
    /// When a looped effect restarts next.
    next: i32,
    /// The last trigger of a `spawnfx` effect acted on, and the server time the pending one plays at.
    seen: u8,
    start: Option<i32>,
    /// The update that last saw the entity.
    stamp: u32,
}

/// Looping script effects do not start while this many elements are alive: the rest is for the effects of play. A
/// share of the pool of elements (`fx::MAX_ELEMS`, the original's 2048).
const LOOP_BUDGET: usize = 1500;

/// Particle clouds drawn at once (`R_AddParticleCloudToScene`'s 256); the farthest ones are the ones left out.
const MAX_CLOUDS: usize = 256;

/// Where and how a script effect entity plays.
fn world_fx_frame(e: &EntityState) -> Frame {
    let (f, r, u) = sim::pm::math::angle_vectors(&e.angles);
    Frame {
        origin: Vec3::from(e.origin),
        axis: [Vec3::from(f), -Vec3::from(r), Vec3::from(u)],
    }
}

/// What to draw this frame.
#[derive(Default)]
pub struct Drawn {
    pub meshes: Vec<DynMesh>,
    pub models: Vec<ModelInstance>,
    /// The omni and spot lights the effects add.
    pub lights: Vec<fx::Light>,
    /// How many sprites and decals the meshes hold.
    pub quads: usize,
    pub decals: usize,
    /// How many particle clouds the meshes hold.
    pub clouds: usize,
    /// How many trail strips the meshes hold.
    pub trails: usize,
    /// How far the camera shaking turns the view (pitch, yaw, roll in degrees); set by the caller.
    pub sway: [f32; 3],
}

impl Effects {
    pub fn new(content: &Content, world: Arc<GfxWorld>) -> Self {
        let mut lib = Library::default();
        for e in content.effects() {
            lib.add(e);
        }
        Self {
            fx: Fx::new(Arc::new(lib)),
            impact: content.impact_table().cloned(),
            world,
            marks: HashMap::new(),
            missiles: HashMap::new(),
            world_fx: HashMap::new(),
            world_fx_stamp: 0,
            clock: 0,
            sweep: None,
            own: u16::MAX,
            view: None,
            projection: None,
            played: BTreeMap::new(),
            missing: HashSet::new(),
        }
    }

    fn impact_effect(
        &self,
        impact_type: i32,
        surface: u8,
        fatal: bool,
    ) -> Option<Arc<FxEffectDef>> {
        let row = usize::try_from(impact_type)
            .ok()
            .and_then(|i| ROW_BY_IMPACT_TYPE.get(i).copied().flatten())?;
        let entry = self.impact.as_ref()?.table.get(row)?;
        if surface == SURFACE_FLESH {
            let i = if fatal {
                FLESH_BODY_FATAL
            } else {
                FLESH_BODY_NONFATAL
            };
            entry.flesh[i].clone()
        } else {
            entry.nonflesh.get(usize::from(surface))?.clone()
        }
    }

    /// The effect a table or a weapon points at. Tables in one zone name effects of another by a placeholder (the
    /// name with a comma in front and nothing in it); the effect itself is in the library under the plain name.
    fn resolve(&self, d: &Arc<FxEffectDef>) -> Arc<FxEffectDef> {
        d.name
            .as_deref()
            .and_then(|n| n.strip_prefix(','))
            .and_then(|n| self.fx.library().get(n))
            .unwrap_or(d)
            .clone()
    }

    fn play(&mut self, kind: &'static str, def: Option<Arc<FxEffectDef>>, at: Vec3, dir: Vec3) {
        if let Some(d) = def {
            let d = self.resolve(&d);
            self.fx.play(&d, Frame::facing(at, dir));
            *self.played.entry(kind).or_default() += 1;
        }
    }

    /// Plays `def` oriented by `frame` (a broken prop's destroy effect).
    pub fn play_frame(&mut self, kind: &'static str, def: &Arc<FxEffectDef>, frame: Frame) {
        let d = self.resolve(def);
        self.fx.play(&d, frame);
        *self.played.entry(kind).or_default() += 1;
    }

    /// The loud hits of physics models (shell casings, debris) since the last call.
    pub fn take_collisions(&mut self) -> Vec<fx::Collision> {
        self.fx.take_collisions()
    }

    /// Starts what `ev` shows. `weapon` resolves a weapon index to its definition.
    pub fn event(&mut self, ev: &ClientEvent, weapon: &dyn Fn(u16) -> Option<Arc<WeaponDef>>) {
        match ev {
            ClientEvent::BulletImpact {
                origin,
                normal,
                surface,
                weapon: w,
                exit,
                ..
            } => {
                // The exit wound's marks are not drawn (only its sound is played).
                if *exit {
                    return;
                }
                let t = weapon(*w).map_or(0, |d| d.impact_type);
                let def = self.impact_effect(t, *surface, false);
                self.play(
                    "bullet_impact",
                    def,
                    Vec3::from(*origin),
                    Vec3::from(*normal),
                );
            }
            ClientEvent::MissileBounce {
                origin,
                normal,
                surface,
                weapon: w,
                ..
            } => {
                let t = weapon(*w).map_or(0, |d| d.impact_type);
                let def = self.impact_effect(t, *surface, false);
                self.play(
                    "missile_bounce",
                    def,
                    Vec3::from(*origin),
                    Vec3::from(*normal),
                );
            }
            ClientEvent::Explosion {
                origin,
                normal,
                surface,
                weapon: w,
                ..
            } => {
                let d = weapon(*w);
                let n = if d
                    .as_ref()
                    .is_some_and(|d| d.proj_explosion_effect_force_normal_up != 0)
                {
                    Vec3::Z
                } else {
                    Vec3::from(*normal)
                };
                // The surface's own blast, then the weapon's (`EV_GRENADE_EXPLODE`, `EV_ROCKET_EXPLODE`); a flashbang
                // or a dud has only its own.
                if let Some(d) = d.as_ref().filter(|d| explodes_on_surface(d)) {
                    let def = self.impact_effect(d.impact_type, *surface, false);
                    self.play("explosion_impact", def, Vec3::from(*origin), n);
                }
                let def = d.as_ref().and_then(|d| d.proj_explosion_effect.clone());
                self.play("explosion", def, Vec3::from(*origin), n);
            }
            ClientEvent::Dud {
                origin,
                normal,
                surface,
                weapon: w,
                settled,
                ..
            } => {
                let (at, n) = (Vec3::from(*origin), Vec3::from(*normal));
                // `EV_CHANGE_TO_DUD`: the table's first surface only.
                let table =
                    self.impact_effect(IMPACT_TYPE_DUD, if *settled { 0 } else { *surface }, false);
                self.play("dud", table, at, n);
                if *settled {
                    return;
                }
                let def = weapon(*w).and_then(|d| d.proj_dud_effect.clone());
                self.play("dud", def, at, n);
            }
            ClientEvent::PlayFx {
                origin,
                forward,
                up,
                name,
                ..
            } => match self.fx.library().get(name).cloned() {
                Some(d) => {
                    let d = self.resolve(&d);
                    self.fx
                        .play(&d, oriented(Vec3::from(*origin), *forward, *up));
                    *self.played.entry("play_fx").or_default() += 1;
                }
                None => {
                    self.missing.insert(name.clone());
                }
            },
            ClientEvent::PlayerDeath { origin, push, .. } => {
                let def = self.impact_effect(1, SURFACE_FLESH, true);
                let dir = Vec3::from(*push).try_normalize().unwrap_or(Vec3::Z);
                self.play(
                    "player_death",
                    def,
                    Vec3::from(*origin) + Vec3::Z * CHEST,
                    dir,
                );
            }
            ClientEvent::PlayerPain { origin, .. } => {
                let def = self.impact_effect(1, SURFACE_FLESH, false);
                self.play(
                    "player_pain",
                    def,
                    Vec3::from(*origin) + Vec3::Z * CHEST,
                    Vec3::Z,
                );
            }
            ClientEvent::Physics { .. } | ClientEvent::Earthquake { .. } => {}
            ClientEvent::WeaponFire {
                eye,
                angles,
                weapon: w,
                shooter,
                vehicle,
            } => {
                let Some(d) = weapon(*w) else { return };
                let mine = *shooter == self.own && !vehicle;
                let (flash, eject) = if mine {
                    (&d.view_flash_effect, &d.view_shell_eject_effect)
                } else {
                    (&d.world_flash_effect, &d.world_shell_eject_effect)
                };
                let (flash_at, brass_at) = match self.view.filter(|_| mine) {
                    Some(t) => (t.flash, t.brass),
                    // A vehicle's event names its muzzle, and it throws no shells.
                    None if *vehicle => {
                        let (f, r, u) = sim::pm::math::angle_vectors(angles);
                        let flash = Frame {
                            origin: Vec3::from(*eye),
                            axis: [Vec3::from(f), -Vec3::from(r), Vec3::from(u)],
                        };
                        (Some(flash), None)
                    }
                    None => {
                        // Another player's gun is not drawn: put the muzzle where a rifle's would be.
                        let (f, r, u) = sim::pm::math::angle_vectors(angles);
                        let (f, r, u) = (Vec3::from(f), Vec3::from(r), Vec3::from(u));
                        let eye = Vec3::from(*eye);
                        let at =
                            |fwd: f32, right: f32, down: f32| eye + f * fwd + r * right - u * down;
                        (
                            Some(Frame {
                                origin: at(30.0, 6.0, 6.0),
                                axis: [f, -r, u],
                            }),
                            Some(Frame {
                                origin: at(12.0, 6.0, 5.0),
                                axis: [r, f, u],
                            }),
                        )
                    }
                };
                for (kind, def, at) in [
                    ("muzzle_flash", flash, flash_at),
                    ("shell_eject", eject, brass_at),
                ] {
                    if let (Some(def), Some(at)) = (def, at) {
                        let def = self.resolve(def);
                        self.fx.play(&def, at);
                        *self.played.entry(kind).or_default() += 1;
                    }
                }
            }
        }
    }

    /// Keeps the trail effect of every missile in flight at the missile: starts it for a new one, follows it as it
    /// moves, ends it when the missile is gone. Each item is the entity number, its position, its velocity and its
    /// weapon.
    pub fn missiles(
        &mut self,
        flying: &[(u16, Vec3, Vec3, u16)],
        weapon: &dyn Fn(u16) -> Option<Arc<WeaponDef>>,
    ) {
        for (number, origin, velocity, w) in flying {
            let frame = Frame::facing(*origin, *velocity);
            if let Some(id) = self.missiles.get(number) {
                self.fx.move_effect(*id, frame);
            } else if let Some(def) = weapon(*w).and_then(|d| d.proj_trail_effect.clone()) {
                let def = self.resolve(&def);
                let id = self.fx.play_attached(&def, frame);
                self.missiles.insert(*number, id);
                *self.played.entry("missile_trail").or_default() += 1;
            }
        }
        let fx = &mut self.fx;
        self.missiles.retain(|n, id| {
            let flying = flying.iter().any(|m| m.0 == *n);
            if !flying {
                fx.stop(*id);
            }
            flying
        });
    }

    /// Keeps the script effect entities of the newest snapshot playing (`CG_Fx`, `CG_LoopFx`): a looped effect starts
    /// when its entity is in range of `eye` and restarts every period (it is stopped beyond its cull distance, and
    /// not started while the effects are near their element budget); a triggered one plays once per trigger, from the
    /// moment the trigger names even if that is already past (a client that joined late sees the end of it); an
    /// entity that is gone stops its effect. `names` gives the effect names by index.
    pub fn world_fx(&mut self, ents: &[EntityState], names: &Events, eye: Vec3, now: i32) {
        self.world_fx_stamp = self.world_fx_stamp.wrapping_add(1);
        let stamp = self.world_fx_stamp;
        for e in ents {
            let looped = e.etype == etype::LOOP_FX;
            if !looped && e.etype != etype::FX {
                continue;
            }
            let w = self.world_fx.entry(e.number).or_insert(WorldFx {
                handle: None,
                next: 0,
                seen: 0,
                start: None,
                stamp,
            });
            w.stamp = stamp;
            // An effect the pool evicted is not playing any more.
            if w.handle.is_some_and(|id| !self.fx.is_live(id)) {
                w.handle = None;
            }
            let origin = Vec3::from(e.origin);
            if looped {
                let cull = e.velocity[0];
                if cull != 0.0 && origin.distance(eye) >= cull {
                    if let Some(id) = w.handle.take() {
                        self.fx.stop(id);
                    }
                    continue;
                }
                let period = i32::try_from(e.pm_flags).unwrap_or(i32::MAX).max(1);
                match w.handle {
                    Some(id) => {
                        // After a stall, skip to now rather than replaying every missed period.
                        if now.wrapping_sub(w.next) >= period.saturating_mul(4) {
                            w.next = now;
                        }
                        while now >= w.next {
                            if !self.fx.retrigger(id, w.next) {
                                w.handle = None;
                                break;
                            }
                            w.next += period;
                        }
                    }
                    None if self.fx.live_elems() < LOOP_BUDGET => {
                        let Some(def) = self.world_fx_def(names, e.model) else {
                            continue;
                        };
                        let frame = world_fx_frame(e);
                        let w = self.world_fx.get_mut(&e.number).expect("just inserted");
                        w.handle = Some(self.fx.play_attached(&def, frame));
                        w.next = now + period;
                        *self.played.entry("looped_fx").or_default() += 1;
                    }
                    None => {}
                }
            } else {
                if e.event_seq != w.seen {
                    w.seen = e.event_seq;
                    // The low 24 bits of the server time it plays at, read as the nearest such time to now.
                    let d = ((e.eflags.wrapping_sub(now as u32) << 8) as i32) >> 8;
                    w.start = Some(now.wrapping_add(d));
                }
                if let Some(start) = w.start.filter(|s| now >= *s) {
                    w.start = None;
                    if let Some(def) = self.world_fx_def(names, e.model) {
                        self.fx.play_at(&def, world_fx_frame(e), start);
                        *self.played.entry("triggered_fx").or_default() += 1;
                    }
                }
            }
        }
        let fx = &mut self.fx;
        self.world_fx.retain(|_, w| {
            let keep = w.stamp == stamp;
            if !keep && let Some(id) = w.handle {
                fx.stop(id);
            }
            keep
        });
    }

    /// The definition of effect `index`, noting the name when the content lacks it.
    fn world_fx_def(&mut self, names: &Events, index: u16) -> Option<Arc<FxEffectDef>> {
        let name = names.fx_name(usize::from(index))?;
        match self.fx.library().get(name).cloned() {
            Some(d) => Some(self.resolve(&d)),
            None => {
                if !self.missing.contains(name) {
                    self.missing.insert(name.to_owned());
                }
                None
            }
        }
    }

    /// Looping script effects playing now.
    pub fn looped_fx(&self) -> usize {
        self.world_fx
            .values()
            .filter(|w| w.handle.is_some_and(|id| self.fx.is_live(id)))
            .count()
    }

    /// Tells the effects who this player is and where the gun in their hands has its muzzle and ejection port.
    pub fn set_view(&mut self, own: u16, tags: Option<ViewTags>) {
        self.own = own;
        self.view = tags;
    }

    /// The camera's horizontal field of view in radians and its aspect ratio (width over height).
    pub fn set_projection(&mut self, fov_x: f32, aspect: f32) {
        self.projection = (fov_x > 0.0 && aspect > 0.0).then_some((fov_x, aspect));
    }

    /// Plays `name` 90 units ahead of a camera, facing it (for `--fx-demo`).
    pub fn demo(&mut self, name: &str, eye: Vec3, yaw: f32, pitch: f32) {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let forward = Vec3::new(cp * cy, cp * sy, sp);
        let def = self.fx.library().get(name).cloned();
        if def.is_none() {
            // Say what is there that looks like it.
            let want = name.to_ascii_lowercase();
            let stem = want.rsplit('/').next().unwrap_or(&want);
            self.missing.insert(name.to_owned());
            let mut near: Vec<_> = self
                .fx
                .library()
                .names()
                .filter(|n| n.contains(stem))
                .collect();
            near.sort_unstable();
            self.missing.extend(
                near.into_iter()
                    .take(30)
                    .map(|n| format!("  did you mean {n}")),
            );
        }
        if let Some(id) = self.sweep.take().map(|s| s.0) {
            self.fx.stop(id);
        }
        match def.map(|d| self.resolve(&d)) {
            Some(d) if d.elems.iter().any(|e| e.trail.is_some()) => {
                let centre = Frame::facing(eye + forward * 150.0, forward);
                let id = self.fx.play_attached(&d, centre);
                self.sweep = Some((id, centre, self.clock));
                *self.played.entry("demo").or_default() += 1;
            }
            d => self.play("demo", d, eye + forward * 90.0, -forward),
        }
    }

    /// Advances every playing effect to `now_ms` of the server clock.
    pub fn update(&mut self, now_ms: i32, world: &dyn Collide) {
        self.clock = now_ms;
        if let Some((id, c, t0)) = self.sweep {
            // Round a circle 60 units wide in 1.5 s, facing along its path.
            let a = (now_ms - t0) as f32 * std::f32::consts::TAU / 1500.0;
            let at = c.origin + c.axis[1] * (60.0 * a.cos()) + c.axis[2] * (60.0 * a.sin());
            let heading = c.axis[1] * -a.sin() + c.axis[2] * a.cos();
            self.fx.move_effect(id, Frame::facing(at, heading));
        }
        let map = FxWorld {
            tracer: Tracer(world),
            map: &self.world,
        };
        self.fx.update(now_ms, &map);
    }

    /// Sounds the effects started since the last call.
    pub fn take_sounds(&mut self) -> Vec<SoundPlay> {
        self.fx.take_sounds()
    }

    /// How much of the line from `start` to `end` shows through the smoke of the last frame drawn, 0 to 1.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn visibility(&self, start: Vec3, end: Vec3) -> f32 {
        self.fx.visibility(start, end)
    }

    pub fn live_elems(&self) -> usize {
        self.fx.live_elems()
    }

    /// What to draw from a camera at `eye` looking along `yaw` and `pitch` (radians).
    pub fn draw(&mut self, eye: Vec3, yaw: f32, pitch: f32, roll: f32) -> Drawn {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let forward = Vec3::new(cp * cy, cp * sy, sp);
        let left = Vec3::new(-sy, cy, 0.0);
        let up = forward.cross(left);
        // Rolled clockwise like the view (`render::View::roll`).
        let (sr, cr) = roll.sin_cos();
        let axis = [forward, left * cr + up * sr, up * cr - left * sr];
        let cam = Camera {
            origin: eye,
            axis,
            frustum: self.projection.map(|(fov_x, aspect)| {
                let tan = (fov_x * 0.5).tan();
                Frustum::new(eye, axis, [tan, tan / aspect])
            }),
        };
        // What the effects that start next are culled against.
        self.fx.set_camera(cam);
        let mut d = Draws::default();
        self.fx.draw(&cam, &mut d);
        let mut out = Drawn {
            quads: d.quads.len(),
            lights: std::mem::take(&mut d.lights),
            ..Drawn::default()
        };

        // Farthest first within a sort order, so blended sprites layer correctly.
        d.quads.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(b.depth.total_cmp(&a.depth))
        });
        for q in &d.quads {
            if out
                .meshes
                .last()
                .is_none_or(|m| !Arc::ptr_eq(&m.material, &q.material))
            {
                out.meshes.push(DynMesh::new(q.material.clone()));
            }
            let v = |i: usize| DynVertex {
                pos: q.corners[i].to_array(),
                color: q.color,
                uv: q.uv[i],
                normal: q.normal.to_array(),
                tangent: q.tangent.to_array(),
            };
            out.meshes
                .last_mut()
                .expect("just pushed")
                .push_quad([v(0), v(1), v(2), v(3)]);
        }

        // Trails last: the strips of points the effects left behind, far to near.
        d.trails.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(b.depth.total_cmp(&a.depth))
        });
        for t in &d.trails {
            let mut mesh = DynMesh::new(t.material.clone());
            let v = |i: u32| {
                let v = &t.verts[i as usize];
                DynVertex {
                    pos: v.pos.to_array(),
                    color: v.color,
                    uv: v.uv,
                    normal: v.normal.to_array(),
                    tangent: v.tangent.to_array(),
                }
            };
            mesh.verts = t.indices.iter().map(|&i| v(i)).collect();
            out.meshes.push(mesh);
            out.trails += 1;
        }

        // Clouds, far to near.
        d.clouds.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(b.depth.total_cmp(&a.depth))
        });
        let skipped = d.clouds.len().saturating_sub(MAX_CLOUDS);
        for c in d.clouds.iter().skip(skipped) {
            out.meshes.push(DynMesh::cloud(
                c.material.clone(),
                render::Cloud {
                    origin: c.origin,
                    axis: c.axis,
                    scale: c.scale,
                    endpos: c.endpos,
                    radius: c.radius,
                    color: c.color,
                },
            ));
            out.clouds += 1;
        }

        let mut seen = HashSet::new();
        for dc in &d.decals {
            seen.insert(dc.id);
            if !self.marks.contains_key(&dc.id) {
                let p = Placement {
                    origin: dc.origin,
                    normal: dc.normal,
                    up: dc.up,
                    half_size: dc.half_size,
                };
                let reach = Vec3::splat(p.half_size[0].max(p.half_size[1]) + 16.0);
                let verts = decal::clip(
                    decal::receivers(
                        &self.world,
                        p.origin - reach,
                        p.origin + reach,
                        // The original marks the brushes with the second material and the models with the first.
                        dc.world_material.as_ref().unwrap_or(&dc.material),
                        &dc.material,
                    ),
                    &p,
                );
                self.marks.insert(
                    dc.id,
                    Mark {
                        material: dc.material.clone(),
                        verts,
                    },
                );
            }
            let m = &self.marks[&dc.id];
            if m.verts.is_empty() {
                continue;
            }
            let mut mesh = DynMesh::new(m.material.clone());
            mesh.light_origin = Some(dc.origin.to_array());
            mesh.verts = m
                .verts
                .iter()
                .map(|v| DynVertex {
                    color: dc.color,
                    ..*v
                })
                .collect();
            out.meshes.push(mesh);
            out.decals += 1;
        }
        self.marks.retain(|id, _| seen.contains(id));

        for m in &d.models {
            let mut inst = ModelInstance::new(m.model.clone(), ModelKind::World);
            inst.origin = m.origin.to_array();
            inst.light_origin = inst.origin;
            inst.angles = crate::props::angles_of(m.axis);
            inst.scale = m.scale;
            out.models.push(inst);
        }
        out
    }
}
