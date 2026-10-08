// SPDX-License-Identifier: GPL-3.0-only
//! Bullets: the point traces shots and missiles use, and `Bullet_Fire`.
//!
//! A shot is traced in two passes. The map, its props and every non-player entity come from
//! [`World::bullet_trace`]; players are then tested one by one against their body skeleton
//! (`G_LocationalTrace`), and a segment that passes through a player's box without touching a
//! bone goes on. The damage a trace deals is worked out first, against shared state, and applied
//! afterwards, so [`Game::bullet_hits`] needs nothing mutable.
//!
//! Ceilings: entities other than players have no locational hit parts, `trigger_damage` volumes
//! are not notified of bullets passing through them, and impact effects are the clients' job.

use sim::Vec3;
use sim::cm::{ENTITYNUM_NONE, ENTITYNUM_WORLD, Trace};
use sim::contents;
use sim::pm::PmType;
use sim::skel::{LocHit, Stance, box_hit_location};
use sim::weapon::damage::{
    MAX_BULLET_EXTENSIONS, MAX_PENETRATIONS, PENETRATION_ADVANCE, PENETRATION_REVERSE_ADVANCE,
    PenetrationTable, RIFLE_BODY_DAMAGE_SCALE, advance_trace, bullet_damage, penetrate_multiplier,
};
use sim::weapon::fire::{AimBasis, bullet_shots};
use sim::weapon::{PenetrateType, WeaponInfo, WeaponParams};

use crate::client::Team;
use crate::combat::{MOD_PISTOL_BULLET, MOD_RIFLE_BULLET, dflags};
use crate::game::Game;

pub(crate) fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn mad(a: Vec3, s: f32, b: Vec3) -> Vec3 {
    [a[0] + s * b[0], a[1] + s * b[1], a[2] + s * b[2]]
}

pub(crate) fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

pub(crate) fn normalized(a: Vec3) -> Vec3 {
    let l = length(a);
    if l == 0.0 {
        [0.0; 3]
    } else {
        mad([0.0; 3], 1.0 / l, a)
    }
}

pub(crate) fn lerp(a: Vec3, b: Vec3, f: f32) -> Vec3 {
    mad(a, f, sub(b, a))
}

/// `MASK_SHOT` as bullets and melee use it (`0x2806891` with the player bit included).
pub const MASK_SHOT_CLIENT: i32 = 0x0280_6891;

pub const SURF_NOIMPACT: i32 = 0x10;
pub const SURF_NOPENETRATE: i32 = 0x100;
pub const SURF_SKY: i32 = 0x4;
const SURF_TYPE_SHIFT: u32 = 20;
const SURF_TYPE_MASK: i32 = 0x01F0_0000;
pub const SURF_TYPE_FLESH: i32 = 7 << SURF_TYPE_SHIFT;
pub const SURF_TYPE_WATER: usize = 20;

/// `SURF_TYPEINDEX`.
pub fn surface_type(flags: i32) -> usize {
    ((flags & SURF_TYPE_MASK) >> SURF_TYPE_SHIFT) as usize
}

/// One point trace of a shot.
#[derive(Debug, Clone, Copy)]
pub struct ShotTrace {
    pub fraction: f32,
    pub normal: Vec3,
    pub surface_flags: i32,
    pub contents: i32,
    /// The entity hit: [`ENTITYNUM_WORLD`] for the map, [`ENTITYNUM_NONE`] for nothing.
    pub hit: u16,
    pub start_solid: bool,
    pub all_solid: bool,
    /// Hit location of a player hit (`partGroup`); 0 elsewhere.
    pub hitloc: u8,
}

impl ShotTrace {
    const MISS: Self = Self {
        fraction: 1.0,
        normal: [0.0; 3],
        surface_flags: 0,
        contents: 0,
        hit: ENTITYNUM_NONE,
        start_solid: false,
        all_solid: false,
        hitloc: 0,
    };

    fn of(t: &Trace) -> Self {
        Self {
            fraction: t.fraction,
            normal: t.normal,
            surface_flags: t.surface_flags,
            contents: t.contents,
            hit: t.hit_id,
            start_solid: t.start_solid,
            all_solid: t.all_solid,
            hitloc: 0,
        }
    }

    pub fn is_hit(&self) -> bool {
        self.hit != ENTITYNUM_NONE && self.fraction < 1.0
    }
}

/// Entry fraction of the segment into the box, or `None` when it misses.
fn segment_box(start: Vec3, end: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for i in 0..3 {
        let d = end[i] - start[i];
        if d.abs() < 1e-9 {
            if start[i] < lo[i] || start[i] > hi[i] {
                return None;
            }
            continue;
        }
        let (mut a, mut b) = ((lo[i] - start[i]) / d, (hi[i] - start[i]) / d);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

impl Game {
    /// `G_LocationalTrace` for a point: the nearest thing `start..end` touches, `ignore` and
    /// what it owns excepted. Players are only hit where a body part is (`rifle` picks the
    /// rifle part priorities); `also_ignore` is a player to skip as well.
    pub fn shot_trace(
        &self,
        start: Vec3,
        end: Vec3,
        ignore: u16,
        also_ignore: u16,
        mask: i32,
        rifle: bool,
    ) -> ShotTrace {
        let Some(world) = self.world.as_ref() else {
            return ShotTrace::MISS;
        };
        let mut best =
            ShotTrace::of(&world.bullet_trace(start, end, ignore, mask & !contents::PLAYER));
        if best.hit == ENTITYNUM_NONE {
            best.fraction = 1.0;
        }
        if mask & contents::PLAYER == 0 {
            return best;
        }
        for (n, c) in self.connected_clients() {
            if n == ignore || n == also_ignore {
                continue;
            }
            let Some(e) = self.ent(n) else { continue };
            if e.contents & contents::PLAYER == 0 {
                continue;
            }
            // A person's shot is judged against the bodies as they were when the shooter saw them.
            let rewound = self
                .lag_time
                .and_then(|t| self.lag.at(n, t, self.level.time));
            let body = rewound
                .as_ref()
                .map_or((&e.origin, &e.mins, &e.maxs, &c.pose), |r| {
                    (&r.origin, &r.mins, &r.maxs, &r.pose)
                });
            let lo = [0, 1, 2].map(|i| body.0[i] + body.1[i] - 1.0);
            let hi = [0, 1, 2].map(|i| body.0[i] + body.2[i] + 1.0);
            if segment_box(start, end, lo, hi).is_none_or(|f| f >= best.fraction) {
                continue;
            }
            let Some(hit) =
                self.locate_hit(c.ps.viewangles[1], body, start, end, rifle, best.fraction)
            else {
                continue;
            };
            best = ShotTrace {
                fraction: hit.fraction,
                normal: hit.normal,
                surface_flags: SURF_TYPE_FLESH,
                contents: contents::PLAYER,
                hit: n,
                start_solid: false,
                all_solid: false,
                hitloc: hit.hitloc,
            };
        }
        best
    }

    /// Where the segment meets player `n`'s body, if it does: the pose skeleton when the
    /// server has one, otherwise the stance box.
    fn locate_hit(
        &self,
        yaw: f32,
        body: (&Vec3, &Vec3, &Vec3, &crate::playeranim::PlayerPoseState),
        start: Vec3,
        end: Vec3,
        rifle: bool,
        max_fraction: f32,
    ) -> Option<LocHit> {
        let (origin, _, maxs, pose) = body;
        if let Some(anims) = self.player_anims.as_ref() {
            return pose.trace(anims, origin, &start, &end, rifle, max_fraction);
        }
        let stance = if maxs[2] < 40.0 {
            Stance::Prone
        } else if maxs[2] < 60.0 {
            Stance::Crouch
        } else {
            Stance::Stand
        };
        let (fraction, loc) = box_hit_location(&start, &end, origin, yaw, stance)?;
        (fraction < max_fraction).then(|| {
            let d = normalized(sub(end, start));
            LocHit {
                fraction,
                bone: 0,
                hitloc: loc as u8,
                normal: [-d[0], -d[1], -d[2]],
            }
        })
    }

    /// Builds the skeleton locational hits use, once per map.
    pub fn ensure_player_anims(&mut self) {
        if self.player_anims.is_some() {
            return;
        }
        // Every stock body shares one skeleton and hit-box layout.
        if let Ok(a) = crate::playeranim::PlayerAnims::new(
            &self.content,
            "body_mp_usmc_assault",
            Some("head_mp_usmc_tactical_mich"),
        ) {
            self.player_anims = Some(std::sync::Arc::new(a));
        }
    }

    /// The penetration table of the map (`info/bullet_penetration_mp`).
    pub fn penetration_table(&mut self) -> std::sync::Arc<PenetrationTable> {
        if let Some(t) = &self.penetration {
            return t.clone();
        }
        let text = self
            .content
            .rawfile("info/bullet_penetration_mp")
            .map(|b| {
                String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())])
                    .into_owned()
            })
            .unwrap_or_default();
        let t = std::sync::Arc::new(PenetrationTable::parse(&text));
        self.penetration = Some(t.clone());
        t
    }

    /// `OnSameTeam`: two players of the same side in a team game.
    pub fn on_same_team(&self, a: u16, b: u16) -> bool {
        let (Some(a), Some(b)) = (self.client(a), self.client(b)) else {
            return false;
        };
        a.team == b.team && matches!(a.team, Team::Axis | Team::Allies)
    }

    /// `LogAccuracyHit`: a live enemy player was hurt.
    pub fn is_accurate_hit(&self, target: u16, attacker: u16) -> bool {
        target != attacker
            && self.ent(target).is_some_and(|e| e.takedamage)
            && self.is_client(target)
            && self.is_client(attacker)
            && self
                .client(target)
                .is_some_and(|c| c.ps.pm_type < PmType::Dead)
            && !self.on_same_team(target, attacker)
    }
}

/// What a shot needs to know about its shooter and weapon.
pub struct BulletParams<'a> {
    pub attacker: u16,
    pub info: &'a WeaponInfo,
    pub perks: u32,
    pub params: &'a WeaponParams,
    pub penetration: &'a PenetrationTable,
    pub friendly_fire: bool,
}

/// One damage event a bullet caused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BulletHit {
    /// The entity hit, [`ENTITYNUM_WORLD`] for the map.
    pub target: u16,
    /// Whether the hit deals damage; every other hit is an impact only (decals, sparks).
    pub damageable: bool,
    /// The surface normal at the point.
    pub normal: Vec3,
    /// `surface_type` of the surface hit; flesh for players and other entities without one.
    pub surface: u8,
    /// The surface takes no marks (sky, no-impact brushes).
    pub no_impact: bool,
    pub point: Vec3,
    pub dir: Vec3,
    pub damage: i32,
    pub flags: i32,
    pub mean: u8,
    pub hitloc: u8,
}

/// `BulletFireParams`.
#[derive(Clone, Copy)]
struct Bp {
    start: Vec3,
    end: Vec3,
    dir: Vec3,
    orig_start: Vec3,
    ignore: u16,
    multiplier: f32,
    mean: u8,
}

/// `BulletTraceResults`.
#[derive(Clone, Copy)]
struct Br {
    t: ShotTrace,
    hit_pos: Vec3,
    /// A damageable entity other than the map was hit.
    hit_ent: Option<u16>,
    depth_surface: usize,
}

impl Br {
    fn zeroed() -> Self {
        Self {
            t: ShotTrace::MISS,
            hit_pos: [0.0; 3],
            hit_ent: None,
            depth_surface: 0,
        }
    }
}

/// `BG_AdvanceTrace`: restarts the bullet past the surface it hit.
fn advance(bp: &mut Bp, br: &Br, dist: f32) -> bool {
    bp.ignore = br.t.hit;
    let (start, ok) = advance_trace(
        br.hit_pos,
        &bp.dir,
        &br.t.normal,
        dist,
        br.t.hit == ENTITYNUM_WORLD,
    );
    bp.start = start;
    ok
}

impl Game {
    /// `Bullet_Trace`.
    fn bullet_trace(&self, bp: &Bp, p: &BulletParams, last_surface: usize) -> Option<Br> {
        let mut t = self.shot_trace(
            bp.start,
            bp.end,
            bp.ignore,
            ENTITYNUM_NONE,
            contents::MASK_SHOT,
            p.info.rifle_bullet,
        );
        if !t.is_hit() {
            return None;
        }
        if t.hit != ENTITYNUM_WORLD && t.surface_flags == 0 {
            t.surface_flags = SURF_TYPE_FLESH;
        }
        let hit_pos = lerp(bp.start, bp.end, t.fraction);
        let mut depth_surface = surface_type(t.surface_flags);
        if t.surface_flags & SURF_NOPENETRATE != 0 {
            depth_surface = 0;
        } else if depth_surface == 0 && last_surface != 0 {
            depth_surface = last_surface;
        }
        Some(Br {
            hit_ent: (t.hit != ENTITYNUM_WORLD).then_some(t.hit),
            t,
            hit_pos,
            depth_surface,
        })
    }

    /// `Bullet_Process`: records the damage the hit deals.
    fn bullet_process(
        &self,
        bp: &Bp,
        br: &Br,
        p: &BulletParams,
        dflag: i32,
        out: &mut Vec<BulletHit>,
    ) {
        let target = br.hit_ent.unwrap_or(ENTITYNUM_WORLD);
        let damageable = br
            .hit_ent
            .is_some_and(|t| self.ent(t).is_some_and(|e| e.takedamage));
        let dist = length(sub(br.hit_pos, bp.orig_start));
        let mut flags = dflag;
        if p.info.armor_piercing {
            flags |= dflags::NO_ARMOR;
        }
        out.push(BulletHit {
            target,
            damageable,
            normal: br.t.normal,
            surface: surface_type(br.t.surface_flags) as u8,
            no_impact: br.t.surface_flags & (SURF_NOIMPACT | SURF_SKY) != 0,
            point: br.hit_pos,
            dir: bp.dir,
            damage: bullet_damage(p.info, dist, bp.multiplier),
            flags,
            mean: bp.mean,
            hitloc: br.t.hitloc,
        });
    }

    /// `Bullet_FireExtended`: a bullet that does not penetrate passes through glass, and
    /// through a player's body when it is a rifle bullet (at half damage).
    fn bullet_fire_extended(&self, bp: &mut Bp, p: &BulletParams, out: &mut Vec<BulletHit>) {
        for _ in 0..MAX_BULLET_EXTENSIONS {
            let Some(br) = self.bullet_trace(bp, p, 0) else {
                return;
            };
            self.bullet_process(bp, &br, p, 0, out);
            if br.t.contents & contents::GLASS != 0 {
                if !advance(bp, &br, PENETRATION_ADVANCE) {
                    return;
                }
                continue;
            }
            let Some(hit) = br
                .hit_ent
                .filter(|e| self.ent(*e).is_some_and(|e| e.takedamage))
            else {
                return;
            };
            if !p.info.rifle_bullet
                || !self.is_client(hit)
                || (!p.friendly_fire && self.on_same_team(hit, p.attacker))
            {
                return;
            }
            bp.multiplier *= RIFLE_BODY_DAMAGE_SCALE;
            advance(bp, &br, 0.0);
        }
    }

    /// `Bullet_FirePenetrate`: the bullet goes on through thin material, weakened by the
    /// thickness it crossed.
    fn bullet_fire_penetrate(&self, bp: &mut Bp, p: &BulletParams, out: &mut Vec<BulletHit>) {
        let Some(mut br) = self.bullet_trace(bp, p, 0) else {
            return;
        };
        self.bullet_process(bp, &br, p, 0, out);
        let depth_of = |surface: usize| p.penetration.depth_for(p.info, surface, p.perks, p.params);
        for _ in 0..MAX_PENETRATIONS {
            let mut max_depth = depth_of(br.depth_surface);
            if max_depth <= 0.0 {
                return;
            }
            let last_hit = br.hit_pos;
            if !advance(bp, &br, PENETRATION_ADVANCE) {
                return;
            }
            let next = self.bullet_trace(bp, p, br.depth_surface);
            let trace_hit = next.is_some();
            br = next.unwrap_or_else(Br::zeroed);

            let mut rev_bp = *bp;
            rev_bp.dir = [-bp.dir[0], -bp.dir[1], -bp.dir[2]];
            rev_bp.start = bp.end;
            rev_bp.end = mad(last_hit, PENETRATION_REVERSE_ADVANCE, rev_bp.dir);
            let mut rev_br = br;
            rev_br.t.normal = [-br.t.normal[0], -br.t.normal[1], -br.t.normal[2]];
            if trace_hit {
                advance(&mut rev_bp, &rev_br, PENETRATION_REVERSE_ADVANCE);
            }
            let rev = self.bullet_trace(&rev_bp, p, rev_br.depth_surface);
            let rev_hit = rev.is_some();
            let rev_br = rev.unwrap_or_else(Br::zeroed);
            let all_solid =
                rev_hit && rev_br.t.all_solid || br.t.start_solid && rev_br.t.start_solid;

            if rev_hit || all_solid {
                let depth = if all_solid {
                    length(sub(rev_bp.end, rev_bp.start))
                } else {
                    length(sub(last_hit, rev_br.hit_pos))
                };
                if rev_hit {
                    let exit_depth = depth_of(rev_br.depth_surface);
                    if exit_depth < max_depth {
                        max_depth = exit_depth;
                    }
                    if max_depth <= 0.0 {
                        return;
                    }
                }
                bp.multiplier = penetrate_multiplier(bp.multiplier, depth, max_depth);
                if bp.multiplier <= 0.0 {
                    return;
                }
                if !all_solid && trace_hit {
                    self.bullet_process(bp, &br, p, dflags::PENETRATION, out);
                }
            } else if trace_hit {
                self.bullet_process(bp, &br, p, dflags::PENETRATION, out);
            }
            if !trace_hit {
                return;
            }
        }
    }

    /// `Bullet_Fire`: every bullet of one trigger pull (several pellets for a shotgun), from
    /// `aim` with `spread` degrees of scatter. `time` seeds the scatter.
    pub fn bullet_hits(
        &self,
        p: &BulletParams,
        aim: &AimBasis,
        spread: f32,
        time: i32,
        out: &mut Vec<BulletHit>,
    ) {
        let penetrates =
            p.params.bullet_penetration_enabled && p.info.penetrate_type != PenetrateType::None;
        for shot in bullet_shots(p.info, aim, spread, time) {
            let mut bp = Bp {
                start: shot.start,
                end: shot.end,
                dir: shot.dir,
                orig_start: shot.start,
                ignore: p.attacker,
                multiplier: 1.0,
                mean: if p.info.rifle_bullet {
                    MOD_RIFLE_BULLET
                } else {
                    MOD_PISTOL_BULLET
                },
            };
            if penetrates {
                self.bullet_fire_penetrate(&mut bp, p, out);
            } else {
                self.bullet_fire_extended(&mut bp, p, out);
            }
        }
    }
}
