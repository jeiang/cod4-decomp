// SPDX-License-Identifier: GPL-3.0-only
//! Install-gated: the real alias tables, channels and files. Skips when the original install is absent.

use assets::vfs::{LANGUAGES, Vfs};
use audio::bank::{Bank, Clip};
use audio::decode::{self, StreamJob};
use audio::{Cue, Sound};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn root() -> Option<PathBuf> {
    let root = std::env::var_os("COD4_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../COD4")));
    if root.join("zone/english").is_dir() {
        Some(root)
    } else {
        eprintln!("skipping: no original install at {}", root.display());
        None
    }
}

fn bank() -> Option<&'static std::sync::Mutex<Bank>> {
    static B: OnceLock<Option<std::sync::Mutex<Bank>>> = OnceLock::new();
    B.get_or_init(|| {
        let root = root()?;
        let vfs = Arc::new(
            Vfs::open_stock(
                &root,
                LANGUAGES.iter().position(|l| *l == "english").unwrap(),
            )
            .unwrap(),
        );
        let zones: Vec<PathBuf> = ["code_post_gfx_mp", "localized_common_mp", "mp_crossfire"]
            .iter()
            .map(|z| root.join(format!("zone/english/{z}.ff")))
            .collect();
        Some(std::sync::Mutex::new(Bank::load(vfs, &zones).unwrap()))
    })
    .as_ref()
}

#[test]
fn channels_and_aliases_load_and_agree() {
    let Some(b) = bank() else { return };
    let b = b.lock().unwrap();
    let names: Vec<&str> = b.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names.len(), 33);
    assert_eq!(names[18], "weapon");
    assert_eq!(names[28], "music");
    assert_eq!(names[24], "ambient");
    assert!(b.stats.lists > 2500, "{:?}", b.stats);
    assert!(b.stats.loaded > 2000 && b.stats.streamed > 100);
    let mut bad = Vec::new();
    for n in b.names() {
        for a in b.aliases_of(n) {
            if usize::from(a.channel) >= b.channels.len() {
                bad.push(format!("{n}: channel {}", a.channel));
            }
            if a.dist.0 <= 0.0 || a.dist.1 <= a.dist.0 {
                bad.push(format!("{n}: dist {:?}", a.dist));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} bad, e.g. {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
}

#[test]
fn every_streamed_alias_names_a_decodable_file() {
    let Some(b) = bank() else { return };
    let b = b.lock().unwrap();
    let mut paths = std::collections::BTreeSet::new();
    for n in b.names() {
        for a in b.aliases_of(n) {
            if let Clip::Streamed(p) = &a.audio {
                paths.insert(p.clone());
            }
        }
    }
    assert!(paths.len() > 50, "{}", paths.len());
    let mut missing = Vec::new();
    let mut mp3 = 0;
    for p in &paths {
        match b.read_stream(p) {
            Ok(bytes) => {
                let ext = decode::extension(p);
                mp3 += usize::from(ext == "mp3");
                if let Err(e) = StreamJob::open(bytes, ext, false, 0.0) {
                    missing.push(format!("{p}: {e}"));
                }
            }
            Err(e) => missing.push(e),
        }
    }
    assert!(
        missing.is_empty(),
        "{} of {}: {:?}",
        missing.len(),
        paths.len(),
        &missing[..missing.len().min(5)]
    );
    assert!(mp3 > 5, "the map's music and ambience are MP3 ({mp3})");
}

#[test]
fn a_weapon_alias_picks_its_variants() {
    let Some(b) = bank() else { return };
    let mut b = b.lock().unwrap();
    let name = b
        .names()
        .filter(|n| n.starts_with("weap_ak47_fire_npc") || n.starts_with("weap_ak47_fire_plr"))
        .min()
        .map(str::to_owned)
        .expect("an ak47 fire alias");
    let n = b.aliases_of(&name).len();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        seen.insert(b.pick(&name).unwrap().name.to_string());
    }
    eprintln!("{name}: {n} variants, {} picked", seen.len());
    assert!(seen.len() >= n.min(2));
}

/// A device-less sound system over the real tables, with the listener at the origin facing +x.
fn sound() -> Option<Sound> {
    let root = root()?;
    let vfs = Arc::new(Vfs::open_stock(&root, 0).unwrap());
    let zones: Vec<PathBuf> = ["code_post_gfx_mp", "localized_common_mp", "mp_crossfire"]
        .iter()
        .map(|z| root.join(format!("zone/english/{z}.ff")))
        .collect();
    let mut s = Sound::new(Bank::load(vfs, &zones).unwrap(), false);
    s.set_listener([0.0; 3], 0.0);
    Some(s)
}

fn energy(s: &mut Sound, frames: usize) -> [f32; 2] {
    let out = s.render(frames);
    let mut e = [0.0f32; 2];
    for f in out.as_chunks::<2>().0 {
        e[0] += f[0] * f[0];
        e[1] += f[1] * f[1];
    }
    e
}

#[test]
fn a_weapon_shot_is_heard_from_its_side_and_fades_with_distance() {
    let Some(mut s) = sound() else { return };
    let name = "weap_ak47_fire_npc";
    let shot = |s: &mut Sound, at: [f32; 3]| {
        s.play(
            name,
            Cue {
                origin: Some(at),
                ..Cue::default()
            },
        )
        .is_some()
        .then(|| energy(s, 48_000))
    };
    let left = shot(&mut s, [300.0, 300.0, 0.0]).expect("plays");
    assert!(left[0] > 3.0 * left[1], "left {left:?}");
    let right = shot(&mut s, [300.0, -300.0, 0.0]).expect("plays");
    assert!(right[1] > 3.0 * right[0], "right {right:?}");
    let near = shot(&mut s, [150.0, 0.0, 0.0]).expect("plays");
    let far = shot(&mut s, [1500.0, 0.0, 0.0]).expect("plays");
    let (n, f) = (near[0] + near[1], far[0] + far[1]);
    assert!(n > 4.0 * f, "near {n} far {f}");
    assert!(
        shot(&mut s, [1.0e6, 0.0, 0.0]).is_none(),
        "beyond the alias's range nothing starts"
    );
    assert!(s.played.by_channel.values().sum::<u64>() >= 4);
}

#[test]
fn music_and_ambience_stream_from_the_iwds() {
    let Some(s) = sound() else { return };
    let mut music = None;
    let mut ambient = None;
    for n in s.bank.names().map(str::to_owned).collect::<Vec<_>>() {
        for a in s.bank.aliases_of(&n) {
            if matches!(a.audio, audio::bank::Clip::Streamed(_)) {
                let ch = &s.bank.channels[usize::from(a.channel)].name;
                if ch == "music" && music.is_none() {
                    music = Some(n.clone());
                }
                if ch == "ambient" && ambient.is_none() {
                    ambient = Some(n.clone());
                }
            }
        }
    }
    for n in [
        music.expect("a streamed music alias"),
        ambient.expect("a streamed ambient alias"),
    ] {
        let mut s2 = sound().unwrap();
        s2.ambient_play(&n, 0);
        // Give the decoder thread time to fill the ring, then listen for a second.
        let mut heard = 0.0;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            let e = energy(&mut s2, 4800);
            heard += e[0] + e[1];
        }
        assert!(heard > 0.1, "{n} is audible: {heard}");
        assert!(s2.played.failed.is_empty(), "{:?}", s2.played.failed);
    }
    drop(s);
}
