// SPDX-License-Identifier: GPL-3.0-only
//! The server's debug lines and 3D strings from the developer-only `line` and `print3d` builtins (`cls.debug`'s
//! `svLines`/`svStrings`, KisakCOD `client/cl_debugdata.cpp:17-140,236-340`).
//!
//! As in the original these never reach the network: they live in the process and only a listen server's own client
//! draws them. Each server frame first ages every entry by one frame and drops the expired ones (`FlushDebugLines`,
//! `FlushDebugStrings`), then the frame's scripts add new ones, then [`DebugDraw::publish`] copies the result for the
//! renderer (`CL_UpdateDebugServerData`). A duration of 0 or 1 therefore shows for one frame.
//!
//! Translated from KisakCOD `client/cl_debugdata.cpp` (`AddDebugStringInternal`, `AddDebugLineInternal`,
//! `FlushDebugStrings`, `FlushDebugLines`).

/// `svLines.max`.
pub const MAX_LINES: usize = 2048;
/// `svStrings.max`.
pub const MAX_STRINGS: usize = 1024;
/// `trDebugString_t::text` keeps 95 characters and the terminator.
pub const MAX_TEXT: usize = 95;

/// `trDebugLine_t`.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugLine {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub color: [f32; 4],
    /// Drawn in front of the world when false; the client draws every line in front (README).
    pub depth_test: bool,
}

/// `trDebugString_t`.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugString {
    pub origin: [f32; 3],
    pub color: [f32; 4],
    pub scale: f32,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct DebugDraw {
    lines: Vec<(DebugLine, i32)>,
    strings: Vec<(DebugString, i32)>,
    /// What the last finished frame left to draw.
    pub shown_lines: Vec<DebugLine>,
    pub shown_strings: Vec<DebugString>,
}

impl DebugDraw {
    pub const fn new() -> Self {
        Self {
            lines: Vec::new(),
            strings: Vec::new(),
            shown_lines: Vec::new(),
            shown_strings: Vec::new(),
        }
    }

    /// `AddDebugLineInternal`: dropped when the buffer is full.
    pub fn add_line(&mut self, line: DebugLine, duration: i32) {
        if self.lines.len() < MAX_LINES {
            self.lines.push((line, duration));
        }
    }

    /// `AddDebugStringInternal`: dropped when the buffer is full; the text is cut to [`MAX_TEXT`] bytes.
    pub fn add_string(&mut self, mut s: DebugString, duration: i32) {
        if self.strings.len() < MAX_STRINGS {
            if s.text.len() > MAX_TEXT {
                let mut end = MAX_TEXT;
                while !s.text.is_char_boundary(end) {
                    end -= 1;
                }
                s.text.truncate(end);
            }
            self.strings.push((s, duration));
        }
    }

    /// `FlushDebugLines`/`FlushDebugStrings` at the start of a server frame: one frame older, expired ones gone.
    pub fn flush(&mut self) {
        for (_, d) in self.lines.iter_mut() {
            *d -= 1;
        }
        for (_, d) in self.strings.iter_mut() {
            *d -= 1;
        }
        self.lines.retain(|(_, d)| *d > 0);
        self.strings.retain(|(_, d)| *d > 0);
    }

    /// `CL_UpdateDebugServerData` after a server frame: the buffers as the renderer sees them.
    pub fn publish(&mut self) {
        self.shown_lines.clear();
        self.shown_lines
            .extend(self.lines.iter().map(|(l, _)| l.clone()));
        self.shown_strings.clear();
        self.shown_strings
            .extend(self.strings.iter().map(|(s, _)| s.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> DebugLine {
        DebugLine {
            start: [0.0; 3],
            end: [1.0; 3],
            color: [1.0; 4],
            depth_test: false,
        }
    }

    #[test]
    fn an_entry_shows_for_its_duration_and_at_least_one_frame() {
        let mut d = DebugDraw::default();
        d.flush();
        d.add_line(line(), 0);
        d.add_line(line(), 3);
        d.publish();
        assert_eq!(d.shown_lines.len(), 2);
        for left in [1, 1, 0] {
            d.flush();
            d.publish();
            assert_eq!(d.shown_lines.len(), left);
        }
    }

    #[test]
    fn full_buffers_drop_new_entries_and_long_text_is_cut() {
        let mut d = DebugDraw::default();
        for _ in 0..MAX_LINES + 5 {
            d.add_line(line(), 1);
        }
        let s = DebugString {
            origin: [0.0; 3],
            color: [1.0; 4],
            scale: 1.0,
            text: "x".repeat(200),
        };
        d.add_string(s, 1);
        d.publish();
        assert_eq!(d.shown_lines.len(), MAX_LINES);
        assert_eq!(d.shown_strings[0].text.len(), MAX_TEXT);
    }
}
