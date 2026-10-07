// SPDX-License-Identifier: GPL-3.0-or-later
//! The entity channels of `soundaliases/channels.def`, a rawfile of `code_post_gfx_mp`: one row per channel
//! (`name, priority, 2d|3d, restricted|unrestricted, pause|nopause, max voices`), in the order the aliases'
//! channel index refers to. Empty or missing columns take the defaults.

/// Voices the original mixes at once (`SND_MAX_CHANNELS`): 8 2D, 32 3D and 13 streams, plus the reserved ones.
pub const MAX_CHANNELS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelDef {
    pub name: String,
    /// Larger replaces smaller when the voices run out.
    pub priority: u8,
    /// Sounds on this channel are positioned in the world.
    pub is_3d: bool,
    /// At most one sound per entity on this channel: a new one replaces the old.
    pub restricted: bool,
    pub pausable: bool,
    pub max_voices: u8,
}

pub fn parse(text: &str) -> Vec<ChannelDef> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split(',').map(str::trim);
        let Some(name) = cols.next().filter(|n| !n.is_empty() && n.len() <= 64) else {
            continue;
        };
        let priority = cols.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let is_3d = cols.next().is_some_and(|v| v.eq_ignore_ascii_case("3d"));
        let restricted = !cols.next().is_some_and(|v| v.eq_ignore_ascii_case("unrestricted"));
        let pausable = !cols.next().is_some_and(|v| v.eq_ignore_ascii_case("nopause"));
        let max_voices = cols
            .next()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .map_or(MAX_CHANNELS, |v| v.min(MAX_CHANNELS)) as u8;
        out.push(ChannelDef {
            name: name.to_owned(),
            priority,
            is_3d,
            restricted,
            pausable,
            max_voices,
        });
        if out.len() == MAX_CHANNELS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_default_missing_columns() {
        let t = "# comment\n\nphysics,0,3d,unrestricted,pause,6\nauto,1,3d,unrestricted\nbody,3,3d\nmenu,2,2d,unrestricted,nopause\nbulletimpact,1,3d,unrestricted,,10\nplain\n";
        let c = parse(t);
        assert_eq!(c.len(), 6);
        assert_eq!((c[0].priority, c[0].max_voices, c[0].restricted), (0, 6, false));
        assert!(c[1].is_3d && c[1].pausable && c[1].max_voices == 64);
        assert!(c[2].restricted, "restricted is the default");
        assert!(!c[3].is_3d && !c[3].pausable);
        assert_eq!((c[4].pausable, c[4].max_voices), (true, 10));
        assert_eq!((c[5].priority, c[5].is_3d), (0, false));
    }
}
