// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Hand-built zip: (name, data, deflate?). Duplicated names stay duplicated.
fn zip(entries: &[(&str, &[u8], bool)], comment: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut cd = vec![];
    for (name, data, deflate) in entries {
        let raw = if *deflate {
            miniz_oxide::deflate::compress_to_vec(data, 6)
        } else {
            data.to_vec()
        };
        let method: u16 = if *deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend([20, 0, 0, 0]);
        out.extend(method.to_le_bytes());
        out.extend([0; 8]); // time, date, crc
        out.extend((raw.len() as u32).to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(3u16.to_le_bytes()); // local extra differs from central's
        out.extend(name.as_bytes());
        out.extend([0xee; 3]);
        out.extend(&raw);

        cd.extend(0x0201_4b50u32.to_le_bytes());
        cd.extend([20, 0, 20, 0, 0, 0]);
        cd.extend(method.to_le_bytes());
        cd.extend([0; 8]);
        cd.extend((raw.len() as u32).to_le_bytes());
        cd.extend((data.len() as u32).to_le_bytes());
        cd.extend((name.len() as u16).to_le_bytes());
        cd.extend([0; 12]);
        cd.extend(offset.to_le_bytes());
        cd.extend(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend(&cd);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((cd.len() as u32).to_le_bytes());
    out.extend(cd_off.to_le_bytes());
    out.extend((comment.len() as u16).to_le_bytes());
    out.extend(comment);
    out
}

fn open(entries: &[(&str, &[u8], bool)]) -> Iwd {
    Iwd::new(Box::new(zip(entries, b""))).unwrap()
}

fn read(iwd: &Iwd, name: &str) -> Option<Vec<u8>> {
    iwd.find(name).map(|e| iwd.read(e).unwrap())
}

#[test]
fn reads_stored_and_deflate_case_insensitive_with_backslashes() {
    let big = vec![b'x'; 5000];
    let iwd = open(&[("images/A.iwi", &big, true), ("sound/b.wav", b"raw", false)]);
    assert_eq!(read(&iwd, "IMAGES\\a.IWI").unwrap(), big);
    assert_eq!(read(&iwd, "Sound/B.wav").unwrap(), b"raw");
    assert!(read(&iwd, "nope").is_none());
}

#[test]
fn last_duplicate_wins_within_an_iwd() {
    let iwd = open(&[
        ("a.cfg", b"first", false),
        ("A.CFG", b"second", true),
        ("b.cfg", b"b", false),
    ]);
    assert_eq!(iwd.entries().len(), 3);
    assert_eq!(read(&iwd, "a.cfg").unwrap(), b"second");
}

#[test]
fn finds_eocd_behind_a_comment() {
    let iwd = Iwd::new(Box::new(zip(&[("a", b"1", false)], &[b'P'; 300]))).unwrap();
    assert_eq!(read(&iwd, "a").unwrap(), b"1");
}

#[test]
fn rejects_garbage_and_truncation() {
    assert!(Iwd::new(Box::new(vec![0u8; 100])).is_err());
    assert!(Iwd::new(Box::new(vec![])).is_err());
    let mut z = zip(&[("a", b"hello", false)], b"");
    let iwd_ok = Iwd::new(Box::new(z.clone())).unwrap();
    let e = iwd_ok.find("a").unwrap().clone();
    z.truncate(e.header_offset as usize + 32); // cut inside entry data, central dir gone
    assert!(Iwd::new(Box::new(z)).is_err());
}

#[test]
fn rejects_size_mismatch() {
    let mut z = zip(&[("a", b"hello", false)], b"");
    // central directory claims 6 bytes uncompressed for 5 stored bytes
    let cd = z.len() - 22 - (46 + 1);
    z[cd + 24] = 6;
    let iwd = Iwd::new(Box::new(z)).unwrap();
    assert!(iwd.read(iwd.find("a").unwrap()).is_err());
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cod4e-vfs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn file(&self, rel: &str, bytes: &[u8]) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn iwd(&self, rel: &str, files: &[(&str, &[u8])]) {
        let e: Vec<_> = files.iter().map(|(n, d)| (*n, *d, true)).collect();
        self.file(rel, &zip(&e, b""));
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn names(v: &Vfs) -> Vec<String> {
    v.nodes()
        .iter()
        .map(|n| match &n.kind {
            NodeKind::Dir(p) => p
                .strip_prefix(p.ancestors().nth(2).unwrap())
                .unwrap()
                .display()
                .to_string(),
            NodeKind::Iwd { path, .. } => path.file_name().unwrap().to_string_lossy().into_owned(),
        })
        .collect()
}

fn get(v: &Vfs, n: &str) -> String {
    String::from_utf8(v.read(n).unwrap().unwrap()).unwrap()
}

#[test]
fn later_sorted_iwd_wins_and_main_rejects_non_iw() {
    let t = Tmp::new();
    t.iwd("main/iw_00.iwd", &[("a.cfg", b"00"), ("only00", b"x")]);
    t.iwd("main/IW_13.iwd", &[("A.cfg", b"13")]);
    t.iwd("main/zz_patch.iwd", &[("a.cfg", b"zz"), ("zz_only", b"z")]);
    let v = Vfs::open_stock(&t.0, 0).unwrap();
    assert_eq!(get(&v, "a.cfg"), "13");
    assert_eq!(get(&v, "ONLY00"), "x");
    assert!(v.read("zz_only").unwrap().is_none());
}

#[test]
fn main_without_iwds_is_an_error() {
    let t = Tmp::new();
    t.iwd("main/zz.iwd", &[("a", b"")]);
    assert!(Vfs::open_stock(&t.0, 0).is_err());
}

#[test]
fn localized_nodes_lose_to_non_localized_and_filter_by_language() {
    let t = Tmp::new();
    t.iwd("main/iw_00.iwd", &[("both", b"plain")]);
    t.iwd(
        "main/localized_english_iw00.iwd",
        &[("both", b"en"), ("loc", b"en")],
    );
    t.iwd(
        "main/localized_french_iw00.iwd",
        &[("both", b"fr"), ("loc", b"fr")],
    );
    let en = Vfs::open_stock(&t.0, 0).unwrap();
    assert_eq!(get(&en, "both"), "plain");
    assert_eq!(get(&en, "loc"), "en");
    let fr = Vfs::open_stock(&t.0, 1).unwrap();
    assert_eq!(get(&fr, "loc"), "fr");
    assert_eq!(
        names(&fr).last().map(String::as_str),
        Some("localized_english_iw00.iwd")
    );
}

#[test]
fn localized_iwds_sort_english_first_then_name() {
    let t = Tmp::new();
    t.iwd("main/iw_00.iwd", &[]);
    for n in [
        "localized_english_iw01",
        "localized_english_iw00",
        "localized_german_iw00",
    ] {
        t.iwd(&format!("main/{n}.iwd"), &[]);
    }
    let v = Vfs::open_stock(&t.0, 0).unwrap();
    let loc: Vec<_> = names(&v)
        .into_iter()
        .filter(|n| n.starts_with("localized"))
        .collect();
    // highest priority first: non-English localized beats English; later name beats earlier
    assert_eq!(
        loc,
        [
            "localized_german_iw00.iwd",
            "localized_english_iw01.iwd",
            "localized_english_iw00.iwd"
        ]
    );
}

#[test]
fn directory_priority_and_loose_files() {
    let t = Tmp::new();
    t.iwd("main/iw_00.iwd", &[("f", b"main-iwd"), ("g", b"g")]);
    t.file("main/F", b"main-loose");
    t.file("main_shared/h", b"shared");
    t.file("players/Profiles/active.txt", b"me");
    let v = Vfs::open_stock(&t.0, 0).unwrap();
    // IWDs come before the loose dir of the same game directory
    assert_eq!(get(&v, "f"), "main-iwd");
    assert_eq!(get(&v, "h"), "shared");
    assert_eq!(get(&v, "profiles\\ACTIVE.txt"), "me");
    assert!(v.read("missing").unwrap().is_none());
}

#[test]
fn later_game_dir_overrides_earlier() {
    let t = Tmp::new();
    t.iwd("main/iw_00.iwd", &[("x", b"main")]);
    t.iwd("mods/m/zz_a.iwd", &[("x", b"mod")]);
    let mut b = Builder::new(&t.0);
    b.add_game_dir("main").unwrap();
    b.add_game_dir("mods/m").unwrap();
    assert_eq!(get(&b.finish(0).unwrap(), "x"), "mod");
}

#[test]
fn file_source_ranged_reads() {
    let t = Tmp::new();
    t.file("f", b"0123456789");
    let s = FileSource::open(&t.0.join("f")).unwrap();
    let mut b = [0; 4];
    s.read_exact_at(3, &mut b).unwrap();
    assert_eq!(&b, b"3456");
    assert!(s.read_exact_at(8, &mut b).is_err());
    let _: &dyn ReadAt = &s;
}

fn install() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("COD4_PATH").unwrap_or_else(|| "./COD4".into()));
    p.join("main").is_dir().then_some(p).or_else(|| {
        eprintln!("skipping: no COD4 install");
        None
    })
}

fn iwd_of<'a>(v: &'a Vfs, name: &str) -> (&'a str, &'a Iwd) {
    let (i, _) = v.locate_node(name).unwrap();
    match &v.nodes()[i].kind {
        NodeKind::Iwd { path, iwd } => (path.file_name().unwrap().to_str().unwrap(), iwd),
        NodeKind::Dir(_) => panic!("expected IWD"),
    }
}

#[test]
fn install_cross_iwd_collisions_resolve_to_the_patch() {
    let Some(root) = install() else { return };
    let v = Vfs::open_stock(&root, 0).unwrap();
    for name in [
        "default_filter.cfg",
        "images/loadscreen_mp_broadcast.iwi",
        "images/compass_map_mp_broadcast.iwi",
    ] {
        assert_eq!(iwd_of(&v, name).0, "iw_13.iwd", "{name}");
    }
    // bit-identical in iw_13 and localized_english_iw00; iw_13 is found first
    assert_eq!(iwd_of(&v, "images/specialty_new.iwi").0, "iw_13.iwd");
}

#[test]
fn install_duplicate_cfgs_in_iw_00_take_the_last_entry() {
    let Some(root) = install() else { return };
    let v = Vfs::open_stock(&root, 0).unwrap();
    let iw00 = v
        .nodes()
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::Iwd { path, iwd } if path.ends_with("iw_00.iwd") => Some(iwd),
            _ => None,
        })
        .unwrap();
    let dups = [
        "avatar_dev",
        "chad",
        "createfx",
        "default_mp_gamesettings",
        "jake",
        "jiesang",
        "massey",
        "robotg",
        "roger",
        "test",
    ];
    for d in dups {
        let name = format!("{d}.cfg");
        let all: Vec<_> = iw00
            .entries()
            .iter()
            .filter(|e| normalize(&e.name) == name)
            .collect();
        assert_eq!(all.len(), 2, "{name}");
        // not shadowed by an earlier IWD for these names, so iw_00 serves them
        let (iwd_name, iwd) = iwd_of(&v, &name);
        assert_eq!(iwd_name, "iw_00.iwd");
        assert_eq!(iwd.find(&name).unwrap(), all[1], "{name}");
    }
}

#[test]
fn install_reads_known_file() {
    let Some(root) = install() else { return };
    let v = Vfs::open_stock(&root, 0).unwrap();
    // startup-required file, and a zero-length stored entry
    assert!(v.read("fileSysCheck.cfg").unwrap().is_some());
    assert_eq!(v.read("language.cfg").unwrap().unwrap().len(), 0);
    let (_, iwd) = iwd_of(&v, "images/loadscreen_mp_broadcast.iwi");
    let e = iwd.find("images/loadscreen_mp_broadcast.iwi").unwrap();
    let bytes = v
        .read("images/loadscreen_mp_broadcast.iwi")
        .unwrap()
        .unwrap();
    assert_eq!(bytes.len(), e.size as usize);
    assert_eq!(&bytes[..3], b"IWi");
    assert!(v.read("DEFAULT_MP.cfg").unwrap().is_some()); // localized english tier
}
