// SPDX-License-Identifier: GPL-3.0-only
//! Finds the CoD4 install: `--cod4`, `COD4_PATH`, the Windows registry, Steam
//! libraries, `./COD4`, then a native folder prompt.
//!
//! The registry keys are the commonly documented ones for the retail and
//! Steam releases; no Windows machine has confirmed them yet. A wrong guess
//! only falls through to the next source.
use std::path::{Path, PathBuf};

pub const STEAM_DIR: &str = "Call of Duty 4";

#[derive(Clone, Debug)]
pub struct Install {
    pub path: PathBuf,
    /// Which rule found it.
    pub source: String,
}

#[derive(Debug, Default)]
pub struct Detection {
    pub install: Option<Install>,
    /// Every place that was looked at, in order, and why it failed.
    pub tried: Vec<String>,
}

/// A usable install has the `main` and `zone` directories.
pub fn is_install(dir: &Path) -> bool {
    has_dir(dir, "main") && has_dir(dir, "zone")
}

fn has_dir(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            e.file_name().to_string_lossy().eq_ignore_ascii_case(name) && e.path().is_dir()
        })
    })
}

pub struct Options<'a> {
    pub explicit: Option<&'a Path>,
    pub prompt: bool,
    pub exe_dir: Option<&'a Path>,
}

pub fn detect(opts: &Options) -> Detection {
    let mut d = Detection::default();
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    if let Some(p) = opts.explicit {
        candidates.push(("--cod4".into(), p.to_owned()));
    }
    if let Some(p) = std::env::var_os("COD4_PATH").filter(|p| !p.is_empty()) {
        candidates.push(("COD4_PATH".into(), p.into()));
    }
    candidates.extend(platform_candidates());
    candidates.push(("./COD4".into(), PathBuf::from("COD4")));
    if let Some(dir) = opts.exe_dir {
        candidates.push(("COD4 next to the executable".into(), dir.join("COD4")));
        candidates.push(("the executable's folder".into(), dir.to_owned()));
    }
    for (source, path) in candidates {
        if is_install(&path) {
            d.install = Some(Install { path, source });
            return d;
        }
        d.tried
            .push(format!("{source}: {} (no main/ and zone/)", path.display()));
    }
    if opts.prompt {
        match prompt_for_folder() {
            Some(path) if is_install(&path) => {
                d.install = Some(Install {
                    path,
                    source: "folder prompt".into(),
                });
            }
            Some(path) => d.tried.push(format!(
                "folder prompt: {} (no main/ and zone/)",
                path.display()
            )),
            None => d
                .tried
                .push("folder prompt: cancelled or unavailable".into()),
        }
    } else {
        d.tried.push("folder prompt: disabled".into());
    }
    d
}

/// The prompt needs a display; headless Linux has none.
fn prompt_for_folder() -> Option<PathBuf> {
    if cfg!(all(unix, not(target_os = "macos")))
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        return None;
    }
    rfd::FileDialog::new()
        .set_title("Select your Call of Duty 4 install folder")
        .pick_folder()
}

#[cfg(windows)]
fn platform_candidates() -> Vec<(String, PathBuf)> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let mut out = Vec::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    for key in [
        "SOFTWARE\\WOW6432Node\\Activision\\Call of Duty 4",
        "SOFTWARE\\Activision\\Call of Duty 4",
    ] {
        if let Ok(k) = hklm.open_subkey(key)
            && let Ok(v) = k.get_value::<String, _>("InstallPath")
        {
            out.push((format!("registry HKLM\\{key}"), PathBuf::from(v)));
        }
    }
    let mut steam_roots: Vec<PathBuf> = Vec::new();
    if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("SOFTWARE\\Valve\\Steam")
        && let Ok(v) = k.get_value::<String, _>("SteamPath")
    {
        steam_roots.push(PathBuf::from(v));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(p) = std::env::var_os(var) {
            steam_roots.push(PathBuf::from(p).join("Steam"));
        }
    }
    out.extend(steam_candidates(&steam_roots));
    out
}

#[cfg(not(windows))]
fn platform_candidates() -> Vec<(String, PathBuf)> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let roots = [
        home.join("Library/Application Support/Steam"),
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
    ];
    steam_candidates(&roots)
}

/// The game in each Steam root and in each library the root's
/// `libraryfolders.vdf` lists.
fn steam_candidates(roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut libs: Vec<PathBuf> = Vec::new();
    for root in roots {
        libs.push(root.clone());
        if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            libs.extend(vdf_library_paths(&vdf));
        }
    }
    libs.dedup();
    libs.into_iter()
        .map(|l| {
            let game = l.join("steamapps/common").join(STEAM_DIR);
            (format!("Steam library {}", l.display()), game)
        })
        .collect()
}

/// `"path"  "D:\\SteamLibrary"` values of a `libraryfolders.vdf`.
pub fn vdf_library_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines()
        .filter_map(|line| {
            let mut quoted = line.split('"').skip(1).step_by(2);
            (quoted.next()? == "path").then(|| quoted.next())?
        })
        .map(|p| PathBuf::from(p.replace("\\\\", "\\")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_folders() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t\t\"label\"\t\t\"\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}\n";
        assert_eq!(
            vdf_library_paths(vdf),
            [
                PathBuf::from("C:\\Program Files (x86)\\Steam"),
                PathBuf::from("D:\\SteamLibrary")
            ]
        );
    }

    #[test]
    fn explicit_path_wins_over_everything() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Main")).unwrap();
        std::fs::create_dir(dir.path().join("zone")).unwrap();
        let ok = detect(&Options {
            explicit: Some(dir.path()),
            prompt: false,
            exe_dir: None,
        });
        assert_eq!(ok.install.unwrap().source, "--cod4");
    }
}
