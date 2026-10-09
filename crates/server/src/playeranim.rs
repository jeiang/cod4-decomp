// SPDX-License-Identifier: GPL-3.0-only
// Torso event selection and flinch/stumble windows follow KisakCOD (bgame/bg_animation_mp.cpp, bgame/bg_pmove.cpp, bgame/bg_weapons.cpp; GPL-3.0, copyright the KisakCOD contributors and Activision).
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
//! * the movement direction as forward, back, strafe left or strafe right, from the movement direction and
//!   `PMF_BACKWARDS_RUN` the way `PM_SetStrafeCondition` reads the usercmd: strafing only inside `player_strafeAnimCosAngle`
//!   (60 degrees) of the side axis, so back diagonals play the backward clips;
//! * ADS idle poses;
//! * the script's weapon sets, chosen from the weapon's player anim type and class: unarmed (`none`), pistol, rocket
//!   launcher, held grenade, "hold" (carrying the bomb), briefcase (the plant pose when idle, the rifle clips moving)
//!   and everything else as a two-handed rifle;
//! * ladder climbing, the laststand idle, a turret gunner's aim pose (the centred one), and death animations by what the player was
//!   doing, picked at random among the listed clips;
//! * the damage stumble: a player who is moving when hit plays `pb_stumble_*` for the stumble window
//!   (`player_dmgtimer_stumbleTime`), by weapon family and strafe direction.
//!
//! The legs clip plays at the rate `BG_RunLerpFrameRate` gives it, the player's speed over the clip's own root-motion
//! speed, so the feet match the ground; a switch between two moving loops carries the phase over; and every switch
//! blends over what `BG_SetNewAnimation` blends: the clip's `initialLerp`, else 170 ms (still to still), 250 ms (moving
//! to still) or 120 ms (to moving), and at least the 400 ms after a crouch or prone change that is left.
//!
//! Events the script plays over the ground selection are the legs channel: the jump, the landing, the prone to crouch
//! transition and the mantle climb. The clip holds the legs for its `duration` (plus 50 ms) and, after a jump, until
//! the player lands; the server decides it and a snapshot carries it ([`LegsWire`]).
//!
//! Beneath the clips run the controllers: the legs and torso yaw swing toward their goals ([`sim::skel::controllers`]),
//! the legs staying put until the view is `bg_legYawTolerance` away, and a lean tilts and shifts the body.
//!
//! On top of the legs clip runs the torso channel, the script's `torso` entries (`pt_*`): the clip a weapon
//! event selects (fire, reload, melee, grenade throw, weapon pullout, flinch) plays over the bones it names and
//! leaves the legs to the locomotion clip. Events come from what a snapshot carries of a player: the weapon
//! state's edges, the player event ring, and the damage timer. Each event picks its clip the way the script's
//! `EVENTS` block orders its conditions (weapon class and player anim type, stance, moving or not, ADS,
//! last stand).
//!
//! What it drops: turn-in-place animations, the shellshock blend, the `knife_melee` event (the knife swing plays the
//! `meleeattack` clips), the crouch to prone clip (`PM_UpdateStance` never raises its event: only prone to crouch
//! plays) and the jump event of a player who falls off a ledge more than a step (a fall of 0.4 s stands in for the
//! original's trace 64 units down). Melee variants cycle instead of being random so every machine shows the same one.

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
/// Fade of a torso clip in and out, seconds.
pub const BLEND_SECONDS: f32 = 0.1;
/// `stanceTransitionTime`: how long, seconds, after a crouch or prone change a blend lasts at least.
const STANCE_BLEND: f32 = 0.4;
/// Seconds off the ground after which a fall plays the jump clip: about the 64 units the original's ground trace
/// looks down for.
const FALL_SECONDS: f32 = 0.4;
/// `player_strafeAnimCosAngle` default: a player strafes (plays the left or right clip) while the forward part of the
/// move is no more than this share of it, 60 degrees off the forward and back axis.
pub const STRAFE_COS: f32 = 0.5;

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
    pub const GRENADE: i32 = 9;
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
    /// A held grenade (`weaponclass grenade`, not an M203).
    Grenade,
    /// `playerAnimType none`.
    Unarmed,
    Briefcase,
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
    // Walking with it has no strafe clips, only the run and the run back.
    const WALK: Dirs = [
        "pb_hold_run",
        "pb_hold_run_back",
        "pb_hold_run",
        "pb_hold_run",
    ];
    Set {
        idle: ["pb_hold_idle", "pb_crouch_hold_idle", "pb_prone_hold"],
        ads: ["pb_hold_idle", "pb_crouch_hold_idle", "pb_prone_hold"],
        walk: [WALK, CROUCH, PRONE],
        run: [STAND, CROUCH, PRONE],
        sprint: "pb_sprint_hold",
    }
};

/// The bomb carrier's briefcase: the plant pose when still, the rifle clips when moving.
const BRIEFCASE: Set = Set {
    idle: [
        "pb_stand_bombplant",
        "pb_crouch_bombplant",
        "pb_prone_bombplant",
    ],
    ads: [
        "pb_stand_bombplant",
        "pb_crouch_bombplant",
        "pb_prone_bombplant",
    ],
    ..RIFLE
};

const GRENADE: Set = {
    const STAND: Dirs = [
        "pb_combatrun_forward_loop_stickgrenade",
        "pb_combatrun_back_loop_grenade",
        "pb_combatrun_left_loop_grenade",
        "pb_combatrun_right_loop_grenade",
    ];
    const CROUCH_RUN: Dirs = [
        "pb_crouch_run_forward_grenade",
        "pb_crouch_run_back_grenade",
        "pb_crouch_run_left_grenade",
        "pb_crouch_run_right_grenade",
    ];
    const CRAWL: Dirs = [
        "pb_prone_grenade_crawl",
        "pb_prone_grenade_crawl_back",
        "pb_prone_grenade_crawl_left",
        "pb_prone_grenade_crawl_right",
    ];
    Set {
        idle: [
            "pb_stand_grenade_pullpin",
            "pb_crouch_grenade_pullpin",
            "pb_prone_aim_grenade",
        ],
        // Standing ADS comes before the grenade in the script's idle; crouched and prone it comes after.
        ads: [
            "pb_stand_ads",
            "pb_crouch_grenade_pullpin",
            "pb_prone_aim_grenade",
        ],
        // Crouch-walking with it is the pistol's.
        walk: [STAND, PISTOL.walk[1], CRAWL],
        run: [STAND, CROUCH_RUN, CRAWL],
        sprint: "pb_sprint",
    }
};

const UNARMED: Set = {
    const WALK: Dirs = ["pb_stand_shoot_walk_forward_unarmed"; 4];
    const CROUCH_WALK: Dirs = ["pb_crouch_walk_forward_unarmed"; 4];
    Set {
        idle: [
            "pb_stand_alert",
            "pb_crouch_bombplant",
            "pb_prone_bombplant",
        ],
        ads: [
            "pb_stand_alert",
            "pb_crouch_bombplant",
            "pb_prone_bombplant",
        ],
        walk: [WALK, CROUCH_WALK, CRAWL],
        run: [
            [
                "pb_pistol_run_fast",
                "pb_combatrun_back_loop_grenade",
                "pb_pistol_run_fast",
                "pb_pistol_run_fast",
            ],
            [
                "pb_crouch_run_forward_grenade",
                "pb_crouch_run_back_grenade",
                "pb_crouch_run_forward_grenade",
                "pb_crouch_run_forward_grenade",
            ],
            CRAWL,
        ],
        sprint: "pb_sprint",
    }
};

const SETS: [(Family, &Set); 7] = [
    (Family::Rifle, &RIFLE),
    (Family::Pistol, &PISTOL),
    (Family::Rpg, &RPG),
    (Family::Hold, &HOLD),
    (Family::Grenade, &GRENADE),
    (Family::Unarmed, &UNARMED),
    (Family::Briefcase, &BRIEFCASE),
];

/// The aim pose of a player mounted on a turret, by stance: the script's `standSAWgunner_aim`, `crouchSAWgunner_aim`
/// and `proneSAWgunner_aim` blend a grid of poses by the turret's yaw and pitch; the entity state carries neither, so
/// this is the level, centred pose of each.
const MOUNTED: [&str; 3] = [
    "pb_saw_gunner_aim_level_center",
    "pb_saw_gunner_lowwall_aim_level_center",
    "pb_saw_gunner_prone_aim_level_center",
];

const CLIMB_UP: &str = "pb_climbup";
const CLIMB_DOWN: &str = "pb_climbdown";
const LASTSTAND_IDLE: &str = "pb_laststand_idle";
const LASTSTAND_DEATH: &str = "pb_laststand_death";

/// Death animations: what the player was doing when hit. The script picks at random among the clips of its entry.
const DEATH_STAND: [&str; 8] = [
    "pb_stand_death_neckdeath",
    "pb_stand_death_headchest_topple",
    "pb_stand_death_frontspin",
    "pb_stand_death_nervedeath",
    "pb_stand_death_legs",
    "pb_stand_death_lowerback",
    "pb_stand_death_head_collapse",
    "pb_stand_death_neckdeath_thrash",
];
const DEATH_CROUCH: [&str; 5] = [
    "pb_crouch_death_headshot_front",
    "pb_crouch_death_clutchchest",
    "pb_crouch_death_flip",
    "pb_crouch_death_fetal",
    "pb_crouch_death_falltohands",
];
const DEATH_PRONE: &str = "pb_prone_death_quickdeath";
const DEATH_RUN: [&str; 3] = [
    "pb_death_run_forward_crumple",
    "pb_death_run_onfront",
    "pb_death_run_stumble",
];
const DEATH_RUN_BACK: &str = "pb_death_run_back";
const DEATH_RUN_LEFT: &str = "pb_death_run_left";
const DEATH_RUN_RIGHT: &str = "pb_death_run_right";
const DEATH_CROUCH_RUN: [&str; 2] = ["pb_crouchrun_death_drop", "pb_crouchrun_death_crumple"];

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

/// Every legs clip an event can force over the ground selection, in wire order.
const LEGS_CLIPS: &[&str] = &[
    "pb_runjump_takeoff",
    "pb_standjump_takeoff",
    "pb_runjump_land",
    "pb_standjump_land",
    "pb_standjump_land_pistol",
    "pb_prone2crouchrun",
    "pb_prone2crouch",
    "pb_crouch2prone",
    "mp_mantle_up_57",
    "mp_mantle_up_51",
    "mp_mantle_up_45",
    "mp_mantle_up_39",
    "mp_mantle_up_33",
    "mp_mantle_up_27",
    "mp_mantle_up_21",
    "mp_mantle_over_high",
    "mp_mantle_over_mid",
    "player_mantle_over_low",
];

/// Mantle clips the script plays on the legs only, over whatever the upper body is doing.
const LEGS_ONLY: &[&str] = &[
    "mp_mantle_up_27",
    "mp_mantle_up_21",
    "player_mantle_over_low",
];

/// The script's per-clip `blendtime` (`initialLerp`, milliseconds); other clips take the default blend.
const INITIAL_LERP: &[(&str, i32)] = &[
    ("pb_runjump_takeoff", 100),
    ("pb_standjump_takeoff", 100),
    ("pb_runjump_land", 50),
    ("pb_standjump_land", 50),
];

fn initial_lerp(clip: &str) -> i32 {
    INITIAL_LERP
        .iter()
        .find(|(c, _)| *c == clip)
        .map_or(0, |(_, ms)| *ms)
}

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
        DEATH_PRONE,
        DEATH_RUN_BACK,
        DEATH_RUN_LEFT,
        DEATH_RUN_RIGHT,
    ]);
    v.extend(MOUNTED);
    v.extend(LEGS_CLIPS);
    v.extend(DEATH_STAND);
    v.extend(DEATH_CROUCH);
    v.extend(DEATH_RUN);
    v.extend(DEATH_CROUCH_RUN);
    v.extend(STUMBLE_RUN.iter().flatten());
    v.extend(STUMBLE_WALK.iter().flatten());
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
    /// The direction slot of a move: `move_dir` is `movementDir` (degrees, positive to the left of the view, flipped
    /// by 180 when `backward`, `PMF_BACKWARDS_RUN`, is set), the way `PM_SetStrafeCondition` reads the usercmd. The
    /// player strafes when the forward part of the move is at most [`STRAFE_COS`] of it, else plays forward or back.
    pub fn from_move(move_dir: f32, backward: bool) -> Dir {
        let (sin, cos) = sim::pm::math::sincos_deg(move_dir + if backward { 180.0 } else { 0.0 });
        if cos.abs() <= STRAFE_COS + 1e-4 {
            if sin >= 0.0 { Dir::Left } else { Dir::Right }
        } else if cos > 0.0 {
            Dir::Forward
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
    /// `PMF_BACKWARDS_RUN`: the move is backward, `move_dir` measured from the back.
    pub backward: bool,
    /// `ps.leanf`, -1 (left) to 1 (right).
    pub lean: f32,
    pub ladder: bool,
    /// Vertical speed, to pick climb up or down on a ladder.
    pub vertical_speed: f32,
    pub mantle: bool,
    /// The mantle in progress: `ps.mantle_state`'s transition index, whether it climbs over the ledge, and its timer
    /// in milliseconds.
    pub mantle_trans: i32,
    pub mantle_over: bool,
    pub mantle_timer: i32,
    /// Off the ground (`groundEntityNum` is none) and not on a ladder.
    pub airborne: bool,
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
    pub events: [u8; 4],
    pub event_seq: u8,
    /// `ps.weapon`: a different weapon ends a torso clip.
    pub weapon: u16,
    /// A remote player's torso channel as the server decided it; `None` where this side decides it from the state.
    pub torso_wire: Option<TorsoWire>,
    /// The same for the legs channel.
    pub legs_wire: Option<LegsWire>,
    /// Picks among a death's clips; both sides derive it from what a snapshot carries.
    pub seed: u32,
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
            backward: ps.pm_flags & pmf::BACKWARDS_RUN != 0,
            lean: ps.leanf,
            ladder: ps.pm_flags & pmf::LADDER != 0,
            vertical_speed: ps.velocity[2],
            mantle: ps.pm_flags & pmf::MANTLE != 0,
            mantle_trans: ps.mantle_state.trans_index,
            mantle_over: ps.mantle_state.flags & 1 != 0,
            mantle_timer: ps.mantle_state.timer,
            airborne: ps.ground_entity_num == sim::cm::ENTITYNUM_NONE
                && ps.pm_flags & pmf::LADDER == 0,
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
            events: ps.events,
            weapon: ps.weapon as u16,
            torso_wire: None,
            legs_wire: None,
            seed: u32::from(ps.event_sequence) << 16 | (ps.damage_duration as u32 & 0xffff),
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
        let dir = Dir::from_move(self.move_dir, self.backward).slot();
        let pg = usize::from(self.pistol_or_grenade());
        Some(match self.motion() {
            Motion::Idle => return None,
            Motion::Sprint => STUMBLE_SPRINT,
            Motion::Walk if self.stance == StanceInput::Stand => STUMBLE_WALK[pg][dir],
            // Crouched walking and running are the script's `stumble_crouch_*` move types, whose entries
            // play the standing `pb_stumble_*` (rifle) and `pb_stumble_pistol_*` (pistol or grenade) clips.
            Motion::Walk | Motion::Run => {
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
        if self.anim_type == anim_type::NONE {
            Family::Unarmed
        } else if self.anim_type == anim_type::HOLD {
            Family::Hold
        } else if self.anim_type == anim_type::BRIEFCASE {
            Family::Briefcase
        } else if self.weap_class == weap_class::ROCKETLAUNCHER
            || self.anim_type == anim_type::ROCKETLAUNCHER
        {
            Family::Rpg
        } else if self.weap_class == weap_class::PISTOL || self.anim_type == anim_type::PISTOL {
            Family::Pistol
        } else if self.throws_grenade() {
            Family::Grenade
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

    /// The direction slot of the legs.
    fn dir(&self) -> Dir {
        Dir::from_move(self.move_dir, self.backward)
    }

    /// The `pb_*` animation this input plays.
    pub fn select(&self) -> &'static str {
        if self.dead {
            return self.death();
        }
        if self.turret {
            return MOUNTED[self.stance.index()];
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
        let dir = self.dir().slot();
        match self.motion() {
            _ if self.mantle => s.idle[st],
            Motion::Idle if self.ads => s.ads[st],
            Motion::Idle => s.idle[st],
            Motion::Walk => s.walk[st][dir],
            Motion::Run => s.run[st][dir],
            Motion::Sprint => s.sprint,
        }
    }

    /// `animSpeedScale` (`BG_RunLerpFrameRate`): the player's speed over the clip's root-motion speed, clamped. A clip
    /// that does not move the body plays at its own rate.
    fn rate_for(&self, clip: &str, info: Option<ClipInfo>) -> f32 {
        let Some(i) = info.filter(|i| i.move_speed != 0.0) else {
            return 1.0;
        };
        let ladder = clip == CLIMB_UP || clip == CLIMB_DOWN;
        let speed = if ladder {
            self.vertical_speed.abs()
        } else {
            self.speed.hypot(self.vertical_speed)
        };
        let scale = speed / i.move_speed;
        if scale >= 0.1 {
            if scale <= 2.0 {
                scale
            } else if ladder {
                scale.min(4.0)
            } else if i.move_speed > 150.0 {
                2.0
            } else if i.move_speed >= 20.0 {
                scale.min(3.0 - (i.move_speed - 20.0) / 130.0)
            } else {
                scale.min(3.0)
            }
        } else if scale < 0.01 && ladder {
            0.0
        } else {
            0.1
        }
    }

    /// The mantle clip for the climb in progress: the "up" clip of its transition, then the "over" clip once the up
    /// clip's length has passed, if the mantle goes over the ledge.
    fn mantle_clip(&self, clips: &dyn Clips) -> Option<&'static str> {
        let t = sim::pm::TRANSITIONS.get(usize::try_from(self.mantle_trans).ok()?)?;
        let up = sim::pm::MANTLE_ANIM_NAMES[t.up_anim];
        let up_ms = clips.clip(up).map_or(0, |c| (c.length * 1000.0) as i32);
        Some(if self.mantle_over && self.mantle_timer >= up_ms {
            sim::pm::MANTLE_ANIM_NAMES[t.over_anim]
        } else {
            up
        })
    }

    /// What `BG_PlayerAngles` and `BG_Player_DoControllersInternal` read.
    fn controller_inputs(&self) -> (controllers::SwingInput, ControllerInput) {
        let motion = self.motion();
        let prone = self.stance == StanceInput::Prone;
        let move_dir = if motion == Motion::Idle {
            0.0
        } else {
            self.move_dir
        };
        let swing = controllers::SwingInput {
            view_pitch: self.view_pitch,
            view_yaw: self.yaw,
            move_dir,
            prone,
            idle: motion == Motion::Idle && !prone,
            mounted: self.turret,
            mantle: self.mantle,
            ladder: self.ladder,
            firing: self.weapon_state == ws::FIRING,
            strafing: motion != Motion::Idle && matches!(self.dir(), Dir::Left | Dir::Right),
        };
        let ctl = ControllerInput {
            view_pitch: self.view_pitch,
            view_yaw: self.yaw,
            move_dir,
            prone,
            crouch: self.stance == StanceInput::Crouch,
            torso_pitch: self.torso_pitch,
            waist_pitch: self.waist_pitch,
            lean: self.lean,
            no_aim: self.turret || self.mantle || self.ladder || self.dead,
        };
        (swing, ctl)
    }

    /// The script's `DEATH` block: by stance and move type, at random among the clips an entry lists.
    fn death(&self) -> &'static str {
        fn pick(clips: &[&'static str], seed: u32) -> &'static str {
            clips[seed as usize % clips.len()]
        }
        if self.laststand {
            return LASTSTAND_DEATH;
        }
        let (crouch, dir) = (self.stance == StanceInput::Crouch, self.dir());
        match (self.stance, self.motion()) {
            (StanceInput::Prone, _) => DEATH_PRONE,
            (StanceInput::Crouch, Motion::Idle) => pick(&DEATH_CROUCH, self.seed),
            (StanceInput::Stand, Motion::Run) if dir == Dir::Back => DEATH_RUN_BACK,
            (_, Motion::Run) if dir == Dir::Left => DEATH_RUN_LEFT,
            (_, Motion::Run) if dir == Dir::Right => DEATH_RUN_RIGHT,
            (_, Motion::Run) if crouch && dir == Dir::Forward => pick(&DEATH_CROUCH_RUN, self.seed),
            (StanceInput::Stand, Motion::Run) => pick(&DEATH_RUN, self.seed),
            // Walking, sprinting and running crouched backward fall to the script's default.
            _ => pick(&DEATH_STAND, self.seed),
        }
    }
}

struct Slot {
    anim: Arc<PlayerAnim>,
    bind: AnimBinding,
    info: ClipInfo,
}

/// What the playback rate and the blends need to know of a clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipInfo {
    /// Seconds per loop or run.
    pub length: f32,
    /// Root-motion speed, units per second (`animation_s.moveSpeed`); 0 for a clip that does not move the body.
    pub move_speed: f32,
    pub looped: bool,
}

impl ClipInfo {
    fn moves(info: Option<ClipInfo>) -> bool {
        info.is_some_and(|i| i.move_speed != 0.0)
    }
}

/// The clips a [`PlayerPoseState`] plays: their lengths and speeds.
pub trait Clips {
    fn clip(&self, name: &str) -> Option<ClipInfo>;
}

/// No clip data: every clip is still, one second long and unlooped for the purpose of rates and blends.
pub struct NoClips;

impl Clips for NoClips {
    fn clip(&self, _: &str) -> Option<ClipInfo> {
        None
    }
}

impl Clips for PlayerAnims {
    fn clip(&self, name: &str) -> Option<ClipInfo> {
        self.slots.get(name).map(|s| s.info)
    }
}

/// Clips whose script entry zeroes `moveSpeed`: the death events and the mantle climbs.
fn still_by_script(name: &str) -> bool {
    DEATH_STAND.contains(&name)
        || DEATH_CROUCH.contains(&name)
        || DEATH_RUN.contains(&name)
        || DEATH_CROUCH_RUN.contains(&name)
        || [
            DEATH_PRONE,
            DEATH_RUN_BACK,
            DEATH_RUN_LEFT,
            DEATH_RUN_RIGHT,
            LASTSTAND_DEATH,
        ]
        .contains(&name)
        || name.contains("mantle_")
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
        let attach: Vec<(&str, &str)> = head.into_iter().map(|h| (h, "")).collect();
        Self::with_attachments(content, body, &attach)
    }

    /// [`PlayerAnims::new`] with the models the scripts attached to the body, and the weapon in the hand, as
    /// `(model, tag)` in order: a model with no tag melds onto the body by bone name (a head), one with a tag hangs
    /// from that bone of the models before it. Their bones follow the body's in the rig, so the animation bindings of
    /// the body are unchanged.
    pub fn with_attachments(
        content: &Content,
        body: &str,
        attachments: &[(&str, &str)],
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
        for (model, _) in attachments {
            parts.push(spec(model)?);
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
                attach: i
                    .checked_sub(1)
                    .map(|a| attachments[a].1)
                    .filter(|tag| !tag.is_empty()),
            })
            .collect();
        let rig = Rig::new(&models)?;
        let mut slots = HashMap::new();
        let mut missing = Vec::new();
        // Upper-body clips are bound where the content has them (a client's); the server does without.
        let torso = TORSO_BODY_CLIPS.iter().chain(TORSO_CLIPS.iter()).copied();
        for name in all_names().into_iter().chain(torso.clone()) {
            let optional = torso.clone().any(|t| t == name);
            match content.player_anim(name) {
                Some(a) => {
                    let p = &a.parts;
                    let length = f32::from(p.num_frames) / p.frame_rate;
                    // `BG_FinalizePlayerAnims`: the root-motion distance over the clip's length.
                    let move_speed = match crate::delta::RootMotion::new(p) {
                        Some(m) if length > 0.0 && !still_by_script(name) => {
                            let (_, v) = crate::delta::rel_delta(&m, 0.0, 1.0);
                            (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() / length
                        }
                        _ => 0.0,
                    };
                    slots.insert(
                        name,
                        Slot {
                            bind: rig.bind(&a.part_names),
                            anim: a.clone(),
                            info: ClipInfo {
                                length,
                                move_speed,
                                looped: p.looping,
                            },
                        },
                    );
                }
                None if optional => {}
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
        let time = if s.info.length > 0.0 {
            seconds / s.info.length
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
    /// Playback rate of the current clip (`animSpeedScale`).
    rate: f32,
    /// The clip being faded out.
    previous: Option<Fade>,
    /// Seconds of cross-fade left, and the length the fade started with.
    blend: f32,
    blend_total: f32,
    /// Seconds since the first update, to seed the phase of a loop.
    clock: f32,
    /// Seconds left of the minimum blend after a crouch or prone change (`stanceTransitionTime`).
    stance_left: f32,
    /// The stance the current clip was chosen in.
    clip_stance: StanceInput,
    /// The legs clip an event forces over the ground selection, with the legs counter a snapshot carries.
    legs: Option<Legs>,
    legs_seq: u8,
    legs_wire_seen: Option<u8>,
    /// Seconds off the ground.
    air: f32,
    /// The legs and torso yaw and the controllers easing toward them.
    swing: controllers::Swing,
    ctl: sim::skel::Controllers,
    ctl_ready: bool,
    torso: Option<Torso>,
    /// Counts every start and end of a torso clip: what a snapshot carries so a client plays the server's choice.
    torso_seq: u8,
    /// The last wire sequence a client acted on.
    wire_seen: Option<u8>,
    /// Swings so far, to take turns among the melee clips.
    swings: u32,
    seen: bool,
    flinching: bool,
}

/// A clip fading out, with where it was.
#[derive(Debug, Clone, Copy)]
struct Fade {
    clip: &'static str,
    seconds: f32,
    rate: f32,
}

/// A legs clip an event forces: the script's `legsTimer`, seconds left, and whether a mantle holds it.
#[derive(Debug, Clone, Copy)]
struct Legs {
    clip: &'static str,
    timer: f32,
    mantle: bool,
}

/// The legs channel as a snapshot carries it: the clip (`0` none, else 1 + its place in the legs clip list) and a
/// counter that changes whenever one starts or ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LegsWire {
    pub clip: u8,
    pub seq: u8,
}

/// Every clip the legs channel can carry: the events', then the death animations, which keep a corpse in the pose
/// the server chose for every viewer.
fn legs_wire_clips() -> impl Iterator<Item = &'static str> {
    LEGS_CLIPS
        .iter()
        .chain(&DEATH_STAND)
        .chain(&DEATH_CROUCH)
        .chain(&DEATH_RUN)
        .chain(&DEATH_CROUCH_RUN)
        .chain(&[
            DEATH_PRONE,
            DEATH_RUN_BACK,
            DEATH_RUN_LEFT,
            DEATH_RUN_RIGHT,
            LASTSTAND_DEATH,
        ])
        .copied()
}

fn legs_name(clip: u8) -> Option<&'static str> {
    legs_wire_clips().nth(usize::from(clip).checked_sub(1)?)
}

fn legs_index(name: &str) -> u8 {
    legs_wire_clips()
        .position(|c| c == name)
        .map_or(0, |i| i as u8 + 1)
}

/// Seconds a legs event holds the legs for, after the clip starts (`legsTimer`: the script's `duration` or the clip's
/// own, 500 ms at least, plus 50 ms).
fn legs_hold(clip: &str, clips: &dyn Clips) -> f32 {
    match clip {
        "pb_runjump_takeoff" | "pb_standjump_takeoff" | "pb_standjump_land_pistol" => 0.055,
        "pb_runjump_land" | "pb_standjump_land" => 0.15,
        _ => clips.clip(clip).map_or(0.5, |c| c.length.max(0.5)) + 0.05,
    }
}

/// `ev::LANDING_*` events: the landings the script's `land` event answers (a fall of at least 12 units).
fn is_landing(event: u8) -> bool {
    (ev::LANDING_FIRST..ev::LANDING_PAIN_FIRST + 29).contains(&event)
}

/// The torso clip playing over the legs.
#[derive(Debug, Clone, Copy)]
struct Torso {
    clip: &'static str,
    seconds: f32,
    /// The script's `duration`: stop this early.
    cap: Option<f32>,
    /// What the clip was chosen for: it ends when the stance or the weapon changes, or (a reload) when the weapon
    /// leaves the reload states. A pullout belongs to no weapon.
    stance: StanceInput,
    weapon: Option<u16>,
    reload: bool,
}

/// The torso channel as a snapshot carries it: the clip (`0` none, else 1 + its place in the torso clip list), the
/// script's `duration` cap in 10 ms (`0` none), and a counter that changes whenever a clip starts or ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TorsoWire {
    pub clip: u8,
    pub cap: u8,
    pub seq: u8,
}

fn torso_name(clip: u8) -> Option<&'static str> {
    let i = usize::from(clip).checked_sub(1)?;
    TORSO_BODY_CLIPS
        .iter()
        .chain(TORSO_CLIPS.iter())
        .nth(i)
        .copied()
}

fn torso_index(name: &str) -> u8 {
    TORSO_BODY_CLIPS
        .iter()
        .chain(TORSO_CLIPS.iter())
        .position(|c| *c == name)
        .map_or(0, |i| i as u8 + 1)
}

/// `weapon_state` values of a reload in progress.
fn in_reload(state: u8) -> bool {
    matches!(
        state,
        ws::RELOADING
            | ws::RELOADING_INTERUPT
            | ws::RELOAD_START
            | ws::RELOAD_START_INTERUPT
            | ws::RELOAD_END
    )
}

impl PlayerPoseState {
    /// Advances the clocks by `dt` seconds and switches animation when `input` selects another. `clips` supplies the
    /// clip lengths and speeds the rate and the blends depend on.
    pub fn update(&mut self, clips: &dyn Clips, dt: f32, input: &PlayerPoseInput) {
        self.clock += dt;
        self.stance_left = (self.stance_left - dt).max(0.0);
        self.start_legs(clips, dt, input);
        let want = if !input.dead {
            self.legs.map_or_else(|| input.select(), |l| l.clip)
        } else if let Some(l) = self.legs {
            l.clip
        } else if self.input.dead {
            // A corpse keeps the animation it died in even as its speed decays.
            self.current.unwrap_or_else(|| input.select())
        } else {
            // A death is chosen from what the player was doing the frame before.
            self.input.dead_selection(input)
        };
        match self.current {
            Some(c) if c == want => {}
            Some(c) => self.switch(clips, c, want, input),
            None => {
                self.current = Some(want);
                self.clip_stance = input.stance;
                self.seconds = 0.0;
            }
        }
        self.rate = input.rate_for(want, clips.clip(want));
        self.seconds += dt * self.rate;
        if let Some(f) = &mut self.previous {
            f.seconds += dt * f.rate;
        }
        self.blend = (self.blend - dt).max(0.0);
        if self.blend == 0.0 {
            self.previous = None;
        }
        if let Some(t) = &mut self.torso {
            t.seconds += dt;
        }
        self.start_torso(input);
        self.step_controllers(dt, input);
        self.input = *input;
    }

    /// `BG_SetNewAnimation` for the legs: how long to blend, where in the new clip to start.
    fn switch(
        &mut self,
        clips: &dyn Clips,
        old: &'static str,
        new: &'static str,
        input: &PlayerPoseInput,
    ) {
        let (old_info, new_info) = (clips.clip(old), clips.clip(new));
        if self.clip_stance != input.stance {
            self.stance_left = STANCE_BLEND;
        }
        self.clip_stance = input.stance;
        let lerp = initial_lerp(new);
        let mut min = if lerp > 0 {
            -1
        } else if !ClipInfo::moves(new_info) {
            if ClipInfo::moves(old_info) { 250 } else { 170 }
        } else {
            120
        };
        min = min.max((self.stance_left * 1000.0) as i32);
        self.blend_total = lerp.max(min) as f32 * 0.001;
        self.blend = self.blend_total;
        let start = match (self.previous, new_info) {
            // Back to a clip still fading out: carry on from where it is.
            (Some(f), _) if f.clip == new => f.seconds,
            (_, Some(n)) if n.move_speed != 0.0 && n.looped => match old_info {
                // Between two moving loops the cycle carries over.
                Some(o) if o.move_speed != 0.0 && o.looped && o.length > 0.0 => {
                    (self.seconds / o.length).fract() * n.length
                }
                _ => {
                    let cycle = n.length * 1000.0 + 200.0;
                    let t = (self.clock * 1000.0) % cycle;
                    (t / cycle).fract() * n.length
                }
            },
            _ => 0.0,
        };
        self.previous = Some(Fade {
            clip: old,
            seconds: self.seconds,
            rate: self.rate,
        });
        self.current = Some(new);
        self.seconds = start;
    }

    /// Decides the legs channel. A remote player follows the server's wire values. Otherwise the events the script
    /// answers play their clip over the ground selection: a jump, a landing, a prone to crouch change and a mantle.
    /// The clip holds the legs while the script's timer runs and, after a jump, until the player is on the ground.
    fn start_legs(&mut self, clips: &dyn Clips, dt: f32, input: &PlayerPoseInput) {
        if let Some(w) = input.legs_wire {
            let before = self.legs_wire_seen.replace(w.seq);
            if before != Some(w.seq) {
                self.legs = legs_name(w.clip).map(|clip| Legs {
                    clip,
                    timer: 0.0,
                    mantle: false,
                });
            }
            return;
        }
        if input.dead {
            // The death animation is the server's choice, held for every viewer from the frame of the death.
            if !self.input.dead || (self.legs.is_none() && !self.seen) {
                let clip = self.input.dead_selection(input);
                self.set_legs(Some(Legs {
                    clip,
                    timer: 0.0,
                    mantle: false,
                }));
            }
            return;
        }
        if self.input.dead {
            // Respawned: the death clip is over.
            self.set_legs(None);
        }
        if let Some(l) = &mut self.legs {
            l.timer -= dt;
        }
        self.air = if input.airborne { self.air + dt } else { 0.0 };
        if !self.seen {
            return;
        }
        let was = self.input;
        let run = input.motion() == Motion::Run && input.stance == StanceInput::Stand;
        let mut start: Option<&'static str> = None;
        // Every event raised since the last input, oldest first.
        let new = usize::from(input.event_seq.wrapping_sub(was.event_seq)).min(4);
        for k in (0..new).rev() {
            let slot = usize::from(input.event_seq.wrapping_sub(k as u8 + 1) & 3);
            let event = input.events[slot];
            if event == ev::JUMP {
                start = Some(if run && !input.backward {
                    "pb_runjump_takeoff"
                } else {
                    "pb_standjump_takeoff"
                });
            } else if is_landing(event) {
                start = Some(if run {
                    "pb_runjump_land"
                } else if input.pistol_or_grenade() && input.anim_type != anim_type::M203 {
                    "pb_standjump_land_pistol"
                } else {
                    "pb_standjump_land"
                });
            }
        }
        if was.stance == StanceInput::Prone && input.stance == StanceInput::Crouch {
            start = Some(if was.moving() {
                "pb_prone2crouchrun"
            } else {
                "pb_prone2crouch"
            });
        }
        // Falling a long way without a jump plays the jump too (the original traces 64 units down).
        if start.is_none() && self.legs.is_none() && self.air >= FALL_SECONDS && !input.mantle {
            start = Some("pb_standjump_takeoff");
        }
        if let Some(clip) = start {
            self.set_legs(Some(Legs {
                clip,
                timer: legs_hold(clip, clips),
                mantle: false,
            }));
        }
        if input.mantle {
            let clip = input.mantle_clip(clips);
            if clip.is_some() && self.legs.map(|l| l.clip) != clip {
                self.set_legs(clip.map(|clip| Legs {
                    clip,
                    timer: 0.0,
                    mantle: true,
                }));
            }
        } else if self.legs.is_some_and(|l| l.mantle) {
            self.set_legs(None);
        }
        if self
            .legs
            .is_some_and(|l| !l.mantle && l.timer < 0.05 && !input.airborne)
        {
            self.set_legs(None);
        }
    }

    fn set_legs(&mut self, l: Option<Legs>) {
        self.legs = l;
        self.legs_seq = self.legs_seq.wrapping_add(1);
    }

    /// The legs channel for a snapshot.
    pub fn legs_wire(&self) -> LegsWire {
        LegsWire {
            clip: self.legs.map_or(0, |l| legs_index(l.clip)),
            seq: self.legs_seq,
        }
    }

    /// `BG_PlayerAngles` and `BG_Player_DoControllersSetup`: swings the legs and torso toward the view and eases the
    /// controllers toward their goals. The first frame of a player starts at the goals.
    fn step_controllers(&mut self, dt: f32, input: &PlayerPoseInput) {
        let (swing_in, ctl_in) = input.controller_inputs();
        let ms = dt * 1000.0;
        if self.ctl_ready {
            self.swing.step(ms, &swing_in);
        } else {
            self.swing.settle(&swing_in);
        }
        let goal = controllers::goal(&ctl_in, &self.swing);
        if self.ctl_ready && !input.dead {
            controllers::ease(&mut self.ctl, &goal, ms);
        } else {
            self.ctl = goal;
        }
        self.ctl_ready = true;
    }

    /// Decides the torso channel. A player this side simulates (`torso_wire` `None`) starts the clip the change
    /// from the last input to `input` calls for (the script's `EVENTS`): weapon state edges for reload, melee,
    /// offhand throw and pullout, the weapon state or a fire event in the player event ring for firing, and the
    /// flinch window opening on a still standing player; a clip ends on death, a stance or weapon change, and a
    /// reload ends when the weapon leaves the reload states. A remote player follows the server's wire values.
    /// The first input only seeds the edges.
    fn start_torso(&mut self, input: &PlayerPoseInput) {
        let seen = std::mem::replace(&mut self.seen, true);
        if let Some(w) = input.torso_wire {
            let before = self.wire_seen.replace(w.seq);
            if input.dead {
                self.torso = None;
            } else if before.is_some_and(|b| b != w.seq) {
                self.torso = torso_name(w.clip).map(|clip| Torso {
                    clip,
                    seconds: 0.0,
                    cap: (w.cap != 0).then(|| f32::from(w.cap) * 0.01),
                    stance: input.stance,
                    weapon: None,
                    reload: false,
                });
            }
            return;
        }
        let flinching = input.flinching();
        let was = (
            self.input.weapon_state,
            self.input.event_seq,
            self.flinching,
        );
        self.flinching = flinching;
        if let Some(t) = self.torso
            && (input.dead
                || input.stance != t.stance
                || t.weapon.is_some_and(|w| w != input.weapon)
                || (t.reload && !in_reload(input.weapon_state)))
        {
            self.set_torso(None);
        }
        if input.dead || !seen {
            return;
        }
        let state_edge = input.weapon_state != was.0;
        let mut reload = false;
        let mut pullout = false;
        let clip = match input.weapon_state {
            ws::FIRING if state_edge => input.fire_clip(),
            ws::RELOADING | ws::RELOAD_START if state_edge && !in_reload(was.0) => {
                reload = true;
                Some((input.reload_clip(), None))
            }
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
                pullout = true;
                Some((input.pullout_clip(), None))
            }
            _ => None,
        }
        .or_else(|| {
            // Every event raised since the last input, not just the newest.
            let new = usize::from(input.event_seq.wrapping_sub(was.1)).min(4);
            let fired = (0..new).any(|k| {
                let slot = usize::from(input.event_seq.wrapping_sub(k as u8 + 1) & 3);
                matches!(
                    input.events[slot],
                    ev::FIRE_WEAPON | ev::FIRE_WEAPON_LASTSHOT
                )
            });
            fired.then(|| input.fire_clip()).flatten()
        })
        .or_else(|| (flinching && !was.2).then(|| (input.flinch_clip(), None)));
        if let Some((clip, cap)) = clip {
            self.set_torso(Some(Torso {
                clip,
                seconds: 0.0,
                cap,
                stance: input.stance,
                weapon: (!pullout).then_some(input.weapon),
                reload,
            }));
        }
    }

    fn set_torso(&mut self, t: Option<Torso>) {
        self.torso = t;
        self.torso_seq = self.torso_seq.wrapping_add(1);
    }

    /// The torso channel for a snapshot.
    pub fn torso_wire(&self) -> TorsoWire {
        TorsoWire {
            clip: self.torso.map_or(0, |t| torso_index(t.clip)),
            cap: self
                .torso
                .and_then(|t| t.cap)
                .map_or(0, |c| (c * 100.0).round() as u8),
            seq: self.torso_seq,
        }
    }

    /// The torso clip still playing for `anims`' lengths, and its blend weight.
    fn torso_layer<'a>(&self, anims: &'a PlayerAnims) -> Option<AnimLayer<'a>> {
        let t = self.torso?;
        let length = anims.slots.get(t.clip)?.info.length;
        let end = t.cap.map_or(length, |c| c.min(length));
        if t.seconds >= end {
            return None;
        }
        let weight = (t.seconds / BLEND_SECONDS)
            .min((end - t.seconds) / BLEND_SECONDS)
            .clamp(0.0, 1.0);
        anims.layer(t.clip, t.seconds, weight)
    }

    /// The torso clip playing over `anims`' legs, if any (a client; the server keeps no torso clips).
    pub fn torso(&self, anims: &PlayerAnims) -> Option<&'static str> {
        self.torso_layer(anims).and(self.torso.map(|t| t.clip))
    }

    /// The torso clip playing and how many seconds into it, if one is.
    pub fn torso_seconds(&self) -> Option<(&'static str, f32)> {
        self.torso.map(|t| (t.clip, t.seconds))
    }

    /// The torso clip chosen, whether or not it has clips to play.
    pub fn torso_choice(&self) -> Option<&'static str> {
        self.torso.map(|t| t.clip)
    }

    /// How long, in milliseconds, the animation the player would die in plays; `None` when the zones lack it.
    pub fn death_duration_ms(&self, anims: &PlayerAnims) -> Option<i32> {
        let slot = anims.slots.get(self.input.dead_selection(&self.input))?;
        Some((slot.info.length * 1000.0) as i32)
    }

    /// The animation currently selected.
    pub fn current(&self) -> Option<&'static str> {
        self.current
    }

    /// How far into the current animation the legs are, seconds, and the rate it plays at (`animSpeedScale`).
    pub fn clip_seconds(&self) -> f32 {
        self.seconds
    }

    pub fn rate(&self) -> f32 {
        self.rate
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
        let ctl = self.ctl;
        let w_prev = if self.previous.is_some() && self.blend_total > 0.0 {
            self.blend / self.blend_total
        } else {
            0.0
        };
        // A legs-only clip (the low mantles) rides over the ground selection, which keeps the upper body.
        let legs_only = self.current.filter(|c| LEGS_ONLY.contains(c));
        let mut layers: [Option<AnimLayer>; 2] = [None, None];
        let mut overlay: [Option<AnimLayer>; 2] = [None, None];
        if let Some(c) = legs_only {
            let base = self.input.select();
            layers[0] = anims.layer(base, self.seconds, 1.0);
            overlay[0] = anims.layer(c, self.seconds, 1.0 - w_prev);
        } else {
            if let Some(c) = self.current {
                layers[0] = anims.layer(c, self.seconds, 1.0 - w_prev);
            }
            if let Some(f) = self.previous {
                layers[1] = anims.layer(f.clip, f.seconds, w_prev);
            }
        }
        overlay[1] = with_torso.then(|| self.torso_layer(anims)).flatten();
        let [a, b] = layers;
        let over: [AnimLayer; 2];
        let over: &[AnimLayer] = match overlay {
            [Some(x), Some(y)] => {
                over = [x, y];
                &over
            }
            [Some(x), None] | [None, Some(x)] => {
                over = [x, x];
                &over[..1]
            }
            [None, None] => &[],
        };
        match (a, b) {
            (Some(a), Some(b)) => anims.rig.pose_overlay(&[a, b], over, &ctl, out),
            (Some(l), None) | (None, Some(l)) => {
                anims
                    .rig
                    .pose_overlay(&[AnimLayer { weight: 1.0, ..l }], over, &ctl, out)
            }
            (None, None) => anims.rig.pose_overlay(&[], over, &ctl, out),
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
            seed: now.seed,
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
            anim_type: anim_type::AUTORIFLE,
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
    fn direction_buckets_follow_the_usercmd_axis_ratio() {
        let at = |d: f32, backward: bool| {
            PlayerPoseInput {
                move_dir: d,
                backward,
                ..running()
            }
            .select()
        };
        assert_eq!(at(0.0, false), "pb_combatrun_forward_loop");
        assert_eq!(at(45.0, false), "pb_combatrun_forward_loop");
        assert_eq!(at(-45.0, false), "pb_combatrun_forward_loop");
        assert_eq!(at(90.0, false), "pb_combatrun_left_loop");
        assert_eq!(at(-90.0, false), "pb_combatrun_right_loop");
        // The pm clamps the movement direction to +-90 and sets the backwards flag: back and to the side is back.
        assert_eq!(at(0.0, true), "pb_combatrun_back_loop");
        assert_eq!(at(45.0, true), "pb_combatrun_back_loop");
        assert_eq!(at(-45.0, true), "pb_combatrun_back_loop");
        // Strafing starts 60 degrees off the forward axis, where the forward part of the move is half of it.
        assert_eq!(at(59.0, false), "pb_combatrun_forward_loop");
        assert_eq!(at(61.0, false), "pb_combatrun_left_loop");
        assert_eq!(at(-61.0, false), "pb_combatrun_right_loop");
        assert_eq!(at(180.0, false), "pb_combatrun_back_loop");
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
        assert_eq!(with(anim_type::HOLD, 0), "pb_hold_idle");
        assert_eq!(with(anim_type::BRIEFCASE, 0), "pb_stand_bombplant");
        assert_eq!(
            with(anim_type::GRENADE, weap_class::GRENADE),
            "pb_stand_grenade_pullpin"
        );
        assert_eq!(
            with(anim_type::M203, weap_class::GRENADE),
            "pb_stand_alert",
            "an M203 is a rifle"
        );
        assert_eq!(with(anim_type::NONE, 0), "pb_stand_alert");
        assert_eq!(with(4, 0), "pb_stand_alert", "autorifles use the rifle set");
        let ads = PlayerPoseInput {
            ads: true,
            anim_type: anim_type::AUTORIFLE,
            ..Default::default()
        };
        assert_eq!(ads.select(), "pb_stand_ads");
        assert_eq!(
            PlayerPoseInput {
                ads: true,
                stance: StanceInput::Prone,
                anim_type: anim_type::AUTORIFLE,
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
        s.update(&NoClips, 0.033, &run);
        assert_eq!(s.current(), Some("pb_combatrun_forward_loop"));
        // Killed while running forward: the run death, and it stays even once the corpse stops.
        let dead = PlayerPoseInput { dead: true, ..run };
        s.update(&NoClips, 0.033, &dead);
        assert_eq!(s.current(), Some("pb_death_run_forward_crumple"));
        s.update(
            &NoClips,
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
        s.update(&NoClips, 0.033, &crouch);
        s.update(
            &NoClips,
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
        s.update(&NoClips, 0.033, &PlayerPoseInput::default());
        s.update(&NoClips, 0.033, &running());
        assert!(s.previous.is_some() && s.blend > 0.0);
        assert!(
            (s.seconds - 0.033).abs() < 1e-6,
            "the new animation starts at its beginning"
        );
        for _ in 0..5 {
            s.update(&NoClips, 0.033, &running());
        }
        assert!(s.previous.is_none(), "the 170 ms fade is over");
        assert!((s.seconds - 0.198).abs() < 1e-5);
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
        s.update(&NoClips, 0.033, &rest);
        s.update(
            &NoClips,
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
        s.update(&NoClips, 0.033, &at(ws::RELOADING));
        assert!(s.torso.is_none());
        s.update(&NoClips, 0.033, &at(ws::READY));
        s.update(&NoClips, 0.033, &at(ws::FIRING));
        assert!(s.torso.is_some());
        s.update(
            &NoClips,
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
        s.update(&NoClips, 0.033, &at(ws::READY));
        s.update(&NoClips, 0.033, &firing);
        s.update(&NoClips, 0.2, &firing);
        let t = s.torso.unwrap().seconds;
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                events: [ev::FIRE_WEAPON; 4],
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
        s.update(&NoClips, 0.033, &PlayerPoseInput::default());
        s.update(&NoClips, 0.033, &hit);
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

    #[test]
    fn a_crouched_player_stumbles_in_the_crouch_clips_walking_or_running() {
        let hit = PlayerPoseInput {
            stance: StanceInput::Crouch,
            damage_timer: 300,
            damage_duration: 300,
            ..running()
        };
        assert_eq!(hit.select(), "pb_stumble_forward");
        assert_eq!(
            PlayerPoseInput {
                walking: true,
                ..hit
            }
            .select(),
            "pb_stumble_forward",
            "a crouched walk does not use the standing walk stumbles"
        );
        let pistol = PlayerPoseInput {
            weap_class: weap_class::PISTOL,
            move_dir: 90.0,
            walking: true,
            ..hit
        };
        assert_eq!(pistol.select(), "pb_stumble_pistol_left");
        let grenade = PlayerPoseInput {
            weap_class: weap_class::GRENADE,
            ..hit
        };
        assert_eq!(grenade.select(), "pb_stumble_pistol_forward");
    }

    #[test]
    fn a_torso_clip_ends_with_the_stance_the_weapon_or_the_reload() {
        let rest = PlayerPoseInput {
            weapon: 3,
            ..Default::default()
        };
        let reloading = |s: &mut PlayerPoseState| {
            s.update(&NoClips, 0.033, &rest);
            s.update(
                &NoClips,
                0.033,
                &PlayerPoseInput {
                    weapon_state: ws::RELOADING,
                    ..rest
                },
            );
            assert!(s.torso.is_some());
        };
        let mut s = PlayerPoseState::default();
        reloading(&mut s);
        // RELOAD_START then RELOADING is one reload, not two.
        let seq = s.torso_seq;
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::RELOAD_END,
                ..rest
            },
        );
        assert!(s.torso.is_some() && s.torso_seq == seq);
        // Crouching mid-reload ends it.
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::RELOAD_END,
                stance: StanceInput::Crouch,
                ..rest
            },
        );
        assert!(s.torso.is_none());
        // Interrupted (the weapon is dropped) ends it.
        let mut s = PlayerPoseState::default();
        reloading(&mut s);
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::READY,
                ..rest
            },
        );
        assert!(s.torso.is_none());
        // A switch to another weapon ends a fire clip.
        let mut s = PlayerPoseState::default();
        s.update(&NoClips, 0.033, &rest);
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::FIRING,
                ..rest
            },
        );
        assert!(s.torso.is_some());
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::FIRING,
                weapon: 4,
                ..rest
            },
        );
        assert!(s.torso.is_none());
        // But the pullout that starts the switch survives the weapon index changing.
        let mut s = PlayerPoseState::default();
        s.update(&NoClips, 0.033, &rest);
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::DROPPING,
                ..rest
            },
        );
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::RAISING,
                weapon: 4,
                ..rest
            },
        );
        assert!(s.torso.is_some());
    }

    #[test]
    fn a_fire_event_is_found_anywhere_in_the_ring() {
        let mut s = PlayerPoseState::default();
        let still = PlayerPoseInput {
            weapon_state: ws::FIRING,
            ..Default::default()
        };
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::READY,
                ..still
            },
        );
        s.update(&NoClips, 0.033, &still);
        let t = s.torso_seq;
        // Three events since the last input, the fire first, a footstep after it.
        s.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                events: [ev::FIRE_WEAPON, ev::FOOTSTEP_RUN, ev::FOOTSTEP_RUN, 0],
                event_seq: 3,
                ..still
            },
        );
        assert_ne!(s.torso_seq, t);
    }

    #[test]
    fn a_remote_player_plays_what_the_server_chose() {
        // The server decides a reload; its snapshot carries the choice; a client with no weapon state plays it.
        let rest = PlayerPoseInput::default();
        let mut server = PlayerPoseState::default();
        server.update(&NoClips, 0.033, &rest);
        server.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::RELOADING,
                ..rest
            },
        );
        let wire = server.torso_wire();
        assert_eq!(torso_name(wire.clip), Some("pt_reload_stand_rifle"));
        let mut client = PlayerPoseState::default();
        let remote = |w| PlayerPoseInput {
            torso_wire: Some(w),
            ..rest
        };
        client.update(
            &NoClips,
            0.033,
            &remote(TorsoWire {
                seq: wire.seq.wrapping_sub(1),
                ..TorsoWire::default()
            }),
        );
        assert_eq!(client.torso_choice(), None);
        client.update(&NoClips, 0.033, &remote(wire));
        assert_eq!(client.torso_choice(), Some("pt_reload_stand_rifle"));
        // The same snapshot again does not restart it.
        client.update(&NoClips, 0.5, &remote(wire));
        client.update(&NoClips, 0.033, &remote(wire));
        assert!(client.torso.unwrap().seconds > 0.5);
        // The server ending the clip ends it.
        server.update(
            &NoClips,
            0.033,
            &PlayerPoseInput {
                weapon_state: ws::READY,
                ..rest
            },
        );
        client.update(&NoClips, 0.033, &remote(server.torso_wire()));
        assert_eq!(client.torso_choice(), None);
    }

    #[test]
    fn every_clip_a_selection_returns_has_a_wire_index() {
        for name in TORSO_BODY_CLIPS.iter().chain(TORSO_CLIPS.iter()) {
            assert_eq!(torso_name(torso_index(name)), Some(*name));
        }
        assert!(TORSO_BODY_CLIPS.len() + TORSO_CLIPS.len() < 255);
    }

    // ---- locomotion: rate, blends, events, families --------------------------------------------------------------

    struct Table(&'static [(&'static str, ClipInfo)]);

    impl Clips for Table {
        fn clip(&self, name: &str) -> Option<ClipInfo> {
            self.0.iter().find(|(c, _)| *c == name).map(|(_, i)| *i)
        }
    }

    const fn clip(length: f32, move_speed: f32, looped: bool) -> ClipInfo {
        ClipInfo {
            length,
            move_speed,
            looped,
        }
    }

    const CLIPS: Table = Table(&[
        ("pb_stand_alert", clip(2.0, 0.0, true)),
        ("pb_crouch_alert", clip(2.0, 0.0, true)),
        ("pb_combatrun_forward_loop", clip(1.0, 200.0, true)),
        ("pb_combatrun_left_loop", clip(2.0, 160.0, true)),
        ("pb_climbup", clip(1.0, 60.0, true)),
        ("pb_prone2crouch", clip(1.5, 0.0, false)),
        ("pb_prone2crouchrun", clip(1.5, 40.0, false)),
        ("mp_mantle_up_57", clip(1.2, 0.0, false)),
        ("mp_mantle_over_high", clip(0.8, 0.0, false)),
    ]);

    fn frames(s: &mut PlayerPoseState, clips: &dyn Clips, n: u32, dt: f32, i: &PlayerPoseInput) {
        for _ in 0..n {
            s.update(clips, dt, i);
        }
    }

    #[test]
    fn the_playback_rate_follows_ground_speed_over_the_clips_own_speed() {
        let rate = |speed: f32, vertical: f32, clip_name: &str, c: ClipInfo| {
            PlayerPoseInput {
                speed,
                vertical_speed: vertical,
                ..running()
            }
            .rate_for(clip_name, Some(c))
        };
        let run = clip(1.0, 200.0, true);
        assert_eq!(rate(200.0, 0.0, "x", run), 1.0);
        assert_eq!(rate(100.0, 0.0, "x", run), 0.5, "half speed, half rate");
        assert_eq!(rate(3.0, 0.0, "x", run), 0.1, "never below a tenth");
        assert_eq!(rate(300.0, 0.0, "x", run), 1.5);
        assert_eq!(rate(900.0, 0.0, "x", run), 2.0, "fast clips cap at twice");
        // Slow clips may play faster: 3 at 20 units a second down to 2.0 at 150.
        let slow = clip(1.0, 80.0, true);
        assert!((rate(800.0, 0.0, "x", slow) - (3.0 - 60.0 / 130.0)).abs() < 1e-5);
        assert_eq!(rate(800.0, 0.0, "x", clip(1.0, 10.0, true)), 3.0);
        // Falling or climbing counts toward the speed.
        assert!((rate(120.0, 160.0, "x", run) - 1.0).abs() < 1e-5);
        // A clip that does not move the body plays at its own rate.
        assert_eq!(rate(300.0, 0.0, "x", clip(1.0, 0.0, true)), 1.0);
        // A ladder climb follows the vertical speed, stops with it and plays up to four times.
        let climb = clip(1.0, 60.0, true);
        assert_eq!(rate(0.0, 30.0, CLIMB_UP, climb), 0.5);
        assert_eq!(rate(0.0, 0.0, CLIMB_UP, climb), 0.0);
        assert_eq!(rate(0.0, 500.0, CLIMB_UP, climb), 4.0);
    }

    #[test]
    fn a_slower_player_plays_the_run_slower() {
        let at = |speed: f32| {
            let mut s = PlayerPoseState::default();
            let i = PlayerPoseInput { speed, ..running() };
            s.update(&CLIPS, 0.0, &i);
            frames(&mut s, &CLIPS, 10, 0.1, &i);
            (s.current(), s.seconds)
        };
        let (c, full) = at(200.0);
        assert_eq!(c, Some("pb_combatrun_forward_loop"));
        assert!((full - 1.0).abs() < 1e-4);
        let (_, half) = at(100.0);
        assert!((half - 0.5).abs() < 1e-4, "{half}");
    }

    #[test]
    fn a_switch_between_moving_loops_keeps_the_cycle() {
        let mut s = PlayerPoseState::default();
        let run = running();
        s.update(&CLIPS, 0.0, &run);
        frames(
            &mut s,
            &CLIPS,
            5,
            0.1,
            &PlayerPoseInput {
                speed: 200.0,
                ..run
            },
        );
        assert!((s.seconds - 0.5).abs() < 1e-4);
        // Strafing left: a two second clip; half a cycle in is one second.
        s.update(
            &CLIPS,
            0.0,
            &PlayerPoseInput {
                move_dir: 90.0,
                ..run
            },
        );
        assert_eq!(s.current(), Some("pb_combatrun_left_loop"));
        assert!((s.seconds - 1.0).abs() < 1e-4, "{}", s.seconds);
    }

    #[test]
    fn blends_last_what_the_switch_calls_for() {
        let still = PlayerPoseInput {
            anim_type: anim_type::AUTORIFLE,
            ..Default::default()
        };
        let blend = |from: PlayerPoseInput, to: PlayerPoseInput| {
            let mut s = PlayerPoseState::default();
            s.update(&CLIPS, 0.0, &from);
            s.update(&CLIPS, 0.0, &to);
            (s.blend_total * 1000.0).round() as i32
        };
        // Still to still, moving to still, and to moving.
        assert_eq!(
            blend(still, PlayerPoseInput { ads: true, ..still }),
            // No clip data for the ADS idle: still to still.
            170
        );
        assert_eq!(blend(running(), still), 250);
        assert_eq!(blend(still, running()), 120);
        // A crouch or prone change holds the blend to at least 400 ms.
        assert_eq!(
            blend(
                still,
                PlayerPoseInput {
                    stance: StanceInput::Crouch,
                    ..still
                }
            ),
            400
        );
        // A clip's own blend time wins over the default: the jump blends in over 100 ms.
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.0, &running());
        s.update(
            &CLIPS,
            0.0,
            &PlayerPoseInput {
                airborne: true,
                event_seq: 1,
                events: [ev::JUMP, 0, 0, 0],
                ..running()
            },
        );
        assert_eq!(s.current(), Some("pb_runjump_takeoff"));
        assert_eq!((s.blend_total * 1000.0).round() as i32, 100);
    }

    fn event(code: u8, seq: u8) -> PlayerPoseInput {
        PlayerPoseInput {
            event_seq: seq,
            events: [code, code, code, code],
            ..running()
        }
    }

    #[test]
    fn a_jump_plays_its_clip_through_the_flight_and_a_landing_the_landing_clip() {
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &running());
        let jump = PlayerPoseInput {
            airborne: true,
            ..event(ev::JUMP, 1)
        };
        s.update(&CLIPS, 0.033, &jump);
        assert_eq!(s.current(), Some("pb_runjump_takeoff"));
        let before = s.legs_wire();
        assert_ne!(before.clip, 0);
        // Airborne for a second, the horizontal speed changing: the takeoff holds.
        for speed in [190.0, 60.0, 0.0, 120.0] {
            frames(
                &mut s,
                &CLIPS,
                8,
                0.033,
                &PlayerPoseInput {
                    speed,
                    airborne: true,
                    ..event(ev::JUMP, 1)
                },
            );
            assert_eq!(s.current(), Some("pb_runjump_takeoff"), "at {speed}");
        }
        // The landing: the land clip, held for its timer, then back to the ground clip.
        let land = event(ev::LANDING_FIRST + 5, 2);
        s.update(&CLIPS, 0.033, &land);
        assert_eq!(s.current(), Some("pb_runjump_land"));
        s.update(&CLIPS, 0.033, &land);
        assert_eq!(s.current(), Some("pb_runjump_land"));
        frames(&mut s, &CLIPS, 5, 0.033, &land);
        assert_eq!(s.current(), Some("pb_combatrun_forward_loop"));
        assert_eq!(s.legs_wire().clip, 0);
        assert_ne!(s.legs_wire().seq, before.seq, "the end is announced too");
    }

    #[test]
    fn a_standing_jump_and_a_pistol_landing_pick_their_own_clips() {
        let still = PlayerPoseInput {
            anim_type: anim_type::PISTOL,
            weap_class: weap_class::PISTOL,
            ..Default::default()
        };
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &still);
        s.update(
            &CLIPS,
            0.033,
            &PlayerPoseInput {
                airborne: true,
                event_seq: 1,
                events: [ev::JUMP; 4],
                ..still
            },
        );
        assert_eq!(s.current(), Some("pb_standjump_takeoff"));
        s.update(
            &CLIPS,
            0.033,
            &PlayerPoseInput {
                event_seq: 2,
                events: [ev::LANDING_PAIN_FIRST + 3; 4],
                ..still
            },
        );
        assert_eq!(s.current(), Some("pb_standjump_land_pistol"));
    }

    #[test]
    fn a_long_fall_without_a_jump_plays_the_jump_and_a_short_hop_does_not() {
        let still = PlayerPoseInput {
            airborne: true,
            anim_type: anim_type::AUTORIFLE,
            ..Default::default()
        };
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &PlayerPoseInput::default());
        frames(&mut s, &CLIPS, 8, 0.033, &still);
        assert_eq!(
            s.current(),
            Some("pb_stand_alert"),
            "a step down is nothing"
        );
        frames(&mut s, &CLIPS, 8, 0.033, &still);
        assert_eq!(s.current(), Some("pb_standjump_takeoff"));
    }

    #[test]
    fn a_remote_player_plays_the_legs_clip_the_server_chose() {
        let wire = |clip: u8, seq: u8| PlayerPoseInput {
            legs_wire: Some(LegsWire { clip, seq }),
            ..Default::default()
        };
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &wire(0, 1));
        assert_eq!(s.current(), Some("pb_stand_alert"));
        s.update(&CLIPS, 0.033, &wire(legs_index("pb_standjump_land"), 2));
        assert_eq!(s.current(), Some("pb_standjump_land"));
        // It stays until the server says otherwise, however long that takes.
        frames(
            &mut s,
            &CLIPS,
            30,
            0.033,
            &wire(legs_index("pb_standjump_land"), 2),
        );
        assert_eq!(s.current(), Some("pb_standjump_land"));
        s.update(&CLIPS, 0.033, &wire(0, 3));
        assert_eq!(s.current(), Some("pb_stand_alert"));
        // The death clip the server chose is the corpse's pose, for a viewer who saw the death and for one who did not.
        let dead = PlayerPoseInput {
            dead: true,
            ..wire(legs_index("pb_stand_death_legs"), 4)
        };
        s.update(&CLIPS, 0.033, &dead);
        assert_eq!(s.current(), Some("pb_stand_death_legs"));
        let mut late = PlayerPoseState::default();
        late.update(&CLIPS, 0.033, &dead);
        assert_eq!(late.current(), Some("pb_stand_death_legs"));
    }

    #[test]
    fn leaving_prone_plays_the_transition_by_whether_the_player_was_moving() {
        let prone = PlayerPoseInput {
            stance: StanceInput::Prone,
            ..running()
        };
        let crouch = PlayerPoseInput {
            stance: StanceInput::Crouch,
            anim_type: anim_type::AUTORIFLE,
            ..Default::default()
        };
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &prone);
        s.update(&CLIPS, 0.033, &crouch);
        assert_eq!(s.current(), Some("pb_prone2crouchrun"));
        let mut s = PlayerPoseState::default();
        s.update(
            &CLIPS,
            0.033,
            &PlayerPoseInput {
                stance: StanceInput::Prone,
                ..Default::default()
            },
        );
        s.update(&CLIPS, 0.033, &crouch);
        assert_eq!(s.current(), Some("pb_prone2crouch"));
        // 1.5 s and 50 ms later the crouch idle returns.
        frames(&mut s, &CLIPS, 60, 0.033, &crouch);
        assert_eq!(s.current(), Some("pb_crouch_alert"));
    }

    #[test]
    fn a_mantle_plays_the_up_clip_then_the_over_clip() {
        let up = PlayerPoseInput {
            mantle: true,
            mantle_trans: 0,
            mantle_over: true,
            ..Default::default()
        };
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &PlayerPoseInput::default());
        s.update(&CLIPS, 0.033, &up);
        assert_eq!(s.current(), Some("mp_mantle_up_57"));
        s.update(
            &CLIPS,
            0.033,
            &PlayerPoseInput {
                mantle_timer: 1100,
                ..up
            },
        );
        assert_eq!(s.current(), Some("mp_mantle_up_57"));
        s.update(
            &CLIPS,
            0.033,
            &PlayerPoseInput {
                mantle_timer: 1200,
                ..up
            },
        );
        assert_eq!(s.current(), Some("mp_mantle_over_high"));
        s.update(&CLIPS, 0.033, &PlayerPoseInput::default());
        assert_eq!(s.current(), Some("pb_stand_alert"), "the climb is over");
        // A mantle that stops at the top has no over clip.
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &PlayerPoseInput::default());
        s.update(
            &CLIPS,
            0.033,
            &PlayerPoseInput {
                mantle_over: false,
                mantle_timer: 5000,
                ..up
            },
        );
        assert_eq!(s.current(), Some("mp_mantle_up_57"));
    }

    #[test]
    fn a_death_is_one_of_the_clips_the_script_lists_for_it() {
        let die = |i: PlayerPoseInput, seed: u32| {
            PlayerPoseInput {
                dead: true,
                seed,
                ..i
            }
            .select()
        };
        let stand = PlayerPoseInput::default();
        let picks: std::collections::HashSet<_> = (0..8).map(|k| die(stand, k)).collect();
        assert_eq!(picks.len(), DEATH_STAND.len(), "every stand death turns up");
        assert!(picks.iter().all(|c| DEATH_STAND.contains(c)));
        let crouch = PlayerPoseInput {
            stance: StanceInput::Crouch,
            ..stand
        };
        assert!((0..5).all(|k| die(crouch, k) == DEATH_CROUCH[k as usize]));
        let run = running();
        assert!((0..3).all(|k| die(run, k) == DEATH_RUN[k as usize]));
        let crouch_run = PlayerPoseInput {
            stance: StanceInput::Crouch,
            ..run
        };
        assert!((0..2).all(|k| die(crouch_run, k) == DEATH_CROUCH_RUN[k as usize]));
        // Strafing deaths apply crouched too; a back run has its own; a walk or a crouched back run falls to the stand
        // deaths.
        let strafe = |i: PlayerPoseInput, d: f32| PlayerPoseInput { move_dir: d, ..i };
        assert_eq!(die(strafe(run, 90.0), 0), DEATH_RUN_LEFT);
        assert_eq!(die(strafe(crouch_run, -90.0), 0), DEATH_RUN_RIGHT);
        let back = PlayerPoseInput {
            backward: true,
            ..run
        };
        assert_eq!(die(back, 0), DEATH_RUN_BACK);
        let crouch_back = PlayerPoseInput {
            stance: StanceInput::Crouch,
            ..back
        };
        assert!(DEATH_STAND.contains(&die(crouch_back, 0)));
        let walk = PlayerPoseInput {
            walking: true,
            ..run
        };
        assert!(DEATH_STAND.contains(&die(walk, 0)));
        assert_eq!(
            die(
                PlayerPoseInput {
                    stance: StanceInput::Prone,
                    ..run
                },
                7
            ),
            DEATH_PRONE
        );
    }

    #[test]
    fn a_mounted_gunner_plays_the_aim_pose_of_the_stance() {
        let on = |stance| PlayerPoseInput {
            turret: true,
            stance,
            ..running()
        };
        assert_eq!(on(StanceInput::Stand).select(), MOUNTED[0]);
        assert_eq!(on(StanceInput::Crouch).select(), MOUNTED[1]);
        assert_eq!(on(StanceInput::Prone).select(), MOUNTED[2]);
    }

    #[test]
    fn the_grenade_unarmed_briefcase_and_hold_sets_have_their_own_clips() {
        let with = |anim_type, weap_class, f: fn(PlayerPoseInput) -> PlayerPoseInput| {
            f(PlayerPoseInput {
                anim_type,
                weap_class,
                ..running()
            })
            .select()
        };
        let grenade = (anim_type::GRENADE, weap_class::GRENADE);
        assert_eq!(
            with(grenade.0, grenade.1, |i| i),
            "pb_combatrun_forward_loop_stickgrenade"
        );
        assert_eq!(
            with(grenade.0, grenade.1, |i| PlayerPoseInput {
                move_dir: 90.0,
                ..i
            }),
            "pb_combatrun_left_loop_grenade"
        );
        assert_eq!(
            with(grenade.0, grenade.1, |i| PlayerPoseInput {
                stance: StanceInput::Crouch,
                move_dir: -90.0,
                ..i
            }),
            "pb_crouch_run_right_grenade"
        );
        assert_eq!(
            with(grenade.0, grenade.1, |i| PlayerPoseInput {
                stance: StanceInput::Prone,
                ..i
            }),
            "pb_prone_grenade_crawl"
        );
        assert_eq!(
            with(grenade.0, grenade.1, |i| PlayerPoseInput {
                stance: StanceInput::Crouch,
                walking: true,
                ..i
            }),
            "pb_crouch_walk_forward_pistol"
        );
        // Unarmed.
        assert_eq!(with(anim_type::NONE, 0, |i| i), "pb_pistol_run_fast");
        assert_eq!(
            with(anim_type::NONE, 0, |i| PlayerPoseInput {
                walking: true,
                ..i
            }),
            "pb_stand_shoot_walk_forward_unarmed"
        );
        assert_eq!(
            with(anim_type::NONE, 0, |i| PlayerPoseInput {
                stance: StanceInput::Crouch,
                ..i
            }),
            "pb_crouch_run_forward_grenade"
        );
        // The briefcase plants when still and runs like a rifle.
        assert_eq!(
            with(anim_type::BRIEFCASE, 0, |i| i),
            "pb_combatrun_forward_loop"
        );
        assert_eq!(
            with(anim_type::BRIEFCASE, 0, |i| PlayerPoseInput {
                speed: 0.0,
                stance: StanceInput::Crouch,
                ..i
            }),
            "pb_crouch_bombplant"
        );
        // Walking with the hold clips has no strafe variants; running does.
        assert_eq!(
            with(anim_type::HOLD, 0, |i| PlayerPoseInput {
                walking: true,
                move_dir: 90.0,
                ..i
            }),
            "pb_hold_run"
        );
        assert_eq!(
            with(anim_type::HOLD, 0, |i| PlayerPoseInput {
                move_dir: 90.0,
                ..i
            }),
            "pb_hold_run_left"
        );
    }

    #[test]
    fn a_leaning_player_walks() {
        // The lean is the walk key of the clips: `PM_Footsteps` treats a lean as walking, which the wire carries only
        // as the lean itself.
        let ps = PlayerState {
            leanf: 1.0,
            ..PlayerState::default()
        };
        let i = PlayerPoseInput::from_ps(&ps, true, None);
        assert!(i.walking && i.lean == 1.0);
    }

    #[test]
    fn the_legs_stay_planted_for_a_small_turn_and_a_lean_tilts_the_body() {
        let mut s = PlayerPoseState::default();
        let at = |yaw: f32, lean: f32| PlayerPoseInput {
            yaw,
            lean,
            anim_type: anim_type::AUTORIFLE,
            ..Default::default()
        };
        s.update(&NoClips, 0.016, &at(0.0, 0.0));
        let planted = s.swing.legs_yaw;
        frames(&mut s, &NoClips, 10, 0.016, &at(15.0, 0.0));
        assert_eq!(s.swing.legs_yaw, planted, "a 15 degree turn");
        frames(&mut s, &NoClips, 60, 0.016, &at(60.0, 0.0));
        assert!(
            (s.swing.legs_yaw - planted).abs() > 1.0,
            "the legs follow once the view is past the tolerance"
        );
        assert_eq!(s.ctl.angles[0][2], 0.0);
        frames(&mut s, &NoClips, 60, 0.016, &at(60.0, 1.0));
        assert!(s.ctl.angles[0][2] > 0.0 && s.ctl.tag_origin_angles[2] > 0.0);
    }

    #[test]
    fn a_corpse_holds_the_death_clip_on_the_wire_without_a_new_one_every_frame() {
        let run = running();
        let mut s = PlayerPoseState::default();
        s.update(&CLIPS, 0.033, &run);
        let dead = PlayerPoseInput {
            dead: true,
            seed: 1,
            ..run
        };
        s.update(&CLIPS, 0.033, &dead);
        assert_eq!(s.current(), Some("pb_death_run_onfront"));
        let wire = s.legs_wire();
        assert_eq!(legs_name(wire.clip), Some("pb_death_run_onfront"));
        frames(
            &mut s,
            &CLIPS,
            20,
            0.033,
            &PlayerPoseInput { speed: 0.0, ..dead },
        );
        assert_eq!(s.legs_wire(), wire, "nothing new is announced for a corpse");
        assert_eq!(s.current(), Some("pb_death_run_onfront"));
        // Respawning ends it.
        s.update(&CLIPS, 0.033, &PlayerPoseInput::default());
        assert_eq!(s.current(), Some("pb_stand_alert"));
        assert_eq!(s.legs_wire().clip, 0);
    }

    #[test]
    fn every_death_clip_can_be_named_on_the_legs_wire() {
        let all: Vec<_> = DEATH_STAND
            .iter()
            .chain(&DEATH_CROUCH)
            .chain(&DEATH_RUN)
            .chain(&DEATH_CROUCH_RUN)
            .chain(&[
                DEATH_PRONE,
                DEATH_RUN_BACK,
                DEATH_RUN_LEFT,
                DEATH_RUN_RIGHT,
                LASTSTAND_DEATH,
            ])
            .collect();
        assert!(all.iter().all(|c| legs_index(c) != 0));
        assert!(legs_wire_clips().count() < 256);
    }
}
