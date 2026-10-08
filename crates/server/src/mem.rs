// SPDX-License-Identifier: GPL-3.0-only
//! Process memory, for the server's own reports.

/// Resident set size in bytes, where the platform tells us cheaply.
pub fn rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        Some(pages * page)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Peak resident set size in bytes.
pub fn peak_rss() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: getrusage fills the zeroed struct we pass.
        let ru = unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
                return None;
            }
            ru
        };
        let n = u64::try_from(ru.ru_maxrss).ok()?;
        // Linux reports KiB, macOS bytes.
        Some(if cfg!(target_os = "macos") {
            n
        } else {
            n * 1024
        })
    }
    #[cfg(not(unix))]
    {
        None
    }
}
