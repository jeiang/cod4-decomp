// SPDX-License-Identifier: GPL-3.0-or-later
use harness::child::{self, ChildArgs};
use harness::diff;
use harness::stage;
use harness::suite::{self, Config};
use harness::watchdog::Selection;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
cod4e-harness: runs the suite and writes a run bundle (a zip).

usage:
  cod4e-harness                      default suite; the zip lands next to this program
  cod4e-harness run [options]
  cod4e-harness diff <a.zip> <b.zip>
  cod4e-harness stages

run options:
  --stage <name>      run this stage (repeatable, or comma separated); default: the suite
  --script <file>     run a console script as a stage (repeatable)
  --cod4 <dir>        CoD4 install (else COD4_PATH, registry, Steam, ./COD4, a folder prompt)
  --out <dir>         where to write the zip (default: ./bundles)
  --timeout <secs>    per-stage timeout, replacing each stage's own
  --no-prompt         never ask for the install folder (also: COD4E_NO_PROMPT=1)

exit status: 0 all stages passed or skipped, 1 a stage failed, crashed or hung, 2 usage or I/O error";

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("__child") => child_main(&args[1..]),
        Some("diff") => diff_main(&args[1..]),
        Some("stages") => {
            for s in stage::STAGES {
                println!(
                    "{:<20}{}{}",
                    s.name,
                    s.description,
                    if s.default {
                        ""
                    } else {
                        " (not in the default suite)"
                    }
                );
            }
            ExitCode::SUCCESS
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("run") => run_main(&args[1..], false),
        None => run_main(&args, true),
        Some(a) if a.starts_with("--") => run_main(&args, false),
        Some(a) => usage_error(&format!("unknown command {a:?}")),
    }
}

fn value(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn run_main(args: &[String], double_click: bool) -> ExitCode {
    let mut cfg = Config {
        selections: Vec::new(),
        cod4: None,
        out_dir: PathBuf::from("bundles"),
        timeout: None,
        prompt: std::env::var_os("COD4E_NO_PROMPT").is_none_or(|v| v.is_empty()),
        exe_dir: std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from)),
    };
    if let Some(dir) = cfg.exe_dir.clone().filter(|_| double_click) {
        cfg.out_dir = dir;
    }
    let parsed = (|| -> Result<(), String> {
        let mut it = args.iter().cloned();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--stage" => {
                    for name in value(&mut it, &a)?.split(',') {
                        let def = stage::find(name).ok_or_else(|| {
                            format!("unknown stage {name:?} (see `cod4e-harness stages`)")
                        })?;
                        cfg.selections.push(Selection::Stage(def.name));
                    }
                }
                "--script" => cfg
                    .selections
                    .push(Selection::Script(value(&mut it, &a)?.into())),
                "--cod4" => cfg.cod4 = Some(value(&mut it, &a)?.into()),
                "--out" => cfg.out_dir = value(&mut it, &a)?.into(),
                "--timeout" => {
                    let s: u64 = value(&mut it, &a)?
                        .parse()
                        .map_err(|_| "--timeout takes whole seconds")?;
                    cfg.timeout = Some(Duration::from_secs(s));
                }
                "--no-prompt" => cfg.prompt = false,
                other => return Err(format!("unknown option {other:?}")),
            }
        }
        Ok(())
    })();
    if let Err(e) = parsed {
        return usage_error(&e);
    }
    if cfg.selections.is_empty() {
        cfg.selections = suite::default_selections();
    }
    let result = suite::run(&cfg);
    let code = match &result {
        Ok(o) => {
            println!(
                "\nbundle: {} ({:.1} MB)",
                o.bundle.path.display(),
                o.bundle.size as f64 / 1_048_576.0
            );
            if !o.bundle.dropped.is_empty() {
                println!("left out to fit 100 MB: {}", o.bundle.dropped.join(", "));
            }
            println!(
                "{}",
                if o.ok() {
                    "all stages passed or were skipped"
                } else {
                    "some stages failed; the bundle says why"
                }
            );
            ExitCode::from(u8::from(!o.ok()))
        }
        Err(e) => {
            eprintln!("error: could not write the bundle: {e}");
            ExitCode::from(2)
        }
    };
    if double_click && cfg!(windows) {
        // A double-clicked console closes on exit; keep the result readable.
        println!("\nSend the zip above to whoever asked for it. Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}

fn diff_main(args: &[String]) -> ExitCode {
    let [a, b] = args else {
        return usage_error("diff takes two bundles");
    };
    let result = diff::open(a.as_ref()).and_then(|a| {
        let b = diff::open(b.as_ref())?;
        diff::diff(&a, &b, &mut std::io::stdout().lock())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn child_main(args: &[String]) -> ExitCode {
    let mut c = ChildArgs {
        stage: None,
        script: None,
        dir: PathBuf::new(),
        install: None,
        dump_socket: None,
    };
    let mut it = args.iter().cloned();
    while let Some(a) = it.next() {
        let v = it.next();
        match (a.as_str(), v) {
            ("--stage", Some(v)) => c.stage = Some(v),
            ("--script", Some(v)) => c.script = Some(v.into()),
            ("--dir", Some(v)) => c.dir = v.into(),
            ("--install", Some(v)) => c.install = Some(v.into()),
            ("--dump-socket", Some(v)) => c.dump_socket = Some(v),
            _ => return usage_error("__child is internal"),
        }
    }
    ExitCode::from(child::run(c) as u8)
}
