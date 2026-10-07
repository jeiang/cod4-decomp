// SPDX-License-Identifier: GPL-3.0-or-later
//! IW3 v5 fastfile (`IWffu100`) decoding: header, incremental zlib, the XFile
//! block model, pointer kinds, the script-string table and the asset list.
//!
//! [`Zone::open`] reads only the header and the asset list, inflating just the
//! stream prefix it needs. [`Zone::decode`] then walks the asset bodies in
//! order, handing each asset the consumer's [`DecodeFilter`] accepts to a sink.
//! Asset types without a decoder yet stop the walk with
//! [`ZoneError::Unsupported`]; later tickets add their types to [`Asset`].

mod asset_type;
pub mod clipmap;
mod error;
pub mod fx;
pub mod gfx;
pub mod gfxworld;
pub mod menu;
pub mod phys;
pub mod sound;
mod stream;
pub mod text;
pub mod weapon;
pub mod world;
pub mod xanim;
pub mod xmodel;

pub use asset_type::XAssetType;
pub use error::{Result, ZoneError};
pub use stream::{Addr, BLOCK_COUNT, Block, BlockUsage, Fields, Ptr, Stream};

use flate2::read::ZlibDecoder;
use gfx::{GfxImage, Material, RawFile, TechniqueSet};
use std::io::Read;
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"IWffu100";
const VERSION: u32 = 5;
/// Bytes of XFile header (size, external size, nine block sizes).
const XFILE_HEADER_LEN: u64 = 44;

/// The XFile header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Inflated length minus the 44-byte header.
    pub size: u32,
    /// Bytes of external data (IWI images) the zone refers to.
    pub external_size: u32,
    pub block_sizes: [u32; BLOCK_COUNT],
}

/// A decoded top-level asset.
#[derive(Debug, Clone)]
pub enum Asset {
    Material(Arc<Material>),
    TechniqueSet(Arc<TechniqueSet>),
    Image(Arc<GfxImage>),
    RawFile(Arc<RawFile>),
    PhysPreset(Arc<phys::PhysPreset>),
    XAnimParts(Arc<xanim::XAnimParts>),
    XModel(Arc<xmodel::XModel>),
    Sound(Arc<sound::SoundAliasList>),
    SoundCurve(Arc<sound::SndCurve>),
    LoadedSound(Arc<sound::LoadedSound>),
    SndDriverGlobals(Arc<sound::SndDriverGlobals>),
    /// Both `Clipmap` (SP) and `ClipmapPvs` (MP) asset types.
    Clipmap(Arc<clipmap::Clipmap>),
    ComWorld(Arc<world::ComWorld>),
    GameWorldMp(Arc<world::GameWorldMp>),
    GfxWorld(Arc<gfxworld::GfxWorld>),
    LightDef(Arc<world::LightDef>),
    Font(Arc<text::Font>),
    MenuList(Arc<menu::MenuList>),
    Localize(Arc<text::LocalizeEntry>),
    Weapon(Arc<weapon::WeaponDef>),
    Fx(Arc<fx::FxEffectDef>),
    ImpactFx(Arc<fx::FxImpactTable>),
    StringTable(Arc<text::StringTable>),
}

/// Per-consumer choice of which assets to retain. Every asset is still
/// consumed from the stream (the format is sequential); rejected ones are
/// dropped instead of delivered.
pub trait DecodeFilter {
    fn keep(&self, ty: XAssetType) -> bool;

    /// Whether presentation-only payload (render vertex/index/texel data,
    /// sound samples, UI) inside kept assets is retained. When `false` it is
    /// still read from the stream but dropped without allocating; see
    /// [`Stream::load_presentation`]. Payload of dropped asset types is always
    /// dropped this way.
    fn keep_presentation(&self) -> bool {
        true
    }
}

/// The two consumers of decoded zones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consumer {
    /// Simulation only: drops renderer-only assets.
    Server,
    Client,
}

impl DecodeFilter for Consumer {
    fn keep(&self, ty: XAssetType) -> bool {
        use XAssetType::*;
        match self {
            Consumer::Client => true,
            Consumer::Server => !matches!(
                ty,
                Image
                    | TechniqueSet
                    | GfxWorld
                    | LightDef
                    | Font
                    | Sound
                    | SoundCurve
                    | LoadedSound
                    | SndDriverGlobals
                    | MenuList
                    | Menu
                    | Fx
                    | ImpactFx
            ),
        }
    }

    fn keep_presentation(&self) -> bool {
        matches!(self, Consumer::Client)
    }
}

/// Keep everything.
pub struct KeepAll;

impl DecodeFilter for KeepAll {
    fn keep(&self, _: XAssetType) -> bool {
        true
    }
}

/// Result of a completed decode.
#[derive(Debug, Clone)]
pub struct ZoneStats {
    pub usage: BlockUsage,
    /// Inflated bytes consumed, equal to `44 + header.size`.
    pub consumed: u64,
    pub assets: usize,
}

pub struct Zone<'a> {
    header: Header,
    script_strings: Vec<Option<Arc<str>>>,
    assets: Vec<XAssetType>,
    /// VIRTUAL offset of the asset array; later references alias each entry's header field.
    assets_off: u32,
    stream: Stream<'a>,
}

impl<'a> Zone<'a> {
    /// Read the container header, the XFile header, the script-string table
    /// and the asset list.
    pub fn open(mut r: impl Read + 'a) -> Result<Self> {
        let mut pre = [0u8; 12];
        r.read_exact(&mut pre)?;
        let magic: [u8; 8] = pre[..8].try_into().unwrap();
        if &magic != MAGIC {
            return Err(ZoneError::BadMagic(magic));
        }
        let version = u32::from_le_bytes(pre[8..].try_into().unwrap());
        if version != VERSION {
            return Err(ZoneError::BadVersion(version));
        }
        let mut stream = Stream::new(Box::new(ZlibDecoder::new(r)));

        let mut h = [0u8; XFILE_HEADER_LEN as usize];
        stream.read_raw(&mut h)?;
        let mut f = Fields::new(&h);
        let size = f.u32();
        let external_size = f.u32();
        let block_sizes: [u32; BLOCK_COUNT] = std::array::from_fn(|_| f.u32());
        stream.set_block_sizes(block_sizes);

        let mut l = [0u8; 16];
        stream.read_raw(&mut l)?;
        let mut f = Fields::new(&l);
        let string_count = f.u32();
        let strings = f.ptr()?;
        let asset_count = f.u32();
        let assets_ptr = f.ptr()?;

        stream.push(Block::Virtual);
        let script_strings = match strings {
            Ptr::Follow => {
                let (_, b) = stream.load(4, string_count * 4)?;
                let ptrs = b
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| Ptr::from_raw(u32::from_le_bytes(*c)))
                    .collect::<Result<Vec<_>>>()?;
                ptrs.into_iter()
                    .map(|p| stream.string(p))
                    .collect::<Result<Vec<_>>>()?
            }
            Ptr::Null if string_count == 0 => Vec::new(),
            _ => return Err(ZoneError::Invalid("script string table pointer")),
        };
        let mut assets_off = 0;
        let assets = match assets_ptr {
            Ptr::Follow => {
                let (at, b) = stream.load(4, asset_count * 8)?;
                assets_off = at.offset;
                let mut out = Vec::with_capacity(asset_count as usize);
                for c in b.as_chunks::<8>().0 {
                    let mut f = Fields::new(c);
                    let t = f.u32();
                    let ty = XAssetType::from_u32(t).ok_or(ZoneError::UnknownAssetType(t))?;
                    // Its header is a stale runtime address with no stream data.
                    if ty != XAssetType::SndDriverGlobals && f.ptr()? != Ptr::Follow {
                        return Err(ZoneError::AssetNotInline(ty));
                    }
                    out.push(ty);
                }
                out
            }
            Ptr::Null if asset_count == 0 => Vec::new(),
            _ => return Err(ZoneError::Invalid("asset list pointer")),
        };
        // The asset bodies are read with VIRTUAL still pushed, by `decode`.
        Ok(Zone {
            header: Header {
                size,
                external_size,
                block_sizes,
            },
            script_strings,
            assets,
            assets_off,
            stream,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The script-string table (`None` for null entries).
    pub fn script_strings(&self) -> &[Option<Arc<str>>] {
        &self.script_strings
    }

    /// Top-level asset types in stream order.
    pub fn asset_types(&self) -> &[XAssetType] {
        &self.assets
    }

    /// Top-level asset counts indexed by `XAssetType as usize`.
    pub fn counts(&self) -> [usize; XAssetType::COUNT] {
        let mut c = [0; XAssetType::COUNT];
        for t in &self.assets {
            c[*t as usize] += 1;
        }
        c
    }

    /// Inflate and discard the remaining stream (verifies the zlib checksum).
    /// Returns the total inflated length including the header.
    pub fn inflate_rest(mut self) -> Result<u64> {
        self.stream.drain()?;
        Ok(self.stream.consumed())
    }

    /// Decode every asset body in order. Assets the filter keeps go to `sink`.
    /// Fails with [`ZoneError::Unsupported`] at the first type with no decoder.
    pub fn decode(
        mut self,
        filter: &dyn DecodeFilter,
        mut sink: impl FnMut(Asset),
    ) -> Result<ZoneStats> {
        let s = &mut self.stream;
        for (i, &ty) in self.assets.iter().enumerate() {
            eprintln!("DBG {i} {ty:?} consumed={}", s.consumed());
            // Later offset pointers alias the header field of this array entry.
            let field = Addr {
                block: Block::Virtual,
                offset: self.assets_off + 8 * i as u32 + 4,
            };
            // Types the consumer drops are decoded without buffering their bulk.
            s.set_keep_presentation(filter.keep(ty) && filter.keep_presentation());
            macro_rules! top {
                ($load:expr, $variant:ident) => {
                    $load?
                        .inspect(|a| s.register(field, a.clone()))
                        .map(Asset::$variant)
                };
            }
            let asset = match ty {
                XAssetType::Material => top!(gfx::material_ptr(s, Ptr::Follow), Material),
                XAssetType::TechniqueSet => top!(gfx::techset_ptr(s, Ptr::Follow), TechniqueSet),
                XAssetType::Image => top!(gfx::image_ptr(s, Ptr::Follow), Image),
                XAssetType::RawFile => {
                    top!(s.temp_asset(Ptr::Follow, 4, 12, gfx::raw_file), RawFile)
                }
                XAssetType::PhysPreset => top!(phys::load(s, Ptr::Follow), PhysPreset),
                XAssetType::XAnimParts => top!(xanim::load(s, Ptr::Follow), XAnimParts),
                XAssetType::XModel => top!(xmodel::load(s, Ptr::Follow), XModel),
                XAssetType::Sound => top!(sound::load_alias_list(s, Ptr::Follow), Sound),
                XAssetType::SoundCurve => top!(sound::load_curve(s, Ptr::Follow), SoundCurve),
                XAssetType::LoadedSound => {
                    top!(sound::load_loaded(s, Ptr::Follow), LoadedSound)
                }
                XAssetType::SndDriverGlobals => {
                    sound::load_driver_globals(s)?.map(Asset::SndDriverGlobals)
                }
                XAssetType::Clipmap | XAssetType::ClipmapPvs => {
                    top!(clipmap::load(s, Ptr::Follow), Clipmap)
                }
                XAssetType::ComWorld => top!(world::load_com_world(s, Ptr::Follow), ComWorld),
                XAssetType::GameWorldMp => {
                    top!(world::load_game_world_mp(s, Ptr::Follow), GameWorldMp)
                }
                XAssetType::GfxWorld => top!(gfxworld::load(s, Ptr::Follow), GfxWorld),
                XAssetType::LightDef => top!(world::load_light_def(s, Ptr::Follow), LightDef),
                XAssetType::Font => top!(text::load_font(s, Ptr::Follow), Font),
                XAssetType::MenuList => top!(menu::load(s, Ptr::Follow), MenuList),
                XAssetType::Localize => top!(text::load_localize(s, Ptr::Follow), Localize),
                XAssetType::Weapon => top!(weapon::load(s, Ptr::Follow), Weapon),
                XAssetType::Fx => top!(fx::load(s, Ptr::Follow), Fx),
                XAssetType::ImpactFx => top!(fx::load_impact(s, Ptr::Follow), ImpactFx),
                XAssetType::StringTable => {
                    top!(text::load_string_table(s, Ptr::Follow), StringTable)
                }
                t => return Err(ZoneError::Unsupported(t)),
            };
            if let (true, Some(a)) = (filter.keep(ty), asset) {
                sink(a);
            }
        }
        s.pop()?;
        let expected = XFILE_HEADER_LEN + u64::from(self.header.size);
        if s.consumed() != expected {
            return Err(ZoneError::SizeMismatch {
                expected,
                actual: s.consumed(),
            });
        }
        if !s.at_end()? {
            return Err(ZoneError::TrailingData);
        }
        Ok(ZoneStats {
            usage: s.usage(),
            consumed: s.consumed(),
            assets: self.assets.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn le(v: &mut Vec<u8>, w: &[u32]) {
        for x in w {
            v.extend(x.to_le_bytes());
        }
    }

    fn material(name_ptr: u32, tex_ptr: u32) -> Vec<u8> {
        let mut m = vec![0u8; 80];
        m[0..4].copy_from_slice(&name_ptr.to_le_bytes());
        m[58] = 1; // textureCount
        m[68..72].copy_from_slice(&tex_ptr.to_le_bytes());
        m
    }

    /// Two materials; the first inserts an image (-2) and the second reaches
    /// it through an offset pointer to the alias slot.
    fn synthetic() -> Vec<u8> {
        let slot = (4u32 << 28 | 32) + 1;
        let mut body = Vec::new();
        le(&mut body, &[0, 0, 2, 0xFFFF_FFFF]); // XAssetList
        le(&mut body, &[4, 0xFFFF_FFFF, 4, 0xFFFF_FFFF]); // 2 x XAsset(material)
        body.extend(material(0xFFFF_FFFF, 0xFFFF_FFFF));
        body.extend(b"a\0");
        le(&mut body, &[0x1111, 0x0302_0100]); // texture def: hash, name/sampler/semantic
        le(&mut body, &[0xFFFF_FFFE]); // image: Insert
        let mut img = vec![0u8; 36];
        img[0] = 3;
        img[24] = 8; // width
        img[32..36].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        body.extend(img);
        body.extend(b"i\0");
        body.extend(material(0xFFFF_FFFF, 0xFFFF_FFFF));
        body.extend(b"b\0");
        le(&mut body, &[0x2222, 0x0302_0100, slot]);
        let mut x = Vec::new();
        // temp 36+80+... is a maximum, not a total; declared generously.
        le(
            &mut x,
            &[body.len() as u32, 0, 200, 0, 0, 0, 52, 0, 0, 0, 0],
        );
        x.extend(body);
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&x).unwrap();
        let mut out = b"IWffu100".to_vec();
        out.extend(5u32.to_le_bytes());
        out.extend(z.finish().unwrap());
        out
    }

    #[test]
    fn insert_slot_aliases_across_assets_and_blocks_balance() {
        let ff = synthetic();
        let zone = Zone::open(&ff[..]).unwrap();
        assert_eq!(zone.asset_types(), [XAssetType::Material; 2]);
        let mut mats = Vec::new();
        let st = zone
            .decode(&KeepAll, |a| match a {
                Asset::Material(m) => mats.push(m),
                _ => unreachable!(),
            })
            .unwrap();
        assert_eq!(st.usage.used[Block::Virtual as usize], 52);
        assert_eq!(
            st.usage.used[Block::Temp as usize],
            0,
            "TEMP rewinds on pop"
        );
        let img = |m: &Material| match &m.textures[0].source {
            gfx::TextureSource::Image(i) => i.clone().unwrap(),
            _ => unreachable!(),
        };
        assert!(Arc::ptr_eq(&img(&mats[0]), &img(&mats[1])));
        assert_eq!(img(&mats[0]).name.as_deref(), Some("i"));
        assert_eq!(img(&mats[0]).width, 8);
    }

    #[test]
    fn server_filter_drops_renderer_assets_but_still_consumes_them() {
        let ff = synthetic();
        let mut kept = 0;
        Zone::open(&ff[..])
            .unwrap()
            .decode(&Consumer::Server, |_| kept += 1)
            .unwrap();
        assert_eq!(kept, 2);
        assert!(!Consumer::Server.keep(XAssetType::Image));
        assert!(Consumer::Client.keep(XAssetType::Image));
    }

    #[test]
    fn block_overflow_is_an_error() {
        let mut ff = synthetic();
        // Corrupt nothing in the zlib stream: rebuild with a too-small VIRTUAL block.
        let mut z = flate2::read::ZlibDecoder::new(&ff[12..]);
        let mut x = Vec::new();
        std::io::Read::read_to_end(&mut z, &mut x).unwrap();
        x[8 + 4 * 4..8 + 4 * 4 + 4].copy_from_slice(&51u32.to_le_bytes());
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(&x).unwrap();
        ff.truncate(12);
        ff.extend(e.finish().unwrap());
        let r = Zone::open(&ff[..]).and_then(|z| z.decode(&KeepAll, |_| {}));
        assert!(matches!(r, Err(ZoneError::BlockOverflow { .. })), "{r:?}");
    }
}
