//! The Python behaviours the radar's output depended on, adapted from the
//! builder-operator-loop port.
//!
//! The radar was a Python program, and its messages quote Python's own
//! renderings: `repr` of values, `str` of `OSError`s, decimal digits and `str`
//! predicates such as `isspace`. Reproducing them here keeps every status line
//! byte-for-byte what a reviewer saw.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use num_bigint::BigInt;

use crate::pyunicode as tables;

fn in_table(cp: u32, table: &[(u32, u32)]) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    in_table(c as u32, tables::SPACE)
}

/// `str.isprintable` for one character.
pub fn is_printable(c: char) -> bool {
    !in_table(c as u32, tables::NOT_PRINTABLE)
}

/// `str.isdecimal` for one character: what `\d` matches in a `str` pattern.
pub fn is_decimal(c: char) -> bool {
    in_table(c as u32, tables::DECIMAL)
}

/// The value `int()` gives a decimal digit, ASCII or not.
pub fn decimal_value(c: char) -> Option<u32> {
    if !is_decimal(c) {
        return None;
    }
    let cp = c as u32;
    let index = tables::DECIMAL_ZERO.partition_point(|&zero| zero <= cp);
    let zero = tables::DECIMAL_ZERO[index.checked_sub(1)?];
    (cp - zero < 10).then_some(cp - zero)
}

/// Python's `\w` for one character in a `str` pattern.
pub fn is_word(c: char) -> bool {
    in_table(c as u32, tables::WORD)
}

/// Whether Python's `\w` and the regex crate's `\w` disagree about one character.
pub fn word_diff(c: char) -> bool {
    (c as u32) >= 0x80 && in_table(c as u32, tables::WORD_DIFF)
}

/// The ranges of a character class, without its brackets, for a `regex` pattern.
pub fn class_body(table: &[(u32, u32)]) -> String {
    let mut out = String::new();
    for &(lo, hi) in table {
        if lo == hi {
            let _ = write!(out, "\\x{{{lo:X}}}");
        } else {
            let _ = write!(out, "\\x{{{lo:X}}}-\\x{{{hi:X}}}");
        }
    }
    out
}

fn class(table: &[(u32, u32)]) -> String {
    format!("[{}]", class_body(table))
}

/// Python's `\d` for a `str` pattern.
pub static D: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| class(tables::DECIMAL));
/// Python's `\s` for a `str` pattern.
pub static S: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| class(tables::SPACE));
/// Python's `\w` for a `str` pattern.
pub static W: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| class(tables::WORD));

/// Python's `\b`, for a `fancy_regex` pattern.
pub fn word_boundary() -> String {
    let w = &*W;
    format!("(?:(?<={w})(?!{w})|(?<!{w})(?={w}))")
}

/// What one ASCII letter matches in a Python pattern compiled with `re.IGNORECASE`.
pub fn ignorecase_letter(letter: char) -> String {
    let mut out = format!(
        "[{}{}",
        letter.to_ascii_lowercase(),
        letter.to_ascii_uppercase()
    );
    for (base, extra) in tables::IGNORECASE_EXTRA {
        if *base == letter.to_ascii_lowercase() {
            for cp in *extra {
                let _ = write!(out, "\\x{{{cp:X}}}");
            }
        }
    }
    out.push(']');
    out
}

/// `str.casefold()`.
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match tables::CASEFOLD.binary_search_by_key(&(c as u32), |&(k, _)| k) {
            Ok(i) => out.push_str(tables::CASEFOLD[i].1),
            Err(_) => out.push(c),
        }
    }
    out
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()` with no separator.
pub fn split_whitespace(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|part| !part.is_empty()).collect()
}

/// `str.splitlines()`.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if boundary {
            out.push(&s[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r'
                && let Some(&(j, '\n')) = chars.peek()
            {
                chars.next();
                next = j + 1;
            }
            start = next;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The first `n` characters, as `s[:n]` takes them.
pub fn prefix_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `len(s)`: a count of code points.
pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// `repr(s)` for a `str`.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            _ if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            _ if (c as u32) < 0x7f => out.push(c),
            _ if is_printable(c) => out.push(c),
            _ if (c as u32) <= 0xff => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            _ if (c as u32) <= 0xffff => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            _ => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push(quote);
    out
}

/// `repr(b)` for `bytes`.
pub fn repr_bytes(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::from("b");
    out.push(quote as char);
    for &byte in b {
        match byte {
            _ if byte == quote || byte == b'\\' => {
                out.push('\\');
                out.push(byte as char);
            }
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(byte as char),
            _ => {
                let _ = write!(out, "\\x{byte:02x}");
            }
        }
    }
    out.push(quote as char);
    out
}

/// `repr(f)` for a `float`.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("scientific notation");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let decpt: i64 = exponent.parse::<i64>().expect("exponent") + 1;
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let n = digits.len() as i64;
    let body = if f == 0.0 {
        "0.0".to_string()
    } else if decpt <= -4 || decpt > 16 {
        let mut m = digits[..1].to_string();
        if n > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    } else if decpt <= 0 {
        format!("0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt >= n {
        format!("{digits}{}.0", "0".repeat((decpt - n) as usize))
    } else {
        format!(
            "{}.{}",
            &digits[..decpt as usize],
            &digits[decpt as usize..]
        )
    };
    format!("{sign}{body}")
}

/// Python refuses to render an `int` of more than this many decimal digits.
pub const INT_MAX_STR_DIGITS: usize = 4300;

/// The `ValueError` Python raises for an over-long decimal `int`.
pub fn int_digits_error(digits: usize) -> String {
    format!(
        "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has \
         {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
    )
}

/// `repr(i)` for an `int`, or the `ValueError` an over-long one raises.
pub fn int_repr(i: &BigInt) -> Result<String, String> {
    let text = i.to_string();
    let digits = text.trim_start_matches('-').len();
    if digits > INT_MAX_STR_DIGITS {
        // Python measures the limit before rendering; the count it reports is
        // its own estimate, so only the refusal is reproduced exactly.
        return Err(
            "Exceeds the limit (4300 digits) for integer string conversion; use \
                    sys.set_int_max_str_digits() to increase the limit"
                .into(),
        );
    }
    Ok(text)
}

/// Unicode digits to ASCII and Unicode spaces to ' ', as `int()` and `float()` first do.
fn number_text(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_space(c) {
            out.push(' ');
        } else if let Some(d) = decimal_value(c) {
            out.push(char::from_digit(d, 10).expect("digit"));
        } else if c.is_ascii() {
            out.push(c);
        } else {
            return None;
        }
    }
    Some(out)
}

/// `int(s, base)`; the error is the `ValueError` message.
pub fn parse_int(s: &str, base: u32) -> Result<BigInt, String> {
    let invalid = || {
        let shown: String = if char_len(s) > 200 {
            prefix_chars(s, 200).to_string()
        } else {
            s.to_string()
        };
        format!(
            "invalid literal for int() with base {base}: {}",
            repr_str(&shown)
        )
    };
    let text = number_text(s).ok_or_else(invalid)?;
    let text =
        text.trim_matches(|c: char| c.is_ascii_whitespace() || ('\x1c'..='\x1f').contains(&c));
    let (negative, mut body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let prefix = match base {
        16 => Some(["0x", "0X"]),
        8 => Some(["0o", "0O"]),
        2 => Some(["0b", "0B"]),
        _ => None,
    };
    let mut after_prefix = false;
    if let Some(prefixes) = prefix {
        for p in prefixes {
            if let Some(rest) = body.strip_prefix(p) {
                body = rest;
                after_prefix = true;
                break;
            }
        }
    }
    // An underscore may follow a digit, or a base prefix, and must precede a digit.
    let mut digits = String::new();
    let mut previous_digit = false;
    let mut previous_underscore = false;
    for (i, c) in body.chars().enumerate() {
        if c == '_' {
            if previous_underscore || !(previous_digit || i == 0 && after_prefix) {
                return Err(invalid());
            }
            previous_underscore = true;
            previous_digit = false;
            continue;
        }
        if c.to_digit(base).is_none() {
            return Err(invalid());
        }
        digits.push(c);
        previous_digit = true;
        previous_underscore = false;
    }
    if digits.is_empty() || previous_underscore {
        return Err(invalid());
    }
    if base == 10 && digits.len() > INT_MAX_STR_DIGITS {
        return Err(int_digits_error(digits.len()));
    }
    let value = BigInt::parse_bytes(digits.as_bytes(), base).ok_or_else(invalid)?;
    Ok(if negative { -value } else { value })
}

/// `float(s)`; the error is the `ValueError` message.
pub fn parse_float(s: &str) -> Result<f64, String> {
    let invalid = || format!("could not convert string to float: {}", repr_str(s));
    let text = number_text(s).ok_or_else(invalid)?;
    let text =
        text.trim_matches(|c: char| c.is_ascii_whitespace() || ('\x1c'..='\x1f').contains(&c));
    let (sign, body) = match text.as_bytes().first() {
        Some(b'-') => ("-", &text[1..]),
        Some(b'+') => ("", &text[1..]),
        _ => ("", text),
    };
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return format!("{sign}{lower}")
            .parse::<f64>()
            .map_err(|_| invalid());
    }
    // Digits with single underscores between them, an optional fraction and
    // an optional exponent: the grammar float() accepts.
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut clean = String::from(sign);
    let digit_run = |i: &mut usize, clean: &mut String| -> bool {
        let start = *i;
        while *i < bytes.len() {
            let b = bytes[*i];
            if b.is_ascii_digit() {
                clean.push(b as char);
                *i += 1;
            } else if b == b'_'
                && *i > start
                && bytes[*i - 1].is_ascii_digit()
                && *i + 1 < bytes.len()
                && bytes[*i + 1].is_ascii_digit()
            {
                *i += 1;
            } else {
                break;
            }
        }
        *i > start
    };
    let whole = digit_run(&mut i, &mut clean);
    let mut fraction = false;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        clean.push('.');
        fraction = digit_run(&mut i, &mut clean);
        if !fraction {
            clean.push('0');
        }
    }
    if !whole && !fraction {
        return Err(invalid());
    }
    if !whole {
        clean.insert(sign.len(), '0');
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        clean.push('e');
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            clean.push(bytes[i] as char);
            i += 1;
        }
        if !digit_run(&mut i, &mut clean) {
            return Err(invalid());
        }
    }
    if i != bytes.len() {
        return Err(invalid());
    }
    clean.parse::<f64>().map_err(|_| invalid())
}

/// A UTF-8 decoding failure, rendered as Python's `UnicodeDecodeError`.
#[derive(Debug, Clone)]
pub struct DecodeError {
    pub start: usize,
    pub end: usize,
    pub byte: u8,
    pub reason: &'static str,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.end == self.start + 1 {
            write!(
                f,
                "'utf-8' codec can't decode byte 0x{:02x} in position {}: {}",
                self.byte, self.start, self.reason
            )
        } else {
            write!(
                f,
                "'utf-8' codec can't decode bytes in position {}-{}: {}",
                self.start,
                self.end - 1,
                self.reason
            )
        }
    }
}

/// Decode UTF-8 strictly, failing as `bytes.decode("utf-8")` does.
pub fn decode_utf8(bytes: &[u8]) -> Result<String, DecodeError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(e) => {
            let start = e.valid_up_to();
            let byte = bytes[start];
            let (end, reason) = match e.error_len() {
                None => (bytes.len(), "unexpected end of data"),
                Some(n) if (0xc2..=0xf4).contains(&byte) => {
                    (start + n, "invalid continuation byte")
                }
                Some(n) => (start + n, "invalid start byte"),
            };
            Err(DecodeError {
                start,
                end,
                byte,
                reason,
            })
        }
    }
}

/// Text mode's universal newlines: `\r\n` and `\r` both read as `\n`.
pub fn universal_newlines(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

/// Why a text read failed.
#[derive(Debug)]
pub enum ReadError {
    Os(io::Error),
    Decode(DecodeError),
}

/// `Path.read_text(encoding="utf-8")`.
pub fn read_text(path: &Path) -> Result<String, ReadError> {
    let bytes = std::fs::read(path).map_err(ReadError::Os)?;
    let text = decode_utf8(&bytes).map_err(ReadError::Decode)?;
    Ok(universal_newlines(&text))
}

/// How the platform reported an `OSError`, which decides how Python renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsStyle {
    /// `open()` and friends: the C runtime's errno, on every platform.
    Errno,
    /// Directory and metadata calls, which on Windows report a Win32 error.
    Native,
}

fn strip_os_suffix(message: &str) -> &str {
    match message.rfind(" (os error ") {
        Some(i) => &message[..i],
        None => message,
    }
}

/// The C runtime's `_dosmaperr` table and its `strerror` texts.
#[cfg(windows)]
fn crt_errno(winerror: i32) -> (i32, &'static str) {
    match winerror {
        2 | 3 | 15 | 18 | 53 | 67 | 161 | 206 => (2, "No such file or directory"),
        4 => (24, "Too many open files"),
        5 | 16 | 19..=36 | 65 | 82 | 83 | 108 | 132 | 158 | 167 => (13, "Permission denied"),
        6 | 114 | 130 => (9, "Bad file descriptor"),
        7..=9 | 1816 => (12, "Not enough space"),
        10 => (7, "Arg list too long"),
        11 | 188..=202 => (8, "Exec format error"),
        17 => (18, "Improper link"),
        80 | 183 => (17, "File exists"),
        89 | 164 | 215 => (11, "Resource temporarily unavailable"),
        109 => (32, "Broken pipe"),
        112 => (28, "No space left on device"),
        128 | 129 => (10, "No child processes"),
        145 => (41, "Directory not empty"),
        _ => (22, "Invalid argument"),
    }
}

/// `str(OSError)` for an operation on `filename` (already rendered as `str(path)`).
pub fn oserror_str(error: &io::Error, filename: &str, style: OsStyle) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    let message = error.to_string();
    #[cfg(windows)]
    {
        if style == OsStyle::Errno {
            let (errno, text) = crt_errno(code);
            return format!("[Errno {errno}] {text}: {}", repr_str(filename));
        }
        let text = strip_os_suffix(&message).trim_end_matches(|c: char| c == '.' || c <= ' ');
        format!("[WinError {code}] {text}: {}", repr_str(filename))
    }
    #[cfg(not(windows))]
    {
        let _ = style;
        format!(
            "[Errno {code}] {}: {}",
            strip_os_suffix(&message),
            repr_str(filename)
        )
    }
}

/// A `datetime.date`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

pub const MAX_DATE: Date = Date {
    year: 9999,
    month: 12,
    day: 31,
};

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

fn days_before_year(year: i32) -> i64 {
    let y = i64::from(year) - 1;
    y * 365 + y / 4 - y / 100 + y / 400
}

fn days_before_month(year: i32, month: u32) -> i64 {
    (1..month).map(|m| i64::from(days_in_month(year, m))).sum()
}

impl Date {
    /// `datetime.date(year, month, day)`; the error is the `ValueError` message.
    pub fn new(year: i64, month: i64, day: i64) -> Result<Date, String> {
        if !(1..=9999).contains(&year) {
            return Err(format!("year must be in 1..9999, not {year}"));
        }
        if !(1..=12).contains(&month) {
            return Err("month must be in 1..12".into());
        }
        let dim = i64::from(days_in_month(year as i32, month as u32));
        if !(1..=dim).contains(&day) {
            return Err(format!(
                "day {day} must be in range 1..{dim} for month {month} in year {year}"
            ));
        }
        Ok(Date {
            year: year as i32,
            month: month as u32,
            day: day as u32,
        })
    }

    /// The proleptic Gregorian ordinal, 1 for 0001-01-01.
    pub fn ordinal(&self) -> i64 {
        days_before_year(self.year) + days_before_month(self.year, self.month) + i64::from(self.day)
    }

    pub fn from_ordinal(ordinal: i64) -> Option<Date> {
        if !(1..=MAX_DATE.ordinal()).contains(&ordinal) {
            return None;
        }
        // Estimate the year, then correct it.
        let mut year = ((ordinal - 1) * 400 / 146_097 + 1) as i32;
        while days_before_year(year + 1) < ordinal {
            year += 1;
        }
        while days_before_year(year) >= ordinal {
            year -= 1;
        }
        let mut rest = ordinal - days_before_year(year);
        let mut month = 1;
        while rest > i64::from(days_in_month(year, month)) {
            rest -= i64::from(days_in_month(year, month));
            month += 1;
        }
        Some(Date {
            year,
            month,
            day: rest as u32,
        })
    }

    /// `date + timedelta(days=n)`, or None where Python raises OverflowError.
    pub fn add_days(&self, days: i64) -> Option<Date> {
        Date::from_ordinal(self.ordinal() + days)
    }

    /// `date.isoformat()`.
    pub fn isoformat(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `date.fromisoformat(s)`, following CPython's C implementation.
    pub fn fromisoformat(s: &str) -> Option<Date> {
        let b = s.as_bytes();
        if !matches!(b.len(), 7 | 8 | 10) {
            return None;
        }
        // The C parser reads a NUL-terminated buffer.
        let at = |i: usize| b.get(i).copied().unwrap_or(0);
        let digits = |p: &mut usize, n: usize| -> Option<i64> {
            let mut value = 0i64;
            for _ in 0..n {
                let c = at(*p);
                if !c.is_ascii_digit() {
                    return None;
                }
                value = value * 10 + i64::from(c - b'0');
                *p += 1;
            }
            Some(value)
        };
        let mut p = 0;
        let year = digits(&mut p, 4)?;
        let separator = at(p) == b'-';
        if separator {
            p += 1;
        }
        if at(p) == b'W' {
            p += 1;
            let week = digits(&mut p, 2)?;
            let weekday = if p < b.len() {
                if separator {
                    if at(p) != b'-' {
                        return None;
                    }
                    p += 1;
                }
                digits(&mut p, 1)?
            } else {
                1
            };
            return Date::from_iso_week(year, week, weekday);
        }
        let month = digits(&mut p, 2)?;
        if separator {
            if at(p) != b'-' {
                return None;
            }
            p += 1;
        }
        let day = digits(&mut p, 2)?;
        Date::new(year, month, day).ok()
    }

    fn from_iso_week(year: i64, week: i64, weekday: i64) -> Option<Date> {
        if !(1..=9999).contains(&year) {
            return None;
        }
        let year = year as i32;
        let first = Date {
            year,
            month: 1,
            day: 1,
        }
        .ordinal();
        let first_weekday = (first + 6) % 7;
        if week <= 0 || week >= 53 {
            let long_year = first_weekday == 3 || first_weekday == 2 && is_leap(year);
            if !(week == 53 && long_year) {
                return None;
            }
        }
        if !(1..=7).contains(&weekday) {
            return None;
        }
        let mut monday = first - first_weekday;
        if first_weekday > 3 {
            monday += 7;
        }
        Date::from_ordinal(monday + (week - 1) * 7 + weekday - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr_str("a'b"), "\"a'b\"");
        assert_eq!(repr_str("a'\"b"), "'a\\'\"b'");
        assert_eq!(
            repr_str("\t\u{0}\u{7f}é\u{a0}\u{2028}\u{1F600}"),
            "'\\t\\x00\\x7fé\\xa0\\u2028\u{1F600}'"
        );
        assert_eq!(repr_bytes(b"a'\x00\xff"), "b\"a'\\x00\\xff\"");
    }

    #[test]
    fn float_repr_matches_python() {
        for (value, expected) in [
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (-0.0, "-0.0"),
            (1.5e300, "1.5e+300"),
            (123456789.123, "123456789.123"),
            (5e-324, "5e-324"),
            (1.0, "1.0"),
        ] {
            assert_eq!(float_repr(value), expected);
        }
    }

    #[test]
    fn fromisoformat_follows_cpython() {
        let ok = |s: &str| Date::fromisoformat(s).map(|d| d.isoformat());
        assert_eq!(ok("20261017ab").as_deref(), Some("2026-10-17"));
        assert_eq!(ok("2026W011xx").as_deref(), Some("2025-12-29"));
        assert_eq!(ok("2026-W01").as_deref(), Some("2025-12-29"));
        assert_eq!(ok("20261017\u{e9}").as_deref(), Some("2026-10-17"));
        assert_eq!(ok("2026-W53").as_deref(), Some("2026-12-28"));
        assert_eq!(ok("2020-W53-7").as_deref(), Some("2021-01-03"));
        for bad in [
            "2026-1017",
            "202610-17",
            "2026-10-1x",
            "0000-01-01",
            "9999-W52-6",
            "+202-10-17",
            " 2026-1-1",
            "2026-W01-1x",
            "2026W01-1",
            "2026-02-30",
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
    }

    #[test]
    fn decode_errors_match_python() {
        let e = decode_utf8(b"ab\xff").unwrap_err();
        assert_eq!(
            e.to_string(),
            "'utf-8' codec can't decode byte 0xff in position 2: invalid start byte"
        );
        let e = decode_utf8(b"\xe2\x82(").unwrap_err();
        assert_eq!(
            e.to_string(),
            "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"
        );
        let e = decode_utf8(b"a\xe2\x82").unwrap_err();
        assert_eq!(
            e.to_string(),
            "'utf-8' codec can't decode bytes in position 1-2: unexpected end of data"
        );
    }

    #[test]
    fn string_predicates_match_python() {
        assert_eq!(splitlines("a\r\nb\rc\x1cd\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(strip("\x1c x \u{3000}"), "x");
        assert_eq!(decimal_value('\u{0663}'), Some(3));
        assert_eq!(parse_int(" 1\u{0661}", 10).unwrap(), BigInt::from(11));
        assert_eq!(parse_int("0b101", 2).unwrap(), BigInt::from(5));
        assert!(parse_int("1__0", 10).is_err());
        assert_eq!(parse_float("1.e+5").unwrap(), 100000.0);
        assert_eq!(parse_float("1_0.5").unwrap(), 10.5);
        assert!(parse_float("1._5").is_err());
    }
}
