// SPDX-License-Identifier: GPL-3.0-or-later
use naga::valid::{Capabilities, ValidationFlags, Validator};

/// Parse WGSL with naga and validate it with no optional capabilities (the WebGL2-compatible floor).
pub fn validate(wgsl: &str) -> Result<(), String> {
    let m = naga::front::wgsl::parse_str(wgsl).map_err(|e| e.emit_to_string(wgsl))?;
    Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&m)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e.into_inner()))
}
