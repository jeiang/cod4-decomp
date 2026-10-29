// SPDX-License-Identifier: GPL-3.0-only
//! The wire: requests, replies, the predicate over a state document, and the base64 of a capture.

use serde_json::{Map, Value, json};

/// One request line, taken apart.
#[derive(Clone, Debug)]
pub struct Request {
    /// The connection the request came from; its reply goes back there.
    pub conn: u64,
    /// The channel's number for this request, stable while it waits (see [`crate::Channel`]).
    pub seq: u64,
    /// The caller's `id`, echoed in the reply; `null` when the caller gave none.
    pub id: Value,
    /// The command name, lowercase.
    pub cmd: String,
    /// Everything else the request carried.
    pub args: Map<String, Value>,
}

impl Request {
    /// Takes one line apart. The line must be a JSON object with a string `cmd`.
    pub fn parse(line: &str, conn: u64, seq: u64) -> Result<Self, String> {
        let mut args: Map<String, Value> =
            serde_json::from_str(line).map_err(|e| format!("the request is not JSON: {e}"))?;
        let cmd = args
            .remove("cmd")
            .and_then(|v| v.as_str().map(str::to_ascii_lowercase))
            .ok_or("the request has no cmd")?;
        let id = args.remove("id").unwrap_or(Value::Null);
        Ok(Self {
            conn,
            seq,
            id,
            cmd,
            args,
        })
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.args.get(key)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(Value::as_str)
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.args.get(key).and_then(Value::as_u64)
    }

    pub fn f32(&self, key: &str) -> Option<f32> {
        self.args.get(key).and_then(Value::as_f64).map(|v| v as f32)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.args.get(key).and_then(Value::as_bool)
    }

    /// A number from a number or a string, which is how a script writes one.
    pub fn number(&self, key: &str) -> Option<f64> {
        match self.args.get(key)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

/// One reply line.
#[derive(Clone, Debug)]
pub struct Reply {
    pub id: Value,
    pub ok: bool,
    pub error: Option<String>,
    /// What the request asked for, merged into the reply's object.
    pub fields: Map<String, Value>,
}

impl Reply {
    pub fn ok(id: Value) -> Self {
        Self {
            id,
            ok: true,
            error: None,
            fields: Map::new(),
        }
    }

    pub fn failed(id: Value, why: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            error: Some(why.into()),
            fields: Map::new(),
        }
    }

    /// Adds `key: value` to the reply.
    pub fn set(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.fields.insert(key.to_owned(), value.into());
        self
    }

    pub fn line(&self) -> String {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone());
        m.insert("ok".into(), Value::Bool(self.ok));
        if let Some(e) = &self.error {
            m.insert("error".into(), Value::String(e.clone()));
        }
        for (k, v) in &self.fields {
            m.insert(k.clone(), v.clone());
        }
        Value::Object(m).to_string()
    }
}

/// A reply to `id` from a handler's answer, which carries a message on failure.
pub fn reply(id: &Value, answer: Result<Value, String>) -> Reply {
    match answer {
        Ok(v) => match v {
            Value::Object(m) => {
                let mut r = Reply::ok(id.clone());
                r.fields = m;
                r
            }
            other => Reply::ok(id.clone()).set("value", other),
        },
        Err(why) => Reply::failed(id.clone(), why),
    }
}

/// A number as JSON: a NaN or an infinity, which JSON has no form of, becomes `null`.
pub fn num(v: f64) -> Value {
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}

/// The value at a dotted path (`player.health`, `entities.0.origin`) of `doc`.
pub fn lookup<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = doc;
    for part in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(part)?,
            Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// How an expectation compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
    Exists,
    Len,
}

fn op(name: &str) -> Option<Op> {
    Some(match name {
        "eq" => Op::Eq,
        "ne" => Op::Ne,
        "gt" => Op::Gt,
        "ge" | "at_least" | "min" => Op::Ge,
        "lt" => Op::Lt,
        "le" | "at_most" | "max" => Op::Le,
        "contains" => Op::Contains,
        "exists" => Op::Exists,
        "len" => Op::Len,
        _ => return None,
    })
}

/// Whether `observed` answers one comparator, and the number a relational comparator read.
fn compare(observed: Option<&Value>, op: Op, want: &Value) -> bool {
    let Some(got) = observed else {
        return op == Op::Ne || matches!(want, Value::Bool(false));
    };
    match op {
        Op::Exists => got.as_bool().unwrap_or(true) == want.as_bool().unwrap_or(true),
        Op::Len => {
            let n = match got {
                Value::Array(a) => a.len(),
                Value::Object(m) => m.len(),
                Value::String(s) => s.chars().count(),
                _ => 0,
            };
            compare(Some(&json!(n)), Op::Eq, want)
        }
        Op::Contains => match (got, want) {
            (Value::String(s), Value::String(w)) => s.contains(w.as_str()),
            (Value::Array(a), w) => a.contains(w),
            _ => false,
        },
        Op::Eq => got == want,
        Op::Ne => got != want,
        Op::Gt | Op::Ge | Op::Lt | Op::Le => {
            let (Some(a), Some(b)) = (number(got), number(want)) else {
                return false;
            };
            match op {
                Op::Gt => a > b,
                Op::Ge => a >= b,
                Op::Lt => a < b,
                Op::Le => a <= b,
                _ => unreachable!(),
            }
        }
    }
}

/// A JSON value as a number: a number, a boolean (0 or 1), or a string that parses.
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Whether `doc` answers every expectation of `spec`, a `{path: expectation}` object.
///
/// An expectation is a value to equal, or a comparator object (`{"gt": 3}`, `{"contains": "x"}`,
/// `{"exists": false}`).
pub fn matches(doc: &Value, spec: &Value) -> Result<(), Vec<String>> {
    let Value::Object(want) = spec else {
        return Err(vec![format!("the expectation is not an object: {spec}")]);
    };
    let mut bad = Vec::new();
    for (path, expect) in want {
        let got = lookup(doc, path);
        let (ok, what) = match expect {
            Value::Object(m) if m.len() == 1 && m.keys().next().and_then(|k| op(k)).is_some() => {
                let (name, value) = m.iter().next().expect("checked one key");
                let o = op(name).expect("checked");
                (compare(got, o, value), format!("{name} {value}"))
            }
            Value::Object(m) if m.keys().all(|k| op(k).is_some()) => {
                let all = m
                    .iter()
                    .all(|(name, value)| compare(got, op(name).expect("checked"), value));
                (all, format!("{m:?}"))
            }
            plain => (got == Some(plain), format!("{plain}")),
        };
        if !ok {
            bad.push(format!(
                "{path}: expected {what}, observed {}",
                got.map_or_else(|| "nothing".to_owned(), Value::to_string)
            ));
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

/// The lines of a console's tail as one text, for `contains` expectations.
pub fn text_lines(lines: &[String]) -> String {
    lines.join("\n")
}

/// `data` in base64 (the standard alphabet, padded), for a capture returned inline.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_its_command_and_arguments() {
        let r = Request::parse(r#"{"id":7,"cmd":"Key","name":"w","action":"down"}"#, 2, 3).unwrap();
        assert_eq!((r.conn, r.seq), (2, 3));
        assert_eq!(r.cmd, "key");
        assert_eq!(r.id, json!(7));
        assert_eq!(r.str("name"), Some("w"));
        assert!(Request::parse(r#"{"cmd":1}"#, 0, 1).is_err());
        assert!(Request::parse("not json", 0, 1).is_err());
    }

    #[test]
    fn a_reply_echoes_the_id_and_merges_the_fields() {
        let line = Reply::ok(json!("a")).set("map", "mp_crash").line();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], json!("a"));
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["map"], json!("mp_crash"));
        let line = reply(&json!(2), Err("no menu".into())).line();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"], json!("no menu"));
    }

    #[test]
    fn expectations_read_dotted_paths_and_comparators() {
        let doc = json!({
            "player": {"health": 100, "origin": [1.0, 2.0, 3.0]},
            "menu": {"open": ["main", "class"]},
            "console": {"text": "called a vote"},
        });
        assert!(matches(&doc, &json!({"player.health": 100})).is_ok());
        assert!(matches(&doc, &json!({"player.health": {"gt": 99}})).is_ok());
        assert!(matches(&doc, &json!({"player.health": {"le": 99}})).is_err());
        assert!(matches(&doc, &json!({"player.origin.2": {"ge": 3}})).is_ok());
        assert!(matches(&doc, &json!({"menu.open.0": "main"})).is_ok());
        assert!(matches(&doc, &json!({"console.text": {"contains": "vote"}})).is_ok());
        assert!(matches(&doc, &json!({"menu.shut": {"exists": false}})).is_ok());
        assert!(matches(&doc, &json!({"menu.open": {"len": 2}})).is_ok());
        let bad =
            matches(&doc, &json!({"player.health": {"gt": 100}, "menu.shut": 1})).unwrap_err();
        assert_eq!(bad.len(), 2);
        // A path that is not there answers `ne` and `exists: false`, and nothing else.
        assert!(matches(&doc, &json!({"nope": {"ne": 1}})).is_ok());
        assert!(matches(&doc, &json!({"nope": 1})).is_err());
    }

    #[test]
    fn base64_pads_and_splits_in_three() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
