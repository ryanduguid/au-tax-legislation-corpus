//! CPython text rules argparse formatting depends on: `str.isspace`, `str.strip`,
//! decimal digit values and `repr()` of a string.

use crate::pyunicode::{
    DECIMAL_ZERO as DECIMAL_ZEROS, NOT_PRINTABLE as PY_NOT_PRINTABLE, SPACE as PY_SPACE,
};

fn in_ranges(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|&(low, high)| {
            if high < cp {
                std::cmp::Ordering::Less
            } else if low > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Rust strings cannot hold the lone surrogates a Python str can (command-line
/// bytes decoded with surrogateescape, or unpaired UTF-16 on Windows). Text
/// decoded from an OS string writes each one as ESCAPE followed by
/// U+10F800 + (surrogate - U+D800), and doubles a genuine ESCAPE, so OS strings
/// map to text one to one. Both are private-use code points at the end of plane 16.
/// shortcut: text that does not come from an OS string (JSON fields) is not
/// escaped. If it holds ESCAPE followed by ESCAPE or by U+10F800..U+10FFFF, that
/// pair prints as an escape; escaping every JSON-derived string would remove it.
const ESCAPE: char = '\u{10F7FF}';
const CARRY_OFFSET: u32 = 0x10_F800 - 0xD800;

/// Append a code point (a Unicode scalar value or a lone surrogate) to OS-derived text.
#[allow(dead_code)]
pub fn push_code_point(out: &mut String, cp: u32) {
    match char::from_u32(cp) {
        Some(ESCAPE) => out.extend([ESCAPE, ESCAPE]),
        Some(ch) => out.push(ch),
        None => out.extend([
            ESCAPE,
            char::from_u32(cp + CARRY_OFFSET).expect("surrogates map to scalar values"),
        ]),
    }
}

/// The code points text stands for, reading the escapes `push_code_point` writes.
pub fn code_points(text: &str) -> impl Iterator<Item = u32> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        let ch = chars.next()?;
        if ch != ESCAPE {
            return Some(ch as u32);
        }
        match chars.peek().copied() {
            Some(ESCAPE) => {
                chars.next();
                Some(ESCAPE as u32)
            }
            Some(next) if (0x10_F800..=0x10_FFFF).contains(&(next as u32)) => {
                chars.next();
                Some(next as u32 - CARRY_OFFSET)
            }
            _ => Some(ESCAPE as u32),
        }
    })
}

/// `len(text)` in Python: code points, with each carried lone surrogate counted once.
pub fn py_len(text: &str) -> usize {
    code_points(text).count()
}

pub fn is_space(cp: u32) -> bool {
    in_ranges(PY_SPACE, cp)
}

/// `str.strip()` with no argument.
pub fn strip(text: &str) -> &str {
    text.trim_matches(|ch: char| is_space(ch as u32))
}

/// The digit value of a Unicode decimal digit (category Nd), as `unicodedata.decimal`.
pub fn decimal_value(cp: u32) -> Option<u32> {
    let index = DECIMAL_ZEROS.partition_point(|&zero| zero <= cp);
    let zero = *DECIMAL_ZEROS.get(index.checked_sub(1)?)?;
    (cp - zero < 10).then_some(cp - zero)
}

/// The gateway's control-character rule: any code point below 32, or 127.
/// `repr()` of a Python string given as code points (lone surrogates allowed).
pub fn repr_code_points(cps: &[u32]) -> String {
    let has_single = cps.contains(&('\'' as u32));
    let has_double = cps.contains(&('"' as u32));
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::with_capacity(cps.len() + 2);
    out.push(quote);
    for &cp in cps {
        match cp {
            0x5C => out.push_str("\\\\"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            _ if cp == quote as u32 => {
                out.push('\\');
                out.push(quote);
            }
            _ if in_ranges(PY_NOT_PRINTABLE, cp) => {
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
            _ => out.push(char::from_u32(cp).expect("printable code points are scalar values")),
        }
    }
    out.push(quote);
    out
}

/// `repr(text)` for text read from a file or built by the program.
pub fn repr(text: &str) -> String {
    let cps: Vec<u32> = text.chars().map(u32::from).collect();
    repr_code_points(&cps)
}

/// `repr(text)` for text decoded from an OS string (`oserror::decode_os`),
/// whose lone surrogates are carried as escape pairs.
pub fn repr_os(text: &str) -> String {
    let cps: Vec<u32> = code_points(text).collect();
    repr_code_points(&cps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_uses_python_whitespace() {
        assert_eq!(strip("\u{1c} a \u{1f}"), "a");
        assert_eq!(strip("\u{a0}a\u{3000}"), "a");
        assert_eq!(strip("\u{200b}a"), "\u{200b}a");
    }

    #[test]
    fn decimal_values_cover_unicode_digits() {
        assert_eq!(decimal_value('7' as u32), Some(7));
        assert_eq!(decimal_value(0x0663), Some(3));
        assert_eq!(decimal_value(0xFF12), Some(2));
        assert_eq!(decimal_value(0x1D7CF), Some(1));
        assert_eq!(decimal_value('a' as u32), None);
        assert_eq!(decimal_value(0x00B2), None);
    }

    #[test]
    fn repr_matches_python() {
        // File text keeps the carrier's code points as themselves; only OS text decodes pairs.
        let pair = "\u{10F7FF}\u{10F800}";
        assert_eq!(repr(pair), r"'\U0010f7ff\U0010f800'");
        assert_eq!(repr_os(pair), r"'\ud800'");
        assert_eq!(repr("200"), "'200'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr("x\u{85}y\u{a0}z\u{200b}"), "'x\\x85y\\xa0z\\u200b'");
        assert_eq!(repr("caf\u{e9}"), "'caf\u{e9}'");
        assert_eq!(repr_code_points(&[0xD800]), "'\\ud800'");
        assert_eq!(repr("\u{e0001}"), "'\\U000e0001'");
    }
}
