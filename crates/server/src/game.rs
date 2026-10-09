// SPDX-License-Identifier: GPL-3.0-only
//! Game state the scripts act on: entities, spawn parsing, configstrings and the level clock.
//!
//! Entity numbers follow the original: client slots first, then spawned entities from
//! [`FIRST_SPAWNED`] upward, the world entity at [`ENTITYNUM_WORLD`]. Fields the original
//! stores in the entity structure (`origin`, `targetname`, ...) live in [`Ent`]; every other
//! field a script sets is stored on its script object by the VM.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gsc::{EntClass, Value, Vm};
use sim::Vec3;
use sim::cm::ENTITYNUM_NONE;
use sim::contents;
use sim::traj::Trajectory;
use sim::world::{ClipEnt, World};

use crate::anim::AnimTree;
use crate::client::Client;
use crate::content::Content;
use crate::cvar::{self, Cvars};
use crate::missile::{Attractors, Missile};
use crate::mover::Mover;
use crate::playeranim::PlayerAnims;
use net::ui::cs;

pub const MAX_GENTITIES: usize = 1024;
pub const ENTITYNUM_WORLD: u16 = 1022;
/// The first entity number [`Game::spawn`] hands out (64 client slots plus 8 corpse slots).
pub const FIRST_SPAWNED: usize = 72;
/// Script objects of hud elements use numbers from here up so they never collide with entities.
pub const HUDELEM_BASE: u16 = 2048;
pub const MAX_HUDELEMS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTy {
    Str,
    Vector,
    Float,
    Int,
}

/// `radiant/keys.txt`: the map-entity keys that become script fields, with their types.
pub fn parse_field_types(text: &str) -> HashMap<String, FieldTy> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("");
        let mut it = line.split_whitespace();
        let (Some(ty), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        let ty = match ty {
            "string" => FieldTy::Str,
            "vector" => FieldTy::Vector,
            "float" => FieldTy::Float,
            "int" => FieldTy::Int,
            _ => continue,
        };
        m.insert(name.to_ascii_lowercase(), ty);
    }
    m
}

pub type SpawnVars = Vec<(String, String)>;

/// Parses the entity string of a map into one key/value list per entity.
pub fn parse_spawn_vars(text: &[u8]) -> Result<Vec<SpawnVars>, String> {
    let text = String::from_utf8_lossy(text);
    let mut out = Vec::new();
    let mut toks = Tokens(text.as_ref());
    while let Some(t) = toks.next() {
        if t != "{" {
            return Err(format!("expected {{ in entity string, found {t:?}"));
        }
        let mut vars = Vec::new();
        loop {
            let k = toks.next().ok_or("entity string ends inside an entity")?;
            if k == "}" {
                break;
            }
            let v = toks.next().ok_or("entity string ends after a key")?;
            vars.push((k.to_owned(), v.to_owned()));
        }
        out.push(vars);
    }
    Ok(out)
}

struct Tokens<'a>(&'a str);

impl<'a> Tokens<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let s = self
            .0
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\0');
        if let Some(rest) = s.strip_prefix('"') {
            let end = rest.find('"')?;
            self.0 = &rest[end + 1..];
            Some(&rest[..end])
        } else if s.is_empty() {
            None
        } else {
            let end = s
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\0')
                .unwrap_or(s.len());
            let (tok, rest) = s.split_at(end.max(1));
            self.0 = rest;
            Some(tok)
        }
    }
}

/// First value of key `k` (case-insensitive).
pub fn spawn_var<'a>(vars: &'a SpawnVars, k: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(k))
        .map(|(_, v)| v.as_str())
}

pub fn parse_vec3(s: &str) -> [f32; 3] {
    let mut v = [0.0; 3];
    for (o, t) in v.iter_mut().zip(s.split_whitespace()) {
        *o = cvar::parse_float(t);
    }
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntKind {
    /// Anything the scripts spawn or the map defines without special engine behavior.
    Plain,
    World,
    Client,
    /// `script_brushmodel`, whose `model` is `*N`.
    Brush,
    Trigger,
    Item,
}

#[derive(Debug, Clone)]
pub struct Ent {
    pub kind: EntKind,
    pub classname: Rc<str>,
    pub targetname: Option<Rc<str>>,
    pub target: Option<Rc<str>>,
    pub model: Rc<str>,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub spawnflags: i32,
    pub count: i32,
    pub health: i32,
    pub dmg: i32,
    pub hidden: bool,
    pub contents: i32,
    /// Local collision bounds (`r.mins`, `r.maxs`).
    pub mins: Vec3,
    pub maxs: Vec3,
    /// The inline model `*N` this entity clips as (`r.bmodel`).
    pub brush_model: Option<u16>,
    /// Script-driven motion of `script_model`, `script_origin` and `script_brushmodel`.
    pub mv: Mover,
    /// Animations playing on the entity, if any.
    pub anim: Option<AnimTree>,
    /// Level time when the slot was freed, in ms.
    pub free_time: i32,
    /// Level time at which the engine frees the entity (`nextthink` of a mantle blocker).
    pub free_at: Option<i32>,
    /// `useCount`: differs for every entity that ever held the slot, so a queued trigger can tell its
    /// entity was freed. Players keep 0.
    pub use_count: u32,
    /// `takedamage`: damage reaches the entity's scripts (`setcandamage`, living players).
    pub takedamage: bool,
    /// `ent->flags`: 1 invulnerable, 2 cannot die, 8 takes no knockback.
    pub flags: i32,
    /// Link, attachment, trigger and corpse state of the entity builtins.
    pub x: crate::link::EntExtra,
    /// Flight state of a grenade or rocket (`ET_MISSILE`).
    pub missile: Option<Box<Missile>>,
    /// `r.ownerNum`: traces made by the owner pass through this entity.
    pub owner: Option<u16>,
    /// A script vehicle's flight and turret state (`scr_vehicle`).
    pub veh: Option<Box<crate::vehicle::Vehicle>>,
    /// A dropped or placed weapon (`ET_ITEM`).
    pub item: Option<Box<crate::items::DroppedItem>>,
    /// The effect this entity plays for the clients (`spawnfx`, `playloopedfx`).
    pub world_fx: Option<WorldFx>,
}

/// What a script effect entity tells the clients to play (`ET_FX`, `ET_LOOP_FX`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorldFx {
    /// `spawnfx`: played once for each `triggerfx`; `triggers` counts them (wrapping, never back to 0), `start_ms`
    /// is the level time the latest one plays at (its call plus its delay).
    Once {
        effect: u16,
        triggers: u8,
        start_ms: i32,
    },
    /// `playloopedfx`: restarted every `period_ms`, for whoever is within `cull` units (0 for everyone).
    Looped {
        effect: u16,
        period_ms: u32,
        cull: f32,
    },
}

impl Ent {
    pub fn new(kind: EntKind, classname: &str) -> Self {
        Self {
            kind,
            classname: classname.into(),
            targetname: None,
            target: None,
            model: "".into(),
            origin: [0.0; 3],
            angles: [0.0; 3],
            spawnflags: 0,
            count: 0,
            health: 0,
            dmg: 0,
            hidden: false,
            contents: 0,
            mins: [0.0; 3],
            maxs: [0.0; 3],
            brush_model: None,
            mv: Mover::default(),
            anim: None,
            free_time: 0,
            free_at: None,
            use_count: 0,
            takedamage: false,
            flags: 0,
            x: crate::link::EntExtra::default(),
            missile: None,
            owner: None,
            veh: None,
            item: None,
            world_fx: None,
        }
    }
}

/// A name the scripts precached (`precachemodel`, `precacheshader`, ...) with its index.
#[derive(Default)]
pub struct Precache {
    names: Vec<Rc<str>>,
}

impl Precache {
    /// Index of `name`, assigning the next one on first use (1-based, 0 means none).
    pub fn index(&mut self, name: &str) -> usize {
        match self.names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
            Some(i) => i + 1,
            None => {
                self.names.push(name.into());
                self.names.len()
            }
        }
    }

    /// As [`Self::index`] but names differing in case are different names.
    pub fn index_exact(&mut self, name: &str) -> usize {
        match self.names.iter().position(|n| &**n == name) {
            Some(i) => i + 1,
            None => {
                self.names.push(name.into());
                self.names.len()
            }
        }
    }

    /// Index of `name` if it was registered, else 0.
    pub fn find(&self, name: &str) -> usize {
        self.names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .map_or(0, |i| i + 1)
    }

    /// Forgets every name after the first `len`.
    pub fn truncate(&mut self, len: usize) {
        self.names.truncate(len);
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Name of the 1-based `index`.
    pub fn name(&self, index: usize) -> Option<&str> {
        self.names.get(index.checked_sub(1)?).map(|n| &**n)
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Script entry points the engine calls (`Scr_ExecEntThread` targets).
#[derive(Default, Clone, Copy)]
pub struct Callbacks {
    pub start_game_type: Option<u32>,
    pub player_connect: Option<u32>,
    pub player_disconnect: Option<u32>,
    pub player_damage: Option<u32>,
    pub player_killed: Option<u32>,
    pub player_last_stand: Option<u32>,
    /// `_hardpoints::giveHardpointItem`, for the `devhardpoint` test hook.
    pub give_hardpoint: Option<u32>,
}

/// A script function the engine wants run: queued by game code, run by the script host as soon
/// as the running builtin or frame step returns (`Scr_ExecEntThread` runs it to its first wait).
pub struct ScriptCall {
    pub func: u32,
    /// The entity the callback runs on (`self`); `level` when `None`.
    pub this: Option<u16>,
    pub args: Vec<Value>,
}

#[derive(Default)]
pub struct Level {
    /// Milliseconds since the map started (`level.time`, `gettime()`).
    pub time: i32,
    pub frame: u32,
    pub frametime: i32,
    /// True during the initial script run.
    pub initializing: bool,
    pub north_yaw: f32,
    pub exit_requested: bool,
    pub map_restart_requested: bool,
    /// A script's `savepersist` argument of `map_restart`, `map` or `exitlevel`: the `game` array survives the
    /// level change (`level.savepersist`).
    pub save_persist: bool,
    pub num_entities: usize,
    /// `setplayerignoreradiusdamage`.
    pub ignore_radius_damage: bool,
    /// Next slot of the player corpse ring (`level.currentPlayerClone`).
    pub next_corpse: usize,
    /// `map(name)` was called: the server changes to this map after the frame.
    pub map_requested: Option<String>,
    /// Entities spawned so far (`useCount` of the next one).
    pub spawn_serial: u32,
    /// `level.pendingTriggerList`: touches the next trigger pass delivers.
    pub pending_triggers: Vec<crate::trigger::PendingTrigger>,
}

/// What happened in play since boot, for harness reports.
#[derive(Default, Debug, Clone, Copy)]
pub struct MatchStats {
    /// Players killed by another player.
    pub kills: u64,
    /// Every player death, including suicides, falls and damage volumes.
    pub deaths: u64,
    /// Spawns after a player's first on the map.
    pub respawns: u64,
    pub spawns: u64,
    /// Bullets fired and grenades or rockets launched.
    pub shots: u64,
    /// Shots that hurt a player.
    pub hits: u64,
    /// Rounds ended (`exitlevel`): the match reached its score or time limit.
    pub matches_ended: u64,
    /// Bombs planted and defused, as the gametype scripts log them.
    pub plants: u64,
    pub defuses: u64,
    /// Hardpoints (UAV, airstrike, helicopter) called in.
    pub hardpoints: u64,
    pub airstrikes: u64,
    pub helicopters: u64,
    /// Rounds a hardpoint helicopter fired, hits it dealt to players, and times one began to crash.
    pub heli_shots: u64,
    pub heli_hits: u64,
    pub heli_crashes: u64,
}

pub struct Game {
    /// `setMiniMap`: the map's world size along its axes, north's direction and the upper left corner (location picks).
    pub compass: Option<[f32; 6]>,
    /// `setteamradar`: whether the players with no team, the axis and the allies have radar (`level.teamHasRadar`).
    pub team_radar: [bool; 3],
    pub vote: crate::vote::VoteState,
    /// `games_mp.log`.
    pub log: crate::gamelog::GameLog,
    /// Players the game dropped for sitting idle (`g_inactivity`), so the network side says why.
    pub inactive: Vec<u16>,
    /// Identifies this start of the level to the clients (`sv_serverId`): a vote called in an earlier level is stale.
    pub server_id: i32,
    pub cvars: Cvars,
    pub content: Content,
    pub level: Level,
    pub ents: Vec<Option<Ent>>,
    /// Collision world of the loaded map with every solid entity linked into it.
    pub world: Option<World>,
    pub ui: crate::ui::ServerUi,
    /// `setarchive(true)`: the network layer keeps the history killcams replay.
    pub archive_enabled: bool,
    pub field_types: HashMap<String, FieldTy>,
    pub models: Precache,
    pub shaders: Precache,
    pub strings: Precache,
    pub fx: Precache,
    pub items: Precache,
    pub menus: Precache,
    pub configstrings: HashMap<u32, String>,
    pub max_clients: usize,
    /// Score per team, indexed like [`Team`](crate::client::Team): 1 axis, 2 allies.
    pub team_score: [i32; 3],
    /// Map zones of the install (`mapexists`).
    pub known_maps: Vec<String>,
    pub rng: u64,
    /// Console output of the game (`Com_Printf`), drained by the server.
    pub printed: Vec<String>,
    /// Builtins that were called but belong to a later milestone, with call counts.
    pub stub_calls: HashMap<String, u64>,
    pub clients: Vec<Client>,
    pub callbacks: Callbacks,
    /// Script calls waiting for the host (see [`ScriptCall`]).
    pub calls: Vec<ScriptCall>,
    /// Errors of script calls that ran nested inside a builtin.
    pub nested_errors: Vec<gsc::VmError>,
    /// Clients whose disconnect callback is queued; their slots free afterwards.
    pub pending_free: Vec<u16>,
    pub pm_params: sim::pm::Params,
    /// Where every player's body has been lately, for judging a shot as its shooter saw it.
    pub lag: crate::lagcomp::LagRing,
    /// While a person's command runs: the server time their shot is judged at.
    pub lag_time: Option<i32>,
    pub stats: MatchStats,
    /// Bot navigation of the current map, built when the first bot joins.
    pub nav: Option<Arc<crate::nav::NavMesh>>,
    pub nav_goals: Vec<Vec3>,
    /// Navigation generation per map load: (map, ms, nodes).
    pub nav_loads: Vec<(String, f32, u32)>,
    pub weapons: sim::weapon::WeaponTable,
    /// `g_fHitLocDamageMult`: the gametype's hit-location scale for non-bullet damage.
    pub hitloc_table: [f32; 19],
    /// `info/bullet_penetration_mp`, parsed on the first shot of a map.
    pub penetration: Option<Arc<sim::weapon::damage::PenetrationTable>>,
    /// The skeleton locational hits are tested against, built on the first shot of a map.
    pub player_anims: Option<Arc<PlayerAnims>>,
    pub attractors: Attractors,
    /// Sound commands for the clients (see `script::sound`), drained by the network side every frame.
    pub sound_out: Vec<(SoundTo, String)>,
    /// The `ambient` command in force, replayed to clients that join.
    pub ambient: Option<String>,
    /// One-shot events for the clients' snapshots.
    pub tempev: crate::tempev::TempEvents,
    /// The vision set in force (`visionsetnaked`, `visionsetnight`) as `(name)`, replayed to clients that join.
    pub vision: [Option<String>; 2],
    /// Entities of dropped weapons, oldest first (`level.droppedWeaponCue`).
    pub dropped: Vec<u16>,
}

/// Who hears a sound command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundTo {
    All,
    Client(u16),
    Team(crate::client::Team),
}

impl Game {
    pub fn new(cvars: Cvars, content: Content) -> Self {
        Self {
            cvars,
            content,
            level: Level::default(),
            ents: Vec::new(),
            world: None,
            ui: crate::ui::ServerUi::default(),
            archive_enabled: false,
            field_types: HashMap::new(),
            models: Precache::default(),
            shaders: Precache::default(),
            strings: Precache::default(),
            fx: Precache::default(),
            items: Precache::default(),
            menus: Precache::default(),
            configstrings: HashMap::new(),
            max_clients: 0,
            team_score: [0; 3],
            known_maps: Vec::new(),
            rng: 0x9E37_79B9_7F4A_7C15,
            printed: Vec::new(),
            stub_calls: HashMap::new(),
            clients: Vec::new(),
            callbacks: Callbacks::default(),
            calls: Vec::new(),
            nested_errors: Vec::new(),
            pending_free: Vec::new(),
            pm_params: sim::pm::Params::default(),
            lag: crate::lagcomp::LagRing::default(),
            lag_time: None,
            stats: MatchStats::default(),
            nav: None,
            nav_goals: Vec::new(),
            vote: Default::default(),
            log: Default::default(),
            inactive: Vec::new(),
            server_id: 0,
            compass: None,
            team_radar: [false; 3],
            nav_loads: Vec::new(),
            hitloc_table: default_hitloc_table(),
            weapons: sim::weapon::WeaponTable::from_infos(Vec::new()).expect("empty table"),
            penetration: None,
            player_anims: None,
            tempev: Default::default(),
            vision: [None, None],
            attractors: Attractors::default(),
            sound_out: Vec::new(),
            ambient: None,
            dropped: Vec::new(),
        }
    }

    pub fn print(&mut self, s: impl Into<String>) {
        self.printed.push(s.into());
    }

    pub fn ent(&self, num: u16) -> Option<&Ent> {
        self.ents.get(usize::from(num)).and_then(Option::as_ref)
    }

    pub fn ent_mut(&mut self, num: u16) -> Option<&mut Ent> {
        self.ents.get_mut(usize::from(num)).and_then(Option::as_mut)
    }

    /// Entities in use, by ascending number.
    pub fn in_use(&self) -> impl Iterator<Item = (u16, &Ent)> {
        self.ents
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| (i as u16, e)))
    }

    /// Tells the clients of a physics world event (`physicsexplosionsphere` and its kin).
    pub fn physics_event(&mut self, origin: sim::Vec3, p: crate::tempev::Physics) {
        let now = self.level.time;
        self.tempev.add_physics(now, origin, &p);
    }

    /// `earthquake`: tells the clients to shake the cameras of whoever is near `origin`.
    pub fn earthquake(&mut self, origin: sim::Vec3, scale: f32, duration_ms: i32, radius: f32) {
        let now = self.level.time;
        let q = crate::tempev::Earthquake {
            scale,
            duration_ms,
            radius,
        };
        self.tempev.add_earthquake(now, origin, &q);
    }

    /// `G_Spawn`: the lowest free slot from [`FIRST_SPAWNED`], growing the table on demand.
    pub fn spawn(&mut self, mut ent: Ent) -> Result<u16, String> {
        let first = FIRST_SPAWNED.max(self.max_clients);
        // The slots from `tempev::FIRST` up carry the one-shot events in snapshots: entities stop short of them.
        let limit = usize::from(crate::tempev::FIRST);
        let slot = (first..self.ents.len())
            .find(|&i| self.ents[i].is_none())
            .unwrap_or_else(|| self.ents.len().max(first));
        if slot >= limit {
            return Err("G_Spawn: no free entities".into());
        }
        if slot >= self.ents.len() {
            self.ents.resize(slot + 1, None);
        }
        self.level.spawn_serial += 1;
        ent.use_count = self.level.spawn_serial;
        self.ents[slot] = Some(ent);
        self.level.num_entities = self.level.num_entities.max(slot + 1);
        Ok(slot as u16)
    }

    /// `G_FreeEntity`: the script object dies at the next `Scr_IncTime`.
    pub fn free_entity(&mut self, vm: &mut Vm, num: u16) {
        self.unlink_all(num);
        self.dropped.retain(|&d| d != num);
        if let Some(slot) = self.ents.get_mut(usize::from(num))
            && let Some(e) = slot.take()
        {
            if e.veh.is_some() {
                // The engine hum on the vehicle ends with it.
                self.sound_out
                    .push((SoundTo::All, format!("stoploop {num} *")));
            }
            self.attractors.free_entity(num);
            if let Some(w) = self.world.as_mut() {
                w.unlink(num);
            }
            vm.free_entity(num);
        }
    }

    /// Clears per-map state for a new `G_InitGame`.
    pub fn reset_level(&mut self, max_clients: usize) {
        self.level = Level::default();
        self.vote = Default::default();
        self.ambient = None;
        self.sound_out.clear();
        self.ents.clear();
        self.world = None;
        self.ui = crate::ui::ServerUi::default();
        self.archive_enabled = false;
        self.models = Precache::default();
        self.shaders = Precache::default();
        self.strings = Precache::default();
        self.fx = Precache::default();
        self.items = Precache::default();
        self.menus = Precache::default();
        self.configstrings.clear();
        self.penetration = None;
        self.player_anims = None;
        self.attractors = Attractors::default();
        self.tempev = Default::default();
        self.vision = [None, None];
        self.dropped.clear();
        self.team_score = [0; 3];
        self.nav = None;
        self.nav_goals.clear();
        self.max_clients = max_clients;
        self.lag = crate::lagcomp::LagRing::new(max_clients);
        self.lag_time = None;
        self.clients = (0..max_clients)
            .map(|n| {
                let mut c = Client::new(n as u16, false, String::new());
                c.conn = crate::client::Conn::Free;
                c
            })
            .collect();
        self.ents.resize(max_clients, None);
        self.calls.clear();
        self.nested_errors.clear();
        self.pending_free.clear();
    }

    pub fn map_exists(&self, map: &str) -> bool {
        self.known_maps.iter().any(|m| m.eq_ignore_ascii_case(map))
    }

    /// A gametype is valid when its script is in the loaded zones.
    pub fn valid_gametype(&self, name: &str) -> bool {
        !name.starts_with('_')
            && self
                .content
                .rawfile(&format!("maps/mp/gametypes/{name}.gsc"))
                .is_some()
    }

    /// Next pseudo-random number (xorshift64*), seeded by `srand`.
    pub fn rand(&mut self) -> u32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u32
    }

    /// `SV_LinkEntity`: publishes entity `num`'s origin, angles, bounds and contents to the
    /// collision world. Call after any of them changes.
    pub fn relink(&mut self, num: u16) {
        let (Some(w), Some(Some(e))) = (self.world.as_mut(), self.ents.get(usize::from(num)))
        else {
            return;
        };
        w.link(
            num,
            &ClipEnt {
                contents: e.contents,
                origin: e.origin,
                angles: e.angles,
                mins: e.mins,
                maxs: e.maxs,
                brush_model: e.brush_model,
                owner: e.owner.unwrap_or(ENTITYNUM_NONE),
            },
        );
    }

    /// The collision setup of the entity classes that clip (`SP_script_model`,
    /// `SP_script_brushmodel`, `SP_script_origin`, inline-model triggers), then links it.
    pub fn init_clip(&mut self, num: u16, radius_height: Option<(f32, f32)>) {
        let Some(w) = self.world.as_ref() else { return };
        let Some(e) = self.ents.get_mut(usize::from(num)).and_then(Option::as_mut) else {
            return;
        };
        let kind_is_item = e.kind == EntKind::Item;
        let inline = e
            .model
            .strip_prefix('*')
            .and_then(|n| n.parse::<u16>().ok());
        if let Some((mins, maxs)) = inline.and_then(|n| w.collision().model_bounds(n)) {
            let n = inline.unwrap_or_default();
            e.mins = mins;
            e.maxs = maxs;
            e.brush_model = Some(n);
            e.contents = w.collision().model_contents(n);
        }
        if matches!(
            &*e.classname,
            "script_model" | "script_origin" | "script_brushmodel"
        ) {
            e.flags |= crate::link::FL_SUPPORTS_LINKTO;
        }
        match &*e.classname {
            "script_model" => e.contents = contents::MISSILECLIP | contents::CLIPSHOT,
            "trigger_hurt" => e.contents = TRIGGER_HURT_CONTENTS,
            "trigger_use" | "trigger_use_touch" => {
                e.contents = contents::USE;
                // `trigger_use`: HINT_NOICON until a script or the map says otherwise.
                e.x.cursor_hint = 1;
            }
            "trigger_radius" => {
                if let Some((r, h)) = radius_height {
                    e.mins = [-r, -r, 0.0];
                    e.maxs = [r, r, h];
                }
                e.contents = sentient_trigger(e.spawnflags);
            }
            "trigger_disk" => {
                // `SP_trigger_disk`: a column 64 wider than the radius, as tall as the world.
                if let Some((r, _)) = radius_height {
                    let r = r + 64.0;
                    e.mins = [-r, -r, -100_000.0];
                    e.maxs = [r, r, 100_000.0];
                }
                e.contents = sentient_trigger(e.spawnflags);
            }
            "trigger_multiple" | "trigger_once" => e.contents = sentient_trigger(e.spawnflags),
            "trigger_damage" => {
                e.contents = TRIGGER_HURT_CONTENTS;
                e.takedamage = true;
                e.health = 32000;
            }
            // `SP_trigger_lookat`: nothing touches it; scripts trace for it.
            "trigger_lookat" => e.contents = contents::TRANSLUCENT,
            _ => {}
        }
        if kind_is_item {
            self.init_item(num);
        }
        self.relink(num);
    }

    /// Builds the bot navigation mesh of the loaded map (once per map load) from the static
    /// collision world: nothing linked, so players and movers do not shape it.
    pub fn ensure_nav(&mut self) -> Option<Arc<crate::nav::NavMesh>> {
        if self.nav.is_none() {
            let clip = self.content.clipmap()?.clone();
            let ents = clip.map_ents.as_ref()?;
            let spawns = crate::nav::spawn_points(&ents.entity_string);
            let seeds: Vec<Vec3> = spawns.iter().map(|s| s.origin).collect();
            let mesh = crate::nav::NavMesh::generate(&World::new(clip), &seeds);
            let st = mesh.stats();
            self.print(format!(
                "navigation: {} nodes, {} edges, {} KiB, generated in {:.0} ms\n",
                st.nodes,
                st.edges,
                st.bytes >> 10,
                st.generation_ms
            ));
            let map = self.content.map_name.clone().unwrap_or_default();
            self.nav_loads.push((map, st.generation_ms, st.nodes));
            self.nav_goals = seeds;
            self.nav = Some(Arc::new(mesh));
        }
        self.nav.clone()
    }

    /// `G_SpawnEntitiesFromString` for the map's entity string: the worldspawn first, then
    /// one entity per entry with the spawn function of its classname.
    pub fn spawn_map_entities(&mut self, vm: &mut Vm, ents: &[SpawnVars]) -> Result<(), String> {
        let (world, rest) = ents.split_first().ok_or("SpawnEntities: no entities")?;
        let get = spawn_var;
        if !get(world, "classname").is_some_and(|c| c.eq_ignore_ascii_case("worldspawn")) {
            return Err("SP_worldspawn: The first entity isn't worldspawn".into());
        }
        let ambient = get(world, "ambienttrack").map_or(String::new(), |s| {
            if s.is_empty() {
                String::new()
            } else {
                format!("n\\{s}")
            }
        });
        self.set_configstring(cs::AMBIENT, &ambient);
        self.set_configstring(cs::MESSAGE, get(world, "message").unwrap_or(""));
        self.cvars
            .force("g_gravity", get(world, "gravity").unwrap_or("800"));
        let north = get(world, "northyaw").unwrap_or("");
        self.set_configstring(cs::NORTHYAW, if north.is_empty() { "0" } else { north });
        self.level.north_yaw = cvar::parse_float(north);
        let gametype = self.cvars.string("g_gametype").to_ascii_lowercase();
        self.set_configstring(cs::GAMETYPE, &gametype);
        if let Some(clip) = self.content.clipmap() {
            self.world = Some(World::new(clip.clone()));
        }
        let mut w = Ent::new(EntKind::World, "worldspawn");
        w.spawnflags = cvar::parse_int(get(world, "spawnflags").unwrap_or("0"));
        if self.ents.len() <= usize::from(ENTITYNUM_WORLD) {
            self.ents.resize(usize::from(ENTITYNUM_WORLD) + 1, None);
        }
        self.ents[usize::from(ENTITYNUM_WORLD)] = Some(w);

        for vars in rest {
            let Some(class) = get(vars, "classname") else {
                self.print("G_CallSpawn: NULL classname\n");
                continue;
            };
            if class.starts_with("dyn_") {
                continue;
            }
            let kind = spawn_kind(class);
            // `light` and `script_struct` are freed by their spawn function.
            if matches!(class, "light" | "script_struct") {
                continue;
            }
            let mut e = Ent::new(kind, class);
            apply_spawn_vars(&mut e, vars);
            let model = e.model.clone();
            let num = self.spawn(e)?;
            self.note_model(&model);
            let radius = get(vars, "radius").map(cvar::parse_float);
            let height = get(vars, "height").map(cvar::parse_float);
            // A disk has no height of its own.
            let extent = if class == "trigger_disk" {
                radius.map(|r| (r, 0.0))
            } else {
                radius.zip(height)
            };
            self.init_clip(num, extent);
            if class.starts_with("trigger_") {
                let int = |k| get(vars, k).map_or(0, cvar::parse_int);
                self.init_trigger_spawn(
                    num,
                    get(vars, "wait").map(cvar::parse_float),
                    int("accumulate"),
                    int("threshold"),
                );
            }
            if class.starts_with("trigger_use") {
                if let Some(h) =
                    get(vars, "cursorhint").and_then(|h| crate::script::hint_value(h, true))
                    && let Some(e) = self.ent_mut(num)
                {
                    e.x.cursor_hint = h;
                }
                if let Some(text) = get(vars, "hintstring") {
                    let slot = self.hint_string_index(text)?;
                    if let Some(e) = self.ent_mut(num) {
                        e.x.hint = Some(slot);
                    }
                }
            }
            let obj = vm.entity(num, EntClass::Entity);
            for (k, v) in vars {
                if let Some(ty) = self.field_types.get(&k.to_ascii_lowercase()) {
                    obj.set(&k.to_ascii_lowercase().into(), typed_value(*ty, v));
                }
            }
        }
        Ok(())
    }
}

/// `trigger_hurt` contents: every trigger type (`CONTENTS_ANY_TRIGGER`).
pub const TRIGGER_HURT_CONTENTS: i32 = 0x405C_0008;

/// `InitSentientTrigger`: which kinds of player set the trigger off.
fn sentient_trigger(spawnflags: i32) -> i32 {
    let mut c = 0;
    if spawnflags & 8 == 0 {
        c |= contents::PLAYERTRIGGER;
    }
    if spawnflags & 1 != 0 {
        c |= contents::AXISTRIGGER;
    }
    if spawnflags & 2 != 0 {
        c |= contents::ALLIESTRIGGER;
    }
    if spawnflags & 4 != 0 {
        c |= contents::NEUTRALTRIGGER;
    }
    c
}

fn spawn_kind(class: &str) -> EntKind {
    match class {
        "script_brushmodel" => EntKind::Brush,
        c if c.starts_with("trigger_") => EntKind::Trigger,
        c if c.starts_with("weapon_") => EntKind::Item,
        _ => EntKind::Plain,
    }
}

/// The ten engine-stored fields (`G_ParseEntityField`); everything else is a script field.
fn apply_spawn_vars(e: &mut Ent, vars: &SpawnVars) {
    for (k, v) in vars {
        match k.to_ascii_lowercase().as_str() {
            "origin" => e.origin = parse_vec3(v),
            "angles" => e.angles = parse_vec3(v),
            "model" => e.model = v.as_str().into(),
            "spawnflags" => e.spawnflags = cvar::parse_int(v),
            "target" => e.target = Some(v.as_str().into()),
            "targetname" => e.targetname = Some(v.as_str().into()),
            "count" => e.count = cvar::parse_int(v),
            "health" => e.health = cvar::parse_int(v),
            "dmg" => e.dmg = cvar::parse_int(v),
            _ => {}
        }
    }
}

pub fn typed_value(ty: FieldTy, v: &str) -> Value {
    match ty {
        FieldTy::Str => Value::str(v),
        FieldTy::Vector => Value::Vector(parse_vec3(v)),
        FieldTy::Float => Value::Float(cvar::parse_float(v)),
        FieldTy::Int => Value::Int(cvar::parse_int(v)),
    }
}

/// The engine-stored fields of entity `ent` as script values (`Scr_GetEntityField`).
pub fn get_ent_field(e: &Ent, name: &str) -> Option<Value> {
    Some(match name {
        "classname" => Value::str(&e.classname),
        "origin" => Value::Vector(e.origin),
        "angles" => Value::Vector(e.angles),
        "model" => Value::str(&e.model),
        "spawnflags" => Value::Int(e.spawnflags),
        "target" => e.target.as_deref().map_or(Value::Undefined, Value::str),
        "targetname" => e.targetname.as_deref().map_or(Value::Undefined, Value::str),
        "count" => Value::Int(e.count),
        "health" => Value::Int(e.health),
        "dmg" => Value::Int(e.dmg),
        _ => return None,
    })
}

/// `Scr_SetEntityField`; `Ok(true)` when `name` is an engine field.
pub fn set_ent_field(e: &mut Ent, name: &str, v: &Value) -> Result<bool, String> {
    let int = |v: &Value| match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) => Ok(*f as i32),
        o => Err(format!("type {} is not an int", o.type_name())),
    };
    let string = |v: &Value| match v {
        Value::Str(s) => Ok(Some(Rc::from(&**s))),
        Value::Undefined => Ok(None),
        o => Err(format!("type {} is not a string", o.type_name())),
    };
    let vec = |v: &Value| match v {
        Value::Vector(x) => Ok(*x),
        o => Err(format!("type {} is not a vector", o.type_name())),
    };
    match name {
        "classname" | "model" | "spawnflags" => {
            return Err(format!("field '{name}' is read-only"));
        }
        "origin" => {
            e.origin = vec(v)?;
            e.mv.pos.tr = Trajectory::stationary(e.origin);
        }
        "angles" => {
            e.angles = vec(v)?;
            e.mv.ang.tr = Trajectory::stationary(e.angles);
        }
        "target" => e.target = string(v)?,
        "targetname" => e.targetname = string(v)?,
        "count" => e.count = int(v)?,
        "health" => e.health = int(v)?,
        "dmg" => e.dmg = int(v)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn default_hitloc_table() -> [f32; 19] {
    let mut t = [1.0; 19];
    t[18] = 0.0;
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entity_strings() {
        let t = b"{\n\"classname\" \"worldspawn\"\n\"gravity\" \"800\"\n}\n{\n\"classname\" \"script_model\"\n\"origin\" \"1 2 3\"\n}\n\0";
        let e = parse_spawn_vars(t).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[1][1], ("origin".into(), "1 2 3".into()));
        assert!(parse_spawn_vars(b"{ \"a\"").is_err());
    }

    #[test]
    fn script_models_reach_clients_by_precache_index() {
        let mut g = Game::new(Cvars::new(), Content::default());
        let mut e = Ent::new(EntKind::Plain, "script_model");
        e.model = "vehicle_sedan".into();
        e.origin = [1.0, 2.0, 3.0];
        let n = g.spawn(e).unwrap();
        // Not registered yet: a client could not name it.
        let published = |g: &Game| {
            crate::netsv::world_entities(g)
                .into_iter()
                .find(|s| s.number == n)
        };
        assert!(published(&g).is_none());
        g.note_model("vehicle_sedan");
        let s = published(&g).expect("a registered script_model is published");
        assert_eq!(s.etype, net::entity::etype::SCRIPT_MODEL);
        assert_eq!(s.origin, [1.0, 2.0, 3.0]);
        assert_eq!(
            g.configstrings[&(u32::from(net::ui::cs::MODELS) + u32::from(s.model))],
            "vehicle_sedan"
        );
        g.ent_mut(n).unwrap().hidden = true;
        assert!(published(&g).is_none(), "a hidden model is not drawn");
    }

    #[test]
    fn a_plane_is_published_with_its_owner_and_team() {
        use crate::client::{Client, Conn, Team};
        let mut g = Game::new(Cvars::new(), Content::default());
        g.clients = vec![Client::new(0, false, "p0".into())];
        g.clients[0].conn = Conn::Connected;
        g.clients[0].team = Team::Allies;
        let mut e = Ent::new(EntKind::Plain, "script_model");
        e.model = "vehicle_mig29_desert".into();
        e.x.plane_owner = Some(0);
        let n = g.spawn(e).unwrap();
        g.note_model("vehicle_mig29_desert");
        let s = crate::netsv::world_entities(&g)
            .into_iter()
            .find(|s| s.number == n)
            .unwrap();
        assert_eq!(s.etype, net::entity::etype::PLANE);
        assert_eq!(s.client, 0);
        assert_ne!(s.eflags & crate::netsv::eflags::TEAM_ALLIES, 0);
    }

    #[test]
    fn a_launched_script_model_is_published_with_its_launch() {
        let mut g = Game::new(Cvars::new(), Content::default());
        let mut e = Ent::new(EntKind::Plain, "script_model");
        e.model = "com_barrel".into();
        e.origin = [10.0, 20.0, 30.0];
        let n = g.spawn(e).unwrap();
        g.note_model("com_barrel");
        let published = |g: &Game| {
            crate::netsv::world_entities(g)
                .into_iter()
                .find(|s| s.number == n)
                .unwrap()
        };
        assert_eq!(
            published(&g).eflags & crate::netsv::eflags::PHYSICS_LAUNCH,
            0
        );
        g.ent_mut(n).unwrap().x.physics_launch = Some(([11.0, 20.0, 34.0], [0.0, 50.0, 300.0]));
        let s = published(&g);
        assert_ne!(s.eflags & crate::netsv::eflags::PHYSICS_LAUNCH, 0);
        assert_eq!(s.origin, [10.0, 20.0, 30.0]);
        assert_eq!(s.launch_point, [11.0, 20.0, 34.0]);
        assert_eq!(s.velocity, [0.0, 50.0, 300.0]);
    }

    #[test]
    fn field_types_ignore_comments() {
        let m = parse_field_types(
            "// x\nvector\torigin\nfloat script_wait // c\nint  \tscript_cheap\nbad x\n",
        );
        assert_eq!(m["script_wait"], FieldTy::Float);
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn spawn_starts_after_the_client_and_corpse_slots() {
        let mut g = Game::new(Cvars::new(), Content::default());
        g.max_clients = 32;
        let a = g.spawn(Ent::new(EntKind::Plain, "a")).unwrap();
        let b = g.spawn(Ent::new(EntKind::Plain, "b")).unwrap();
        assert_eq!((a, b), (72, 73));
        g.ents[72] = None;
        assert_eq!(g.spawn(Ent::new(EntKind::Plain, "c")).unwrap(), 72);
    }
}
