// SPDX-License-Identifier: GPL-3.0-or-later
//! `cod4e`, the player-facing client. For now: a fly camera over a map, and the scripted flythrough the harness
//! drives as stage 3 (`--flythrough`), which records frame times, a video and a screenshot.

mod app;
mod display;
mod flythrough;
mod input;
mod listen;
mod models;
mod netplay;
mod shell;
mod showcase;
mod sound;
mod ui;
mod video;
mod viewmodel;

use display::{FullscreenKind, Request};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
cod4e: CoD4 multiplayer client (world viewer)

usage: cod4e [options]
  --install <dir>        original install (else COD4_PATH, else ./COD4)
  --map <name>           map zone to load (default mp_crash)
  --size WxH             window size, or the video mode for --fullscreen exclusive
  --refresh <hz>         refresh rate for exclusive fullscreen
  --fullscreen <kind>    windowed (default), borderless or exclusive
  --present <mode>       auto (default, vsync), fifo, mailbox or immediate
  --shadows <mode>       sun shadow maps: depth (default, hardware comparison), color or off
  --no-fog / --no-lights switch the map's fog / spot and omni primary lights off
  --fov <degrees>        horizontal field of view at 4:3 (default 80); wider displays widen it (Hor+)
  --flythrough           fly the scripted path, then exit
  --duration <secs>      flythrough length (default 12)
  --out <dir>            flythrough output: frames.raw.csv, client.json, flythrough.mp4, screenshot.png
  --show-models          smoke scene: stock player models in different poses and the first-person weapon in
                         front of a fixed camera (--model-count N sets the number of players, default 7)
  --video / --screenshot record a video / save a screenshot during the flythrough
  --config <path>        key binds and settings file (default: <config dir>/cod4e/config_mp.cfg)
  --audio-selftest       check the sound system on the real tables without a window or sound card, then exit (harness stage)
  --input-selftest       check key binds, mouse look and the config file without a window, then exit (harness stage)
  --listen               play a team deathmatch against bots on a server started inside this process
  --bots <n>             bots on the listen server (default 9)
  --connect <host:port>  play on a server (see cod4e-server)
  --name <name>          player name on the server
  --no-sound             mix the sound without opening a sound card
  --ui-tour <dir>        open the stock menus in turn, save a screenshot of each and ui.json to <dir>, then exit (harness)
  --ui-script <steps>    drive the menus like a player: click=<item>, menu=<name>[:secs], ingame[=secs], wait=<secs>, shot=<name>;
                         writes ui-script.json (and shots) to --out, then exits (harness)
  --no-autojoin          do not answer the server's team and class menus by default (direct --listen/--connect)
  --autoplay             a scripted player instead of the keyboard, for --duration seconds (harness stage 4)
  --list-display-modes [--json]   print the GPU, monitors, video modes and present modes, then exit

Interactive: WASD move, Space/Ctrl up/down, Shift fast, arrows look, click to capture the mouse, Esc quits.
Keys are the config file's binds, e.g. bind w +forward.";

pub struct Cli {
    pub install: PathBuf,
    pub map: String,
    pub request: Request,
    pub present: String,
    pub fov: f32,
    pub settings: render::Settings,
    pub flythrough: bool,
    pub duration: f32,
    pub out: Option<PathBuf>,
    pub video: bool,
    pub screenshot: bool,
    pub list: bool,
    pub config: Option<PathBuf>,
    pub input_selftest: bool,
    pub audio_selftest: bool,
    /// Number of showcase players, when the scene is on.
    pub show_models: Option<usize>,
    /// Server to play on, `host:port`.
    pub connect: Option<String>,
    /// Play on a server started inside this process.
    pub listen: bool,
    pub bots: usize,
    pub name: String,
    /// A scripted player instead of the keyboard (harness): walks, aims at and shoots enemies for `--duration`.
    pub autoplay: bool,
    /// Never open a sound card (the harness: CI machines have none).
    pub no_sound: bool,
    /// Open the stock menus one after another with no world, save a screenshot of each and a report (harness stage).
    pub ui_tour: Option<PathBuf>,
    /// Steps driving the menus like a player (`click=Join Game,menu=class:30,shot=a,...`); see `app::UiScript`.
    pub ui_script: Option<String>,
    /// Answer the server's team and class menus with the default choices (direct `--listen`/`--connect`, until the player
    /// picks); off with `--no-autojoin` and in menu-started matches.
    pub autojoin: bool,
}

impl Cli {
    /// Runs for `--duration` seconds, then writes the report and exits.
    pub fn timed(&self) -> bool {
        self.flythrough || self.autoplay
    }

    /// No world to start with: the player begins in the menus (no automation or direct-connect flag given).
    pub fn menu_mode(&self) -> bool {
        !(self.flythrough
            || self.show_models.is_some()
            || self.netplay()
            || self.list
            || self.autoplay)
    }

    pub fn netplay(&self) -> bool {
        self.connect.is_some() || self.listen
    }
}

fn parse(args: &[String]) -> Result<Cli, String> {
    let mut c = Cli {
        install: std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), Into::into),
        map: "mp_crash".into(),
        request: Request::default(),
        present: "auto".into(),
        fov: 80.0,
        settings: render::Settings::default(),
        flythrough: false,
        duration: 12.0,
        out: None,
        video: false,
        screenshot: false,
        list: false,
        config: None,
        input_selftest: false,
        audio_selftest: false,
        show_models: None,
        connect: None,
        listen: false,
        bots: 9,
        name: "player".into(),
        autoplay: false,
        no_sound: false,
        ui_tour: None,
        ui_script: None,
        autojoin: true,
    };
    let mut it = args.iter();
    let mut size_given = false;
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--install" => c.install = val(a)?.into(),
            "--map" => c.map = val(a)?,
            "--size" => {
                let v = val(a)?;
                let (w, h) = v.split_once('x').ok_or("--size takes WxH")?;
                c.request.size = Some((
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                ));
                size_given = true;
            }
            "--refresh" => c.request.refresh = Some(val(a)?.parse().map_err(|_| "bad refresh")?),
            "--fullscreen" => {
                let v = val(a)?;
                c.request.kind =
                    FullscreenKind::parse(&v).ok_or(format!("unknown fullscreen kind {v}"))?;
            }
            "--present" => c.present = val(a)?,
            "--shadows" => {
                c.settings.shadows = match val(a)?.as_str() {
                    "depth" => render::ShadowMode::Depth,
                    "color" => render::ShadowMode::Color,
                    "off" => render::ShadowMode::Off,
                    other => return Err(format!("unknown shadow mode {other}")),
                }
            }
            "--no-fog" => c.settings.fog = false,
            "--no-lights" => c.settings.primary_lights = false,
            "--fov" => c.fov = val(a)?.parse().map_err(|_| "bad fov")?,
            "--duration" => c.duration = val(a)?.parse().map_err(|_| "bad duration")?,
            "--out" => c.out = Some(val(a)?.into()),
            "--flythrough" => c.flythrough = true,
            "--video" => c.video = true,
            "--screenshot" => c.screenshot = true,
            "--input-selftest" => c.input_selftest = true,
            "--audio-selftest" => c.audio_selftest = true,
            "--config" => c.config = Some(val(a)?.into()),
            "--show-models" => c.show_models = Some(7),
            "--model-count" => {
                c.show_models = Some(val(a)?.parse().map_err(|_| "bad model count")?)
            }
            "--connect" => c.connect = Some(val(a)?),
            "--listen" => c.listen = true,
            "--bots" => c.bots = val(a)?.parse().map_err(|_| "bad bot count")?,
            "--name" => c.name = val(a)?,
            "--autoplay" => c.autoplay = true,
            "--no-sound" => c.no_sound = true,
            "--ui-tour" => c.ui_tour = Some(val(a)?.into()),
            "--ui-script" => c.ui_script = Some(val(a)?),
            "--no-autojoin" => c.autojoin = false,
            "--list-display-modes" => c.list = true,
            "--json" => {}
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if c.request.kind == FullscreenKind::Borderless && !size_given {
        c.request.size = None;
    }
    if c.connect.is_some() && c.listen {
        return Err("--connect and --listen are alternatives".into());
    }
    if c.autoplay && !c.netplay() {
        return Err("--autoplay needs --connect or --listen".into());
    }
    if (c.video || c.screenshot || c.timed() || c.ui_script.is_some()) && c.out.is_none() {
        c.out = Some(".".into());
    }
    Ok(c)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match parse(&args) {
        Ok(c) => c,
        Err(e) if e.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if cli.input_selftest {
        let bad = input::selftest::run();
        return if bad.is_empty() {
            ExitCode::SUCCESS
        } else {
            eprintln!("input selftest failed: {}", bad.join("; "));
            ExitCode::from(1)
        };
    }
    if cli.audio_selftest {
        return match sound::selftest(&cli.install, &cli.map) {
            Ok(m) => {
                let json = serde_json::to_string_pretty(&m).unwrap_or_default();
                if let Some(out) = &cli.out {
                    let _ = std::fs::create_dir_all(out);
                    let _ = std::fs::write(out.join("audio.json"), &json);
                }
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(bad) => {
                eprintln!("audio selftest failed: {}", bad.join("; "));
                ExitCode::from(1)
            }
        };
    }
    match app::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn options_parse_and_borderless_defaults_to_native_size() {
        let c = parse(&args(
            "--flythrough --size 1920x1080 --refresh 144 --fullscreen exclusive --present mailbox",
        ))
        .unwrap();
        assert_eq!(c.request.size, Some((1920, 1080)));
        assert_eq!(c.request.refresh, Some(144.0));
        assert_eq!(c.request.kind, FullscreenKind::Exclusive);
        assert!(c.flythrough && c.out.is_some());
        assert_eq!(
            parse(&args("--fullscreen borderless"))
                .unwrap()
                .request
                .size,
            None
        );
        assert!(parse(&args("--size 12")).is_err());
        assert!(parse(&args("--bogus")).is_err());
    }
}
