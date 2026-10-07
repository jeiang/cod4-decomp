// SPDX-License-Identifier: GPL-3.0-or-later
//! WeaponDef: the fixed weapon definition with its model, effect, material and
//! sound references.

use super::error::{Result, ZoneError};
use super::fx::{self, FxEffectDef};
use super::gfx::{self, Material, Name};
use super::stream::{Fields, Ptr, Stream};
use super::xmodel::{self, XModel};
use std::sync::Arc;

/// Size of the weapon header in the stream.
const WEAPON_SIZE: u32 = 2168;
/// Entries in the bounce sound table.
const BOUNCE_SOUNDS: u32 = 29;

/// Sound alias names the weapon plays; `None` when unset.
#[derive(Debug)]
pub struct WeaponSounds {
    pub pickup_sound: Name,
    pub pickup_sound_player: Name,
    pub ammo_pickup_sound: Name,
    pub ammo_pickup_sound_player: Name,
    pub projectile_sound: Name,
    pub pullback_sound: Name,
    pub pullback_sound_player: Name,
    pub fire_sound: Name,
    pub fire_sound_player: Name,
    pub fire_loop_sound: Name,
    pub fire_loop_sound_player: Name,
    pub fire_stop_sound: Name,
    pub fire_stop_sound_player: Name,
    pub fire_last_sound: Name,
    pub fire_last_sound_player: Name,
    pub empty_fire_sound: Name,
    pub empty_fire_sound_player: Name,
    pub melee_swipe_sound: Name,
    pub melee_swipe_sound_player: Name,
    pub melee_hit_sound: Name,
    pub melee_miss_sound: Name,
    pub rechamber_sound: Name,
    pub rechamber_sound_player: Name,
    pub reload_sound: Name,
    pub reload_sound_player: Name,
    pub reload_empty_sound: Name,
    pub reload_empty_sound_player: Name,
    pub reload_start_sound: Name,
    pub reload_start_sound_player: Name,
    pub reload_end_sound: Name,
    pub reload_end_sound_player: Name,
    pub detonate_sound: Name,
    pub detonate_sound_player: Name,
    pub night_vision_wear_sound: Name,
    pub night_vision_wear_sound_player: Name,
    pub night_vision_remove_sound: Name,
    pub night_vision_remove_sound_player: Name,
    pub alt_switch_sound: Name,
    pub alt_switch_sound_player: Name,
    pub raise_sound: Name,
    pub raise_sound_player: Name,
    pub first_raise_sound: Name,
    pub first_raise_sound_player: Name,
    pub putaway_sound: Name,
    pub putaway_sound_player: Name,
}

impl WeaponSounds {
    fn load(s: &mut Stream, f: &mut Fields) -> Result<Self> {
        Ok(WeaponSounds {
            pickup_sound: custom_sound(s, f)?,
            pickup_sound_player: custom_sound(s, f)?,
            ammo_pickup_sound: custom_sound(s, f)?,
            ammo_pickup_sound_player: custom_sound(s, f)?,
            projectile_sound: custom_sound(s, f)?,
            pullback_sound: custom_sound(s, f)?,
            pullback_sound_player: custom_sound(s, f)?,
            fire_sound: custom_sound(s, f)?,
            fire_sound_player: custom_sound(s, f)?,
            fire_loop_sound: custom_sound(s, f)?,
            fire_loop_sound_player: custom_sound(s, f)?,
            fire_stop_sound: custom_sound(s, f)?,
            fire_stop_sound_player: custom_sound(s, f)?,
            fire_last_sound: custom_sound(s, f)?,
            fire_last_sound_player: custom_sound(s, f)?,
            empty_fire_sound: custom_sound(s, f)?,
            empty_fire_sound_player: custom_sound(s, f)?,
            melee_swipe_sound: custom_sound(s, f)?,
            melee_swipe_sound_player: custom_sound(s, f)?,
            melee_hit_sound: custom_sound(s, f)?,
            melee_miss_sound: custom_sound(s, f)?,
            rechamber_sound: custom_sound(s, f)?,
            rechamber_sound_player: custom_sound(s, f)?,
            reload_sound: custom_sound(s, f)?,
            reload_sound_player: custom_sound(s, f)?,
            reload_empty_sound: custom_sound(s, f)?,
            reload_empty_sound_player: custom_sound(s, f)?,
            reload_start_sound: custom_sound(s, f)?,
            reload_start_sound_player: custom_sound(s, f)?,
            reload_end_sound: custom_sound(s, f)?,
            reload_end_sound_player: custom_sound(s, f)?,
            detonate_sound: custom_sound(s, f)?,
            detonate_sound_player: custom_sound(s, f)?,
            night_vision_wear_sound: custom_sound(s, f)?,
            night_vision_wear_sound_player: custom_sound(s, f)?,
            night_vision_remove_sound: custom_sound(s, f)?,
            night_vision_remove_sound_player: custom_sound(s, f)?,
            alt_switch_sound: custom_sound(s, f)?,
            alt_switch_sound_player: custom_sound(s, f)?,
            raise_sound: custom_sound(s, f)?,
            raise_sound_player: custom_sound(s, f)?,
            first_raise_sound: custom_sound(s, f)?,
            first_raise_sound_player: custom_sound(s, f)?,
            putaway_sound: custom_sound(s, f)?,
            putaway_sound_player: custom_sound(s, f)?,
        })
    }
}

/// One AI accuracy-versus-range curve.
#[derive(Debug)]
pub struct AccuracyGraph {
    pub name: Name,
    /// `(range, accuracy)` knots.
    pub knots: Arc<[[f32; 2]]>,
    /// The authored knots; the stream stores them with the same count as `knots`.
    pub original_knots: Arc<[[f32; 2]]>,
    /// The header's separate count for the authored knots.
    pub original_knot_count: i32,
}

impl AccuracyGraph {
    fn load(
        s: &mut Stream,
        [name, knots, original]: [Ptr; 3],
        [count, original_count]: [i32; 2],
    ) -> Result<Self> {
        let n =
            u32::try_from(count).map_err(|_| ZoneError::Invalid("negative accuracy knot count"))?;
        let name = s.string(name)?;
        let knots = knot_array(s, knots, n)?;
        let original_knots = knot_array(s, original, n)?;
        Ok(AccuracyGraph {
            name,
            knots,
            original_knots,
            original_knot_count: original_count,
        })
    }
}

fn knot_array(s: &mut Stream, p: Ptr, n: u32) -> Result<Arc<[[f32; 2]]>> {
    s.array(p, n, 4, 8, |_, f| Ok([f.f32(), f.f32()]))
}

/// A weapon definition. Enumerations are stored as their integer codes;
/// `script`-string fields hold indices into the zone's script-string table.
#[derive(Debug)]
pub struct WeaponDef {
    pub internal_name: Name,
    pub display_name: Name,
    pub overlay_name: Name,
    pub gun_models: Vec<Option<Arc<XModel>>>,
    pub hand_model: Option<Arc<XModel>>,
    pub anims: Vec<Name>,
    pub mode_name: Name,
    /// Script-string indices.
    pub hide_tags: [u16; 8],
    /// Script-string indices.
    pub notetrack_sound_map_keys: [u16; 16],
    /// Script-string indices.
    pub notetrack_sound_map_values: [u16; 16],
    pub player_anim_type: i32,
    pub weap_type: i32,
    pub weap_class: i32,
    pub penetrate_type: i32,
    pub impact_type: i32,
    pub inventory_type: i32,
    pub fire_type: i32,
    pub offhand_class: i32,
    pub stance: i32,
    pub view_flash_effect: Option<Arc<FxEffectDef>>,
    pub world_flash_effect: Option<Arc<FxEffectDef>>,
    pub sounds: WeaponSounds,
    /// Per-surface bounce sounds (29 entries) when present.
    pub bounce_sound: Option<Arc<[Name]>>,
    pub view_shell_eject_effect: Option<Arc<FxEffectDef>>,
    pub world_shell_eject_effect: Option<Arc<FxEffectDef>>,
    pub view_last_shot_eject_effect: Option<Arc<FxEffectDef>>,
    pub world_last_shot_eject_effect: Option<Arc<FxEffectDef>>,
    pub reticle_center: Option<Arc<Material>>,
    pub reticle_side: Option<Arc<Material>>,
    pub reticle_center_size: i32,
    pub reticle_side_size: i32,
    pub reticle_min_ofs: i32,
    pub active_reticle_type: i32,
    pub v_stand_move: [f32; 3],
    pub v_stand_rot: [f32; 3],
    pub v_ducked_ofs: [f32; 3],
    pub v_ducked_move: [f32; 3],
    pub v_ducked_rot: [f32; 3],
    pub v_prone_ofs: [f32; 3],
    pub v_prone_move: [f32; 3],
    pub v_prone_rot: [f32; 3],
    pub pos_move_rate: f32,
    pub pos_prone_move_rate: f32,
    pub stand_move_min_speed: f32,
    pub ducked_move_min_speed: f32,
    pub prone_move_min_speed: f32,
    pub pos_rot_rate: f32,
    pub pos_prone_rot_rate: f32,
    pub stand_rot_min_speed: f32,
    pub ducked_rot_min_speed: f32,
    pub prone_rot_min_speed: f32,
    pub world_models: Vec<Option<Arc<XModel>>>,
    pub world_clip_model: Option<Arc<XModel>>,
    pub rocket_model: Option<Arc<XModel>>,
    pub knife_model: Option<Arc<XModel>>,
    pub world_knife_model: Option<Arc<XModel>>,
    pub hud_icon: Option<Arc<Material>>,
    pub hud_icon_ratio: i32,
    pub ammo_counter_icon: Option<Arc<Material>>,
    pub ammo_counter_icon_ratio: i32,
    pub ammo_counter_clip: i32,
    pub start_ammo: i32,
    pub ammo_name: Name,
    pub ammo_index: i32,
    pub clip_name: Name,
    pub clip_index: i32,
    pub max_ammo: i32,
    pub clip_size: i32,
    pub shot_count: i32,
    pub shared_ammo_cap_name: Name,
    pub shared_ammo_cap_index: i32,
    pub shared_ammo_cap: i32,
    pub damage: i32,
    pub player_damage: i32,
    pub melee_damage: i32,
    pub damage_type: i32,
    pub fire_delay: i32,
    pub melee_delay: i32,
    pub melee_charge_delay: i32,
    pub detonate_delay: i32,
    pub fire_time: i32,
    pub rechamber_time: i32,
    pub rechamber_bolt_time: i32,
    pub hold_fire_time: i32,
    pub detonate_time: i32,
    pub melee_time: i32,
    pub melee_charge_time: i32,
    pub reload_time: i32,
    pub reload_show_rocket_time: i32,
    pub reload_empty_time: i32,
    pub reload_add_time: i32,
    pub reload_start_time: i32,
    pub reload_start_add_time: i32,
    pub reload_end_time: i32,
    pub drop_time: i32,
    pub raise_time: i32,
    pub alt_drop_time: i32,
    pub alt_raise_time: i32,
    pub quick_drop_time: i32,
    pub quick_raise_time: i32,
    pub first_raise_time: i32,
    pub empty_raise_time: i32,
    pub empty_drop_time: i32,
    pub sprint_in_time: i32,
    pub sprint_loop_time: i32,
    pub sprint_out_time: i32,
    pub night_vision_wear_time: i32,
    pub night_vision_wear_time_fade_out_end: i32,
    pub night_vision_wear_time_power_up: i32,
    pub night_vision_remove_time: i32,
    pub night_vision_remove_time_power_down: i32,
    pub night_vision_remove_time_fade_in_start: i32,
    pub fuse_time: i32,
    pub ai_fuse_time: i32,
    pub require_lockon_to_fire: i32,
    pub no_ads_when_mag_empty: i32,
    pub avoid_drop_cleanup: i32,
    pub auto_aim_range: f32,
    pub aim_assist_range: f32,
    pub aim_assist_range_ads: f32,
    pub aim_padding: f32,
    pub enemy_crosshair_range: f32,
    pub crosshair_color_change: i32,
    pub move_speed_scale: f32,
    pub ads_move_speed_scale: f32,
    pub sprint_duration_scale: f32,
    pub ads_zoom_fov: f32,
    pub ads_zoom_in_frac: f32,
    pub ads_zoom_out_frac: f32,
    pub overlay_material: Option<Arc<Material>>,
    pub overlay_material_low_res: Option<Arc<Material>>,
    pub overlay_reticle: i32,
    pub overlay_interface: i32,
    pub overlay_width: f32,
    pub overlay_height: f32,
    pub ads_bob_factor: f32,
    pub ads_view_bob_mult: f32,
    pub hip_spread_stand_min: f32,
    pub hip_spread_ducked_min: f32,
    pub hip_spread_prone_min: f32,
    pub hip_spread_stand_max: f32,
    pub hip_spread_ducked_max: f32,
    pub hip_spread_prone_max: f32,
    pub hip_spread_decay_rate: f32,
    pub hip_spread_fire_add: f32,
    pub hip_spread_turn_add: f32,
    pub hip_spread_move_add: f32,
    pub hip_spread_ducked_decay: f32,
    pub hip_spread_prone_decay: f32,
    pub hip_reticle_side_pos: f32,
    pub ads_trans_in_time: i32,
    pub ads_trans_out_time: i32,
    pub ads_idle_amount: f32,
    pub hip_idle_amount: f32,
    pub ads_idle_speed: f32,
    pub hip_idle_speed: f32,
    pub idle_crouch_factor: f32,
    pub idle_prone_factor: f32,
    pub gun_max_pitch: f32,
    pub gun_max_yaw: f32,
    pub sway_max_angle: f32,
    pub sway_lerp_speed: f32,
    pub sway_pitch_scale: f32,
    pub sway_yaw_scale: f32,
    pub sway_horiz_scale: f32,
    pub sway_vert_scale: f32,
    pub sway_shell_shock_scale: f32,
    pub ads_sway_max_angle: f32,
    pub ads_sway_lerp_speed: f32,
    pub ads_sway_pitch_scale: f32,
    pub ads_sway_yaw_scale: f32,
    pub ads_sway_horiz_scale: f32,
    pub ads_sway_vert_scale: f32,
    pub rifle_bullet: i32,
    pub armor_piercing: i32,
    pub bolt_action: i32,
    pub aim_down_sight: i32,
    pub rechamber_while_ads: i32,
    pub ads_view_error_min: f32,
    pub ads_view_error_max: f32,
    pub cook_off_hold: i32,
    pub clip_only: i32,
    pub ads_fire_only: i32,
    pub cancel_auto_holster_when_empty: i32,
    pub suppress_ammo_reserve_display: i32,
    pub enhanced: i32,
    pub laser_sight_during_nightvision: i32,
    pub kill_icon: Option<Arc<Material>>,
    pub kill_icon_ratio: i32,
    pub flip_kill_icon: i32,
    pub dpad_icon: Option<Arc<Material>>,
    pub dpad_icon_ratio: i32,
    pub no_partial_reload: i32,
    pub segmented_reload: i32,
    pub reload_ammo_add: i32,
    pub reload_start_add: i32,
    pub alt_weapon_name: Name,
    pub alt_weapon_index: u32,
    pub drop_ammo_min: i32,
    pub drop_ammo_max: i32,
    pub blocks_prone: i32,
    pub silenced: i32,
    pub explosion_radius: i32,
    pub explosion_radius_min: i32,
    pub explosion_inner_damage: i32,
    pub explosion_outer_damage: i32,
    pub damage_cone_angle: f32,
    pub projectile_speed: i32,
    pub projectile_speed_up: i32,
    pub projectile_speed_forward: i32,
    pub projectile_activate_dist: i32,
    pub proj_lifetime: f32,
    pub time_to_accelerate: f32,
    pub projectile_curvature: f32,
    pub projectile_model: Option<Arc<XModel>>,
    pub proj_explosion: i32,
    pub proj_explosion_effect: Option<Arc<FxEffectDef>>,
    pub proj_explosion_effect_force_normal_up: i32,
    pub proj_dud_effect: Option<Arc<FxEffectDef>>,
    pub proj_explosion_sound: Name,
    pub proj_dud_sound: Name,
    pub proj_impact_explode: i32,
    pub stickiness: i32,
    pub has_detonator: i32,
    pub timed_detonation: i32,
    pub rotate: i32,
    pub hold_button_to_throw: i32,
    pub freeze_movement_when_firing: i32,
    pub low_ammo_warning_threshold: f32,
    pub parallel_bounce: [f32; 29],
    pub perpendicular_bounce: [f32; 29],
    pub proj_trail_effect: Option<Arc<FxEffectDef>>,
    pub v_projectile_color: [f32; 3],
    pub guided_missile_type: i32,
    pub max_steering_accel: f32,
    pub proj_ignition_delay: i32,
    pub proj_ignition_effect: Option<Arc<FxEffectDef>>,
    pub proj_ignition_sound: Name,
    pub ads_aim_pitch: f32,
    pub ads_crosshair_in_frac: f32,
    pub ads_crosshair_out_frac: f32,
    pub ads_gun_kick_reduced_kick_bullets: i32,
    pub ads_gun_kick_reduced_kick_percent: f32,
    pub ads_gun_kick_pitch_min: f32,
    pub ads_gun_kick_pitch_max: f32,
    pub ads_gun_kick_yaw_min: f32,
    pub ads_gun_kick_yaw_max: f32,
    pub ads_gun_kick_accel: f32,
    pub ads_gun_kick_speed_max: f32,
    pub ads_gun_kick_speed_decay: f32,
    pub ads_gun_kick_static_decay: f32,
    pub ads_view_kick_pitch_min: f32,
    pub ads_view_kick_pitch_max: f32,
    pub ads_view_kick_yaw_min: f32,
    pub ads_view_kick_yaw_max: f32,
    pub ads_view_kick_center_speed: f32,
    pub ads_view_scatter_min: f32,
    pub ads_view_scatter_max: f32,
    pub ads_spread: f32,
    pub hip_gun_kick_reduced_kick_bullets: i32,
    pub hip_gun_kick_reduced_kick_percent: f32,
    pub hip_gun_kick_pitch_min: f32,
    pub hip_gun_kick_pitch_max: f32,
    pub hip_gun_kick_yaw_min: f32,
    pub hip_gun_kick_yaw_max: f32,
    pub hip_gun_kick_accel: f32,
    pub hip_gun_kick_speed_max: f32,
    pub hip_gun_kick_speed_decay: f32,
    pub hip_gun_kick_static_decay: f32,
    pub hip_view_kick_pitch_min: f32,
    pub hip_view_kick_pitch_max: f32,
    pub hip_view_kick_yaw_min: f32,
    pub hip_view_kick_yaw_max: f32,
    pub hip_view_kick_center_speed: f32,
    pub hip_view_scatter_min: f32,
    pub hip_view_scatter_max: f32,
    pub fight_dist: f32,
    pub max_dist: f32,
    /// Accuracy against AI.
    pub ai_vs_ai_accuracy: AccuracyGraph,
    /// Accuracy against the player.
    pub ai_vs_player_accuracy: AccuracyGraph,
    pub position_reload_trans_time: i32,
    pub left_arc: f32,
    pub right_arc: f32,
    pub top_arc: f32,
    pub bottom_arc: f32,
    pub accuracy: f32,
    pub ai_spread: f32,
    pub player_spread: f32,
    pub min_turn_speed: [f32; 2],
    pub max_turn_speed: [f32; 2],
    pub pitch_convergence_time: f32,
    pub yaw_convergence_time: f32,
    pub suppress_time: f32,
    pub max_range: f32,
    pub anim_hor_rotate_inc: f32,
    pub player_position_dist: f32,
    pub use_hint_string: Name,
    pub drop_hint_string: Name,
    pub use_hint_string_index: i32,
    pub drop_hint_string_index: i32,
    pub horiz_view_jitter: f32,
    pub vert_view_jitter: f32,
    pub script: Name,
    pub oo_pos_anim_length: [f32; 2],
    pub min_damage: i32,
    pub min_player_damage: i32,
    pub max_damage_range: f32,
    pub min_damage_range: f32,
    pub destabilization_rate_time: f32,
    pub destabilization_curvature_max: f32,
    pub destabilize_distance: i32,
    pub location_damage_multipliers: [f32; 19],
    pub fire_rumble: Name,
    pub melee_impact_rumble: Name,
    pub ads_dof_start: f32,
    pub ads_dof_end: f32,
}

fn name(s: &mut Stream, f: &mut Fields) -> Result<Name> {
    let p = f.ptr()?;
    s.string(p)
}

/// A sound alias reference: a pointer to a shared slot that holds the alias name.
fn custom_sound(s: &mut Stream, f: &mut Fields) -> Result<Name> {
    let p = f.ptr()?;
    let slot = s.shared(p, 4, 4, |s, b| name(s, &mut Fields::new(b)))?;
    Ok(slot.and_then(|n| (*n).clone()))
}

fn bounce_sounds(s: &mut Stream, p: Ptr) -> Result<Option<Arc<[Name]>>> {
    match p {
        Ptr::Null => Ok(None),
        p => s
            .array(p, BOUNCE_SOUNDS, 4, 4, |s, f| custom_sound(s, f))
            .map(Some),
    }
}

fn weapon(s: &mut Stream, h: &[u8]) -> Result<WeaponDef> {
    let mut f = Fields::new(h);
    let internal_name = name(s, &mut f)?;
    let display_name = name(s, &mut f)?;
    let overlay_name = name(s, &mut f)?;
    let mut gun_models = Vec::with_capacity(16);
    for _ in 0..16 {
        let p = f.ptr()?;
        gun_models.push(xmodel::load(s, p)?);
    }
    let p = f.ptr()?;
    let hand_model = xmodel::load(s, p)?;
    let mut anims = Vec::with_capacity(33);
    for _ in 0..33 {
        let p = f.ptr()?;
        anims.push(s.string(p)?);
    }
    let mode_name = name(s, &mut f)?;
    let hide_tags = [0; 8].map(|_| f.u16());
    let notetrack_sound_map_keys = [0; 16].map(|_| f.u16());
    let notetrack_sound_map_values = [0; 16].map(|_| f.u16());
    let player_anim_type = f.i32();
    let weap_type = f.i32();
    let weap_class = f.i32();
    let penetrate_type = f.i32();
    let impact_type = f.i32();
    let inventory_type = f.i32();
    let fire_type = f.i32();
    let offhand_class = f.i32();
    let stance = f.i32();
    let p = f.ptr()?;
    let view_flash_effect = fx::load(s, p)?;
    let p = f.ptr()?;
    let world_flash_effect = fx::load(s, p)?;
    let sounds = WeaponSounds::load(s, &mut f)?;
    let bounce_sound = bounce_sounds(s, f.ptr()?)?;
    let p = f.ptr()?;
    let view_shell_eject_effect = fx::load(s, p)?;
    let p = f.ptr()?;
    let world_shell_eject_effect = fx::load(s, p)?;
    let p = f.ptr()?;
    let view_last_shot_eject_effect = fx::load(s, p)?;
    let p = f.ptr()?;
    let world_last_shot_eject_effect = fx::load(s, p)?;
    let p = f.ptr()?;
    let reticle_center = gfx::material_ptr(s, p)?;
    let p = f.ptr()?;
    let reticle_side = gfx::material_ptr(s, p)?;
    let reticle_center_size = f.i32();
    let reticle_side_size = f.i32();
    let reticle_min_ofs = f.i32();
    let active_reticle_type = f.i32();
    let v_stand_move = [0; 3].map(|_| f.f32());
    let v_stand_rot = [0; 3].map(|_| f.f32());
    let v_ducked_ofs = [0; 3].map(|_| f.f32());
    let v_ducked_move = [0; 3].map(|_| f.f32());
    let v_ducked_rot = [0; 3].map(|_| f.f32());
    let v_prone_ofs = [0; 3].map(|_| f.f32());
    let v_prone_move = [0; 3].map(|_| f.f32());
    let v_prone_rot = [0; 3].map(|_| f.f32());
    let pos_move_rate = f.f32();
    let pos_prone_move_rate = f.f32();
    let stand_move_min_speed = f.f32();
    let ducked_move_min_speed = f.f32();
    let prone_move_min_speed = f.f32();
    let pos_rot_rate = f.f32();
    let pos_prone_rot_rate = f.f32();
    let stand_rot_min_speed = f.f32();
    let ducked_rot_min_speed = f.f32();
    let prone_rot_min_speed = f.f32();
    let mut world_models = Vec::with_capacity(16);
    for _ in 0..16 {
        let p = f.ptr()?;
        world_models.push(xmodel::load(s, p)?);
    }
    let p = f.ptr()?;
    let world_clip_model = xmodel::load(s, p)?;
    let p = f.ptr()?;
    let rocket_model = xmodel::load(s, p)?;
    let p = f.ptr()?;
    let knife_model = xmodel::load(s, p)?;
    let p = f.ptr()?;
    let world_knife_model = xmodel::load(s, p)?;
    let p = f.ptr()?;
    let hud_icon = gfx::material_ptr(s, p)?;
    let hud_icon_ratio = f.i32();
    let p = f.ptr()?;
    let ammo_counter_icon = gfx::material_ptr(s, p)?;
    let ammo_counter_icon_ratio = f.i32();
    let ammo_counter_clip = f.i32();
    let start_ammo = f.i32();
    let ammo_name = name(s, &mut f)?;
    let ammo_index = f.i32();
    let clip_name = name(s, &mut f)?;
    let clip_index = f.i32();
    let max_ammo = f.i32();
    let clip_size = f.i32();
    let shot_count = f.i32();
    let shared_ammo_cap_name = name(s, &mut f)?;
    let shared_ammo_cap_index = f.i32();
    let shared_ammo_cap = f.i32();
    let damage = f.i32();
    let player_damage = f.i32();
    let melee_damage = f.i32();
    let damage_type = f.i32();
    let fire_delay = f.i32();
    let melee_delay = f.i32();
    let melee_charge_delay = f.i32();
    let detonate_delay = f.i32();
    let fire_time = f.i32();
    let rechamber_time = f.i32();
    let rechamber_bolt_time = f.i32();
    let hold_fire_time = f.i32();
    let detonate_time = f.i32();
    let melee_time = f.i32();
    let melee_charge_time = f.i32();
    let reload_time = f.i32();
    let reload_show_rocket_time = f.i32();
    let reload_empty_time = f.i32();
    let reload_add_time = f.i32();
    let reload_start_time = f.i32();
    let reload_start_add_time = f.i32();
    let reload_end_time = f.i32();
    let drop_time = f.i32();
    let raise_time = f.i32();
    let alt_drop_time = f.i32();
    let alt_raise_time = f.i32();
    let quick_drop_time = f.i32();
    let quick_raise_time = f.i32();
    let first_raise_time = f.i32();
    let empty_raise_time = f.i32();
    let empty_drop_time = f.i32();
    let sprint_in_time = f.i32();
    let sprint_loop_time = f.i32();
    let sprint_out_time = f.i32();
    let night_vision_wear_time = f.i32();
    let night_vision_wear_time_fade_out_end = f.i32();
    let night_vision_wear_time_power_up = f.i32();
    let night_vision_remove_time = f.i32();
    let night_vision_remove_time_power_down = f.i32();
    let night_vision_remove_time_fade_in_start = f.i32();
    let fuse_time = f.i32();
    let ai_fuse_time = f.i32();
    let require_lockon_to_fire = f.i32();
    let no_ads_when_mag_empty = f.i32();
    let avoid_drop_cleanup = f.i32();
    let auto_aim_range = f.f32();
    let aim_assist_range = f.f32();
    let aim_assist_range_ads = f.f32();
    let aim_padding = f.f32();
    let enemy_crosshair_range = f.f32();
    let crosshair_color_change = f.i32();
    let move_speed_scale = f.f32();
    let ads_move_speed_scale = f.f32();
    let sprint_duration_scale = f.f32();
    let ads_zoom_fov = f.f32();
    let ads_zoom_in_frac = f.f32();
    let ads_zoom_out_frac = f.f32();
    let p = f.ptr()?;
    let overlay_material = gfx::material_ptr(s, p)?;
    let p = f.ptr()?;
    let overlay_material_low_res = gfx::material_ptr(s, p)?;
    let overlay_reticle = f.i32();
    let overlay_interface = f.i32();
    let overlay_width = f.f32();
    let overlay_height = f.f32();
    let ads_bob_factor = f.f32();
    let ads_view_bob_mult = f.f32();
    let hip_spread_stand_min = f.f32();
    let hip_spread_ducked_min = f.f32();
    let hip_spread_prone_min = f.f32();
    let hip_spread_stand_max = f.f32();
    let hip_spread_ducked_max = f.f32();
    let hip_spread_prone_max = f.f32();
    let hip_spread_decay_rate = f.f32();
    let hip_spread_fire_add = f.f32();
    let hip_spread_turn_add = f.f32();
    let hip_spread_move_add = f.f32();
    let hip_spread_ducked_decay = f.f32();
    let hip_spread_prone_decay = f.f32();
    let hip_reticle_side_pos = f.f32();
    let ads_trans_in_time = f.i32();
    let ads_trans_out_time = f.i32();
    let ads_idle_amount = f.f32();
    let hip_idle_amount = f.f32();
    let ads_idle_speed = f.f32();
    let hip_idle_speed = f.f32();
    let idle_crouch_factor = f.f32();
    let idle_prone_factor = f.f32();
    let gun_max_pitch = f.f32();
    let gun_max_yaw = f.f32();
    let sway_max_angle = f.f32();
    let sway_lerp_speed = f.f32();
    let sway_pitch_scale = f.f32();
    let sway_yaw_scale = f.f32();
    let sway_horiz_scale = f.f32();
    let sway_vert_scale = f.f32();
    let sway_shell_shock_scale = f.f32();
    let ads_sway_max_angle = f.f32();
    let ads_sway_lerp_speed = f.f32();
    let ads_sway_pitch_scale = f.f32();
    let ads_sway_yaw_scale = f.f32();
    let ads_sway_horiz_scale = f.f32();
    let ads_sway_vert_scale = f.f32();
    let rifle_bullet = f.i32();
    let armor_piercing = f.i32();
    let bolt_action = f.i32();
    let aim_down_sight = f.i32();
    let rechamber_while_ads = f.i32();
    let ads_view_error_min = f.f32();
    let ads_view_error_max = f.f32();
    let cook_off_hold = f.i32();
    let clip_only = f.i32();
    let ads_fire_only = f.i32();
    let cancel_auto_holster_when_empty = f.i32();
    let suppress_ammo_reserve_display = f.i32();
    let enhanced = f.i32();
    let laser_sight_during_nightvision = f.i32();
    let p = f.ptr()?;
    let kill_icon = gfx::material_ptr(s, p)?;
    let kill_icon_ratio = f.i32();
    let flip_kill_icon = f.i32();
    let p = f.ptr()?;
    let dpad_icon = gfx::material_ptr(s, p)?;
    let dpad_icon_ratio = f.i32();
    let no_partial_reload = f.i32();
    let segmented_reload = f.i32();
    let reload_ammo_add = f.i32();
    let reload_start_add = f.i32();
    let alt_weapon_name = name(s, &mut f)?;
    let alt_weapon_index = f.u32();
    let drop_ammo_min = f.i32();
    let drop_ammo_max = f.i32();
    let blocks_prone = f.i32();
    let silenced = f.i32();
    let explosion_radius = f.i32();
    let explosion_radius_min = f.i32();
    let explosion_inner_damage = f.i32();
    let explosion_outer_damage = f.i32();
    let damage_cone_angle = f.f32();
    let projectile_speed = f.i32();
    let projectile_speed_up = f.i32();
    let projectile_speed_forward = f.i32();
    let projectile_activate_dist = f.i32();
    let proj_lifetime = f.f32();
    let time_to_accelerate = f.f32();
    let projectile_curvature = f.f32();
    let p = f.ptr()?;
    let projectile_model = xmodel::load(s, p)?;
    let proj_explosion = f.i32();
    let p = f.ptr()?;
    let proj_explosion_effect = fx::load(s, p)?;
    let proj_explosion_effect_force_normal_up = f.i32();
    let p = f.ptr()?;
    let proj_dud_effect = fx::load(s, p)?;
    let proj_explosion_sound = custom_sound(s, &mut f)?;
    let proj_dud_sound = custom_sound(s, &mut f)?;
    let proj_impact_explode = f.i32();
    let stickiness = f.i32();
    let has_detonator = f.i32();
    let timed_detonation = f.i32();
    let rotate = f.i32();
    let hold_button_to_throw = f.i32();
    let freeze_movement_when_firing = f.i32();
    let low_ammo_warning_threshold = f.f32();
    let parallel_bounce = [0; 29].map(|_| f.f32());
    let perpendicular_bounce = [0; 29].map(|_| f.f32());
    let p = f.ptr()?;
    let proj_trail_effect = fx::load(s, p)?;
    let v_projectile_color = [0; 3].map(|_| f.f32());
    let guided_missile_type = f.i32();
    let max_steering_accel = f.f32();
    let proj_ignition_delay = f.i32();
    let p = f.ptr()?;
    let proj_ignition_effect = fx::load(s, p)?;
    let proj_ignition_sound = custom_sound(s, &mut f)?;
    let ads_aim_pitch = f.f32();
    let ads_crosshair_in_frac = f.f32();
    let ads_crosshair_out_frac = f.f32();
    let ads_gun_kick_reduced_kick_bullets = f.i32();
    let ads_gun_kick_reduced_kick_percent = f.f32();
    let ads_gun_kick_pitch_min = f.f32();
    let ads_gun_kick_pitch_max = f.f32();
    let ads_gun_kick_yaw_min = f.f32();
    let ads_gun_kick_yaw_max = f.f32();
    let ads_gun_kick_accel = f.f32();
    let ads_gun_kick_speed_max = f.f32();
    let ads_gun_kick_speed_decay = f.f32();
    let ads_gun_kick_static_decay = f.f32();
    let ads_view_kick_pitch_min = f.f32();
    let ads_view_kick_pitch_max = f.f32();
    let ads_view_kick_yaw_min = f.f32();
    let ads_view_kick_yaw_max = f.f32();
    let ads_view_kick_center_speed = f.f32();
    let ads_view_scatter_min = f.f32();
    let ads_view_scatter_max = f.f32();
    let ads_spread = f.f32();
    let hip_gun_kick_reduced_kick_bullets = f.i32();
    let hip_gun_kick_reduced_kick_percent = f.f32();
    let hip_gun_kick_pitch_min = f.f32();
    let hip_gun_kick_pitch_max = f.f32();
    let hip_gun_kick_yaw_min = f.f32();
    let hip_gun_kick_yaw_max = f.f32();
    let hip_gun_kick_accel = f.f32();
    let hip_gun_kick_speed_max = f.f32();
    let hip_gun_kick_speed_decay = f.f32();
    let hip_gun_kick_static_decay = f.f32();
    let hip_view_kick_pitch_min = f.f32();
    let hip_view_kick_pitch_max = f.f32();
    let hip_view_kick_yaw_min = f.f32();
    let hip_view_kick_yaw_max = f.f32();
    let hip_view_kick_center_speed = f.f32();
    let hip_view_scatter_min = f.f32();
    let hip_view_scatter_max = f.f32();
    let fight_dist = f.f32();
    let max_dist = f.f32();
    let (name0, name1) = (f.ptr()?, f.ptr()?);
    let (knots0, knots1) = (f.ptr()?, f.ptr()?);
    let (original0, original1) = (f.ptr()?, f.ptr()?);
    let counts = [f.i32(), f.i32(), f.i32(), f.i32()];
    let ai_vs_ai_accuracy =
        AccuracyGraph::load(s, [name0, knots0, original0], [counts[0], counts[2]])?;
    let ai_vs_player_accuracy =
        AccuracyGraph::load(s, [name1, knots1, original1], [counts[1], counts[3]])?;
    let position_reload_trans_time = f.i32();
    let left_arc = f.f32();
    let right_arc = f.f32();
    let top_arc = f.f32();
    let bottom_arc = f.f32();
    let accuracy = f.f32();
    let ai_spread = f.f32();
    let player_spread = f.f32();
    let min_turn_speed = [0; 2].map(|_| f.f32());
    let max_turn_speed = [0; 2].map(|_| f.f32());
    let pitch_convergence_time = f.f32();
    let yaw_convergence_time = f.f32();
    let suppress_time = f.f32();
    let max_range = f.f32();
    let anim_hor_rotate_inc = f.f32();
    let player_position_dist = f.f32();
    let use_hint_string = name(s, &mut f)?;
    let drop_hint_string = name(s, &mut f)?;
    let use_hint_string_index = f.i32();
    let drop_hint_string_index = f.i32();
    let horiz_view_jitter = f.f32();
    let vert_view_jitter = f.f32();
    let script = name(s, &mut f)?;
    let oo_pos_anim_length = [0; 2].map(|_| f.f32());
    let min_damage = f.i32();
    let min_player_damage = f.i32();
    let max_damage_range = f.f32();
    let min_damage_range = f.f32();
    let destabilization_rate_time = f.f32();
    let destabilization_curvature_max = f.f32();
    let destabilize_distance = f.i32();
    let location_damage_multipliers = [0; 19].map(|_| f.f32());
    let fire_rumble = name(s, &mut f)?;
    let melee_impact_rumble = name(s, &mut f)?;
    let ads_dof_start = f.f32();
    let ads_dof_end = f.f32();
    Ok(WeaponDef {
        internal_name,
        display_name,
        overlay_name,
        gun_models,
        hand_model,
        anims,
        mode_name,
        hide_tags,
        notetrack_sound_map_keys,
        notetrack_sound_map_values,
        player_anim_type,
        weap_type,
        weap_class,
        penetrate_type,
        impact_type,
        inventory_type,
        fire_type,
        offhand_class,
        stance,
        view_flash_effect,
        world_flash_effect,
        sounds,
        bounce_sound,
        view_shell_eject_effect,
        world_shell_eject_effect,
        view_last_shot_eject_effect,
        world_last_shot_eject_effect,
        reticle_center,
        reticle_side,
        reticle_center_size,
        reticle_side_size,
        reticle_min_ofs,
        active_reticle_type,
        v_stand_move,
        v_stand_rot,
        v_ducked_ofs,
        v_ducked_move,
        v_ducked_rot,
        v_prone_ofs,
        v_prone_move,
        v_prone_rot,
        pos_move_rate,
        pos_prone_move_rate,
        stand_move_min_speed,
        ducked_move_min_speed,
        prone_move_min_speed,
        pos_rot_rate,
        pos_prone_rot_rate,
        stand_rot_min_speed,
        ducked_rot_min_speed,
        prone_rot_min_speed,
        world_models,
        world_clip_model,
        rocket_model,
        knife_model,
        world_knife_model,
        hud_icon,
        hud_icon_ratio,
        ammo_counter_icon,
        ammo_counter_icon_ratio,
        ammo_counter_clip,
        start_ammo,
        ammo_name,
        ammo_index,
        clip_name,
        clip_index,
        max_ammo,
        clip_size,
        shot_count,
        shared_ammo_cap_name,
        shared_ammo_cap_index,
        shared_ammo_cap,
        damage,
        player_damage,
        melee_damage,
        damage_type,
        fire_delay,
        melee_delay,
        melee_charge_delay,
        detonate_delay,
        fire_time,
        rechamber_time,
        rechamber_bolt_time,
        hold_fire_time,
        detonate_time,
        melee_time,
        melee_charge_time,
        reload_time,
        reload_show_rocket_time,
        reload_empty_time,
        reload_add_time,
        reload_start_time,
        reload_start_add_time,
        reload_end_time,
        drop_time,
        raise_time,
        alt_drop_time,
        alt_raise_time,
        quick_drop_time,
        quick_raise_time,
        first_raise_time,
        empty_raise_time,
        empty_drop_time,
        sprint_in_time,
        sprint_loop_time,
        sprint_out_time,
        night_vision_wear_time,
        night_vision_wear_time_fade_out_end,
        night_vision_wear_time_power_up,
        night_vision_remove_time,
        night_vision_remove_time_power_down,
        night_vision_remove_time_fade_in_start,
        fuse_time,
        ai_fuse_time,
        require_lockon_to_fire,
        no_ads_when_mag_empty,
        avoid_drop_cleanup,
        auto_aim_range,
        aim_assist_range,
        aim_assist_range_ads,
        aim_padding,
        enemy_crosshair_range,
        crosshair_color_change,
        move_speed_scale,
        ads_move_speed_scale,
        sprint_duration_scale,
        ads_zoom_fov,
        ads_zoom_in_frac,
        ads_zoom_out_frac,
        overlay_material,
        overlay_material_low_res,
        overlay_reticle,
        overlay_interface,
        overlay_width,
        overlay_height,
        ads_bob_factor,
        ads_view_bob_mult,
        hip_spread_stand_min,
        hip_spread_ducked_min,
        hip_spread_prone_min,
        hip_spread_stand_max,
        hip_spread_ducked_max,
        hip_spread_prone_max,
        hip_spread_decay_rate,
        hip_spread_fire_add,
        hip_spread_turn_add,
        hip_spread_move_add,
        hip_spread_ducked_decay,
        hip_spread_prone_decay,
        hip_reticle_side_pos,
        ads_trans_in_time,
        ads_trans_out_time,
        ads_idle_amount,
        hip_idle_amount,
        ads_idle_speed,
        hip_idle_speed,
        idle_crouch_factor,
        idle_prone_factor,
        gun_max_pitch,
        gun_max_yaw,
        sway_max_angle,
        sway_lerp_speed,
        sway_pitch_scale,
        sway_yaw_scale,
        sway_horiz_scale,
        sway_vert_scale,
        sway_shell_shock_scale,
        ads_sway_max_angle,
        ads_sway_lerp_speed,
        ads_sway_pitch_scale,
        ads_sway_yaw_scale,
        ads_sway_horiz_scale,
        ads_sway_vert_scale,
        rifle_bullet,
        armor_piercing,
        bolt_action,
        aim_down_sight,
        rechamber_while_ads,
        ads_view_error_min,
        ads_view_error_max,
        cook_off_hold,
        clip_only,
        ads_fire_only,
        cancel_auto_holster_when_empty,
        suppress_ammo_reserve_display,
        enhanced,
        laser_sight_during_nightvision,
        kill_icon,
        kill_icon_ratio,
        flip_kill_icon,
        dpad_icon,
        dpad_icon_ratio,
        no_partial_reload,
        segmented_reload,
        reload_ammo_add,
        reload_start_add,
        alt_weapon_name,
        alt_weapon_index,
        drop_ammo_min,
        drop_ammo_max,
        blocks_prone,
        silenced,
        explosion_radius,
        explosion_radius_min,
        explosion_inner_damage,
        explosion_outer_damage,
        damage_cone_angle,
        projectile_speed,
        projectile_speed_up,
        projectile_speed_forward,
        projectile_activate_dist,
        proj_lifetime,
        time_to_accelerate,
        projectile_curvature,
        projectile_model,
        proj_explosion,
        proj_explosion_effect,
        proj_explosion_effect_force_normal_up,
        proj_dud_effect,
        proj_explosion_sound,
        proj_dud_sound,
        proj_impact_explode,
        stickiness,
        has_detonator,
        timed_detonation,
        rotate,
        hold_button_to_throw,
        freeze_movement_when_firing,
        low_ammo_warning_threshold,
        parallel_bounce,
        perpendicular_bounce,
        proj_trail_effect,
        v_projectile_color,
        guided_missile_type,
        max_steering_accel,
        proj_ignition_delay,
        proj_ignition_effect,
        proj_ignition_sound,
        ads_aim_pitch,
        ads_crosshair_in_frac,
        ads_crosshair_out_frac,
        ads_gun_kick_reduced_kick_bullets,
        ads_gun_kick_reduced_kick_percent,
        ads_gun_kick_pitch_min,
        ads_gun_kick_pitch_max,
        ads_gun_kick_yaw_min,
        ads_gun_kick_yaw_max,
        ads_gun_kick_accel,
        ads_gun_kick_speed_max,
        ads_gun_kick_speed_decay,
        ads_gun_kick_static_decay,
        ads_view_kick_pitch_min,
        ads_view_kick_pitch_max,
        ads_view_kick_yaw_min,
        ads_view_kick_yaw_max,
        ads_view_kick_center_speed,
        ads_view_scatter_min,
        ads_view_scatter_max,
        ads_spread,
        hip_gun_kick_reduced_kick_bullets,
        hip_gun_kick_reduced_kick_percent,
        hip_gun_kick_pitch_min,
        hip_gun_kick_pitch_max,
        hip_gun_kick_yaw_min,
        hip_gun_kick_yaw_max,
        hip_gun_kick_accel,
        hip_gun_kick_speed_max,
        hip_gun_kick_speed_decay,
        hip_gun_kick_static_decay,
        hip_view_kick_pitch_min,
        hip_view_kick_pitch_max,
        hip_view_kick_yaw_min,
        hip_view_kick_yaw_max,
        hip_view_kick_center_speed,
        hip_view_scatter_min,
        hip_view_scatter_max,
        fight_dist,
        max_dist,
        ai_vs_ai_accuracy,
        ai_vs_player_accuracy,
        position_reload_trans_time,
        left_arc,
        right_arc,
        top_arc,
        bottom_arc,
        accuracy,
        ai_spread,
        player_spread,
        min_turn_speed,
        max_turn_speed,
        pitch_convergence_time,
        yaw_convergence_time,
        suppress_time,
        max_range,
        anim_hor_rotate_inc,
        player_position_dist,
        use_hint_string,
        drop_hint_string,
        use_hint_string_index,
        drop_hint_string_index,
        horiz_view_jitter,
        vert_view_jitter,
        script,
        oo_pos_anim_length,
        min_damage,
        min_player_damage,
        max_damage_range,
        min_damage_range,
        destabilization_rate_time,
        destabilization_curvature_max,
        destabilize_distance,
        location_damage_multipliers,
        fire_rumble,
        melee_impact_rumble,
        ads_dof_start,
        ads_dof_end,
    })
}

pub(super) fn load(s: &mut Stream, p: Ptr) -> Result<Option<Arc<WeaponDef>>> {
    s.temp_asset(p, 4, WEAPON_SIZE, weapon)
}
