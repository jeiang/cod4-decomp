// SPDX-License-Identifier: GPL-3.0-only
//! Banned addresses: permanent ones, kept in a file that survives a restart (one address per line),
//! and brief ones (`kick`, `tempBanUser`, `sv_kickBanTime`) that expire.
//!
//! The original bans by the player's key hash (`SV_BanGuidBriefly`, `ban.txt`); this server has no
//! such key, so the address is the identity.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct BanList {
    permanent: BTreeSet<IpAddr>,
    brief: Vec<(IpAddr, Instant)>,
    /// Where the permanent bans are kept; none for a list that lives in memory only.
    file: Option<PathBuf>,
}

impl BanList {
    /// The list kept in `file`; a missing or unreadable file is an empty list. Lines that are not
    /// addresses (and `#` comments) are skipped.
    pub fn load(file: PathBuf) -> Self {
        let permanent = std::fs::read_to_string(&file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split('#').next()?.trim().parse().ok())
            .collect();
        Self {
            permanent,
            brief: Vec::new(),
            file: Some(file),
        }
    }

    fn save(&self) -> Result<(), String> {
        let Some(f) = &self.file else { return Ok(()) };
        let text: String = self.permanent.iter().map(|ip| format!("{ip}\n")).collect();
        std::fs::write(f, text).map_err(|e| format!("{}: {e}", f.display()))
    }

    /// Bans `ip` for good. An error means the ban holds for this run but could not be written down.
    pub fn ban(&mut self, ip: IpAddr) -> Result<(), String> {
        self.permanent.insert(ip);
        self.save()
    }

    /// Bans `ip` until `secs` seconds after `now`.
    pub fn ban_briefly(&mut self, ip: IpAddr, now: Instant, secs: u64) {
        self.brief.retain(|(i, _)| *i != ip);
        self.brief.push((ip, now + Duration::from_secs(secs)));
    }

    /// Lifts both kinds of ban; whether there was one.
    pub fn unban(&mut self, ip: IpAddr) -> Result<bool, String> {
        let before = self.brief.len();
        self.brief.retain(|(i, _)| *i != ip);
        let was = self.permanent.remove(&ip) || self.brief.len() != before;
        if was {
            self.save()?;
        }
        Ok(was)
    }

    pub fn is_banned(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.brief.retain(|(_, until)| *until > now);
        self.permanent.contains(&ip) || self.brief.iter().any(|(i, _)| *i == ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 1, 2, 3));
    const B: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 1, 2, 4));

    #[test]
    fn permanent_bans_survive_a_restart_and_unban_is_written_too() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ban.txt");
        let mut l = BanList::load(f.clone());
        let now = Instant::now();
        assert!(!l.is_banned(A, now));
        l.ban(A).unwrap();
        l.ban(B).unwrap();
        let mut again = BanList::load(f.clone());
        assert!(again.is_banned(A, now) && again.is_banned(B, now));
        assert!(again.unban(A).unwrap());
        assert!(!again.unban(A).unwrap());
        let mut third = BanList::load(f);
        assert!(!third.is_banned(A, now) && third.is_banned(B, now));
    }

    #[test]
    fn a_brief_ban_ends_on_time_and_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ban.txt");
        let mut l = BanList::load(f.clone());
        let t0 = Instant::now();
        l.ban_briefly(A, t0, 300);
        assert!(l.is_banned(A, t0 + Duration::from_secs(299)));
        assert!(!l.is_banned(A, t0 + Duration::from_secs(300)));
        l.ban_briefly(B, t0, 300);
        assert!(!BanList::load(f).is_banned(B, t0));
    }

    #[test]
    fn unban_lifts_a_brief_ban_and_comments_in_the_file_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ban.txt");
        std::fs::write(&f, "# old\nnot an address\n10.1.2.4 # spammer\n").unwrap();
        let mut l = BanList::load(f);
        let now = Instant::now();
        assert!(l.is_banned(B, now) && !l.is_banned(A, now));
        l.ban_briefly(A, now, 60);
        assert!(l.unban(A).unwrap());
        assert!(!l.is_banned(A, now));
    }
}
