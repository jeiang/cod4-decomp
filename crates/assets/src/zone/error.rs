// SPDX-License-Identifier: GPL-3.0-or-later
use super::asset_type::XAssetType;
use super::stream::{Addr, Block};
use std::fmt;

#[derive(Debug)]
pub enum ZoneError {
    Io(std::io::Error),
    BadMagic([u8; 8]),
    BadVersion(u32),
    UnknownAssetType(u32),
    /// An asset-list entry whose header is not stored inline.
    AssetNotInline(XAssetType),
    /// The asset type has no decoder yet.
    Unsupported(XAssetType),
    /// A pointer value that is invalid for the field that holds it.
    BadPointer(u32),
    /// An offset pointer whose target was never decoded (or has another type).
    BadOffset(Addr),
    BlockOverflow {
        block: Block,
        used: u64,
        size: u32,
    },
    /// Pop on an empty block stack.
    BlockUnderflow,
    /// A decoded value broke an invariant of the format.
    Invalid(&'static str),
    /// The decoded stream length disagrees with the header.
    SizeMismatch {
        expected: u64,
        actual: u64,
    },
    TrailingData,
}

impl fmt::Display for ZoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "fastfile i/o: {e}"),
            Self::BadMagic(m) => write!(f, "not an unsigned IW fastfile (magic {m:?})"),
            Self::BadVersion(v) => write!(f, "unsupported fastfile version {v}"),
            Self::UnknownAssetType(t) => write!(f, "unknown asset type {t}"),
            Self::AssetNotInline(t) => write!(f, "{} header is not inline", t.name()),
            Self::Unsupported(t) => write!(f, "no decoder for asset type {}", t.name()),
            Self::BadPointer(p) => write!(f, "invalid pointer {p:#010x}"),
            Self::BadOffset(a) => write!(f, "unresolved offset pointer {a:?}"),
            Self::BlockOverflow { block, used, size } => {
                write!(f, "block {block:?} overflow: {used} > {size}")
            }
            Self::BlockUnderflow => write!(f, "block stack underflow"),
            Self::Invalid(m) => write!(f, "invalid zone data: {m}"),
            Self::SizeMismatch { expected, actual } => {
                write!(f, "stream is {actual} bytes, header says {expected}")
            }
            Self::TrailingData => write!(f, "data after the end of the zone body"),
        }
    }
}

impl std::error::Error for ZoneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ZoneError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, ZoneError>;
