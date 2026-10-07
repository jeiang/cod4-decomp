// SPDX-License-Identifier: GPL-3.0-or-later
//! Removes user names, home paths and host names from everything that goes
//! into a bundle.

/// Replacement rules, longest first so a home path wins over its user name.
pub struct Scrubber {
    rules: Vec<(String, String)>,
    names: Vec<String>,
}

impl Scrubber {
    /// Rules from the running environment.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let home = var("HOME").or_else(|| var("USERPROFILE"));
        let user = var("USER").or_else(|| var("USERNAME"));
        let host = sysinfo::System::host_name().or_else(|| var("HOSTNAME"));
        Self::new(home.as_deref(), user.as_deref(), host.as_deref())
    }

    pub fn new(home: Option<&str>, user: Option<&str>, host: Option<&str>) -> Self {
        let mut rules: Vec<(String, String)> = Vec::new();
        let mut names = Vec::new();
        // Plain, forward-slash and JSON-escaped spellings of the same path.
        if let Some(home) = home
            .map(|h| h.trim_end_matches(['/', '\\']))
            .filter(|h| h.len() > 1)
        {
            let fwd = home.replace('\\', "/");
            let esc = home.replace('\\', "\\\\");
            for v in [home.to_owned(), fwd, esc] {
                rules.push((v, "<home>".into()));
            }
        }
        // Names shorter than three characters would mangle unrelated text.
        for (n, to) in [(user, "<user>"), (host, "<host>")] {
            if let Some(n) = n.filter(|n| n.len() >= 3) {
                rules.push((n.to_owned(), to.into()));
                names.push(n.to_owned());
            }
        }
        rules.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        rules.dedup();
        Self { rules, names }
    }

    /// Scrub text. Matching ignores ASCII case (Windows paths).
    pub fn text(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for (from, to) in &self.rules {
            out = replace_ci(&out, from, to);
        }
        out
    }

    /// Overwrite user and host names in a binary file (a minidump) in place,
    /// as ASCII and UTF-16LE, keeping every length so offsets stay valid.
    pub fn bytes(&self, data: &mut [u8]) {
        for name in &self.names {
            let ascii = name.as_bytes().to_vec();
            let wide: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            for pat in [ascii, wide] {
                blank_all(data, &pat);
            }
        }
    }
}

fn replace_ci(s: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return s.to_owned();
    }
    let (hay, needle) = (s.to_ascii_lowercase(), from.to_ascii_lowercase());
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    // ASCII lowercasing preserves byte offsets, so indices map back onto `s`.
    while let Some(i) = hay[last..].find(&needle) {
        let at = last + i;
        out.push_str(&s[last..at]);
        out.push_str(to);
        last = at + from.len();
    }
    out.push_str(&s[last..]);
    out
}

fn blank_all(data: &mut [u8], pat: &[u8]) {
    if pat.is_empty() || data.len() < pat.len() {
        return;
    }
    let lower: Vec<u8> = pat.iter().map(u8::to_ascii_lowercase).collect();
    let mut i = 0;
    while i + pat.len() <= data.len() {
        if data[i..i + pat.len()]
            .iter()
            .zip(&lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
        {
            for (j, b) in data[i..i + pat.len()].iter_mut().enumerate() {
                // Keep UTF-16 NUL high bytes as they are.
                if *b != 0 || pat[j] != 0 {
                    *b = b'x';
                }
            }
            i += pat.len();
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_paths_users_and_hosts() {
        let s = Scrubber::new(Some("C:\\Users\\Alice"), Some("alice"), Some("ALICE-PC"));
        assert_eq!(
            s.text(
                r#"C:\Users\Alice\x C:/users/alice/y "C:\\Users\\Alice\\z" by alice on alice-pc"#
            ),
            r#"<home>\x <home>/y "<home>\\z" by <user> on <host>"#
        );
        let u = Scrubber::new(Some("/home/bob"), Some("bob"), None);
        assert_eq!(
            u.text("/home/bob/.cache, bobsled"),
            "<home>/.cache, <user>sled"
        );
    }

    #[test]
    fn tiny_names_are_left_alone() {
        let s = Scrubber::new(Some("/"), Some("al"), None);
        assert_eq!(s.text("/usr/al/ball"), "/usr/al/ball");
    }

    #[test]
    fn binary_scrub_keeps_length_and_covers_utf16() {
        let s = Scrubber::new(None, Some("Alice"), None);
        let mut d = b"..alice..".to_vec();
        d.extend("ALICE".encode_utf16().flat_map(u16::to_le_bytes));
        let n = d.len();
        s.bytes(&mut d);
        assert_eq!(d.len(), n);
        assert_eq!(&d[..9], b"..xxxxx..");
        assert_eq!(&d[9..], b"x\0x\0x\0x\0x\0");
    }
}
