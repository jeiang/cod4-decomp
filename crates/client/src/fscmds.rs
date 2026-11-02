// SPDX-License-Identifier: GPL-3.0-only
//! The file system's console commands (KisakCOD universal/com_files.cpp:185, 1765-1925 and
//! client_mp/cl_main_mp.cpp:3015): `dir`, `fdir`, `path`, `fullpath` and `fs_openedList`. Each returns the lines
//! stock prints.

use assets::vfs::{LANGUAGES, NodeKind, Vfs};

/// `FS_Dir_f`: `dir <directory> [extension]` lists the files directly in a directory of the search path.
pub fn dir(vfs: &Vfs, args: &[&str]) -> Vec<String> {
    let (path, ext) = match args {
        [path] => (*path, ""),
        [path, ext] => (*path, *ext),
        _ => return vec!["usage: dir <directory> [extension]".to_owned()],
    };
    let mut out = vec![
        format!("Directory of {path} {ext}"),
        "---------------".to_owned(),
    ];
    let prefix = format!("{}/", path.replace('\\', "/").trim_matches('/'));
    let prefix = prefix.trim_start_matches('/').to_ascii_lowercase();
    let ext = ext.to_ascii_lowercase();
    out.extend(
        vfs.list(&prefix, &ext)
            .into_iter()
            .filter_map(|n| n.strip_prefix(&prefix).map(str::to_owned))
            .filter(|n| !n.is_empty() && !n.contains('/')),
    );
    out
}

/// `FS_NewDir_f`: `fdir <filter>` lists every file of the search path the filter (`*`, `?`, any case) matches,
/// and the count.
pub fn fdir(vfs: &Vfs, args: &[&str]) -> Vec<String> {
    let Some(filter) = args.first() else {
        return vec![
            "usage: fdir <filter>".to_owned(),
            "example: fdir *q3dm*.bsp".to_owned(),
        ];
    };
    let filter = filter.replace('\\', "/").to_ascii_lowercase();
    let files: Vec<String> = vfs
        .list("", "")
        .into_iter()
        .filter(|n| glob(filter.as_bytes(), n.as_bytes()))
        .map(|n| n.replace('/', "\\"))
        .collect();
    let mut out = vec!["---------------".to_owned()];
    let n = files.len();
    out.extend(files);
    out.push(format!("{n} files listed"));
    out
}

/// `FS_DisplayPath`: `path` (`cull`, the nodes of the current language only) and `fullpath` print the
/// language and the search path, an IWD with its file count and a folder as it is. This build keeps no file
/// handle open between reads, so the handle list is empty.
pub fn path(vfs: &Vfs, cull: bool) -> Vec<String> {
    let lang = |i: usize| LANGUAGES.get(i).copied().unwrap_or("english");
    let mut out = vec![
        format!("Current language: {}", lang(vfs.language())),
        "Current search path:".to_owned(),
    ];
    for node in vfs.nodes() {
        if cull && node.language.is_some_and(|l| l != vfs.language()) {
            continue;
        }
        match &node.kind {
            NodeKind::Iwd { path, iwd } => {
                out.push(format!(
                    "{} ({} files)",
                    path.display(),
                    iwd.entries().len()
                ));
                if let Some(l) = node.language {
                    out.push(format!("    localized assets iwd file for {}", lang(l)));
                }
            }
            NodeKind::Dir(dir) => {
                out.push(dir.display().to_string());
                if let Some(l) = node.language {
                    out.push(format!("    localized assets game folder for {}", lang(l)));
                }
            }
        }
    }
    out.push(String::new());
    out.push("File Handles:".to_owned());
    out
}

/// `CL_OpenedIWDList_f`: the base names of the search path's IWDs that are not localized (`FS_LoadedIwdNames`).
pub fn opened_iwds(vfs: &Vfs) -> String {
    let names: Vec<String> = vfs
        .nodes()
        .iter()
        .filter(|n| n.language.is_none())
        .filter_map(|n| match &n.kind {
            NodeKind::Iwd { path, .. } => {
                path.file_stem().map(|s| s.to_string_lossy().into_owned())
            }
            NodeKind::Dir(_) => None,
        })
        .collect();
    format!("Opened IWD Names: {}", names.join(" "))
}

/// `Com_FilterPath`: `*` any run, `?` any one byte; both sides are lowercase with `/`.
fn glob(p: &[u8], n: &[u8]) -> bool {
    match p.split_first() {
        None => n.is_empty(),
        Some((b'*', rest)) => (0..=n.len()).any(|i| glob(rest, &n[i..])),
        Some((&c, rest)) => n
            .split_first()
            .is_some_and(|(&d, nrest)| (c == b'?' || c == d) && glob(rest, nrest)),
    }
}

#[cfg(test)]
mod tests {
    use super::glob;

    #[test]
    fn the_filter_matches_as_com_filter_path_does() {
        assert!(glob(b"*q3dm*.bsp", b"maps/q3dm17.bsp"));
        assert!(glob(b"maps/mp_?ackup.d3dbsp", b"maps/mp_backup.d3dbsp"));
        assert!(!glob(b"*.cfg", b"maps/mp_backup.d3dbsp"));
    }
}
