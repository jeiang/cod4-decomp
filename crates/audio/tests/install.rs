// SPDX-License-Identifier: GPL-3.0-only
//! Install-gated: the real alias tables, channels and files. Skips when the original install is absent.

use assets::vfs::{LANGUAGES, Vfs};
use audio::bank::{Bank, Clip};
use audio::decode::{self, StreamJob};
use audio::{Cue, Sound};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn root() -> Option<PathBuf> {
    let configured = std::env::var_os("COD4_PATH");
    let root = configured
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../COD4")));
    if root.join("zone/english").is_dir() {
        Some(root)
    } else if configured.is_some() {
        panic!(
            "COD4_PATH has no zone/english directory: {}",
            root.display()
        );
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
fn every_stock_map_alias_resolves_to_playable_audio() {
    let Some(root) = root() else { return };
    let custom: std::collections::HashSet<_> =
        assets::paths::custom_maps(&root, "").into_iter().collect();
    let maps: Vec<_> = assets::paths::maps(&root, "")
        .into_iter()
        .filter(|map| !custom.contains(map))
        .collect();
    assert!(!maps.is_empty(), "no stock maps found");

    let vfs = Arc::new(Vfs::open_stock(&root, 0).unwrap());
    let mut unresolved = Vec::new();
    for map in &maps {
        let zones: Vec<PathBuf> = [
            "code_post_gfx_mp",
            "localized_code_post_gfx_mp",
            "localized_common_mp",
            map.as_str(),
        ]
        .iter()
        .map(|zone| root.join(format!("zone/english/{zone}.ff")))
        .collect();
        let mut sound = Sound::new(
            Bank::load(vfs.clone(), &zones).unwrap_or_else(|e| panic!("{map}: {e}")),
            false,
        );
        sound.set_listener([0.0; 3], 0.0);
        assert!(
            sound.bank.stats.pcm_bytes <= 128 * 1024 * 1024,
            "{map}: decoded {} PCM bytes, exceeding the 128 MiB per-map budget",
            sound.bank.stats.pcm_bytes
        );
        for list in sound.bank.names().map(str::to_owned).collect::<Vec<_>>() {
            for alias in sound.bank.aliases_of(&list) {
                if matches!(alias.audio, Clip::Silent) {
                    unresolved.push(format!("{map}: {list}/{}", alias.name));
                }
            }
        }
        for name in [
            "mp_player_join",
            "mp_player_leave",
            "match_countdown_tick",
            "mp_experience_tick",
            "enter_prestige",
        ] {
            let aliases = sound.bank.aliases_of(name);
            assert!(!aliases.is_empty(), "{map}: missing match alias {name}");
            assert!(
                aliases
                    .iter()
                    .any(|alias| !matches!(alias.audio, Clip::Null | Clip::Silent)),
                "{map}: match alias {name} has no playable audio"
            );
            assert!(
                sound.play(name, Cue::default()).is_some(),
                "{map}: match alias {name} did not start"
            );
        }
    }
    assert!(
        unresolved.is_empty(),
        "{} unresolved aliases, e.g. {:?}",
        unresolved.len(),
        &unresolved[..unresolved.len().min(20)]
    );
}

#[test]
fn localized_front_end_menu_aliases_have_audio() {
    let Some(root) = root() else { return };
    let vfs = Arc::new(Vfs::open_stock(&root, 0).unwrap());
    // The front end's zones, as the client loads them: an install without the single-player
    // zone (`code_post_gfx.ff`, e.g. a multiplayer-only copy) is still valid, so absent
    // optional zones are noted and skipped. The multiplayer zones are required.
    let mut zones: Vec<PathBuf> = Vec::new();
    for zone in [
        "code_post_gfx_mp",
        "code_post_gfx",
        "localized_code_post_gfx_mp",
    ] {
        match assets::paths::zone_file(&root, "", zone) {
            Some(path) => zones.push(path),
            None if zone.ends_with("_mp") => {
                panic!("install lacks required zone {zone}.ff under {}", root.display())
            }
            None => eprintln!("note: install has no {zone}.ff (single-player zone); skipping it"),
        }
    }
    let mut sound = Sound::new(Bank::load(vfs, &zones).unwrap(), false);
    sound.set_listener([0.0; 3], 0.0);
    for list in sound.bank.names().map(str::to_owned).collect::<Vec<_>>() {
        for alias in sound.bank.aliases_of(&list) {
            assert!(
                !matches!(alias.audio, Clip::Silent),
                "stock menu list {list}/{} is unresolved",
                alias.name
            );
        }
    }
    for name in [
        "mouse_click",
        "mouse_over",
        "mouse_submenu_over",
        "music_mainmenu_mp",
    ] {
        let aliases = sound.bank.aliases_of(name);
        assert!(!aliases.is_empty(), "missing menu alias {name}");
        assert!(
            aliases
                .iter()
                .any(|alias| !matches!(alias.audio, Clip::Null | Clip::Silent)),
            "menu alias {name} has no playable audio"
        );
        assert!(
            sound.play(name, Cue::default()).is_some(),
            "menu alias {name} did not start"
        );
    }
}

fn stock_zone_loaded_sounds(path: &PathBuf) -> std::collections::HashMap<String, Arc<[u8]>> {
    struct SoundsOnly;
    impl assets::zone::DecodeFilter for SoundsOnly {
        fn keep(&self, ty: assets::zone::XAssetType) -> bool {
            ty == assets::zone::XAssetType::Sound
        }
    }

    let zone = assets::zone::Zone::open(std::io::BufReader::new(
        std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ))
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut sounds = std::collections::HashMap::new();
    zone.decode(&SoundsOnly, |asset| {
        if let assets::zone::Asset::Sound(list) = asset {
            for alias in list.aliases.iter() {
                let Some(name) = alias.name.as_deref() else {
                    continue;
                };
                if name.starts_with(',') {
                    continue;
                }
                let Some(assets::zone::sound::SoundSource::Loaded(Some(loaded))) =
                    alias.sound_file.as_deref().map(|file| &file.source)
                else {
                    continue;
                };
                if !loaded.data.is_empty() {
                    sounds.insert(name.to_ascii_lowercase(), loaded.data.clone());
                }
            }
        }
    })
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    sounds
}

#[test]
fn stock_match_zones_override_earlier_loaded_sound_data() {
    let Some(root) = root() else { return };
    let custom: std::collections::HashSet<_> =
        assets::paths::custom_maps(&root, "").into_iter().collect();
    let maps: Vec<_> = assets::paths::maps(&root, "")
        .into_iter()
        .filter(|map| !custom.contains(map))
        .collect();
    assert!(!maps.is_empty(), "no stock maps found");

    let shared_zones = [
        "code_post_gfx_mp",
        "localized_code_post_gfx_mp",
        "localized_common_mp",
    ];
    let shared_sounds: Vec<_> = shared_zones
        .iter()
        .map(|zone| {
            let path = root.join(format!("zone/english/{zone}.ff"));
            ((*zone).to_owned(), stock_zone_loaded_sounds(&path))
        })
        .collect();
    let mut conflicts = Vec::new();
    for map in maps {
        let mut known = std::collections::HashMap::new();
        for (zone, sounds) in &shared_sounds {
            for (name, data) in sounds {
                if known
                    .get(name)
                    .is_some_and(|previous: &Arc<[u8]>| previous.as_ref() != data.as_ref())
                {
                    conflicts.push(format!("{map}: {zone}/{name}"));
                }
                known.insert(name.clone(), data.clone());
            }
        }
        let map_sounds = stock_zone_loaded_sounds(&root.join(format!("zone/english/{map}.ff")));
        for (name, data) in map_sounds {
            if known
                .get(&name)
                .is_some_and(|previous: &Arc<[u8]>| previous.as_ref() != data.as_ref())
            {
                conflicts.push(format!("{map}: {name}"));
            }
        }
    }
    assert!(
        conflicts
            .iter()
            .any(|conflict| conflict == "mp_crash_snow: veh_mig29_close_loop2"),
        "expected stock mp_crash_snow to redefine veh_mig29_close_loop2 with later PCM; found {conflicts:?}"
    );
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
