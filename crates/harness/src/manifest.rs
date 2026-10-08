// SPDX-License-Identifier: GPL-3.0-only
//! `manifest.json`: what built, ran and hosted this run. No host name, user
//! name, serial number or home path (the bundle writer scrubs the last two).
use crate::install::Detection;
use crate::stage::StageReport;
use serde_json::{Value, json};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

pub const SCHEMA: u32 = 1;

pub fn build_hash() -> &'static str {
    env!("COD4E_BUILD_HASH")
}

pub fn collect(
    unix_time: u64,
    install: &Detection,
    mp_zone_count: Option<usize>,
    crash_capture: Result<(), &str>,
    stages: &[StageReport],
) -> Value {
    let sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let cpus = sys.cpus();
    let mut m = json!({
        "schema": SCHEMA,
        "created": crate::time::iso(unix_time),
        "build": {
            "hash": build_hash(),
            "version": env!("CARGO_PKG_VERSION"),
            "profile": env!("COD4E_BUILD_PROFILE"),
            "target": env!("COD4E_BUILD_TARGET"),
        },
        "os": {
            "family": std::env::consts::OS,
            "name": System::name(),
            "version": System::os_version(),
            "kernel": System::kernel_version(),
            "arch": std::env::consts::ARCH,
        },
        "cpu": {
            "brand": cpus.first().map(|c| c.brand().to_owned()),
            "physical_cores": System::physical_core_count(),
            "logical_cores": cpus.len(),
        },
        "memory_bytes": sys.total_memory(),
        "gpu": null,
        "display_modes": null,
        "cvars": null,
        "install": {
            "found": install.install.is_some(),
            "source": install.install.as_ref().map(|i| i.source.clone()),
            "path": install.install.as_ref().map(|i| i.path.display().to_string()),
            "mp_zones": mp_zone_count,
            "tried": install.tried,
        },
        "crash_capture": match crash_capture {
            Ok(()) => json!("minidumper"),
            Err(e) => json!(format!("unavailable: {e}")),
        },
    });
    // Stages that open a window or run a server know the GPU, display modes
    // and cvars; the last report that has them wins.
    for key in ["gpu", "display_modes", "cvars"] {
        for env in stages.iter().filter_map(|s| s.environment.as_ref()) {
            if let Some(v) = env.get(key) {
                m[key] = v.clone();
            }
        }
    }
    m
}
