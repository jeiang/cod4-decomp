// SPDX-License-Identifier: GPL-3.0-or-later
//! Stage 3 against a fake `cod4e` shell script that follows the client CLI
//! contract. Unix only: the fake is a `sh` script.
#![cfg(unix)]
use harness::install::Detection;
use harness::manifest;
use harness::stage::{StageCtx, StageReport, Status};
use harness::stages::client_flythrough::{Options, run_with};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MODES: &str = r#"{"gpu":{"name":"Fake GPU","backend":"vulkan","driver":"1","device_type":"discrete","features":[],"limits":{}},
"monitors":[{"name":"M","primary":true,"native":{"width":2560,"height":1440,"refresh_mhz":60000},"scale_factor":1.0,
"modes":[{"width":2560,"height":1440,"refresh_mhz":60000,"bit_depth":32},{"width":2560,"height":1440,"refresh_mhz":30000,"bit_depth":32}]}],
"present_modes":["fifo","mailbox"]}"#;

const SCRIPT: &str = r#"#!/bin/sh
here=$(dirname "$0")
beh=$(cat "$here/behavior")
if [ "$1" = --list-display-modes ]; then
  case $beh in nodisplay) echo "no display: nothing to open a window on" >&2; exit 1;;
    brokenwm) echo "cannot connect to the window system" >&2; exit 1;; esac
  cat "$here/modes.json"; exit 0
fi
all="$*"
while [ $# -gt 0 ]; do
  case $1 in --out) out=$2; shift;; esac
  shift
done
mkdir -p "$out"
[ "$beh" = hang ] && exec sleep 30
echo "$all" > "$out/args.txt"
if [ "$beh" = error ]; then
  echo '{"status":"error","error":"no adapter"}' > "$out/client.json"; exit 1
fi
printf 'cpu_ms,gpu_ms,present_interval_ms,mem_bytes\n1.0,0.5,16.0,100\n2.0,,17.0,200\n3.0,0.7,18.0,300\n' > "$out/frames.raw.csv"
printf 'video' > "$out/flythrough.ivf"
printf 'png' > "$out/screenshot.png"
echo '{"status":"ok","frames":3,"video":{"file":"flythrough.ivf","frames":3,"dropped":0,"bytes":5},"screenshot":"screenshot.png","zone_load_ms":42.0,"world":{"surfaces_drawn_p50":7}}' > "$out/client.json"
"#;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new(behavior: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("cod4e");
        fs::write(&exe, SCRIPT).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("modes.json"), MODES).unwrap();
        let f = Self { dir };
        f.behave(behavior);
        f
    }
    fn behave(&self, b: &str) {
        fs::write(self.dir.path().join("behavior"), b).unwrap();
    }
    fn opts(&self) -> Options {
        Options {
            client: self.dir.path().join("cod4e"),
            secs: 3,
            grace: Duration::from_millis(500),
        }
    }
}

fn run(fake: &Fake, install: bool) -> (StageReport, PathBuf, tempfile::TempDir) {
    let out = tempfile::tempdir().unwrap();
    let ctx = StageCtx {
        dir: out.path().to_owned(),
        install: install.then(|| PathBuf::from("/fake/install")),
    };
    let r = run_with(&ctx, &fake.opts()).unwrap();
    (r, out.path().to_owned(), out)
}

#[test]
fn runs_every_planned_mode_and_aggregates() {
    let fake = Fake::new("ok");
    let (r, dir, _keep) = run(&fake, true);
    assert_eq!(r.status, Status::Passed, "{r:?}");
    let slugs = [
        "native-borderless",
        "1920x1080-windowed",
        "1280x720-windowed",
        "exclusive-2560x1440-30000mhz",
    ];
    for s in slugs {
        assert!(dir.join(s).join("frames.csv").is_file(), "{s}");
        assert!(!dir.join(s).join("frames.raw.csv").exists(), "{s} raw kept");
        for f in [
            "frames.csv",
            "flythrough.ivf",
            "screenshot.png",
            "client.log",
            "client.json",
        ] {
            assert!(r.files.contains(&format!("{s}/{f}")), "{s}/{f} not listed");
        }
        let p = r.series[&format!("{s}.frame.cpu_ms")];
        assert_eq!((p.n, p.p50, p.max), (3, 2.0, 3.0));
        // The empty gpu_ms cell is not a sample.
        assert_eq!(r.series[&format!("{s}.frame.gpu_ms")].n, 2);
        assert_eq!(r.metrics[&format!("{s}.zone_load_ms")], 42.0);
        assert_eq!(r.metrics[&format!("{s}.video_bytes")], 5.0);
    }
    let args = fs::read_to_string(dir.join("exclusive-2560x1440-30000mhz/args.txt")).unwrap();
    for want in [
        "--install /fake/install",
        "--map mp_crash --flythrough --duration 3",
        "--video --screenshot --size 2560x1440 --refresh 30 --fullscreen exclusive",
    ] {
        assert!(args.contains(want), "{want} not in {args}");
    }
    let env = r.environment.as_ref().unwrap();
    assert_eq!(env["gpu"]["name"], "Fake GPU");
    assert_eq!(env["display_modes"]["present_modes"][1], "mailbox");
    let runs = env["display_modes"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4);
    assert_eq!(runs[3]["fullscreen"], "exclusive");
    assert_eq!(runs[3]["refresh_mhz"], 30000);

    // The manifest picks up the GPU and display modes.
    let m = manifest::collect(0, &Detection::default(), None, Ok(()), &[r]);
    assert_eq!(m["gpu"]["name"], "Fake GPU");
    assert_eq!(m["display_modes"]["monitors"][0]["native"]["width"], 2560);
}

#[test]
fn missing_install_or_display_skips() {
    let fake = Fake::new("ok");
    let (r, _, _k) = run(&fake, false);
    assert_eq!(
        (r.status, r.reason.as_deref()),
        (Status::Skipped, Some("no install"))
    );

    fake.behave("nodisplay");
    let (r, _, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Skipped);
    assert!(r.reason.unwrap().starts_with("no display"));

    fake.behave("brokenwm");
    let (r, _, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    assert!(r.reason.unwrap().contains("window system"));
}

#[test]
fn error_and_hung_runs_fail_the_stage_but_not_the_other_runs() {
    let fake = Fake::new("error");
    let (r, _, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(r.notes.len(), 4);
    assert!(
        r.notes.iter().all(|n| n.contains("no adapter")),
        "{:?}",
        r.notes
    );

    fake.behave("hang");
    let (r, dir, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(r.notes.len(), 4, "a hung run must not stop the others");
    assert!(r.notes.iter().all(|n| n.contains("Hung")), "{:?}", r.notes);
    assert!(Path::new(&dir.join("native-borderless/client.log")).exists());
}
