// SPDX-License-Identifier: GPL-3.0-only
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
//! * ladder climbing, the laststand idle, and death animations by what the player was doing;
//! * the damage stumble: a player who is moving when hit plays `pb_stumble_*` for the stumble window
//!   (`player_dmgtimer_stumbleTime`), by weapon family and strafe direction.
//!
//! On top of the legs clip runs the torso channel, the script's `torso` entries (`pt_*`): the clip a weapon
//! event selects (fire, reload, melee, grenade throw, weapon pullout, flinch) plays over the bones it names and
//! leaves the legs to the locomotion clip. Events come from what a snapshot carries of a player: the weapon
//! state's edges, the player event ring, and the damage timer. Each event picks its clip the way the script's
//! `EVENTS` block orders its conditions (weapon class and player anim type, stance, moving or not, ADS,
//! last stand).
//!
//! What it drops: turn-in-place animations, jump/land/shellshock blends, the SMG-crouch and unarmed variants
//! (they use the rifle set), mantle animations (`mp_mantle_*`, not `pb_*`; a mantling player holds its idle
//! pose), the random choice among several death animations (the first listed is used; melee variants cycle
//! instead of being random so every machine shows the same one) and the `knife_melee` event (the knife swing
//! plays the `meleeattack` clips). Animations cross-fade over 100 ms when the selection changes, which is the
//! script's default `blendtime`.

use std::collections::HashMap;
use std::sync::Arc;

use assets::zone::weapon::WeaponDef;
use sim::pm::{PlayerState, PmType, ef, ev, pmf, weapon_state as ws};
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
    pub const OTHER: i32 = 1;
    pub const PISTOL: i32 = 2;
    pub const SMG: i32 = 3;
    pub const AUTORIFLE: i32 = 4;
    pub const SNIPER: i32 = 6;
    pub const ROCKETLAUNCHER: i32 = 7;
    pub const M203: i32 = 12;
    pub const HOLD: i32 = 13;
    pub const BRIEFCASE: i32 = 14;
}

/// `weaponClass_t` values the selection uses.
pub mod weap_class {
    pub const RIFLE: i32 = 0;
    pub const MG: i32 = 1;
    pub const SMG: i32 = 2;
    pub const PISTOL: i32 = 4;
    pub const GRENADE: i32 = 5;
    pub const ROCKETLAUNCHER: i32 = 6;
}

/// `player_dmgtimer_flinchTime` and `player_dmgtimer_stumbleTime` defaults, milliseconds: how long after a hit the
/// flinch and stumble animations play.
pub const FLINCH_MS: i32 = 500;
pub const STUMBLE_MS: i32 = 500;

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

/// Stumble animations by weapon family (rifle, pistol, grenade) and direction slot (forward, back, left, right).
const STUMBLE_RUN: [Dirs; 3] = [
    [
        "pb_stumble_forward",
        "pb_stumble_back",
        "pb_stumble_left",
        "pb_stumble_right",
    ],
    [
        "pb_stumble_pistol_forward",
        "pb_stumble_pistol_back",
        "pb_stumble_pistol_left",
        "pb_stumble_pistol_right",
    ],
    [
        "pb_stumble_grenade_forward",
        "pb_stumble_grenade_back",
        "pb_stumble_grenade_left",
        "pb_stumble_grenade_right",
    ],
];
/// Walking stumbles: rifle, then pistol and grenade alike.
const STUMBLE_WALK: [Dirs; 2] = [
    [
        "pb_stumble_walk_forward",
        "pb_stumble_walk_back",
        "pb_stumble_walk_left",
        "pb_stumble_walk_right",
    ],
    [
        "pb_stumble_pistol_walk_forward",
        "pb_stumble_pistol_walk_back",
        "pb_stumble_pistol_walk_left",
        "pb_stumble_pistol_walk_right",
    ],
];
const STUMBLE_SPRINT: &str = "pb_stumble_forward";

/// `pb_*` animations the torso channel plays as a whole-body (`both`) entry.
const TORSO_BODY_CLIPS: &[&str] = &["pb_crouch_grenade_throw", "pb_stand_grenade_throw"];

/// Every `pt_*` animation the torso channel can select.
const TORSO_CLIPS: &[&str] = &[
    "pt_laststand_fire",
    "pt_prone_shoot_pistol",
    "pt_crouch_shoot_ads_pistol",
    "pt_crouch_shoot_pistol",
    "pt_stand_shoot_pistol",
    "pt_prone_shoot_auto",
    "pt_crouch_shoot_auto_ads",
    "pt_crouch_shoot_auto",
    "pt_stand_shoot_auto_ads",
    "pt_stand_shoot_auto",
    "pt_crouch_shoot_ads",
    "pt_prone_shoot_RPG",
    "pt_stand_shoot_RPG",
    "pt_hold_prone_throw",
    "pt_hold_throw",
    "pt_prone_grenade_throw",
    "pt_crouch_grenade_throw",
    "pt_stand_grenade_throw",
    "pt_crouch_shoot",
    "pt_rifle_fire_ads",
    "pt_rifle_fire",
    "pt_stand_shoot_shotgun",
    "pt_stand_shoot_ads",
    "pt_stand_shoot",
    "pt_melee_prone_pistol",
    "pt_melee_prone",
    "pt_melee_crouch_left2left",
    "pt_melee_crouch_left2right",
    "pt_melee_crouch_right2left",
    "pt_melee_right2right_1",
    "pt_melee_right2right_2",
    "pt_melee_right2left",
    "pt_melee_left2left_1",
    "pt_melee_left2right",
    "pt_prone_pullout_pose",
    "pt_crouch_pullout_pose",
    "pt_stand_pullout_pose",
    "pt_laststand_reload",
    "pt_reload_crouch_pistol",
    "pt_reload_crouchwalk_pistol",
    "pt_reload_prone_pistol",
    "pt_reload_prone_RPG",
    "pt_reload_stand_RPG",
    "pt_reload_stand_pistol",
    "pt_reload_prone_auto",
    "pt_reload_stand_auto_mp40",
    "pt_reload_crouchwalk",
    "pt_reload_crouch_rifle",
    "pt_reload_stand_auto",
    "pt_reload_stand_rifle",
    "pt_flinch_pistol_forward",
    "pt_flinch_pistol_back",
    "pt_flinch_pistol_left",
    "pt_flinch_pistol_right",
    "pt_flinch_grenade_forward",
    "pt_flinch_grenade_back",
    "pt_flinch_grenade_left",
    "pt_flinch_grenade_right",
    "pt_flinch_forward",
    "pt_flinch_back",
    "pt_flinch_left",
    "pt_flinch_right",
];

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
    v.extend(STUMBLE_RUN.iter().flatten());
    v.extend(STUMBLE_WALK.iter().flatten());
    v.extend(TORSO_BODY_CLIPS.iter().copied());
    v.extend(TORSO_CLIPS.iter().copied());
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
    /// `ps.weapon_state` (`sim::pm::weapon_state`): its edges start the fire, reload, melee, throw and pullout clips.
    pub weapon_state: u8,
    /// The newest entry of the player event ring and its sequence number; a new sequence with a fire event starts
    /// another fire clip while the weapon state stays in `FIRING`.
    pub event: u8,
    pub event_seq: u8,
    /// `ps.damage_timer` and `ps.damage_duration`, milliseconds, and `ps.flinch_yaw_anim`.
    pub damage_timer: i32,
    pub damage_duration: i32,
    pub flinch_dir: u8,
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
            weapon_state: ps.weapon_state,
            event: ps.events[usize::from(ps.event_sequence.wrapping_sub(1) & 3)],
            event_seq: ps.event_sequence,
            damage_timer: ps.damage_timer,
            damage_duration: ps.damage_duration,
            flinch_dir: ps.flinch_yaw_anim,
        }
    }

    /// Inside the window after a hit in which the last `window` milliseconds of the damage timer still run
    /// (`PM_ShouldFlinch`, and the stumble test of `PM_GetMoveAnim`).
    fn recently_hit(&self, window: i32) -> bool {
        self.damage_timer > (self.damage_duration - window).max(0)
    }

    fn pistol_or_grenade(&self) -> bool {
        self.weap_class == weap_class::PISTOL || self.throws_grenade()
    }

    /// A grenade class weapon that is not an M203 launcher: the script's `weaponclass grenade, playerAnimType all
    /// NOT m203`.
    fn throws_grenade(&self) -> bool {
        self.weap_class == weap_class::GRENADE && self.anim_type != anim_type::M203
    }

    /// The `pb_stumble_*` animation of a moving player who was just hit.
    fn stumble(&self) -> Option<&'static str> {
        if !self.recently_hit(STUMBLE_MS) || self.stance == StanceInput::Prone || self.mantle {
            return None;
        }
        let dir = Dir::from_degrees(self.move_dir).slot();
        let pg = usize::from(self.pistol_or_grenade());
        Some(match self.motion() {
            Motion::Idle => return None,
            Motion::Sprint => STUMBLE_SPRINT,
            Motion::Walk => STUMBLE_WALK[pg][dir],
            Motion::Run => {
                let family = if self.stance != StanceInput::Stand {
                    pg
                } else if self.weap_class == weap_class::PISTOL {
                    1
                } else if self.throws_grenade() {
                    2
                } else {
                    0
                };
                STUMBLE_RUN[family][dir]
            }
        })
    }

    /// `movetype prone`, `crouching` (any crouched movement), `idlecr`, and `moving` of the script's conditions.
    fn prone(&self) -> bool {
        self.stance == StanceInput::Prone
    }

    fn crouching(&self) -> bool {
        self.stance == StanceInput::Crouch
    }

    fn moving(&self) -> bool {
        self.motion() != Motion::Idle
    }

    /// The script's `fireweapon` event: the torso clip and the cap on how long it plays (`duration`, seconds).
    fn fire_clip(&self) -> Option<(&'static str, Option<f32>)> {
        if self.turret {
            return None;
        }
        if self.laststand {
            return Some(("pt_laststand_fire", None));
        }
        if self.throws_grenade() {
            return self.throw_clip(self.anim_type == anim_type::HOLD);
        }
        let (prone, mv, crouch, ads) = (self.prone(), self.moving(), self.crouching(), self.ads);
        let auto = matches!(self.weap_class, weap_class::MG | weap_class::SMG);
        let clip = if self.weap_class == weap_class::PISTOL {
            match () {
                _ if prone => "pt_prone_shoot_pistol",
                _ if mv => return None,
                _ if crouch && ads => "pt_crouch_shoot_ads_pistol",
                _ if crouch => "pt_crouch_shoot_pistol",
                _ => "pt_stand_shoot_pistol",
            }
        } else if auto {
            match () {
                _ if prone => "pt_prone_shoot_auto",
                _ if mv => return None,
                _ if crouch && ads => "pt_crouch_shoot_auto_ads",
                _ if crouch => "pt_crouch_shoot_auto",
                _ if ads => "pt_stand_shoot_auto_ads",
                _ => "pt_stand_shoot_auto",
            }
        } else if self.weap_class == weap_class::ROCKETLAUNCHER {
            match () {
                _ if mv => return None,
                _ if crouch => "pt_crouch_shoot_ads",
                _ if prone => "pt_prone_shoot_RPG",
                _ => "pt_stand_shoot_RPG",
            }
        } else if self.anim_type == anim_type::SNIPER {
            match () {
                _ if prone => "pt_prone_shoot_auto",
                _ if mv => return None,
                _ if crouch && ads => "pt_crouch_shoot_ads",
                _ if crouch => "pt_crouch_shoot",
                _ if ads => "pt_rifle_fire_ads",
                _ => "pt_rifle_fire",
            }
        } else if self.anim_type == anim_type::OTHER && prone {
            "pt_prone_shoot_auto"
        } else if self.anim_type == anim_type::OTHER {
            "pt_stand_shoot_shotgun"
        } else {
            match () {
                _ if prone => "pt_prone_shoot_auto",
                _ if mv => return None,
                _ if crouch && ads => "pt_crouch_shoot_ads",
                _ if crouch => "pt_crouch_shoot",
                _ if ads => "pt_stand_shoot_ads",
                _ => "pt_stand_shoot",
            }
        };
        Some((clip, auto.then_some(0.15)))
    }

    /// The grenade throw of `fireweapon`; `hold` is the carried-object (bomb) throw.
    fn throw_clip(&self, hold: bool) -> Option<(&'static str, Option<f32>)> {
        let clip = match (hold, self.stance, self.moving()) {
            (true, StanceInput::Prone, _) => "pt_hold_prone_throw",
            (true, ..) => "pt_hold_throw",
            (_, StanceInput::Prone, _) => "pt_prone_grenade_throw",
            (_, StanceInput::Crouch, false) => "pb_crouch_grenade_throw",
            (_, StanceInput::Crouch, true) => "pt_crouch_grenade_throw",
            (_, StanceInput::Stand, false) => "pb_stand_grenade_throw",
            (_, StanceInput::Stand, true) => "pt_stand_grenade_throw",
        };
        Some((clip, None))
    }

    /// The script's `reload` event.
    fn reload_clip(&self) -> &'static str {
        let (prone, crouch, mv) = (self.prone(), self.crouching(), self.moving());
        if self.laststand {
            "pt_laststand_reload"
        } else if self.weap_class == weap_class::PISTOL && crouch {
            if mv {
                "pt_reload_crouchwalk_pistol"
            } else {
                "pt_reload_crouch_pistol"
            }
        } else if self.weap_class == weap_class::PISTOL && prone {
            "pt_reload_prone_pistol"
        } else if self.weap_class == weap_class::ROCKETLAUNCHER {
            if prone {
                "pt_reload_prone_RPG"
            } else {
                "pt_reload_stand_RPG"
            }
        } else if self.weap_class == weap_class::PISTOL {
            "pt_reload_stand_pistol"
        } else if self.anim_type == anim_type::SMG {
            match () {
                _ if prone => "pt_reload_prone_auto",
                _ if crouch && mv => "pt_reload_crouchwalk",
                _ => "pt_reload_stand_auto_mp40",
            }
        } else if self.anim_type == anim_type::AUTORIFLE {
            match () {
                _ if prone => "pt_reload_prone_auto",
                _ if crouch && mv => "pt_reload_crouchwalk",
                _ if crouch => "pt_reload_crouch_rifle",
                _ => "pt_reload_stand_auto",
            }
        } else if crouch {
            "pt_reload_crouch_rifle"
        } else if prone {
            "pt_reload_prone_auto"
        } else {
            "pt_reload_stand_rifle"
        }
    }

    /// The script's `meleeattack` event; `n` counts swings so the variants take turns (the script picks at random).
    fn melee_clip(&self, n: u32) -> (&'static str, Option<f32>) {
        const CROUCH: [&str; 3] = [
            "pt_melee_crouch_left2left",
            "pt_melee_crouch_left2right",
            "pt_melee_crouch_right2left",
        ];
        const STAND: [(&str, f32); 5] = [
            ("pt_melee_right2right_1", 0.4),
            ("pt_melee_right2right_2", 0.4),
            ("pt_melee_right2left", 0.3),
            ("pt_melee_left2left_1", 0.4),
            ("pt_melee_left2right", 0.3),
        ];
        if self.pistol_or_grenade() && self.anim_type != anim_type::M203 {
            ("pt_melee_prone_pistol", None)
        } else if self.prone() {
            ("pt_melee_prone", None)
        } else if self.crouching() {
            (CROUCH[n as usize % CROUCH.len()], None)
        } else {
            let (c, d) = STAND[n as usize % STAND.len()];
            (c, Some(d))
        }
    }

    /// The script's `dropweapon` event (the weapon pullout pose).
    fn pullout_clip(&self) -> &'static str {
        match self.stance {
            StanceInput::Prone => "pt_prone_pullout_pose",
            StanceInput::Crouch => "pt_crouch_pullout_pose",
            StanceInput::Stand => "pt_stand_pullout_pose",
        }
    }

    /// The script's `flinch_*` entries: by hit direction, for a pistol, a grenade, or any other weapon.
    fn flinch_clip(&self) -> &'static str {
        const RIFLE: [&str; 4] = [
            "pt_flinch_forward",
            "pt_flinch_back",
            "pt_flinch_left",
            "pt_flinch_right",
        ];
        const PISTOL: [&str; 4] = [
            "pt_flinch_pistol_forward",
            "pt_flinch_pistol_back",
            "pt_flinch_pistol_left",
            "pt_flinch_pistol_right",
        ];
        const GRENADE: [&str; 4] = [
            "pt_flinch_grenade_forward",
            "pt_flinch_grenade_back",
            "pt_flinch_grenade_left",
            "pt_flinch_grenade_right",
        ];
        let table = if self.weap_class == weap_class::PISTOL {
            &PISTOL
        } else if self.throws_grenade() {
            &GRENADE
        } else {
            &RIFLE
        };
        table[usize::from(self.flinch_dir & 3)]
    }

    /// A standing, still player inside the flinch window (`PM_Footsteps_NotMoving`).
    fn flinching(&self) -> bool {
        self.recently_hit(FLINCH_MS) && self.stance == StanceInput::Stand && !self.moving()
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
        if let Some(stumble) = self.stumble() {
            return stumble;
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
        Self::with_weapon(content, body, head, None)
    }

    /// [`PlayerAnims::new`] with the world model of a held weapon attached at the body's `tag_weapon_right`; its bones
    /// follow the head's in the rig, so the animation bindings of the body and head are unchanged.
    pub fn with_weapon(
        content: &Content,
        body: &str,
        head: Option<&str>,
        weapon: Option<&str>,
    ) -> Result<Self, String> {
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
        let weapon_part = parts.len();
        if let Some(w) = weapon {
            parts.push(spec(w)?);
        }
        let texts: Vec<Vec<&str>> = parts
            .iter()
            .map(|(_, n)| n.iter().map(|s| &**s).collect())
            .collect();
        let models: Vec<RigModel> = parts
            .iter()
            .zip(&texts)
            .enumerate()
            .map(|(i, ((m, _), t))| RigModel {
                model: m.clone(),
                bone_names: t,
                attach: (weapon.is_some() && i == weapon_part).then_some("tag_weapon_right"),
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
    torso: Option<Torso>,
    /// Swings so far, to take turns among the melee clips.
    swings: u32,
    seen: bool,
    flinching: bool,
}

/// The torso clip playing over the legs.
#[derive(Debug, Clone, Copy)]
struct Torso {
    clip: &'static str,
    seconds: f32,
    /// The script's `duration`: stop this early.
    cap: Option<f32>,
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
        if let Some(t) = &mut self.torso {
            t.seconds += dt;
        }
        self.start_torso(input);
        self.input = *input;
    }

    /// Starts the torso clip the change from the last input to `input` calls for (the script's `EVENTS`): weapon
    /// state edges for reload, melee, offhand throw and pullout, the weapon state or a new fire event for firing,
    /// and the flinch window opening on a still standing player. The first input only seeds the edges.
    fn start_torso(&mut self, input: &PlayerPoseInput) {
        let flinching = input.flinching();
        let was = (
            self.input.weapon_state,
            self.input.event_seq,
            self.flinching,
        );
        let seen = std::mem::replace(&mut self.seen, true);
        self.flinching = flinching;
        if input.dead {
            self.torso = None;
            return;
        }
        if !seen {
            return;
        }
        let state_edge = input.weapon_state != was.0;
        let clip = match input.weapon_state {
            ws::FIRING if state_edge => input.fire_clip(),
            ws::RELOADING | ws::RELOAD_START if state_edge => Some((input.reload_clip(), None)),
            ws::MELEE_INIT if state_edge => {
                self.swings = self.swings.wrapping_add(1);
                Some(input.melee_clip(self.swings))
            }
            ws::OFFHAND_HOLD if state_edge => PlayerPoseInput {
                weap_class: weap_class::GRENADE,
                anim_type: anim_type::NONE,
                ..*input
            }
            .throw_clip(false),
            ws::DROPPING | ws::DROPPING_QUICK if state_edge && !input.mantle => {
                Some((input.pullout_clip(), None))
            }
            _ => None,
        }
        .or_else(|| {
            let fired = input.event_seq != was.1
                && matches!(input.event, ev::FIRE_WEAPON | ev::FIRE_WEAPON_LASTSHOT);
            fired.then(|| input.fire_clip()).flatten()
        })
        .or_else(|| (flinching && !was.2).then(|| (input.flinch_clip(), None)));
        if let Some((clip, cap)) = clip {
            self.torso = Some(Torso {
                clip,
                seconds: 0.0,
                cap,
            });
        }
    }

    /// The torso clip still playing for `anims`' lengths, and its blend weight.
    fn torso_layer<'a>(&self, anims: &'a PlayerAnims) -> Option<AnimLayer<'a>> {
        let t = self.torso?;
        let length = anims.slots.get(t.clip)?.length;
        let end = t.cap.map_or(length, |c| c.min(length));
        if t.seconds >= end {
            return None;
        }
        let weight = (t.seconds / BLEND_SECONDS)
            .min((end - t.seconds) / BLEND_SECONDS)
            .clamp(0.0, 1.0);
        anims.layer(t.clip, t.seconds, weight)
    }

    /// The torso clip playing over `anims`' legs, if any.
    pub fn torso(&self, anims: &PlayerAnims) -> Option<&'static str> {
        self.torso_layer(anims).and(self.torso.map(|t| t.clip))
    }

    /// The animation currently selected.
    pub fn current(&self) -> Option<&'static str> {
        self.current
    }

    /// Poses `anims`' rig for the last input into `out` (entity space; see
    /// [`sim::skel::Pose::to_world`]).
    pub fn pose(&self, anims: &PlayerAnims, out: &mut Pose) {
        self.pose_with(anims, true, out);
    }

    /// The body as the server's hit volumes see it: the legs clip alone, without the torso channel, so locational
    /// damage does not shift with a player's firing or reloading (the torso clips are presentation).
    pub fn hit_pose(&self, anims: &PlayerAnims, out: &mut Pose) {
        self.pose_with(anims, false, out);
    }

    fn pose_with(&self, anims: &PlayerAnims, with_torso: bool, out: &mut Pose) {
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
        let torso = with_torso.then(|| self.torso_layer(anims)).flatten();
        let torso: &[AnimLayer] = torso.as_slice();
        let [a, b] = layers;
        match (a, b) {
            (Some(a), Some(b)) => anims.rig.pose_overlay(&[a, b], torso, &ctl, out),
            (Some(l), None) | (None, Some(l)) => {
                anims
                    .rig
                    .pose_overlay(&[AnimLayer { weight: 1.0, ..l }], torso, &ctl, out)
            }
            (None, None) => anims.rig.pose_overlay(&[], torso, &ctl, out),
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
        self.hit_pose(anims, &mut pose);
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

    fn at(ws: u8) -> PlayerPoseInput {
        PlayerPoseInput {
            weapon_state: ws,
            ..Default::default()
        }
    }

    /// The torso clip a weapon-state edge from rest starts.
    fn torso_after(rest: PlayerPoseInput, ws: u8) -> Option<&'static str> {
        let mut s = PlayerPoseState::default();
        s.update(0.033, &rest);
        s.update(
            0.033,
            &PlayerPoseInput {
                weapon_state: ws,
                ..rest
            },
        );
        s.torso.map(|t| t.clip)
    }

    #[test]
    fn fire_clip_follows_weapon_class_stance_and_motion() {
        let rifle = PlayerPoseInput::default();
        let with =
            |f: &dyn Fn(PlayerPoseInput) -> PlayerPoseInput| f(rifle).fire_clip().map(|c| c.0);
        assert_eq!(with(&|i| i), Some("pt_stand_shoot"));
        assert_eq!(
            with(&|i| PlayerPoseInput { ads: true, ..i }),
            Some("pt_stand_shoot_ads")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                stance: StanceInput::Crouch,
                ..i
            }),
            Some("pt_crouch_shoot")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                stance: StanceInput::Prone,
                ..i
            }),
            Some("pt_prone_shoot_auto")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                speed: 190.0,
                trying_to_move: true,
                ..i
            }),
            None,
            "no firing clip while moving"
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                laststand: true,
                ..i
            }),
            Some("pt_laststand_fire")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                weap_class: weap_class::PISTOL,
                stance: StanceInput::Crouch,
                ads: true,
                ..i
            }),
            Some("pt_crouch_shoot_ads_pistol")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                weap_class: weap_class::SMG,
                ..i
            }),
            Some("pt_stand_shoot_auto")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                weap_class: weap_class::ROCKETLAUNCHER,
                ..i
            }),
            Some("pt_stand_shoot_RPG")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                anim_type: anim_type::SNIPER,
                ads: true,
                ..i
            }),
            Some("pt_rifle_fire_ads")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                weap_class: weap_class::GRENADE,
                ..i
            }),
            Some("pb_stand_grenade_throw")
        );
        assert_eq!(
            with(&|i| PlayerPoseInput {
                weap_class: weap_class::GRENADE,
                stance: StanceInput::Prone,
                ..i
            }),
            Some("pt_prone_grenade_throw")
        );
    }

    #[test]
    fn reload_clip_follows_class_and_stance() {
        let i = PlayerPoseInput::default();
        assert_eq!(i.reload_clip(), "pt_reload_stand_rifle");
        let crouch = PlayerPoseInput {
            stance: StanceInput::Crouch,
            ..i
        };
        assert_eq!(crouch.reload_clip(), "pt_reload_crouch_rifle");
        assert_eq!(
            PlayerPoseInput {
                weap_class: weap_class::PISTOL,
                ..crouch
            }
            .reload_clip(),
            "pt_reload_crouch_pistol"
        );
        assert_eq!(
            PlayerPoseInput {
                anim_type: anim_type::SMG,
                ..i
            }
            .reload_clip(),
            "pt_reload_stand_auto_mp40"
        );
        assert_eq!(
            PlayerPoseInput {
                anim_type: anim_type::AUTORIFLE,
                ..i
            }
            .reload_clip(),
            "pt_reload_stand_auto"
        );
        assert_eq!(
            PlayerPoseInput {
                laststand: true,
                ..i
            }
            .reload_clip(),
            "pt_laststand_reload"
        );
        assert_eq!(
            PlayerPoseInput {
                weap_class: weap_class::ROCKETLAUNCHER,
                stance: StanceInput::Prone,
                ..i
            }
            .reload_clip(),
            "pt_reload_prone_RPG"
        );
    }

    #[test]
    fn weapon_state_edges_start_the_matching_torso_clip() {
        let rest = PlayerPoseInput::default();
        assert_eq!(torso_after(rest, ws::FIRING), Some("pt_stand_shoot"));
        assert_eq!(
            torso_after(rest, ws::RELOADING),
            Some("pt_reload_stand_rifle")
        );
        assert_eq!(
            torso_after(rest, ws::RELOAD_START),
            Some("pt_reload_stand_rifle")
        );
        assert_eq!(
            torso_after(rest, ws::DROPPING),
            Some("pt_stand_pullout_pose")
        );
        assert_eq!(
            torso_after(rest, ws::OFFHAND_HOLD),
            Some("pb_stand_grenade_throw")
        );
        assert!(
            torso_after(rest, ws::MELEE_INIT)
                .unwrap()
                .starts_with("pt_melee_")
        );
        assert_eq!(torso_after(rest, ws::RAISING), None);
        assert_eq!(
            torso_after(
                PlayerPoseInput {
                    stance: StanceInput::Prone,
                    ..rest
                },
                ws::DROPPING_QUICK
            ),
            Some("pt_prone_pullout_pose")
        );
    }

    #[test]
    fn a_player_first_seen_mid_reload_does_not_start_a_clip_and_death_clears_it() {
        let mut s = PlayerPoseState::default();
        s.update(0.033, &at(ws::RELOADING));
        assert!(s.torso.is_none());
        s.update(0.033, &at(ws::READY));
        s.update(0.033, &at(ws::FIRING));
        assert!(s.torso.is_some());
        s.update(
            0.033,
            &PlayerPoseInput {
                dead: true,
                ..at(ws::FIRING)
            },
        );
        assert!(s.torso.is_none());
    }

    #[test]
    fn a_new_fire_event_refires_while_the_state_stays_firing() {
        let mut s = PlayerPoseState::default();
        let firing = at(ws::FIRING);
        s.update(0.033, &at(ws::READY));
        s.update(0.033, &firing);
        s.update(0.2, &firing);
        let t = s.torso.unwrap().seconds;
        s.update(
            0.033,
            &PlayerPoseInput {
                event: ev::FIRE_WEAPON,
                event_seq: 1,
                ..firing
            },
        );
        assert!(s.torso.unwrap().seconds < t, "the clip restarted");
    }

    #[test]
    fn a_still_standing_player_flinches_and_a_moving_one_stumbles() {
        let hit = PlayerPoseInput {
            damage_timer: 400,
            damage_duration: 400,
            flinch_dir: 2,
            ..Default::default()
        };
        let mut s = PlayerPoseState::default();
        s.update(0.033, &PlayerPoseInput::default());
        s.update(0.033, &hit);
        assert_eq!(s.torso.map(|t| t.clip), Some("pt_flinch_left"));
        let pistol = PlayerPoseInput {
            weap_class: weap_class::PISTOL,
            ..hit
        };
        assert_eq!(pistol.flinch_clip(), "pt_flinch_pistol_left");
        // Past the window the legs go back to their normal clip.
        let late = PlayerPoseInput {
            damage_timer: 10,
            damage_duration: 900,
            ..running()
        };
        assert_eq!(late.select(), "pb_combatrun_forward_loop");
        let moving = PlayerPoseInput {
            damage_timer: 300,
            damage_duration: 300,
            ..running()
        };
        assert_eq!(moving.select(), "pb_stumble_forward");
        assert_eq!(
            PlayerPoseInput {
                move_dir: 90.0,
                ..moving
            }
            .select(),
            "pb_stumble_left"
        );
        assert_eq!(
            PlayerPoseInput {
                walking: true,
                ..moving
            }
            .select(),
            "pb_stumble_walk_forward"
        );
        assert_eq!(
            PlayerPoseInput {
                sprinting: true,
                ..moving
            }
            .select(),
            "pb_stumble_forward"
        );
        assert_eq!(
            PlayerPoseInput {
                weap_class: weap_class::PISTOL,
                ..moving
            }
            .select(),
            "pb_stumble_pistol_forward"
        );
    }
}
