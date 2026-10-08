// SPDX-License-Identifier: GPL-3.0-only
//! `harness diff a.zip b.zip`: percentile deltas per stage, environment
//! differences, and log errors that are new in `b`.
use crate::stage::StageReport;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

pub struct Bundle {
    pub name: String,
    pub manifest: Value,
    pub stages: Vec<StageReport>,
    /// `(bundle path, text)` of every log.
    pub logs: Vec<(String, String)>,
}

pub fn open(path: &Path) -> io::Result<Bundle> {
    let bad = |m: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {m}", path.display()),
        )
    };
    let mut z = zip::ZipArchive::new(File::open(path)?).map_err(|e| bad(e.to_string()))?;
    let mut read = |name: &str| -> io::Result<Option<String>> {
        let Ok(mut f) = z.by_name(name) else {
            return Ok(None);
        };
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        Ok(Some(s))
    };
    let json = |name: &str, s: Option<String>| -> io::Result<Value> {
        let s = s.ok_or_else(|| bad(format!("not a harness bundle: no {name}")))?;
        serde_json::from_str(&s).map_err(|e| bad(format!("{name}: {e}")))
    };
    let manifest = json("manifest.json", read("manifest.json")?)?;
    let summary = json("summary.json", read("summary.json")?)?;
    let stages = serde_json::from_value(summary["stages"].clone())
        .map_err(|e| bad(format!("summary.json stages: {e}")))?;
    let names: Vec<String> = z
        .file_names()
        .filter(|n| n.ends_with(".log"))
        .map(str::to_owned)
        .collect();
    let mut logs = Vec::new();
    for n in names {
        let mut s = String::new();
        z.by_name(&n)?.read_to_string(&mut s).ok();
        logs.push((n, s));
    }
    Ok(Bundle {
        name: path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        manifest,
        stages,
        logs,
    })
}

fn flatten(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) => m.iter().for_each(|(k, v)| {
            flatten(
                &if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                },
                v,
                out,
            )
        }),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, v)| flatten(&format!("{prefix}[{i}]"), v, out)),
        Value::String(s) => {
            out.insert(prefix.to_owned(), s.clone());
        }
        other => {
            out.insert(prefix.to_owned(), other.to_string());
        }
    }
}

/// Manifest keys that differ on every run and say nothing about the machine.
const NOISE: [&str; 2] = ["created", "install.tried"];

fn is_noise(key: &str) -> bool {
    NOISE
        .iter()
        .any(|n| key == *n || key.starts_with(&format!("{n}[")))
}

fn pct(a: f64, b: f64) -> String {
    if a == 0.0 {
        return if b == 0.0 { "0%".into() } else { "new".into() };
    }
    format!("{:+.1}%", (b - a) / a * 100.0)
}

fn num(v: f64) -> String {
    if v.abs() >= 1e6 || (v != 0.0 && v.abs() < 0.01) {
        format!("{v:.3e}")
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

/// The text of an error-looking log line with digits and hex blurred, so
/// timings and addresses do not make old errors look new.
fn error_key(line: &str) -> Option<String> {
    let l = line.to_ascii_lowercase();
    if !["error", "panic", "fatal", "failed", "crash"]
        .iter()
        .any(|w| l.contains(w))
    {
        return None;
    }
    Some(
        line.chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect(),
    )
}

pub fn diff(a: &Bundle, b: &Bundle, out: &mut impl Write) -> io::Result<()> {
    writeln!(
        out,
        "a: {}  (build {}, {})",
        a.name,
        a.manifest["build"]["hash"].as_str().unwrap_or("?"),
        a.manifest["created"].as_str().unwrap_or("?")
    )?;
    writeln!(
        out,
        "b: {}  (build {}, {})",
        b.name,
        b.manifest["build"]["hash"].as_str().unwrap_or("?"),
        b.manifest["created"].as_str().unwrap_or("?")
    )?;

    writeln!(out, "\nenvironment")?;
    let (mut fa, mut fb) = (BTreeMap::new(), BTreeMap::new());
    flatten("", &a.manifest, &mut fa);
    flatten("", &b.manifest, &mut fb);
    let keys: BTreeSet<_> = fa
        .keys()
        .chain(fb.keys())
        .filter(|k| !is_noise(k))
        .collect();
    let mut same = true;
    for k in keys {
        let (x, y) = (fa.get(k), fb.get(k));
        if x != y {
            same = false;
            writeln!(
                out,
                "  {k}: {} -> {}",
                x.map_or("-", String::as_str),
                y.map_or("-", String::as_str)
            )?;
        }
    }
    if same {
        writeln!(out, "  identical")?;
    }

    let by_name = |s: &[StageReport]| {
        s.iter()
            .map(|r| (r.name.clone(), r.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (sa, sb) = (by_name(&a.stages), by_name(&b.stages));
    for name in sa.keys().chain(sb.keys()).collect::<BTreeSet<_>>() {
        writeln!(out, "\nstage {name}")?;
        let (Some(x), Some(y)) = (sa.get(name), sb.get(name)) else {
            writeln!(
                out,
                "  only in {}",
                if sa.contains_key(name) { "a" } else { "b" }
            )?;
            continue;
        };
        if x.status == y.status {
            writeln!(out, "  status: {:?}", x.status)?;
        } else {
            writeln!(
                out,
                "  status: {:?} -> {:?}{}",
                x.status,
                y.status,
                y.reason
                    .as_deref()
                    .map_or(String::new(), |r| format!(" ({r})"))
            )?;
        }
        for k in x
            .metrics
            .keys()
            .chain(y.metrics.keys())
            .collect::<BTreeSet<_>>()
        {
            match (x.metrics.get(k), y.metrics.get(k)) {
                (Some(p), Some(q)) if p == q => {}
                (Some(p), Some(q)) => {
                    writeln!(out, "  {k}: {} -> {} ({})", num(*p), num(*q), pct(*p, *q))?
                }
                (p, q) => writeln!(
                    out,
                    "  {k}: {} -> {}",
                    p.map_or("-".into(), |v| num(*v)),
                    q.map_or("-".into(), |v| num(*v))
                )?,
            }
        }
        for k in x
            .series
            .keys()
            .chain(y.series.keys())
            .collect::<BTreeSet<_>>()
        {
            let (Some(p), Some(q)) = (x.series.get(k), y.series.get(k)) else {
                writeln!(
                    out,
                    "  {k}: only in {}",
                    if x.series.contains_key(k) { "a" } else { "b" }
                )?;
                continue;
            };
            writeln!(out, "  {k} (n {} -> {})", p.n, q.n)?;
            for (label, u, v) in [
                ("p50", p.p50, q.p50),
                ("p95", p.p95, q.p95),
                ("p99", p.p99, q.p99),
                ("max", p.max, q.max),
            ] {
                writeln!(
                    out,
                    "    {label:<4}{:>12} -> {:<12} {}",
                    num(u),
                    num(v),
                    pct(u, v)
                )?;
            }
        }
    }

    let errors = |bundle: &Bundle| -> BTreeMap<String, String> {
        bundle
            .logs
            .iter()
            .flat_map(|(_, text)| text.lines())
            .filter_map(|l| Some((error_key(l)?, l.to_owned())))
            .collect()
    };
    let (ea, eb) = (errors(a), errors(b));
    let new: Vec<_> = eb
        .iter()
        .filter(|(k, _)| !ea.contains_key(*k))
        .map(|(_, l)| l)
        .collect();
    writeln!(out, "\nnew log errors")?;
    if new.is_empty() {
        writeln!(out, "  none")?;
    }
    for l in new {
        writeln!(out, "  {l}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::Percentiles;
    use crate::stage::Status;
    use serde_json::json;

    fn bundle(hash: &str, cpu: &str, p50: f64, log: &str) -> Bundle {
        let mut r = StageReport::new("asset-load", Status::Passed);
        r.series.insert(
            "zone.open_ms".into(),
            Percentiles {
                n: 4,
                mean: p50,
                p50,
                p95: p50 * 2.0,
                p99: p50 * 3.0,
                max: p50 * 4.0,
            },
        );
        r.metrics.insert("zones.total".into(), 47.0);
        Bundle {
            name: format!("{hash}.zip"),
            manifest: json!({"created": hash, "build": {"hash": hash}, "cpu": {"brand": cpu}, "install": {"tried": [hash]}}),
            stages: vec![r],
            logs: vec![("stages/01/stderr.log".into(), log.into())],
        }
    }

    #[test]
    fn reports_deltas_environment_and_new_errors_only() {
        let a = bundle(
            "aaa",
            "M1",
            10.0,
            "ERROR zone x: failed after 12 ms\nINFO fine",
        );
        let b = bundle(
            "bbb",
            "M3",
            15.0,
            "ERROR zone x: failed after 99 ms\nERROR zone y: bad pointer 0x12",
        );
        let mut out = Vec::new();
        diff(&a, &b, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("build.hash: aaa -> bbb"), "{s}");
        assert!(s.contains("cpu.brand: M1 -> M3"));
        assert!(!s.contains("install.tried") && !s.contains("created:"));
        assert!(s.contains("p50") && s.contains("+50.0%"), "{s}");
        assert!(!s.contains("zones.total"), "unchanged metrics stay quiet");
        let tail = s.split("new log errors").nth(1).unwrap();
        assert!(
            tail.contains("bad pointer") && !tail.contains("zone x"),
            "{tail}"
        );
    }

    #[test]
    fn identical_bundles_say_so() {
        let a = bundle("aaa", "M1", 10.0, "");
        let mut out = Vec::new();
        diff(&a, &bundle("aaa", "M1", 10.0, ""), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("identical") && s.contains("none"));
    }
}
