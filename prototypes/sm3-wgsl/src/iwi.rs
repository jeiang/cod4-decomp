//! IWD (zip) + IWI v6 image loading (throwaway). Layout facts: 'IWi' tag, version 6, format u8, flags u8,
//! dims 3xu16, 4xu32 picmip file sizes (28-byte header); mip chain stored SMALLEST FIRST (verified by eye).
use std::{collections::HashMap, fs::File, io::Read, path::{Path, PathBuf}};

pub struct Iwd { archives: Vec<(PathBuf, zip::ZipArchive<File>)>, index: HashMap<String, (usize, String)> }

#[derive(Debug, Clone)]
pub struct Iwi { pub format: u8, pub flags: u8, pub w: u32, pub h: u32, pub d: u32, pub cube: bool, pub vol: bool, pub data: Vec<u8> }

impl Iwd {
    pub fn open(main_dir: &Path) -> Iwd {
        let mut archives = vec![]; let mut index = HashMap::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(main_dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "iwd")).collect();
        files.sort();
        for p in files {
            if let Ok(z) = zip::ZipArchive::new(File::open(&p).unwrap()) {
                let ai = archives.len();
                for n in z.file_names() { if let Some(img) = n.strip_prefix("images/") { index.insert(img.trim_end_matches(".iwi").to_lowercase(), (ai, n.to_string())); } }
                archives.push((p, z));
            }
        }
        Iwd { archives, index }
    }
    pub fn has(&self, name: &str) -> bool { self.index.contains_key(&name.to_lowercase()) }
    pub fn load(&mut self, name: &str) -> Option<Iwi> {
        let (ai, ename) = self.index.get(&name.to_lowercase())?.clone();
        let z = &mut self.archives[ai].1;
        let mut b = vec![];
        z.by_name(&ename).ok()?.read_to_end(&mut b).ok()?;
        if b.len() < 28 || &b[..3] != b"IWi" || b[3] != 6 { return None; }
        let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
        let flags = b[5];
        Some(Iwi { format: b[4], flags, w: u16le(6), h: u16le(8), d: u16le(10), cube: flags & 4 != 0, vol: flags & 8 != 0, data: b[28..].to_vec() })
    }
}

/// block size in bytes, block dim (1 for uncompressed) and bytes/pixel
pub fn fmt_info(format: u8) -> Option<(usize, u32)> { match format { 1 => Some((4, 1)), 2 => Some((3, 1)), 3 => Some((2, 1)), 4 | 5 => Some((1, 1)), 11 => Some((8, 4)), 12 | 13 => Some((16, 4)), _ => None } }

/// Mip slices of one 2D image, largest first: (width, height, bytes).
pub fn mips_2d(i: &Iwi) -> Option<Vec<(u32, u32, Vec<u8>)>> {
    let (bs, bd) = fmt_info(i.format)?;
    let nomip = i.flags & 2 != 0;
    let mut dims = vec![]; let (mut w, mut h) = (i.w.max(1), i.h.max(1));
    loop {
        let sz = if bd == 4 { (((w + 3) / 4) * ((h + 3) / 4)) as usize * bs } else { (w * h) as usize * bs };
        dims.push((w, h, sz));
        if nomip || (w == 1 && h == 1) { break; }
        w = (w / 2).max(1); h = (h / 2).max(1);
    }
    // stored smallest first
    let total: usize = dims.iter().map(|d| d.2).sum();
    if total > i.data.len() { return None; }
    let mut off = i.data.len() - total; // tolerate trailing/leading slack
    off = 0.max(off).min(i.data.len() - total);
    let mut out = vec![];
    for d in dims.iter().rev() { out.push((d.0, d.1, i.data[off..off + d.2].to_vec())); off += d.2; }
    out.reverse();
    Some(out)
}
