// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (game_mp/g_main_mp.cpp: G_InitGame, G_LogPrintf, G_ShutdownGame; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! The game log (`g_log`, `games_mp.log`): the lines the engine and the gametype scripts (`logPrint`) write for
//! joins, kills, chat and the start and end of a game, each stamped with the level time as `mmm:ss`.

use std::fs::{File, OpenOptions};
use std::io::Write;

/// The open log file, or nothing when `g_log` is empty or could not be opened.
#[derive(Default)]
pub struct GameLog {
    file: Option<File>,
    /// `g_logSync`: each line is flushed to the disk before the game goes on.
    sync: bool,
}

impl GameLog {
    /// Opens `path` for appending (`g_logSync` asks for synchronous writes). An empty path logs nowhere.
    pub fn open(path: &str, sync: bool) -> std::io::Result<Self> {
        if path.is_empty() {
            return Ok(Self::default());
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Some(file),
            sync,
        })
    }

    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// `G_LogPrintf`: one stamped line. `text` carries its own newline.
    pub fn print(&mut self, level_time_ms: i32, text: &str) {
        let Some(f) = self.file.as_mut() else { return };
        let line = Self::stamped(level_time_ms, text);
        if f.write_all(line.as_bytes()).is_ok() && self.sync {
            let _ = f.sync_data();
        }
    }

    /// `"%3i:%i%i %s"`: minutes, then the tens and units of the seconds.
    pub fn stamped(level_time_ms: i32, text: &str) -> String {
        let secs = level_time_ms / 1000;
        format!(
            "{:3}:{}{} {text}",
            secs / 60,
            secs % 60 / 10,
            secs % 60 % 10
        )
    }

    pub fn close(&mut self) {
        self.file = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_starts_with_the_level_time_as_minutes_and_two_digit_seconds() {
        assert_eq!(GameLog::stamped(0, "x\n"), "  0:00 x\n");
        assert_eq!(GameLog::stamped(61_999, "x\n"), "  1:01 x\n");
        assert_eq!(
            GameLog::stamped(125 * 60 * 1000 + 9_000, "x\n"),
            "125:09 x\n"
        );
    }

    #[test]
    fn lines_append_to_the_file_and_an_empty_name_logs_nowhere() {
        let path = std::env::temp_dir().join(format!("cod4e-gamelog-{}.log", std::process::id()));
        let p = path.to_str().unwrap();
        let mut log = GameLog::open(p, true).unwrap();
        log.print(1_000, "J;1\n");
        log.print(2_000, "Q;1\n");
        drop(log);
        let mut again = GameLog::open(p, false).unwrap();
        again.print(3_000, "K;1\n");
        drop(again);
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(text, "  0:01 J;1\n  0:02 Q;1\n  0:03 K;1\n");
        assert!(!GameLog::open("", false).unwrap().is_open());
    }
}
