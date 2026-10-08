// SPDX-License-Identifier: GPL-3.0-or-later
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
use std::collections::HashMap;
use std::sync::Arc;

mod rng;
pub use rng::Rng;

/// Longest single integration step.
const STEP_MS: i32 = 16;
/// Effects alive at once; older ones are dropped beyond this.
const MAX_EFFECTS: usize = 400;
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
}

/// Counters of a run.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub effects_played: u64,
    pub elems_spawned: u64,
    pub impacts: u64,
    pub dropped: u64,
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
}

struct Effect {
    def: Arc<FxEffectDef>,
    frame: Frame,
    start: i32,
    elems: Vec<Elem>,
    /// Next spawn time of each looping element.
    next_loop: Vec<i32>,
    loop_end: i32,
}

pub struct Fx {
    lib: Arc<Library>,
    effects: Vec<Effect>,
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
    pub fn play_at(&mut self, def: &Arc<FxEffectDef>, frame: Frame, at: i32) {
        if self.effects.len() >= MAX_EFFECTS {
            self.effects.remove(0);
            self.stats.dropped += 1;
        }
        self.stats.effects_played += 1;
        let looping = def.looping_count as usize;
        let one_shot = def.one_shot_count as usize;
        let loop_end = if def.msec_looping_life > 0 {
            at + def.msec_looping_life
        } else {
            at
        };
        let mut e = Effect {
            def: def.clone(),
            frame,
            start: at,
            elems: Vec::new(),
            next_loop: vec![at; looping],
            loop_end,
        };
        for k in looping..(looping + one_shot).min(def.elems.len()) {
            let d = &def.elems[k];
            let count = d.spawn[0] as f32 + d.spawn[1] as f32 * self.rng.f();
            for _ in 0..(count as i32).max(0) {
                self.spawn(&mut e, k, at);
            }
        }
        self.effects.push(e);
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
        let off = Vec3::new(
            pick(r[6], &d.spawn_origin[0]),
            pick(r[7], &d.spawn_origin[1]),
            pick(r[8], &d.spawn_origin[2]),
        );
        let f = &e.frame;
        let mut pos = if d.flags & flags::SPAWN_RELATIVE_TO_EFFECT != 0 {
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
        pos += offset;
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
        });
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
            e.elems.retain(|x| !x.done);
        }
        self.live_elems = effects.iter().map(|e| e.elems.len()).sum();
        self.effects = effects;
    }

    fn update_effect(
        &mut self,
        e: &mut Effect,
        now: i32,
        world: &dyn World,
        spawned: &mut Vec<(Arc<FxEffectDef>, Frame, i32)>,
    ) {
        // Looping elements spawn on their interval until the effect's looping life is over.
        let looping = e.next_loop.len();
        for k in 0..looping {
            let d = &e.def.elems[k];
            let interval = d.spawn[0].max(1);
            let count = d.spawn[1].max(1);
            while e.next_loop[k] <= now && e.next_loop[k] <= e.loop_end {
                let t = e.next_loop[k];
                for _ in 0..count {
                    self.spawn(e, k, t);
                }
                e.next_loop[k] = t + interval;
            }
            if e.loop_end == e.start {
                // No looping life: one batch only.
                e.next_loop[k] = e.loop_end + 1;
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
            for el in &e.elems {
                if el.done || !el.started || self.now < el.begin {
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
                        let a = element_axis(d, el, (self.now - el.begin) as f32);
                        out.models.push(ModelDraw {
                            model: model.clone(),
                            origin: el.pos,
                            axis: a,
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
        e.spawn = [100, 1];
        let def = effect("fx/loop", 1, 0, 450, vec![e]);
        let mut fx = Fx::new(lib(vec![def.clone()]));
        fx.play(&def, Frame::facing(Vec3::ZERO, Vec3::Z));
        fx.update(2000, &Empty);
        // t = 0, 100, 200, 300 and 400.
        assert_eq!(fx.stats.elems_spawned, 5);
        assert_eq!(fx.live_effects(), 0);
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
