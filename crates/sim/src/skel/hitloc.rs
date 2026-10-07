// SPDX-License-Identifier: GPL-3.0-or-later
//! Hit locations, the bullet priority maps, and the box fallback used without a skeleton.

use crate::Vec3;

/// Number of hit locations (`HITLOC_NUM`); also the length of a weapon's
/// `location_damage_multipliers`.
pub const COUNT: usize = 19;

/// Hit locations in the order of `WeaponDef::location_damage_multipliers`. A model's
/// `part_classification[bone]` holds one of these as a plain index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum HitLocation {
    #[default]
    None = 0,
    Helmet,
    Head,
    Neck,
    TorsoUpper,
    TorsoLower,
    RightArmUpper,
    LeftArmUpper,
    RightArmLower,
    LeftArmLower,
    RightHand,
    LeftHand,
    RightLegUpper,
    LeftLegUpper,
    RightLegLower,
    LeftLegLower,
    RightFoot,
    LeftFoot,
    Gun,
}

/// The names GSC sees (`g_HitLocNames`), indexed by hit location.
pub const NAMES: [&str; COUNT] = [
    "none",
    "helmet",
    "head",
    "neck",
    "torso_upper",
    "torso_lower",
    "right_arm_upper",
    "left_arm_upper",
    "right_arm_lower",
    "left_arm_lower",
    "right_hand",
    "left_hand",
    "right_leg_upper",
    "left_leg_upper",
    "right_leg_lower",
    "left_leg_lower",
    "right_foot",
    "left_foot",
    "gun",
];

const ALL: [HitLocation; COUNT] = {
    use HitLocation::*;
    [
        None,
        Helmet,
        Head,
        Neck,
        TorsoUpper,
        TorsoLower,
        RightArmUpper,
        LeftArmUpper,
        RightArmLower,
        LeftArmLower,
        RightHand,
        LeftHand,
        RightLegUpper,
        LeftLegUpper,
        RightLegLower,
        LeftLegLower,
        RightFoot,
        LeftFoot,
        Gun,
    ]
};

impl HitLocation {
    pub fn from_index(i: u8) -> Option<Self> {
        ALL.get(usize::from(i)).copied()
    }

    /// The GSC string for this location.
    pub fn name(self) -> &'static str {
        NAMES[self as usize]
    }

    /// Case-insensitive; `None` for an unknown name.
    pub fn from_name(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .map(|i| ALL[i])
    }

    /// Head and helmet hits are what the original turns into `MOD_HEAD_SHOT`.
    pub fn is_head(self) -> bool {
        matches!(self, Self::Head | Self::Helmet)
    }
}

/// `name(idx)` for a raw hit-location index; `"none"` past the table.
pub fn name(idx: u8) -> &'static str {
    NAMES.get(usize::from(idx)).copied().unwrap_or(NAMES[0])
}

/// `from_name` returning the raw index.
pub fn from_name(name: &str) -> Option<u8> {
    HitLocation::from_name(name).map(|l| l as u8)
}

/// Per-classification priorities (`bulletPriorityMap`); 20 entries, one past the locations.
///
/// A bone is only hittable at priority 2 or more; 1 means "take the parent bone's
/// classification"; 0 is never hit (the gun). A higher-priority bone wins over a nearer
/// lower-priority one along the same segment.
pub type PriorityMap = [u8; COUNT + 1];

/// Handgun, SMG, shotgun and other non-rifle bullets: every body part ties, so the nearest
/// bone wins.
pub const BULLET_PRIORITY: PriorityMap =
    [1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 0, 0];

/// Rifle bullets (`bRifleBullet`): a bullet that passes through several parts is credited to
/// the most valuable one (helmet, head, neck, torso, ... feet).
pub const RIFLE_PRIORITY: PriorityMap =
    [1, 9, 9, 9, 8, 7, 6, 6, 6, 6, 5, 5, 4, 4, 4, 4, 3, 3, 0, 0];

/// Stance for [`box_hit_location`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stance {
    Stand,
    Crouch,
    Prone,
}

impl Stance {
    /// Top of the player's hull above the feet, as `pmove` sizes it.
    pub fn height(self) -> f32 {
        match self {
            Stance::Stand => 70.0,
            Stance::Crouch => 50.0,
            Stance::Prone => 30.0,
        }
    }
}

/// Fallback hit location from where a segment crosses the player's hull box: the height of
/// the crossing within the stance's hull (head above 88 %, upper torso above 65 %, lower
/// torso above 45 %, upper legs above 25 %, lower legs above 8 %, feet below), and left or
/// right arms/legs by which side of the player's facing the crossing is on. Returns `None`
/// when the segment misses the 30-unit-wide hull.
///
/// A prone player is treated the same way over its 30-unit-high hull, so prone hits come
/// out mostly torso and head; the skeleton trace is the accurate path.
pub fn box_hit_location(
    start: &Vec3,
    end: &Vec3,
    origin: &Vec3,
    yaw_deg: f32,
    stance: Stance,
) -> Option<(f32, HitLocation)> {
    let half = 15.0f32;
    let (mins, maxs) = (
        [origin[0] - half, origin[1] - half, origin[2]],
        [
            origin[0] + half,
            origin[1] + half,
            origin[2] + stance.height(),
        ],
    );
    let d = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for a in 0..3 {
        if d[a] == 0.0 {
            if start[a] < mins[a] || start[a] > maxs[a] {
                return None;
            }
            continue;
        }
        let (mut near, mut far) = ((mins[a] - start[a]) / d[a], (maxs[a] - start[a]) / d[a]);
        if near > far {
            std::mem::swap(&mut near, &mut far);
        }
        t0 = t0.max(near);
        t1 = t1.min(far);
        if t0 > t1 {
            return None;
        }
    }
    let p = [
        start[0] + d[0] * t0,
        start[1] + d[1] * t0,
        start[2] + d[2] * t0,
    ];
    let rel = [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]];
    let (s, c) = crate::pm::math::sincos_deg(yaw_deg);
    // Facing is +x rotated by yaw; the player's left is +y of that frame.
    let left = -rel[0] * s + rel[1] * c > 0.0;
    use HitLocation as H;
    let pick = |right: HitLocation, lft: HitLocation| if left { lft } else { right };
    let f = rel[2] / stance.height();
    let loc = if f >= 0.88 {
        H::Head
    } else if f >= 0.65 {
        H::TorsoUpper
    } else if f >= 0.45 {
        H::TorsoLower
    } else if f >= 0.25 {
        pick(H::RightLegUpper, H::LeftLegUpper)
    } else if f >= 0.08 {
        pick(H::RightLegLower, H::LeftLegLower)
    } else {
        pick(H::RightFoot, H::LeftFoot)
    };
    Some((t0, loc))
}
