// SPDX-License-Identifier: GPL-3.0-only
//! `net-info`: the `getinfo` answer a server browser lists a server by, against the original's `SVC_Info` (KisakCOD
//! `server_mp/sv_main_mp.cpp:488-634`, CoD4 MP 1.8; 1.7 sends the same keys). Servers of each kind are booted one
//! after another and asked over UDP, as the join menu asks: a pure dedicated server, a listen server (booted with the
//! client's listen command line), an impure dedicated one, an impure one running a mod with an IWD of its own, a pure
//! one with an extra IWD in `main`, and a pure one running a custom map with its IWD. Each answer must start with the
//! challenge, carry only `SVC_Info`'s keys in its order, and give `hw` (2 dedicated, 6 listen), `pure` (only with
//! `sv_pure`) and `mod` (unless pure with only `main`'s stock IWDs) as the original does.
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

/// The keys `SVC_Info` sets, in its order (`challenge` first).
const ORDER: [&str; 20] = [
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
    let mut r = if failures.is_empty() {
        StageReport::new(NAME, Status::Passed)
    } else {
        StageReport::new(NAME, Status::Failed).with_reason(failures.join("; "))
    };
    r.notes = notes;
    Ok(r)
}
