// SPDX-License-Identifier: GPL-3.0-only
//! Finds the CoD4 install for `cod4e`, `cod4e-server` and the harness, in this order: the
//! `--install` flag, `COD4_PATH`, the path remembered from an earlier run, the Windows registry
//! and Steam libraries, `./COD4` and `COD4` next to the program, then (when the caller may ask)
//! a native folder prompt. A folder the user picked in the prompt is remembered.
//!
//! The registry keys are the commonly documented ones for the retail and
//! Steam releases; no Windows machine has confirmed them yet. A wrong guess
//! only falls through to the next source.
use std::path::{Path, PathBuf};

pub const STEAM_DIR: &str = "Call of Duty 4";

/// The file in the config folder that holds the remembered install path.
const REMEMBERED: &str = "install-path.txt";

#[derive(Clone, Debug)]
pub struct Install {
    pub path: PathBuf,
    /// Which rule found it.
    pub source: String,
}

#[derive(Debug, Default)]
pub struct Detection {
    pub install: Option<Install>,
    /// The folder dialog was offered (it may have been cancelled or given a wrong folder).
    pub prompted: bool,
    /// Every place that was looked at, in order, and why it failed.
    pub tried: Vec<String>,
}

impl Detection {
    /// What to tell a person when nothing was found.
    pub fn not_found_message(&self, program: &str) -> String {
        let mut m = String::from(
            "Call of Duty 4 install not found (a folder with main and zone). Looked at:\n",
        );
        for t in &self.tried {
            m.push_str("  ");
            m.push_str(t);
            m.push('\n');
        }
        m.push_str(&format!(
            "Run {program} with --install <folder>, or set COD4_PATH to the folder, or run cod4e once and pick the folder when it asks."
        ));
        m
    }
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

/// `<platform config dir>/cod4e`: where the user's profiles and the remembered install live.
pub fn default_config_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    #[cfg(target_os = "macos")]
    let base = env("HOME").map(|h| h.join("Library/Application Support"));
    #[cfg(windows)]
    let base = env("APPDATA");
    #[cfg(not(any(target_os = "macos", windows)))]
    let base = env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")));
    base.map(|b| b.join("cod4e"))
}

pub struct Options<'a> {
    /// The path the command line gave, and the flag's name for messages.
    pub explicit: Option<&'a Path>,
    pub flag: &'a str,
    /// Ask with a folder dialog as the last resort (needs the `prompt` feature and a display).
    pub prompt: bool,
    pub exe_dir: Option<&'a Path>,
}

/// Finds the install. The remembered path always lives in the platform's default config dir, so the client, the
/// server and the harness share one whatever profile folder a run uses.
pub fn detect(opts: &Options) -> Detection {
    detect_in(opts, default_config_dir().as_deref())
}

fn detect_in(opts: &Options, config: Option<&Path>) -> Detection {
    let env = std::env::var_os("COD4_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    detect_from(opts, env, config)
}

fn detect_from(opts: &Options, env: Option<PathBuf>, config: Option<&Path>) -> Detection {
    let mut d = Detection::default();
    let candidates = candidates(opts, env, config.and_then(recall));
    for (source, path) in candidates {
        if is_install(&path) {
            d.install = Some(Install { path, source });
            return d;
        }
        d.tried
            .push(format!("{source}: {} (no main/ and zone/)", path.display()));
    }
    if opts.prompt {
        d.prompted = true;
        match prompt_for_folder() {
            Some(path) if is_install(&path) => {
                if let Some(c) = config {
                    remember(c, &path);
                }
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

/// Where to look, best first. The platform's known locations come after what the user chose.
fn candidates(
    opts: &Options,
    env: Option<PathBuf>,
    remembered: Option<PathBuf>,
) -> Vec<(String, PathBuf)> {
    let mut c: Vec<(String, PathBuf)> = Vec::new();
    if let Some(p) = opts.explicit {
        c.push((opts.flag.into(), p.to_owned()));
    }
    if let Some(p) = env {
        c.push(("COD4_PATH".into(), p));
    }
    if let Some(p) = remembered {
        c.push(("remembered path".into(), p));
    }
    c.extend(platform_candidates());
    c.push(("./COD4".into(), PathBuf::from("COD4")));
    if let Some(dir) = opts.exe_dir {
        c.push(("COD4 next to the executable".into(), dir.join("COD4")));
        c.push(("the executable's folder".into(), dir.to_owned()));
    }
    c
}

/// The path an earlier run remembered, if the file is there.
fn recall(config_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_dir.join(REMEMBERED)).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Keeps `install` for the next run. Failing to write is no reason to stop a run: the next one asks again.
fn remember(config_dir: &Path, install: &Path) {
    let _ = std::fs::create_dir_all(config_dir);
    let _ = std::fs::write(
        config_dir.join(REMEMBERED),
        format!("{}\n", install.display()),
    );
}

/// The prompt needs a display; headless Linux has none.
#[cfg(feature = "prompt")]
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

#[cfg(not(feature = "prompt"))]
fn prompt_for_folder() -> Option<PathBuf> {
    None
}

/// Tells the person why no install was found in a message box, for a console that closes with the program.
/// Without a display (or the `prompt` feature) it does nothing: the caller printed the text too.
#[cfg(feature = "prompt")]
pub fn show_error(text: &str) {
    if cfg!(all(unix, not(target_os = "macos")))
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        return;
    }
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Call of Duty 4 install not found")
        .set_description(text)
        .show();
}

#[cfg(not(feature = "prompt"))]
pub fn show_error(_text: &str) {}

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

    fn options(explicit: Option<&Path>) -> Options<'_> {
        Options {
            explicit,
            flag: "--install",
            prompt: false,
            exe_dir: None,
        }
    }

    fn fake_install(dir: &Path) {
        std::fs::create_dir(dir.join("Main")).unwrap();
        std::fs::create_dir(dir.join("zone")).unwrap();
    }

    #[test]
    fn explicit_path_wins_over_everything() {
        let dir = tempfile::tempdir().unwrap();
        fake_install(dir.path());
        let ok = detect(&options(Some(dir.path())));
        assert_eq!(ok.install.unwrap().source, "--install");
    }

    #[test]
    fn flag_then_environment_then_remembered_then_known_places() {
        let order: Vec<String> = candidates(
            &options(Some(Path::new("a"))),
            Some("b".into()),
            Some("c".into()),
        )
        .into_iter()
        .map(|(s, _)| s)
        .collect();
        assert_eq!(order[..3], ["--install", "COD4_PATH", "remembered path"]);
        assert_eq!(order.last().unwrap(), "./COD4");
    }

    #[test]
    fn a_valid_remembered_path_is_used_and_a_stale_one_is_not() {
        let cfg = tempfile::tempdir().unwrap();
        let game = tempfile::tempdir().unwrap();
        fake_install(game.path());
        assert_eq!(recall(cfg.path()), None);
        remember(cfg.path(), game.path());
        assert_eq!(recall(cfg.path()).as_deref(), Some(game.path()));
        let found = detect_from(&options(None), None, Some(cfg.path()));
        let got = found.install.expect("the remembered install is found");
        assert_eq!(
            (got.source.as_str(), got.path),
            ("remembered path", game.path().to_owned())
        );
        remember(cfg.path(), Path::new("/no/such/place"));
        let stale = detect_from(&options(None), None, Some(cfg.path()));
        assert!(stale.tried.iter().any(|t| t.starts_with("remembered path")));
        assert!(stale.install.is_none_or(|i| i.source != "remembered path"));
    }

    #[test]
    fn the_message_lists_what_was_tried_and_how_to_fix_it() {
        let d = detect(&Options {
            explicit: Some(Path::new("/no/such/place")),
            ..options(None)
        });
        let m = d.not_found_message("cod4e-server");
        assert!(m.contains("/no/such/place") && m.contains("--install") && m.contains("COD4_PATH"));
    }
}
