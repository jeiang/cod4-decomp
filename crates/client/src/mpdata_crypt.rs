// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (win32/win_storage.cpp: LiveStorage_GetCryptKey, LiveStorage_DecryptAndCheck,
// LiveStorage_Encrypt, xxtea_enc, xxtea_dec; qcommon/md4.cpp: Com_BlockChecksum128, Com_BlockChecksum128Cat; GPL-3.0,
// copyright the KisakCOD contributors and Activision).
//! The original's encrypted `mpdata` container (magic `iwm0`) and the CD key its key is made from.
//!
//! The file is 0x211C bytes: the magic, a 4 byte nonce (the tick count when it was written), then 0x2114 bytes
//! encrypted with XXTEA: a 16 byte hash, the 260 byte game directory and the 8192 byte stats area. The XXTEA key is
//! an HMAC-like mix of the nonce with an MD4 of the 34 byte `cl_cdkey` buffer (the 16 characters of the CD key, a NUL,
//! 15 spaces and two NULs: the buffer starts as 32 spaces and the key is copied over its first 16). The hash is the
//! MD4 of the plain game directory and stats, prefixed by `nonce ^ (key[2] - 1836222900)`. All words are little endian.
//!
//! The CD key is per install: the original reads it from the registry (`codkey`, 16 characters and a 4 character
//! checksum) of `HKLM\SOFTWARE\Activision\Call of Duty 4`. Here it comes from the same registry value on Windows and
//! from the Wine prefix's `system.reg` elsewhere. It is never logged or stored; [`CdKey`] prints as a placeholder.

use std::fmt;

/// Size of an `mpdata` file.
pub const FILE_LEN: usize = 0x211C;
/// Words of the encrypted part (hash, game directory and stats).
const BODY_WORDS: usize = 0x845;
/// Length of the hashed part (game directory and stats).
const HASHED_LEN: usize = 0x2104;

/// The CD key as the original's `cl_cdkey` buffer holds it.
#[derive(Clone, PartialEq, Eq)]
pub struct CdKey([u8; 34]);

impl fmt::Debug for CdKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CdKey(..)")
    }
}

impl CdKey {
    /// From the registry value: 16 key characters and 4 checksum characters (the checksum plays no part here).
    pub fn from_registry_value(value: &str) -> Option<CdKey> {
        let v = value.trim().as_bytes();
        if v.len() != 20 || !v.iter().all(u8::is_ascii_alphanumeric) {
            return None;
        }
        let mut buf = [b' '; 34];
        buf[..16].copy_from_slice(&v[..16]);
        buf[16] = 0;
        buf[32] = 0;
        buf[33] = 0;
        Some(CdKey(buf))
    }

    /// The install's CD key, `None` when it cannot be found. Windows asks the registry; elsewhere the Wine prefix
    /// (`WINEPREFIX`, else `~/.wine`) holds it.
    pub fn read() -> Option<CdKey> {
        #[cfg(windows)]
        {
            registry_windows()
        }
        #[cfg(not(windows))]
        {
            let prefix = std::env::var_os("WINEPREFIX")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".wine"))
                })?;
            [
                prefix.join("system.reg"),
                prefix.join("pfx").join("system.reg"),
            ]
            .iter()
            .find_map(|p| {
                std::fs::read_to_string(p)
                    .ok()
                    .and_then(|t| key_from_system_reg(&t))
            })
        }
    }
}

const KEY_PATHS: [&str; 2] = [
    "SOFTWARE\\Wow6432Node\\Activision\\Call of Duty 4",
    "SOFTWARE\\Activision\\Call of Duty 4",
];

#[cfg(windows)]
fn registry_windows() -> Option<CdKey> {
    use std::os::windows::process::CommandExt;
    KEY_PATHS.iter().find_map(|path| {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let out = std::process::Command::new(std::path::Path::new(&root).join("System32\\reg.exe"))
            .creation_flags(0x0800_0000)
            .args(["query", &format!("HKLM\\{path}"), "/v", "codkey"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .find(|l| l.trim_start().to_ascii_lowercase().starts_with("codkey"))?;
        CdKey::from_registry_value(line.split_whitespace().last()?)
    })
}

/// The `codkey` of a Wine `system.reg` (the section is keyed `Software\\Wow6432Node\\Activision\\Call of Duty 4`).
#[cfg(any(not(windows), test))]
fn key_from_system_reg(text: &str) -> Option<CdKey> {
    let mut inside = false;
    for line in text.lines() {
        if let Some(head) = line.strip_prefix('[') {
            let section = head.split(']').next().unwrap_or("").replace("\\\\", "\\");
            inside = KEY_PATHS.iter().any(|p| p.eq_ignore_ascii_case(&section));
        } else if inside
            && let Some((name, value)) = line.split_once('=')
            && name.eq_ignore_ascii_case("\"codkey\"")
        {
            return CdKey::from_registry_value(value.trim().trim_matches('"'));
        }
    }
    None
}

fn md4(data: &[u8]) -> [u8; 16] {
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    let mut st = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in msg.as_chunks::<64>().0.iter() {
        let mut x = [0u32; 16];
        for (w, c) in x.iter_mut().zip(block.as_chunks::<4>().0.iter()) {
            *w = u32::from_le_bytes(*c);
        }
        let [mut a, mut b, mut c, mut d] = st;
        for i in 0..16 {
            let f = (b & c) | (!b & d);
            let t = a
                .wrapping_add(f)
                .wrapping_add(x[i])
                .rotate_left([3, 7, 11, 19][i % 4]);
            (a, b, c, d) = (d, t, b, c);
        }
        const R2: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];
        for i in 0..16 {
            let g = (b & c) | (b & d) | (c & d);
            let t = a
                .wrapping_add(g)
                .wrapping_add(x[R2[i]])
                .wrapping_add(0x5a82_7999)
                .rotate_left([3, 5, 9, 13][i % 4]);
            (a, b, c, d) = (d, t, b, c);
        }
        const R3: [usize; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];
        for i in 0..16 {
            let h = b ^ c ^ d;
            let t = a
                .wrapping_add(h)
                .wrapping_add(x[R3[i]])
                .wrapping_add(0x6ed9_eba1)
                .rotate_left([3, 9, 11, 15][i % 4]);
            (a, b, c, d) = (d, t, b, c);
        }
        for (s, v) in st.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 16];
    for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(st) {
        o.copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// `Com_BlockChecksum128`: MD4 of the 4 byte key then the data.
fn block_checksum(data: &[u8], key: u32) -> [u8; 16] {
    let mut buf = key.to_le_bytes().to_vec();
    buf.extend_from_slice(data);
    md4(&buf)
}

/// `LiveStorage_GetCryptKey`: the XXTEA key (4 words) and the hash key's `key[2]` for one nonce.
fn crypt_key(cd: &CdKey, nonce: u32) -> [u32; 4] {
    let key_hash = block_checksum(&cd.0, 529_771_271);
    let mut ipad = [0u8; 20];
    let mut opad = [0u8; 32];
    for (i, w) in key_hash.as_chunks::<4>().0.iter().enumerate() {
        let w = u32::from_le_bytes(*w);
        ipad[i * 4..i * 4 + 4].copy_from_slice(&(w ^ 0x3636_3636).to_le_bytes());
        opad[i * 4..i * 4 + 4].copy_from_slice(&(w ^ 0x5c5c_5c5c).to_le_bytes());
    }
    ipad[16..].copy_from_slice(&nonce.to_le_bytes());
    let inner = md4(&ipad);
    // The outer hash covers the 16 byte pad and the inner hash.
    opad[16..].copy_from_slice(&inner);
    let out = md4(&opad);
    let mut k = [0u32; 4];
    for (w, c) in k.iter_mut().zip(out.as_chunks::<4>().0.iter()) {
        *w = u32::from_le_bytes(*c);
    }
    k
}

/// The hash key of the plain part: `nonce ^ (key[2] - 1836222900)`.
fn hash_key(nonce: u32, k: &[u32; 4]) -> u32 {
    nonce ^ k[2].wrapping_sub(1_836_222_900)
}

const DELTA: u32 = 0x9e37_79b9;

fn mx(z: u32, y: u32, sum: u32, k: u32) -> u32 {
    ((z ^ k).wrapping_add(y ^ sum)) ^ (((z << 4) ^ (y >> 3)).wrapping_add((y << 2) ^ (z >> 5)))
}

fn xxtea_dec(v: &mut [u32], k: &[u32; 4]) {
    let n = v.len();
    let mut sum = DELTA.wrapping_mul((0x34 / n as u32) + 6);
    let mut y = v[0];
    while sum != 0 {
        let e = ((sum >> 2) & 3) as usize;
        for p in (1..n).rev() {
            v[p] = v[p].wrapping_sub(mx(v[p - 1], y, sum, k[(e ^ p) & 3]));
            y = v[p];
        }
        v[0] = v[0].wrapping_sub(mx(v[n - 1], y, sum, k[e]));
        y = v[0];
        sum = sum.wrapping_sub(DELTA);
    }
}

#[cfg(test)]
fn xxtea_enc(v: &mut [u32], k: &[u32; 4]) {
    let n = v.len();
    let mut z = v[n - 1];
    let mut sum = 0u32;
    for _ in 0..(0x34 / n as u32) + 6 {
        sum = sum.wrapping_add(DELTA);
        let e = ((sum >> 2) & 3) as usize;
        for p in 0..n - 1 {
            v[p] = v[p].wrapping_add(mx(z, v[p + 1], sum, k[(e ^ p) & 3]));
            z = v[p];
        }
        v[n - 1] = v[n - 1].wrapping_add(mx(z, v[0], sum, k[(e ^ (n - 1)) & 3]));
        z = v[n - 1];
    }
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

/// Decrypts an `iwm0` file and checks its hash. The result is the file as the decrypted `ice0` container: the magic
/// `ice0`, the nonce, the hash, the game directory and the stats area. `None` when the size or magic is wrong or
/// the hash does not match: a tampered file, or one made with another CD key. The game directory the original also
/// compares with its `fs_game` is not compared: it is inside the hash, so a changed one is rejected anyway, and a
/// profile copied between mods is still read.
pub fn decrypt(file: &[u8], key: &CdKey) -> Option<Vec<u8>> {
    if file.len() != FILE_LEN || &file[..4] != b"iwm0" {
        return None;
    }
    let nonce = u32::from_le_bytes(file[4..8].try_into().unwrap());
    let k = crypt_key(key, nonce);
    let mut w = words(&file[8..]);
    debug_assert_eq!(w.len(), BODY_WORDS);
    xxtea_dec(&mut w, &k);
    let mut out = Vec::with_capacity(FILE_LEN);
    out.extend_from_slice(b"ice0");
    out.extend_from_slice(&file[4..8]);
    for v in w {
        out.extend_from_slice(&v.to_le_bytes());
    }
    (block_checksum(&out[24..24 + HASHED_LEN], hash_key(nonce, &k)) == out[8..24]).then_some(out)
}

/// The `iwm0` file for a decrypted container (`LiveStorage_Encrypt`). Only tests write the original's format: the
/// client never writes where the original reads (see the profile module).
#[cfg(test)]
pub fn encrypt(plain: &[u8], key: &CdKey, nonce: u32) -> Vec<u8> {
    assert_eq!(plain.len(), FILE_LEN);
    let k = crypt_key(key, nonce);
    let mut body = plain[8..].to_vec();
    let hash = block_checksum(&body[16..], hash_key(nonce, &k));
    body[..16].copy_from_slice(&hash);
    let mut w = words(&body);
    xxtea_enc(&mut w, &k);
    let mut out = b"iwm0".to_vec();
    out.extend_from_slice(&nonce.to_le_bytes());
    for v in w {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn test_key() -> CdKey {
        CdKey::from_registry_value("ABCD1234EFGH5678WXYZ").unwrap()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn md4_matches_the_rfc_vectors() {
        assert_eq!(hex(&md4(b"")), "31d6cfe0d16ae931b73c59d7e0c089c0");
        assert_eq!(hex(&md4(b"abc")), "a448017aaf21d8525fc10ae87aa6729d");
        assert_eq!(
            hex(&md4(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            )),
            "e33b4ddc9c38f2199c3e7b164fcc0536"
        );
    }

    #[test]
    fn xxtea_inverts() {
        let k = [1, 2, 3, 4];
        let orig: Vec<u32> = (0..BODY_WORDS as u32)
            .map(|i| i.wrapping_mul(2_654_435_761))
            .collect();
        let mut v = orig.clone();
        xxtea_enc(&mut v, &k);
        assert_ne!(v, orig);
        xxtea_dec(&mut v, &k);
        assert_eq!(v, orig);
    }

    #[test]
    fn cd_key_values_are_checked() {
        assert!(CdKey::from_registry_value("ABCD1234EFGH5678WXY").is_none());
        assert!(CdKey::from_registry_value("ABCD1234EFGH5678WX-Z").is_none());
        assert_eq!(format!("{:?}", test_key()), "CdKey(..)");
    }

    #[test]
    fn key_is_found_in_a_wine_registry_file() {
        let reg = "[Software\\\\Wow6432Node\\\\Activision\\\\Call of Duty 4] 1791476094\n#time=1dd5740275b5016\n\"codkey\"=\"ABCD1234EFGH5678WXYZ\"\n\n[Software\\\\Wow6432Node\\\\Other]\n\"codkey\"=\"00000000000000000000\"\n";
        assert_eq!(key_from_system_reg(reg), Some(test_key()));
        assert_eq!(
            key_from_system_reg("[Software\\\\Other]\n\"codkey\"=\"ABCD1234EFGH5678WXYZ\"\n"),
            None
        );
    }

    fn plain() -> Vec<u8> {
        let mut p = vec![0u8; FILE_LEN];
        p[..4].copy_from_slice(b"ice0");
        for (i, b) in p[0x11C..].iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        p
    }

    #[test]
    fn round_trip_with_a_test_key() {
        let p = plain();
        let enc = encrypt(&p, &test_key(), 0x1234_5678);
        assert_eq!(&enc[..4], b"iwm0");
        let dec = decrypt(&enc, &test_key()).expect("decrypts");
        // The container comes back as `ice0` with everything after the hash intact.
        assert_eq!(&dec[..4], b"ice0");
        assert_eq!(&dec[24..], &p[24..]);
    }

    #[test]
    fn tampered_files_and_other_keys_are_rejected() {
        let enc = encrypt(&plain(), &test_key(), 99);
        let other = CdKey::from_registry_value("ZZZZ1234EFGH5678WXYZ").unwrap();
        assert!(decrypt(&enc, &other).is_none());
        for at in [4, 8, 100, 0x200, FILE_LEN - 1] {
            let mut bad = enc.clone();
            bad[at] ^= 1;
            assert!(decrypt(&bad, &test_key()).is_none(), "byte {at}");
        }
        assert!(decrypt(&enc[..FILE_LEN - 4], &test_key()).is_none());
    }
}
