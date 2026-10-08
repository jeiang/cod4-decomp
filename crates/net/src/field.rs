// SPDX-License-Identifier: GPL-3.0-only
//! Field-table delta coding: a state struct is a table of fields, each read and written through
//! two function pointers as a raw 32-bit word, plus how many bits that word takes on the wire.
//! A delta is the count of leading fields that may differ, then one changed bit per field and the
//! value of each changed field (Q3 `MSG_WriteDeltaEntity` style, with our own field set).
//!
//! Quantized kinds compare after quantization, so a change below the step costs nothing and the
//! reader reproduces exactly what the writer compared.

use crate::bits::{BitReader, BitWriter, Overflow};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// An unsigned integer of `n` bits (the word holds it zero-extended).
    Bits(u8),
    /// A signed integer of `n` bits (the word holds it as an `i32`).
    SBits(u8),
    /// An `f32` (the word holds its bits): 14 bits for a whole number in -4096..4096, else 33.
    Float,
    /// An `f32` rounded to a multiple of `step` and stored as a signed integer of `bits`.
    Fixed { bits: u8, step: f32 },
    /// An angle in degrees (an `f32`) stored in 16 bits, wrapped to 0..360.
    Angle16,
}

/// One field of `T`.
pub struct Field<T> {
    pub get: fn(&T) -> u32,
    pub set: fn(&mut T, u32),
    pub kind: Kind,
}

const ANGLE_STEP: f32 = 360.0 / 65536.0;

/// The canonical wire value of a word: what the reader will reconstruct from the written bits.
fn canon(kind: Kind, word: u32) -> u32 {
    match kind {
        Kind::Bits(n) => {
            if n >= 32 {
                word
            } else {
                word & ((1 << n) - 1)
            }
        }
        Kind::SBits(n) => sign_extend(word, n),
        Kind::Float => word,
        Kind::Fixed { bits, step } => {
            let raw = quantize_fixed(f32::from_bits(word), bits, step);
            (raw as f32 * step).to_bits()
        }
        Kind::Angle16 => {
            let raw = quantize_angle(f32::from_bits(word));
            (raw as f32 * ANGLE_STEP).to_bits()
        }
    }
}

fn sign_extend(word: u32, n: u8) -> u32 {
    if n >= 32 {
        return word;
    }
    let s = 32 - u32::from(n);
    (((word << s) as i32) >> s) as u32
}

fn quantize_fixed(f: f32, bits: u8, step: f32) -> i32 {
    let max = (1i64 << (bits - 1)) - 1;
    let q = (f / step).round();
    if q.is_nan() {
        0
    } else {
        (q as i64).clamp(-max - 1, max) as i32
    }
}

fn quantize_angle(f: f32) -> u16 {
    let r = (f / ANGLE_STEP).round();
    if r.is_nan() {
        0
    } else {
        (r as i64 & 0xffff) as u16
    }
}

fn write_value(w: &mut BitWriter, kind: Kind, word: u32) {
    match kind {
        Kind::Bits(n) => w.write_bits(word, u32::from(n)),
        Kind::SBits(n) => w.write_bits(word, u32::from(n)),
        Kind::Float => {
            let f = f32::from_bits(word);
            let i = f as i32;
            if (-4096..4096).contains(&i) && (i as f32).to_bits() == word {
                w.write_bool(true);
                w.write_bits((i + 4096) as u32, 13);
            } else {
                w.write_bool(false);
                w.write_u32(word);
            }
        }
        Kind::Fixed { bits, step } => {
            w.write_bits(
                quantize_fixed(f32::from_bits(word), bits, step) as u32,
                u32::from(bits),
            );
        }
        Kind::Angle16 => w.write_bits(u32::from(quantize_angle(f32::from_bits(word))), 16),
    }
}

fn read_value(r: &mut BitReader, kind: Kind) -> Result<u32, Overflow> {
    Ok(match kind {
        Kind::Bits(n) => r.read_bits(u32::from(n))?,
        Kind::SBits(n) => sign_extend(r.read_bits(u32::from(n))?, n),
        Kind::Float => {
            if r.read_bool()? {
                ((r.read_bits(13)? as i32 - 4096) as f32).to_bits()
            } else {
                r.read_u32()?
            }
        }
        Kind::Fixed { bits, step } => {
            (sign_extend(r.read_bits(u32::from(bits))?, bits) as i32 as f32 * step).to_bits()
        }
        Kind::Angle16 => (f32::from(r.read_u16()?) * ANGLE_STEP).to_bits(),
    })
}

/// Bits that hold a field count for a table of `n` fields.
fn count_bits(n: usize) -> u32 {
    usize::BITS - n.leading_zeros()
}

/// How many leading fields of `table` differ between `from` and `to` (0 when none do).
pub fn changed_count<T>(table: &[Field<T>], from: &T, to: &T) -> usize {
    table
        .iter()
        .rposition(|f| canon(f.kind, (f.get)(from)) != canon(f.kind, (f.get)(to)))
        .map_or(0, |i| i + 1)
}

/// Writes the delta from `from` to `to`: the field count, then each field's changed bit and
/// value. With a count of 0 only the count is written.
pub fn write_delta<T>(w: &mut BitWriter, table: &[Field<T>], from: &T, to: &T) {
    let count = changed_count(table, from, to);
    w.write_bits(count as u32, count_bits(table.len()));
    for f in &table[..count] {
        let (a, b) = (canon(f.kind, (f.get)(from)), (f.get)(to));
        if a == canon(f.kind, b) {
            w.write_bool(false);
        } else {
            w.write_bool(true);
            write_value(w, f.kind, b);
        }
    }
}

/// Reads a delta written by [`write_delta`] and applies it to `base`.
pub fn read_delta<T>(r: &mut BitReader, table: &[Field<T>], base: &mut T) -> Result<(), Overflow> {
    let count = r.read_bits(count_bits(table.len()))? as usize;
    if count > table.len() {
        return Err(Overflow);
    }
    for f in &table[..count] {
        if r.read_bool()? {
            let v = read_value(r, f.kind)?;
            (f.set)(base, v);
        }
    }
    Ok(())
}

/// Sets every field of `to` to its canonical (quantized) value, so a state compares equal to what
/// a reader reconstructs.
pub fn canonicalize<T>(table: &[Field<T>], s: &mut T) {
    for f in table {
        let v = canon(f.kind, (f.get)(s));
        (f.set)(s, v);
    }
}

/// Writes the entries of `to` that differ from `from` as (index gap, value) pairs ended by a gap
/// of 0 after the last: for arrays of words that change in a few places.
pub fn write_sparse(w: &mut BitWriter, from: &[i32], to: &[i32]) {
    debug_assert_eq!(from.len(), to.len());
    let mut last = 0usize;
    for (i, (a, b)) in from.iter().zip(to).enumerate() {
        if a != b {
            w.write_uvar((i + 1 - last) as u32);
            w.write_ivar(b.wrapping_sub(*a));
            last = i + 1;
        }
    }
    w.write_uvar(0);
}

pub fn read_sparse(r: &mut BitReader, base: &mut [i32]) -> Result<(), Overflow> {
    let mut at = 0usize;
    loop {
        let gap = r.read_uvar()? as usize;
        if gap == 0 {
            return Ok(());
        }
        at += gap;
        let d = r.read_ivar()?;
        let slot = base.get_mut(at - 1).ok_or(Overflow)?;
        *slot = slot.wrapping_add(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Default, PartialEq)]
    struct S {
        a: u32,
        b: i32,
        f: f32,
        p: f32,
        ang: f32,
    }

    macro_rules! field {
        ($f:ident, $get:expr, $set:expr, $k:expr) => {
            Field::<S> {
                get: |s| $get(s.$f),
                set: |s, v| s.$f = $set(v),
                kind: $k,
            }
        };
    }

    fn table() -> Vec<Field<S>> {
        vec![
            field!(a, |v| v, |v| v, Kind::Bits(10)),
            field!(b, |v: i32| v as u32, |v| v as i32, Kind::SBits(12)),
            field!(f, f32::to_bits, f32::from_bits, Kind::Float),
            field!(
                p,
                f32::to_bits,
                f32::from_bits,
                Kind::Fixed {
                    bits: 12,
                    step: 0.25
                }
            ),
            field!(ang, f32::to_bits, f32::from_bits, Kind::Angle16),
        ]
    }

    fn round_trip(from: &S, to: &S) -> (S, usize) {
        let t = table();
        let mut w = BitWriter::new();
        write_delta(&mut w, &t, from, to);
        let bits = w.bit_len();
        let mut got = from.clone();
        read_delta(&mut BitReader::new(w.as_bytes()), &t, &mut got).unwrap();
        (got, bits)
    }

    #[test]
    fn delta_reproduces_the_canonical_target() {
        let from = S::default();
        let mut to = S {
            a: 700,
            b: -2000,
            f: 123.456,
            p: 10.13,
            ang: 359.99,
        };
        let (got, _) = round_trip(&from, &to);
        canonicalize(&table(), &mut to);
        assert_eq!(got, to);
        assert_eq!(got.a, 700);
        assert_eq!(got.b, -2000);
        assert_eq!(got.f, 123.456);
        assert_eq!(got.p, 10.25);
        assert!((got.ang - 359.99).abs() < ANGLE_STEP);
    }

    #[test]
    fn unchanged_costs_only_the_count_and_small_whole_floats_are_cheap() {
        let s = S {
            a: 5,
            ..S::default()
        };
        let (got, bits) = round_trip(&s, &s);
        assert_eq!(got, s);
        assert_eq!(bits, count_bits(5) as usize);
        let mut t = s.clone();
        t.f = 100.0;
        let (got, bits) = round_trip(&s, &t);
        assert_eq!(got.f, 100.0);
        assert_eq!(
            bits,
            count_bits(5) as usize + 2 + 1 + 14,
            "two unchanged fields, then one cheap float"
        );
    }

    #[test]
    fn sub_step_changes_are_not_sent_and_do_not_drift() {
        let from = S {
            p: 10.0,
            ..S::default()
        };
        let to = S {
            p: 10.05,
            ..S::default()
        };
        assert_eq!(changed_count(&table(), &from, &to), 0);
        let to = S {
            p: 10.2,
            ..S::default()
        };
        assert_eq!(changed_count(&table(), &from, &to), 4);
    }

    #[test]
    fn out_of_range_values_clamp_and_nan_is_zero() {
        let (got, _) = round_trip(
            &S::default(),
            &S {
                a: 5000,
                p: f32::NAN,
                f: f32::INFINITY,
                ang: -90.0,
                ..S::default()
            },
        );
        assert_eq!(got.a, 5000 & 1023);
        assert_eq!(got.p, 0.0);
        assert_eq!(got.f, f32::INFINITY);
        assert!((got.ang - 270.0).abs() < ANGLE_STEP);
        let (got, _) = round_trip(
            &S::default(),
            &S {
                p: 1.0e9,
                ..S::default()
            },
        );
        assert_eq!(got.p, 2047.0 * 0.25);
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        let t = table();
        let mut w = BitWriter::new();
        write_delta(
            &mut w,
            &t,
            &S::default(),
            &S {
                f: 1e9,
                a: 3,
                ..S::default()
            },
        );
        let bytes = w.into_bytes();
        for n in 0..bytes.len() {
            let mut base = S::default();
            let _ = read_delta(&mut BitReader::new(&bytes[..n]), &t, &mut base);
        }
    }

    #[test]
    fn sparse_words_round_trip() {
        let from = vec![0, 5, 0, 0, 9, 1, 2, 3];
        let mut to = from.clone();
        to[1] = -7;
        to[7] = i32::MAX;
        to[4] = i32::MIN;
        let mut w = BitWriter::new();
        write_sparse(&mut w, &from, &to);
        let mut got = from.clone();
        read_sparse(&mut BitReader::new(w.as_bytes()), &mut got).unwrap();
        assert_eq!(got, to);
        let mut w = BitWriter::new();
        write_sparse(&mut w, &to, &to);
        assert_eq!(w.bit_len(), 5, "nothing changed: just the end marker");
    }
}
