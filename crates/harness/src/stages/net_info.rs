// SPDX-License-Identifier: GPL-3.0-only
//! `net-info`: the `getinfo` answer a server browser lists a server by, against the original's `SVC_Info` (the CoD4
//! MP 1.7 `iw3mp.exe`, `0x531c..0x532163`; KisakCOD's 1.8 `server_mp/sv_main_mp.cpp:488-634` sends the same keys but
//! `hc` and `od`). Servers of each kind are booted one after another and asked over UDP, as the join menu asks: a pure
//! dedicated server, a listen server (booted with the client's listen command line), an impure dedicated one, an
//! impure one running a mod with an IWD of its own, a pure one with an extra IWD in `main`, a pure one running a custom
//! map with its IWD, and a hardcore one. Each answer must start with the challenge, carry only `SVC_Info`'s keys in its
//! order, and give `hw` (2 dedicated, 6 listen), `pure` (only with `sv_pure`), `mod` (unless pure with only `main`'s
//! stock IWDs) and `hc` (`scr_hardcore` when set) as the original does.
//! Then one more dedicated server checks the stock dvar registrations (#398) as a monitor and an operator see them:
//! `getstatus` carries `fs_game` and `sv_floodProtect`, and rcon `set` is held to each dvar's type, range and cheat
//! protection.
use crate::stage::{StageCtx, StageReport, Status};
use net::oob::Oob;
use net::{Transport, UdpTransport};
use server::server::{Server, listen_cmdline};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const NAME: &str = "net-info";
/// How long a server has to answer.
const ASK_LIMIT: Duration = Duration::from_secs(10);
const CHALLENGE: u32 = 434_017;
/// The scratch mod, which holds one IWD.
const MOD: &str = "mods/netinfo";
/// The scratch custom map: the stock crash zone under another name, with an IWD.
const CUSTOM: &str = "mp_netinfo";

/// The keys `SVC_Info` sets, in its order (`challenge` first); `hc` and `od` are the 1.7 `iw3mp.exe`'s (KisakCOD's
/// 1.8 has neither).
const ORDER: [&str; 22] = [
    "challenge",
    "protocol",
    "hostname",
    "mapname",
    "clients",
    "sv_maxclients",
    "gametype",
    "pure",
    "minPing",
    "maxPing",
    "game",
    "sv_allowAnonymous",
    "con_disabled",
    "pswrd",
    "ff",
    "kc",
    "hc",
    "od",
    "hw",
    "mod",
    "voice",
    "pb",
];

struct Case {
    what: &'static str,
    root: PathBuf,
    args: Vec<String>,
    map: &'static str,
    /// Keys and the value each must have; `None`: the key must be absent.
    want: Vec<(&'static str, Option<&'static str>)>,
}

fn set(pairs: &[(&str, &str)]) -> Vec<String> {
    pairs
        .iter()
        .flat_map(|(k, v)| ["+set".to_owned(), (*k).to_owned(), (*v).to_owned()])
        .collect()
}

/// A folder this stage made outside its own (the bundle would follow its links), removed when the stage ends.
struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(windows)]
fn link(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        std::os::windows::fs::symlink_dir(from, to)
    } else {
        std::os::windows::fs::symlink_file(from, to)
    }
}

/// Links every entry of `from` into `to` except those named in `skip`.
fn link_all(from: &Path, to: &Path, skip: &[&str]) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if !skip.iter().any(|s| name.eq_ignore_ascii_case(s)) {
            link(&entry.path(), &to.join(name))?;
        }
    }
    Ok(())
}

/// An IWD (a zip) holding one small text file.
fn write_iwd(path: &Path, file: &str) -> io::Result<()> {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path)?);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file(file, opts).map_err(io::Error::other)?;
    z.write_all(b"net-info\n")?;
    z.finish().map_err(io::Error::other)?;
    Ok(())
}

/// The real install with the mod `MOD` (one IWD) and the custom map `CUSTOM` (the crash zone and an IWD) added.
fn mod_install(real: &Path, dir: &Path) -> io::Result<PathBuf> {
    let root = dir.join("mod");
    link_all(real, &root, &["mods", "usermaps"])?;
    let mod_dir = root.join(MOD);
    std::fs::create_dir_all(&mod_dir)?;
    write_iwd(&mod_dir.join("zz_netinfo.iwd"), "netinfo_mod.txt")?;
    let map_dir = root.join("usermaps").join(CUSTOM);
    std::fs::create_dir_all(&map_dir)?;
    link(
        &real.join("zone/english/mp_crash.ff"),
        &map_dir.join(format!("{CUSTOM}.ff")),
    )?;
    write_iwd(&map_dir.join(format!("{CUSTOM}.iwd")), "netinfo_map.txt")?;
    Ok(root)
}

/// The real install with `main/iw_30.iwd` added: an IWD the loader takes that is not one of the stock `iw_00`..`iw_24`.
fn extra_iwd_install(real: &Path, dir: &Path) -> io::Result<PathBuf> {
    let root = dir.join("extra");
    link_all(real, &root, &["main"])?;
    link_all(&real.join("main"), &root.join("main"), &[])?;
    write_iwd(&root.join("main/iw_30.iwd"), "netinfo_main.txt")?;
    Ok(root)
}

/// Asks `s` for its info as a server browser does and serves it until it answers.
fn ask(s: &mut Server) -> Result<Vec<(String, String)>, String> {
    let port = s.net_addr().ok_or("the server has no UDP socket")?.port();
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    let mut t =
        UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 4096];
    let end = Instant::now() + ASK_LIMIT;
    let mut next = Instant::now();
    while Instant::now() < end {
        if Instant::now() >= next {
            t.send_to(to, &Oob::GetInfo(CHALLENGE).encode());
            next = Instant::now() + Duration::from_millis(500);
        }
        s.run_for(Duration::from_millis(30));
        while let Ok(Some((n, from))) = t.recv_from(&mut buf, None) {
            if from.port() == port
                && let Some(Oob::InfoResponse(kv)) = Oob::parse(&buf[..n])
            {
                return Ok(kv);
            }
        }
    }
    Err(format!("no infoResponse within {ASK_LIMIT:?}"))
}

fn show(kv: &[(String, String)]) -> String {
    kv.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Holds the answer against `SVC_Info`'s key set and order and the case's values.
fn check(kv: &[(String, String)], want: &[(&str, Option<&str>)]) -> Result<(), String> {
    let challenge = CHALLENGE.to_string();
    if kv.first().map(|(k, v)| (k.as_str(), v.as_str())) != Some(("challenge", &challenge)) {
        return Err(format!("the first key is not challenge={challenge}"));
    }
    let mut last = None;
    for (k, _) in kv {
        let Some(at) = ORDER.iter().position(|o| o == k) else {
            return Err(format!("`{k}` is not a key SVC_Info sends"));
        };
        if last.is_some_and(|l| at <= l) {
            return Err(format!("`{k}` is out of SVC_Info's order"));
        }
        last = Some(at);
    }
    for (k, v) in want {
        let got = kv.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if got != *v {
            return Err(format!("`{k}` is {got:?}, the original's is {v:?}"));
        }
    }
    Ok(())
}

fn run_case(c: &Case) -> Result<Vec<(String, String)>, String> {
    let mut s = Server::boot(&c.root, &c.args, false)?;
    let line = format!("map {}", c.map);
    s.exec_line(&line).map_err(|e| format!("{line}: {e}"))?;
    ask(&mut s)
}

/// The rcon password the stock dvar case sets.
const RCON: &str = "netinfo";

/// Sends `cmd` over rcon and serves the server long enough for it to run and answer (and past rcon's 500 ms
/// spacing), returning what it printed back.
fn rcon(s: &mut Server, t: &mut UdpTransport, to: SocketAddr, cmd: &str) -> String {
    let p = Oob::Rcon {
        password: RCON.into(),
        command: cmd.into(),
    };
    t.send_to(to, &p.encode());
    let mut out = String::new();
    let mut buf = vec![0u8; 4096];
    let end = Instant::now() + Duration::from_millis(700);
    while Instant::now() < end {
        s.run_for(Duration::from_millis(30));
        while let Ok(Some((n, _))) = t.recv_from(&mut buf, None) {
            if let Some(Oob::Print(text)) = Oob::parse(&buf[..n]) {
                out.push_str(&text);
            }
        }
    }
    out
}

/// The stock dvar registrations as a monitor and an operator see them (#398): `getstatus` carries `fs_game` and
/// `sv_floodProtect` (SERVERINFO in the 1.7 binary), and over rcon `set sv_reconnectlimit 60` is taken (Int
/// 0..1800), `set sv_voice 7` is stored as the Bool 1, and `set g_mantleBlockTimeBuffer 0` with `sv_cheats 0` (a
/// `map` start) is refused with the stock "is cheat protected." line (KisakCOD `universal/dvar.cpp`).
fn stock_dvars(install: &Path) -> Result<String, String> {
    let mut s = Server::boot(
        install,
        &set(&[("net_port", "0"), ("rcon_password", RCON)]),
        false,
    )?;
    s.exec_line("map mp_crash")
        .map_err(|e| format!("map mp_crash: {e}"))?;
    let port = s.net_addr().ok_or("the server has no UDP socket")?.port();
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    let mut t =
        UdpTransport::bind(SocketAddr::from(([127, 0, 0, 1], 0))).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 4096];
    let end = Instant::now() + ASK_LIMIT;
    let mut next = Instant::now();
    let mut status = None;
    while status.is_none() && Instant::now() < end {
        if Instant::now() >= next {
            t.send_to(to, &Oob::GetStatus(CHALLENGE).encode());
            next = Instant::now() + Duration::from_millis(500);
        }
        s.run_for(Duration::from_millis(30));
        while let Ok(Some((n, from))) = t.recv_from(&mut buf, None) {
            if from.port() == port
                && let Some(Oob::StatusResponse { info, .. }) = Oob::parse(&buf[..n])
            {
                status = Some(info);
            }
        }
    }
    let info = status.ok_or_else(|| format!("no statusResponse within {ASK_LIMIT:?}"))?;
    for key in ["fs_game", "sv_floodProtect"] {
        if !info.iter().any(|(k, _)| k == key) {
            return Err(format!("getstatus has no {key}: {}", show(&info)));
        }
    }
    let value = |s: &Server, n: &str| s.game.cvars.string(n).to_owned();
    if value(&s, "sv_cheats") != "0" {
        return Err(format!(
            "sv_cheats is {:?} after map",
            value(&s, "sv_cheats")
        ));
    }
    let said = rcon(&mut s, &mut t, to, "set sv_reconnectlimit 60");
    if value(&s, "sv_reconnectlimit") != "60" {
        return Err(format!(
            "rcon set sv_reconnectlimit 60 left {:?} ({said:?})",
            value(&s, "sv_reconnectlimit")
        ));
    }
    let said = rcon(&mut s, &mut t, to, "set sv_voice 7");
    if value(&s, "sv_voice") != "1" {
        return Err(format!(
            "rcon set sv_voice 7 stored {:?}, want the Bool 1 ({said:?})",
            value(&s, "sv_voice")
        ));
    }
    let said = rcon(&mut s, &mut t, to, "set g_mantleBlockTimeBuffer 0");
    if !said.contains("g_mantleBlockTimeBuffer is cheat protected.") {
        return Err(format!("set g_mantleBlockTimeBuffer 0 answered {said:?}"));
    }
    if value(&s, "g_mantleBlockTimeBuffer") != "500" {
        return Err(format!(
            "the refused set changed g_mantleBlockTimeBuffer to {:?}",
            value(&s, "g_mantleBlockTimeBuffer")
        ));
    }
    Ok(format!(
        "stock dvars: getstatus fs_game={:?} sv_floodProtect={:?}; rcon sv_reconnectlimit 60, sv_voice 7 -> 1, {}",
        info.iter().find(|(k, _)| k == "fs_game").map(|(_, v)| v),
        info.iter()
            .find(|(k, _)| k == "sv_floodProtect")
            .map(|(_, v)| v),
        said.trim()
    ))
}

pub fn run(ctx: &StageCtx) -> io::Result<StageReport> {
    let Some(install) = ctx.install.as_deref() else {
        return Ok(StageReport::new(NAME, Status::Skipped).with_reason("install not found"));
    };
    if !install.join("zone/english/mp_crash.ff").is_file() {
        return Ok(StageReport::new(NAME, Status::Failed)
            .with_reason("the install has no zone/english/mp_crash.ff for the custom map"));
    }
    let scratch = Tmp(std::env::temp_dir().join(format!("cod4e-netinfo-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let modded = mod_install(install, &scratch.0)?;
    let extra = extra_iwd_install(install, &scratch.0)?;
    let dedicated = |pure: &'static str, more: &[(&'static str, &'static str)]| {
        let mut a = set(&[("net_port", "0"), ("sv_pure", pure)]);
        a.extend(set(more));
        a
    };
    let cases = [
        Case {
            what: "pure dedicated server",
            root: install.to_owned(),
            // `sv_keywords` is in `getstatus`, not `getinfo`.
            args: dedicated("1", &[("sv_keywords", "netinfo")]),
            map: "mp_crash",
            want: vec![
                ("mapname", Some("mp_crash")),
                ("pure", Some("1")),
                ("game", None),
                ("hw", Some("2")),
                ("mod", Some("0")),
                ("hc", None),
                ("od", None),
            ],
        },
        Case {
            what: "listen server",
            root: install.to_owned(),
            args: listen_cmdline(0, ""),
            map: "mp_crash",
            want: vec![("pure", Some("1")), ("hw", Some("6")), ("mod", Some("0"))],
        },
        Case {
            what: "impure dedicated server",
            root: install.to_owned(),
            args: dedicated("0", &[]),
            map: "mp_crash",
            want: vec![("pure", None), ("hw", Some("2")), ("mod", Some("1"))],
        },
        Case {
            what: "impure dedicated server running a mod with an IWD",
            root: modded.clone(),
            args: dedicated("0", &[("fs_game", MOD)]),
            map: "mp_crash",
            // `fs_numServerIwds` is a client's count, so a dedicated server's own IWDs do not make it pure.
            want: vec![
                ("pure", None),
                ("game", Some(MOD)),
                ("hw", Some("2")),
                ("mod", Some("1")),
            ],
        },
        Case {
            what: "pure dedicated server with an extra IWD in main",
            root: extra,
            args: dedicated("1", &[]),
            map: "mp_crash",
            want: vec![("pure", Some("1")), ("mod", Some("1"))],
        },
        Case {
            what: "pure dedicated server running a custom map",
            root: modded,
            args: dedicated("1", &[]),
            map: CUSTOM,
            want: vec![
                ("mapname", Some(CUSTOM)),
                ("pure", Some("1")),
                ("mod", Some("1")),
            ],
        },
        Case {
            what: "pure dedicated hardcore server",
            root: install.to_owned(),
            args: dedicated("1", &[("scr_hardcore", "1")]),
            map: "mp_crash",
            want: vec![("hc", Some("1")), ("od", None), ("mod", Some("0"))],
        },
    ];
    let mut notes = Vec::new();
    let mut failures = Vec::new();
    for c in &cases {
        match run_case(c) {
            Ok(kv) => match check(&kv, &c.want) {
                Ok(()) => notes.push(format!("{}: {}", c.what, show(&kv))),
                Err(e) => failures.push(format!("{}: {e} ({})", c.what, show(&kv))),
            },
            Err(e) => failures.push(format!("{}: {e}", c.what)),
        }
    }
    match stock_dvars(install) {
        Ok(note) => notes.push(note),
        Err(e) => failures.push(format!("stock dvars: {e}")),
    }
    let mut r = if failures.is_empty() {
        StageReport::new(NAME, Status::Passed)
    } else {
        StageReport::new(NAME, Status::Failed).with_reason(failures.join("; "))
    };
    r.notes = notes;
    Ok(r)
}
