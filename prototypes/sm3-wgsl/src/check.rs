//! naga validation helpers shared by every path.
use naga::valid::{Capabilities, ValidationFlags, Validator};

#[derive(Debug, Clone)]
pub struct Fail { pub stage: &'static str, pub msg: String }
impl Fail { pub fn new(stage: &'static str, msg: impl Into<String>) -> Self { Fail { stage, msg: msg.into() } } }

pub fn validate(m: &naga::Module) -> Result<naga::valid::ModuleInfo, Fail> {
    Validator::new(ValidationFlags::all(), Capabilities::empty()).validate(m)
        .map_err(|e| {
            let mut m = format!("{}", e.as_inner());
            let mut src = std::error::Error::source(e.as_inner());
            while let Some(s) = src { m.push_str(&format!(" <- {s}")); src = s.source(); }
            Fail::new("naga-validate", m)
        })
}

/// WGSL text -> naga module -> validate (what wgpu does when it receives WGSL).
pub fn check_wgsl(src: &str) -> Result<(), Fail> {
    let m = naga::front::wgsl::parse_str(src).map_err(|e| Fail::new("wgsl-reparse", e.emit_to_string(src).lines().take(6).collect::<Vec<_>>().join(" | ")))?;
    validate(&m).map(|_| ())
}

/// SPIR-V words -> naga IR -> validate -> WGSL -> reparse+validate.
pub fn spv_to_wgsl(words: &[u32]) -> Result<String, Fail> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words { bytes.extend_from_slice(&w.to_le_bytes()); }
    let opts = naga::front::spv::Options { adjust_coordinate_space: false, strict_capabilities: false, block_ctx_dump_prefix: None };
    let m = naga::front::spv::parse_u8_slice(&bytes, &opts).map_err(|e| Fail::new("spv-parse", format!("{e}")))?;
    let info = validate(&m)?;
    let wgsl = naga::back::wgsl::write_string(&m, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| Fail::new("wgsl-out", format!("{e}")))?;
    check_wgsl(&wgsl)?;
    Ok(wgsl)
}

pub fn words_of(bytes: &[u8]) -> Vec<u32> { bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect() }

/// Normalise an error message into a bucket key (strip ids, numbers).
pub fn bucket(f: &Fail) -> String {
    let mut s = String::new(); let mut prev_digit = false;
    for c in f.msg.chars() { if c.is_ascii_digit() { if !prev_digit { s.push('#'); } prev_digit = true; } else { s.push(c); prev_digit = false; } }
    let s: String = s.chars().take(110).collect();
    format!("[{}] {}", f.stage, s)
}
