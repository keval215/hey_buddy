//! Strict, untrusted-JSON front end: size/UTF-8/NUL/depth checks, then a duplicate-key-rejecting
//! parse into a small value tree. Nothing here trusts the model's bytes; the typed plan is built
//! from this tree field by field (unknown fields rejected) and re-serialised canonically.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::error::PlanError;

/// Deepest legal nesting: root{ steps[ step{ target{ ... or step{ keys[ ... } = 4; one spare.
pub(crate) const MAX_DEPTH: usize = 5;

const DUP_MARK: &str = "hb_duplicate_key";

#[derive(Debug)]
pub(crate) enum Val {
    Null,
    Bool,
    /// Non-negative integer that fits u64.
    Uint(u64),
    /// Any other number (negative, fractional, huge).
    OtherNum,
    Str(String),
    Arr(Vec<Val>),
    Obj(Vec<(String, Val)>),
}

struct Strict(Val);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(V)
    }
}

struct V;

impl<'de> Visitor<'de> for V {
    type Value = Strict;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("json")
    }
    fn visit_bool<E>(self, _: bool) -> Result<Strict, E> {
        Ok(Strict(Val::Bool))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Strict, E> {
        Ok(Strict(Val::Uint(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Strict, E> {
        Ok(Strict(if v >= 0 {
            Val::Uint(v as u64)
        } else {
            Val::OtherNum
        }))
    }
    fn visit_f64<E>(self, _: f64) -> Result<Strict, E> {
        Ok(Strict(Val::OtherNum))
    }
    fn visit_str<E>(self, v: &str) -> Result<Strict, E> {
        Ok(Strict(Val::Str(v.to_owned())))
    }
    fn visit_string<E>(self, v: String) -> Result<Strict, E> {
        Ok(Strict(Val::Str(v)))
    }
    fn visit_unit<E>(self) -> Result<Strict, E> {
        Ok(Strict(Val::Null))
    }
    fn visit_none<E>(self) -> Result<Strict, E> {
        Ok(Strict(Val::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
        let mut v = Vec::new();
        while let Some(Strict(x)) = a.next_element()? {
            // The draft is capped at 16 KiB, so this is bounded; the cap is belt and braces.
            if v.len() >= 4096 {
                return Err(de::Error::custom("hb_too_many"));
            }
            v.push(x);
        }
        Ok(Strict(Val::Arr(v)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
        let mut v: Vec<(String, Val)> = Vec::new();
        while let Some((k, Strict(x))) = a.next_entry::<String, Strict>()? {
            if v.iter().any(|(e, _)| *e == k) {
                return Err(de::Error::custom(DUP_MARK));
            }
            v.push((k, x));
        }
        Ok(Strict(Val::Obj(v)))
    }
}

/// Maximum `{`/`[` nesting outside strings (cheap pre-scan; runs before any parsing).
fn max_depth(b: &[u8]) -> usize {
    let (mut depth, mut max, mut in_str, mut esc) = (0usize, 0usize, false, false);
    for &c in b {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Bytes -> value tree. Order: size, UTF-8, NUL, depth, JSON syntax + duplicate keys.
pub(crate) fn parse_bytes(draft: &[u8], max_bytes: usize) -> Result<Val, PlanError> {
    if draft.len() > max_bytes {
        return Err(PlanError::TooLarge);
    }
    let s = std::str::from_utf8(draft).map_err(|_| PlanError::NotUtf8)?;
    if draft.contains(&0) {
        return Err(PlanError::Nul);
    }
    if max_depth(draft) > MAX_DEPTH {
        return Err(PlanError::TooDeep);
    }
    match serde_json::from_str::<Strict>(s) {
        Ok(Strict(v)) => Ok(v),
        Err(e) => {
            // Only the fixed marker is inspected; the serde message itself is dropped.
            if e.to_string().starts_with(DUP_MARK) {
                Err(PlanError::DuplicateKey)
            } else {
                Err(PlanError::Malformed)
            }
        }
    }
}

/// An object whose fields are consumed one by one; leftovers are unknown fields.
pub(crate) struct Obj(Vec<(String, Val)>);

impl Obj {
    pub(crate) fn from(v: Val) -> Result<Self, PlanError> {
        match v {
            Val::Obj(items) => Ok(Self(items)),
            _ => Err(PlanError::WrongType),
        }
    }

    pub(crate) fn take(&mut self, key: &str) -> Option<Val> {
        let i = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(i).1)
    }

    pub(crate) fn require(&mut self, key: &str) -> Result<Val, PlanError> {
        self.take(key).ok_or(PlanError::MissingField)
    }

    /// Errors if any field was not consumed.
    pub(crate) fn finish(self) -> Result<(), PlanError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(PlanError::UnknownField)
        }
    }
}

pub(crate) fn as_string(v: Val) -> Result<String, PlanError> {
    match v {
        Val::Str(s) => Ok(s),
        _ => Err(PlanError::WrongType),
    }
}

pub(crate) fn as_array(v: Val) -> Result<Vec<Val>, PlanError> {
    match v {
        Val::Arr(a) => Ok(a),
        _ => Err(PlanError::WrongType),
    }
}
