// SPDX-License-Identifier: GPL-3.0-only
// Translated from KisakCOD (GPL-3.0): gfx_d3d/r_screenshot.cpp (R_ScreenshotCommand, R_ScreenshotFilename,
// R_TakeScreenshotTga); Copyright (C) Activision / Infinity Ward and the KisakCOD contributors.
//! The `screenshot` (TGA) and `screenshotJpeg` (JPEG, quality 90) commands, and the F12 bind of the stock profile that
//! runs the second: the picture on the screen goes to `screenshots/shotNNNN.<ext>`.

use std::path::{Path, PathBuf};

/// The picture's file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Tga,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Tga => "tga",
        }
    }
}

/// One `screenshot` or `screenshotJpeg` command, taken at the next frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub format: Format,
    /// `screenshot <name>` writes `<name>.<ext>`; no name (or `silent`) numbers the file.
    pub name: Option<String>,
    /// `screenshot silent`: no message.
    pub silent: bool,
}

impl Request {
    /// The command's request from its words; `None` for the `levelshot` and `savegame` forms, which make the
    /// original's own pictures (a map's thumbnail, a save's) that this client has no use for.
    pub fn parse(format: Format, args: &[String]) -> Option<Request> {
        let first = args.first().map(String::as_str);
        if matches!(first, Some("levelshot" | "savegame")) {
            return None;
        }
        let silent = first == Some("silent");
        let name = match (args.len(), first) {
            (1, Some(n)) if !silent => Some(n.to_owned()),
            _ => None,
        };
        Some(Request {
            format,
            name,
            silent,
        })
    }
}

/// A picture that is held in memory: `rgb` is `width * height * 3` bytes, top row first.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// The file `req` writes inside `dir`: `shot0000` up to the first number that is free (`next` remembers where the
/// last one was, as the original's `lastNumber` does), or the name the player gave. `None` when the numbers are
/// used up or the name would leave the folder.
pub fn target(dir: &Path, req: &Request, next: &mut u32) -> Option<PathBuf> {
    let ext = req.format.extension();
    match &req.name {
        Some(n) => {
            let ok =
                !n.is_empty() && !n.contains(['/', '\\', ':']) && !n.split('.').all(str::is_empty);
            ok.then(|| dir.join(format!("{n}.{ext}")))
        }
        None => {
            while *next <= 9999 {
                let p = dir.join(format!("shot{:04}.{ext}", *next));
                if !p.exists() {
                    return (*next < 9999).then_some(p);
                }
                *next += 1;
            }
            None
        }
    }
}

/// An uncompressed 24-bit TGA with the origin at the top left, as the original writes it.
fn tga(p: &Picture) -> Vec<u8> {
    let mut out = vec![0u8; 18];
    out[2] = 2;
    out[12..14].copy_from_slice(&(p.width as u16).to_le_bytes());
    out[14..16].copy_from_slice(&(p.height as u16).to_le_bytes());
    out[16] = 24;
    out[17] = 32;
    out.reserve(p.rgb.len());
    for px in p.rgb.chunks_exact(3) {
        out.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    out
}

fn jpeg(p: &Picture) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let (w, h) = (
        u16::try_from(p.width).map_err(|e| e.to_string())?,
        u16::try_from(p.height).map_err(|e| e.to_string())?,
    );
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(&p.rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes the picture to `path`.
pub fn write(path: &Path, format: Format, p: &Picture) -> Result<(), String> {
    let bytes = match format {
        Format::Tga => tga(p),
        Format::Jpeg => jpeg(p)?,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    fn picture() -> Picture {
        // 2x1: a red pixel, then a blue one.
        Picture {
            width: 2,
            height: 1,
            rgb: vec![255, 0, 0, 0, 0, 255],
        }
    }

    #[test]
    fn files_are_numbered_from_the_first_free_number() {
        let dir = tempfile::tempdir().unwrap();
        let req = Request::parse(Format::Jpeg, &[]).unwrap();
        let mut next = 0;
        let a = target(dir.path(), &req, &mut next).unwrap();
        assert_eq!(a.file_name().unwrap(), "shot0000.jpg");
        write(&a, Format::Jpeg, &picture()).unwrap();
        let b = target(dir.path(), &req, &mut next).unwrap();
        assert_eq!(b.file_name().unwrap(), "shot0001.jpg");
        // A fresh counter finds the same free number by looking.
        let mut again = 0;
        assert_eq!(target(dir.path(), &req, &mut again).unwrap(), b);
    }

    #[test]
    fn a_given_name_is_used_and_cannot_leave_the_folder() {
        let dir = Path::new("screenshots");
        let mut n = 0;
        let req = Request::parse(Format::Tga, &words("mine")).unwrap();
        assert_eq!(target(dir, &req, &mut n).unwrap(), dir.join("mine.tga"));
        for bad in ["../x", "a/b", "..", "c:x"] {
            let req = Request::parse(Format::Tga, &words(bad)).unwrap();
            assert_eq!(target(dir, &req, &mut n), None, "{bad}");
        }
    }

    #[test]
    fn silent_numbers_the_file_and_the_special_forms_are_not_ours() {
        let r = Request::parse(Format::Tga, &words("silent")).unwrap();
        assert!(r.silent && r.name.is_none());
        assert_eq!(Request::parse(Format::Tga, &words("levelshot")), None);
        assert_eq!(Request::parse(Format::Tga, &words("savegame x")), None);
    }

    #[test]
    fn the_tga_is_bgr_top_down_and_the_jpeg_decodes_to_the_picture() {
        let t = tga(&picture());
        assert_eq!((t[2], t[12], t[14], t[16], t[17]), (2, 2, 1, 24, 32));
        assert_eq!(&t[18..], [0, 0, 255, 255, 0, 0]);
        let j = jpeg(&picture()).unwrap();
        assert_eq!(&j[..2], [0xff, 0xd8]);
    }
}
