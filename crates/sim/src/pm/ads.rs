// SPDX-License-Identifier: GPL-3.0-only
//! Aim-down-sights state as far as movement depends on it: the `SIGHT_AIMING` flag (which slows
//! walking and blocks sprint), the blend fraction, and the walking flag derived from it.

use super::state::{PlayerState, PmType, button, ef, ev, pmf, weapon_state as ws};
use super::{Pml, Pmove};

/// `PM_ExitAimDownSight`.
pub(super) fn exit_ads(ps: &mut PlayerState) {
    ps.add_event(ev::RESET_ADS, 0);
    ps.pm_flags &= !pmf::SIGHT_AIMING;
}

/// `BG_UsingSniperScope`.
pub(super) fn using_sniper_scope(pm: &Pmove<'_>) -> bool {
    pm.weapon.overlay_reticle && pm.ps.weapon_pos_frac > 0.0
}

/// `PM_IsAdsAllowed`.
fn ads_allowed(pm: &Pmove<'_>, pml: &Pml) -> bool {
    let ps = &pm.ps;
    match ps.pm_type {
        PmType::NormalLinked if pml.almost_ground_plane => return false,
        PmType::Noclip
        | PmType::Ufo
        | PmType::Spectator
        | PmType::Intermission
        | PmType::Dead
        | PmType::DeadLinked => return false,
        _ => {}
    }
    if !pm.weapon.is_player || !pm.weapon.aim_down_sight {
        return false;
    }
    let state = ps.weapon_state;
    if (ws::OFFHAND_INIT..=ws::OFFHAND_END).contains(&state)
        || matches!(
            state,
            ws::MELEE_INIT
                | ws::MELEE_FIRE
                | ws::MELEE_END
                | ws::RAISING
                | ws::RAISING_ALTSWITCH
                | ws::DROPPING
                | ws::DROPPING_QUICK
                | ws::NIGHTVISION_WEAR
                | ws::NIGHTVISION_REMOVE
        )
    {
        return false;
    }
    ps.e_flags & ef::TURRET_ACTIVE == 0
        && ps.weapon_flags & 0x20 == 0
        && (!pm.weapon.no_ads_when_mag_empty || pm.weapon.ammo_in_clip != 0)
}

/// `PM_UpdateAimDownSightFlag`.
pub(super) fn update_flag(pm: &mut Pmove<'_>, pml: &Pml) {
    pm.ps.pm_flags &= !pmf::SIGHT_AIMING;
    let mut allowed = ads_allowed(pm, pml);
    let requested = pm.cmd.buttons & button::ADS != 0;
    if pm.cmd.buttons & button::SPRINT != 0
        && (!pm.weapon.overlay_reticle || pm.cmd.buttons & button::BREATH == 0)
    {
        exit_ads(&mut pm.ps);
        allowed = false;
    }
    if requested && allowed {
        if pm.ps.pm_flags & pmf::PRONE == 0 || using_sniper_scope(pm) {
            pm.ps.pm_flags |= pmf::SIGHT_AIMING;
        } else if pm.oldcmd.buttons & button::ADS == 0
            || pm.cmd.forwardmove == 0 && pm.cmd.rightmove == 0
        {
            pm.ps.pm_flags |= pmf::SIGHT_AIMING | pmf::PRONEMOVE_OVERRIDDEN;
        }
    }
}

/// `PM_UpdatePlayerWalkingFlag`: aiming down sights on foot is walking pace.
pub(super) fn update_walking_flag(pm: &mut Pmove<'_>) {
    let ps = &mut pm.ps;
    ps.pm_flags &= !pmf::WALKING;
    if ps.pm_type < PmType::Dead
        && pm.cmd.buttons & button::ADS != 0
        && ps.pm_flags & pmf::PRONE == 0
        && ps.pm_flags & pmf::SIGHT_AIMING != 0
        && !matches!(
            ps.weapon_state,
            ws::RELOADING
                | ws::RELOAD_START
                | ws::RELOAD_END
                | ws::RELOAD_START_INTERUPT
                | ws::RELOADING_INTERUPT
        )
    {
        ps.pm_flags |= pmf::WALKING;
    }
}

/// `PM_UpdateAimDownSightLerp`: blends `weapon_pos_frac` toward the requested sight state.
pub(super) fn update_lerp(pm: &mut Pmove<'_>, pml: &Pml) {
    let w = pm.weapon;
    let params = pm.params;
    let time = pm.cmd.server_time;
    let ps = &mut pm.ps;
    if params.player_scope_exit_on_damage && ps.damage_count != 0 && w.overlay_reticle {
        exit_ads(ps);
        ps.weapon_pos_frac = 0.0;
        ps.ads_delay_time = 0;
        return;
    }
    if !w.aim_down_sight || ps.e_flags & ef::TURRET_ACTIVE != 0 {
        ps.weapon_pos_frac = 0.0;
        ps.ads_delay_time = 0;
        return;
    }
    let state = ps.weapon_state;
    let reloading_blocks = if !w.segmented_reload {
        state == ws::RELOADING && ps.weapon_time - w.position_reload_trans_time > 0
    } else {
        matches!(
            state,
            ws::RELOADING | ws::RELOADING_INTERUPT | ws::RELOAD_START | ws::RELOAD_START_INTERUPT
        ) || state == ws::RELOAD_END && ps.weapon_time - w.position_reload_trans_time > 0
    };
    let mut requested = false;
    if reloading_blocks || !w.rechamber_while_ads && state == ws::RECHAMBERING {
        requested = false;
    } else if ps.pm_flags & pmf::SIGHT_AIMING != 0 {
        requested = true;
    }
    if w.ads_fire_only && ps.weapon_delay != 0 && state == ws::FIRING {
        requested = true;
    }
    if ps.weapon_pos_frac != 1.0 || requested || params.player_ads_exit_delay <= 0 {
        ps.ads_delay_time = 0;
    } else {
        if ps.ads_delay_time == 0 {
            ps.ads_delay_time = params.player_ads_exit_delay + time;
        }
        if ps.ads_delay_time <= time {
            ps.ads_delay_time = 0;
        } else {
            requested = true;
        }
    }
    if requested && ps.weapon_pos_frac != 1.0 || !requested && ps.weapon_pos_frac != 0.0 {
        let rate = if requested {
            w.pos_blend_in_rate
        } else {
            -w.pos_blend_out_rate
        };
        let frac = ps.weapon_pos_frac + pml.msec as f32 * rate;
        ps.weapon_pos_frac = frac.clamp(0.0, 1.0);
    }
}

/// `PM_InteruptWeaponWithProneMove`: whether a prone move may cancel the weapon's current state,
/// idling it when so.
pub(super) fn interrupt_weapon_with_prone_move(ps: &mut PlayerState) -> bool {
    let s = ps.weapon_state;
    if s <= ws::DROPPING_QUICK
        || matches!(
            s,
            ws::RELOADING
                | ws::RELOAD_START
                | ws::RELOAD_END
                | ws::RELOAD_START_INTERUPT
                | ws::RELOADING_INTERUPT
                | ws::RECHAMBERING
        )
    {
        return true;
    }
    if s == ws::FIRING
        || matches!(s, ws::MELEE_INIT | ws::MELEE_FIRE | ws::MELEE_END)
        || (ws::OFFHAND_INIT..=ws::OFFHAND_END).contains(&s)
        || matches!(s, ws::NIGHTVISION_WEAR | ws::NIGHTVISION_REMOVE)
    {
        return false;
    }
    // `PM_Weapon_Idle`.
    ps.weapon_flags &= !2;
    ps.pm_flags &= !pmf::PRONEMOVE_OVERRIDDEN;
    ps.weapon_time = 0;
    ps.weapon_delay = 0;
    ps.weapon_state = ws::READY;
    true
}
