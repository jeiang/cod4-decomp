// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (cgame_mp/cg_draw_mp.cpp CG_DrawFriendlyNames, CG_DrawCrosshairNames, CG_DrawOverheadNames, CG_FadeCrosshairNameAlpha; cgame_mp/cg_players_mp.cpp CG_AddPlayerSpriteDrawSurfs; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The names and pictures over other players' heads: teammates' names (with their rank), the name of the player the
//! crosshair is on, and the head icons the scripts set (`headicon`: the bomb carrier, flag carriers, objective
//! markers) plus the connection-interrupted, speaking and killcam "you" markers.
//!
//! [`Names::update`] runs once a frame on the facts [`crate::hud::NameScan`] holds: it keeps the fade timers and
//! settles what to draw. [`draw`] projects that onto the screen.

use super::elems::{Screen, project};
use super::scores::rank_cell;
use super::{LiveUi, NearPlayer};
use crate::input::Cvars;
use crate::ui::Ui;
use crate::ui::paint::{Painter, TextDraw};
use crate::ui::place::{Px, horz, vert};
use std::collections::HashMap;

/// Materials of the status markers (`cgMedia`).
const KILLCAM_YOU: &str = "headiconyouinkillcam";
const DISCONNECTED: &str = "headicondisconnected";
const TALKING: &str = "headicontalkballoon";
/// The world height above the `j_head` bone of a name and of an icon, and what a second marker stacks above an icon.
const NAME_HEIGHT: f32 = 10.0;
const ICON_HEIGHT: f32 = 21.0;
const ICON_STACK: f32 = 16.0;
/// Radius, in world units, a head icon has before `cg_scriptIconSize` and friends add to it.
const ICON_RADIUS: f32 = 10.0;
/// Radius of a head icon that keeps its size on screen, per unit of its size, as a fraction of half the screen height.
const CONSTANT_ICON_SCALE: f32 = 0.0043;
/// A name is not shown below this opacity.
const EPSILON: f32 = 0.0001;
/// "No player" for the crosshair's memory.
const NO_CLIENT: u16 = u16::MAX;

/// The dvars that shape the names and icons, with the stock engine's defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Cfg {
    pub draw_friendly: bool,
    pub draw_crosshair: bool,
    pub friendly_fade_in: i32,
    pub friendly_fade_out: i32,
    pub enemy_fade_in: i32,
    pub enemy_fade_out: i32,
    pub max_dist: f32,
    pub near_dist: f32,
    pub far_dist: f32,
    pub far_scale: f32,
    pub size: f32,
    pub icon_size: f32,
    pub rank_size: f32,
    pub font: i32,
    pub through_walls: bool,
    pub icon_min_radius: f32,
    pub script_icon_size: f32,
    pub constant_size_icons: bool,
    pub killcam_icon_size: f32,
    pub connection_icon_size: f32,
    pub voice_icon_size: f32,
}

impl Cfg {
    pub fn read(cvars: &Cvars) -> Self {
        let f = |name: &str, default: f32| {
            cvars
                .get(name)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(default)
        };
        let i = |name: &str, default: i32| f(name, default as f32) as i32;
        let b = |name: &str, default: bool| f(name, f32::from(u8::from(default))) != 0.0;
        Self {
            draw_friendly: b("cg_drawFriendlyNames", true),
            draw_crosshair: b("cg_drawCrosshairNames", true),
            friendly_fade_in: i("cg_friendlyNameFadeIn", 0),
            friendly_fade_out: i("cg_friendlyNameFadeOut", 1500),
            enemy_fade_in: i("cg_enemyNameFadeIn", 250),
            enemy_fade_out: i("cg_enemyNameFadeOut", 250),
            max_dist: f("cg_overheadNamesMaxDist", 10000.0),
            near_dist: f("cg_overheadNamesNearDist", 256.0),
            far_dist: f("cg_overheadNamesFarDist", 1024.0),
            far_scale: f("cg_overheadNamesFarScale", 0.6),
            size: f("cg_overheadNamesSize", 0.5),
            icon_size: f("cg_overheadIconSize", 0.7),
            rank_size: f("cg_overheadRankSize", 0.5),
            font: i("cg_overheadNamesFont", 2),
            through_walls: b("cg_drawThroughWalls", false),
            icon_min_radius: f("cg_headIconMinScreenRadius", 0.02),
            script_icon_size: f("cg_scriptIconSize", 0.0),
            constant_size_icons: b("cg_constantSizeHeadIcons", false),
            killcam_icon_size: f("cg_youInKillCamSize", 6.0),
            connection_icon_size: f("cg_connectionIconSize", 0.0),
            voice_icon_size: f("cg_voiceIconSize", 0.0),
        }
    }
}

/// A name to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct NameDraw {
    pub client: u16,
    pub name: String,
    pub rank: u8,
    pub prestige: u8,
    /// Over the head, in the world.
    pub pos: [f32; 3],
    /// Team colour with the fade in its alpha.
    pub color: [f32; 4],
}

/// A head icon to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct IconDraw {
    pub material: String,
    pub pos: [f32; 3],
    /// World units added to the icon's radius, or its size on screen when `constant`.
    pub size: f32,
    pub constant: bool,
}

/// When a name was first seen and last seen.
#[derive(Clone, Copy, Debug, Default)]
struct Fade {
    visible: bool,
    start: i32,
    last: i32,
}

/// The player the crosshair last settled on.
#[derive(Clone, Copy, Debug)]
struct Cross {
    client: u16,
    start: i32,
    last: i32,
}

/// Fade timers between frames, and what the last [`Names::update`] settled to draw.
pub struct Names {
    overhead: HashMap<u16, Fade>,
    cross: Cross,
    pub names: Vec<NameDraw>,
    pub icons: Vec<IconDraw>,
    /// The crosshair's player is among `names` this frame.
    pub crosshair_drawn: bool,
}

impl Default for Names {
    fn default() -> Self {
        Self {
            overhead: HashMap::new(),
            cross: Cross {
                client: NO_CLIENT,
                start: 0,
                last: 0,
            },
            names: Vec::new(),
            icons: Vec::new(),
            crosshair_drawn: false,
        }
    }
}

/// `CG_FadeCrosshairNameAlpha`: nothing until a name has been in sight for `fade_in`, then full while it is, and
/// fading over `fade_out` once it is not.
pub fn fade_alpha(time: i32, start: i32, last: i32, fade_in: i32, fade_out: i32) -> f32 {
    let since = time - last;
    if since >= fade_out || last - start < fade_in {
        0.0
    } else {
        (fade_out - since) as f32 / fade_out as f32
    }
}

/// The scale a name has at `dist` from the eye: full size up to `near`, shrinking to `far_scale` at `far`.
pub fn distance_scale(dist: f32, cfg: &Cfg) -> f32 {
    if dist < cfg.near_dist {
        1.0
    } else if dist <= cfg.far_dist {
        let frac = (dist - cfg.near_dist) / (cfg.far_dist - cfg.near_dist);
        frac * cfg.far_scale + 1.0 - frac
    } else {
        cfg.far_scale
    }
}

/// Can the viewer see `p`'s name (`CG_CanSeeFriendlyHead`, less the look through smoke).
fn sees_head(p: &NearPlayer, eye: [f32; 3], cfg: &Cfg) -> bool {
    if cfg.through_walls {
        return true;
    }
    let d = [p.head[0] - eye[0], p.head[1] - eye[1], p.head[2] - eye[2]];
    p.clear && d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= cfg.max_dist * cfg.max_dist
}

impl Names {
    /// Settles this frame's names and icons. `colors` are the viewer's-team and other-team name colours.
    pub fn update(&mut self, live: &LiveUi, cfg: &Cfg, colors: [[f32; 4]; 2]) {
        self.names.clear();
        self.icons.clear();
        self.crosshair_drawn = false;
        if !live.active {
            self.overhead.clear();
            return;
        }
        let (now, mine) = (live.time, live.own_team);
        // A spectator sees every name; a player sees their own side's; with no sides there are none.
        let friendly = |team: u8| mine == 3 || (mine != 0 && team == mine);
        let color = |team: u8| colors[usize::from(team != mine)];
        let put = |names: &mut Vec<NameDraw>, p: &NearPlayer, alpha: f32| {
            let name = live.name(p.client);
            if alpha <= EPSILON || name.is_empty() {
                return;
            }
            let (rank, prestige) = live
                .ranks
                .get(usize::from(p.client))
                .copied()
                .unwrap_or_default();
            let mut c = color(live.team(p.client));
            c[3] = alpha;
            let draw = NameDraw {
                client: p.client,
                name: name.to_owned(),
                rank,
                prestige,
                pos: [p.head[0], p.head[1], p.head[2] + NAME_HEIGHT],
                color: c,
            };
            match names.iter_mut().find(|n| n.client == p.client) {
                Some(n) => n.color[3] = n.color[3].max(alpha),
                None => names.push(draw),
            }
        };
        if cfg.draw_friendly {
            for p in &live.scan.near {
                if !friendly(live.team(p.client)) {
                    self.overhead.remove(&p.client);
                    continue;
                }
                let f = self.overhead.entry(p.client).or_default();
                if !live.scan.flashed && sees_head(p, live.eye, cfg) {
                    if !f.visible {
                        f.visible = true;
                        f.start = now;
                    }
                } else {
                    f.visible = false;
                }
                if f.visible {
                    f.last = now;
                }
                let alpha = fade_alpha(
                    now,
                    f.start,
                    f.last,
                    cfg.friendly_fade_in,
                    cfg.friendly_fade_out,
                );
                put(&mut self.names, p, alpha);
            }
        }
        // The crosshair's player: its timers run whether or not the name is drawn, as in the original.
        if let Some(hit) = live.scan.crosshair {
            let fade_out = if friendly(live.team(hit)) {
                cfg.friendly_fade_out
            } else {
                cfg.enemy_fade_out
            };
            if self.cross.client != hit || now - self.cross.last > fade_out {
                self.cross = Cross {
                    client: hit,
                    start: now,
                    last: now,
                };
            }
            self.cross.last = now;
        }
        if cfg.draw_crosshair
            && let Some(p) = live
                .scan
                .near
                .iter()
                .find(|p| p.client == self.cross.client)
        {
            let (fade_in, fade_out) = if friendly(live.team(p.client)) {
                (cfg.friendly_fade_in, cfg.friendly_fade_out)
            } else {
                (cfg.enemy_fade_in, cfg.enemy_fade_out)
            };
            let alpha = fade_alpha(now, self.cross.start, self.cross.last, fade_in, fade_out);
            put(&mut self.names, p, alpha);
            self.crosshair_drawn = self.names.iter().any(|n| n.client == p.client);
        }
        for p in &live.scan.near {
            self.icons_of(p, live, cfg);
        }
    }

    /// The head icon the scripts gave `p`, and the one status marker above it (`CG_AddPlayerSpriteDrawSurfs`).
    fn icons_of(&mut self, p: &NearPlayer, live: &LiveUi, cfg: &Cfg) {
        let mine = live.own_team;
        let mut above = 0.0;
        let add =
            |icons: &mut Vec<IconDraw>, material: &str, size: f32, stack: f32, constant: bool| {
                icons.push(IconDraw {
                    material: material.to_owned(),
                    pos: [p.head[0], p.head[1], p.head[2] + stack + ICON_HEIGHT],
                    size,
                    constant,
                });
            };
        if let Some((material, team)) = &p.icon
            && (*team == 0 || mine == 3 || *team == mine)
        {
            add(
                &mut self.icons,
                material,
                cfg.script_icon_size,
                0.0,
                cfg.constant_size_icons,
            );
            above = cfg.script_icon_size + ICON_STACK;
        }
        if p.you {
            add(
                &mut self.icons,
                KILLCAM_YOU,
                cfg.killcam_icon_size,
                above,
                true,
            );
        } else if p.interrupted {
            add(
                &mut self.icons,
                DISCONNECTED,
                cfg.connection_icon_size,
                above,
                false,
            );
        } else if p.talking && (mine == 3 || live.team(p.client) == mine) {
            add(
                &mut self.icons,
                TALKING,
                cfg.voice_icon_size,
                above - 5.0,
                false,
            );
        }
    }
}

/// The radius in pixels of a head icon of `size` at `depth` in front of the eye, on a screen `height` pixels high
/// seen with the vertical focal scale `focal` (half the height per unit of tangent).
pub fn icon_radius_px(icon: &IconDraw, depth: f32, focal: f32, height: f32, cfg: &Cfg) -> f32 {
    let r = icon.size + ICON_RADIUS;
    let px = if icon.constant {
        r * CONSTANT_ICON_SCALE * height * 0.5
    } else {
        r / depth.max(1.0) * focal * height * 0.5
    };
    px.max(cfg.icon_min_radius * height * 0.5)
}

/// Draws the frame's names and icons over the world.
pub fn draw(ui: &Ui, p: &mut Painter, live: &LiveUi, names: &Names, cfg: &Cfg) {
    let Some(clip) = live.clip.as_ref().filter(|_| live.active) else {
        return;
    };
    let size = ui.place.size;
    let eye = glam::Vec3::from(live.eye);
    // Vertical focal scale of the view: the length of the clip matrix's y row.
    let focal = glam::Vec3::new(clip.x_axis.y, clip.y_axis.y, clip.z_axis.y).length();
    for icon in &names.icons {
        let Screen::Front(x, y) = project(clip, icon.pos, size) else {
            continue;
        };
        let depth = (clip.x_axis.w * icon.pos[0]
            + clip.y_axis.w * icon.pos[1]
            + clip.z_axis.w * icon.pos[2]
            + clip.w_axis.w)
            .abs();
        let r = icon_radius_px(icon, depth, focal, size.1, cfg);
        let img = p.named(&ui.assets, &icon.material);
        p.pic(
            &img,
            Px {
                x: x - r,
                y: y - r,
                w: r * 2.0,
                h: r * 2.0,
            },
            [1.0; 4],
        );
    }
    for n in &names.names {
        let Screen::Front(x, y) = project(clip, n.pos, size) else {
            continue;
        };
        let dist = (glam::Vec3::from(n.pos) - eye).length();
        let scale = distance_scale(dist, cfg);
        let text_size = cfg.size * scale;
        // Sizes are in pixels of the physical screen, like the original's `CL_DrawTextPhysical`.
        let unit = ui.place.scale.0;
        let width = ui.text_width(&n.name, cfg.font, text_size);
        let (x, y) = ((x - width * 0.5).round(), y.round());
        ui.draw_text(
            p,
            &TextDraw {
                text: &n.name,
                font_enum: cfg.font,
                scale: text_size / unit,
                style: 3,
                color: n.color,
                x,
                y,
                horz: horz::NOSCALE,
                vert: vert::NOSCALE,
            },
        );
        let Some((icon, level)) = rank_cell(ui, n.rank, n.prestige) else {
            continue;
        };
        let text_h = ui.text_height(cfg.font, text_size);
        let icon_size = cfg.icon_size * text_h;
        let rank_scale = cfg.rank_size * scale;
        let level_w = ui.text_width(&level, cfg.font, rank_scale);
        let rx = x - (level_w + scale * 2.0 + icon_size);
        let img = p.named(&ui.assets, &icon);
        let white = [1.0, 1.0, 1.0, n.color[3]];
        p.pic(
            &img,
            Px {
                x: rx,
                y: y - (icon_size + text_h) * 0.5,
                w: icon_size,
                h: icon_size,
            },
            white,
        );
        let rank_h = ui.text_height(cfg.font, rank_scale);
        ui.draw_text(
            p,
            &TextDraw {
                text: &level,
                font_enum: cfg.font,
                scale: rank_scale / unit,
                style: 3,
                color: white,
                x: rx + icon_size,
                y: y + rank_h * 0.25,
                horz: horz::NOSCALE,
                vert: vert::NOSCALE,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::NameScan;

    const COLORS: [[f32; 4]; 2] = [[0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]];

    fn cfg() -> Cfg {
        Cfg::read(&Cvars::default())
    }

    /// A match on the allies side: client 1 is a teammate, 2 an enemy, 3 another teammate.
    fn live(near: Vec<NearPlayer>, crosshair: Option<u16>) -> LiveUi {
        LiveUi {
            active: true,
            time: 10_000,
            own: 0,
            own_team: 2,
            eye: [0.0; 3],
            names: ["me", "ann", "bob", "cy"].map(String::from).into(),
            teams: vec![2, 2, 1, 2],
            ranks: vec![(0, 0); 4],
            scan: NameScan {
                near,
                crosshair,
                flashed: false,
            },
            ..LiveUi::default()
        }
    }

    fn near(client: u16) -> NearPlayer {
        NearPlayer {
            client,
            head: [500.0, 0.0, 60.0],
            clear: true,
            ..NearPlayer::default()
        }
    }

    fn drawn(n: &Names) -> Vec<u16> {
        n.names.iter().map(|d| d.client).collect()
    }

    #[test]
    fn a_name_fades_in_after_the_delay_and_out_after_it_is_gone() {
        // Linear: out of the range of 250 ms, 0 until 100 ms of sight, then full, then fading.
        assert_eq!(fade_alpha(1000, 1000, 1000, 100, 250), 0.0);
        assert_eq!(fade_alpha(1100, 1000, 1100, 100, 250), 1.0);
        assert_eq!(fade_alpha(1225, 1000, 1100, 100, 250), 0.5);
        assert_eq!(fade_alpha(1350, 1000, 1100, 100, 250), 0.0);
    }

    #[test]
    fn names_shrink_between_the_near_and_far_distance() {
        let c = cfg();
        assert_eq!(distance_scale(100.0, &c), 1.0);
        assert_eq!(distance_scale(5000.0, &c), c.far_scale);
        let mid = distance_scale((c.near_dist + c.far_dist) * 0.5, &c);
        assert!((mid - (1.0 + c.far_scale) * 0.5).abs() < 1e-5);
    }

    #[test]
    fn a_visible_teammate_has_a_name_in_the_my_team_colour_and_an_enemy_none() {
        let mut n = Names::default();
        n.update(&live(vec![near(1), near(2)], None), &cfg(), COLORS);
        assert_eq!(drawn(&n), [1]);
        assert_eq!(n.names[0].name, "ann");
        assert_eq!(n.names[0].color, COLORS[0]);
        assert_eq!(n.names[0].pos, [500.0, 0.0, 70.0]);
    }

    #[test]
    fn a_teammate_behind_a_wall_fades_out_over_the_fade_time_and_a_flash_hides_the_names() {
        let mut n = Names::default();
        let c = cfg();
        let mut l = live(vec![near(1)], None);
        n.update(&l, &c, COLORS);
        assert_eq!(drawn(&n), [1]);
        l.scan.near[0].clear = false;
        l.time += c.friendly_fade_out / 2;
        n.update(&l, &c, COLORS);
        assert_eq!(drawn(&n), [1]);
        assert!((n.names[0].color[3] - 0.5).abs() < 1e-5);
        l.time += c.friendly_fade_out;
        n.update(&l, &c, COLORS);
        assert!(n.names.is_empty());
        // In sight again, but blinded.
        l.scan.near[0].clear = true;
        l.scan.flashed = true;
        n.update(&l, &c, COLORS);
        assert!(n.names.is_empty());
    }

    #[test]
    fn an_enemy_under_the_crosshair_shows_in_the_enemy_colour_after_its_fade_in() {
        let mut n = Names::default();
        let c = cfg();
        let mut l = live(vec![near(2)], Some(2));
        n.update(&l, &c, COLORS);
        assert!(n.names.is_empty(), "fading in");
        l.time += c.enemy_fade_in;
        n.update(&l, &c, COLORS);
        assert_eq!(drawn(&n), [2]);
        assert_eq!(n.names[0].color, COLORS[1]);
        // The crosshair moves off: the name fades out and is gone after the fade time.
        l.scan.crosshair = None;
        l.time += c.enemy_fade_out / 2;
        n.update(&l, &c, COLORS);
        assert!((n.names[0].color[3] - 0.5).abs() < 1e-5);
        l.time += c.enemy_fade_out;
        n.update(&l, &c, COLORS);
        assert!(n.names.is_empty());
    }

    #[test]
    fn a_friend_both_overhead_and_under_the_crosshair_is_named_once() {
        let mut n = Names::default();
        n.update(&live(vec![near(1)], Some(1)), &cfg(), COLORS);
        assert_eq!(drawn(&n), [1]);
    }

    #[test]
    fn the_switch_off_dvars_remove_the_names() {
        let mut n = Names::default();
        let c = Cfg {
            draw_friendly: false,
            draw_crosshair: false,
            ..cfg()
        };
        n.update(&live(vec![near(1), near(2)], Some(2)), &c, COLORS);
        assert!(n.names.is_empty());
    }

    #[test]
    fn a_script_icon_shows_to_the_team_it_is_for_and_a_status_marker_stacks_above_it() {
        let mut n = Names::default();
        let c = cfg();
        let mut carrier = near(1);
        carrier.icon = Some(("waypoint_bomb".into(), 2));
        carrier.interrupted = true;
        let mut other = near(2);
        other.icon = Some(("waypoint_defend".into(), 1));
        n.update(&live(vec![carrier, other], None), &c, COLORS);
        let mats: Vec<_> = n.icons.iter().map(|i| i.material.as_str()).collect();
        assert_eq!(mats, ["waypoint_bomb", DISCONNECTED]);
        // The marker sits higher than the icon by the icon's size and a gap.
        assert_eq!(
            n.icons[1].pos[2] - n.icons[0].pos[2],
            c.script_icon_size + ICON_STACK
        );
    }

    #[test]
    fn head_icons_without_a_team_show_to_everyone_and_to_spectators() {
        let c = cfg();
        let mut p = near(2);
        p.icon = Some(("hud_icon".into(), 0));
        let mut q = near(2);
        q.icon = Some(("hud_axis".into(), 1));
        for (team, want) in [(2u8, 1usize), (3, 2)] {
            let mut l = live(vec![p.clone(), q.clone()], None);
            l.own_team = team;
            let mut n = Names::default();
            n.update(&l, &c, COLORS);
            assert_eq!(n.icons.len(), want, "viewer on team {team}");
        }
    }

    #[test]
    fn the_killcam_marks_you_and_only_a_teammate_is_shown_talking() {
        let mut n = Names::default();
        let mut me = near(0);
        me.you = true;
        let mut friend = near(1);
        friend.talking = true;
        let mut foe = near(2);
        foe.talking = true;
        n.update(&live(vec![me, friend, foe], None), &cfg(), COLORS);
        let mats: Vec<_> = n.icons.iter().map(|i| i.material.as_str()).collect();
        assert_eq!(mats, [KILLCAM_YOU, TALKING]);
    }

    #[test]
    fn a_small_or_far_icon_keeps_a_minimum_size_and_a_constant_one_ignores_distance() {
        let c = cfg();
        let mut icon = IconDraw {
            material: String::new(),
            pos: [0.0; 3],
            size: 0.0,
            constant: false,
        };
        let near_r = icon_radius_px(&icon, 100.0, 1.0, 1000.0, &c);
        let far_r = icon_radius_px(&icon, 5000.0, 1.0, 1000.0, &c);
        assert!(near_r > far_r);
        assert_eq!(far_r, c.icon_min_radius * 500.0);
        icon.constant = true;
        assert_eq!(
            icon_radius_px(&icon, 100.0, 1.0, 1000.0, &c),
            icon_radius_px(&icon, 5000.0, 1.0, 1000.0, &c)
        );
    }
}
