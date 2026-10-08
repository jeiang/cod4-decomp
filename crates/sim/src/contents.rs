// SPDX-License-Identifier: GPL-3.0-only
//! Brush content flags and the trace masks built from them (IW3 values).

pub const SOLID: i32 = 0x1;
pub const FOLIAGE: i32 = 0x2;
pub const NONCOLLIDING: i32 = 0x4;
pub const VEHICLETRIGGER: i32 = 0x8;
pub const GLASS: i32 = 0x10;
pub const WATER: i32 = 0x20;
pub const CANSHOOTCLIP: i32 = 0x40;
pub const MISSILECLIP: i32 = 0x80;
pub const ITEM: i32 = 0x100;
pub const VEHICLECLIP: i32 = 0x200;
pub const ITEMCLIP: i32 = 0x400;
pub const SKY: i32 = 0x800;
pub const AI_NOSIGHT: i32 = 0x1000;
pub const CLIPSHOT: i32 = 0x2000;
pub const ACTOR: i32 = 0x4000;
pub const FAKE_ACTOR: i32 = 0x8000;
pub const PLAYERCLIP: i32 = 0x10000;
pub const MONSTERCLIP: i32 = 0x20000;
pub const AXISTRIGGER: i32 = 0x40000;
pub const ALLIESTRIGGER: i32 = 0x80000;
pub const NEUTRALTRIGGER: i32 = 0x100000;
pub const USE: i32 = 0x200000;
pub const NONSENTIENTTRIGGER: i32 = 0x400000;
pub const VEHICLE: i32 = 0x800000;
pub const MANTLE: i32 = 0x1000000;
pub const PLAYER: i32 = 0x2000000;
pub const CORPSE: i32 = 0x4000000;
pub const DETAIL: i32 = 0x8000000;
pub const STRUCTURAL: i32 = 0x10000000;
pub const TRANSLUCENT: i32 = 0x20000000;
pub const PLAYERTRIGGER: i32 = 0x40000000;
pub const NODROP: i32 = i32::MIN;

pub const MASK_ALL: i32 = -1;
pub const MASK_SOLID: i32 = SOLID;
pub const MASK_WATER: i32 = WATER;
pub const MASK_PLAYERSOLID: i32 = SOLID | GLASS | PLAYERCLIP | VEHICLE | PLAYER;
pub const MASK_SHOT: i32 = SOLID | GLASS | WATER | SKY | CLIPSHOT | ACTOR | VEHICLE | PLAYER;
pub const MASK_CHARACTER: i32 = PLAYER | ACTOR | FAKE_ACTOR;
pub const MASK_IGNORE_CHARACTERS: i32 = !MASK_CHARACTER;
pub const MASK_DEADSOLID: i32 = MASK_PLAYERSOLID & !PLAYER;
