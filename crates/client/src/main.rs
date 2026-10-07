// SPDX-License-Identifier: GPL-3.0-or-later
//! `cod4e`, the player-facing client. For now: a fly camera over a map, and the scripted flythrough the harness
//! drives as stage 3 (`--flythrough`), which records frame times, a video and a screenshot.

mod app;
mod display;
mod flythrough;
mod video;

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
  --fov <degrees>        horizontal field of view at 4:3 (default 80); wider displays widen it (Hor+)
  --flythrough           fly the scripted path, then exit
  --duration <secs>      flythrough length (default 12)
  --out <dir>            flythrough output: frames.raw.csv, client.json, flythrough.mp4, screenshot.png
  --video / --screenshot record a video / save a screenshot during the flythrough
  --list-display-modes [--json]   print the GPU, monitors, video modes and present modes, then exit

Interactive: WASD move, Space/Ctrl up/down, Shift fast, click to capture the mouse, Esc quits.";

pub struct Cli {
    pub install: PathBuf,
    pub map: String,
    pub request: Request,
    pub present: String,
    pub fov: f32,
    pub flythrough: bool,
    pub duration: f32,
    pub out: Option<PathBuf>,
    pub video: bool,
    pub screenshot: bool,
    pub list: bool,
}

fn parse(args: &[String]) -> Result<Cli, String> {
    let mut c = Cli {
        install: std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), Into::into),
        map: "mp_crash".into(),
        request: Request::default(),
        present: "auto".into(),
        fov: 80.0,
        flythrough: false,
        duration: 12.0,
        out: None,
        video: false,
        screenshot: false,
        list: false,
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
                c.request.kind = FullscreenKind::parse(&v).ok_or(format!("unknown fullscreen kind {v}"))?;
            }
            "--present" => c.present = val(a)?,
            "--fov" => c.fov = val(a)?.parse().map_err(|_| "bad fov")?,
            "--duration" => c.duration = val(a)?.parse().map_err(|_| "bad duration")?,
            "--out" => c.out = Some(val(a)?.into()),
            "--flythrough" => c.flythrough = true,
            "--video" => c.video = true,
            "--screenshot" => c.screenshot = true,
            "--list-display-modes" => c.list = true,
            "--json" => {}
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if c.request.kind == FullscreenKind::Borderless && !size_given {
        c.request.size = None;
    }
    if (c.video || c.screenshot || c.flythrough) && c.out.is_none() {
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
        let c = parse(&args("--flythrough --size 1920x1080 --refresh 144 --fullscreen exclusive --present mailbox")).unwrap();
        assert_eq!(c.request.size, Some((1920, 1080)));
        assert_eq!(c.request.refresh, Some(144.0));
        assert_eq!(c.request.kind, FullscreenKind::Exclusive);
        assert!(c.flythrough && c.out.is_some());
        assert_eq!(parse(&args("--fullscreen borderless")).unwrap().request.size, None);
        assert!(parse(&args("--size 12")).is_err());
        assert!(parse(&args("--bogus")).is_err());
    }
}
