// SPDX-License-Identifier: GPL-3.0-only
//! Decisions about the match the client is in that do not need a window: what to do when the server announces a level,
//! and the map rotation a menu-started server runs.

/// What the client does when the server announces level `new` while it is in `current`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LevelChange {
    /// The map the client already has loaded (the first announcement after joining, or a restart): keep the world and
    /// renderer, forget the old level's state.
    Same,
    /// Another map: load its data and rebuild the renderer, collision, models and sound.
    Load,
}

pub fn level_change(current: &str, new: &str) -> LevelChange {
    if current.eq_ignore_ascii_case(new) {
        LevelChange::Same
    } else {
        LevelChange::Load
    }
}

/// `sv_mapRotation` for a server that starts on `current`: the maps after it in `maps` order, wrapping around to
/// `current` last, each played with `gametype`. A single map rotates to itself.
pub fn rotation(gametype: &str, maps: &[String], current: &str) -> String {
    let at = maps
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current))
        .map_or(0, |i| i + 1);
    let mut order: Vec<&str> = maps[at.min(maps.len())..]
        .iter()
        .chain(&maps[..at.min(maps.len())])
        .map(String::as_str)
        .filter(|m| !m.eq_ignore_ascii_case(current))
        .collect();
    order.push(current);
    order
        .iter()
        .map(|m| format!("gametype {gametype} map {m}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn same_map_keeps_the_world_other_maps_reload() {
        assert_eq!(level_change("mp_crash", "MP_Crash"), LevelChange::Same);
        assert_eq!(level_change("mp_crash", "mp_bog"), LevelChange::Load);
        assert_eq!(level_change("", "mp_bog"), LevelChange::Load);
    }

    #[test]
    fn rotation_continues_after_the_current_map_and_wraps() {
        let m = maps(&["mp_a", "mp_b", "mp_c"]);
        assert_eq!(
            rotation("war", &m, "mp_b"),
            "gametype war map mp_c gametype war map mp_a gametype war map mp_b"
        );
        assert_eq!(
            rotation("dom", &m, "mp_c"),
            "gametype dom map mp_a gametype dom map mp_b gametype dom map mp_c"
        );
    }

    #[test]
    fn rotation_of_one_map_or_an_unlisted_map_still_plays_the_current_map() {
        assert_eq!(
            rotation("war", &maps(&["mp_a"]), "mp_a"),
            "gametype war map mp_a"
        );
        assert_eq!(rotation("war", &[], "mp_x"), "gametype war map mp_x");
        let r = rotation("war", &maps(&["mp_a", "mp_b"]), "mp_x");
        assert_eq!(
            r,
            "gametype war map mp_a gametype war map mp_b gametype war map mp_x"
        );
    }
}
