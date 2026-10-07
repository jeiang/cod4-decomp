// SPDX-License-Identifier: GPL-3.0-or-later
//! The client's effects: [`ClientEvent`]s become effect playbacks (impact sprites and decals, explosions, blood), the
//! playback runs on the server clock against the static map, and every frame yields the sprites, decals, models and
//! sounds to draw.
//!
//! Which effect an impact plays is the original's: the weapon's impact type picks a row of the impact table, the
//! surface type a column; flesh has its own four columns (body or head, fatal or not).

use crate::decal::{self, Placement};
use crate::events::ClientEvent;
use assets::zone::fx::{FxEffectDef, FxImpactTable};
use assets::zone::gfx::Material;
use assets::zone::gfxworld::GfxWorld;
use assets::zone::weapon::WeaponDef;
use fx::{Camera, Draws, Frame, Fx, Library, SoundPlay};
use glam::Vec3;
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
const SURFACE_FLESH: u8 = 7;
const FLESH_BODY_NONFATAL: usize = 0;
const FLESH_BODY_FATAL: usize = 1;
/// How high above a player's feet blood appears.
const CHEST: f32 = 40.0;

/// What the world looks like to a particle: solid map geometry.
struct Tracer<'a>(&'a dyn Collide);

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
    /// What was played, by kind; for the report.
    pub played: BTreeMap<&'static str, u64>,
    /// Names of effects an event asked for that the content lacks.
    pub missing: HashSet<String>,
}

/// What to draw this frame.
#[derive(Default)]
pub struct Drawn {
    pub meshes: Vec<DynMesh>,
    pub models: Vec<ModelInstance>,
    /// How many sprites and decals the meshes hold.
    pub quads: usize,
    pub decals: usize,
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

    fn play(&mut self, kind: &'static str, def: Option<Arc<FxEffectDef>>, at: Vec3, dir: Vec3) {
        if let Some(d) = def {
            self.fx.play(&d, Frame::facing(at, dir));
            *self.played.entry(kind).or_default() += 1;
        }
    }

    /// Starts what `ev` shows. `weapon` resolves a weapon index to its definition.
    pub fn event(&mut self, ev: &ClientEvent, weapon: &dyn Fn(u16) -> Option<Arc<WeaponDef>>) {
        match ev {
            ClientEvent::BulletImpact {
                origin,
                normal,
                surface,
                weapon: w,
                ..
            } => {
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
                let def = d
                    .as_ref()
                    .and_then(|d| d.proj_explosion_effect.clone())
                    .or_else(|| {
                        self.impact_effect(d.as_ref().map_or(0, |d| d.impact_type), 0, false)
                    });
                self.play("explosion", def, Vec3::from(*origin), n);
            }
            ClientEvent::PlayFx {
                origin,
                forward,
                name,
                ..
            } => match self.fx.library().get(name).cloned() {
                Some(d) => self.play(
                    "play_fx",
                    Some(d),
                    Vec3::from(*origin),
                    Vec3::from(*forward),
                ),
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
            ClientEvent::PhysicsExplosion { .. } => {}
        }
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
        self.play("demo", def, eye + forward * 90.0, -forward);
    }

    /// Advances every playing effect to `now_ms` of the server clock.
    pub fn update(&mut self, now_ms: i32, world: &dyn Collide) {
        self.fx.update(now_ms, &Tracer(world));
    }

    /// Sounds the effects started since the last call.
    pub fn take_sounds(&mut self) -> Vec<SoundPlay> {
        self.fx.take_sounds()
    }

    pub fn live_elems(&self) -> usize {
        self.fx.live_elems()
    }

    /// What to draw from a camera at `eye` looking along `yaw` and `pitch` (radians).
    pub fn draw(&mut self, eye: Vec3, yaw: f32, pitch: f32) -> Drawn {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let forward = Vec3::new(cp * cy, cp * sy, sp);
        let left = Vec3::new(-sy, cy, 0.0);
        let cam = Camera {
            origin: eye,
            axis: [forward, left, forward.cross(left)],
        };
        let mut d = Draws::default();
        self.fx.draw(&cam, &mut d);
        let mut out = Drawn {
            quads: d.quads.len(),
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
                    decal::world_triangles(&self.world, p.origin - reach, p.origin + reach),
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
            let f = m.axis[0];
            inst.angles = [-f.z.asin().to_degrees(), f.y.atan2(f.x).to_degrees(), 0.0];
            out.models.push(inst);
        }
        out
    }
}
