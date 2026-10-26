//! The text and timestamp checks the evidence stages share: Python's
//! `str.strip`, `re`'s `\d` and `datetime.date/datetime.fromisoformat` as the
//! monitor, capture and live-evidence exporters apply them.
use crate::py::{self, Date};
use crate::pyjson::Json;

/// Why a value is not stripped, control-free text.
pub enum TextRefusal {
    /// Not a `str`, or only whitespace.
    Empty,
    /// A code point below 32 or 127 after stripping.
    Control,
}

/// `value.strip()` of a non-empty `str` without control characters, as
/// `_non_empty` and `_required_text` check it.
pub fn stripped(value: &Json) -> Result<Vec<u32>, TextRefusal> {
    let Json::Str(s) = value else {
        return Err(TextRefusal::Empty);
    };
    let text = s.code_points();
    let space = |cp: &u32| char::from_u32(*cp).is_some_and(py::is_space);
    let (Some(start), Some(end)) = (
        text.iter().position(|cp| !space(cp)),
        text.iter().rposition(|cp| !space(cp)),
    ) else {
        return Err(TextRefusal::Empty);
    };
    let text = text[start..=end].to_vec();
    if text.iter().any(|&cp| cp < 32 || cp == 127) {
        return Err(TextRefusal::Control);
    }
    Ok(text)
}

/// `re`'s `\d` in a `str` pattern: any Unicode decimal digit.
pub fn decimal(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(py::is_decimal)
}

fn decimals(points: &[u32]) -> bool {
    points.iter().all(|&cp| decimal(cp))
}

/// The ASCII number `points` spell; `fromisoformat` takes no other digits.
fn ascii_number(points: &[u32]) -> Option<u32> {
    points
        .iter()
        .map(|&cp| char::from_u32(cp).filter(char::is_ascii_digit))
        .collect::<Option<String>>()
        .and_then(|s| s.parse().ok())
}

fn at(text: &[u32], index: usize, c: char) -> bool {
    text.get(index) == Some(&u32::from(c))
}

fn ymd(text: &[u32]) -> Option<Date> {
    let (y, m, d) = (
        ascii_number(&text[0..4])?,
        ascii_number(&text[5..7])?,
        ascii_number(&text[8..10])?,
    );
    Date::new(i64::from(y), i64::from(m), i64::from(d)).ok()
}

/// `re.fullmatch(r"\d{4}-\d{2}-\d{2}", text)` and a valid
/// `date.fromisoformat(text)`; for this shape that is also
/// `date.fromisoformat(text).isoformat() == text`.
pub fn iso_date(text: &[u32]) -> bool {
    text.len() == 10
        && at(text, 4, '-')
        && at(text, 7, '-')
        && decimals(&text[0..4])
        && decimals(&text[5..7])
        && decimals(&text[8..10])
        && ymd(text).is_some()
}

/// Which zone designators a timestamp may carry.
#[derive(Clone, Copy, PartialEq)]
pub enum Zone {
    /// A final `Z` only.
    Utc,
    /// `Z`, `+HH:MM`, `-HH:MM` or none.
    Any,
}

/// The date of the instant `text` names, when it matches
/// `\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,MAX})?` and the zone, and
/// `datetime.fromisoformat` accepts it once its fraction is cut or padded to
/// 6 digits (`_instant_for_validation`). `T24:00:00` is midnight of the next
/// day. Offsets follow CPython 3.15, which refuses minutes past 59 that 3.14
/// accepted.
pub fn instant(text: &[u32], max_fraction: usize, zone: Zone) -> Option<Date> {
    let shaped = text.len() >= 19
        && at(text, 4, '-')
        && at(text, 7, '-')
        && at(text, 10, 'T')
        && at(text, 13, ':')
        && at(text, 16, ':')
        && [0..4, 5..7, 8..10, 11..13, 14..16, 17..19]
            .into_iter()
            .all(|r| decimals(&text[r]));
    if !shaped {
        return None;
    }
    let mut i = 19;
    let mut fraction: &[u32] = &[];
    if at(text, i, '.') {
        let start = i + 1;
        let mut end = start;
        while end < text.len() && decimal(text[end]) {
            end += 1;
        }
        if end == start || end - start > max_fraction {
            return None;
        }
        fraction = &text[start..end];
        i = end;
    }
    let rest = &text[i..];
    let offset = match rest {
        [z] if *z == u32::from('Z') => Some((0, 0)),
        [] if zone == Zone::Any => None,
        [sign, h1, h2, colon, m1, m2]
            if zone == Zone::Any
                && (*sign == u32::from('+') || *sign == u32::from('-'))
                && *colon == u32::from(':')
                && decimals(&[*h1, *h2, *m1, *m2]) =>
        {
            Some((ascii_number(&[*h1, *h2])?, ascii_number(&[*m1, *m2])?))
        }
        _ => return None,
    };
    if offset.is_some_and(|(h, m)| h > 23 || m > 59) {
        return None;
    }
    let date = ymd(text)?;
    let (hour, minute, second) = (
        ascii_number(&text[11..13])?,
        ascii_number(&text[14..16])?,
        ascii_number(&text[17..19])?,
    );
    // The fraction is cut to 6 digits before parsing, so only those must be ASCII.
    let kept = &fraction[..fraction.len().min(6)];
    let micro = if kept.is_empty() {
        0
    } else {
        ascii_number(kept)?
    };
    if minute > 59 || second > 59 {
        return None;
    }
    match hour {
        0..=23 => Some(date),
        24 if minute == 0 && second == 0 && micro == 0 => date.add_days(1),
        _ => None,
    }
}
