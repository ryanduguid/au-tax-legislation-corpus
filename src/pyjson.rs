//! JSON read and written as CPython 3.15's `json` module does.
//!
//! Documents are read with the C scanner's grammar (NaN and infinities,
//! arbitrary-size integers, the digit limit on `int`), and a repeated member
//! is refused when its object closes, as the radar's `object_pairs_hook`
//! refused it. A `\uXXXX` escape may leave a lone surrogate in a string, as
//! Python's `str` allows; such a string is kept as code points. Output
//! matches `json.dumps` with the radar's options.
use std::fmt::Write as _;

use num_bigint::BigInt;

use crate::py;

/// A JSON string: text, or code points that include a lone surrogate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PyStr {
    Text(String),
    Lone(Vec<u32>),
}

impl PyStr {
    pub fn from_code_points(points: Vec<u32>) -> PyStr {
        if points.iter().all(|&cp| char::from_u32(cp).is_some()) {
            PyStr::Text(points.into_iter().filter_map(char::from_u32).collect())
        } else {
            PyStr::Lone(points)
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PyStr::Text(s) => Some(s),
            PyStr::Lone(_) => None,
        }
    }

    /// The string's code points, surrogates included.
    pub fn code_points(&self) -> Vec<u32> {
        match self {
            PyStr::Text(s) => s.chars().map(u32::from).collect(),
            PyStr::Lone(points) => points.clone(),
        }
    }
}

/// A decoded JSON value, keeping object member order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(PyStr),
    Array(Vec<Json>),
    Object(Vec<(PyStr, Json)>),
}

impl Json {
    pub fn str(text: &str) -> Json {
        Json::Str(PyStr::Text(text.to_string()))
    }

    pub fn obj<const N: usize>(pairs: [(&str, Json); N]) -> Json {
        Json::Object(
            pairs
                .into_iter()
                .map(|(k, v)| (PyStr::Text(k.to_string()), v))
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(pairs) => pairs
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// `value[key]` for a member the caller has already shown to exist.
    pub fn at(&self, key: &str) -> &Json {
        self.get(key).unwrap_or(&Json::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => s.as_str(),
            _ => None,
        }
    }

    /// `set(value) == keys` for a dict; false for anything else.
    pub fn has_exactly(&self, keys: &[&str]) -> bool {
        let Json::Object(pairs) = self else {
            return false;
        };
        pairs.len() == keys.len()
            && pairs
                .iter()
                .all(|(k, _)| k.as_str().is_some_and(|k| keys.contains(&k)))
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Int(i) => i != &BigInt::from(0),
            Json::Float(f) => *f != 0.0,
            Json::Str(PyStr::Text(s)) => !s.is_empty(),
            Json::Str(PyStr::Lone(_)) => true,
            Json::Array(items) => !items.is_empty(),
            Json::Object(pairs) => !pairs.is_empty(),
        }
    }
}

/// Why a JSON text could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    /// `JSONDecodeError` or the `ValueError` of the `int` digit limit.
    Invalid(String),
    /// The `object_pairs_hook` found a repeated member.
    Duplicate,
}

fn decode_error(msg: &str, doc: &[char], pos: usize) -> DecodeError {
    let lineno = doc[..pos].iter().filter(|c| **c == '\n').count() + 1;
    let colno = match doc[..pos].iter().rposition(|c| *c == '\n') {
        Some(i) => pos - i,
        None => pos + 1,
    };
    DecodeError::Invalid(format!("{msg}: line {lineno} column {colno} (char {pos})"))
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

enum Frame {
    Array(Vec<Json>),
    Object(Vec<(PyStr, Json)>, PyStr),
}

/// `json.loads(text, object_pairs_hook=_reject_duplicate_json_members)`.
pub fn loads(text: &str) -> Result<Json, DecodeError> {
    decode(text, true)
}

/// `json.loads(text)`: a repeated member keeps its first position and takes
/// the last value, as a `dict` built from the pairs does.
pub fn loads_standard(text: &str) -> Result<Json, DecodeError> {
    decode(text, false)
}

fn decode(text: &str, reject_duplicates: bool) -> Result<Json, DecodeError> {
    let s: Vec<char> = text.chars().collect();
    if s.first() == Some(&'\u{feff}') {
        return Err(decode_error(
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
            &s,
            0,
        ));
    }
    let len = s.len();
    let skip = |mut i: usize| {
        while i < len && is_ws(s[i]) {
            i += 1;
        }
        i
    };
    let mut idx = skip(0);
    let mut stack: Vec<Frame> = Vec::new();
    let expecting_value = |pos: usize| decode_error("Expecting value", &s, pos);

    // Reads a key and its delimiter, leaving idx at the value.
    let read_key = |idx: &mut usize| -> Result<PyStr, DecodeError> {
        if *idx >= len || s[*idx] != '"' {
            return Err(decode_error(
                "Expecting property name enclosed in double quotes",
                &s,
                *idx,
            ));
        }
        let (key, next) = scanstring(&s, *idx + 1)?;
        *idx = skip(next);
        if *idx >= len || s[*idx] != ':' {
            return Err(decode_error("Expecting ':' delimiter", &s, *idx));
        }
        *idx = skip(*idx + 1);
        Ok(key)
    };

    'value: loop {
        if idx >= len {
            return Err(expecting_value(idx));
        }
        let mut value = match s[idx] {
            '"' => {
                let (string, next) = scanstring(&s, idx + 1)?;
                idx = next;
                Json::Str(string)
            }
            '{' => {
                idx = skip(idx + 1);
                if idx < len && s[idx] == '}' {
                    idx += 1;
                    Json::Object(Vec::new())
                } else {
                    let key = read_key(&mut idx)?;
                    stack.push(Frame::Object(Vec::new(), key));
                    continue 'value;
                }
            }
            '[' => {
                idx = skip(idx + 1);
                if idx < len && s[idx] == ']' {
                    idx += 1;
                    Json::Array(Vec::new())
                } else {
                    stack.push(Frame::Array(Vec::new()));
                    continue 'value;
                }
            }
            _ => {
                let literal = |word: &str| {
                    s[idx..]
                        .iter()
                        .take(word.chars().count())
                        .copied()
                        .eq(word.chars())
                };
                if literal("null") {
                    idx += 4;
                    Json::Null
                } else if literal("true") {
                    idx += 4;
                    Json::Bool(true)
                } else if literal("false") {
                    idx += 5;
                    Json::Bool(false)
                } else if literal("NaN") {
                    idx += 3;
                    Json::Float(f64::NAN)
                } else if literal("Infinity") {
                    idx += 8;
                    Json::Float(f64::INFINITY)
                } else if literal("-Infinity") {
                    idx += 9;
                    Json::Float(f64::NEG_INFINITY)
                } else {
                    let (number, next) =
                        match_number(&s, idx).ok_or_else(|| expecting_value(idx))??;
                    idx = next;
                    number
                }
            }
        };
        // Attach the finished value to its container, closing any that end here.
        loop {
            let Some(frame) = stack.last_mut() else {
                let end = skip(idx);
                if end != len {
                    return Err(decode_error("Extra data", &s, end));
                }
                return Ok(value);
            };
            match frame {
                Frame::Array(items) => {
                    items.push(value);
                    idx = skip(idx);
                    if idx < len && s[idx] == ']' {
                        idx += 1;
                        let Some(Frame::Array(items)) = stack.pop() else {
                            unreachable!()
                        };
                        value = Json::Array(items);
                        continue;
                    }
                    if idx >= len || s[idx] != ',' {
                        return Err(decode_error("Expecting ',' delimiter", &s, idx));
                    }
                    let comma = idx;
                    idx = skip(idx + 1);
                    if idx < len && s[idx] == ']' {
                        return Err(decode_error(
                            "Illegal trailing comma before end of array",
                            &s,
                            comma,
                        ));
                    }
                    continue 'value;
                }
                Frame::Object(pairs, key) => {
                    pairs.push((std::mem::replace(key, PyStr::Text(String::new())), value));
                    idx = skip(idx);
                    if idx < len && s[idx] == '}' {
                        idx += 1;
                        let Some(Frame::Object(pairs, _)) = stack.pop() else {
                            unreachable!()
                        };
                        let mut keys = std::collections::HashMap::with_capacity(pairs.len());
                        let mut unique: Vec<(PyStr, Json)> = Vec::with_capacity(pairs.len());
                        for (k, v) in pairs {
                            if let Some(&at) = keys.get(&k) {
                                if reject_duplicates {
                                    return Err(DecodeError::Duplicate);
                                }
                                unique[at] = (k, v);
                            } else {
                                keys.insert(k.clone(), unique.len());
                                unique.push((k, v));
                            }
                        }
                        value = Json::Object(unique);
                        continue;
                    }
                    if idx >= len || s[idx] != ',' {
                        return Err(decode_error("Expecting ',' delimiter", &s, idx));
                    }
                    let comma = idx;
                    idx = skip(idx + 1);
                    if idx < len && s[idx] == '}' {
                        return Err(decode_error(
                            "Illegal trailing comma before end of object",
                            &s,
                            comma,
                        ));
                    }
                    let next_key = read_key(&mut idx)?;
                    if let Some(Frame::Object(_, key)) = stack.last_mut() {
                        *key = next_key;
                    }
                    continue 'value;
                }
            }
        }
    }
}

/// The C scanner's number match; None where it raises StopIteration.
fn match_number(s: &[char], start: usize) -> Option<Result<(Json, usize), DecodeError>> {
    let len = s.len();
    let digit = |i: usize| i < len && s[i].is_ascii_digit();
    let mut idx = start;
    if s[idx] == '-' {
        idx += 1;
        if idx >= len {
            return None;
        }
    }
    if ('1'..='9').contains(&s[idx]) {
        idx += 1;
        while digit(idx) {
            idx += 1;
        }
    } else if s[idx] == '0' {
        idx += 1;
    } else {
        return None;
    }
    let mut is_float = false;
    if idx + 1 < len && s[idx] == '.' && s[idx + 1].is_ascii_digit() {
        is_float = true;
        idx += 2;
        while digit(idx) {
            idx += 1;
        }
    }
    if idx + 1 < len && (s[idx] == 'e' || s[idx] == 'E') {
        let e_start = idx;
        idx += 1;
        if idx + 1 < len && (s[idx] == '-' || s[idx] == '+') {
            idx += 1;
        }
        while digit(idx) {
            idx += 1;
        }
        if s[idx - 1].is_ascii_digit() {
            is_float = true;
        } else {
            idx = e_start;
        }
    }
    let text: String = s[start..idx].iter().collect();
    if is_float {
        let value: f64 = text.parse().expect("a JSON float literal");
        Some(Ok((Json::Float(value), idx)))
    } else {
        Some(
            py::parse_int(&text, 10)
                .map(|i| (Json::Int(i), idx))
                .map_err(DecodeError::Invalid),
        )
    }
}

/// The C `scanstring`: `end` is the index after the opening quote.
fn scanstring(s: &[char], end: usize) -> Result<(PyStr, usize), DecodeError> {
    let len = s.len();
    let begin = end - 1;
    let mut out: Vec<u32> = Vec::new();
    let mut end = end;
    loop {
        let mut next = end;
        let mut c = '\0';
        while next < len {
            c = s[next];
            if c == '"' || c == '\\' {
                break;
            }
            if (c as u32) <= 0x1f {
                return Err(decode_error("Invalid control character at", s, next));
            }
            next += 1;
        }
        if next >= len || c != '"' && c != '\\' {
            return Err(decode_error("Unterminated string starting at", s, begin));
        }
        out.extend(s[end..next].iter().map(|&c| u32::from(c)));
        next += 1;
        if c == '"' {
            return Ok((PyStr::from_code_points(out), next));
        }
        if next == len {
            return Err(decode_error("Unterminated string starting at", s, begin));
        }
        let c = s[next];
        if c != 'u' {
            end = next + 1;
            let replacement = match c {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\x08',
                'f' => '\x0c',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return Err(decode_error("Invalid \\escape", s, end - 2)),
            };
            out.push(u32::from(replacement));
            continue;
        }
        next += 1;
        end = next + 4;
        if end > len {
            return Err(decode_error("Invalid \\uXXXX escape", s, next - 1));
        }
        let hex = |from: usize, end: usize| -> Result<u32, DecodeError> {
            let mut value = 0;
            for &digit in &s[from..from + 4] {
                value = value << 4
                    | digit
                        .to_digit(16)
                        .ok_or_else(|| decode_error("Invalid \\uXXXX escape", s, end - 5))?;
            }
            Ok(value)
        };
        let mut code = hex(next, end)?;
        // CPython 3.15 reads a following `\u` as the low half, and refuses it
        // there when fewer than 4 hex digits follow.
        if (0xd800..=0xdbff).contains(&code) && end + 1 < len && s[end] == '\\' && s[end + 1] == 'u'
        {
            if end + 6 > len {
                return Err(decode_error("Invalid \\uXXXX escape", s, end + 1));
            }
            let low = hex(end + 2, end + 6)?;
            if (0xdc00..=0xdfff).contains(&low) {
                code = 0x10000 + (((code - 0xd800) << 10) | (low - 0xdc00));
                end += 6;
            }
        }
        out.push(code);
    }
}

/// `json.loads(bytes)`: the encoding `json.detect_encoding` picks, decoded with
/// `surrogatepass`. The error is None for a `UnicodeDecodeError`, else what
/// the scanner refused.
pub fn loads_bytes(data: &[u8]) -> Result<Json, Option<DecodeError>> {
    let points: Vec<u32> = (|| {
        Some(
            if data.starts_with(&[0, 0, 0xfe, 0xff]) || data.starts_with(&[0xff, 0xfe, 0, 0]) {
                utf32(data, None)?
            } else if data.starts_with(&[0xfe, 0xff]) || data.starts_with(&[0xff, 0xfe]) {
                utf16(data, None)?
            } else if let Some(rest) = data.strip_prefix(b"\xef\xbb\xbf") {
                utf8_surrogatepass(rest)?
            } else if data.len() >= 4 && data[0] == 0 {
                if data[1] != 0 {
                    utf16(data, Some(true))?
                } else {
                    utf32(data, Some(true))?
                }
            } else if data.len() >= 4 && data[1] == 0 {
                if data[2] != 0 || data[3] != 0 {
                    utf16(data, Some(false))?
                } else {
                    utf32(data, Some(false))?
                }
            } else if data.len() == 2 && data[0] == 0 {
                utf16(data, Some(true))?
            } else if data.len() == 2 && data[1] == 0 {
                utf16(data, Some(false))?
            } else {
                utf8_surrogatepass(data)?
            },
        )
    })()
    .ok_or(None)?;
    // A raw lone surrogate becomes an escape, which the scanner turns back into one.
    let mut text = String::with_capacity(points.len());
    for cp in points {
        match char::from_u32(cp) {
            Some(c) => text.push(c),
            None => text.push_str(&format!("\\u{cp:04x}")),
        }
    }
    loads_standard(&text).map_err(Some)
}

fn utf8_surrogatepass(data: &[u8]) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        // An encoded surrogate (ED A0..BF xx) is accepted as a lone code point.
        if data[i] == 0xed
            && i + 2 < data.len()
            && (0xa0..=0xbf).contains(&data[i + 1])
            && (0x80..=0xbf).contains(&data[i + 2])
        {
            out.push(0xd000 | (u32::from(data[i + 1] & 0x3f) << 6) | u32::from(data[i + 2] & 0x3f));
            i += 3;
            continue;
        }
        let end = (i + 4).min(data.len());
        let chunk = match std::str::from_utf8(&data[i..end]) {
            Ok(s) => s,
            Err(e) if e.valid_up_to() > 0 => {
                std::str::from_utf8(&data[i..i + e.valid_up_to()]).ok()?
            }
            Err(_) => return None,
        };
        let c = chunk.chars().next()?;
        out.push(u32::from(c));
        i += c.len_utf8();
    }
    Some(out)
}

fn utf16(data: &[u8], big: Option<bool>) -> Option<Vec<u32>> {
    let (big, body) = match big {
        Some(b) => (b, data),
        None => (data[0] == 0xfe, &data[2..]),
    };
    if body.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = body
        .chunks(2)
        .map(|c| {
            if big {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = u32::from(units[i]);
        if (0xd800..0xdc00).contains(&u)
            && i + 1 < units.len()
            && (0xdc00..0xe000).contains(&u32::from(units[i + 1]))
        {
            out.push(0x10000 + ((u - 0xd800) << 10) + (u32::from(units[i + 1]) - 0xdc00));
            i += 2;
        } else {
            out.push(u);
            i += 1;
        }
    }
    Some(out)
}

fn utf32(data: &[u8], big: Option<bool>) -> Option<Vec<u32>> {
    let (big, body) = match big {
        Some(b) => (b, data),
        None => (data[0] == 0, &data[4..]),
    };
    if body.len() % 4 != 0 {
        return None;
    }
    body.chunks(4)
        .map(|c| {
            let v = if big {
                u32::from_be_bytes([c[0], c[1], c[2], c[3]])
            } else {
                u32::from_le_bytes([c[0], c[1], c[2], c[3]])
            };
            (v <= 0x10ffff).then_some(v)
        })
        .collect()
}

/// `json.dumps` options the radar uses; `ensure_ascii` is always on.
#[derive(Clone, Copy)]
pub enum Layout {
    /// `indent=2, sort_keys=True`.
    Indented,
    /// `sort_keys=True, separators=(",", ":")`.
    Canonical,
}

fn dump_str(points: &[u32], out: &mut String) {
    out.push('"');
    for &cp in points {
        match cp {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(char::from_u32(cp).unwrap_or('?')),
            0x10000.. => {
                let v = cp - 0x10000;
                let _ = write!(
                    out,
                    "\\u{:04x}\\u{:04x}",
                    0xd800 + (v >> 10),
                    0xdc00 + (v & 0x3ff)
                );
            }
            _ => {
                let _ = write!(out, "\\u{cp:04x}");
            }
        }
    }
    out.push('"');
}

fn dump_into(value: &Json, level: usize, layout: Layout, out: &mut String) {
    let newline = |out: &mut String, level: usize| {
        if let Layout::Indented = layout {
            out.push('\n');
            out.push_str(&"  ".repeat(level));
        }
    };
    let (comma, colon) = match layout {
        Layout::Indented => (",", ": "),
        Layout::Canonical => (",", ":"),
    };
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(i) => out.push_str(&i.to_string()),
        Json::Float(f) if f.is_nan() => out.push_str("NaN"),
        Json::Float(f) if f.is_infinite() => {
            out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" })
        }
        Json::Float(f) => out.push_str(&py::float_repr(*f)),
        Json::Str(s) => dump_str(&s.code_points(), out),
        Json::Array(items) if items.is_empty() => out.push_str("[]"),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(comma);
                }
                newline(out, level + 1);
                dump_into(item, level + 1, layout, out);
            }
            newline(out, level);
            out.push(']');
        }
        Json::Object(pairs) if pairs.is_empty() => out.push_str("{}"),
        Json::Object(pairs) => {
            let mut order: Vec<(Vec<u32>, &Json)> =
                pairs.iter().map(|(k, v)| (k.code_points(), v)).collect();
            order.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (i, (k, v)) in order.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(comma);
                }
                newline(out, level + 1);
                dump_str(&k, out);
                out.push_str(colon);
                dump_into(v, level + 1, layout, out);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

/// `json.dumps(value, ...)` with the given layout.
pub fn dumps(value: &Json, layout: Layout) -> String {
    let mut out = String::new();
    dump_into(value, 0, layout, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> DecodeError {
        loads(text).unwrap_err()
    }

    #[test]
    fn errors_match_the_c_scanner() {
        let invalid = |m: &str| DecodeError::Invalid(m.to_string());
        assert_eq!(
            err("{\"a\":1,}"),
            invalid("Illegal trailing comma before end of object: line 1 column 7 (char 6)")
        );
        assert_eq!(
            err("\"\\q\""),
            invalid("Invalid \\escape: line 1 column 2 (char 1)")
        );
        assert_eq!(err("1 2"), invalid("Extra data: line 1 column 3 (char 2)"));
        assert_eq!(err("{\"a\":1,\"a\":2}"), DecodeError::Duplicate);
        // A repeated member is found when its object closes, before a later syntax error.
        assert_eq!(err("[{\"a\":1,\"a\":2}, }"), DecodeError::Duplicate);
        assert!(matches!(
            err("[{\"a\":1, }, {\"a\":1,\"a\":2}]"),
            DecodeError::Invalid(_)
        ));
    }

    #[test]
    fn lone_surrogates_survive_as_python_keeps_them() {
        let value = loads("[\"\\ud800\", \"\\ud83d\\ude00\", \"\\udc00\\ud800x\"]").unwrap();
        let Json::Array(items) = &value else { panic!() };
        assert_eq!(items[0], Json::Str(PyStr::Lone(vec![0xd800])));
        assert_eq!(items[1], Json::str("\u{1f600}"));
        assert_eq!(items[2], Json::Str(PyStr::Lone(vec![0xdc00, 0xd800, 0x78])));
        assert_eq!(
            loads("{\"\\ud800\":1,\"\\ud800\":2}").unwrap_err(),
            DecodeError::Duplicate
        );
    }

    #[test]
    fn dumps_matches_python() {
        let value = loads("{\"b\": [1, 2.5, \"\u{e9}\\n\\u0001\u{1f600}\"], \"a\": {}, \"c\": []}")
            .unwrap();
        assert_eq!(
            dumps(&value, Layout::Indented),
            "{\n  \"a\": {},\n  \"b\": [\n    1,\n    2.5,\n    \"\\u00e9\\n\\u0001\\ud83d\\ude00\"\n  ],\n  \"c\": []\n}"
        );
        assert_eq!(
            dumps(&value, Layout::Canonical),
            "{\"a\":{},\"b\":[1,2.5,\"\\u00e9\\n\\u0001\\ud83d\\ude00\"],\"c\":[]}"
        );
    }

    /// Probed against CPython 3.15's scanner (tools/json_escape_probe.py in the
    /// port's evidence).
    #[test]
    fn truncated_escapes_fail_where_cpython_315_fails() {
        let unterminated = "Unterminated string starting at: line 1 column 1 (char 0)";
        let invalid = |at: usize| {
            format!(
                "Invalid \\uXXXX escape: line 1 column {} (char {at})",
                at + 1
            )
        };
        let cases: [(&str, String); 11] = [
            (r#""\ud83d"#, unterminated.into()),
            (r#""é"#, unterminated.into()),
            (r#""\u00e"#, invalid(2)),
            (r#""\ud83d\"#, unterminated.into()),
            (r#""\ud83d\u"#, invalid(8)),
            (r#""\ud83d\ude0"#, invalid(8)),
            (r#""😀"#, unterminated.into()),
            (r#""\ud83d\u12""#, invalid(8)),
            (r#""\ud83d\uzzzz""#, invalid(8)),
            (r#""\ud83dx"#, unterminated.into()),
            (r#""😀x"#, unterminated.into()),
        ];
        for (text, message) in cases {
            assert_eq!(err(text), DecodeError::Invalid(message), "{text}");
        }
        assert_eq!(
            loads(r#""\ud83d\ud83d""#).unwrap(),
            Json::Str(PyStr::Lone(vec![0xd83d, 0xd83d]))
        );
    }
}
