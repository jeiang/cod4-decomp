// SPDX-License-Identifier: GPL-3.0-only
//! Typed access to builtin arguments, with the original's error wording.

use gsc::{Array, EntRef, Obj, Value};
use std::rc::Rc;

#[derive(Clone, Copy)]
pub struct Args<'a> {
    pub name: &'a str,
    pub v: &'a [Value],
}

impl<'a> Args<'a> {
    pub fn new(name: &'a str, v: &'a [Value]) -> Self {
        Self { name, v }
    }

    pub fn len(&self) -> usize {
        self.v.len()
    }

    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    pub fn get(&self, i: usize) -> Result<&'a Value, String> {
        self.v
            .get(i)
            .ok_or_else(|| format!("parameter {} does not exist", i + 1))
    }

    pub fn opt(&self, i: usize) -> Option<&'a Value> {
        self.v.get(i)
    }

    pub fn int(&self, i: usize) -> Result<i32, String> {
        match self.get(i)? {
            Value::Int(n) => Ok(*n),
            o => Err(format!("type {} is not an int", o.type_name())),
        }
    }

    /// An int or a float.
    pub fn float(&self, i: usize) -> Result<f32, String> {
        match self.get(i)? {
            Value::Float(x) => Ok(*x),
            Value::Int(n) => Ok(*n as f32),
            o => Err(format!("type {} is not a float", o.type_name())),
        }
    }

    pub fn string(&self, i: usize) -> Result<&'a str, String> {
        match self.get(i)? {
            Value::Str(s) => Ok(s),
            o => Err(format!("type {} is not a string", o.type_name())),
        }
    }

    pub fn vector(&self, i: usize) -> Result<[f32; 3], String> {
        match self.get(i)? {
            Value::Vector(v) => Ok(*v),
            o => Err(format!("type {} is not a vector", o.type_name())),
        }
    }

    pub fn array(&self, i: usize) -> Result<&'a Rc<Array>, String> {
        match self.get(i)? {
            Value::Array(a) => Ok(a),
            o => Err(format!("type {} is not an array", o.type_name())),
        }
    }

    /// An entity object; `None` for `undefined`.
    pub fn entity_or_undefined(&self, i: usize) -> Result<Option<EntRef>, String> {
        match self.get(i)? {
            Value::Undefined => Ok(None),
            Value::Object(o) => o
                .entity()
                .map(Some)
                .ok_or_else(|| "not an entity".to_owned()),
            o => Err(format!("type {} is not an entity", o.type_name())),
        }
    }

    pub fn entity(&self, i: usize) -> Result<EntRef, String> {
        match self.get(i)? {
            Value::Object(o) => o.entity().ok_or_else(|| "not an entity".to_owned()),
            o => Err(format!("type {} is not an entity", o.type_name())),
        }
    }

    pub fn object(&self, i: usize) -> Result<&'a Obj, String> {
        match self.get(i)? {
            Value::Object(o) => Ok(o),
            o => Err(format!("type {} is not an object", o.type_name())),
        }
    }

    /// Any value as the string the original's string conversion gives (`Scr_GetString` is
    /// strict, this is for printing).
    pub fn display(&self, i: usize) -> Result<String, String> {
        Ok(display(self.get(i)?))
    }
}

pub fn display(v: &Value) -> String {
    match v {
        Value::Undefined => "undefined".into(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => format_float(*x),
        Value::Str(s) | Value::LocStr(s) => s.to_string(),
        Value::Vector(v) => format!(
            "({}, {}, {})",
            format_float(v[0]),
            format_float(v[1]),
            format_float(v[2])
        ),
        other => format!("<{}>", other.type_name()),
    }
}

/// `%g` as C prints it (six significant digits).
pub fn format_float(x: f32) -> String {
    let x = f64::from(x);
    if x == 0.0 {
        return "0".into();
    }
    let exp = x.abs().log10().floor() as i32;
    if !(-5..6).contains(&exp) {
        let s = format!("{:.5e}", x);
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let m = m.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    let s = format!("{x:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_g() {
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(800.0), "800");
        assert_eq!(format_float(0.05), "0.05");
        assert_eq!(format_float(1.0 / 3.0), "0.333333");
        assert_eq!(format_float(1234567.0), "1.23457e+06");
        assert_eq!(format_float(0.00001), "1e-05");
        assert_eq!(format_float(-2.5), "-2.5");
    }
}
