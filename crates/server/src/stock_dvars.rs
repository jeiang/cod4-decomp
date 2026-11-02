// SPDX-License-Identifier: GPL-3.0-only
//! The stock registration (`Dvar_Register*`: name, type and range, default, flags) of every dvar the server registers,
//! from KisakCOD MP 1.8 with the 1.7 binary's registration calls where they differ, held against
//! [`super::core_dvars`]: a dvar registered without a stock row here, or one whose row it does not match, fails.

use crate::cvar::{ARCHIVE, CHEAT, Cvars, INIT, Kind, LATCH, ROM, SERVERINFO, SYSTEMINFO};

const PROTOCOL: i32 = net::oob::PROTOCOL as i32;

/// The flags the server knows (`DVAR_TEMP`, `DVAR_SAVED` and `DVAR_USERINFO` on a server-only dvar change nothing).
const KNOWN: u32 = ARCHIVE | SERVERINFO | SYSTEMINFO | INIT | LATCH | ROM | CHEAT;

#[rustfmt::skip]
fn stock() -> Vec<(&'static str, Kind, &'static str, u32)> {
    vec![
        // universal/dvar.cpp, com_files.cpp, game_mp/g_main_mp.cpp (G_RegisterDvars) and server_mp/sv_init_mp.cpp
        // (SV_Init); "1.7:" marks a value read from the 1.7 binary's registration call where KisakCOD differs.
        ("dedicated", Kind::Int(0, 2), "2", LATCH), // g_main_mp.cpp:409 (an enum, kept as its index)
        ("developer", Kind::Int(0, 2), "0", 0),
        ("developer_script", Kind::Bool, "0", 0),
        ("sv_fps", Kind::Int(10, 1000), "20", 0),
        ("sv_maxclients", Kind::Int(1, 64), "32", ARCHIVE | SERVERINFO | LATCH),
        ("ui_maxclients", Kind::Int(1, 64), "32", ARCHIVE | SERVERINFO | LATCH),
        ("g_gametype", Kind::String, "war", SERVERINFO | LATCH),
        ("mapname", Kind::String, "", ROM | SERVERINFO),
        ("sv_mapname", Kind::String, "", ROM | SERVERINFO),
        ("sv_hostname", Kind::String, "CoD4Host", ARCHIVE | SERVERINFO),
        ("net_port", Kind::Int(0, 65535), "28960", LATCH),
        ("g_password", Kind::String, "", 0),
        ("g_speed", Kind::Int(i32::MIN, i32::MAX), "190", 0),
        ("g_mantleBlockTimeBuffer", Kind::Int(0, 60_000), "500", CHEAT), // g_main_mp.cpp:714
        ("g_antilag", Kind::Bool, "1", ARCHIVE | SERVERINFO), // g_main_mp.cpp:614
        ("g_smoothClients", Kind::Bool, "1", 0), // g_main_mp.cpp:613
        ("g_allowvote", Kind::Bool, "1", ARCHIVE), // ui_mp/ui_main_mp.cpp:4744
        ("g_deadChat", Kind::Bool, "0", ARCHIVE), // g_main_mp.cpp:501
        // sv_init_mp.cpp:704 registers a Bool 1; 1.7: Int 4, 0..INT_MAX, ARCHIVE | SERVERINFO.
        ("sv_floodProtect", Kind::Int(0, i32::MAX), "4", ARCHIVE | SERVERINFO),
        ("g_useholdtime", Kind::Int(0, i32::MAX), "0", 0),
        ("g_maxDroppedWeapons", Kind::Int(2, 32), "16", 0),
        ("g_dropForwardSpeed", Kind::Float(0.0, 1000.0), "10", ARCHIVE),
        ("g_dropUpSpeedBase", Kind::Float(0.0, 1000.0), "10", ARCHIVE),
        ("g_dropUpSpeedRand", Kind::Float(0.0, 1000.0), "5", ARCHIVE),
        ("g_dropHorzSpeedRand", Kind::Float(0.0, 1000.0), "100", ARCHIVE),
        ("pickupPrints", Kind::Bool, "0", CHEAT),
        ("player_throwbackInnerRadius", Kind::Float(0.0, f32::MAX), "90", CHEAT),
        ("player_throwbackOuterRadius", Kind::Float(0.0, f32::MAX), "160", CHEAT),
        ("player_MGUseRadius", Kind::Float(0.0, f32::MAX), "128", CHEAT),
        ("bg_maxGrenadeIndicatorSpeed", Kind::Float(0.0, 1000.0), "20", CHEAT), // bgame/bg_misc.cpp:717
        ("perk_grenadeDeath", Kind::String, "frag_grenade_short_mp", CHEAT), // bgame/bg_perks_mp.cpp:105
        ("g_useholdspawndelay", Kind::Int(0, 1000), "500", ARCHIVE | CHEAT), // g_main_mp.cpp:707
        ("g_gravity", Kind::Float(1.0, f32::MAX), "800", 0),
        ("g_knockback", Kind::Float(-f32::MAX, f32::MAX), "1000", 0),
        ("g_minGrenadeDamageSpeed", Kind::Float(0.0, f32::MAX), "400", CHEAT),
        ("g_inactivity", Kind::Int(0, i32::MAX), "0", 0),
        ("g_synchronousClients", Kind::Bool, "0", SYSTEMINFO),
        ("sv_cheats", Kind::Bool, "1", 0),
        ("sv_serverid", Kind::Int(i32::MIN, i32::MAX), "0", SYSTEMINFO | ROM),
        ("g_voteAbstainWeight", Kind::Float(0.0, 1.0), "0.5", ARCHIVE),
        ("g_logSync", Kind::Bool, "0", ARCHIVE),
        ("g_oldVoting", Kind::Bool, "1", ARCHIVE),
        ("fs_game", Kind::String, "", SERVERINFO | SYSTEMINFO | INIT), // sv_client_mp.cpp:75; 1.7: 0x1c
        ("g_playerCollisionEjectSpeed", Kind::Int(0, 32_000), "25", ARCHIVE),
        ("g_redCrosshairs", Kind::Bool, "1", LATCH | ARCHIVE),
        ("g_banIPs", Kind::String, "", ARCHIVE),
        ("g_motd", Kind::String, "", 0), // g_main_mp.cpp:454
        ("g_no_script_spam", Kind::Bool, "0", 0), // g_main_mp.cpp:624
        ("sv_paused", Kind::Int(0, 2), "0", ROM), // qcommon/common.cpp:1584
        ("sv_botsPressAttackBtn", Kind::Bool, "1", 0), // sv_init_mp.cpp:784
        ("g_friendlyfireDist", Kind::Float(0.0, 15000.0), "256", CHEAT),
        ("g_friendlyNameDist", Kind::Float(0.0, 15000.0), "15000", CHEAT),
        ("sv_iwds", Kind::String, "", SYSTEMINFO | ROM),
        ("sv_iwdNames", Kind::String, "", SYSTEMINFO | ROM),
        ("sv_showCommands", Kind::Bool, "0", 0),
        ("sv_padPackets", Kind::Int(0, i32::MAX), "0", 0),
        ("g_dumpAnims", Kind::Int(-1, 1023), "-1", CHEAT),
        ("sv_keywords", Kind::String, "", SERVERINFO),
        ("protocol", Kind::Int(PROTOCOL, PROTOCOL), "", ROM | SERVERINFO),
        ("shortversion", Kind::String, net::SHORT_VERSION, ROM | SERVERINFO),
        ("version", Kind::String, "", ROM),
        ("sv_privateClients", Kind::Int(0, 64), "0", SERVERINFO),
        ("sv_pure", Kind::Bool, "0", SERVERINFO | SYSTEMINFO),
        ("sv_voice", Kind::Bool, "0", ARCHIVE | SERVERINFO | SYSTEMINFO),
        ("voice_global", Kind::Bool, "0", ARCHIVE),
        ("voice_deadChat", Kind::Bool, "0", ARCHIVE),
        ("perk_parabolicRadius", Kind::Float(0.0, f32::MAX), "400", CHEAT),
        ("perk_parabolicAngle", Kind::Float(0.0, 180.0), "180", CHEAT),
        ("sv_allowDownload", Kind::Bool, "1", ARCHIVE), // sv_init_mp.cpp:720
        ("sv_wwwDownload", Kind::Bool, "0", ARCHIVE),
        ("sv_wwwBaseURL", Kind::String, "", ARCHIVE),
        ("sv_wwwDlDisconnected", Kind::Bool, "0", ARCHIVE),
        ("sv_punkbuster", Kind::Bool, "1", ARCHIVE | SERVERINFO | INIT), // sv_init_mp.cpp:699; 1.7: 0x15
        ("sv_allowAnonymous", Kind::Bool, "0", SERVERINFO),
        ("sv_disableClientConsole", Kind::Bool, "0", SERVERINFO),
        ("sv_minPing", Kind::Int(0, 999), "0", ARCHIVE | SERVERINFO),
        ("sv_maxPing", Kind::Int(0, 999), "0", ARCHIVE | SERVERINFO),
        ("sv_timeout", Kind::Int(0, 1800), "240", 0), // 1.7: 240, 0..1800
        ("sv_connectTimeout", Kind::Int(0, 1800), "45", 0), // 1.7: 45, 0..1800
        ("sv_reconnectlimit", Kind::Int(0, 1800), "3", ARCHIVE), // sv_init_mp.cpp:766; 1.7: 3, 0..1800
        ("sv_privatePassword", Kind::String, "", 0),
        ("sv_maxRate", Kind::Int(0, 25_000), "5000", ARCHIVE | SERVERINFO),
        ("rcon_password", Kind::String, "", 0),
        ("sv_kickBanTime", Kind::Float(0.0, 3600.0), "300", 0), // sv_init_mp.cpp:778
        ("sv_mapRotation", Kind::String, "", 0),
        ("sv_mapRotationCurrent", Kind::String, "", 0),
        ("nextmap", Kind::String, "", 0),
        ("gamename", Kind::String, "Call of Duty 4", SERVERINFO | ROM),
        ("g_log", Kind::String, "games_mp.log", ARCHIVE),
        ("loc_language", Kind::Int(0, 14), "0", ARCHIVE | LATCH),
        // The movement, weapon and missile dvars (bgame/bg_misc.cpp, bg_jump.cpp, bg_mantle.cpp, bg_perks_mp.cpp,
        // game/g_missile.cpp); 0x80 CHEAT, 0x40 ROM, 0x8 SYSTEMINFO.
        ("friction", Kind::Float(0.0, 100.0), "5.5", 0x80), // bgame/bg_misc.cpp:515
        ("stopspeed", Kind::Float(0.0, 1000.0), "100", 0x80), // bgame/bg_misc.cpp:601
        ("inertiaMax", Kind::Float(0.0, 1000.0), "50", 0x80), // bgame/bg_misc.cpp:503
        ("inertiaAngle", Kind::Float(-1.0, 1.0), "0", 0x80), // bgame/bg_misc.cpp:508
        ("jump_height", Kind::Float(0.0, 1000.0), "39", 0x80), // bgame/bg_jump.cpp:24
        ("jump_stepSize", Kind::Float(0.0, 64.0), "18", 0x80), // bgame/bg_jump.cpp:28
        ("jump_slowdownEnable", Kind::Bool, "1", 0x80), // bgame/bg_jump.cpp:33
        ("jump_ladderPushVel", Kind::Float(0.0, 1024.0), "128", 0x80), // bgame/bg_jump.cpp:37
        ("jump_spreadAdd", Kind::Float(0.0, 512.0), "64", 0x80), // bgame/bg_jump.cpp:45
        ("mantle_enable", Kind::Bool, "1", 0x80), // bgame/bg_mantle.cpp:69
        ("mantle_check_range", Kind::Float(0.0, 128.0), "20", 0x80), // bgame/bg_mantle.cpp:74
        ("mantle_check_radius", Kind::Float(0.0, 15.0), "0.1", 0x80), // bgame/bg_mantle.cpp:82
        ("mantle_check_angle", Kind::Float(0.0, 180.0), "60", 0x80), // bgame/bg_mantle.cpp:90
        ("mantle_view_yawcap", Kind::Float(0.0, 180.0), "60", 0x80), // bgame/bg_mantle.cpp:98
        ("bg_ladder_yawcap", Kind::Float(0.0, 360.0), "100", 0x80), // bgame/bg_misc.cpp:438
        ("bg_prone_yawcap", Kind::Float(0.0, 360.0), "85", 0x80), // bgame/bg_misc.cpp:446
        ("player_view_pitch_up", Kind::Float(0.0, 90.0), "85", 0x80), // bgame/bg_misc.cpp:357
        ("player_view_pitch_down", Kind::Float(0.0, 90.0), "85", 0x80), // bgame/bg_misc.cpp:365
        ("bg_fallDamageMinHeight", Kind::Float(1.0, f32::MAX), "128", 0x88), // bgame/bg_misc.cpp:488
        ("bg_fallDamageMaxHeight", Kind::Float(1.0, f32::MAX), "300", 0x88), // bgame/bg_misc.cpp:496
        ("bg_foliagesnd_minspeed", Kind::Float(0.0, f32::MAX), "40", 0x80), // bgame/bg_misc.cpp:454
        ("bg_foliagesnd_maxspeed", Kind::Float(0.0, f32::MAX), "180", 0x80), // bgame/bg_misc.cpp:462
        ("bg_foliagesnd_slowinterval", Kind::Int(0, i32::MAX), "1500", 0x80), // bgame/bg_misc.cpp:468
        ("bg_foliagesnd_fastinterval", Kind::Int(0, i32::MAX), "500", 0x80), // bgame/bg_misc.cpp:474
        ("bg_foliagesnd_resetinterval", Kind::Int(0, i32::MAX), "500", 0x80), // bgame/bg_misc.cpp:480
        ("player_strafeSpeedScale", Kind::Float(0.0, 20.0), "0.8", 0x80), // bgame/bg_misc.cpp:834
        ("player_backSpeedScale", Kind::Float(0.0, 20.0), "0.7", 0x80), // bgame/bg_misc.cpp:842
        ("player_spectateSpeedScale", Kind::Float(0.0, 20.0), "1", 0x80), // bgame/bg_misc.cpp:858
        ("player_moveThreshhold", Kind::Float(9.9999999e-09, 20.0), "10", 0xc0), // bgame/bg_misc.cpp:817
        ("player_footstepsThreshhold", Kind::Float(0.0, 50000.0), "0", 0xc0), // bgame/bg_misc.cpp:826
        ("player_sprintForwardMinimum", Kind::Int(0, 255), "105", 0x0), // bgame/bg_misc.cpp:864
        ("player_sprintSpeedScale", Kind::Float(0.0, 5.0), "1.5", 0x80), // bgame/bg_misc.cpp:872
        ("player_sprintTime", Kind::Float(0.0, 12.8), "4", 0x80), // bgame/bg_misc.cpp:880
        ("player_sprintMinTime", Kind::Float(0.0, 12.8), "1", 0x80), // bgame/bg_misc.cpp:888
        ("player_sprintRechargePause", Kind::Float(0.0, 9000.0), "0", 0x80), // bgame/bg_misc.cpp:896
        ("player_sprintStrafeSpeedScale", Kind::Float(0.0, 5000.0), "0.667", 0x80), // bgame/bg_misc.cpp:904
        ("player_sprintCameraBob", Kind::Float(0.0, 2.0), "0.5", 0x80), // bgame/bg_misc.cpp:912
        ("perk_sprintMultiplier", Kind::Float(0.0, f32::MAX), "2", 0x80), // bgame/bg_perks_mp.cpp:135
        ("player_dmgtimer_maxTime", Kind::Float(0.0, f32::MAX), "750", 0x80), // bgame/bg_misc.cpp:938
        ("player_dmgtimer_minScale", Kind::Float(0.0, 1.0), "0", 0x80), // bgame/bg_misc.cpp:946
        ("player_meleeChargeFriction", Kind::Float(1.0, 5000.0), "1200", 0x80), // bgame/bg_misc.cpp:1199
        ("player_adsExitDelay", Kind::Int(0, 1000), "0", 0x80), // bgame/bg_misc.cpp:799
        ("player_scopeExitOnDamage", Kind::Bool, "0", 0x80), // bgame/bg_misc.cpp:794
        ("bg_bobMax", Kind::Float(0.0, 36.0), "8", 0x80), // bgame/bg_misc.cpp:657
        ("bg_bobAmplitudeStanding", Kind::Vec2(0.0, 1.0), "0.007 0.007", 0x80), // bgame/bg_misc.cpp:631
        ("bg_bobAmplitudeDucked", Kind::Vec2(0.0, 1.0), "0.0075 0.0075", 0x80), // bgame/bg_misc.cpp:640
        ("bg_bobAmplitudeProne", Kind::Vec2(0.0, 1.0), "0.02 0.005", 0x80), // bgame/bg_misc.cpp:649
        ("bg_bobAmplitudeSprinting", Kind::Vec2(0.0, 1.0), "0.02 0.014", 0x80), // bgame/bg_misc.cpp:622
        ("bg_aimSpreadMoveSpeedThreshold", Kind::Float(0.0, 300.0), "11", 0x80), // bgame/bg_misc.cpp:661
        ("player_burstFireCooldown", Kind::Float(0.0, 60.0), "0.2", 0x80), // bgame/bg_misc.cpp:1207
        ("player_sustainAmmo", Kind::Bool, "0", 0x80), // bgame/bg_misc.cpp:813
        ("perk_weapSpreadMultiplier", Kind::Float(0.0, 1.0), "0.65", 0x80), // bgame/bg_perks_mp.cpp:69
        ("perk_weapReloadMultiplier", Kind::Float(0.0, 1.0), "0.5", 0x80), // bgame/bg_perks_mp.cpp:77
        ("perk_weapRateMultiplier", Kind::Float(0.0, 1.0), "0.75", 0x80), // bgame/bg_perks_mp.cpp:85
        ("perk_bulletPenetrationMultiplier", Kind::Float(0.0, 30.0), "2", 0x80), // bgame/bg_perks_mp.cpp:101
        ("player_meleeRange", Kind::Float(0.0, 1000.0), "64", 0x80), // bgame/bg_misc.cpp:1175
        ("player_meleeWidth", Kind::Float(0.0, 1000.0), "10", 0x80), // bgame/bg_misc.cpp:1183
        ("player_meleeHeight", Kind::Float(0.0, 1000.0), "10", 0x80), // bgame/bg_misc.cpp:1191
        ("bullet_penetrationEnabled", Kind::Bool, "1", 0x80), // game_mp/g_main_mp.cpp:442
        ("player_breath_hold_time", Kind::Float(0.0, 30.0), "4.5", 0x80), // bgame/bg_misc.cpp:732
        ("player_breath_gasp_time", Kind::Float(0.0, 30.0), "1", 0x80), // bgame/bg_misc.cpp:740
        ("player_breath_fire_delay", Kind::Float(0.0, 30.0), "0", 0x80), // bgame/bg_misc.cpp:748
        ("player_breath_gasp_scale", Kind::Float(0.0, 50.0), "4.5", 0x80), // bgame/bg_misc.cpp:756
        ("player_breath_hold_lerp", Kind::Float(0.0, 50.0), "1", 0x80), // bgame/bg_misc.cpp:764
        ("player_breath_gasp_lerp", Kind::Float(0.0, 50.0), "6", 0x80), // bgame/bg_misc.cpp:772
        ("perk_extraBreath", Kind::Float(0.0, f32::MAX), "5", 0x80), // bgame/bg_perks_mp.cpp:93
        ("missileJavClimbHeightDirect", Kind::Float(0.0, f32::MAX), "10000", 0x80), // game/g_missile.cpp:102
        ("missileJavClimbAngleDirect", Kind::Float(0.0, f32::MAX), "85", 0x80), // game/g_missile.cpp:118
        ("missileJavClimbCeilingDirect", Kind::Float(0.0, f32::MAX), "0", 0x80), // game/g_missile.cpp:136
        ("missileJavTurnRateDirect", Kind::Float(0.0, f32::MAX), "60", 0x80), // game/g_missile.cpp:152
        ("missileJavAccelClimb", Kind::Float(0.0, f32::MAX), "300", 0x80), // game/g_missile.cpp:168
        ("missileJavAccelDescend", Kind::Float(0.0, f32::MAX), "3000", 0x80), // game/g_missile.cpp:176
        ("missileJavSpeedLimitClimb", Kind::Float(0.0, f32::MAX), "1000", 0x80), // game/g_missile.cpp:184
        ("missileJavSpeedLimitDescend", Kind::Float(0.0, f32::MAX), "6000", 0x80), // game/g_missile.cpp:192
        ("missileJavTurnDecel", Kind::Float(0.0, 1.0), "0.05", 0x80), // game/g_missile.cpp:199
        ("missileJavClimbToOwner", Kind::Float(0.0, f32::MAX), "700", 0x80), // game/g_missile.cpp:202
        ("missileWaterMaxDepth", Kind::Float(0.0, f32::MAX), "60", 0x80), // game/g_missile.cpp:206
    ]
}

/// Registered here but not by stock: the engine's own switches (log filter, masters, WebTransport, bots, per-address
/// client limit, ban file).
const NOT_STOCK: &[&str] = &[
    "log_filter",
    "sv_public",
    "sv_master1",
    "sv_master2",
    "sv_master3",
    "sv_master4",
    "sv_master5",
    "net_wt",
    "net_wt_cert",
    "net_wt_key",
    "net_wt_info",
    "bot_idle",
    "bot_count",
    "sv_maxClientsPerIP",
    "sv_banFile",
];

/// Where this server differs from stock on purpose (each one's reason is at its registration in `core_dvars`): the
/// dvar and what differs.
const DELIBERATE: &[(&str, &str)] = &[
    ("sv_fps", "default"),
    ("sv_cheats", "flags"),
    ("sv_punkbuster", "default"),
    ("sv_maxRate", "default"),
    ("nextmap", "default"),
    ("g_log", "default"),
];

#[test]
fn core_dvars_are_registered_as_stock() {
    let stock = stock();
    let mut wrong = Vec::new();
    for (name, kind, default, flags) in super::core_dvars() {
        if NOT_STOCK.contains(&name) {
            assert!(
                !stock.iter().any(|s| s.0 == name),
                "{name} is both stock and not stock"
            );
            continue;
        }
        let Some(&(_, s_kind, s_default, s_flags)) = stock.iter().find(|s| s.0 == name) else {
            wrong.push(format!("{name}: no stock registration listed"));
            continue;
        };
        let deliberate = |what: &str| DELIBERATE.contains(&(name, what));
        if kind != s_kind {
            wrong.push(format!("{name}: type {kind:?}, stock {s_kind:?}"));
        }
        let s_default = s_kind
            .read(s_default)
            .unwrap_or_else(|| s_default.to_owned());
        let default = kind.read(&default).unwrap_or(default);
        if default != s_default && !deliberate("default") {
            wrong.push(format!("{name}: default {default:?}, stock {s_default:?}"));
        }
        if flags != s_flags & KNOWN && !deliberate("flags") {
            wrong.push(format!(
                "{name}: flags {flags:#x}, stock {:#x}",
                s_flags & KNOWN
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_server_info_carries_only_stock_serverinfo_dvars() {
    let mut c = Cvars::new();
    super::register_core_dvars(&mut c);
    let stock = stock();
    let info = c.info_string(SERVERINFO);
    let keys: Vec<_> = info.split('\\').skip(1).step_by(2).collect();
    for key in ["fs_game", "sv_floodProtect"] {
        assert!(keys.contains(&key), "{key} missing from {info}");
    }
    for key in &keys {
        assert!(
            stock.iter().any(|s| s.0 == *key && s.3 & SERVERINFO != 0),
            "{key} is in the server info, stock does not publish it"
        );
    }
}

#[test]
fn the_console_holds_values_to_the_stock_types_ranges_and_cheat_protection() {
    let mut c = Cvars::new();
    super::register_core_dvars(&mut c);
    assert_eq!(c.set_external("sv_reconnectlimit", "60"), Ok(()));
    assert_eq!(c.string("sv_reconnectlimit"), "60");
    assert_eq!(c.set_external("sv_voice", "7"), Ok(()));
    assert_eq!(c.string("sv_voice"), "1");
    c.force("sv_cheats", "0");
    for name in [
        "g_mantleBlockTimeBuffer",
        "perk_grenadeDeath",
        "g_useholdspawndelay",
    ] {
        assert_eq!(
            c.set_external(name, "0").map_err(|e| e.to_string()),
            Err(format!("{name} is cheat protected."))
        );
    }
    assert_eq!(
        c.set_external("fs_game", "mods/x")
            .map_err(|e| e.to_string()),
        Err("fs_game is write protected.".to_owned())
    );
}
