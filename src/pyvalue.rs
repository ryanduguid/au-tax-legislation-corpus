//! Python operations on decoded JSON values: `type()`, `repr()`, `str()`,
//! `==`, `hash()` and one-line `json.dumps`, and the bytes `print` writes.
use std::fmt::Write as _;

use num_bigint::BigInt;
use num_bigint::Sign;

use crate::py;
use crate::pyjson::{Json, PyStr};

/// `type(value).__name__`.
pub fn type_name(value: &Json) -> &'static str {
    match value {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Int(_) => "int",
        Json::Float(_) => "float",
        Json::Str(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// `repr()` of a `str` given as code points, lone surrogates included.
pub fn repr_points(points: &[u32]) -> String {
    let has = |c: char| points.contains(&u32::from(c));
    let quote = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for &cp in points {
        match char::from_u32(cp) {
            Some(c) if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            // repr_str renders one character as Python does once the quote is settled.
            Some(c) if c != '\'' && c != '"' => {
                let one = py::repr_str(&c.to_string());
                out.push_str(&one[1..one.len() - 1]);
            }
            Some(c) => out.push(c),
            None => {
                let _ = write!(out, "\\u{cp:04x}");
            }
        }
    }
    out.push(quote);
    out
}

fn repr_pystr(s: &PyStr) -> String {
    match s {
        PyStr::Text(text) => py::repr_str(text),
        PyStr::Lone(points) => repr_points(points),
    }
}

/// `repr(value)`.
pub fn repr(value: &Json) -> String {
    match value {
        Json::Null => "None".into(),
        Json::Bool(true) => "True".into(),
        Json::Bool(false) => "False".into(),
        Json::Int(i) => py::int_repr(i).unwrap_or_else(|e| e),
        Json::Float(f) => py::float_repr(*f),
        Json::Str(s) => repr_pystr(s),
        Json::Array(items) => format!(
            "[{}]",
            items.iter().map(repr).collect::<Vec<_>>().join(", ")
        ),
        Json::Object(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_pystr(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(value)` as code points: a `str` is itself, anything else its `repr`.
pub fn str_points(value: &Json) -> Vec<u32> {
    match value {
        Json::Str(s) => s.code_points(),
        other => repr(other).chars().map(u32::from).collect(),
    }
}

/// `str(value)` where the caller knows no lone surrogate can occur, or does not care.
pub fn py_str(value: &Json) -> String {
    match value {
        Json::Str(PyStr::Text(s)) => s.clone(),
        other => points_lossy(&str_points(other)),
    }
}

/// `str(value)` as stderr writes it: `backslashreplace` turns a lone surrogate into `\udXXX`.
pub fn str_for_stderr(value: &Json) -> String {
    str_points(value)
        .into_iter()
        .map(|cp| char::from_u32(cp).map_or_else(|| format!("\\u{cp:04x}"), String::from))
        .collect()
}

pub fn points_lossy(points: &[u32]) -> String {
    points
        .iter()
        .map(|&cp| char::from_u32(cp).unwrap_or('\u{fffd}'))
        .collect()
}

/// An exact number: Python compares `bool`, `int` and `float` by value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Number {
    Int(BigInt),
    /// A non-integral finite float, by its bits.
    Fraction(u64),
    Infinite(bool),
    /// NaN, which equals nothing under `==`; the id tells identical objects apart.
    Nan(usize),
}

fn number(value: &Json, nan_id: usize) -> Option<Number> {
    Some(match value {
        Json::Bool(b) => Number::Int(BigInt::from(u8::from(*b))),
        Json::Int(i) => Number::Int(i.clone()),
        Json::Float(f) if f.is_nan() => Number::Nan(nan_id),
        Json::Float(f) if f.is_infinite() => Number::Infinite(*f > 0.0),
        Json::Float(f) if f.fract() == 0.0 => Number::Int(float_to_int(*f)),
        Json::Float(f) => Number::Fraction(f.to_bits()),
        _ => return None,
    })
}

/// The exact integer value of an integral finite float.
pub fn float_to_int(f: f64) -> BigInt {
    let bits = f.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = if exponent == 0 {
        (bits & ((1 << 52) - 1)) << 1
    } else {
        (bits & ((1 << 52) - 1)) | (1 << 52)
    };
    let shift = exponent - 1075;
    let magnitude = if shift >= 0 {
        BigInt::from(mantissa) << shift as usize
    } else {
        BigInt::from(mantissa >> (-shift).min(63) as u64)
    };
    if f.is_sign_negative() && magnitude.sign() != Sign::NoSign {
        -magnitude
    } else {
        magnitude
    }
}

/// `a == b` for decoded JSON values. `json` decodes every NaN to one shared
/// object: NaN equals nothing under `==`, but inside a list or dict the
/// comparison checks identity first, so a nested NaN equals another.
pub fn py_eq(a: &Json, b: &Json) -> bool {
    eq(a, b, false)
}

/// `a == b` as a tuple, list or dict compares its items: identity first, so a
/// NaN from `json` equals itself.
pub fn item_eq(a: &Json, b: &Json) -> bool {
    eq(a, b, true)
}

fn eq(a: &Json, b: &Json, nested: bool) -> bool {
    if let (Some(x), Some(y)) = (number(a, 0), number(b, usize::from(!nested))) {
        return x == y;
    }
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Str(x), Json::Str(y)) => x.code_points() == y.code_points(),
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| eq(p, q, true))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .any(|(k2, v2)| k.code_points() == k2.code_points() && eq(v, v2, true))
                })
        }
        _ => false,
    }
}

/// A hashable Python value used as a `dict` key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    None,
    Number(Number),
    Str(Vec<u32>),
    Tuple(Vec<Key>),
}

/// `hash(value)`'s equivalence class, or Python's `TypeError` for a list or dict.
///
/// Every NaN `json` decodes is one object, so a dict finds it by identity.
pub fn key(value: &Json) -> Result<Key, String> {
    if let Some(n) = number(value, 0) {
        return Ok(Key::Number(n));
    }
    match value {
        Json::Null => Ok(Key::None),
        Json::Str(s) => Ok(Key::Str(s.code_points())),
        other => Err(format!("unhashable type: '{}'", type_name(other))),
    }
}

/// `json.dumps(value)` on one line with the default separators, as code points.
pub fn dumps_line(value: &Json, ensure_ascii: bool) -> Vec<u32> {
    let mut out = Vec::new();
    dump(value, ensure_ascii, &mut out);
    out
}

pub(crate) fn push(out: &mut Vec<u32>, text: &str) {
    out.extend(text.chars().map(u32::from));
}

pub(crate) fn dump_str(
    points: impl IntoIterator<Item = u32>,
    ensure_ascii: bool,
    out: &mut Vec<u32>,
) {
    out.push(u32::from('"'));
    for cp in points {
        match cp {
            0x22 => push(out, "\\\""),
            0x5c => push(out, "\\\\"),
            0x0a => push(out, "\\n"),
            0x0d => push(out, "\\r"),
            0x09 => push(out, "\\t"),
            0x08 => push(out, "\\b"),
            0x0c => push(out, "\\f"),
            0..=0x1f => push(out, &format!("\\u{cp:04x}")),
            0x20..=0x7e => out.push(cp),
            _ if !ensure_ascii => out.push(cp),
            0x10000.. => {
                let v = cp - 0x10000;
                push(
                    out,
                    &format!(
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (v >> 10),
                        0xdc00 + (v & 0x3ff)
                    ),
                );
            }
            _ => push(out, &format!("\\u{cp:04x}")),
        }
    }
    out.push(u32::from('"'));
}

fn dump(value: &Json, ensure_ascii: bool, out: &mut Vec<u32>) {
    match value {
        Json::Null => push(out, "null"),
        Json::Bool(true) => push(out, "true"),
        Json::Bool(false) => push(out, "false"),
        Json::Int(i) => push(out, &i.to_string()),
        Json::Float(f) if f.is_nan() => push(out, "NaN"),
        Json::Float(f) if f.is_infinite() => {
            push(out, if *f > 0.0 { "Infinity" } else { "-Infinity" })
        }
        Json::Float(f) => push(out, &py::float_repr(*f)),
        Json::Str(s) => dump_str(s.points(), ensure_ascii, out),
        Json::Array(items) => {
            out.push(u32::from('['));
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    push(out, ", ");
                }
                dump(item, ensure_ascii, out);
            }
            out.push(u32::from(']'));
        }
        Json::Object(pairs) => {
            out.push(u32::from('{'));
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    push(out, ", ");
                }
                dump_str(k.points(), ensure_ascii, out);
                push(out, ": ");
                dump(v, ensure_ascii, out);
            }
            out.push(u32::from('}'));
        }
    }
}

/// `json.dumps(value, indent=indent, ensure_ascii=False, sort_keys=True)`.
pub fn dumps_sorted(value: &Json, indent: Option<usize>) -> Vec<u32> {
    dumps_with(value, indent, false, true)
}

/// `json.dumps(value, indent=indent, ensure_ascii=ensure_ascii, sort_keys=sort_keys)`.
pub fn dumps_with(
    value: &Json,
    indent: Option<usize>,
    ensure_ascii: bool,
    sort_keys: bool,
) -> Vec<u32> {
    dumps_until(value, indent, ensure_ascii, sort_keys, usize::MAX)
}

/// `dumps_with`'s text up to the first container nested `limit` deep (the
/// top-level value is depth 0): what `json.dump`'s pure-Python encoder has
/// written when its recursion check stops it entering that container.
pub fn dumps_until(
    value: &Json,
    indent: Option<usize>,
    ensure_ascii: bool,
    sort_keys: bool,
    limit: usize,
) -> Vec<u32> {
    walk_until(value, (indent, ensure_ascii, sort_keys, limit), false).0
}

/// Why `json.dump` stopped before the end of a value.
#[derive(Debug, PartialEq)]
pub enum Stop {
    /// Its recursion check refused a container nested `limit` deep.
    Depth,
    /// A strict UTF-8 text file could not encode this chunk: a string
    /// holding a lone surrogate, with the separator before it in a list.
    Unencodable(Vec<u32>),
}

/// `json.dump(value, f, indent=indent, ensure_ascii=False)` into a strict
/// UTF-8 text file: the text written before it stops, and why it stopped,
/// at the first container nested `limit` deep or the first chunk the file
/// cannot encode, whichever the encoder reaches first.
pub fn dump_until_stop(
    value: &Json,
    indent: Option<usize>,
    sort_keys: bool,
    limit: usize,
) -> (Vec<u32>, Option<Stop>) {
    walk_until(value, (indent, false, sort_keys, limit), true)
}

/// The encoder's walk. With `stop_lone`, a string holding a lone surrogate
/// ends it at the start of the chunk the encoder yields for that string.
fn walk_until(
    value: &Json,
    options: (Option<usize>, bool, bool, usize),
    stop_lone: bool,
) -> (Vec<u32>, Option<Stop>) {
    /// Where the walk stopped: at `limit`, or at a string chunk starting at
    /// `.0` of the output and ending with the encoded string `.1`.
    enum At {
        Depth,
        Lone(usize, Vec<u32>),
    }
    fn walk(
        value: &Json,
        options: (Option<usize>, bool, bool, usize),
        stop_lone: bool,
        level: usize,
        chunk: usize,
        out: &mut Vec<u32>,
    ) -> Result<(), At> {
        let (indent, ensure_ascii, sort_keys, limit) = options;
        if matches!(value, Json::Array(_) | Json::Object(_)) && level >= limit {
            return Err(At::Depth);
        }
        let lone = |s: &PyStr, chunk: usize| {
            if stop_lone && matches!(s, PyStr::Lone(_)) {
                let mut text = Vec::new();
                dump_str(s.points(), ensure_ascii, &mut text);
                return Err(At::Lone(chunk, text));
            }
            Ok(())
        };
        let newline = |out: &mut Vec<u32>, level: usize| {
            if let Some(width) = indent {
                out.push(u32::from('\n'));
                out.extend(std::iter::repeat_n(u32::from(' '), width * level));
            }
        };
        let separator = if indent.is_some() { "," } else { ", " };
        match value {
            Json::Str(s) if stop_lone && matches!(s, PyStr::Lone(_)) => lone(s, chunk)?,
            Json::Array(items) if !items.is_empty() => {
                // A list yields its opening, or the separator, with the item
                // after it when that item is a string or number.
                let mut start = out.len();
                out.push(u32::from('['));
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        start = out.len();
                        push(out, separator);
                    }
                    newline(out, level + 1);
                    walk(item, options, stop_lone, level + 1, start, out)?;
                }
                newline(out, level);
                out.push(u32::from(']'));
            }
            Json::Object(pairs) if !pairs.is_empty() => {
                let mut ordered: Vec<&(PyStr, Json)> = pairs.iter().collect();
                if sort_keys {
                    ordered.sort_by_key(|(k, _)| k.code_points());
                }
                out.push(u32::from('{'));
                for (i, (k, v)) in ordered.into_iter().enumerate() {
                    if i > 0 {
                        push(out, separator);
                    }
                    newline(out, level + 1);
                    // A dict yields each key and each value as its own chunk.
                    lone(k, out.len())?;
                    dump_str(k.points(), ensure_ascii, out);
                    push(out, ": ");
                    let start = out.len();
                    walk(v, options, stop_lone, level + 1, start, out)?;
                }
                newline(out, level);
                out.push(u32::from('}'));
            }
            other => dump(other, ensure_ascii, out),
        }
        Ok(())
    }
    let mut out = Vec::new();
    match walk(value, options, stop_lone, 0, 0, &mut out) {
        Ok(()) => (out, None),
        Err(At::Depth) => (out, Some(Stop::Depth)),
        Err(At::Lone(chunk, text)) => {
            let mut rest = out.split_off(chunk);
            rest.extend(text);
            (out, Some(Stop::Unencodable(rest)))
        }
    }
}

/// The bytes `print(text)` writes to a UTF-8 `surrogateescape` stream, or the
/// `UnicodeEncodeError` message for a surrogate that handler cannot map.
pub fn encode_print(points: &[u32]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(points.len());
    let mut i = 0;
    while i < points.len() {
        let cp = points[i];
        if let Some(c) = char::from_u32(cp) {
            let mut buffer = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            i += 1;
            continue;
        }
        // The encoder hands the whole run of surrogates to the error handler.
        let end = (i..points.len())
            .find(|&j| char::from_u32(points[j]).is_some())
            .unwrap_or(points.len());
        if points[i..end]
            .iter()
            .all(|cp| (0xdc80..=0xdcff).contains(cp))
        {
            out.extend(points[i..end].iter().map(|cp| (cp - 0xdc00) as u8));
            i = end;
            continue;
        }
        return Err(if end - i == 1 {
            format!(
                "'utf-8' codec can't encode character '\\u{cp:04x}' in position {i}: surrogates not allowed"
            )
        } else {
            format!(
                "'utf-8' codec can't encode characters in position {i}-{}: surrogates not allowed",
                end - 1
            )
        });
    }
    Ok(out)
}

/// A strict UTF-8 encoding error for code points, as `str.encode()` words it.
pub fn utf8_strict_error(points: &[u32]) -> Option<String> {
    let start = points.iter().position(|&cp| char::from_u32(cp).is_none())?;
    let end = (start..points.len())
        .find(|&j| char::from_u32(points[j]).is_some())
        .unwrap_or(points.len());
    Some(if end - start == 1 {
        format!(
            "'utf-8' codec can't encode character '\\u{:04x}' in position {start}: surrogates not allowed",
            points[start]
        )
    } else {
        format!(
            "'utf-8' codec can't encode characters in position {start}-{}: surrogates not allowed",
            end - 1
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::loads_standard;

    fn j(text: &str) -> Json {
        loads_standard(text).unwrap()
    }

    #[test]
    fn values_render_as_python_does() {
        assert_eq!(
            repr(&j("[1, 2.5, null, true, \"a'b\", {\"k\": []}]")),
            "[1, 2.5, None, True, \"a'b\", {'k': []}]"
        );
        assert_eq!(repr(&j("\"\\ud800x\"")), "'\\ud800x'");
        assert_eq!(py_str(&j("\"x\"")), "x");
        assert_eq!(py_str(&j("1e300")), "1e+300");
    }

    #[test]
    fn equality_and_hashing_follow_python() {
        assert!(py_eq(&j("1"), &j("true")));
        assert!(py_eq(&j("1.0"), &j("1")));
        assert!(py_eq(&j("[1, {\"a\": 2.0}]"), &j("[true, {\"a\": 2}]")));
        assert!(!py_eq(&j("NaN"), &j("NaN")));
        assert!(!py_eq(&j("9007199254740993"), &j("9007199254740992.0")));
        assert_eq!(key(&j("1")), key(&j("1.0")));
        assert_eq!(key(&j("NaN")), key(&j("NaN")));
        assert!(!py_eq(&j("NaN"), &j("NaN")));
        assert!(py_eq(&j("[NaN]"), &j("[NaN]")));
        assert_eq!(key(&j("[]")).unwrap_err(), "unhashable type: 'list'");
    }

    #[test]
    fn printing_follows_utf8_mode_stdout() {
        assert_eq!(
            encode_print(&[0x61, 0xdc80, 0xdcff]).unwrap(),
            vec![0x61, 0x80, 0xff]
        );
        assert_eq!(
            encode_print(&[0x61, 0xd800]).unwrap_err(),
            "'utf-8' codec can't encode character '\\ud800' in position 1: surrogates not allowed"
        );
        assert_eq!(
            String::from_utf8(
                dumps_line(&j("{\"a\": [1, \"\\u00e9\"]}"), false)
                    .iter()
                    .map(|&c| char::from_u32(c).unwrap())
                    .collect::<String>()
                    .into_bytes()
            )
            .unwrap(),
            "{\"a\": [1, \"\u{e9}\"]}"
        );
    }

    #[test]
    fn dumps_until_stops_before_the_first_container_at_the_limit() {
        let value = j(r#"{"k": [1, [2, [3]]], "z": 0}"#);
        let text = |limit| {
            String::from_iter(
                dumps_until(&value, Some(1), true, false, limit)
                    .into_iter()
                    .filter_map(char::from_u32),
            )
        };
        // json.dumps(value, indent=1) cut where the encoder would enter [3].
        assert_eq!(text(3), "{\n \"k\": [\n  1,\n  [\n   2,\n   ");
        assert_eq!(text(0), "");
        assert_eq!(
            text(usize::MAX),
            String::from_iter(
                dumps_with(&value, Some(1), true, false)
                    .into_iter()
                    .filter_map(char::from_u32)
            )
        );
    }
}
