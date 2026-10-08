// SPDX-License-Identifier: GPL-3.0-only
//! Performance data plumbing: percentile summaries and the per-frame and
//! per-tick CSV writers. Clients and servers feed these during perf stages.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Distribution summary of one measured series.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Percentiles {
    /// Nearest-rank percentiles; `None` for an empty series.
    pub fn from_samples(samples: &[f64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut v = samples.to_vec();
        v.sort_by(f64::total_cmp);
        let rank = |p: f64| v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1];
        Some(Self {
            n: v.len(),
            mean: v.iter().sum::<f64>() / v.len() as f64,
            p50: rank(0.50),
            p95: rank(0.95),
            p99: rank(0.99),
            max: v[v.len() - 1],
        })
    }
}

/// One rendered frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameSample {
    pub cpu_ms: f64,
    /// From timestamp queries, where the adapter supports them.
    pub gpu_ms: Option<f64>,
    pub present_interval_ms: f64,
    pub mem_bytes: u64,
}

/// Writes `frames.csv` and accumulates its series.
pub struct FrameRecorder {
    out: BufWriter<File>,
    cpu: Vec<f64>,
    gpu: Vec<f64>,
    present: Vec<f64>,
    mem: Vec<f64>,
}

impl FrameRecorder {
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        writeln!(out, "frame,cpu_ms,gpu_ms,present_interval_ms,mem_bytes")?;
        Ok(Self {
            out,
            cpu: vec![],
            gpu: vec![],
            present: vec![],
            mem: vec![],
        })
    }

    pub fn push(&mut self, s: FrameSample) -> io::Result<()> {
        let gpu = s.gpu_ms.map_or(String::new(), |g| format!("{g:.4}"));
        writeln!(
            self.out,
            "{},{:.4},{gpu},{:.4},{}",
            self.cpu.len(),
            s.cpu_ms,
            s.present_interval_ms,
            s.mem_bytes
        )?;
        self.cpu.push(s.cpu_ms);
        if let Some(g) = s.gpu_ms {
            self.gpu.push(g);
        }
        self.present.push(s.present_interval_ms);
        self.mem.push(s.mem_bytes as f64);
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<BTreeMap<String, Percentiles>> {
        self.out.flush()?;
        Ok(series([
            ("frame.cpu_ms", &self.cpu),
            ("frame.gpu_ms", &self.gpu),
            ("frame.present_interval_ms", &self.present),
            ("frame.mem_bytes", &self.mem),
        ]))
    }
}

/// Writes `ticks.csv`: total tick time, one column per server subsystem and the process
/// resident size when it was last sampled.
pub struct TickRecorder {
    out: BufWriter<File>,
    names: Vec<String>,
    total: Vec<f64>,
    per: Vec<Vec<f64>>,
}

impl TickRecorder {
    pub fn create(path: &Path, subsystems: &[&str]) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        write!(out, "tick,tick_ms")?;
        for s in subsystems {
            write!(out, ",{s}_ms")?;
        }
        writeln!(out, ",rss_bytes")?;
        Ok(Self {
            out,
            names: subsystems.iter().map(|s| (*s).to_owned()).collect(),
            total: vec![],
            per: vec![vec![]; subsystems.len()],
        })
    }

    /// `subsystem_ms` is in the order given to [`TickRecorder::create`].
    pub fn push(&mut self, tick_ms: f64, subsystem_ms: &[f64], rss: u64) -> io::Result<()> {
        assert_eq!(subsystem_ms.len(), self.names.len(), "subsystem count");
        write!(self.out, "{},{tick_ms:.4}", self.total.len())?;
        for (col, ms) in self.per.iter_mut().zip(subsystem_ms) {
            write!(self.out, ",{ms:.4}")?;
            col.push(*ms);
        }
        writeln!(self.out, ",{rss}")?;
        self.total.push(tick_ms);
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<BTreeMap<String, Percentiles>> {
        self.out.flush()?;
        let mut m = series([("tick.ms", &self.total)]);
        for (name, col) in self.names.iter().zip(&self.per) {
            m.extend(series([(format!("tick.{name}_ms").as_str(), col)]));
        }
        Ok(m)
    }
}

fn series<const N: usize>(items: [(&str, &Vec<f64>); N]) -> BTreeMap<String, Percentiles> {
    items
        .into_iter()
        .filter_map(|(k, v)| Some((k.to_owned(), Percentiles::from_samples(v)?)))
        .collect()
}

/// Resident memory of this process in bytes.
pub fn process_rss() -> Option<u64> {
    server::mem::rss().or_else(sysinfo_rss)
}

/// Highest resident size this process has reached, from the kernel's own high-water mark.
pub fn peak_rss() -> Option<u64> {
    server::mem::peak_rss()
}

/// Highest memory this process's cgroup has used (page cache included), which is what
/// `MemoryMax` limits. `None` outside a cgroup-v2 Linux with `memory.peak`.
pub fn cgroup_peak() -> Option<u64> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = own.lines().find_map(|l| l.strip_prefix("0::"))?;
    let file = format!("/sys/fs/cgroup{path}/memory.peak");
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

fn sysinfo_rss() -> Option<u64> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = sysinfo::get_current_pid().ok()?;
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map(|p| p.memory())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let p = Percentiles::from_samples(&v).unwrap();
        assert_eq!(
            (p.p50, p.p95, p.p99, p.max, p.n),
            (50.0, 95.0, 99.0, 100.0, 100)
        );
        assert!(Percentiles::from_samples(&[]).is_none());
        let one = Percentiles::from_samples(&[7.0]).unwrap();
        assert_eq!((one.p50, one.p99, one.max), (7.0, 7.0, 7.0));
    }

    #[test]
    fn recorders_write_csv_and_summaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = FrameRecorder::create(&dir.path().join("frames.csv")).unwrap();
        for i in 0..10 {
            f.push(FrameSample {
                cpu_ms: f64::from(i),
                gpu_ms: (i % 2 == 0).then_some(1.0),
                present_interval_ms: 16.6,
                mem_bytes: 100,
            })
            .unwrap();
        }
        let s = f.finish().unwrap();
        assert_eq!(s["frame.cpu_ms"].n, 10);
        assert_eq!(s["frame.gpu_ms"].n, 5);
        let csv = std::fs::read_to_string(dir.path().join("frames.csv")).unwrap();
        assert_eq!(csv.lines().count(), 11);
        assert!(
            csv.lines()
                .nth(2)
                .unwrap()
                .starts_with("1,1.0000,,16.6000,100")
        );

        let mut t = TickRecorder::create(&dir.path().join("ticks.csv"), &["gsc", "pmove"]).unwrap();
        t.push(2.0, &[1.0, 0.5], 4096).unwrap();
        let s = t.finish().unwrap();
        assert_eq!(s["tick.pmove_ms"].p50, 0.5);
        let csv = std::fs::read_to_string(dir.path().join("ticks.csv")).unwrap();
        assert_eq!(
            csv.lines().next().unwrap(),
            "tick,tick_ms,gsc_ms,pmove_ms,rss_bytes"
        );
        assert_eq!(csv.lines().nth(1).unwrap(), "0,2.0000,1.0000,0.5000,4096");
    }
}
