// SPDX-License-Identifier: GPL-3.0-only
//! The XFile stream reader: inflated byte source, the nine-block allocator
//! with its push/pop stack, pointer kinds, and the offset-pointer registry.

use super::error::{Result, ZoneError};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

/// `XFileBlock`, in the order of the nine sizes in the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Block {
    Temp = 0,
    Runtime,
    LargeRuntime,
    PhysicalRuntime,
    Virtual,
    Large,
    Physical,
    Vertex,
    Index,
}

pub const BLOCK_COUNT: usize = 9;

impl Block {
    const ALL: [Block; BLOCK_COUNT] = [
        Block::Temp,
        Block::Runtime,
        Block::LargeRuntime,
        Block::PhysicalRuntime,
        Block::Virtual,
        Block::Large,
        Block::Physical,
        Block::Vertex,
        Block::Index,
    ];

    pub fn all() -> [Block; BLOCK_COUNT] {
        Self::ALL
    }

    /// The three `*Runtime` blocks only reserve zeroed space; no bytes come from the stream.
    fn in_stream(self) -> bool {
        !matches!(
            self,
            Block::Runtime | Block::LargeRuntime | Block::PhysicalRuntime
        )
    }
}

/// A position inside a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Addr {
    pub block: Block,
    pub offset: u32,
}

/// A decoded 32-bit zone pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ptr {
    Null,
    /// `-1`: the pointee's data follows inline in the stream.
    Follow,
    /// `-2`: as `Follow`, and a 4-byte alias slot is reserved in VIRTUAL.
    Insert,
    /// `((block << 28) | offset) + 1`: points at already-loaded data.
    Offset(Addr),
}

impl Ptr {
    pub fn from_raw(v: u32) -> Result<Ptr> {
        Ok(match v {
            0 => Ptr::Null,
            0xFFFF_FFFF => Ptr::Follow,
            0xFFFF_FFFE => Ptr::Insert,
            _ => {
                let v = v - 1;
                let block = *Block::ALL
                    .get((v >> 28) as usize)
                    .ok_or(ZoneError::BadPointer(v + 1))?;
                Ptr::Offset(Addr {
                    block,
                    offset: v & 0x0FFF_FFFF,
                })
            }
        })
    }
}

/// Little-endian field cursor over a fixed-size struct image.
pub struct Fields<'a> {
    b: &'a [u8],
    p: usize,
    base: Option<Addr>,
}

impl<'a> Fields<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Fields {
            b,
            p: 0,
            base: None,
        }
    }

    /// A cursor over a struct image that sits at `at` in its block, so
    /// [`slot`](Self::slot) can name its pointer fields.
    pub fn at(b: &'a [u8], at: Addr) -> Self {
        Fields {
            b,
            p: 0,
            base: Some(at),
        }
    }

    /// Address of the next field. A `-1` pointer to a header-in-TEMP asset
    /// can later be referenced by an offset to this address; pass it as the
    /// `slot` of [`Stream::temp_asset_at`].
    pub fn slot(&self) -> Option<Addr> {
        self.base.map(|a| Addr {
            block: a.block,
            offset: a.offset + self.p as u32,
        })
    }
    pub fn skip(&mut self, n: usize) {
        self.p += n;
    }
    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let r = self.b[self.p..self.p + N].try_into().unwrap();
        self.p += N;
        r
    }
    pub fn u8(&mut self) -> u8 {
        self.bytes::<1>()[0]
    }
    pub fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.bytes())
    }
    pub fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.bytes())
    }
    pub fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.bytes())
    }
    pub fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes())
    }
    pub fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.bytes())
    }
    pub fn ptr(&mut self) -> Result<Ptr> {
        Ptr::from_raw(self.u32())
    }
}

/// Block accounting after a decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockUsage {
    pub declared: [u32; BLOCK_COUNT],
    /// Final allocation offset per block.
    pub used: [u32; BLOCK_COUNT],
    /// Highest allocation offset ever reached per block (TEMP is reset on pop).
    pub peak: [u32; BLOCK_COUNT],
}

const BUF_LEN: usize = 64 * 1024;

pub struct Stream<'a> {
    src: Box<dyn Read + 'a>,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    consumed: u64,
    declared: [u32; BLOCK_COUNT],
    used: [u32; BLOCK_COUNT],
    peak: [u32; BLOCK_COUNT],
    /// (block, offset at push time)
    stack: Vec<(Block, u32)>,
    registry: HashMap<(Addr, TypeId), Box<dyn Any + Send + Sync>>,
    keep_presentation: bool,
}

impl<'a> Stream<'a> {
    pub(super) fn new(src: Box<dyn Read + 'a>) -> Self {
        Stream {
            src,
            buf: vec![0; BUF_LEN],
            pos: 0,
            end: 0,
            consumed: 0,
            declared: [0; BLOCK_COUNT],
            used: [0; BLOCK_COUNT],
            peak: [0; BLOCK_COUNT],
            stack: Vec::new(),
            registry: HashMap::new(),
            keep_presentation: true,
        }
    }

    pub(super) fn set_block_sizes(&mut self, sizes: [u32; BLOCK_COUNT]) {
        self.declared = sizes;
    }

    pub(super) fn set_keep_presentation(&mut self, keep: bool) {
        self.keep_presentation = keep;
    }

    /// Whether the current asset's presentation payload is retained.
    pub fn keep_presentation(&self) -> bool {
        self.keep_presentation
    }

    /// Bytes taken from the inflated stream so far.
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    pub fn usage(&self) -> BlockUsage {
        BlockUsage {
            declared: self.declared,
            used: self.used,
            peak: self.peak,
        }
    }

    fn fill(&mut self) -> Result<bool> {
        self.pos = 0;
        self.end = self.src.read(&mut self.buf)?;
        Ok(self.end != 0)
    }

    /// Read exactly `out.len()` stream bytes (no block accounting).
    pub(super) fn read_raw(&mut self, out: &mut [u8]) -> Result<()> {
        let mut o = 0;
        while o < out.len() {
            if self.pos == self.end && !self.fill()? {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            let n = (out.len() - o).min(self.end - self.pos);
            out[o..o + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            o += n;
        }
        self.consumed += out.len() as u64;
        Ok(())
    }

    /// Whether the inflated stream has ended (this also checks the zlib trailer).
    pub(super) fn at_end(&mut self) -> Result<bool> {
        Ok(self.pos == self.end && !self.fill()?)
    }

    /// Inflate and discard the rest of the stream; returns the bytes skipped.
    pub fn drain(&mut self) -> Result<u64> {
        let mut n = 0u64;
        loop {
            n += (self.end - self.pos) as u64;
            if !self.fill()? {
                self.consumed += n;
                return Ok(n);
            }
        }
    }

    // ---- blocks ----

    pub fn push(&mut self, b: Block) {
        self.stack.push((b, self.used[b as usize]));
    }

    /// Pop the top block. TEMP rewinds to its push-time offset.
    pub fn pop(&mut self) -> Result<()> {
        let (b, saved) = self.stack.pop().ok_or(ZoneError::BlockUnderflow)?;
        if b == Block::Temp {
            self.used[b as usize] = saved;
        }
        Ok(())
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    fn top(&self) -> Result<Block> {
        self.stack
            .last()
            .map(|e| e.0)
            .ok_or(ZoneError::BlockUnderflow)
    }

    /// Reserve `len` bytes at `align` in the top block without reading.
    pub fn alloc(&mut self, align: u32, len: u32) -> Result<Addr> {
        let block = self.top()?;
        self.alloc_in(block, align, len)
    }

    fn alloc_in(&mut self, block: Block, align: u32, len: u32) -> Result<Addr> {
        let i = block as usize;
        let off = self.used[i].next_multiple_of(align.max(1));
        let end = off as u64 + len as u64;
        if end > self.declared[i] as u64 {
            return Err(ZoneError::BlockOverflow {
                block,
                used: end,
                size: self.declared[i],
            });
        }
        self.used[i] = end as u32;
        self.peak[i] = self.peak[i].max(self.used[i]);
        Ok(Addr { block, offset: off })
    }

    /// Allocate in the top block and read the bytes (zeros in RUNTIME blocks).
    pub fn load(&mut self, align: u32, len: u32) -> Result<(Addr, Vec<u8>)> {
        let a = self.alloc(align, len)?;
        let mut v = vec![0; len as usize];
        if a.block.in_stream() {
            self.read_raw(&mut v)?;
        }
        Ok((a, v))
    }

    /// Like [`load`](Self::load) for presentation-only payload (render vertex/index/texel data, sound samples): with the consumer
    /// not keeping render data, the bytes are consumed and block usage is
    /// accounted without buffering them, and the returned vector is empty.
    pub fn load_presentation(&mut self, align: u32, len: u32) -> Result<(Addr, Vec<u8>)> {
        if self.keep_presentation {
            return self.load(align, len);
        }
        let a = self.alloc(align, len)?;
        if a.block.in_stream() {
            self.skip_raw(len as u64)?;
        }
        Ok((a, Vec::new()))
    }

    fn skip_raw(&mut self, mut n: u64) -> Result<()> {
        let total = n;
        while n > 0 {
            if self.pos == self.end && !self.fill()? {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            let k = n.min((self.end - self.pos) as u64);
            self.pos += k as usize;
            n -= k;
        }
        self.consumed += total;
        Ok(())
    }

    /// Reserve the 4-byte alias slot used by `-2` pointers.
    pub fn insert_slot(&mut self) -> Result<Addr> {
        self.alloc_in(Block::Virtual, 4, 4)
    }

    // ---- registry ----

    pub fn register<V: Any + Send + Sync>(&mut self, at: Addr, v: V) {
        self.registry.insert((at, TypeId::of::<V>()), Box::new(v));
    }

    pub fn lookup<V: Any + Clone>(&self, at: Addr) -> Result<V> {
        self.registry
            .get(&(at, TypeId::of::<V>()))
            .and_then(|b| b.downcast_ref::<V>())
            .cloned()
            .ok_or(ZoneError::BadOffset(at))
    }

    // ---- pointer-following helpers ----

    /// NUL-terminated string at alignment 1 in the top block.
    pub fn string(&mut self, p: Ptr) -> Result<Option<Arc<str>>> {
        match p {
            Ptr::Null => Ok(None),
            Ptr::Offset(a) => self.lookup::<Arc<str>>(a).map(Some),
            Ptr::Insert => Err(ZoneError::BadPointer(0xFFFF_FFFE)),
            Ptr::Follow => {
                let mut bytes = Vec::new();
                loop {
                    let mut b = [0u8];
                    self.read_raw(&mut b)?;
                    if b[0] == 0 {
                        break;
                    }
                    bytes.push(b[0]);
                }
                let at = self.alloc(1, bytes.len() as u32 + 1)?;
                let s: Arc<str> = String::from_utf8_lossy(&bytes).into();
                self.register(at, s.clone());
                Ok(Some(s))
            }
        }
    }

    /// One struct behind a pointer, loaded into the top block. Offsets resolve
    /// through the registry.
    pub fn shared<T: Any + Send + Sync>(
        &mut self,
        p: Ptr,
        align: u32,
        size: u32,
        f: impl FnOnce(&mut Self, &[u8]) -> Result<T>,
    ) -> Result<Option<Arc<T>>> {
        match p {
            Ptr::Null => Ok(None),
            Ptr::Offset(a) => self.lookup::<Arc<T>>(a).map(Some),
            Ptr::Insert => Err(ZoneError::BadPointer(0xFFFF_FFFE)),
            Ptr::Follow => {
                let (at, bytes) = self.load(align, size)?;
                let v = Arc::new(f(self, &bytes)?);
                self.register(at, v.clone());
                Ok(Some(v))
            }
        }
    }

    /// An array behind a pointer. The whole array is read first, then each
    /// element's nested data is decoded in order by `f`.
    pub fn array<T: Any + Send + Sync>(
        &mut self,
        p: Ptr,
        count: u32,
        align: u32,
        elem_size: u32,
        mut f: impl FnMut(&mut Self, &mut Fields) -> Result<T>,
    ) -> Result<Arc<[T]>> {
        match p {
            Ptr::Null => Ok(Arc::from(Vec::new())),
            Ptr::Offset(a) if count > 0 => self.lookup::<Arc<[T]>>(a),
            Ptr::Offset(_) => Ok(Arc::from(Vec::new())),
            Ptr::Insert => Err(ZoneError::BadPointer(0xFFFF_FFFE)),
            Ptr::Follow => {
                if count == 0 {
                    // An empty array still advances the allocation to its alignment.
                    self.alloc(align, 0)?;
                    return Ok(Arc::from(Vec::new()));
                }
                let len = count
                    .checked_mul(elem_size)
                    .ok_or(ZoneError::Invalid("array too large"))?;
                let (at, bytes) = self.load(align, len)?;
                let mut v = Vec::with_capacity(count as usize);
                for (i, chunk) in bytes.chunks_exact(elem_size as usize).enumerate() {
                    let el = Addr {
                        block: at.block,
                        offset: at.offset + (i as u32) * elem_size,
                    };
                    v.push(f(self, &mut Fields::at(chunk, el))?);
                }
                let v: Arc<[T]> = v.into();
                self.register(at, v.clone());
                Ok(v)
            }
        }
    }

    /// A header-in-TEMP asset behind a pointer (material, techset, image, rawfile, ...).
    ///
    /// `Follow`/`Insert`: push TEMP, read the header there, reserve the alias
    /// slot for `Insert`, push VIRTUAL and let `f` decode the members, pop
    /// both. `Offset` resolves the alias slot.
    pub fn temp_asset<T: Any + Send + Sync>(
        &mut self,
        p: Ptr,
        align: u32,
        size: u32,
        f: impl FnOnce(&mut Self, &[u8]) -> Result<T>,
    ) -> Result<Option<Arc<T>>> {
        self.temp_ptr(p, align, size, true, f)
    }

    /// Like [`temp_asset`](Self::temp_asset), but `push_virtual` selects whether
    /// members load into VIRTUAL (assets) or stay in TEMP (e.g. image load defs).
    pub fn temp_ptr<T: Any + Send + Sync>(
        &mut self,
        p: Ptr,
        align: u32,
        size: u32,
        push_virtual: bool,
        f: impl FnOnce(&mut Self, &[u8]) -> Result<T>,
    ) -> Result<Option<Arc<T>>> {
        self.temp_ptr_at(None, p, align, size, push_virtual, f)
    }

    /// [`temp_asset`](Self::temp_asset) for a pointer field at `slot`
    /// ([`Fields::slot`]): a `-1` pointer also registers the asset at the
    /// field's own address, which later offset pointers may name.
    pub fn temp_asset_at<T: Any + Send + Sync>(
        &mut self,
        slot: Option<Addr>,
        p: Ptr,
        align: u32,
        size: u32,
        f: impl FnOnce(&mut Self, &[u8]) -> Result<T>,
    ) -> Result<Option<Arc<T>>> {
        self.temp_ptr_at(slot, p, align, size, true, f)
    }

    pub fn temp_ptr_at<T: Any + Send + Sync>(
        &mut self,
        slot: Option<Addr>,
        p: Ptr,
        align: u32,
        size: u32,
        push_virtual: bool,
        f: impl FnOnce(&mut Self, &[u8]) -> Result<T>,
    ) -> Result<Option<Arc<T>>> {
        match p {
            Ptr::Null => Ok(None),
            Ptr::Offset(a) => self.lookup::<Arc<T>>(a).map(Some),
            Ptr::Follow | Ptr::Insert => {
                self.push(Block::Temp);
                let (_, hdr) = self.load(align, size)?;
                let slot = if p == Ptr::Insert {
                    Some(self.insert_slot()?)
                } else {
                    slot
                };
                if push_virtual {
                    self.push(Block::Virtual);
                }
                let v = Arc::new(f(self, &hdr)?);
                if push_virtual {
                    self.pop()?;
                }
                if let Some(s) = slot {
                    self.register(s, v.clone());
                }
                self.pop()?;
                Ok(Some(v))
            }
        }
    }
}
