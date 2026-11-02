// SPDX-License-Identifier: GPL-3.0-only
// Persistent smoke follows KisakCOD (GPL-3.0, KisakCOD contributors and Activision),
// `cgame_mp/cg_main_mp.cpp:1578-1616` and `cgame_mp/cg_ents_mp.cpp:1462-1482`
// (CoD4 MP 1.8; native 1.7 unverified).
// Translated in part from KisakCOD cgame/cg_event.cpp (CG_PlayFxOnTag) and
// EffectsCore/fx_system.cpp / fx_update.cpp (FX_PlayBoltedEffect / FX_UpdateEffectBolt),
// GPL-3.0, copyright the KisakCOD contributors and Activision.
//! The client's effects: [`ClientEvent`]s become effect playbacks (impact sprites and decals, explosions, blood), the
//! playback runs on the server clock against the static map, and every frame yields the sprites, decals, models and
//! sounds to draw.
//!
//! Which effect an impact plays is the original's: the weapon's impact type picks a row of the impact table, the
//! surface type a column; flesh has its own columns (body or head; the stock event carries only the head bit, so the fatal columns are not used).

use crate::ambient::Ambient;
use crate::decal::{self, Placement};
use crate::events::{ClientEvent, Events, Shooter};
use crate::tracer::{self, Tracers};
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
pub(crate) const SURFACE_FLESH: u8 = 7;
/// How far a bullet is followed for its tracer.
const TRACER_RANGE: f32 = 8192.0;
/// The effect of a knife blow that struck a player (`cgMedia.fxKnifeBlood`), and the one shown with `cg_blood` off
/// (`cgMedia.fxKnifeNoBlood`).
const KNIFE_BLOOD: &str = "impacts/flesh_hit_knife";
const KNIFE_NO_BLOOD: &str = "impacts/flesh_hit_knife_noblood";
/// What a bullet's hit on a player plays with `cg_blood` off (`cgMedia.fxNoBloodFleshHit`).
const NO_BLOOD_FLESH_HIT: &str = "impacts/flesh_hit_noblood";
/// The column of the impact table's flesh effects (`g_FleshTypeName`: body_nonfatal, body_fatal, head_nonfatal,
/// head_fatal): the bullet event only says whether the head was hit.
fn flesh_column(head: bool) -> usize {
    2 * usize::from(head)
}

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
    ambient: Ambient,
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
    smoke: HashMap<u16, SmokeCloud>,
    smoke_quads: usize,
    /// The client number of this player and the gun in their hands, for the first-person flash.
    own: u16,
    view: Option<ViewTags>,
    /// The camera's horizontal field of view in radians and its aspect ratio, for the view volume that the effects
    /// cull against; `None` culls nothing.
    projection: Option<(f32, f32)>,
    /// Where the guns of the other players are, by client number.
    remote_tags: HashMap<u16, ViewTags>,
    /// The effects that follow a gun's tag until they end (a muzzle flash, a shell leaving the ejection port).
    bolted: Vec<Bolted>,
    script_bolts: Vec<ScriptBolt>,
    bolt_stats: ScriptBoltStats,
    tracers: Tracers,
    /// Tracers rolled by shots, not yet launched: the shooter, their eye and the way they look.
    pending_tracers: Vec<(u16, Vec3, Vec3)>,
    tracer_cvars: tracer::Cvars,
    /// `fx_marks`.
    marks_on: bool,
    /// `cg_blood`: off, a hit on a player plays the no-blood effects.
    blood_on: bool,
    /// `cg_bloodLimit` and `cg_bloodLimitMsec`: at most one blood effect in this window.
    blood_limit: Option<i32>,
    last_blood_ms: i32,
    /// `cg_brass`: off, no shell leaves a gun.
    brass_on: bool,
    /// The state of the dice for the tracers.
    dice: u32,
    /// `gfx_tracer`, which the tracers are drawn with; none, none are drawn.
    tracer_material: Option<Arc<Material>>,
    /// What was played, by kind; for the report. `blood` counts every play of an impact table's flesh effect looked up
    /// for a flesh surface, whatever asked for it.
    pub played: BTreeMap<&'static str, u64>,
    /// Impacts on a player that reached the effects: `BULLET_IMPACT` events on flesh (exit wounds aside) and missile
    /// bounces, blasts and duds on flesh. Stock draws at most one blood effect for each, and nothing else draws one
    /// (`CG_BulletHitEvent`; a player's pain is player state, not an effect).
    pub flesh_hits: u64,
    /// The last `impact_effect` lookup took the flesh column; `play` takes it to count blood.
    flesh_lookup: bool,
    /// Total draws (sprite quads, clouds, models, trail strips) that passed the camera's cull, by the lower-case name
    /// of the effect that was played; a runner's children count against it.
    pub effect_quads: BTreeMap<String, u64>,
    /// Names of effects an event asked for that the content lacks.
    pub missing: HashSet<String>,
}

/// A tag of a gun an effect is bolted to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Flash,
    Brass,
    Knife,
}

/// An effect that follows `tag` of `shooter`'s gun (`CG_PlayBoltedEffect`).
struct Bolted {
    id: u64,
    shooter: u16,
    tag: Tag,
}

struct ScriptBolt {
    id: Option<u64>,
    entity: u16,
    /// The entity's model when the event came ([`ClientEvent::PlayFxOnTag`]); the bolt starts only on that model.
    model: u16,
    /// What was drawn under the number this frame had that model.
    same_model: bool,
    name: String,
    tag: String,
    frame: Option<Frame>,
    attempted: bool,
    /// The owner's teleport bit when last seen; a flip stops the effect (`FX_GetBoltTemporalBits`).
    teleport: Option<u32>,
    sample: Option<(i32, u64, Frame, Vec3)>,
    last_frame: Option<Frame>,
    /// When the bolt began waiting for its entity to be drawn.
    waiting_since: Option<i32>,
}

#[derive(Default, Clone, Copy)]
pub struct ScriptBoltStats {
    pub active: usize,
    pub resolved_frames: u64,
    pub stopped: u64,
    pub expired: u64,
    pub missing_tags: u64,
    pub max_tag_displacement: f32,
    pub max_element_displacement: f32,
    pub max_follow_error: f32,
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

/// Kept until the server expiry, even across snapshot culling, so reentry does not
/// layer a second cloud over the first. Restarting Effects clears this cache.
struct SmokeCloud {
    start: i32,
    end: i32,
    handle: u64,
    def: Arc<FxEffectDef>,
}

fn smoke_clouds(
    fx: &mut Fx,
    clouds: &mut HashMap<u16, SmokeCloud>,
    ents: &[EntityState],
    effect: &dyn Fn(u16) -> Option<(Arc<FxEffectDef>, bool)>,
    now: i32,
) -> u64 {
    clouds.retain(|_, cloud| now <= cloud.end);
    let mut started = 0;
    for e in ents.iter().filter(|e| crate::events::persistent_smoke(e)) {
        let start = e.custom_explode_start_time;
        if now < start || now > e.time2 {
            continue;
        }
        if clouds
            .get(&e.number)
            .is_some_and(|cloud| cloud.start == start)
        {
            continue;
        }
        let Some((def, force_up)) = effect(e.weapon) else {
            continue;
        };
        let normal = if force_up {
            Vec3::Z
        } else {
            Vec3::from(sim::pm::math::angle_vectors(&e.angles).0)
        };
        let handle = fx.play_at(&def, Frame::facing(Vec3::from(e.origin), normal), start);
        if handle != 0 {
            clouds.insert(
                e.number,
                SmokeCloud {
                    start,
                    end: e.time2,
                    handle,
                    def,
                },
            );
            started += 1;
        }
    }
    started
}

/// Looping script effects do not start while this many elements are alive: the rest is for the effects of play. About
/// half of the pool of elements (`fx::MAX_ELEMS`, the original's 2048): the other half is for impacts, explosions and
/// muzzle flashes.
const LOOP_BUDGET: usize = 1000;

/// Particle clouds drawn at once (`R_AddParticleCloudToScene`'s 256); the farthest ones are the ones left out.
const MAX_CLOUDS: usize = 256;

/// How long a `playfxontag` bolt may wait for its entity to be drawn. Entities are drawn `INTERP_DELAY_MS` (100)
/// behind the snapshot the event came in, and a projectile shows only from its launch time; anything longer means the
/// client never poses the entity, which stock (`CG_GetBoneIndex` on its DObj) would.
const BOLT_POSE_WAIT_MS: i32 = 1000;

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
    /// How many tracers are in flight.
    pub tracers: usize,
    /// How far the camera shaking turns the view (pitch, yaw, roll in degrees); set by the caller.
    pub sway: [f32; 3],
}

/// Where `tag` is on the first of the posed `models` that has it, in the world (`CG_DObjGetWorldTagMatrix`).
pub fn model_tag_frame(models: &[ModelInstance], content: &Content, tag: &str) -> Option<Frame> {
    models.iter().find_map(|m| {
        let names = content.model_bone_names(m.model.name.as_deref()?)?;
        let i = names.iter().position(|n| n.eq_ignore_ascii_case(tag))?;
        let (quat, trans) = m
            .bones
            .get(i)
            .map(|b| (b.quat, b.trans))
            .or_else(|| m.model.base_mat.get(i).map(|b| (b.quat, b.trans)))?;
        let axes = sim::skel::quat::axes(&quat);
        let (f, r, u) = sim::pm::math::angle_vectors(&m.angles);
        let axis = [Vec3::from(f), -Vec3::from(r), Vec3::from(u)];
        let world = |v: [f32; 3]| axis[0] * v[0] + axis[1] * v[1] + axis[2] * v[2];
        Some(Frame {
            origin: Vec3::from(m.origin) + world(trans),
            axis: axes.map(world),
        })
    })
}

impl Effects {
    pub fn new(content: &Content, world: Arc<GfxWorld>) -> Self {
        let mut lib = Library::default();
        for e in content.effects() {
            lib.add(e);
        }
        Self {
            fx: Fx::new(Arc::new(lib)),
            ambient: Ambient::empty(),
            impact: content.impact_table().cloned(),
            world,
            marks: HashMap::new(),
            missiles: HashMap::new(),
            world_fx: HashMap::new(),
            world_fx_stamp: 0,
            smoke: HashMap::new(),
            smoke_quads: 0,
            clock: 0,
            sweep: None,
            own: u16::MAX,
            view: None,
            projection: None,
            remote_tags: HashMap::new(),
            bolted: Vec::new(),
            script_bolts: Vec::new(),
            bolt_stats: ScriptBoltStats::default(),
            tracers: Tracers::default(),
            pending_tracers: Vec::new(),
            tracer_cvars: tracer::Cvars::default(),
            marks_on: true,
            blood_on: true,
            blood_limit: None,
            last_blood_ms: -1,
            brass_on: true,
            dice: 0x9e37_79b9,
            tracer_material: None,
            played: BTreeMap::new(),
            flesh_hits: 0,
            flesh_lookup: false,
            effect_quads: BTreeMap::new(),
            missing: HashSet::new(),
        }
    }

    /// Starts the generated map effects in the same pool as gameplay and entity effects, unless `clientSideEffects`
    /// (`ambient`) is off.
    pub fn with_ambient(mut self, content: &Content, ambient: bool) -> Self {
        if ambient {
            self.ambient = Ambient::new(content, &mut self.fx);
        }
        self
    }

    /// Restarts the client-side effects with the level clock, including a same-map round restart.
    pub fn restart(&mut self, content: &Content, ambient: bool) {
        *self = Self::new(content, self.world.clone()).with_ambient(content, ambient);
    }

    fn impact_effect(
        &mut self,
        impact_type: i32,
        surface: u8,
        flesh: usize,
    ) -> Option<Arc<FxEffectDef>> {
        let row = usize::try_from(impact_type)
            .ok()
            .and_then(|i| ROW_BY_IMPACT_TYPE.get(i).copied().flatten())?;
        let entry = self.impact.as_ref()?.table.get(row)?;
        self.flesh_lookup = surface == SURFACE_FLESH;
        if surface == SURFACE_FLESH {
            entry.flesh.get(flesh)?.clone()
        } else {
            entry.nonflesh.get(usize::from(surface))?.clone()
        }
    }

    /// `d` is one of the impact table's flesh (blood) effects.
    fn is_blood(&self, d: &Arc<FxEffectDef>) -> bool {
        self.impact.as_ref().is_some_and(|t| {
            t.table.iter().any(|e| {
                e.flesh
                    .iter()
                    .any(|f| f.as_ref().is_some_and(|f| Arc::ptr_eq(f, d)))
            })
        })
    }

    /// `d`, after a warning when it is a stub (the name behind a comma and nothing in it): a reference to another
    /// zone's effect that linking left unresolved, which plays nothing.
    fn resolve(&self, d: &Arc<FxEffectDef>) -> Arc<FxEffectDef> {
        if let Some(plain) = d.name.as_deref().and_then(|n| n.strip_prefix(',')) {
            logging::dedup!(
                WARN,
                plain,
                effect = plain,
                "effect reference has no definition in the loaded zones"
            );
        }
        d.clone()
    }

    fn play(&mut self, kind: &'static str, def: Option<Arc<FxEffectDef>>, at: Vec3, dir: Vec3) {
        let flesh = std::mem::take(&mut self.flesh_lookup);
        if let Some(d) = def {
            let d = self.resolve(&d);
            let _span =
                tracing::debug_span!("fx_spawn", kind, effect = d.name.as_deref().unwrap_or(""))
                    .entered();
            self.fx.play(&d, Frame::facing(at, dir));
            *self.played.entry(kind).or_default() += 1;
            // The no-blood replacement is not the table's, so it does not count.
            if flesh && self.is_blood(&d) {
                *self.played.entry("blood").or_default() += 1;
            }
        }
    }

    /// Plays `def` oriented by `frame` (a broken prop's destroy effect).
    pub fn play_frame(&mut self, kind: &'static str, def: &Arc<FxEffectDef>, frame: Frame) {
        let d = self.resolve(def);
        let _span =
            tracing::debug_span!("fx_spawn", kind, effect = d.name.as_deref().unwrap_or(""))
                .entered();
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
                head,
                ..
            } => {
                // The exit wound's marks are not drawn (only its sound is played).
                if *exit {
                    return;
                }
                if *surface == SURFACE_FLESH {
                    self.flesh_hits += 1;
                }
                let t = weapon(*w).map_or(0, |d| d.impact_type);
                // `cg_bloodLimit`: a second blood effect inside the window is not played.
                if *surface == SURFACE_FLESH
                    && self.blood_on
                    && let Some(ms) = self.blood_limit
                {
                    if self.last_blood_ms >= 0 && self.clock - self.last_blood_ms < ms {
                        return;
                    }
                    self.last_blood_ms = self.clock;
                }
                let mut def = self.impact_effect(t, *surface, flesh_column(*head));
                // `CG_BulletHitEvent`: with blood off, a hit on a player plays the no-blood effect instead.
                let kind = match (*surface == SURFACE_FLESH, self.blood_on) {
                    (true, false) if def.is_some() => {
                        def = self.fx.library().get(NO_BLOOD_FLESH_HIT).cloned();
                        "bullet_impact_noblood"
                    }
                    (true, _) => "bullet_impact_flesh",
                    (false, _) => "bullet_impact",
                };
                self.play(kind, def, Vec3::from(*origin), Vec3::from(*normal));
            }
            ClientEvent::MissileBounce {
                origin,
                normal,
                surface,
                weapon: w,
                ..
            } => {
                if *surface == SURFACE_FLESH {
                    self.flesh_hits += 1;
                }
                let t = weapon(*w).map_or(0, |d| d.impact_type);
                let def = self.impact_effect(t, *surface, 0);
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
                persistent_smoke,
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
                    if *surface == SURFACE_FLESH {
                        self.flesh_hits += 1;
                    }
                    let def = self.impact_effect(d.impact_type, *surface, 0);
                    self.play("explosion_impact", def, Vec3::from(*origin), n);
                }
                // A persistent smoke's weapon FX is started from its original burst time
                // by the snapshot path, not again at the first rendered frame.
                if !*persistent_smoke {
                    let def = d.as_ref().and_then(|d| d.proj_explosion_effect.clone());
                    self.play("explosion", def, Vec3::from(*origin), n);
                }
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
                if !*settled && *surface == SURFACE_FLESH {
                    self.flesh_hits += 1;
                }
                // `EV_CHANGE_TO_DUD`: the table's first surface only.
                let table =
                    self.impact_effect(IMPACT_TYPE_DUD, if *settled { 0 } else { *surface }, 0);
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
                    if name.is_empty() {
                        logging::dedup!(DEBUG, "unnamed", "play_fx event without an effect name");
                    } else {
                        logging::dedup!(
                            WARN,
                            "effect-missing",
                            effect = name.as_str(),
                            "effect not in the library"
                        );
                    }
                    self.missing.insert(name.clone());
                }
            },
            ClientEvent::PlayFxOnTag {
                entity,
                model,
                name,
                tag,
            } => {
                self.script_bolts.push(ScriptBolt {
                    id: None,
                    entity: *entity,
                    model: *model,
                    same_model: false,
                    name: name.clone(),
                    tag: tag.clone(),
                    frame: None,
                    attempted: false,
                    teleport: None,
                    sample: None,
                    last_frame: None,
                    waiting_since: None,
                });
            }
            // A death plays no blood of its own in stock: the bullet impacts did (the event only pushes the body).
            ClientEvent::PlayerDeath { .. }
            | ClientEvent::Physics { .. }
            | ClientEvent::Earthquake { .. } => {}
            ClientEvent::WeaponFire {
                eye,
                angles,
                weapon: w,
                shooter,
                from,
                last_shot,
            } => {
                let Some(d) = weapon(*w) else {
                    logging::dedup!(
                        WARN,
                        "weapon_fire",
                        weapon = *w,
                        "fire event for a weapon with no data"
                    );
                    return;
                };
                // The first-person flash is the own gun's, or the turret's when the own player is on it
                // (`CG_FireWeapon`: `playerUsingTurret` picks `viewFlashEffect`).
                let mine = match *from {
                    Shooter::Player => *shooter == self.own,
                    Shooter::Vehicle => false,
                    Shooter::Turret { gunner, .. } => gunner == self.own,
                };
                let at_muzzle = from.at_muzzle();
                let (flash, eject) = if mine {
                    (&d.view_flash_effect, &d.view_shell_eject_effect)
                } else {
                    (&d.world_flash_effect, &d.world_shell_eject_effect)
                };
                // The round that empties the clip has its own shell, where the gun has one (`CG_EjectWeaponBrass`).
                let last = if mine {
                    &d.view_last_shot_eject_effect
                } else {
                    &d.world_last_shot_eject_effect
                };
                let both = d.view_last_shot_eject_effect.is_some()
                    && d.world_last_shot_eject_effect.is_some();
                let eject = last
                    .as_ref()
                    .filter(|_| *last_shot && both)
                    .or(eject.as_ref());
                // A bolt-action throws its shell when the bolt is worked (`ClientEvent::EjectBrass`), not with the shot;
                // `cg_brass` off throws none at all (`CG_EjectWeaponBrass`).
                let eject = eject.filter(|_| d.bolt_action == 0 && self.brass_on);
                // `CG_FireWeapon` -> `DrawBulletImpacts`: one shot in so many is a tracer, rolled here; where it ends is
                // found at the next update, against the map and the players as they are then.
                if *from != Shooter::Vehicle
                    && sim::weapon::WeaponType::from_raw(d.weap_type)
                        == sim::weapon::WeaponType::Bullet
                {
                    let chance = if mine {
                        self.tracer_cvars.own_chance
                    } else {
                        self.tracer_cvars.chance
                    };
                    if self.roll() < chance {
                        let (f, _, _) = sim::pm::math::angle_vectors(angles);
                        self.pending_tracers
                            .push((*shooter, Vec3::from(*eye), Vec3::from(f)));
                    }
                }
                let (flash_at, brass_at) = match (
                    self.tag_frame(*shooter, Tag::Flash),
                    self.tag_frame(*shooter, Tag::Brass),
                ) {
                    (None, None) if at_muzzle => {
                        // The event names the gun's muzzle, and a turret or vehicle throws no shells.
                        let (f, r, u) = sim::pm::math::angle_vectors(angles);
                        let flash = Frame {
                            origin: Vec3::from(*eye),
                            axis: [Vec3::from(f), -Vec3::from(r), Vec3::from(u)],
                        };
                        (Some(flash), None)
                    }
                    (None, None) => {
                        // A gun that is not drawn (out of view, or no model yet): put the muzzle where a rifle's would
                        // be.
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
                    tags => tags,
                };
                if let (Some(def), Some(at)) = (flash, flash_at) {
                    self.play_bolted("muzzle_flash", def, *shooter, Tag::Flash, at, !at_muzzle);
                }
                if let (Some(def), Some(at)) = (eject, brass_at) {
                    self.play_bolted("shell_eject", def, *shooter, Tag::Brass, at, !at_muzzle);
                }
            }
            ClientEvent::EjectBrass { weapon: w, shooter } => {
                let Some(d) = weapon(*w) else {
                    logging::dedup!(
                        WARN,
                        "eject_brass",
                        weapon = *w,
                        "brass event for a weapon with no data"
                    );
                    return;
                };
                let def = if *shooter == self.own {
                    &d.view_shell_eject_effect
                } else {
                    &d.world_shell_eject_effect
                };
                if let (true, Some(def), Some(at)) =
                    (self.brass_on, def, self.tag_frame(*shooter, Tag::Brass))
                {
                    self.play_bolted("shell_eject", def, *shooter, Tag::Brass, at, true);
                }
            }
            ClientEvent::MeleeHit {
                flesh,
                attacker,
                knife,
                ..
            } => {
                // `CG_MeleeBloodEvent`: the attacker sees the knife bloody (or, with `cg_blood` off, the no-blood
                // effect); nobody else's view shows it.
                let (name, kind) = if self.blood_on {
                    (KNIFE_BLOOD, "knife_blood")
                } else {
                    (KNIFE_NO_BLOOD, "knife_noblood")
                };
                if *flesh
                    && *knife
                    && *attacker == self.own
                    && let Some(def) = self.fx.library().get(name).cloned()
                    && let Some(at) = self.tag_frame(*attacker, Tag::Knife)
                {
                    self.play_bolted(kind, &def, *attacker, Tag::Knife, at, true);
                }
            }
        }
    }

    /// Where `tag` of `shooter`'s gun is: on the view model for the player whose eyes these are, on the third-person
    /// model for the others.
    fn tag_frame(&self, shooter: u16, tag: Tag) -> Option<Frame> {
        let t = if shooter == self.own {
            self.view.as_ref()
        } else {
            self.remote_tags.get(&shooter)
        }?;
        match tag {
            Tag::Flash => t.flash,
            Tag::Brass => t.brass,
            Tag::Knife => t.knife,
        }
    }

    /// Launches the tracers shots rolled: from the muzzle where the gun is drawn, else from 30 units ahead of the eye,
    /// to what the bullet meets or the end of its range (`DrawBulletImpacts`: 8192 units). The bullet's spread is not
    /// reproduced: the tracer flies along the aim.
    fn launch_tracers(&mut self, world: &dyn Collide) {
        for (shooter, eye, dir) in std::mem::take(&mut self.pending_tracers) {
            let far = eye + dir * TRACER_RANGE;
            let t = world.trace(
                eye.to_array(),
                far.to_array(),
                [0.0; 3],
                [0.0; 3],
                shooter,
                sim::contents::SOLID | sim::contents::CLIPSHOT | sim::contents::PLAYER,
            );
            let end = eye.lerp(far, t.fraction);
            let start = self
                .tag_frame(shooter, Tag::Flash)
                .map_or(eye + dir * 30.0, |f| f.origin);
            self.tracers.spawn(self.clock, start, end);
            *self.played.entry("tracer").or_default() += 1;
        }
    }

    /// Plays `def` at `at`, which is where `tag` of `shooter`'s gun is now; with `follow` it goes on following the tag
    /// until it ends (`CG_PlayBoltedEffect`).
    fn play_bolted(
        &mut self,
        kind: &'static str,
        def: &Arc<FxEffectDef>,
        shooter: u16,
        tag: Tag,
        at: Frame,
        follow: bool,
    ) {
        let d = self.resolve(def);
        let _span =
            tracing::debug_span!("fx_spawn", kind, effect = d.name.as_deref().unwrap_or(""))
                .entered();
        *self.played.entry(kind).or_default() += 1;
        if !follow {
            self.fx.play(&d, at);
            return;
        }
        let id = self.fx.play_at(&d, at, self.clock);
        self.bolted.push(Bolted { id, shooter, tag });
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

    /// Reconstructs active GENERAL smoke from the authoritative burst time. This also
    /// runs after an effects restart, without delivering an explosion event or sound.
    pub fn smoke(
        &mut self,
        ents: &[EntityState],
        weapon: &dyn Fn(u16) -> Option<Arc<WeaponDef>>,
        now: i32,
    ) {
        let started = smoke_clouds(
            &mut self.fx,
            &mut self.smoke,
            ents,
            &|w| {
                let weapon = weapon(w)?;
                Some((
                    weapon.proj_explosion_effect.clone()?,
                    weapon.proj_explosion_effect_force_normal_up != 0,
                ))
            },
            now,
        );
        if started != 0 {
            *self.played.entry("persistent_smoke").or_default() += started;
        }
    }

    pub fn smoke_report(&self) -> serde_json::Value {
        serde_json::json!(
            self.smoke
                .iter()
                .map(|(number, cloud)| {
                    serde_json::json!({
                        "number": number,
                        "start_time": cloud.start,
                        "end_time": cloud.end,
                        "age_ms": self.clock.wrapping_sub(cloud.start),
                        "live": self.fx.is_live(cloud.handle),
                    })
                })
                .collect::<Vec<_>>()
        )
    }

    pub fn smoke_quads(&self) -> usize {
        self.smoke_quads
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
        let Some(name) = names.fx_name(usize::from(index)) else {
            logging::dedup!(
                DEBUG,
                "unannounced",
                fx_index = index,
                "script effect name not announced"
            );
            return None;
        };
        match self.fx.library().get(name).cloned() {
            Some(d) => Some(self.resolve(&d)),
            None => {
                logging::dedup!(
                    WARN,
                    "script-effect-missing",
                    effect = name,
                    "script effect not in the library"
                );
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

    pub fn bolted_stats(&self) -> ScriptBoltStats {
        ScriptBoltStats {
            active: self.script_bolts.len(),
            ..self.bolt_stats
        }
    }

    pub fn has_script_bolt(&self, entity: u16) -> bool {
        self.script_bolts.iter().any(|b| b.entity == entity)
    }

    /// Begins a rendered pose frame; only tags actually resolved this frame may move an attachment. A bolt is judged
    /// only once [`Effects::script_model_tags`] poses its entity with the model the event came with: events are read
    /// from the newest snapshot, but entities are drawn `INTERP_DELAY_MS` behind it, so an entity spawned with its
    /// `playfxontag` (an airstrike's `tag_origin` model) is not drawn yet, and a number the event's snapshot gave to
    /// another entity (a killcam's replayed past against the live world, `CG_TransitionKillcam` resetting nothing on
    /// the way in) may still be drawn as that one. Stock reads the event as the snapshot becomes current, when the
    /// entity's DObj exists (`CG_PlayBoltedEffect`, cg_event.cpp:46-61).
    pub fn begin_script_tags(&mut self) {
        for b in &mut self.script_bolts {
            b.frame = None;
            b.attempted = false;
        }
    }

    /// Resolves requested tags against the exact posed model instances submitted to the renderer for `entity`, whose
    /// drawn state has model `model` (configstring index).
    pub fn script_model_tags(
        &mut self,
        entity: u16,
        model: u16,
        models: &[ModelInstance],
        content: &Content,
    ) {
        for bolt in self.script_bolts.iter_mut().filter(|b| b.entity == entity) {
            bolt.same_model = bolt.model == model;
            // Not started and drawn as another entity or model: it waits. A playing effect follows whatever the number
            // is drawn as and stops once the tag is gone (`FX_UpdateEffectBolt`).
            if bolt.id.is_none() && !bolt.same_model {
                continue;
            }
            bolt.attempted = true;
            bolt.frame = model_tag_frame(models, content, &bolt.tag);
        }
    }

    /// Applies the current animated world tags. As `FX_UpdateEffectBolt` (fx_update.cpp) does, an effect whose entity
    /// vanished, teleported or no longer resolves the tag (a new model) is stopped and unbolted (`FX_StopEffect`):
    /// it spawns nothing more and what it spawned plays out at the last frame.
    pub fn update_script_bolts(&mut self, entities: &[EntityState], now_ms: i32) {
        let mut bolts = std::mem::take(&mut self.script_bolts);
        bolts.retain_mut(|b| {
            let owner = entities.iter().find(|e| e.number == b.entity);
            let teleport = owner.map(|e| e.eflags & net::entity::TELEPORT_BIT);
            if owner.is_none() || b.teleport.is_some_and(|old| Some(old) != teleport) {
                if let Some(id) = b.id {
                    self.fx.stop(id);
                }
                self.bolt_stats.stopped += 1;
                return false;
            }
            b.teleport = teleport;
            if let Some(id) = b.id
                && !self.fx.is_live(id)
            {
                self.bolt_stats.expired += 1;
                return false;
            }
            if b.attempted && b.frame.is_none() {
                if let Some(id) = b.id {
                    // A tag that resolved before and no longer does: the number is drawn as another model now. A tag
                    // is missing only from the model the event named.
                    self.fx.stop(id);
                    self.bolt_stats.stopped += 1;
                    self.bolt_stats.missing_tags += u64::from(b.same_model);
                } else {
                    self.bolt_stats.missing_tags += 1;
                    logging::dedup!(
                        WARN,
                        "bolt-tag",
                        entity = b.entity,
                        tag = b.tag.as_str(),
                        "effect attachment tag not found"
                    );
                }
                return false;
            }
            if b.id.is_none() && b.frame.is_none() {
                // Still waiting for its entity to be drawn: an entity the client never poses gets no effect, which
                // is reported rather than left waiting.
                let since = *b.waiting_since.get_or_insert(now_ms);
                if now_ms.wrapping_sub(since) > BOLT_POSE_WAIT_MS {
                    self.bolt_stats.missing_tags += 1;
                    logging::dedup!(
                        WARN,
                        "bolt-unposed",
                        entity = b.entity,
                        etype = owner.map_or(0, |e| e.etype),
                        tag = b.tag.as_str(),
                        "effect attachment entity was never drawn"
                    );
                    return false;
                }
            }
            if let Some(frame) = b.frame {
                self.bolt_stats.resolved_frames += 1;
                let before = b.id.and_then(|id| self.fx.attachment_sample(id));
                if let Some(id) = b.id {
                    self.fx.move_bolted(id, frame);
                } else if let Some(def) = self.fx.library().get(&b.name).cloned() {
                    let def = self.resolve(&def);
                    b.id = Some(self.fx.play_at(&def, frame, now_ms));
                    *self.played.entry("play_fx_on_tag").or_default() += 1;
                } else {
                    self.missing.insert(b.name.clone());
                    logging::dedup!(
                        WARN,
                        "bolt-effect",
                        effect = b.name.as_str(),
                        "effect not in the library"
                    );
                    return false;
                }
                if let Some((elem, pos)) = b.id.and_then(|id| self.fx.attachment_sample(id)) {
                    if let (Some((old_elem, old_pos)), Some(old_frame)) = (before, b.last_frame)
                        && old_elem == elem
                    {
                        let local = old_pos - old_frame.origin;
                        let expected = frame.origin
                            + frame.axis[0] * local.dot(old_frame.axis[0])
                            + frame.axis[1] * local.dot(old_frame.axis[1])
                            + frame.axis[2] * local.dot(old_frame.axis[2]);
                        self.bolt_stats.max_follow_error =
                            self.bolt_stats.max_follow_error.max(pos.distance(expected));
                    }
                    if let Some((start, original_elem, original_frame, original_pos)) = b.sample {
                        if elem == original_elem && now_ms - start >= 500 {
                            self.bolt_stats.max_tag_displacement = self
                                .bolt_stats
                                .max_tag_displacement
                                .max(frame.origin.distance(original_frame.origin));
                            self.bolt_stats.max_element_displacement = self
                                .bolt_stats
                                .max_element_displacement
                                .max(pos.distance(original_pos));
                        }
                    } else {
                        b.sample = Some((now_ms, elem, frame, pos));
                    }
                }
                b.last_frame = Some(frame);
            }
            true
        });
        self.script_bolts = bolts;
    }

    /// Tells the effects where the guns of the other players are this frame, by client number: what their muzzle
    /// flashes and shells follow.
    pub fn set_remote_tags(&mut self, tags: HashMap<u16, ViewTags>) {
        self.remote_tags = tags;
    }

    /// What the player has set about tracers.
    pub fn set_tracer_cvars(&mut self, c: tracer::Cvars) {
        self.tracer_cvars = c;
    }

    /// `fx_marks`: off, bullet holes, blood and scorches are not drawn.
    pub fn set_marks(&mut self, on: bool) {
        self.marks_on = on;
    }

    /// `cg_blood` and `cg_brass`.
    pub fn set_blood_and_brass(&mut self, blood: bool, brass: bool) {
        self.blood_on = blood;
        self.brass_on = brass;
    }

    /// `cg_bloodLimit` (on, blood plays at most once every `ms`).
    pub fn set_blood_limit(&mut self, ms: Option<i32>) {
        self.blood_limit = ms;
    }
    /// A random number in [0, 1).
    fn roll(&mut self) -> f32 {
        self.dice ^= self.dice << 13;
        self.dice ^= self.dice >> 17;
        self.dice ^= self.dice << 5;
        (self.dice >> 8) as f32 / (1u32 << 24) as f32
    }

    /// The material tracers are drawn with (`gfx_tracer`).
    pub fn set_tracer_material(&mut self, material: Option<Arc<Material>>) {
        self.tracer_material = material;
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
        // What is bolted to a gun goes where the gun has gone; what has ended is forgotten.
        let mut bolted = std::mem::take(&mut self.bolted);
        bolted.retain(|b| {
            if !self.fx.is_live(b.id) {
                return false;
            }
            if let Some(at) = self.tag_frame(b.shooter, b.tag) {
                self.fx.move_bolted(b.id, at);
            }
            true
        });
        self.bolted = bolted;
        self.launch_tracers(world);
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

    pub fn ambient(&self) -> &Ambient {
        &self.ambient
    }

    pub fn ambient_live_effects(&self) -> usize {
        self.ambient.live_effects(&self.fx)
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
        let quads = &mut self.effect_quads;
        let ambient_quads = &mut self.ambient.quads;
        self.smoke_quads = 0;
        let smoke_quads = &mut self.smoke_quads;
        let smoke = &self.smoke;
        self.fx.draw_counted(&cam, &mut d, &mut |name, n| {
            if let Some(count) = quads.get_mut(name) {
                *count += n as u64;
            } else {
                quads.insert(name.to_owned(), n as u64);
            }
            if let Some(count) = ambient_quads.get_mut(name) {
                *count += n as u64;
            }
            if smoke
                .values()
                .any(|cloud| cloud.def.name.as_deref() == Some(name))
            {
                *smoke_quads += n;
            }
        });
        let beams = self.tracers.beams(self.clock);
        let mut out = Drawn {
            tracers: beams.len(),
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
        if let Some(m) = &self.tracer_material
            && let Some(mesh) = tracer::beam_mesh(m, &beams, eye, &self.tracer_cvars)
        {
            out.meshes.push(mesh);
        }

        let mut seen = HashSet::new();
        let marks_on = self.marks_on;
        for dc in d.decals.iter().filter(|_| marks_on) {
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

#[cfg(test)]
mod flesh_tests {
    #[test]
    fn a_head_hit_uses_the_head_column() {
        assert_eq!(
            [super::flesh_column(false), super::flesh_column(true)],
            [0, 2]
        );
    }
}

#[cfg(test)]
mod smoke_tests {
    use super::*;
    use assets::zone::fx::{FxElemDef, FxVisuals, Range};

    fn probe() -> Arc<FxEffectDef> {
        let zero = Range {
            base: 0.0,
            amplitude: 0.0,
        };
        let elem = FxElemDef {
            flags: 0,
            spawn: [1, 0],
            spawn_range: zero,
            fade_in_range: zero,
            fade_out_range: zero,
            spawn_frustum_cull_radius: 0.0,
            spawn_delay_msec: Range {
                base: 0,
                amplitude: 0,
            },
            life_span_msec: Range {
                base: 1_000,
                amplitude: 0,
            },
            spawn_origin: [zero; 3],
            spawn_offset_radius: zero,
            spawn_offset_height: zero,
            spawn_angles: [zero; 3],
            angular_velocity: [zero; 3],
            initial_rotation: zero,
            gravity: zero,
            reflection_factor: zero,
            atlas: [0; 6],
            atlas_entry_count: 0,
            elem_type: assets::zone::fx::elem::OMNI_LIGHT,
            vel_samples: Arc::from(Vec::new()),
            vis_samples: Arc::from(Vec::new()),
            visuals: FxVisuals::None,
            coll_mins: [0.0; 3],
            coll_maxs: [0.0; 3],
            effect_on_impact: None,
            effect_on_death: None,
            effect_emitted: None,
            emit_dist: zero,
            emit_dist_variance: zero,
            trail: None,
            sort_order: 0,
            lighting_frac: 0,
            use_item_clip: false,
        };
        Arc::new(FxEffectDef {
            name: Some("smoke-age-probe".into()),
            flags: 0,
            total_size: 0,
            msec_looping_life: 0,
            looping_count: 0,
            one_shot_count: 1,
            emission_count: 0,
            elems: Arc::from(vec![elem]),
        })
    }

    fn smoke(start: i32) -> EntityState {
        let mut e = EntityState::new(42);
        e.etype = etype::GENERAL;
        e.eflags = net::entity::SMOKE_EXPLODE;
        e.custom_explode_start_time = start;
        e.time2 = start + 61_000;
        e
    }

    #[test]
    fn a_late_or_restarted_smoke_uses_original_effect_age_not_join_age() {
        let def = probe();
        let effect = |_: u16| Some((def.clone(), false));
        let e = smoke(1_000);
        for now in [1_000, 1_200, 1_500, 2_001, 10_000] {
            // Each iteration also represents rebuilding the FX pool on restart.
            let mut fx = Fx::new(Arc::new(Library::default()));
            let mut clouds = HashMap::new();
            assert_eq!(
                smoke_clouds(&mut fx, &mut clouds, std::slice::from_ref(&e), &effect, now),
                1
            );
            fx.update(now, &fx::Empty);
            assert_eq!(clouds[&42].start, 1_000);
            // A probe with a 1000 ms life must already be over on a late join.
            // Starting at `now` instead would incorrectly leave one element alive.
            assert_eq!(
                fx.live_elems(),
                usize::from(now < 2_000),
                "first seen {now}"
            );
            assert!(fx.take_sounds().is_empty());
        }
    }

    #[test]
    fn snapshot_reentry_keeps_the_existing_cloud_and_reuse_starts_a_new_one() {
        let def = probe();
        let effect = |_: u16| Some((def.clone(), false));
        let mut fx = Fx::new(Arc::new(Library::default()));
        let mut clouds = HashMap::new();
        let e = smoke(1_000);
        assert_eq!(
            smoke_clouds(
                &mut fx,
                &mut clouds,
                std::slice::from_ref(&e),
                &effect,
                1_000
            ),
            1
        );
        let handle = clouds[&42].handle;
        assert_eq!(smoke_clouds(&mut fx, &mut clouds, &[], &effect, 1_100), 0);
        assert_eq!(smoke_clouds(&mut fx, &mut clouds, &[e], &effect, 1_300), 0);
        assert_eq!(clouds[&42].handle, handle);
        assert_eq!(
            smoke_clouds(&mut fx, &mut clouds, &[smoke(2_000)], &effect, 2_000),
            1
        );
        assert_ne!(clouds[&42].handle, handle);
        assert_eq!(clouds[&42].start, 2_000);
    }

    #[test]
    fn smoke_reconstruction_obeys_both_ends_of_the_server_interval() {
        let def = probe();
        let effect = |_: u16| Some((def.clone(), false));
        for (now, expected) in [(999, 0), (1_000, 1), (62_000, 1), (62_001, 0)] {
            let mut fx = Fx::new(Arc::new(Library::default()));
            let mut clouds = HashMap::new();
            assert_eq!(
                smoke_clouds(&mut fx, &mut clouds, &[smoke(1_000)], &effect, now),
                expected
            );
        }
    }
}
