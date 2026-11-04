// SPDX-License-Identifier: GPL-3.0-only
//! Stage 3 against a fake `cod4e` shell script that follows the client CLI
//! contract. Unix only: the fake is a `sh` script.
#![cfg(unix)]
use harness::manifest;
use harness::stage::{StageCtx, StageReport, Status};
use harness::stages::client_flythrough::{Options, run_with};
use install::Detection;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
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
  case $1 in --out) out=$2; shift;; --fullscreen) kind=$2; shift;; --map) map=$2; shift;; esac
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
row2='2,10.00,-20.50,64.25,-90.00,10.00,0.00,65.00,80.69'
[ "$beh" = badpose ] && row2='2,11.00,-20.50,64.25,-90.00,10.00,0.00,65.00,80.69'
printf 'frame,x,y,z,yaw,pitch,roll,cg_fov,fov_x\n1,0.00,0.00,64.25,-90.00,10.00,0.00,65.00,80.69\n%s\n3,20.00,-20.50,64.25,-90.00,10.00,0.00,65.00,80.69\n' "$row2" > "$out/frames.pose.csv"
pose='{"map":"'$map'","frame":2,"origin":[10.0,-20.5,64.25],"yaw":-90.0,"pitch":10.0,"roll":0.0,"cg_fov":65.0,"fov_x":80.69,"width":1280,"height":720,"setviewpos":"setviewpos 10.00 -20.50 64.25 -90.00 10.00"}'
[ "$beh" = fellback ] && [ "$kind" = exclusive ] && kind=borderless
health='{"device_losses":[],"errors":[]}'
[ "$beh" = gpuerror ] && health='{"device_losses":[],"errors":["Buffer is invalid"]}'
video='{"file":"flythrough.ivf","frames":90,"dropped":0,"source_width":2560,"source_height":1440,"width":1280,"height":720,"fps":30,"ring_full":0,"queue_full":0,"map_failed":0,"gpu_reset":0,"pacing_missed":0,"first_slot":0,"last_slot":89,"first_encoded_slot":0,"last_encoded_slot":89,"duration_ticks":90}'
[ -f "$here/video.json" ] && video=$(cat "$here/video.json")
echo '{"status":"ok","frames":3,"video":'"$video"',"screenshot":"screenshot.png","pose":'"$pose"',"zone_load_ms":42.0,"world":{"surfaces_drawn_p50":7},"mode":{"fullscreen":"'$kind'"},"gpu_health":'$health'}' > "$out/client.json"
"#;

struct Fake {
    dir: tempfile::TempDir,
}

/// The fake client script, written once for the whole test binary.
///
/// Writing an executable in one test thread while another forks races:
/// the forked child inherits the open write descriptor until it execs, and
/// exec of a file someone holds open for writing fails with ETXTBSY (#538).
/// Every test forces this before it spawns anything, so no fork can overlap
/// the write; each fake then reaches it through a symlink (no descriptor),
/// and `$0` stays the symlink so the script reads its own fake's files.
/// Statics are not dropped, so the few-hundred-byte directory outlives the
/// test binary in the temp dir.
static SCRIPT_DIR: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("cod4e");
    fs::write(&exe, SCRIPT).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    dir
});

impl Fake {
    fn new(behavior: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(SCRIPT_DIR.path().join("cod4e"), dir.path().join("cod4e"))
            .unwrap();
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
            "frames.pose.csv",
        ] {
            assert!(r.files.contains(&format!("{s}/{f}")), "{s}/{f} not listed");
        }
        let p = r.series[&format!("{s}.frame.cpu_ms")];
        assert_eq!((p.n, p.p50, p.max), (3, 2.0, 3.0));
        // The empty gpu_ms cell is not a sample.
        assert_eq!(r.series[&format!("{s}.frame.gpu_ms")].n, 2);
        assert_eq!(r.metrics[&format!("{s}.zone_load_ms")], 42.0);
        assert_eq!(r.metrics[&format!("{s}.video_bytes")], 5.0);
        assert_eq!(r.metrics[&format!("{s}.video_fps")], 30.0);
        for loss in [
            "ring_full",
            "queue_full",
            "map_failed",
            "gpu_reset",
            "pacing_missed",
        ] {
            assert_eq!(r.metrics[&format!("{s}.video_{loss}")], 0.0);
        }
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains(s) && n.contains("2560x1440 -> 1280x720 at 30 fps"))
        );
    }
    let args = fs::read_to_string(dir.join("exclusive-2560x1440-30000mhz/args.txt")).unwrap();
    for want in [
        "--install /fake/install",
        "--map mp_crash --present auto --flythrough --duration 3",
        "--video --screenshot --size 2560x1440 --refresh 30 --fullscreen exclusive",
    ] {
        assert!(args.contains(want), "{want} not in {args}");
    }
    let env = r.environment.as_ref().unwrap();
    assert_eq!(env["gpu"]["name"], "Fake GPU");
    assert_eq!(env["display_modes"]["present_modes"][1], "mailbox");
    let runs = env["display_modes"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 8);
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
    assert_eq!(r.notes.len(), 8);
    assert!(
        r.notes.iter().all(|n| n.contains("no adapter")),
        "{:?}",
        r.notes
    );

    fake.behave("hang");
    let (r, dir, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(r.notes.len(), 8, "a hung run must not stop the others");
    assert!(r.notes.iter().all(|n| n.contains("Hung")), "{:?}", r.notes);
    assert!(Path::new(&dir.join("native-borderless/client.log")).exists());
}

#[test]
fn an_exclusive_run_that_was_not_exclusive_fails_and_says_so() {
    let fake = Fake::new("fellback");
    let (r, _, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    let failed = r.reason.unwrap();
    assert!(failed.contains("exclusive-2560x1440-30000mhz"), "{failed}");
}

#[test]
fn graphics_errors_the_client_reports_fail_every_run() {
    let fake = Fake::new("gpuerror");
    let (r, _, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(
        r.notes
            .iter()
            .filter(|n| n.contains("Buffer is invalid"))
            .count(),
        8
    );
}

#[test]
fn a_screenshot_pose_that_is_not_its_frames_camera_fails_every_run() {
    let fake = Fake::new("badpose");
    let (r, _, _k) = run(&fake, true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(
        r.notes
            .iter()
            .filter(|n| n.contains("not the screenshot's pose"))
            .count(),
        8
    );
    assert!(
        r.notes
            .iter()
            .filter(|n| !n.contains(": recording "))
            .all(|n| n.contains("not the screenshot's pose")),
        "{:?}",
        r.notes
    );
}

fn video_fixture() -> serde_json::Value {
    serde_json::json!({
        "file": "flythrough.ivf", "frames": 90, "dropped": 0,
        "source_width": 2560, "source_height": 1440,
        "width": 1280, "height": 720, "fps": 30,
        "ring_full": 0, "queue_full": 0, "map_failed": 0, "gpu_reset": 0,
        "pacing_missed": 0, "first_slot": 0, "last_slot": 89,
        "first_encoded_slot": 0, "last_encoded_slot": 89, "duration_ticks": 90
    })
}

fn run_video(video: &serde_json::Value) -> StageReport {
    let fake = Fake::new("ok");
    fs::write(fake.dir.path().join("video.json"), video.to_string()).unwrap();
    run(&fake, true).0
}

#[test]
fn video_contract_fields_are_required() {
    let fixture = video_fixture();
    for key in fixture.as_object().unwrap().keys() {
        let mut video = fixture.clone();
        video.as_object_mut().unwrap().remove(key);
        let r = run_video(&video);
        assert_eq!(r.status, Status::Failed, "missing {key}: {r:?}");
        assert!(
            r.notes.iter().all(|n| n.contains("bad client.json")),
            "{key}: {r:?}"
        );
    }
}

#[test]
fn video_accounting_and_coverage_inconsistencies_fail() {
    let cases = [
        ("loss breakdown", serde_json::json!({"ring_full": 1})),
        ("due interval", serde_json::json!({"frames": 89})),
        ("due endpoints", serde_json::json!({"first_slot": null})),
        ("due endpoints", serde_json::json!({"last_slot": null})),
        ("due endpoints", serde_json::json!({"first_slot": 90})),
        ("due endpoints", serde_json::json!({"last_slot": u64::MAX})),
        ("duration_ticks", serde_json::json!({"duration_ticks": 89})),
        (
            "duration_ticks",
            serde_json::json!({"first_encoded_slot": null}),
        ),
        (
            "duration_ticks",
            serde_json::json!({"last_encoded_slot": null}),
        ),
        (
            "outside due",
            serde_json::json!({"last_encoded_slot": 90, "duration_ticks": 91}),
        ),
        (
            "outside due",
            serde_json::json!({"first_slot": 1, "frames": 89}),
        ),
        (
            "outside due",
            serde_json::json!({"first_encoded_slot": 89, "last_encoded_slot": 0}),
        ),
        (
            "endpoint gaps",
            serde_json::json!({"first_encoded_slot": 5, "frames": 85, "pacing_missed": 5, "duration_ticks": 85}),
        ),
        (
            "startup allowance",
            serde_json::json!({"first_slot": 31, "last_slot": 120, "first_encoded_slot": 31, "last_encoded_slot": 120}),
        ),
        (
            "duration minus",
            serde_json::json!({"frames": 59, "last_slot": 58, "last_encoded_slot": 58, "duration_ticks": 59}),
        ),
        (
            "too few frames",
            serde_json::json!({"frames": 0, "dropped": 90, "ring_full": 90, "first_encoded_slot": null, "last_encoded_slot": null, "duration_ticks": 0}),
        ),
        (
            "loss breakdown",
            serde_json::json!({"ring_full": u64::MAX, "queue_full": 1}),
        ),
        (
            "due interval",
            serde_json::json!({"frames": u64::MAX, "pacing_missed": 1}),
        ),
    ];
    for (message, patch) in cases {
        let mut video = video_fixture();
        video
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let r = run_video(&video);
        assert_eq!(r.status, Status::Failed, "{patch}: {r:?}");
        assert!(
            r.notes.iter().any(|n| n.contains(message)),
            "{message}: {r:?}"
        );
    }
}

#[test]
fn every_loss_class_and_pacing_count_toward_five_percent_limit() {
    for class in [
        "ring_full",
        "queue_full",
        "map_failed",
        "gpu_reset",
        "pacing_missed",
    ] {
        for (lost, expected) in [(4, Status::Passed), (5, Status::Failed)] {
            let mut video = video_fixture();
            video["frames"] = (90 - lost).into();
            video[class] = lost.into();
            if class != "pacing_missed" {
                video["dropped"] = lost.into();
            }
            let r = run_video(&video);
            assert_eq!(r.status, expected, "{class}={lost}: {r:?}");
            assert_eq!(
                r.metrics[&format!("native-borderless.video_{class}")],
                f64::from(lost)
            );
            if expected == Status::Failed {
                assert!(
                    r.notes
                        .iter()
                        .any(|n| n.contains("missing source opportunities exceed 5%")),
                    "{r:?}"
                );
            }
        }
    }
    let mut video = video_fixture();
    video["frames"] = 85.into();
    video["dropped"] = 2.into();
    video["queue_full"] = 2.into();
    video["pacing_missed"] = 3.into();
    assert_eq!(run_video(&video).status, Status::Failed);
}

#[test]
fn video_dimensions_follow_actual_source_not_requested_mode() {
    for (source_width, source_height, width, height) in [
        (5120, 1440, 1280, 360),
        (2536, 1416, 1280, 714),
        (641, 481, 640, 480),
        (640, 360, 640, 360),
        (720, 1280, 404, 720),
    ] {
        let mut video = video_fixture();
        video["source_width"] = source_width.into();
        video["source_height"] = source_height.into();
        video["width"] = width.into();
        video["height"] = height.into();
        let r = run_video(&video);
        assert_eq!(r.status, Status::Passed, "{video}: {r:?}");
    }
}

#[test]
fn invalid_video_dimensions_and_target_fps_fail() {
    for patch in [
        serde_json::json!({"width": 1281}),
        serde_json::json!({"height": 721}),
        serde_json::json!({"width": 0}),
        serde_json::json!({"height": 0}),
        serde_json::json!({"source_width": 0}),
        serde_json::json!({"source_height": 0}),
        serde_json::json!({"source_width": 640, "source_height": 360}),
        serde_json::json!({"source_width": 5120, "source_height": 1440}),
        serde_json::json!({"width": 640, "height": 360}),
        serde_json::json!({"fps": 60}),
    ] {
        let mut video = video_fixture();
        video
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let r = run_video(&video);
        assert_eq!(r.status, Status::Failed, "{patch}: {r:?}");
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("video dimensions") || n.contains("video target fps")),
            "{r:?}"
        );
    }
}

#[test]
fn video_coverage_allowances_are_inclusive() {
    let mut video = video_fixture();
    video["frames"] = 95.into();
    video["pacing_missed"] = 5.into();
    video["first_slot"] = 30.into();
    video["last_slot"] = 129.into();
    video["first_encoded_slot"] = 35.into();
    video["last_encoded_slot"] = 129.into();
    video["duration_ticks"] = 95.into();
    let r = run_video(&video);
    assert_eq!(r.status, Status::Passed, "{r:?}");
    assert_eq!(r.metrics["native-borderless.video_missing_share"], 0.05);

    // Exactly duration minus one second of due coverage is accepted.
    video["frames"] = 60.into();
    video["pacing_missed"] = 0.into();
    video["first_slot"] = 0.into();
    video["last_slot"] = 59.into();
    video["first_encoded_slot"] = 0.into();
    video["last_encoded_slot"] = 59.into();
    video["duration_ticks"] = 60.into();
    let r = run_video(&video);
    assert_eq!(r.status, Status::Passed, "{r:?}");
}
