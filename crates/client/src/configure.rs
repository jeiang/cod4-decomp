// SPDX-License-Identifier: GPL-3.0-only
//! `Com_SetRecommended` (the video options' Recommended button): `configure_mp.csv` names, per CPU class (GHz and
//! memory) and per GPU name pattern, the dvars a machine should run with. The CPU section's columns are `cpu ghz`,
//! `sys mb` and dvar names, a row per class; the GPU section starts at a `gpu` row, whose other columns are dvar
//! names, and each row after it is a name pattern (a `*` matches any run) with the values. The best CPU row is the
//! highest class the machine reaches; the first GPU row whose pattern the adapter name contains wins.

/// What a machine should run with: the lines the original prints and the dvars to set (and archive).
pub struct Recommended {
    pub cpu_line: String,
    pub gpu_line: String,
    pub dvars: Vec<(String, String)>,
}

/// The original benchmarks a core and scales by the core count (`Sys_GetInfo`: one core 1, two 1.75, more 2). A
/// 2007 core measured about 3 GHz; every machine that runs this is at least that.
fn configure_ghz() -> f64 {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let factor = match cores {
        1 => 1.0,
        2 => 1.75,
        _ => 2.0,
    };
    3.0 * factor * 1.02
}

/// Physical memory in MB: read where the system tells it; the CSV's largest class (640 MB) otherwise, which every
/// machine that runs this has.
fn sys_mb() -> i64 {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo")
        && let Some(kb) = text
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.trim().split(' ').next())
            .and_then(|v| v.parse::<i64>().ok())
    {
        return (kb / 1024 + 8).max(128);
    }
    4096
}

/// One CSV line as cells: a quoted cell may hold commas, and a doubled quote is one.
fn cells(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                cell.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    out.push(cell);
    out.into_iter().map(|c| c.trim().to_owned()).collect()
}

/// `Com_DoesGpuStringMatch`: the pattern, with its runs of blanks folded, must appear in the name, `*` matching
/// anything, case-insensitively.
pub fn gpu_matches(pattern: &str, name: &str) -> bool {
    let folded: String = pattern
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let template = format!(
        "*{}{}",
        folded,
        if folded.ends_with('*') { "" } else { "*" }
    );
    fn at(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| at(rest, &n[i..])),
            Some((c, rest)) => n.first() == Some(c) && at(rest, &n[1..]),
        }
    }
    at(template.as_bytes(), name.to_ascii_lowercase().as_bytes())
}

pub fn recommended(csv: &str, gpu: &str) -> Result<Recommended, String> {
    recommended_for(csv, gpu, configure_ghz(), sys_mb())
}

fn recommended_for(csv: &str, gpu: &str, ghz: f64, mb: i64) -> Result<Recommended, String> {
    let rows: Vec<Vec<String>> = csv
        .lines()
        .map(cells)
        .filter(|r| !r.iter().all(String::is_empty) && !r[0].starts_with('#'))
        .collect();
    let gpu_at = rows
        .iter()
        .position(|r| r[0].eq_ignore_ascii_case("gpu"))
        .ok_or("no gpu section")?;
    let (cpu, gpus) = rows.split_at(gpu_at);
    let Some((cpu_head, cpu_rows)) = cpu.split_first() else {
        return Err("no cpu section".into());
    };
    if !cpu_head
        .first()
        .is_some_and(|c| c.eq_ignore_ascii_case("cpu ghz"))
        || !cpu_head
            .get(1)
            .is_some_and(|c| c.eq_ignore_ascii_case("sys mb"))
    {
        return Err("\"cpu ghz\" and \"sys mb\" should be the first columns".into());
    }
    let mut best: Option<(f64, i64, &Vec<String>)> = None;
    for r in cpu_rows {
        let g: f64 = r[0].parse().map_err(|_| format!("bad cpu ghz {}", r[0]))?;
        let m: i64 = r
            .get(1)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("bad sys mb in the {g} GHz row"))?;
        if g <= ghz && mb >= m && best.is_none_or(|(bg, bm, _)| g > bg || (g == bg && m > bm)) {
            best = Some((g, m, r));
        }
    }
    let (g, m, cpu_row) = best.ok_or("no cpu class fits this machine")?;
    let mut dvars: Vec<(String, String)> = cpu_head[2..]
        .iter()
        .zip(&cpu_row[2..])
        .filter(|(n, _)| !n.is_empty())
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();
    let (gpu_head, gpu_rows) = gpus.split_first().ok_or("no gpu section")?;
    let gpu_row = gpu_rows
        .iter()
        .find(|r| gpu_matches(&r[0], gpu))
        .ok_or_else(|| format!("no gpu row matches {gpu:?}"))?;
    dvars.extend(
        gpu_head[1..]
            .iter()
            .zip(&gpu_row[1..])
            .filter(|(n, _)| !n.is_empty())
            .map(|(n, v)| (n.clone(), v.clone())),
    );
    Ok(Recommended {
        cpu_line: format!("configure_mp.csv: using CPU configuration {g:.0} GHz {m} MB"),
        gpu_line: format!(
            "configure_mp.csv: using GPU configuration \"{}\"",
            gpu_row[0]
        ),
        dvars,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "# comment\n\ncpu ghz,sys mb,ragdoll_enable,r_lodScaleRigid\n0,128,0,2\n3,128,1,1.5\n6,640,1,1\n\ngpu,r_aaSamples,sm_enable\nGeForce FX 59*,1,0\nRadeon  X*,2,1\n*,4,1\n";

    #[test]
    fn picks_the_highest_cpu_class_reached_and_the_first_gpu_pattern() {
        let r = recommended_for(CSV, "AMD Radeon X1900", 6.1, 2048).unwrap();
        assert_eq!(
            r.cpu_line,
            "configure_mp.csv: using CPU configuration 6 GHz 640 MB"
        );
        assert_eq!(
            r.gpu_line,
            "configure_mp.csv: using GPU configuration \"Radeon  X*\""
        );
        assert_eq!(
            r.dvars,
            [
                ("ragdoll_enable", "1"),
                ("r_lodScaleRigid", "1"),
                ("r_aaSamples", "2"),
                ("sm_enable", "1")
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        let r = recommended_for(CSV, "RX 9070", 3.5, 256).unwrap();
        assert!(r.cpu_line.contains("3 GHz 128 MB"), "{}", r.cpu_line);
        assert_eq!(r.dvars[2], ("r_aaSamples".to_owned(), "4".to_owned()));
    }

    #[test]
    fn gpu_patterns_fold_blanks_and_match_inside_the_name() {
        assert!(gpu_matches(
            "GeForce   FX 59*",
            "NVIDIA GeForce FX 5900 Ultra"
        ));
        assert!(!gpu_matches("GeForce FX 59*", "GeForce 6800"));
        assert!(gpu_matches("*", "anything"));
    }

    #[test]
    fn a_malformed_file_is_refused() {
        assert!(recommended_for("gpu,x\n*,1\n", "g", 6.0, 640).is_err());
        assert!(recommended_for("cpu ghz,sys mb\n0,128\n", "g", 6.0, 640).is_err());
    }
}
