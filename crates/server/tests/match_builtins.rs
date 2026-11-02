// SPDX-License-Identifier: GPL-3.0-only
//! The score, name, laser, aim assist, turret arc, clan and vibrate builtins driven through GSC (KisakCOD
//! `game_mp/g_scr_main_mp.cpp` and `game/g_scr_mover.cpp`; issues #410 and #411).

use gsc::{Builtins, CallOutcome, EntClass, Options, Value, Vm, compile};
use net::ui::ServerCmd;
use server::content::Content;
use server::cvar::Cvars;
use server::game::Game;
use server::script::{Dispatch, ScriptHost};
use server::ui::Dest;
use sim::traj::TrType;

const SCRIPT: &str = r#"
t_spawn() { return spawn("script_model", (0, 0, 0)); }
t_laser(on) { if (on) self laseron(); else self laseroff(); }
t_aim(on) { if (on) self enableaimassist(); else self disableaimassist(); }
t_arcs() { self setrightarc(30); self setleftarc(40); self settoparc(-5); self setbottomarc(20); }
t_arc() { self setleftarc(10); }
t_vibrate() { self vibrate((0, 1, 0), 10, 2, 3); }
t_vibrate_short() { self vibrate((0, 1, 0), 10, 2); }
t_name_mode(m) { setclientnamemode(m); }
t_update_names() { updateclientnames(); }
t_clanid() { return self getclanid(); }
t_dm() { self updatedmscores(); }
"#;

struct Rig {
    g: Game,
    vm: Vm,
    dispatch: Dispatch,
    prog_names: gsc::Program,
}

impl Rig {
    fn new() -> Self {
        let prog = compile(
            &[("t.gsc", SCRIPT)],
            &Builtins::stock_mp(),
            Options::default(),
        )
        .unwrap();
        let dispatch = Dispatch::new(&prog);
        let vm = Vm::new(prog.clone()).unwrap();
        let mut g = Game::new(Cvars::new(), Content::default());
        g.reset_level(8);
        Self {
            g,
            vm,
            dispatch,
            prog_names: prog,
        }
    }

    fn call(&mut self, name: &str, this: Option<u16>, args: &[Value]) -> Result<Value, String> {
        let f = self.prog_names.find("t", name).unwrap();
        let this = this.map(|n| self.vm.entity(n, EntClass::Entity));
        let mut host = ScriptHost {
            game: &mut self.g,
            dispatch: &self.dispatch,
        };
        match self.vm.call(&mut host, f, this, args) {
            Ok(CallOutcome::Finished(v)) => Ok(v),
            Ok(CallOutcome::Pending) => Err("waited".into()),
            Err(e) => Err(e.message),
        }
    }

    fn spawn(&mut self) -> u16 {
        let v = self.call("t_spawn", None, &[]).unwrap();
        v.entity().expect("an entity").num
    }

    fn player(&mut self, name: &str, score: i32) -> u16 {
        let n = self
            .g
            .connect_client(&mut self.vm, true, name)
            .expect("slot");
        self.g.client_begin(&mut self.vm, n);
        self.g.client_mut(n).unwrap().score = score;
        n
    }
}

fn err(r: Result<Value, String>) -> String {
    r.expect_err("the call should fail")
}

#[test]
fn laseron_and_laseroff_flag_the_entity_and_need_an_entity() {
    let mut r = Rig::new();
    let n = r.spawn();
    r.call("t_laser", Some(n), &[Value::Int(1)]).unwrap();
    assert!(r.g.ent(n).unwrap().x.laser);
    r.call("t_laser", Some(n), &[Value::Int(0)]).unwrap();
    assert!(!r.g.ent(n).unwrap().x.laser);
}

#[test]
fn aim_assist_is_only_for_brush_models() {
    let mut r = Rig::new();
    let n = r.spawn();
    assert!(
        err(r.call("t_aim", Some(n), &[Value::Int(1)]))
            .contains("Currently only supported on entities with brush models")
    );
    r.g.ent_mut(n).unwrap().brush_model = Some(1);
    r.call("t_aim", Some(n), &[Value::Int(1)]).unwrap();
    assert!(r.g.ent(n).unwrap().x.aim_assist);
    r.call("t_aim", Some(n), &[Value::Int(0)]).unwrap();
    assert!(!r.g.ent(n).unwrap().x.aim_assist);
}

#[test]
fn the_arcs_set_a_turrets_limits_and_never_cross_its_front() {
    let mut r = Rig::new();
    let n = r.spawn();
    assert!(err(r.call("t_arc", Some(n), &[])).contains("entity is not a turret"));
    r.g.ent_mut(n).unwrap().turret = Some(Box::new(server::turret::Turret {
        weapon: 0,
        arc_min: [-1.0, -1.0],
        arc_max: [1.0, 1.0],
        stance: 0,
        player_spread: 0.0,
        use_hint: None,
        drop_hint: None,
        gunner: None,
        user_origin: [0.0; 3],
        prev_stance: None,
        fire_time: 0,
        trigger_down: false,
        gun_angles: [0.0; 3],
    }));
    r.call("t_arcs", Some(n), &[]).unwrap();
    let t = r.g.ent(n).unwrap().turret.as_deref().unwrap();
    // Right 30 and left 40 are the yaw limits; a top arc of -5 would cross the front and stops at it.
    assert_eq!(t.arc_min, [0.0, -30.0]);
    assert_eq!(t.arc_max, [20.0, 40.0]);
}

#[test]
fn vibrate_swings_the_angles_on_a_sine_and_wants_four_arguments() {
    let mut r = Rig::new();
    let n = r.spawn();
    assert!(err(r.call("t_vibrate_short", Some(n), &[])).contains("illegal call to vibrate()"));
    r.g.level.time = 1000;
    r.call("t_vibrate", Some(n), &[]).unwrap();
    let tr = r.g.ent(n).unwrap().mv.ang.tr;
    assert_eq!(tr.kind, TrType::Sine);
    assert_eq!(tr.time, 4000);
    assert_eq!(tr.duration, 2000);
    // Facing +x, an impulse along +y is all to the left: minus the roll.
    assert!(tr.delta[0].abs() < 1e-4, "{:?}", tr.delta);
    assert!((tr.delta[2] - (-10.0)).abs() < 1e-4, "{:?}", tr.delta);
}

#[test]
fn names_change_only_on_request_in_manual_mode() {
    let mut r = Rig::new();
    assert!(
        err(r.call("t_update_names", None, &[])).contains("Only works in [manual_change] mode")
    );
    assert!(err(r.call("t_name_mode", None, &[Value::str("sideways")])).contains("Unknown mode"));
    let n = r.player("old", 0);
    r.call("t_name_mode", None, &[Value::str("manual_change")])
        .unwrap();
    assert!(r.g.level.manual_name_change);
    r.g.client_mut(n).unwrap().new_name = "new".into();
    assert_eq!(r.g.client(n).unwrap().name, "old");
    r.call("t_update_names", None, &[]).unwrap();
    assert_eq!(r.g.client(n).unwrap().name, "new");
    r.call("t_name_mode", None, &[Value::str("auto_change")])
        .unwrap();
    assert!(!r.g.level.manual_name_change);
}

#[test]
fn getclanid_is_the_string_zero() {
    let mut r = Rig::new();
    let n = r.player("p", 0);
    match r.call("t_clanid", Some(n), &[]).unwrap() {
        Value::Str(s) => assert_eq!(&*s, "0"),
        v => panic!("not a string: {v:?}"),
    }
}

#[test]
fn updatedmscores_sends_the_player_their_own_score_and_the_best_other() {
    let mut r = Rig::new();
    let me = r.player("me", 3);
    let best = r.player("best", 9);
    r.player("worst", 1);
    r.g.ui.out.clear();
    r.call("t_dm", Some(me), &[]).unwrap();
    let sent: Vec<_> =
        r.g.ui
            .out
            .iter()
            .filter_map(|o| match o.cmd {
                ServerCmd::ClientScore { client, score } => Some((o.to, client, score)),
                _ => None,
            })
            .collect();
    assert_eq!(
        sent,
        [(Dest::Client(me), me, 3), (Dest::Client(me), best, 9)]
    );
}
