// SPDX-License-Identifier: GPL-3.0-or-later
//! The collision world and script movers on a booted stock map. Skips without `COD4_PATH`.

use gsc::{EntClass, EntRef, Value};
use server::game::{Ent, EntKind};
use server::mover;
use server::script::Args;
use server::server::Server;
use sim::cm::{Collide, ENTITYNUM_WORLD};
use sim::contents;
use std::path::PathBuf;

fn crash() -> Option<Server> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    let args: Vec<String> = [
        "+set",
        "net_port",
        "0",
        "+set",
        "g_gametype",
        "war",
        "+map",
        "mp_crash",
    ]
    .map(String::from)
    .to_vec();
    Some(Server::boot(&root, &args, false).expect("boot"))
}

fn v(x: f32, y: f32, z: f32) -> Value {
    Value::Vector([x, y, z])
}

#[test]
fn spawn_points_stand_on_the_floor_and_triggers_are_not_shot() {
    let Some(s) = crash() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let world = s.game.world.as_ref().expect("world");
    let (mut floors, mut spawns) = (0, 0);
    for (_, e) in s
        .game
        .in_use()
        .filter(|(_, e)| &*e.classname == "mp_tdm_spawn")
    {
        spawns += 1;
        let (a, b) = (e.origin, [e.origin[0], e.origin[1], e.origin[2] - 200.0]);
        let t = world.trace(
            [a[0], a[1], a[2] + 8.0],
            b,
            [-15.0, -15.0, 0.0],
            [15.0, 15.0, 70.0],
            1023,
            contents::MASK_PLAYERSOLID,
        );
        assert!(!t.start_solid, "spawn {a:?} starts inside the map");
        if t.fraction < 1.0 && t.normal[2] > 0.7 {
            floors += 1;
        }
    }
    assert!(spawns >= 10);
    assert_eq!(floors, spawns, "every spawn has walkable floor below it");
    // A bullet through the middle of a trigger brush (they are not shot-solid) sees only the world.
    let (_, trig) = s
        .game
        .in_use()
        .find(|(_, e)| &*e.classname == "trigger_multiple")
        .unwrap();
    let c = trig.origin;
    let t = world.trace(
        [c[0], c[1], c[2] + 300.0],
        [c[0], c[1], c[2] - 300.0],
        [0.0; 3],
        [0.0; 3],
        1023,
        contents::MASK_SHOT,
    );
    assert!(t.hit_id == ENTITYNUM_WORLD || t.fraction == 1.0);
}

#[test]
fn a_scripted_brush_model_blocks_shots_where_it_is_and_follows_moveto() {
    let Some(mut s) = crash() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    // A solid copy of the first trigger's inline model, floating high above the map.
    let (mut proto, model) = {
        let (_, t) = s
            .game
            .in_use()
            .find(|(_, e)| &*e.classname == "trigger_multiple")
            .unwrap();
        (t.clone(), t.model.clone())
    };
    proto.classname = "script_brushmodel".into();
    proto.kind = EntKind::Brush;
    proto.origin = [0.0, 0.0, 9000.0];
    proto.model = model;
    let n = s
        .game
        .spawn(Ent::new(EntKind::Brush, "script_brushmodel"))
        .unwrap();
    *s.game.ent_mut(n).unwrap() = proto;
    s.game.init_clip(n, None);
    s.game.ent_mut(n).unwrap().contents = contents::SOLID;
    s.game.relink(n);

    let shoot = |s: &Server, x: f32| {
        s.game.world.as_ref().unwrap().trace(
            [x, 0.0, 9500.0],
            [x, 0.0, 8500.0],
            [0.0; 3],
            [0.0; 3],
            1023,
            contents::MASK_SHOT,
        )
    };
    let top = s.game.ent(n).unwrap().maxs[2];
    let hit = shoot(&s, 0.0);
    assert_eq!(hit.hit_id, n);
    // The inline model bounds are padded by about a unit around the brushes.
    assert!(
        (9500.0 - hit.fraction * 1000.0 - (9000.0 + top)).abs() < 1.5,
        "{hit:?}"
    );
    assert_eq!(
        shoot(&s, 1000.0).fraction,
        1.0,
        "nothing 1000 units to the side"
    );

    // moveto: 1000 units in x over 2 s, which moves the hit with it.
    let me = EntRef {
        num: n,
        class: EntClass::Entity,
    };
    let args = [v(1000.0, 0.0, 9000.0), Value::Float(2.0)];
    mover::move_to(&mut s.game, me, Args::new("moveto", &args)).unwrap();
    s.run_frames(30);
    let mid = s.game.ent(n).unwrap().origin;
    assert!(
        (mid[0] - 500.0).abs() < 40.0,
        "about half way after 1 s, got {mid:?}"
    );
    s.run_frames(40);
    assert_eq!(s.game.ent(n).unwrap().origin, [1000.0, 0.0, 9000.0]);
    assert_eq!(shoot(&s, 0.0).fraction, 1.0, "it left");
    assert_eq!(shoot(&s, 1000.0).hit_id, n, "it arrived");
    assert!(s.script_errors.is_empty(), "{:#?}", s.script_errors);
}
