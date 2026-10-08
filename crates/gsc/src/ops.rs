// SPDX-License-Identifier: GPL-3.0-only
//! Operators on script values. Errors are the original's runtime error messages.

use std::rc::Rc;

use crate::bytecode::Op;
use crate::value::{Key, Str, Value};

/// Longest string a concatenation may produce (original: 0x2000 bytes with the terminator).
const MAX_STRING: usize = 0x2000 - 1;

pub fn cast_bool(v: &Value) -> Result<bool, String> {
    match v {
        Value::Int(i) => Ok(*i != 0),
        Value::Float(f) => Ok(*f != 0.0),
        other => Err(format!("cannot cast {} to bool", other.type_name())),
    }
}

/// `%g`, as the original formats floats converted to strings.
pub fn format_g(x: f32) -> String {
    let x = f64::from(x);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.into();
    }
    if !x.is_finite() {
        return if x.is_nan() {
            "nan"
        } else if x < 0.0 {
            "-inf"
        } else {
            "inf"
        }
        .into();
    }
    // Six significant digits, then decide between fixed and exponent notation on the
    // exponent after rounding.
    let sci = format!("{x:.5e}");
    let (mant, exp) = sci.split_once('e').expect("exponent");
    let exp: i32 = exp.parse().expect("exponent digits");
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        let s = format!("{x:.decimals$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    } else {
        let mant = if mant.contains('.') {
            mant.trim_end_matches('0').trim_end_matches('.')
        } else {
            mant
        };
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

pub fn format_vector(v: &[f32; 3]) -> String {
    format!(
        "({}, {}, {})",
        format_g(v[0]),
        format_g(v[1]),
        format_g(v[2])
    )
}

fn unmatching(a: &Value, b: &Value) -> String {
    format!(
        "pair '{}' and '{}' has unmatching types '{}' and '{}'",
        debug_string(a),
        debug_string(b),
        b.type_name(),
        a.type_name()
    )
}

/// How a value shows up in a type-mismatch message.
fn debug_string(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format_g(*f),
        Value::Str(s) | Value::LocStr(s) => s.to_string(),
        Value::Vector(v) => format_vector(v),
        other => other.type_name().to_string(),
    }
}

fn to_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Int(i) => Some(*i as f32),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// The original's "weaker pair" cast: int widens to float, and a number against a vector
/// becomes a vector of that number.
enum Pair {
    Int(i32, i32),
    Float(f32, f32),
    Vector([f32; 3], [f32; 3]),
}

fn weaker(a: &Value, b: &Value) -> Result<Pair, String> {
    Ok(match (a, b) {
        (Value::Int(x), Value::Int(y)) => Pair::Int(*x, *y),
        (Value::Float(x), Value::Float(y)) => Pair::Float(*x, *y),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            Pair::Float(to_f32(a).expect("number"), to_f32(b).expect("number"))
        }
        (Value::Vector(x), Value::Vector(y)) => Pair::Vector(*x, *y),
        (Value::Vector(x), n @ (Value::Int(_) | Value::Float(_))) => {
            let n = to_f32(n).expect("number");
            Pair::Vector(*x, [n; 3])
        }
        (n @ (Value::Int(_) | Value::Float(_)), Value::Vector(y)) => {
            let n = to_f32(n).expect("number");
            Pair::Vector([n; 3], *y)
        }
        _ => return Err(unmatching(a, b)),
    })
}

fn vec_map(a: [f32; 3], b: [f32; 3], f: impl Fn(f32, f32) -> f32) -> Value {
    Value::Vector([f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])])
}

fn int_pair(a: &Value, b: &Value) -> Result<(i32, i32), String> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok((*x, *y)),
        _ => Err(unmatching(a, b)),
    }
}

fn string_of(v: &Value) -> Option<Str> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string().into()),
        Value::Float(f) => Some(format_g(*f).into()),
        Value::Vector(v) => Some(format_vector(v).into()),
        _ => None,
    }
}

fn plus(a: Value, b: Value) -> Result<Value, String> {
    if matches!(a, Value::Str(_)) || matches!(b, Value::Str(_)) {
        // A string pairs with a number or vector by converting that to text.
        if let (Some(x), Some(y)) = (string_of(&a), string_of(&b)) {
            if x.len() + y.len() > MAX_STRING {
                return Err(format!(
                    "cannot concat \"{x}\" and \"{y}\" - max string length exceeded"
                ));
            }
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(&x);
            s.push_str(&y);
            return Ok(Value::Str(s.into()));
        }
        return Err(unmatching(&a, &b));
    }
    Ok(match weaker(&a, &b)? {
        Pair::Int(x, y) => Value::Int(x.wrapping_add(y)),
        Pair::Float(x, y) => Value::Float(x + y),
        Pair::Vector(x, y) => vec_map(x, y, |p, q| p + q),
    })
}

fn compare(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    // Only ints and floats order; `<=` and `>=` are the negations of `>` and `<`, so NaN
    // answers true for them. The caller encodes that.
    match weaker(a, b)? {
        Pair::Int(x, y) => Ok(x.cmp(&y)),
        Pair::Float(x, y) => Ok(x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)),
        Pair::Vector(..) => Err(unmatching(a, b)),
    }
}

/// `==` as scripts see it; `waittillmatch` uses the same comparison.
pub fn equal(a: &Value, b: &Value) -> bool {
    equality(a, b).unwrap_or(false)
}

fn equality(a: &Value, b: &Value) -> Result<bool, String> {
    match (a, b) {
        (Value::Undefined, Value::Undefined) => Ok(true),
        (Value::Str(x), Value::Str(y)) | (Value::LocStr(x), Value::LocStr(y)) => Ok(x == y),
        (Value::Func(x), Value::Func(y)) => Ok(x == y),
        (Value::Anim(x), Value::Anim(y)) => Ok(x == y),
        (Value::Object(x), Value::Object(y)) => Ok(x == y),
        (Value::Float(x), Value::Float(y)) => Ok((x - y).abs() < 0.000_001),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            Ok((to_f32(a).expect("number") - to_f32(b).expect("number")).abs() < 0.000_001)
        }
        (Value::Vector(x), Value::Vector(y)) => Ok(x == y),
        _ => Err(unmatching(a, b)),
    }
}

pub fn binary(op: Op, a: Value, b: Value) -> Result<Value, String> {
    use std::cmp::Ordering::{Greater, Less};
    let bool_v = |x: bool| Ok(Value::Int(i32::from(x)));
    match op {
        Op::Add => plus(a, b),
        Op::Sub => Ok(match weaker(&a, &b)? {
            Pair::Int(x, y) => Value::Int(x.wrapping_sub(y)),
            Pair::Float(x, y) => Value::Float(x - y),
            Pair::Vector(x, y) => vec_map(x, y, |p, q| p - q),
        }),
        Op::Mul => Ok(match weaker(&a, &b)? {
            Pair::Int(x, y) => Value::Int(x.wrapping_mul(y)),
            Pair::Float(x, y) => Value::Float(x * y),
            Pair::Vector(x, y) => vec_map(x, y, |p, q| p * q),
        }),
        Op::Div => match weaker(&a, &b)? {
            // Integer division yields a float.
            Pair::Int(x, y) if y != 0 => Ok(Value::Float((f64::from(x) / f64::from(y)) as f32)),
            Pair::Float(x, y) if y != 0.0 => Ok(Value::Float(x / y)),
            Pair::Vector(x, y) if y.iter().all(|c| *c != 0.0) => Ok(vec_map(x, y, |p, q| p / q)),
            Pair::Int(..) | Pair::Float(..) | Pair::Vector(..) => Err("divide by 0".into()),
        },
        Op::Mod => {
            let (x, y) = int_pair(&a, &b)?;
            if y == 0 {
                Err("divide by 0".into())
            } else {
                Ok(Value::Int(x.wrapping_rem(y)))
            }
        }
        Op::BitAnd => int_pair(&a, &b).map(|(x, y)| Value::Int(x & y)),
        Op::BitOr => int_pair(&a, &b).map(|(x, y)| Value::Int(x | y)),
        Op::BitXor => int_pair(&a, &b).map(|(x, y)| Value::Int(x ^ y)),
        // x86 `shl`/`sar` take the count modulo 32.
        Op::Shl => int_pair(&a, &b).map(|(x, y)| Value::Int(x.wrapping_shl(y as u32))),
        Op::Shr => int_pair(&a, &b).map(|(x, y)| Value::Int(x.wrapping_shr(y as u32))),
        Op::Eq => bool_v(equality(&a, &b)?),
        Op::Ne => bool_v(!equality(&a, &b)?),
        Op::Lt => bool_v(compare(&a, &b)? == Less),
        Op::Gt => bool_v(compare(&a, &b)? == Greater),
        Op::Le => bool_v(compare(&a, &b)? != Greater),
        Op::Ge => bool_v(compare(&a, &b)? != Less),
        _ => unreachable!("not a binary operator: {op:?}"),
    }
}

/// Unary minus is `0 - x`.
pub fn neg(v: Value) -> Result<Value, String> {
    binary(Op::Sub, Value::Int(0), v)
}

pub fn bit_not(v: Value) -> Result<Value, String> {
    match v {
        Value::Int(i) => Ok(Value::Int(!i)),
        other => Err(format!("~ cannot be applied to \"{}\"", other.type_name())),
    }
}

/// `(x, y, z)`; the parts must be numbers.
pub fn vector(x: &Value, y: &Value, z: &Value) -> Result<Value, String> {
    let mut v = [0.0; 3];
    for (slot, part) in v.iter_mut().zip([x, y, z]) {
        *slot = to_f32(part).ok_or_else(|| format!("type {} is not a float", part.type_name()))?;
    }
    Ok(Value::Vector(v))
}

/// `base[key]`.
pub fn index(base: &Value, k: &Value) -> Result<Value, String> {
    match (base, k) {
        (Value::Array(a), k) => {
            let key = match k {
                Value::Int(i) => Key::Int(*i),
                Value::Str(s) => Key::Str(s.clone()),
                other => return Err(format!("{} is not an array index", other.type_name())),
            };
            Ok(a.get(&key).cloned().unwrap_or(Value::Undefined))
        }
        (Value::Str(s), Value::Int(i)) => {
            match usize::try_from(*i).ok().and_then(|i| s.as_bytes().get(i)) {
                Some(b) => Ok(Value::Str(Rc::from(
                    String::from_utf8_lossy(&[*b]).as_ref(),
                ))),
                None => Err(format!("string index {i} out of range")),
            }
        }
        (Value::Str(_), other) => Err(format!("{} is not a string index", other.type_name())),
        (Value::Vector(v), Value::Int(i)) => {
            match usize::try_from(*i).ok().and_then(|i| v.get(i)) {
                Some(c) => Ok(Value::Float(*c)),
                None => Err(format!("vector index {i} out of range")),
            }
        }
        (Value::Vector(_), other) => Err(format!("{} is not a vector index", other.type_name())),
        (other, _) => Err(format!(
            "{} is not an array, string, or vector",
            other.type_name()
        )),
    }
}

/// `.size`: element count of arrays, length of strings, 1 for objects.
pub fn size(v: &Value) -> Result<Value, String> {
    match v {
        Value::Array(a) => Ok(Value::Int(a.len() as i32)),
        Value::Str(s) => Ok(Value::Int(s.len() as i32)),
        Value::Object(_) => Ok(Value::Int(1)),
        other => Err(format!("size cannot be applied to {}", other.type_name())),
    }
}
