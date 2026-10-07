// SPDX-License-Identifier: GPL-3.0-or-later
//! Server-side player body animation: which `pb_*` animation a player is in, how far through
//! it, and the resulting skeleton for locational hits.
//!
//! The original runs `playeranim.script` against per-client conditions (stance, move type,
//! strafing, weapon class and player anim type, ADS, ...) every frame. The server only needs a
//! body that is in the right place for a shot, so selection here is a compact table instead of
//! the script interpreter. What it keeps from the script:
//!
//! * stance (stand, crouch, prone) and whether the player is idle, walking, running or
//!   sprinting (`player_moveThreshhold` speed gate, `PMF_WALKING`, `PMF_SPRINTING`);
//! * the movement direction as forward, back, strafe left or strafe right;
//! * ADS idle poses;
//! * four weapon families chosen from the weapon's player anim type and class: pistol,
//!   rocket launcher, "hold" (carrying the bomb/briefcase) and everything else as a two-handed
//!   rifle;
//! * ladder climbing, the laststand idle, and death animations by what the player was doing.
//!
//! What it drops: the `torso` partial animations (`pt_*` fire, reload, melee, pain, flinch),
//! turn-in-place animations, jump/land/shellshock blends, the grenade-specific, SMG-crouch and
//! unarmed variants (they use the rifle set), mantle animations (`mp_mantle_*`, not `pb_*`;
//! a mantling player holds its idle pose), and the random choice among several death
//! animations (the first listed is used). Animations cross-fade over 100 ms when the selection
//! changes, which is the script's default `blendtime`.

use std::collections::HashMap;
use std::sync::Arc;

use assets::zone::weapon::WeaponDef;
use sim::pm::{PlayerState, PmType, ef, pmf};
use sim::skel::controllers::{self, ControllerInput};
use sim::skel::hitloc::{BULLET_PRIORITY, RIFLE_PRIORITY};
use sim::skel::{AnimBinding, AnimLayer, LocHit, Placement, Pose, Rig, RigModel, Stance};

use crate::content::{Content, PlayerAnim};

/// `player_moveThreshhold` default: below this horizontal speed a player is idle.
pub const MOVE_THRESHOLD: f32 = 10.0;
/// Cross-fade between two selected animations, seconds.
pub const BLEND_SECONDS: f32 = 0.1;

/// `playeranimtypes.txt` indices the selection uses.
pub mod anim_type {
    pub const NONE: i32 = 0;
    pub const PISTOL: i32 = 2;
    pub const ROCKETLAUNCHER: i32 = 7;
    pub const HOLD: i32 = 13;
    pub const BRIEFCASE: i32 = 14;
}

/// `weaponClass_t` values the selection uses.
pub mod weap_class {
    pub const PISTOL: i32 = 4;
    pub const ROCKETLAUNCHER: i32 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Rifle,
    Pistol,
    Rpg,
    Hold,
}

/// Direction slots: forward, back, strafe left, strafe right.
type Dirs = [&'static str; 4];

/// One family's animations, indexed by stance (stand, crouch, prone).
struct Set {
    idle: [&'static str; 3],
    ads: [&'static str; 3],
    walk: [Dirs; 3],
    run: [Dirs; 3],
    sprint: &'static str,
}

const CRAWL: Dirs = [
    "pb_prone_crawl",
    "pb_prone_crawl_back",
    "pb_prone_crawl_left",
    "pb_prone_crawl_right",
];

const RIFLE: Set = Set {
    idle: ["pb_stand_alert", "pb_crouch_alert", "pb_prone_aim"],
    ads: ["pb_stand_ads", "pb_crouch_ads", "pb_prone_aim"],
    walk: [
        [
            "pb_stand_shoot_walk_forward",
            "pb_stand_shoot_walk_back",
            "pb_stand_shoot_walk_left",
            "pb_stand_shoot_walk_right",
        ],
        [
            "pb_crouch_shoot_run_forward",
            "pb_crouch_shoot_run_back",
            "pb_crouch_shoot_run_left",
            "pb_crouch_shoot_run_right",
        ],
        CRAWL,
    ],
    run: [
        [
            "pb_combatrun_forward_loop",
            "pb_combatrun_back_loop",
            "pb_combatrun_left_loop",
            "pb_combatrun_right_loop",
        ],
        [
            "pb_crouch_run_forward",
            "pb_crouch_run_back",
            "pb_crouch_run_left",
            "pb_crouch_run_right",
        ],
        CRAWL,
    ],
    sprint: "pb_sprint",
};

const PISTOL: Set = Set {
    idle: [
        "pb_stand_alert_pistol",
        "pb_crouch_alert_pistol",
        "pb_prone_aim_pistol",
    ],
    ads: [
        "pb_stand_ads_pistol",
        "pb_crouch_ads_pistol",
        "pb_prone_aim_pistol",
    ],
    walk: [
        [
            "pb_combatwalk_forward_loop_pistol",
            "pb_combatwalk_back_loop_pistol",
            "pb_combatwalk_left_loop_pistol",
            "pb_combatwalk_right_loop_pistol",
        ],
        [
            "pb_crouch_walk_forward_pistol",
            "pb_crouch_walk_back_pistol",
            "pb_crouch_walk_left_pistol",
            "pb_crouch_walk_right_pistol",
        ],
        CRAWL,
    ],
    run: [
        [
            "pb_pistol_run_fast",
            "pb_combatrun_back_loop_pistol",
            "pb_combatrun_left_loop_pistol",
            "pb_combatrun_right_loop_pistol",
        ],
        [
            "pb_crouch_run_forward_pistol",
            "pb_crouch_run_back_pistol",
            "pb_crouch_run_left_pistol",
            "pb_crouch_run_right_pistol",
        ],
        CRAWL,
    ],
    sprint: "pb_sprint_pistol",
};

const RPG: Set = Set {
    idle: [
        "pb_stand_alert_RPG",
        "pb_crouch_alert_RPG",
        "pb_prone_aim_RPG",
    ],
    ads: ["pb_stand_ads_RPG", "pb_crouch_ads_RPG", "pb_prone_aim_RPG"],
    walk: [
        [
            "pb_walk_forward_RPG_ads",
            "pb_walk_back_RPG_ads",
            "pb_walk_left_RPG_ads",
            "pb_walk_right_RPG_ads",
        ],
        [
            "pb_crouch_walk_forward_RPG",
            "pb_crouch_walk_back_RPG",
            "pb_crouch_walk_left_RPG",
            "pb_crouch_walk_right_RPG",
        ],
        CRAWL,
    ],
    run: [
        [
            "pb_combatrun_forward_RPG",
            "pb_combatrun_back_RPG",
            "pb_combatrun_left_RPG",
            "pb_combatrun_right_RPG",
        ],
        [
            "pb_crouch_run_forward_RPG",
            "pb_crouch_run_back_RPG",
            "pb_crouch_run_left_RPG",
            "pb_crouch_run_right_RPG",
        ],
        CRAWL,
    ],
    sprint: "pb_sprint_RPG",
};

const HOLD: Set = {
    const STAND: Dirs = [
        "pb_hold_run",
        "pb_hold_run_back",
        "pb_hold_run_left",
        "pb_hold_run_right",
    ];
    const CROUCH: Dirs = [
        "pb_crouch_hold_run",
        "pb_crouch_hold_run_back",
        "pb_crouch_hold_run_left",
        "pb_crouch_hold_run_right",
    ];
    const PRONE: Dirs = [
        "pb_prone_crawl_hold",
        "pb_prone_crawl_back_hold",
        "pb_prone_crawl_left_hold",
        "pb_prone_crawl_right_hold",
    ];
    Set {
        idle: ["pb_hold_idle", "pb_crouch_hold_idle", "pb_prone_hold"],
        ads: ["pb_hold_idle", "pb_crouch_hold_idle", "pb_prone_hold"],
        walk: [STAND, CROUCH, PRONE],
        run: [STAND, CROUCH, PRONE],
        sprint: "pb_sprint_hold",
    }
};

const SETS: [(Family, &Set); 4] = [
    (Family::Rifle, &RIFLE),
    (Family::Pistol, &PISTOL),
    (Family::Rpg, &RPG),
    (Family::Hold, &HOLD),
];

const CLIMB_UP: &str = "pb_climbup";
const CLIMB_DOWN: &str = "pb_climbdown";
const LASTSTAND_IDLE: &str = "pb_laststand_idle";
const LASTSTAND_DEATH: &str = "pb_laststand_death";

/// Death animations: what the player was doing when hit.
const DEATH_STAND: &str = "pb_stand_death_neckdeath";
const DEATH_CROUCH: &str = "pb_crouch_death_headshot_front";
const DEATH_PRONE: &str = "pb_prone_death_quickdeath";
const DEATH_RUN: Dirs = [
    "pb_death_run_forward_crumple",
    "pb_death_run_back",
    "pb_death_run_left",
    "pb_death_run_right",
];
const DEATH_CROUCH_RUN: &str = "pb_crouchrun_death_drop";

fn set(f: Family) -> &'static Set {
    SETS.iter()
        .find(|(g, _)| *g == f)
        .map(|(_, s)| *s)
        .unwrap_or(&RIFLE)
}

/// Every animation name selection can return.
fn all_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for (_, s) in SETS {
        v.extend(s.idle);
        v.extend(s.ads);
        for d in s.walk.iter().chain(&s.run) {
            v.extend(d);
        }
        v.push(s.sprint);
    }
    v.extend([
        CLIMB_UP,
        CLIMB_DOWN,
        LASTSTAND_IDLE,
        LASTSTAND_DEATH,
        DEATH_STAND,
        DEATH_CROUCH,
        DEATH_PRONE,
        DEATH_CROUCH_RUN,
    ]);
    v.extend(DEATH_RUN);
    v.sort_unstable();
    v.dedup();
    v
}

/// Which way the legs are going relative to the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dir {
    #[default]
    Forward,
    Back,
    Left,
    Right,
}

impl Dir {
    /// `movementDir` degrees (positive to the left of the view) to a direction slot.
    pub fn from_degrees(d: f32) -> Dir {
        if d.abs() <= 45.0 {
            Dir::Forward
        } else if (45.0..=135.0).contains(&d) {
            Dir::Left
        } else if (-135.0..=-45.0).contains(&d) {
            Dir::Right
        } else {
            Dir::Back
        }
    }

    fn slot(self) -> usize {
        match self {
            Dir::Forward => 0,
            Dir::Back => 1,
            Dir::Left => 2,
            Dir::Right => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Idle,
    Walk,
    Run,
    Sprint,
}

/// What the pose needs of a player, once per server frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayerPoseInput {
    pub stance: StanceInput,
    /// Horizontal speed, `|velocity.xy|`.
    pub speed: f32,
    /// The player is pressing a movement key (`forwardmove` or `rightmove` nonzero).
    pub trying_to_move: bool,
    /// `PMF_WALKING` (ADS or the walk key) or leaning.
    pub walking: bool,
    pub sprinting: bool,
    /// `ps.movement_dir` in degrees.
    pub move_dir: f32,
    pub ladder: bool,
    /// Vertical speed, to pick climb up or down on a ladder.
    pub vertical_speed: f32,
    pub mantle: bool,
    pub ads: bool,
    pub laststand: bool,
    pub dead: bool,
    /// The weapon's `player_anim_type` and `weap_class`.
    pub anim_type: i32,
    pub weap_class: i32,
    pub view_pitch: f32,
    pub yaw: f32,
    pub torso_pitch: f32,
    pub waist_pitch: f32,
    /// Mounted on a turret: the aim controllers stay at rest.
    pub turret: bool,
}

/// [`Stance`] with a `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StanceInput {
    #[default]
    Stand,
    Crouch,
    Prone,
}

impl StanceInput {
    fn index(self) -> usize {
        self as usize
    }

    pub fn stance(self) -> Stance {
        match self {
            StanceInput::Stand => Stance::Stand,
            StanceInput::Crouch => Stance::Crouch,
            StanceInput::Prone => Stance::Prone,
        }
    }
}

impl PlayerPoseInput {
    /// Reads a player state. `trying_to_move` comes from the usercmd (`forwardmove` or
    /// `rightmove` nonzero); `weapon` is the weapon the state's `weapon` index names.
    pub fn from_ps(ps: &PlayerState, trying_to_move: bool, weapon: Option<&WeaponDef>) -> Self {
        let stance = if ps.pm_flags & pmf::PRONE != 0 {
            StanceInput::Prone
        } else if ps.pm_flags & pmf::DUCKED != 0 {
            StanceInput::Crouch
        } else {
            StanceInput::Stand
        };
        Self {
            stance,
            speed: (ps.velocity[0] * ps.velocity[0] + ps.velocity[1] * ps.velocity[1]).sqrt(),
            trying_to_move,
            walking: ps.pm_flags & pmf::WALKING != 0 || ps.leanf != 0.0,
            sprinting: ps.pm_flags & pmf::SPRINTING != 0,
            move_dir: f32::from(ps.movement_dir),
            ladder: ps.pm_flags & pmf::LADDER != 0,
            vertical_speed: ps.velocity[2],
            mantle: ps.pm_flags & pmf::MANTLE != 0,
            ads: ps.weapon_pos_frac >= 1.0,
            laststand: ps.pm_type == PmType::LastStand,
            dead: matches!(ps.pm_type, PmType::Dead | PmType::DeadLinked),
            anim_type: weapon.map_or(anim_type::NONE, |w| w.player_anim_type),
            weap_class: weapon.map_or(0, |w| w.weap_class),
            view_pitch: ps.viewangles[0],
            yaw: ps.viewangles[1],
            torso_pitch: ps.torso_pitch,
            waist_pitch: ps.waist_pitch,
            turret: ps.e_flags & ef::TURRET_ACTIVE != 0,
        }
    }

    fn family(&self) -> Family {
        if self.anim_type == anim_type::HOLD || self.anim_type == anim_type::BRIEFCASE {
            Family::Hold
        } else if self.weap_class == weap_class::ROCKETLAUNCHER
            || self.anim_type == anim_type::ROCKETLAUNCHER
        {
            Family::Rpg
        } else if self.weap_class == weap_class::PISTOL || self.anim_type == anim_type::PISTOL {
            Family::Pistol
        } else {
            Family::Rifle
        }
    }

    fn motion(&self) -> Motion {
        if self.speed < MOVE_THRESHOLD || !self.trying_to_move {
            Motion::Idle
        } else if self.sprinting && self.stance == StanceInput::Stand {
            Motion::Sprint
        } else if self.walking {
            Motion::Walk
        } else {
            Motion::Run
        }
    }

    /// The `pb_*` animation this input plays.
    pub fn select(&self) -> &'static str {
        if self.dead {
            return self.death();
        }
        if self.laststand {
            return LASTSTAND_IDLE;
        }
        if self.ladder {
            return if self.vertical_speed < 0.0 {
                CLIMB_DOWN
            } else {
                CLIMB_UP
            };
        }
        let s = set(self.family());
        let st = self.stance.index();
        let dir = Dir::from_degrees(self.move_dir).slot();
        match self.motion() {
            _ if self.mantle => s.idle[st],
            Motion::Idle if self.ads => s.ads[st],
            Motion::Idle => s.idle[st],
            Motion::Walk => s.walk[st][dir],
            Motion::Run => s.run[st][dir],
            Motion::Sprint => s.sprint,
        }
    }

    fn death(&self) -> &'static str {
        if self.laststand {
            return LASTSTAND_DEATH;
        }
        let moving = self.motion() != Motion::Idle;
        match (self.stance, moving) {
            (StanceInput::Prone, _) => DEATH_PRONE,
            (StanceInput::Crouch, true) => DEATH_CROUCH_RUN,
            (StanceInput::Crouch, false) => DEATH_CROUCH,
            (StanceInput::Stand, true) => DEATH_RUN[Dir::from_degrees(self.move_dir).slot()],
            (StanceInput::Stand, false) => DEATH_STAND,
        }
    }

    fn controllers(&self) -> sim::skel::Controllers {
        controllers::compute(&ControllerInput {
            view_pitch: self.view_pitch,
            move_dir: if self.motion() == Motion::Idle {
                0.0
            } else {
                self.move_dir
            },
            prone: self.stance == StanceInput::Prone,
            torso_pitch: self.torso_pitch,
            waist_pitch: self.waist_pitch,
            no_aim: self.turret || self.mantle || self.ladder || self.dead,
        })
    }
}

struct Slot {
    anim: Arc<PlayerAnim>,
    bind: AnimBinding,
    /// Seconds per loop or run.
    length: f32,
}

/// The skeleton of one body/head model pair with the `pb_*` animations bound to it.
pub struct PlayerAnims {
    rig: Rig,
    slots: HashMap<&'static str, Slot>,
    missing: Vec<&'static str>,
}

impl PlayerAnims {
    /// Builds the rig from the loaded models (`head` melds onto `body` by bone name) and binds
    /// every retained animation the selection can return. Animations the zones lack are listed
    /// by [`PlayerAnims::missing`].
    pub fn new(content: &Content, body: &str, head: Option<&str>) -> Result<Self, String> {
        let spec =
            |name: &str| -> Result<(Arc<assets::zone::xmodel::XModel>, Vec<Arc<str>>), String> {
                let m = content
                    .model(name)
                    .ok_or_else(|| format!("model {name} not loaded"))?;
                let names = content
                    .model_bone_names(name)
                    .ok_or_else(|| format!("model {name} has no bone names"))?;
                Ok((m.clone(), names.to_vec()))
            };
        let mut parts = vec![spec(body)?];
        if let Some(h) = head {
            parts.push(spec(h)?);
        }
        let texts: Vec<Vec<&str>> = parts
            .iter()
            .map(|(_, n)| n.iter().map(|s| &**s).collect())
            .collect();
        let models: Vec<RigModel> = parts
            .iter()
            .zip(&texts)
            .map(|((m, _), t)| RigModel {
                model: m.clone(),
                bone_names: t,
                attach: None,
            })
            .collect();
        let rig = Rig::new(&models)?;
        let mut slots = HashMap::new();
        let mut missing = Vec::new();
        for name in all_names() {
            match content.player_anim(name) {
                Some(a) => {
                    let p = &a.parts;
                    let length = f32::from(p.num_frames) / p.frame_rate;
                    slots.insert(
                        name,
                        Slot {
                            bind: rig.bind(&a.part_names),
                            anim: a.clone(),
                            length,
                        },
                    );
                }
                None => missing.push(name),
            }
        }
        Ok(Self {
            rig,
            slots,
            missing,
        })
    }

    pub fn rig(&self) -> &Rig {
        &self.rig
    }

    /// Selectable animations the loaded zones do not have.
    pub fn missing(&self) -> &[&'static str] {
        &self.missing
    }

    fn layer(&self, name: &'static str, seconds: f32, weight: f32) -> Option<AnimLayer<'_>> {
        let s = self.slots.get(name)?;
        let time = if s.length > 0.0 {
            seconds / s.length
        } else {
            0.0
        };
        Some(AnimLayer {
            anim: &s.anim.parts,
            bind: &s.bind,
            time,
            weight,
        })
    }
}

/// One player's animation clock: the current and previous selection and how far into each.
#[derive(Debug, Clone, Default)]
pub struct PlayerPoseState {
    input: PlayerPoseInput,
    current: Option<&'static str>,
    seconds: f32,
    previous: Option<(&'static str, f32)>,
    /// Seconds of cross-fade left.
    blend: f32,
}

impl PlayerPoseState {
    /// Advances the clocks by `dt` seconds and switches animation when `input` selects another.
    pub fn update(&mut self, dt: f32, input: &PlayerPoseInput) {
        let want = if !input.dead {
            input.select()
        } else if self.input.dead {
            // A corpse keeps the animation it died in even as its speed decays.
            self.current.unwrap_or_else(|| input.select())
        } else {
            // A death is chosen from what the player was doing the frame before.
            self.input.dead_selection(input)
        };
        match self.current {
            Some(c) if c == want => self.seconds += dt,
            Some(c) => {
                self.previous = Some((c, self.seconds + dt));
                self.blend = BLEND_SECONDS;
                self.current = Some(want);
                self.seconds = dt;
            }
            None => {
                self.current = Some(want);
                self.seconds = dt;
            }
        }
        if let Some((_, t)) = &mut self.previous {
            *t += dt;
        }
        self.blend = (self.blend - dt).max(0.0);
        if self.blend == 0.0 {
            self.previous = None;
        }
        self.input = *input;
    }

    /// The animation currently selected.
    pub fn current(&self) -> Option<&'static str> {
        self.current
    }

    /// Poses `anims`' rig for the last input into `out` (entity space; see
    /// [`sim::skel::Pose::to_world`]).
    pub fn pose(&self, anims: &PlayerAnims, out: &mut Pose) {
        let ctl = self.input.controllers();
        let w_prev = if self.previous.is_some() {
            self.blend / BLEND_SECONDS
        } else {
            0.0
        };
        let mut layers: [Option<AnimLayer>; 2] = [None, None];
        if let Some(c) = self.current {
            layers[0] = anims.layer(c, self.seconds, 1.0 - w_prev);
        }
        if let Some((p, t)) = self.previous {
            layers[1] = anims.layer(p, t, w_prev);
        }
        let [a, b] = layers;
        match (a, b) {
            (Some(a), Some(b)) => anims.rig.pose(&[a, b], &ctl, out),
            (Some(l), None) | (None, Some(l)) => {
                anims.rig.pose(&[AnimLayer { weight: 1.0, ..l }], &ctl, out)
            }
            (None, None) => anims.rig.pose(&[], &ctl, out),
        }
    }

    /// Locational trace of a world-space segment against this player (standing at `origin`,
    /// facing the last input's yaw), for use after the coarse box clip hit the entity. `rifle`
    /// selects the rifle priority map (`bRifleBullet`). Returns `None` for a miss; the caller
    /// falls back to [`sim::skel::box_hit_location`] when it has no [`PlayerAnims`].
    pub fn trace(
        &self,
        anims: &PlayerAnims,
        origin: &sim::Vec3,
        start: &sim::Vec3,
        end: &sim::Vec3,
        rifle: bool,
        max_fraction: f32,
    ) -> Option<LocHit> {
        let mut pose = Pose::default();
        self.pose(anims, &mut pose);
        sim::skel::trace_player(
            &anims.rig,
            &pose,
            &Placement {
                origin: *origin,
                angles: [0.0, self.input.yaw, 0.0],
            },
            start,
            end,
            if rifle {
                &RIFLE_PRIORITY
            } else {
                &BULLET_PRIORITY
            },
            max_fraction,
        )
    }
}

impl PlayerPoseInput {
    /// The death animation for the frame `now` flips `dead`, chosen from this (previous)
    /// input's stance and motion.
    fn dead_selection(&self, now: &PlayerPoseInput) -> &'static str {
        PlayerPoseInput {
            dead: true,
            laststand: self.laststand || now.laststand,
            ..*self
        }
        .select()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> PlayerPoseInput {
        PlayerPoseInput {
            speed: 190.0,
            trying_to_move: true,
            ..Default::default()
        }
    }

    #[test]
    fn motion_needs_speed_and_intent() {
        let idle = PlayerPoseInput::default();
        assert_eq!(idle.select(), "pb_stand_alert");
        assert_eq!(
            PlayerPoseInput {
                speed: 5.0,
                trying_to_move: true,
                ..idle
            }
            .select(),
            "pb_stand_alert"
        );
        assert_eq!(
            PlayerPoseInput {
                speed: 190.0,
                ..idle
            }
            .select(),
            "pb_stand_alert",
            "coasting is not running"
        );
        assert_eq!(running().select(), "pb_combatrun_forward_loop");
        assert_eq!(
            PlayerPoseInput {
                walking: true,
                ..running()
            }
            .select(),
            "pb_stand_shoot_walk_forward"
        );
    }

    #[test]
    fn sprint_is_standing_only() {
        let sprint = PlayerPoseInput {
            sprinting: true,
            ..running()
        };
        assert_eq!(sprint.select(), "pb_sprint");
        let crouched = PlayerPoseInput {
            stance: StanceInput::Crouch,
            ..sprint
        };
        assert_eq!(crouched.select(), "pb_crouch_run_forward");
    }

    #[test]
    fn direction_buckets_follow_movement_dir() {
        let at = |d: f32| {
            PlayerPoseInput {
                move_dir: d,
                ..running()
            }
            .select()
        };
        assert_eq!(at(0.0), "pb_combatrun_forward_loop");
        assert_eq!(at(45.0), "pb_combatrun_forward_loop");
        assert_eq!(at(90.0), "pb_combatrun_left_loop");
        assert_eq!(at(-90.0), "pb_combatrun_right_loop");
        assert_eq!(at(180.0), "pb_combatrun_back_loop");
        assert_eq!(at(-150.0), "pb_combatrun_back_loop");
    }

    #[test]
    fn weapon_family_comes_from_class_and_anim_type() {
        let with = |anim_type, weap_class| {
            PlayerPoseInput {
                anim_type,
                weap_class,
                ..Default::default()
            }
            .select()
        };
        assert_eq!(
            with(anim_type::PISTOL, weap_class::PISTOL),
            "pb_stand_alert_pistol"
        );
        assert_eq!(
            with(anim_type::ROCKETLAUNCHER, weap_class::ROCKETLAUNCHER),
            "pb_stand_alert_RPG"
        );
        assert_eq!(with(anim_type::BRIEFCASE, 0), "pb_hold_idle");
        assert_eq!(with(4, 0), "pb_stand_alert", "autorifles use the rifle set");
        let ads = PlayerPoseInput {
            ads: true,
            ..Default::default()
        };
        assert_eq!(ads.select(), "pb_stand_ads");
        assert_eq!(
            PlayerPoseInput {
                ads: true,
                stance: StanceInput::Prone,
                ..Default::default()
            }
            .select(),
            "pb_prone_aim"
        );
    }

    #[test]
    fn ladder_and_laststand_override_movement() {
        let ladder = PlayerPoseInput {
            ladder: true,
            vertical_speed: -50.0,
            ..running()
        };
        assert_eq!(ladder.select(), "pb_climbdown");
        assert_eq!(
            PlayerPoseInput {
                vertical_speed: 50.0,
                ..ladder
            }
            .select(),
            "pb_climbup"
        );
        assert_eq!(
            PlayerPoseInput {
                laststand: true,
                ..running()
            }
            .select(),
            "pb_laststand_idle"
        );
    }

    #[test]
    fn a_death_animation_is_fixed_by_what_the_player_was_doing() {
        let mut s = PlayerPoseState::default();
        let run = running();
        s.update(0.033, &run);
        assert_eq!(s.current(), Some("pb_combatrun_forward_loop"));
        // Killed while running forward: the run death, and it stays even once the corpse stops.
        let dead = PlayerPoseInput { dead: true, ..run };
        s.update(0.033, &dead);
        assert_eq!(s.current(), Some("pb_death_run_forward_crumple"));
        s.update(
            0.033,
            &PlayerPoseInput {
                dead: true,
                speed: 0.0,
                ..PlayerPoseInput::default()
            },
        );
        assert_eq!(s.current(), Some("pb_death_run_forward_crumple"));
        // Killed crouched and still.
        let mut s = PlayerPoseState::default();
        let crouch = PlayerPoseInput {
            stance: StanceInput::Crouch,
            ..Default::default()
        };
        s.update(0.033, &crouch);
        s.update(
            0.033,
            &PlayerPoseInput {
                dead: true,
                ..crouch
            },
        );
        assert_eq!(s.current(), Some("pb_crouch_death_headshot_front"));
    }

    #[test]
    fn cross_fade_runs_out_and_clocks_advance() {
        let mut s = PlayerPoseState::default();
        s.update(0.033, &PlayerPoseInput::default());
        s.update(0.033, &running());
        assert!(s.previous.is_some() && s.blend > 0.0);
        assert!(
            (s.seconds - 0.033).abs() < 1e-6,
            "the new animation starts at its beginning"
        );
        for _ in 0..4 {
            s.update(0.033, &running());
        }
        assert!(s.previous.is_none(), "fade is over after {BLEND_SECONDS} s");
        assert!((s.seconds - 0.165).abs() < 1e-5);
    }
}
