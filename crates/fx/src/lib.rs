// SPDX-License-Identifier: GPL-3.0-only
//! Effect playback. An [`Fx`] runs [`FxEffectDef`]s: each effect spawns its elements (looping ones at an interval,
//! one-shot ones at once, runners that start further effects), and every element lives for its lifespan moving by
//! its velocity samples and gravity, bouncing off the world when it collides, and reads its colour, size and
//! rotation from its visual samples. [`Fx::draw`] turns the live elements into what the renderer draws and the mixer
//! plays.
//!
//! The simulation integrates in fixed steps from the element's spawn so the result depends on the effect's age and
//! not on the frame rate.

use assets::zone::fx::{FxEffectDef, FxElemDef, FxVisuals, elem, flags};
use assets::zone::gfx::Material;
use assets::zone::xmodel::XModel;
use glam::Vec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod rigid;
mod rng;
pub use rigid::{Body, GRAVITY as PHYS_GRAVITY, Mass, Shape};
pub use rng::Rng;

/// Longest single integration step.
const STEP_MS: i32 = 16;
/// Effects alive at once; older ones are dropped beyond this.
const MAX_EFFECTS: usize = 1024;
/// Elements alive at once over all effects.
const MAX_ELEMS: usize = 6000;
/// Gravity of a `gravity` factor of one, in units per second squared.
const GRAVITY: f32 = 800.0;
const VELOCITY_SCALE: f32 = 1000.0;

/// What an element asks of the world: the first thing a moving box hits.
pub trait World {
    /// A trace of a box from `a` to `b`: the fraction travelled and the surface normal, `None` for a clear path.
    fn trace(&self, a: Vec3, b: Vec3, mins: Vec3, maxs: Vec3) -> Option<(f32, Vec3)>;
}

/// A world with nothing in it.
pub struct Empty;

impl World for Empty {
    fn trace(&self, _: Vec3, _: Vec3, _: Vec3, _: Vec3) -> Option<(f32, Vec3)> {
        None
    }
}

/// Effects by name, and the impact tables.
#[derive(Default)]
pub struct Library {
    effects: HashMap<String, Arc<FxEffectDef>>,
}

impl Library {
    pub fn add(&mut self, def: Arc<FxEffectDef>) {
        if let Some(n) = &def.name {
            self.effects.insert(n.to_ascii_lowercase(), def);
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<FxEffectDef>> {
        self.effects.get(&name.to_ascii_lowercase())
    }

    /// The names of every effect, lowercase.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.effects.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.effects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }
}

/// Where an effect is and which way it faces: `axis[0]` is forward (the surface normal for an impact).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub origin: Vec3,
    pub axis: [Vec3; 3],
}

impl Frame {
    /// A frame facing `forward`, with the other two axes any right-handed pair.
    pub fn facing(origin: Vec3, forward: Vec3) -> Frame {
        let f = forward.try_normalize().unwrap_or(Vec3::Z);
        Frame {
            origin,
            axis: basis(f),
        }
    }
}

/// A right-handed basis whose first axis is `f`.
pub fn basis(f: Vec3) -> [Vec3; 3] {
    let up = if f.z.abs() < 0.999 { Vec3::Z } else { Vec3::Y };
    let right = up.cross(f).normalize();
    [f, right, f.cross(right)]
}

/// The camera, for billboards.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub origin: Vec3,
    /// Forward, left, up.
    pub axis: [Vec3; 3],
}

/// One camera-facing or oriented quad.
#[derive(Clone)]
pub struct Quad {
    pub material: Arc<Material>,
    /// Corners in the order the renderer's index pattern wants: top left, bottom left, bottom right, top right.
    pub corners: [Vec3; 4],
    pub uv: [[f32; 2]; 4],
    /// RGBA.
    pub color: [u8; 4],
    pub normal: Vec3,
    pub tangent: Vec3,
    /// Distance to the camera, to sort far to near.
    pub depth: f32,
    pub sort_order: u8,
}

/// One vertex of a trail ribbon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailVertex {
    pub pos: Vec3,
    pub normal: Vec3,
    pub tangent: Vec3,
    pub uv: [f32; 2],
    /// RGBA.
    pub color: [u8; 4],
}

/// A trail: the strip a moving effect leaves behind it, as indexed triangles.
#[derive(Clone)]
pub struct Trail {
    pub material: Arc<Material>,
    pub verts: Vec<TrailVertex>,
    /// Three per triangle, into `verts`.
    pub indices: Vec<u32>,
    /// Distance of the strip's newest point to the camera, to sort far to near.
    pub depth: f32,
    pub sort_order: u8,
}

/// A mark on the world's surfaces.
#[derive(Clone)]
pub struct Decal {
    /// Stable for the life of the element, so the renderer can keep the geometry it cut.
    pub id: u64,
    pub material: Arc<Material>,
    pub origin: Vec3,
    pub normal: Vec3,
    /// Texture up.
    pub up: Vec3,
    pub half_size: [f32; 2],
    pub color: [u8; 4],
}

/// A model element.
#[derive(Clone)]
pub struct ModelDraw {
    pub model: Arc<XModel>,
    pub origin: Vec3,
    /// The element's axes.
    pub axis: [Vec3; 3],
    pub scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoundPlay {
    pub alias: String,
    pub origin: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub origin: Vec3,
    pub color: [u8; 3],
    pub radius: f32,
}

/// What [`Fx::draw`] produces for one frame.
#[derive(Default)]
pub struct Draws {
    pub quads: Vec<Quad>,
    pub decals: Vec<Decal>,
    pub models: Vec<ModelDraw>,
    pub lights: Vec<Light>,
    pub trails: Vec<Trail>,
}

/// Counters of a run.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub effects_played: u64,
    pub elems_spawned: u64,
    pub impacts: u64,
    pub dropped: u64,
}

/// What makes an element one point of a trail: the effect travelled `dist` units when it spawned, and the effect's
/// right and up axes then span the plane of the ribbon's cross-section.
#[derive(Clone, Copy)]
struct TrailPoint {
    seq: u32,
    dist: f32,
    right: Vec3,
    up: Vec3,
}

struct Elem {
    /// Index into the effect definition's elements.
    def: usize,
    r: [f32; 24],
    begin: i32,
    life: f32,
    /// World position now, and its velocity not counting the element's own samples.
    pos: Vec3,
    base_vel: Vec3,
    orient: [Vec3; 3],
    /// Simulated up to this time.
    at: i32,
    done: bool,
    at_rest: bool,
    started: bool,
    emit_left: f32,
    id: u64,
    trail: Option<TrailPoint>,
    /// The rigid body of a model element with `USE_MODEL_PHYSICS`, once it has started.
    body: Option<Body>,
}

struct Effect {
    def: Arc<FxEffectDef>,
    frame: Frame,
    elems: Vec<Elem>,
    /// Next spawn time of each looping element.
    next_loop: Vec<i32>,
    loop_end: i32,
    id: u64,
    /// Where the owner moved the effect to since the last update.
    goal: Option<Frame>,
    /// Distance the effect has travelled, and the time of its last move.
    dist: f32,
    last_move: i32,
    /// Elements spawned so far, per element definition (looping ones stop at their count).
    spawned: Vec<u32>,
}

pub struct Fx {
    lib: Arc<Library>,
    effects: Vec<Effect>,
    /// Handles of the effects that exist, for [`Fx::is_live`]; rebuilt by every update.
    live_ids: HashSet<u64>,
    now: i32,
    rng: Rng,
    next_id: u64,
    live_elems: usize,
    pub stats: Stats,
    /// Sounds started since the last [`Fx::take_sounds`].
    sounds: Vec<SoundPlay>,
}

fn pick(r: f32, range: &assets::zone::fx::Range<f32>) -> f32 {
    range.base + range.amplitude * r
}

impl Fx {
    pub fn new(lib: Arc<Library>) -> Fx {
        Fx {
            lib,
            effects: Vec::new(),
            live_ids: HashSet::new(),
            now: 0,
            rng: Rng::new(0x5EED),
            next_id: 1,
            live_elems: 0,
            stats: Stats::default(),
            sounds: Vec::new(),
        }
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.lib
    }

    /// Effects alive.
    pub fn live_effects(&self) -> usize {
        self.effects.len()
    }

    pub fn live_elems(&self) -> usize {
        self.live_elems
    }

    pub fn take_sounds(&mut self) -> Vec<SoundPlay> {
        std::mem::take(&mut self.sounds)
    }

    /// Plays `def` at `frame`, starting `at` the given time (milliseconds on the effect clock).
    pub fn play_at(&mut self, def: &Arc<FxEffectDef>, frame: Frame, at: i32) -> u64 {
        if self.effects.len() >= MAX_EFFECTS {
            let old = self.effects.remove(0);
            self.live_ids.remove(&old.id);
            self.stats.dropped += 1;
        }
        self.stats.effects_played += 1;
        let mut e = Effect {
            def: def.clone(),
            frame,
            elems: Vec::new(),
            next_loop: Vec::new(),
            loop_end: at,
            id: self.next_id,
            goal: None,
            dist: 0.0,
            last_move: at,
            spawned: Vec::new(),
        };
        self.next_id += 1;
        self.begin(&mut e, at);
        let id = e.id;
        self.live_ids.insert(id);
        self.effects.push(e);
        id
    }

    /// Starts (or restarts) the effect's timeline at `at`: its looping elements begin their intervals, its one-shot
    /// elements spawn, and its trails take their first point.
    fn begin(&mut self, e: &mut Effect, at: i32) {
        let def = e.def.clone();
        let def = &def;
        let looping = def.looping_count as usize;
        let one_shot = def.one_shot_count as usize;
        e.loop_end = if def.msec_looping_life > 0 {
            at.saturating_add(def.msec_looping_life)
        } else {
            at
        };
        e.next_loop = vec![at; looping];
        e.spawned = vec![0; def.elems.len()];
        e.dist = 0.0;
        e.last_move = at;
        for k in 0..def.elems.len() {
            if is_trail(&def.elems[k]) {
                // Trails spawn by distance travelled, not on an interval.
                if let Some(n) = e.next_loop.get_mut(k) {
                    *n = i32::MAX;
                }
                self.spawn_trail_point(e, k, at, 0.0);
            }
        }
        for k in looping..(looping + one_shot).min(def.elems.len()) {
            let d = &def.elems[k];
            if is_trail(d) {
                continue;
            }
            let count = d.spawn[0] as f32 + d.spawn[1] as f32 * self.rng.f();
            for _ in 0..(count as i32).max(0) {
                self.spawn(e, k, at);
            }
        }
    }

    /// Whether effect `id` still exists (it ended, or the pool dropped it).
    pub fn is_live(&self, id: u64) -> bool {
        self.live_ids.contains(&id)
    }

    /// Restarts effect `id` at `at` (`FX_RetriggerEffect`): its looping elements start over and its one-shot elements
    /// spawn again, without a second effect. Returns false if the effect is over.
    pub fn retrigger(&mut self, id: u64, at: i32) -> bool {
        let Some(i) = self.effects.iter().position(|e| e.id == id) else {
            return false;
        };
        let mut e = self.effects.swap_remove(i);
        let attached = e.loop_end == i32::MAX;
        self.begin(&mut e, at);
        if attached {
            e.loop_end = i32::MAX;
        }
        self.effects.push(e);
        true
    }

    /// Plays `def` at `frame` as an effect that goes on until [`Fx::stop`] and follows [`Fx::move_effect`]: a missile's
    /// smoke trail. Returns its handle.
    pub fn play_attached(&mut self, def: &Arc<FxEffectDef>, frame: Frame) -> u64 {
        let id = self.play_at(def, frame, self.now);
        if let Some(e) = self.effects.iter_mut().find(|e| e.id == id) {
            e.loop_end = i32::MAX;
        }
        id
    }

    /// Moves a playing effect to `frame`; it spawns what its movement calls for (trail points along the way) at the
    /// next update. Does nothing if the effect is over.
    pub fn move_effect(&mut self, id: u64, frame: Frame) {
        if let Some(e) = self.effects.iter_mut().find(|e| e.id == id) {
            e.goal = Some(frame);
        }
    }

    /// Ends the looping of an effect started with [`Fx::play_attached`]; what it already spawned plays out.
    pub fn stop(&mut self, id: u64) {
        if let Some(e) = self.effects.iter_mut().find(|e| e.id == id) {
            e.loop_end = e.loop_end.min(self.now);
        }
    }

    pub fn play(&mut self, def: &Arc<FxEffectDef>, frame: Frame) {
        self.play_at(def, frame, self.now);
    }

    /// Plays the effect called `name`, if the library has it.
    pub fn play_named(&mut self, name: &str, frame: Frame) -> bool {
        match self.lib.get(name).cloned() {
            Some(d) => {
                self.play(&d, frame);
                true
            }
            None => false,
        }
    }

    fn spawn(&mut self, e: &mut Effect, k: usize, time: i32) {
        if self.live_elems >= MAX_ELEMS {
            self.stats.dropped += 1;
            return;
        }
        let d = &e.def.elems[k];
        let mut r = [0.0f32; 24];
        for v in &mut r {
            *v = self.rng.f();
        }
        let delay = d.spawn_delay_msec.base as f32 + d.spawn_delay_msec.amplitude as f32 * r[17];
        let begin = time + delay.max(0.0) as i32;
        let life =
            (d.life_span_msec.base as f32 + d.life_span_msec.amplitude as f32 * r[18]).max(1.0);
        // The spawn point.
        let f = &e.frame;
        let (pos, offset) = spawn_origin(d, &r, f);
        let orient = match d.flags & flags::RUN_MASK {
            0 => [Vec3::X, Vec3::Y, Vec3::Z],
            flags::RUN_RELATIVE_TO_OFFSET => {
                let dir = offset.try_normalize().unwrap_or(Vec3::X);
                basis(dir)
            }
            _ => f.axis,
        };
        let id = self.next_id;
        self.next_id += 1;
        self.live_elems += 1;
        self.stats.elems_spawned += 1;
        e.elems.push(Elem {
            def: k,
            r,
            begin,
            life,
            pos,
            base_vel: Vec3::ZERO,
            orient,
            at: begin,
            done: false,
            at_rest: false,
            started: false,
            emit_left: d.emit_dist.base + d.emit_dist.amplitude * r[19],
            id,
            trail: None,
            body: None,
        });
    }

    /// Spawns the trail point of element `k` that the effect leaves at `dist` units of travel, from the effect's
    /// frame now.
    fn spawn_trail_point(&mut self, e: &mut Effect, k: usize, time: i32, dist: f32) {
        let before = e.elems.len();
        self.spawn(e, k, time);
        if e.elems.len() == before {
            return;
        }
        let seq = e.spawned[k];
        e.spawned[k] += 1;
        let [_, right, up] = e.frame.axis;
        e.elems[before].trail = Some(TrailPoint {
            seq,
            dist,
            right,
            up,
        });
    }

    /// Carries the effect to `goal`: a trail point for every split distance crossed on the way, and the newest point
    /// of each trail follows the effect.
    fn move_trails(&mut self, e: &mut Effect, goal: Frame, now: i32) {
        let from = e.frame;
        let moved = from.origin.distance(goal.origin);
        let (t0, d0) = (e.last_move, e.dist);
        let def = e.def.clone();
        for (k, d) in def.elems.iter().enumerate() {
            let Some(trail) = d.trail.as_ref().filter(|_| is_trail(d)) else {
                continue;
            };
            let split = trail.split_dist.max(1) as f32;
            let mut n = (d0 / split).floor() + 1.0;
            while moved > 0.0 && n * split <= d0 + moved {
                let lerp = (n * split - d0) / moved;
                e.frame = lerp_frame(&from, &goal, lerp);
                let time = t0 + ((now - t0) as f32 * lerp) as i32;
                self.spawn_trail_point(e, k, time, n * split);
                n += 1.0;
            }
        }
        e.frame = goal;
        e.dist = d0 + moved;
        e.last_move = now;
        for (k, d) in def.elems.iter().enumerate() {
            if !is_trail(d) {
                continue;
            }
            if let Some(el) = e
                .elems
                .iter_mut()
                .rev()
                .find(|x| x.def == k && x.trail.is_some())
            {
                el.pos = spawn_origin(d, &el.r, &goal).0;
                if let Some(t) = &mut el.trail {
                    t.dist = e.dist;
                    t.right = goal.axis[1];
                    t.up = goal.axis[2];
                }
            }
        }
    }

    /// Advances every effect to `now_ms`.
    pub fn update(&mut self, now_ms: i32, world: &dyn World) {
        self.now = now_ms;
        let mut effects = std::mem::take(&mut self.effects);
        let mut spawned: Vec<(Arc<FxEffectDef>, Frame, i32)> = Vec::new();
        for e in &mut effects {
            self.update_effect(e, now_ms, world, &mut spawned);
        }
        // Effects started by this update run too, from the moment they were started.
        let mut rounds = 0;
        while !spawned.is_empty() && rounds < 4 {
            rounds += 1;
            let batch = std::mem::take(&mut spawned);
            for (def, frame, at) in batch {
                self.effects = Vec::new();
                self.play_at(&def, frame, at);
                let mut ne = self.effects.pop().expect("just played");
                self.update_effect(&mut ne, now_ms, world, &mut spawned);
                effects.push(ne);
            }
        }
        // Drop what is over.
        effects.retain(|e| {
            let looping_over = e.next_loop.iter().all(|&n| n > e.loop_end);
            !(looping_over && e.elems.iter().all(|x| x.done))
        });
        for e in &mut effects {
            // A trail keeps the last of its expired points: the strip is cut off between it and the next.
            let mut next_done: Vec<bool> = vec![true; e.def.elems.len()];
            let mut keep = vec![true; e.elems.len()];
            for (i, x) in e.elems.iter().enumerate().rev() {
                keep[i] = !x.done || (x.trail.is_some() && !next_done[x.def]);
                if x.trail.is_some() {
                    next_done[x.def] = x.done;
                }
            }
            let mut i = 0;
            e.elems.retain(|_| {
                i += 1;
                keep[i - 1]
            });
        }
        self.live_elems = effects.iter().map(|e| e.elems.len()).sum();
        self.live_ids.clear();
        self.live_ids.extend(effects.iter().map(|e| e.id));
        self.effects = effects;
    }

    fn update_effect(
        &mut self,
        e: &mut Effect,
        now: i32,
        world: &dyn World,
        spawned: &mut Vec<(Arc<FxEffectDef>, Frame, i32)>,
    ) {
        if let Some(goal) = e.goal.take() {
            self.move_trails(e, goal, now);
        }
        // Looping elements spawn on their interval until the effect's looping life is over.
        let looping = e.next_loop.len();
        for k in 0..looping {
            let d = e.def.clone();
            let d = &d.elems[k];
            if is_trail(d) {
                continue;
            }
            let interval = d.spawn[0].max(1);
            // A looping element spawns one at every interval, `count` in all; only the unlimited ones stop with the
            // effect's looping life.
            let limited = d.spawn[1] != i32::MAX;
            let until = if limited { i32::MAX } else { e.loop_end };
            while e.next_loop[k] <= now && e.next_loop[k] <= until {
                let t = e.next_loop[k];
                self.spawn(e, k, t);
                e.spawned[k] += 1;
                e.next_loop[k] = if limited && e.spawned[k] as i64 >= i64::from(d.spawn[1]) {
                    i32::MAX
                } else {
                    t + interval
                };
            }
        }
        let def = e.def.clone();
        let frame = e.frame;
        let mut i = 0;
        while i < e.elems.len() {
            let el = &mut e.elems[i];
            let d = &def.elems[el.def];
            self.update_elem(el, d, &frame, now, world, spawned);
            i += 1;
        }
    }

    fn update_elem(
        &mut self,
        el: &mut Elem,
        d: &FxElemDef,
        frame: &Frame,
        now: i32,
        world: &dyn World,
        spawned: &mut Vec<(Arc<FxEffectDef>, Frame, i32)>,
    ) {
        if el.done || now < el.begin {
            return;
        }
        if !el.started {
            el.started = true;
            self.on_start(el, d, frame, spawned);
        }
        let end = el.begin + el.life as i32;
        let until = now.min(end);
        while el.at < until && !el.at_rest && !el.done {
            let step = (until - el.at).min(STEP_MS);
            self.step(el, d, step, world, spawned);
            el.at += step;
        }
        if now >= end {
            if let Some(name) = d.effect_on_death.as_deref()
                && !el.done
                && let Some(def) = self.lib.get(name)
            {
                spawned.push((
                    def.clone(),
                    Frame {
                        origin: el.pos,
                        ..*frame
                    },
                    end,
                ));
            }
            el.done = true;
        }
    }

    /// What an element does the moment it appears.
    fn on_start(
        &mut self,
        el: &mut Elem,
        d: &FxElemDef,
        frame: &Frame,
        spawned: &mut Vec<(Arc<FxEffectDef>, Frame, i32)>,
    ) {
        match (&d.visuals, d.elem_type) {
            (FxVisuals::Effects(names), elem::RUNNER) if !names.is_empty() => {
                let n = ((el.r[16] * names.len() as f32) as usize).min(names.len() - 1);
                if let Some(name) = names[n].as_deref()
                    && let Some(def) = self.lib.get(name)
                {
                    spawned.push((
                        def.clone(),
                        Frame {
                            origin: el.pos,
                            axis: frame.axis,
                        },
                        el.begin,
                    ));
                }
                el.done = true;
            }
            (FxVisuals::Sounds(names), elem::SOUND) if !names.is_empty() => {
                let n = ((el.r[16] * names.len() as f32) as usize).min(names.len() - 1);
                if let Some(name) = names[n].as_deref() {
                    self.sounds.push(SoundPlay {
                        alias: name.to_owned(),
                        origin: el.pos,
                    });
                }
                el.done = true;
            }
            (FxVisuals::Models(models), elem::MODEL) if d.flags & flags::USE_MODEL_PHYSICS != 0 => {
                // The element is a rigid body with its model's PhysPreset, spinning as the element asks; one whose
                // model has no preset cannot be simulated and is dropped, as in the original.
                let preset = models
                    .get(pick_index(el, models.len()))
                    .and_then(|m| m.as_ref())
                    .and_then(|m| m.phys_preset.as_ref().map(|p| (m, p)));
                let Some((model, preset)) = preset else {
                    el.done = true;
                    return;
                };
                let spin = |k: usize| pick(el.r[3 + k], &d.angular_velocity[k]) * VELOCITY_SCALE;
                el.body = Some(
                    Body::new(
                        preset,
                        &Shape::of_model(model),
                        el.pos,
                        element_axis(d, el, 0.0),
                        velocity(d, el, 0.0),
                    )
                    // The original's `Phys_ObjSetAngularVelocity` takes (pitch, yaw, roll) rates as the world's
                    // (y, z, x) rotation rates.
                    .spinning(Vec3::new(spin(2), spin(0), spin(1))),
                );
            }
            _ => {}
        }
    }

    /// One integration step of `ms` milliseconds.
    fn step(
        &mut self,
        el: &mut Elem,
        d: &FxElemDef,
        ms: i32,
        world: &dyn World,
        spawned: &mut Vec<(Arc<FxEffectDef>, Frame, i32)>,
    ) {
        let dt = ms as f32 * 0.001;
        if let Some(b) = &mut el.body {
            b.step(dt, world);
            el.pos = b.origin();
            el.at_rest = b.asleep();
            return;
        }
        let t = ((el.at - el.begin) as f32 / el.life).clamp(0.0, 0.999_999);
        let v = velocity(d, el, t) + el.base_vel;
        let from = el.pos;
        let mut to = from + v * dt;
        let g = pick(el.r[15], &d.gravity) * GRAVITY * dt;
        to.z -= g * dt * 0.5;
        el.base_vel.z -= g;
        if d.flags & flags::USE_COLLISION != 0
            && let Some((frac, normal)) =
                world.trace(from, to, Vec3::from(d.coll_mins), Vec3::from(d.coll_maxs))
        {
            self.stats.impacts += 1;
            el.pos = from.lerp(to, frac);
            let impact = d
                .effect_on_impact
                .as_deref()
                .and_then(|n| self.lib.get(n))
                .cloned();
            let at = el.at + (ms as f32 * frac) as i32;
            if d.flags & flags::DIE_ON_TOUCH != 0 {
                if let Some(def) = impact {
                    spawned.push((def, Frame::facing(el.pos, normal), at));
                }
                el.done = true;
                return;
            }
            let pre = v;
            let reflect = pick(el.r[16], &d.reflection_factor);
            let scaled = pre * reflect;
            if let Some(def) = impact
                && pre.length_squared() > 1.0
            {
                spawned.push((def, Frame::facing(el.pos, normal), at));
            }
            if scaled.length_squared() <= 1.0 && normal.z > 0.7 {
                el.at_rest = true;
                return;
            }
            let along = scaled.dot(normal);
            let post = scaled - normal * (2.0 * along);
            el.base_vel += post - pre;
            return;
        }
        // Emitters drop an effect every so often along their path.
        if let Some(name) = d.effect_emitted.as_deref()
            && (d.emit_dist.base > 0.0 || d.emit_dist.amplitude > 0.0)
        {
            el.emit_left -= from.distance(to);
            if el.emit_left <= 0.0
                && let Some(def) = self.lib.get(name)
            {
                spawned.push((
                    def.clone(),
                    Frame::facing(to, v.try_normalize().unwrap_or(Vec3::Z)),
                    el.at + ms,
                ));
                el.emit_left = d.emit_dist.base
                    + d.emit_dist.amplitude * self.rng.f()
                    + d.emit_dist_variance.base * self.rng.f();
                el.emit_left = el.emit_left.max(1.0);
            }
        }
        el.pos = to;
    }

    /// Everything to draw at the current time.
    pub fn draw(&self, cam: &Camera, out: &mut Draws) {
        for e in &self.effects {
            self.draw_trails(e, cam, out);
            for el in &e.elems {
                if el.done || el.trail.is_some() || !el.started || self.now < el.begin {
                    continue;
                }
                let d = &e.def.elems[el.def];
                let t = ((self.now - el.begin) as f32 / el.life).clamp(0.0, 0.999_999);
                let vis = visual_state(d, el, t);
                let sort_order = d.sort_order;
                match (&d.visuals, d.elem_type) {
                    (
                        FxVisuals::Materials(mats),
                        elem::SPRITE_BILLBOARD | elem::SPRITE_ORIENTED | elem::TAIL,
                    ) => {
                        let Some(Some(material)) = mats.get(pick_index(el, mats.len())) else {
                            continue;
                        };
                        if vis.color[3] == 0 {
                            continue;
                        }
                        let (tangent, up, normal) = match d.elem_type {
                            elem::SPRITE_BILLBOARD => (-cam.axis[1], cam.axis[2], -cam.axis[0]),
                            elem::SPRITE_ORIENTED => (el.orient[1], el.orient[2], el.orient[0]),
                            _ => {
                                let vdir = (velocity(d, el, t) + el.base_vel)
                                    .try_normalize()
                                    .unwrap_or(Vec3::Z);
                                let tail_start = el.pos - vdir * vis.size[1];
                                let to_cam = cam.origin - tail_start;
                                let tangent = vdir.cross(to_cam).try_normalize().unwrap_or(Vec3::X);
                                let normal = tangent.cross(vdir);
                                let pos = tail_start;
                                let q = quad(
                                    material, pos, tangent, vdir, normal, &vis, d, el, cam, t,
                                    self.now, sort_order,
                                );
                                out.quads.push(q);
                                continue;
                            }
                        };
                        out.quads.push(quad(
                            material, el.pos, tangent, up, normal, &vis, d, el, cam, t, self.now,
                            sort_order,
                        ));
                    }
                    (FxVisuals::Models(models), elem::MODEL) => {
                        let Some(Some(model)) = models.get(pick_index(el, models.len())) else {
                            continue;
                        };
                        let scale = vis.scale;
                        if scale == 0.0 {
                            continue;
                        }
                        let (origin, axis) = match &el.body {
                            Some(b) => (b.origin(), b.axes()),
                            None => (el.pos, element_axis(d, el, (self.now - el.begin) as f32)),
                        };
                        out.models.push(ModelDraw {
                            model: model.clone(),
                            origin,
                            axis,
                            scale,
                        });
                    }
                    (FxVisuals::Decals(mats), elem::DECAL) => {
                        let Some([Some(material), _]) = mats.get(pick_index(el, mats.len())) else {
                            continue;
                        };
                        let rot = vis.rotation;
                        let (s, c) = rot.sin_cos();
                        let n = e.frame.axis[0];
                        let [_, a1, a2] = basis(n);
                        out.decals.push(Decal {
                            id: el.id,
                            material: material.clone(),
                            origin: el.pos,
                            normal: n,
                            up: a1 * s + a2 * c,
                            half_size: vis.size,
                            color: vis.color,
                        });
                    }
                    (_, elem::OMNI_LIGHT) => {
                        out.lights.push(Light {
                            origin: el.pos,
                            color: [vis.color[0], vis.color[1], vis.color[2]],
                            radius: vis.size[0],
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Where an element of `d` with random draws `r` spawns for an effect at `f`, and the offset from the frame's spawn
/// point that the run direction may follow.
fn spawn_origin(d: &FxElemDef, r: &[f32; 24], f: &Frame) -> (Vec3, Vec3) {
    let off = Vec3::new(
        pick(r[6], &d.spawn_origin[0]),
        pick(r[7], &d.spawn_origin[1]),
        pick(r[8], &d.spawn_origin[2]),
    );
    let pos = if d.flags & flags::SPAWN_RELATIVE_TO_EFFECT != 0 {
        f.origin + f.axis[0] * off.x + f.axis[1] * off.y + f.axis[2] * off.z
    } else {
        f.origin + off
    };
    let mut offset = Vec3::ZERO;
    match d.flags & flags::SPAWN_OFFSET_MASK {
        flags::SPAWN_OFFSET_SPHERE => {
            let dir = random_dir(r[9], r[10]);
            offset = dir * pick(r[11], &d.spawn_offset_radius);
        }
        flags::SPAWN_OFFSET_CYLINDER => {
            let radius = pick(r[11], &d.spawn_offset_radius);
            let yaw = r[9] * std::f32::consts::TAU;
            offset = f.axis[1] * (radius * yaw.cos())
                + f.axis[2] * (radius * yaw.sin())
                + f.axis[0] * pick(r[10], &d.spawn_offset_height);
        }
        _ => {}
    }
    (pos + offset, offset)
}

fn is_trail(d: &FxElemDef) -> bool {
    d.elem_type == elem::TRAIL && d.trail.is_some() && matches!(d.visuals, FxVisuals::Materials(_))
}

fn lerp_frame(a: &Frame, b: &Frame, t: f32) -> Frame {
    Frame {
        origin: a.origin.lerp(b.origin, t),
        axis: [0, 1, 2].map(|i| {
            a.axis[i]
                .lerp(b.axis[i], t)
                .try_normalize()
                .unwrap_or(a.axis[i])
        }),
    }
}

#[derive(Clone, Copy)]
struct Section {
    pos: Vec3,
    right: Vec3,
    up: Vec3,
    rotation: f32,
    size: [f32; 2],
    color: [u8; 4],
    u: f32,
}

fn lerp_section(a: &Section, b: &Section, t: f32) -> Section {
    Section {
        pos: a.pos.lerp(b.pos, t),
        right: a.right.lerp(b.right, t),
        up: a.up.lerp(b.up, t),
        u: a.u + (b.u - a.u) * t,
        ..*a
    }
}

impl Fx {
    /// Draws the trails of `e`: one strip per trail element, through the points the effect left, each cross-section
    /// the trail definition's polygon (an angled pair of fins) at the point's size, colour and rotation. Points whose
    /// life is over are not drawn; the strip ends in the cut between the last of them and the next point.
    fn draw_trails(&self, e: &Effect, cam: &Camera, out: &mut Draws) {
        for (k, d) in e.def.elems.iter().enumerate() {
            let (true, Some(td), FxVisuals::Materials(mats)) = (is_trail(d), &d.trail, &d.visuals)
            else {
                continue;
            };
            let points: Vec<&Elem> = e
                .elems
                .iter()
                .filter(|x| x.def == k && x.trail.is_some() && x.started && x.begin <= self.now)
                .collect();
            let (Some(first), Some(last)) = (points.first(), points.last()) else {
                continue;
            };
            let Some(Some(material)) = mats.get(pick_index(last, mats.len())) else {
                continue;
            };
            let repeat = td.repeat_dist.max(1) as f32;
            let mut u0 = -(first.trail.expect("filtered").dist / repeat).floor();
            if td.scroll_time_msec != 0 {
                let s = td.scroll_time_msec;
                u0 += if s <= 0 {
                    1.0 - (self.now % s) as f32 / s as f32
                } else {
                    (self.now % -s) as f32 / -s as f32
                };
            }
            let mut sections: Vec<Section> = Vec::new();
            let mut prev: Option<(f32, Section)> = None;
            for el in &points {
                let tp = el.trail.expect("filtered");
                let norm = (self.now - el.begin) as f32 / el.life;
                let vis = visual_state(d, el, norm.clamp(0.0, 0.999_999));
                let mut sec = Section {
                    pos: el.pos,
                    right: tp.right,
                    up: tp.up,
                    rotation: vis.rotation,
                    size: vis.size,
                    color: vis.color,
                    u: tp.dist / repeat + u0,
                };
                if norm < 1.0 {
                    match prev {
                        None if tp.seq == 0 => sec.color[3] = 0,
                        Some((pn, ps)) if pn >= 1.0 => {
                            sections.push(lerp_section(&ps, &sec, (1.0 - pn) / (norm - pn)));
                        }
                        _ => {}
                    }
                    sections.push(sec);
                }
                prev = Some((norm, sec));
            }
            if sections.len() < 2 || td.verts.is_empty() {
                continue;
            }
            let per = td.verts.len();
            let mut verts = Vec::with_capacity(per * sections.len());
            for s in &sections {
                let (sin, cos) = s.rotation.sin_cos();
                let left = s.right * cos + s.up * sin;
                let up = s.right * sin - s.up * cos;
                for v in td.verts.iter() {
                    verts.push(TrailVertex {
                        pos: s.pos + left * (v[0] * s.size[0]) + up * (v[1] * s.size[1]),
                        normal: (left * v[2] + up * v[3]).try_normalize().unwrap_or(left),
                        tangent: left,
                        uv: [s.u, v[4]],
                        color: s.color,
                    });
                }
            }
            let mut indices = Vec::new();
            for seg in 0..sections.len() - 1 {
                let near = (seg * per) as u32;
                let far = near + per as u32;
                for pair in td.indices.as_chunks::<2>().0 {
                    let (a, b) = (u32::from(pair[0]), u32::from(pair[1]));
                    indices.extend([near + a, near + b, far + a, far + b, far + a, near + b]);
                }
            }
            out.trails.push(Trail {
                material: material.clone(),
                verts,
                indices,
                depth: (last.pos - cam.origin).dot(cam.axis[0]),
                sort_order: d.sort_order,
            });
        }
    }
}

fn pick_index(el: &Elem, count: usize) -> usize {
    ((el.r[20] * count as f32) as usize).min(count.saturating_sub(1))
}

fn random_dir(a: f32, b: f32) -> Vec3 {
    let z = 1.0 - 2.0 * a;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = b * std::f32::consts::TAU;
    Vec3::new(r * phi.cos(), r * phi.sin(), z)
}

/// The velocity of the element's own samples at lifetime fraction `t`, in units per second.
fn velocity(d: &FxElemDef, el: &Elem, t: f32) -> Vec3 {
    let n = d.vel_samples.len().saturating_sub(1);
    if n == 0 {
        return Vec3::ZERO;
    }
    let sp = n as f32 * t;
    let i = (sp as usize).min(n - 1);
    let l = sp - i as f32;
    let (w0, w1) = (n as f32 - n as f32 * l, n as f32 * l);
    let (p, q) = (&d.vel_samples[i], &d.vel_samples[i + 1]);
    let eval = |f: &assets::zone::fx::VelFrame| {
        Vec3::new(
            el.r[0] * f.velocity.amplitude[0] + f.velocity.base[0],
            el.r[1] * f.velocity.amplitude[1] + f.velocity.base[1],
            el.r[2] * f.velocity.amplitude[2] + f.velocity.base[2],
        )
    };
    let mut v = Vec3::ZERO;
    if d.flags & flags::HAS_VELOCITY_WORLD != 0 {
        v += (eval(&p.world) * w0 + eval(&q.world) * w1) * VELOCITY_SCALE;
    }
    if d.flags & flags::HAS_VELOCITY_LOCAL != 0 {
        let local = (eval(&p.local) * w0 + eval(&q.local) * w1) * VELOCITY_SCALE;
        v += el.orient[0] * local.x + el.orient[1] * local.y + el.orient[2] * local.z;
    }
    v
}

struct Vis {
    color: [u8; 4],
    size: [f32; 2],
    scale: f32,
    rotation: f32,
}

fn visual_state(d: &FxElemDef, el: &Elem, t: f32) -> Vis {
    let n = d.vis_samples.len().saturating_sub(1);
    if n == 0 {
        return Vis {
            color: [255; 4],
            size: [1.0; 2],
            scale: 1.0,
            rotation: 0.0,
        };
    }
    let sp = n as f32 * t;
    let i = (sp as usize).min(n - 1);
    let l = sp - i as f32;
    let (p, q) = (&d.vis_samples[i], &d.vis_samples[i + 1]);
    let vl = el.r[23];
    let mut color = [0u8; 4];
    for (c, out) in color.iter_mut().enumerate() {
        let from = f32::from(p.base.color[c]) * (1.0 - vl) + f32::from(p.amplitude.color[c]) * vl;
        let to = f32::from(q.base.color[c]) * (1.0 - vl) + f32::from(q.amplitude.color[c]) * vl;
        *out = (from * (1.0 - l) + to * l).round().clamp(0.0, 255.0) as u8;
    }
    // The zone stores effect colours blue first.
    color.swap(0, 2);
    let size = |ch: usize, r: f32| {
        let from = r * p.amplitude.size[ch] + p.base.size[ch];
        let to = r * q.amplitude.size[ch] + q.base.size[ch];
        from * (1.0 - l) + to * l
    };
    let s0 = size(0, el.r[21]);
    let s1 = if d.flags & flags::NONUNIFORM_SCALE != 0 {
        size(1, el.r[22])
    } else {
        s0
    };
    let scale = {
        let from = el.r[12] * p.amplitude.scale + p.base.scale;
        let to = el.r[12] * q.amplitude.scale + q.base.scale;
        from * (1.0 - l) + to * l
    };
    // Rotation: the initial angle plus the integral of the rotation rate over the life so far.
    let rr = el.r[13];
    let w1 = l * l * 0.5;
    let w0 = l - w1;
    let total = rr * p.amplitude.rotation_total
        + p.base.rotation_total
        + (rr * p.amplitude.rotation_delta + p.base.rotation_delta) * w0
        + (rr * q.amplitude.rotation_delta + q.base.rotation_delta) * w1;
    let rotation = pick(vl, &d.initial_rotation) + total * el.life;
    Vis {
        color,
        size: [s0, s1],
        scale,
        rotation,
    }
}

/// The element's own axes: its spawn angles plus its angular velocity, in the frame of its velocity.
fn element_axis(d: &FxElemDef, el: &Elem, elapsed: f32) -> [Vec3; 3] {
    let a = |k: usize, rs: usize, rv: usize| {
        pick(el.r[rs], &d.spawn_angles[k]) + elapsed * pick(el.r[rv], &d.angular_velocity[k])
    };
    let (pitch, yaw, roll) = (a(0, 12, 3), a(1, 13, 4), a(2, 14, 5));
    let (cp, sp) = (pitch.cos(), pitch.sin());
    let (cy, sy) = (yaw.cos(), yaw.sin());
    let (cr, sr) = (roll.cos(), roll.sin());
    let local = [
        Vec3::new(cp * cy, cp * sy, -sp),
        Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp),
        Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp),
    ];
    let o = &el.orient;
    local.map(|v| o[0] * v.x + o[1] * v.y + o[2] * v.z)
}

#[allow(clippy::too_many_arguments)]
fn quad(
    material: &Arc<Material>,
    pos: Vec3,
    tangent: Vec3,
    binormal: Vec3,
    normal: Vec3,
    vis: &Vis,
    d: &FxElemDef,
    el: &Elem,
    cam: &Camera,
    t: f32,
    now: i32,
    sort_order: u8,
) -> Quad {
    let (s, c) = vis.rotation.sin_cos();
    let rot_t = tangent * c + binormal * s;
    let rot_b = tangent * s - binormal * c;
    let left = rot_t * vis.size[0];
    let up = rot_b * vis.size[1];
    let (s0, ds, t0, dt) = atlas(d, el, t, (now - el.begin) as f32);
    Quad {
        material: material.clone(),
        corners: [
            pos - left + up,
            pos - left - up,
            pos + left - up,
            pos + left + up,
        ],
        uv: [[s0, t0 + dt], [s0, t0], [s0 + ds, t0], [s0 + ds, t0 + dt]],
        color: vis.color,
        normal,
        tangent: rot_t,
        depth: (pos - cam.origin).dot(cam.axis[0]),
        sort_order,
    }
}

/// The atlas cell's texture rectangle: start and extent in s and t.
fn atlas(d: &FxElemDef, el: &Elem, t: f32, elapsed_ms: f32) -> (f32, f32, f32, f32) {
    let count = i32::from(d.atlas_entry_count);
    if count <= 1 {
        return (0.0, 1.0, 0.0, 1.0);
    }
    let [behavior, index, fps, loops, col_bits, row_bits] = d.atlas;
    let mut i = match behavior & 3 {
        0 => i32::from(index),
        1 => (count as f32 * el.r[10]) as i32,
        _ => (el.id as i32) & (count - 1),
    };
    if behavior & 4 != 0 {
        i += (count as f32 * t) as i32;
    } else if fps != 0 {
        i += i32::from(fps) * elapsed_ms as i32 / 1000;
    }
    if behavior & 8 != 0 && i >= count * i32::from(loops) {
        i = count - 1;
    }
    let i = i & (count - 1);
    let ds = 1.0 / (1 << col_bits) as f32;
    let dt = 1.0 / (1 << row_bits) as f32;
    (
        (i & ((1 << col_bits) - 1)) as f32 * ds,
        ds,
        (i >> col_bits) as f32 * dt,
        dt,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use assets::zone::fx::Range;

    fn range<T: Copy>(base: T, amplitude: T) -> Range<T> {
        Range { base, amplitude }
    }

    fn elem_def(elem_type: u8, visuals: FxVisuals) -> FxElemDef {
        let zero = range(0.0, 0.0);
        FxElemDef {
            flags: 0,
            spawn: [1, 0],
            spawn_range: zero,
            fade_in_range: zero,
            fade_out_range: zero,
            spawn_frustum_cull_radius: 0.0,
            spawn_delay_msec: range(0, 0),
            life_span_msec: range(100, 0),
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
            elem_type,
            vel_samples: Arc::from(Vec::new()),
            vis_samples: Arc::from(Vec::new()),
            visuals,
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
        }
    }

    fn effect(
        name: &str,
        looping: u32,
        one_shot: u32,
        life: i32,
        elems: Vec<FxElemDef>,
    ) -> Arc<FxEffectDef> {
        Arc::new(FxEffectDef {
            name: Some(name.into()),
            flags: 0,
            total_size: 0,
            msec_looping_life: life,
            looping_count: looping,
            one_shot_count: one_shot,
            emission_count: 0,
            elems: elems.into(),
        })
    }

    fn sound(alias: &str) -> FxElemDef {
        elem_def(
            elem::SOUND,
            FxVisuals::Sounds(Arc::from(vec![Some(Arc::<str>::from(alias))])),
        )
    }

    /// A floor at z = 0.
    struct Floor;

    impl World for Floor {
        fn trace(&self, a: Vec3, b: Vec3, _: Vec3, _: Vec3) -> Option<(f32, Vec3)> {
            (a.z >= 0.0 && b.z < 0.0).then(|| (a.z / (a.z - b.z), Vec3::Z))
        }
    }

    fn lib(effects: Vec<Arc<FxEffectDef>>) -> Arc<Library> {
        let mut l = Library::default();
        for e in effects {
            l.add(e);
        }
        Arc::new(l)
    }

    #[test]
    fn one_shot_elements_spawn_together_and_die_after_their_lifespan() {
        let mut e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        e.spawn = [3, 0];
        let def = effect("fx/test", 0, 1, 0, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        assert_eq!(fx.stats.elems_spawned, 3);
        fx.update(50, &Empty);
        assert_eq!((fx.live_effects(), fx.live_elems()), (1, 3));
        fx.update(200, &Empty);
        assert_eq!((fx.live_effects(), fx.live_elems()), (0, 0));
    }

    #[test]
    fn a_looping_element_spawns_on_its_interval_for_the_looping_life_only() {
        let mut e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        e.spawn = [100, i32::MAX];
        let def = effect("fx/loop", 1, 0, 450, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        fx.update(2000, &Empty);
        // t = 0, 100, 200, 300 and 400.
        assert_eq!(fx.stats.elems_spawned, 5);
        assert_eq!(fx.live_effects(), 0);
    }

    #[test]
    fn a_looping_element_stops_at_its_count_even_without_a_looping_life() {
        let mut e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        e.spawn = [100, 3];
        let def = effect("fx/count", 1, 0, 0, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        fx.update(2000, &Empty);
        assert_eq!(fx.stats.elems_spawned, 3);
    }

    fn trail_effect(life: i32) -> Arc<FxEffectDef> {
        let material = Arc::new(Material {
            name: None,
            game_flags: 0,
            sort_key: 0,
            atlas_rows: 1,
            atlas_columns: 1,
            draw_surf: 0,
            surface_type_bits: 0,
            hash_index: 0,
            state_bits_entry: [0; 34],
            state_flags: 0,
            camera_region: 0,
            technique_set: None,
            textures: Arc::from(Vec::new()),
            constants: Arc::from(Vec::new()),
            state_bits: Arc::from(Vec::new()),
        });
        let mut e = elem_def(
            elem::TRAIL,
            FxVisuals::Materials(vec![Some(material)].into()),
        );
        e.spawn = [1, i32::MAX];
        e.life_span_msec = range(life, 0);
        e.trail = Some(Arc::new(assets::zone::fx::FxTrailDef {
            scroll_time_msec: 0,
            repeat_dist: 100,
            split_dist: 50,
            // One fin, a unit wide.
            verts: Arc::new([[1.0, 0.0, 0.0, 1.0, 0.0], [-1.0, 0.0, 0.0, 1.0, 1.0]]),
            indices: Arc::new([0, 1]),
        }));
        effect("fx/trail", 1, 0, 0, vec![e])
    }

    fn trail_cam() -> Camera {
        Camera {
            origin: Vec3::new(-500.0, 0.0, 0.0),
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
        }
    }

    #[test]
    fn a_moving_effect_leaves_a_strip_through_the_points_it_passed_that_ends_when_they_expire() {
        let def = trail_effect(1000);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        let id = fx.play_attached(&def, Frame::facing(Vec3::ZERO, Vec3::X));
        // 400 units along x in 400 ms: a point every 50 units, plus the newest at the effect.
        for i in 1..=8 {
            fx.move_effect(id, Frame::facing(Vec3::X * (i as f32 * 50.0), Vec3::X));
            fx.update(i * 50, &Empty);
        }
        let mut d = Draws::default();
        fx.draw(&trail_cam(), &mut d);
        assert_eq!(d.trails.len(), 1);
        let t = &d.trails[0];
        assert_eq!(t.verts.len(), 2 * 9);
        // Each of the eight segments is two triangles of the fin.
        assert_eq!(t.indices.len(), 8 * 6);
        let (min, max) = t.verts.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
            (a.min(v.pos.x), b.max(v.pos.x))
        });
        assert_eq!((min, max), (0.0, 400.0));
        // A trail whose effect stopped fades out and the strip goes with it.
        fx.stop(id);
        fx.update(3000, &Empty);
        let mut d = Draws::default();
        fx.draw(&trail_cam(), &mut d);
        assert!(d.trails.is_empty());
        assert_eq!(fx.live_elems(), 0);
    }

    #[test]
    fn a_standing_effect_leaves_no_strip() {
        let def = trail_effect(1000);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::ZERO, Vec3::X));
        fx.update(300, &Empty);
        let mut d = Draws::default();
        fx.draw(&trail_cam(), &mut d);
        assert!(d.trails.is_empty());
    }

    #[test]
    fn a_runner_starts_the_effect_it_names() {
        let child = effect("fx/child", 0, 1, 0, vec![sound("child_sound")]);
        let runner = elem_def(
            elem::RUNNER,
            FxVisuals::Effects(Arc::from(vec![Some(Arc::<str>::from("fx/child"))])),
        );
        let parent = effect("fx/parent", 0, 1, 0, vec![runner]);
        let mut fx = Fx::new(lib(vec![child, parent.clone()]));
        fx.play(&parent, Frame::facing(Vec3::new(1.0, 2.0, 3.0), Vec3::Z));
        fx.update(10, &Empty);
        let s = fx.take_sounds();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].alias, "child_sound");
        assert_eq!(s[0].origin, Vec3::new(1.0, 2.0, 3.0));
        assert!(fx.take_sounds().is_empty());
    }

    fn falling_particle() -> (Arc<FxEffectDef>, Arc<Library>) {
        let mut e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        e.flags = flags::USE_COLLISION | flags::DIE_ON_TOUCH;
        e.gravity = range(1.0, 0.0);
        e.life_span_msec = range(2000, 0);
        e.effect_on_impact = Some("fx/landed".into());
        let def = effect("fx/fall", 0, 1, 0, vec![e]);
        let landed = effect("fx/landed", 0, 1, 0, vec![sound("thud")]);
        (def.clone(), lib(vec![def, landed]))
    }

    #[test]
    fn a_falling_particle_lands_and_plays_its_impact_effect_on_the_floor() {
        let (def, lib) = falling_particle();
        let mut fx = Fx::new(lib);
        fx.play(&def, Frame::facing(Vec3::new(0.0, 0.0, 10.0), Vec3::Z));
        // Free fall from 10 units takes sqrt(2 * 10 / 800) = 0.158 s.
        fx.update(100, &Empty);
        fx.update(100, &Floor);
        assert!(fx.take_sounds().is_empty(), "not landed at 100 ms");
        fx.update(300, &Floor);
        assert_eq!(fx.stats.impacts, 1);
        let s = fx.take_sounds();
        assert_eq!(s.len(), 1, "one impact");
        assert_eq!(s[0].alias, "thud");
        assert!(s[0].origin.z.abs() < 0.5, "{:?}", s[0].origin);
        assert_eq!(fx.live_elems(), 0);
    }

    fn debris_model(preset: Option<assets::zone::phys::PhysPreset>) -> Arc<XModel> {
        use assets::zone::xmodel::LodInfo;
        let lod = || LodInfo {
            dist: 0.0,
            surf_count: 0,
            surf_index: 0,
            part_bits: [0; 4],
            lod: 0,
            smc_index_plus_one: 0,
            smc_alloc_bits: 0,
        };
        Arc::new(XModel {
            name: Some("debris".into()),
            num_bones: 0,
            num_root_bones: 0,
            lod_ramp_type: 0,
            bone_names: Arc::new([]),
            parent_list: Arc::new([]),
            quats: Arc::new([]),
            trans: Arc::new([]),
            part_classification: Arc::new([]),
            base_mat: Arc::new([]),
            surfs: Arc::new([]),
            materials: Arc::new([]),
            lod_info: [lod(), lod(), lod(), lod()],
            coll_surfs: Arc::new([]),
            contents: 0,
            bone_info: Arc::new([]),
            radius: 0.0,
            mins: [-3.0; 3],
            maxs: [3.0; 3],
            num_lods: 1,
            coll_lod: 0,
            mem_usage: 0,
            flags: 0,
            bad: false,
            phys_preset: preset.map(Arc::new),
            phys_geoms: None,
        })
    }

    fn preset(bounce: f32) -> assets::zone::phys::PhysPreset {
        assets::zone::phys::PhysPreset {
            name: None,
            kind: 0,
            mass: 2.0,
            bounce,
            friction: 0.6,
            bullet_force_scale: 1.0,
            explosive_force_scale: 1.0,
            snd_alias_prefix: None,
            pieces_spread_fraction: 0.0,
            pieces_upward_velocity: 0.0,
            temp_default_to_cylinder: false,
        }
    }

    /// Where the one model of the effect is drawn at `now`.
    fn model_at(fx: &mut Fx, now: i32, world: &dyn World) -> Option<Vec3> {
        fx.update(now, world);
        let mut out = Draws::default();
        let cam = Camera {
            origin: Vec3::ZERO,
            axis: [Vec3::X, Vec3::Y, Vec3::Z],
        };
        fx.draw(&cam, &mut out);
        out.models.first().map(|m| m.origin)
    }

    fn debris_effect(model: Arc<XModel>, flags: i32) -> Arc<FxEffectDef> {
        let mut e = elem_def(elem::MODEL, FxVisuals::Models(Arc::from(vec![Some(model)])));
        e.flags = flags;
        e.life_span_msec = range(6000, 0);
        effect("fx/debris", 0, 1, 0, vec![e])
    }

    #[test]
    fn a_model_with_physics_falls_bounces_and_comes_to_rest_on_its_preset() {
        let def = debris_effect(debris_model(Some(preset(0.4))), flags::USE_MODEL_PHYSICS);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::new(0.0, 0.0, 100.0), Vec3::Z));
        let start = model_at(&mut fx, 0, &Floor).unwrap();
        assert_eq!(start.z, 100.0);
        // A quarter second of free fall: 800 * 0.25^2 / 2 = 25 units.
        let early = model_at(&mut fx, 250, &Floor).unwrap();
        assert!((start.z - early.z - 25.0).abs() < 5.0, "{early}");
        let mut lowest = f32::MAX;
        let mut bounced = false;
        let mut last = early;
        for t in (300..=2500).step_by(50) {
            let at = model_at(&mut fx, t, &Floor).unwrap();
            lowest = lowest.min(at.z);
            bounced |= at.z > last.z + 1.0;
            last = at;
        }
        assert!(bounced, "bounce 0.4 sends it back up");
        assert!(lowest > 2.0, "its box rests on the floor: {lowest}");
        let a = model_at(&mut fx, 4000, &Floor).unwrap();
        let b = model_at(&mut fx, 5000, &Floor).unwrap();
        assert!(a.distance(b) < 0.01, "at rest: {a} {b}");
        assert!(a.z < 10.0, "{a}");
    }

    #[test]
    fn a_model_with_physics_but_no_preset_is_dropped() {
        let def = debris_effect(debris_model(None), flags::USE_MODEL_PHYSICS);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::new(0.0, 0.0, 100.0), Vec3::Z));
        assert_eq!(model_at(&mut fx, 50, &Floor), None);
    }

    #[test]
    fn the_landing_point_does_not_depend_on_the_frame_rate() {
        let land = |frame_ms: i32| {
            let (def, lib) = falling_particle();
            let mut fx = Fx::new(lib);
            fx.play(&def, Frame::facing(Vec3::new(0.0, 0.0, 10.0), Vec3::Z));
            let mut t = 0;
            while t < 400 {
                t += frame_ms;
                fx.update(t, &Floor);
            }
            fx.take_sounds()[0].origin
        };
        let (a, b) = (land(5), land(33));
        assert!((a - b).length() < 1e-3, "{a:?} vs {b:?}");
    }

    #[test]
    fn a_retriggered_effect_spawns_its_one_shots_again_without_a_second_effect() {
        let mut e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        e.spawn = [2, 0];
        e.life_span_msec.base = 1000;
        let def = effect("fx/retrigger", 0, 1, 0, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        let id = fx.play_attached(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        fx.update(100, &Empty);
        assert_eq!((fx.live_effects(), fx.stats.elems_spawned), (1, 2));
        assert!(fx.retrigger(id, 300));
        fx.update(400, &Empty);
        assert_eq!(
            (fx.live_effects(), fx.stats.elems_spawned, fx.live_elems()),
            (1, 4, 4)
        );
        // Stopped and played out, an effect cannot be retriggered.
        fx.stop(id);
        fx.update(5000, &Empty);
        assert!(!fx.retrigger(id, 5100));
    }

    #[test]
    fn a_handle_is_live_until_its_effect_ends_or_is_evicted() {
        let e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        let def = effect("fx/live", 0, 1, 0, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        let first = fx.play_attached(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        assert!(fx.is_live(first));
        for _ in 0..MAX_EFFECTS {
            fx.play_attached(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        }
        assert!(!fx.is_live(first));
        let one = fx.play_attached(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        fx.stop(one);
        fx.update(10_000, &Empty);
        assert!(!fx.is_live(one));
    }

    #[test]
    fn effects_beyond_the_cap_drop_the_oldest() {
        let e = elem_def(elem::SPRITE_BILLBOARD, FxVisuals::None);
        let def = effect("fx/many", 0, 1, 0, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        for _ in 0..MAX_EFFECTS + 5 {
            fx.play(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        }
        assert_eq!(fx.live_effects(), MAX_EFFECTS);
        assert_eq!(fx.stats.dropped, 5);
    }

    #[test]
    fn effect_names_match_without_regard_to_case() {
        let def = effect("FX/Misc/Smoke", 0, 1, 0, vec![sound("a")]);
        let mut fx = Fx::new(lib(vec![def]));
        assert!(fx.play_named("fx/misc/smoke", Frame::facing(Vec3::ZERO, Vec3::Z)));
        assert!(!fx.play_named("fx/misc/none", Frame::facing(Vec3::ZERO, Vec3::Z)));
    }
}
