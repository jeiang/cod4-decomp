// SPDX-License-Identifier: GPL-3.0-or-later
//! Tag lookup and attachments against the stock models. Skips without `COD4_PATH`.

use server::game::{Ent, EntKind, Game};
use server::server::Server;
use std::path::PathBuf;

fn boot() -> Option<Server> {
    let root = PathBuf::from(std::env::var_os("COD4_PATH")?);
    let args: Vec<String> = ["+set", "net_port", "0", "+map", "mp_crash"]
        .map(String::from)
        .to_vec();
    Some(Server::boot(&root, &args, false).expect("boot"))
}

fn model_ent(g: &mut Game, model: &str, origin: [f32; 3], angles: [f32; 3]) -> u16 {
    let mut e = Ent::new(EntKind::Plain, "script_model");
    e.model = model.into();
    e.origin = origin;
    e.angles = angles;
    g.spawn(e).unwrap()
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 0.05)
}

#[test]
fn tags_are_rest_pose_bones_in_world_space() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let g = &mut s.game;
    let gun = g.content.skeleton("weapon_ak47").expect("model").clone();
    let flash = gun.tag("tag_flash").expect("tag")[3];
    let e = model_ent(g, "weapon_ak47", [100.0, 200.0, 300.0], [0.0; 3]);
    let m = g.world_tag(e, "tag_flash").expect("tag");
    assert!(near(
        m[3],
        [100.0 + flash[0], 200.0 + flash[1], 300.0 + flash[2]]
    ));
    // Turned a quarter around, the muzzle that pointed ahead (+X) points along +Y.
    g.ent_mut(e).unwrap().angles = [0.0, 90.0, 0.0];
    let m = g.world_tag(e, "TAG_FLASH").expect("case-insensitive");
    assert!(near(
        m[3],
        [100.0 - flash[1], 200.0 + flash[0], 300.0 + flash[2]]
    ));
    assert!(g.world_tag(e, "tag_nonexistent").is_none());
    assert!(g.world_tag(e, "").is_none());
}

#[test]
fn a_model_less_entity_has_no_tags_and_parts_are_named_in_bone_order() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let g = &mut s.game;
    let bare = g.spawn(Ent::new(EntKind::Plain, "script_origin")).unwrap();
    assert!(!g.dobj_exists(bare));
    assert!(g.world_tag(bare, "tag_origin").is_none());
    let body = g.content.skeleton("body_mp_usmc_assault").expect("model");
    assert_eq!(body.name(0), Some("tag_origin"));
    assert_eq!(body.bone_index("J_HEAD"), Some(35));
}

#[test]
fn tags_of_attached_models_are_found_through_the_attach_tag() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let g = &mut s.game;
    let e = model_ent(g, "body_mp_usmc_assault", [0.0; 3], [0.0; 3]);
    assert!(g.world_tag(e, "tag_flash").is_none());
    assert!(g.attach_model(e, "weapon_ak47", "tag_weapon_right", false));
    let hand = g.local_tag(e, "tag_weapon_right").expect("hand")[3];
    let flash = g
        .content
        .skeleton("weapon_ak47")
        .unwrap()
        .tag("tag_flash")
        .unwrap()[3];
    let m = g.world_tag(e, "tag_flash").expect("through the attachment");
    // The muzzle sits in front of the hand (the hand bone's axes are turned, so only the
    // distance is stable).
    let d = |a: [f32; 3], b: [f32; 3]| {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    };
    let len = (flash[0].powi(2) + flash[1].powi(2) + flash[2].powi(2)).sqrt();
    assert!(
        (d(m[3], hand) - len).abs() < 0.1,
        "{:?} {:?} {len}",
        m[3],
        hand
    );
    assert!(g.detach_model(e, "weapon_ak47", "tag_weapon_right"));
    assert!(g.world_tag(e, "tag_flash").is_none());
}

#[test]
fn spawn_points_drop_to_the_floor_with_the_player_hull() {
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let g = &mut s.game;
    let clip = g.content.clipmap().expect("clipmap").clone();
    let text = &clip.map_ents.as_ref().expect("entities").entity_string;
    let spawns = server::game::parse_spawn_vars(text).unwrap();
    let spawn = spawns
        .iter()
        .find(|v| server::game::spawn_var(v, "classname") == Some("mp_tdm_spawn"))
        .expect("a tdm spawn");
    let origin = server::game::parse_vec3(server::game::spawn_var(spawn, "origin").unwrap());
    let mut e = Ent::new(EntKind::Plain, "script_origin");
    e.origin = [origin[0], origin[1], origin[2] + 60.0];
    let n = g.spawn(e).unwrap();
    g.place_spawn_point(n);
    let placed = g.ent(n).unwrap().origin;
    assert_eq!([placed[0], placed[1]], [origin[0], origin[1]]);
    use sim::cm::Collide;
    let w = g.world.as_ref().expect("world");
    let (mins, maxs) = ([-15.0, -15.0, 0.0], [15.0, 15.0, 70.0]);
    let mask = sim::contents::MASK_PLAYERSOLID;
    let below = [placed[0], placed[1], placed[2] - 2.0];
    assert!(!w.trace(placed, placed, mins, maxs, n, mask).all_solid);
    assert!(
        w.trace(placed, below, mins, maxs, n, mask).fraction < 1.0,
        "not standing on anything at {placed:?}"
    );
    assert!(placed[2] <= origin[2] + 60.0 + 128.0);
}

mod scripted {
    use super::*;
    use gsc::{Builtins, CallOutcome, Key, Options, Value, Vm, compile};
    use server::script::{Dispatch, ScriptHost};

    macro_rules! same {
        ($a:expr, $b:expr) => {
            assert_eq!(format!("{:?}", $a), format!("{:?}", $b))
        };
    }

    const SCRIPT: &str = r#"
t_parts() { return getpartname("weapon_ak47", 3); }
t_parts_count() { return getnumparts("weapon_ak47"); }
t_parts_bad() { getpartname("weapon_ak47", 4); }
t_tag()
{
	a = spawn("script_model", (0, 0, 0));
	a setmodel("weapon_ak47");
	return a gettagorigin("tag_flash");
}
t_tag_missing()
{
	a = spawn("script_model", (0, 0, 0));
	a setmodel("weapon_ak47");
	a gettagorigin("tag_nope");
}
t_tag_no_model()
{
	a = spawn("script_origin", (0, 0, 0));
	a gettagorigin("tag_flash");
}
t_attach()
{
	a = spawn("script_model", (0, 0, 0));
	a setmodel("body_mp_usmc_assault");
	a attach("weapon_ak47", "tag_weapon_right", true);
	r = [];
	r[0] = a getattachsize();
	r[1] = a getattachmodelname(0);
	r[2] = a getattachtagname(0);
	r[3] = a getattachignorecollision(0);
	a detach("weapon_ak47", "tag_weapon_right");
	r[4] = a getattachsize();
	return r;
}
t_attach_twice()
{
	a = spawn("script_model", (0, 0, 0));
	a setmodel("body_mp_usmc_assault");
	a attach("weapon_ak47", "tag_weapon_right");
	a attach("weapon_ak47", "tag_weapon_right");
}
t_detach_missing()
{
	a = spawn("script_model", (0, 0, 0));
	a setmodel("body_mp_usmc_assault");
	a detach("weapon_ak47", "tag_weapon_right");
}
t_bad_index()
{
	a = spawn("script_model", (0, 0, 0));
	a getattachmodelname(0);
}
t_hide()
{
	a = spawn("script_model", (0, 0, 0));
	a setmodel("weapon_ak47");
	a hidepart("tag_clip");
	a showpart("tag_clip");
	a showallparts();
	a hidepart("tag_nope");
}
t_link()
{
	p = spawn("script_model", (0, 0, 0));
	p setmodel("weapon_ak47");
	c = spawn("script_origin", (1, 2, 3));
	c linkto(p, "tag_flash", (0, 0, 0), (0, 0, 0));
	r = [];
	r[0] = p;
	r[1] = c;
	return r;
}
t_link_bad_tag()
{
	p = spawn("script_model", (0, 0, 0));
	p setmodel("weapon_ak47");
	c = spawn("script_origin", (1, 2, 3));
	c linkto(p, "tag_nope");
}
t_link_unsupported()
{
	p = spawn("script_model", (0, 0, 0));
	t = spawn("trigger_radius", (0, 0, 0), 0, 50, 100);
	t linkto(p);
}
t_touch()
{
	trig = spawn("trigger_radius", (0, 0, 0), 0, 50, 100);
	o1 = spawn("script_origin", (10, 0, 50));
	o2 = spawn("script_origin", (200, 0, 50));
	r = [];
	r[0] = o1 istouching(trig);
	r[1] = o2 istouching(trig);
	r[2] = trig istouching(o1);
	return r;
}
t_touch_two_shapes()
{
	a = spawn("trigger_radius", (0, 0, 0), 0, 50, 100);
	b = spawn("trigger_radius", (0, 0, 0), 0, 50, 100);
	a istouching(b);
}
t_hint_wrong_class()
{
	a = spawn("script_origin", (0, 0, 0));
	a sethintstring("x");
}
t_combine() { return combineangles((0, 90, 0), (0, 90, 0)); }
t_local() 
{
	a = spawn("script_origin", (10, 0, 0));
	a.angles = (0, 90, 0);
	return a localtoworldcoords((5, 0, 0));
}
t_entbynum()
{
	a = spawn("script_origin", (0, 0, 0));
	r = [];
	r[0] = getentbynum(a getentitynumber()) == a;
	r[1] = isdefined(getentbynum(1000));
	r[2] = isdefined(getentbynum(5000));
	return r;
}
"#;

    fn run(s: &mut Server, name: &str) -> Result<Value, String> {
        let prog = compile(
            &[("t.gsc", SCRIPT)],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .map_err(|e| format!("{e:?}"))?;
        let f = prog.find("t", name).ok_or("no such function")?;
        let dispatch = Dispatch::new(&prog);
        let mut vm = Vm::new(prog).map_err(|e| e.to_string())?;
        let mut host = ScriptHost {
            game: &mut s.game,
            dispatch: &dispatch,
        };
        match vm.call(&mut host, f, None, &[]) {
            Ok(CallOutcome::Finished(v)) => Ok(v),
            Ok(CallOutcome::Pending) => Err("waited".into()),
            Err(e) => Err(e.message),
        }
    }

    fn at(v: &Value, i: i32) -> Value {
        match v {
            Value::Array(a) => a.get(&Key::Int(i)).cloned().unwrap_or(Value::Undefined),
            _ => panic!("not an array: {v:?}"),
        }
    }

    fn err(s: &mut Server, name: &str) -> String {
        run(s, name).expect_err(name)
    }

    #[test]
    fn part_queries_use_the_model_bones() {
        let Some(mut s) = boot() else { return };
        same!(run(&mut s, "t_parts").unwrap(), Value::str("tag_flash"));
        same!(run(&mut s, "t_parts_count").unwrap(), Value::Int(4));
        assert_eq!(err(&mut s, "t_parts_bad"), "index out of range (0 - 3)");
    }

    #[test]
    fn tag_origin_and_its_errors() {
        let Some(mut s) = boot() else { return };
        let Value::Vector(v) = run(&mut s, "t_tag").unwrap() else {
            panic!("vector expected")
        };
        assert!((v[0] - 12.696_575).abs() < 1e-3, "{v:?}");
        assert_eq!(
            err(&mut s, "t_tag_missing"),
            "tag 'tag_nope' does not exist in model 'weapon_ak47' (or any attached submodels)"
        );
        assert_eq!(
            err(&mut s, "t_tag_no_model"),
            "entity has no model defined (classname 'script_origin')"
        );
    }

    #[test]
    fn attachments_round_trip_with_the_originals_errors() {
        let Some(mut s) = boot() else { return };
        let r = run(&mut s, "t_attach").unwrap();
        same!(at(&r, 0), Value::Int(1));
        same!(at(&r, 1), Value::str("weapon_ak47"));
        same!(at(&r, 2), Value::str("tag_weapon_right"));
        same!(at(&r, 3), Value::Int(1));
        same!(at(&r, 4), Value::Int(0));
        assert_eq!(
            err(&mut s, "t_attach_twice"),
            "model 'weapon_ak47' already attached to tag 'tag_weapon_right'"
        );
        assert_eq!(
            err(&mut s, "t_detach_missing"),
            "failed to detach model 'weapon_ak47' from tag 'tag_weapon_right'"
        );
        assert_eq!(err(&mut s, "t_bad_index"), "bad index");
    }

    #[test]
    fn hiding_a_part_needs_the_part_to_exist() {
        let Some(mut s) = boot() else { return };
        assert_eq!(
            err(&mut s, "t_hide"),
            "cannot find part 'tag_nope' in entity model"
        );
    }

    #[test]
    fn linking_follows_the_parents_tag_next_frame() {
        let Some(mut s) = boot() else { return };
        let r = run(&mut s, "t_link").unwrap();
        let (Value::Object(p), Value::Object(c)) = (at(&r, 0), at(&r, 1)) else {
            panic!("entities expected")
        };
        let (p, c) = (p.entity().unwrap().num, c.entity().unwrap().num);
        assert!(s.game.is_linked(c));
        let prog = compile(
            &[("t.gsc", "main() {}")],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let mut vm = Vm::new(prog).unwrap();
        {
            let parent = s.game.ent_mut(p).unwrap();
            parent.origin = [100.0, 0.0, 0.0];
            parent.angles = [0.0, 90.0, 0.0];
        }
        s.game.run_mover(&mut vm, c);
        let o = s.game.ent(c).unwrap().origin;
        // tag_flash is 12.7 units ahead of the gun, which now faces +Y.
        assert!(near(o, [100.0, 12.696_575, -0.880_493]), "{o:?}");
    }

    #[test]
    fn link_failures_name_the_cause() {
        let Some(mut s) = boot() else { return };
        assert_eq!(
            err(&mut s, "t_link_bad_tag"),
            "failed to link entity since tag 'tag_nope' does not exist in parent model 'weapon_ak47'"
        );
        assert_eq!(
            err(&mut s, "t_link_unsupported"),
            "entity (classname: 'trigger_radius') does not currently support linkTo"
        );
    }

    #[test]
    fn touch_tests_use_the_shape_of_the_trigger() {
        let Some(mut s) = boot() else { return };
        let r = run(&mut s, "t_touch").unwrap();
        same!(
            (at(&r, 0), at(&r, 1), at(&r, 2)),
            (Value::Int(1), Value::Int(0), Value::Int(1))
        );
        assert_eq!(
            err(&mut s, "t_touch_two_shapes"),
            "istouching cannot be called on 2 brush/cylinder entities"
        );
    }

    #[test]
    fn trigger_setters_reject_other_classes() {
        let Some(mut s) = boot() else { return };
        assert!(
            err(&mut s, "t_hint_wrong_class").starts_with("The setHintString command only works")
        );
    }

    #[test]
    fn angle_and_coordinate_helpers() {
        let Some(mut s) = boot() else { return };
        let Value::Vector(a) = run(&mut s, "t_combine").unwrap() else {
            panic!()
        };
        assert!((a[1] - 180.0).abs() < 1e-3 && a[0].abs() < 1e-3, "{a:?}");
        let Value::Vector(w) = run(&mut s, "t_local").unwrap() else {
            panic!()
        };
        assert!(
            (w[0] - 10.0).abs() < 1e-3 && (w[1] - 5.0).abs() < 1e-3,
            "{w:?}"
        );
        let r = run(&mut s, "t_entbynum").unwrap();
        same!(
            (at(&r, 0), at(&r, 1), at(&r, 2)),
            (Value::Int(1), Value::Int(0), Value::Int(0))
        );
    }
}

#[test]
fn a_cloned_player_falls_to_the_floor_and_becomes_a_corpse() {
    use gsc::{Builtins, Options, Vm, compile};
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    let mut vm = Vm::new(prog).unwrap();
    let g = &mut s.game;
    let clip = g.content.clipmap().expect("clipmap").clone();
    let text = &clip.map_ents.as_ref().expect("entities").entity_string;
    let spawns = server::game::parse_spawn_vars(text).unwrap();
    let spawn = spawns
        .iter()
        .find(|v| server::game::spawn_var(v, "classname") == Some("mp_tdm_spawn"))
        .expect("a tdm spawn");
    let origin = server::game::parse_vec3(server::game::spawn_var(spawn, "origin").unwrap());
    let n = g.connect_client(&mut vm, true, "victim").expect("slot");
    g.client_mut(n).unwrap().ps.origin = [origin[0], origin[1], origin[2] + 40.0];
    g.ent_mut(n).unwrap().model = "body_mp_usmc_assault".into();
    g.ent_mut(n).unwrap().mins = [-15.0, -15.0, 0.0];
    g.ent_mut(n).unwrap().maxs = [15.0, 15.0, 70.0];
    g.level.time = 0;
    let body = g
        .clone_player(&mut vm, n, 5000, "pb_death_run_onfront")
        .expect("corpse");
    assert_eq!(usize::from(body), server::link::CORPSE_BASE);
    assert_eq!(&*g.ent(body).unwrap().model, "body_mp_usmc_assault");
    g.level.frametime = 50;
    for t in 1..=100 {
        g.level.time = t * 50;
        g.run_mover(&mut vm, body);
    }
    let c = g.ent(body).unwrap().x.corpse.as_ref().unwrap();
    assert!(!c.falling, "the body should have landed");
    let z = g.ent(body).unwrap().origin[2];
    assert!(z < origin[2] + 40.0, "it fell: {z}");
    // After the clone time the body is only a corpse for bullets, no longer an actor.
    assert_eq!(g.ent(body).unwrap().contents, sim::contents::CORPSE);
    // The ring hands out the next slot, and wraps after eight bodies.
    let second = g
        .clone_player(&mut vm, n, 0, "pb_death_run_onfront")
        .unwrap();
    assert_eq!(usize::from(second), server::link::CORPSE_BASE + 1);
}

#[test]
fn a_linked_player_is_carried_by_its_parent_and_leaves_in_place() {
    use gsc::{Builtins, Options, Vm, compile};
    use sim::pm::PmType;
    let Some(mut s) = boot() else {
        eprintln!("COD4_PATH not set; skipping");
        return;
    };
    let prog = compile(
        &[("t.gsc", "main() {}")],
        &Builtins::stock_mp(),
        Options::default(),
    )
    .unwrap();
    let mut vm = Vm::new(prog).unwrap();
    let g = &mut s.game;
    let n = g.connect_client(&mut vm, true, "rider").expect("slot");
    g.client_mut(n).unwrap().session = server::client::Session::Playing;
    g.ent_mut(n).unwrap().health = 100;
    g.client_mut(n).unwrap().ps.origin = [10.0, 0.0, 0.0];
    g.ent_mut(n).unwrap().origin = [10.0, 0.0, 0.0];
    let mut p = Ent::new(EntKind::Plain, "script_origin");
    p.flags |= server::link::FL_SUPPORTS_LINKTO;
    let parent = g.spawn(p).unwrap();
    g.link_to(n, parent, None, None).unwrap();
    g.client_end_frame(&mut vm, n);
    assert_eq!(g.client(n).unwrap().ps.pm_type, PmType::NormalLinked);
    g.ent_mut(parent).unwrap().origin = [0.0, 0.0, 500.0];
    g.run_mover(&mut vm, n);
    assert!(near(g.client(n).unwrap().ps.origin, [10.0, 0.0, 500.0]));
    assert!(near(g.ent(n).unwrap().origin, [10.0, 0.0, 500.0]));
    g.unlink(n);
    g.client_end_frame(&mut vm, n);
    assert_eq!(g.client(n).unwrap().ps.pm_type, PmType::Normal);
    assert!(near(g.client(n).unwrap().ps.origin, [10.0, 0.0, 500.0]));
}
