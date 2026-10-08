use server::server::Server;
use std::path::PathBuf;
#[test]
fn probe() {
    let Some(root) = std::env::var_os("COD4_PATH") else { return };
    let args: Vec<String> = ["+set", "net_port", "0"].map(String::from).to_vec();
    let mut s = Server::boot(&PathBuf::from(root), &args, false).unwrap();
    for l in ["set g_gametype war","set scr_war_timelimit 0","set scr_war_scorelimit 0","map mp_crash","bots 8"] { s.exec_line(l).unwrap(); }
    s.exec_line("until spawns 8 60").unwrap();
    s.exec_line("devhardpoint 0 helicopter_mp").unwrap();
    s.exec_line("until helicopters 1 200").unwrap();
    for i in 0..40 {
        s.run_frames(20);
        eprintln!("t={}s", i);
        for (n,e,v) in s.game.vehicles() { eprintln!("heli {n} {:?} {:.0}mph ang {:?} {:?} stage {}", e.origin.map(|x| x as i32), v.speed/17.6, e.angles.map(|x| x as i32), v.state, v.stage); }
        eprintln!("shots {} hits {} crashes {}", s.game.stats.heli_shots, s.game.stats.heli_hits, s.game.stats.heli_crashes);
    }
}
