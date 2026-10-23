//! CPython 3.15's C `datetime.fromisoformat` and `date.fromisoformat`
//! (`Modules/_datetimemodule.c`), with the `ValueError` text each raises.
//!
//! The C parser reads the string's UTF-8 bytes as a NUL-terminated buffer,
//! so a read past the end sees `0`; partial digit runs keep the digits read.
use crate::py::Date;
use crate::pyvalue::repr_points;

/// A parsed `datetime`: its date, clock and UTC offset in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub date: Date,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    pub microsecond: i32,
    pub offset_us: Option<i64>,
}

struct Buf<'a>(&'a [u8]);

impl Buf<'_> {
    fn at(&self, i: usize) -> u8 {
        self.0.get(i).copied().unwrap_or(0)
    }

    /// `parse_digits`: `n` ASCII digits into `var`; None at the first other byte.
    fn digits(&self, p: &mut usize, var: &mut i32, n: usize) -> Option<()> {
        for _ in 0..n {
            let digit = self.at(*p).wrapping_sub(b'0');
            *p += 1;
            if digit > 9 {
                return None;
            }
            *var = *var * 10 + i32::from(digit);
        }
        Some(())
    }
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

fn ymd_to_ord(year: i32, month: i32, day: i32) -> i64 {
    let y = i64::from(year) - 1;
    let before_year = y * 365 + y / 4 - y / 100 + y / 400;
    let before_month: i64 = (1..month).map(|m| i64::from(days_in_month(year, m))).sum();
    before_year + before_month + i64::from(day)
}

/// `ord_to_ymd`, without a range limit.
fn ord_to_ymd(ordinal: i64) -> (i32, i32, i32) {
    let mut year = ((ordinal - 1) * 400 / 146_097 + 1) as i32;
    while ymd_to_ord(year + 1, 1, 1) <= ordinal {
        year += 1;
    }
    while ymd_to_ord(year, 1, 1) > ordinal {
        year -= 1;
    }
    let mut rest = (ordinal - ymd_to_ord(year, 1, 1) + 1) as i32;
    let mut month = 1;
    while rest > days_in_month(year, month) {
        rest -= days_in_month(year, month);
        month += 1;
    }
    (year, month, rest)
}

/// `iso_to_ymd`: 0 on success, else its negative code.
fn iso_to_ymd(iso_year: i32, week: i32, day: i32) -> Result<(i32, i32, i32), i32> {
    if !(1..=9999).contains(&iso_year) {
        return Err(-4);
    }
    if !(1..53).contains(&week) {
        let first_weekday = (ymd_to_ord(iso_year, 1, 1) + 6) % 7;
        let long_year = first_weekday == 3 || (first_weekday == 2 && is_leap(iso_year));
        if week != 53 || !long_year {
            return Err(-2);
        }
    }
    if !(1..8).contains(&day) {
        return Err(-3);
    }
    let first_day = ymd_to_ord(iso_year, 1, 1);
    let first_weekday = (first_day + 6) % 7;
    let mut monday = first_day - first_weekday;
    if first_weekday > 3 {
        monday += 7;
    }
    Ok(ord_to_ymd(monday + i64::from((week - 1) * 7 + day - 1)))
}

/// `parse_isoformat_date(dtstr, len)`: the date, or a negative code.
fn parse_date(b: &Buf, len: usize) -> Result<(i32, i32, i32), i32> {
    let mut p = 0;
    let (mut year, mut month, mut day) = (0, 0, 0);
    b.digits(&mut p, &mut year, 4).ok_or(-1)?;
    let separator = b.at(p) == b'-';
    if separator {
        p += 1;
    }
    if b.at(p) == b'W' {
        p += 1;
        let (mut week, mut weekday) = (0, 0);
        b.digits(&mut p, &mut week, 2).ok_or(-3)?;
        if p < len {
            if separator {
                p += 1;
                if b.at(p - 1) != b'-' {
                    return Err(-2);
                }
            }
            b.digits(&mut p, &mut weekday, 1).ok_or(-4)?;
        } else {
            weekday = 1;
        }
        return iso_to_ymd(year, week, weekday).map_err(|rv| rv - 3);
    }
    b.digits(&mut p, &mut month, 2).ok_or(-1)?;
    if separator {
        p += 1;
        if b.at(p - 1) != b'-' {
            return Err(-2);
        }
    }
    b.digits(&mut p, &mut day, 2).ok_or(-1)?;
    Ok((year, month, day))
}

/// `parse_hh_mm_ss_ff(tstr, tstr_end)`: the return code and the fields,
/// which keep what was read before a failure.
fn parse_clock(b: &Buf, start: usize, end: usize) -> (i32, [i32; 4]) {
    let mut v = [0; 4];
    let mut p = start;
    let mut has_separator = true;
    for i in 0..3 {
        if b.digits(&mut p, &mut v[i], 2).is_none() {
            return (-3, v);
        }
        let c = b.at(p);
        p += 1;
        if i == 0 {
            has_separator = c == b':';
        }
        if c == b'.' || c == b',' {
            if i < 2 || p >= end {
                return (-3, v);
            }
            break;
        } else if p >= end {
            return (i32::from(c != 0), v);
        } else if has_separator && c == b':' {
            if i == 2 {
                return (-4, v);
            }
            continue;
        } else if !has_separator {
            p -= 1;
        } else {
            return (-4, v);
        }
    }
    let to_parse = (end - p).min(6);
    if b.digits(&mut p, &mut v[3], to_parse).is_none() {
        return (-3, v);
    }
    const CORRECTION: [i32; 5] = [100_000, 10_000, 1_000, 100, 10];
    if to_parse < 6 {
        v[3] *= CORRECTION[to_parse - 1];
    }
    while b.at(p).is_ascii_digit() {
        p += 1;
    }
    (i32::from(b.at(p) != 0), v)
}

/// `check_time_args`.
fn check_time(h: i32, m: i32, s: i32, us: i32) -> Result<(), String> {
    if !(0..=23).contains(&h) {
        return Err(format!("hour must be in 0..23, not {h}"));
    }
    if !(0..=59).contains(&m) {
        return Err(format!("minute must be in 0..59, not {m}"));
    }
    if !(0..=59).contains(&s) {
        return Err(format!("second must be in 0..59, not {s}"));
    }
    if !(0..=999_999).contains(&us) {
        return Err(format!("microsecond must be in 0..999999, not {us}"));
    }
    Ok(())
}

enum Time {
    /// `parse_isoformat_time`'s negative code.
    Invalid,
    /// Code -6: the offset's own `ValueError`.
    Offset(String),
    Parsed([i32; 4], Option<i64>),
}

/// `parse_isoformat_time` over the bytes after the separator.
fn parse_time(t: &Buf) -> Time {
    let len = t.0.len();
    let mut tz = 0;
    loop {
        if matches!(t.at(tz), b'Z' | b'+' | b'-') {
            break;
        }
        tz += 1;
        if tz >= len {
            break;
        }
    }
    let (rv, clock) = parse_clock(t, 0, tz);
    if rv < 0 {
        return Time::Invalid;
    }
    if tz == len {
        return if rv == 1 {
            Time::Invalid
        } else {
            Time::Parsed(clock, None)
        };
    }
    if t.at(tz) == b'Z' {
        return if t.at(tz + 1) != 0 {
            Time::Invalid
        } else {
            Time::Parsed(clock, Some(0))
        };
    }
    let sign = if t.at(tz) == b'-' { -1 } else { 1 };
    let (rv, [h, m, s, us]) = parse_clock(t, tz + 1, len);
    if let Err(message) = check_time(h, m, s, us) {
        return Time::Offset(message);
    }
    if rv != 0 {
        return Time::Invalid;
    }
    let seconds = i64::from(h * 3600 + m * 60 + s);
    Time::Parsed(clock, Some(sign * (seconds * 1_000_000 + i64::from(us))))
}

/// `_find_isoformat_datetime_separator`, or None for its -1.
fn find_separator(b: &Buf, len: usize) -> Option<usize> {
    if len == 7 {
        return Some(7);
    }
    if b.at(4) == b'-' {
        if b.at(5) != b'W' {
            return Some(10);
        }
        if len > 8 && b.at(8) == b'-' {
            if len == 9 {
                return None;
            }
            return Some(if len > 10 && b.at(10).is_ascii_digit() {
                8
            } else {
                10
            });
        }
        return Some(8);
    }
    if b.at(4) != b'W' {
        return Some(8);
    }
    let mut idx = 7;
    while idx < len && b.at(idx).is_ascii_digit() {
        idx += 1;
    }
    Some(if idx < 9 {
        idx
    } else if idx % 2 == 0 {
        7
    } else {
        8
    })
}

fn is_surrogate(cp: u32) -> bool {
    (0xd800..0xe000).contains(&cp)
}

/// `datetime.fromisoformat(string)`; the error is the `ValueError` message.
pub fn datetime_fromisoformat(string: &[u32]) -> Result<DateTime, String> {
    let invalid = || format!("Invalid isoformat string: {}", repr_points(string));
    if string.len() < 7 {
        return Err(invalid());
    }
    // `_sanitize_isoformat_str`: a surrogate separator reads as `T`.
    let mut points = string.to_vec();
    for pos in [7, 8, 10] {
        if pos > points.len() {
            break;
        }
        if points.get(pos).is_some_and(|&cp| is_surrogate(cp)) {
            points[pos] = u32::from('T');
            break;
        }
    }
    let Some(text) = points
        .iter()
        .map(|&cp| char::from_u32(cp))
        .collect::<Option<String>>()
    else {
        return Err(invalid());
    };
    let b = Buf(text.as_bytes());
    let len = b.0.len();
    let separator = find_separator(&b, len).ok_or_else(invalid)?;
    let (mut year, mut month, mut day) = parse_date(&b, separator).map_err(|_| invalid())?;
    let mut clock = [0; 4];
    let mut offset_us = None;
    if len > separator {
        let lead = b.at(separator);
        let width = if lead & 0x80 == 0 {
            1
        } else {
            match lead & 0xf0 {
                0xe0 => 3,
                0xf0 => 4,
                _ => 2,
            }
        };
        match parse_time(&Buf(&b.0[separator + width..])) {
            Time::Invalid => return Err(invalid()),
            Time::Offset(message) => return Err(message),
            Time::Parsed(parsed, offset) => {
                clock = parsed;
                offset_us = offset;
            }
        }
    }
    let [mut hour, minute, second, microsecond] = clock;
    if hour == 24 && (1..=12).contains(&month) {
        let dim = days_in_month(year, month);
        if day <= dim {
            if minute != 0 || second != 0 || microsecond != 0 {
                return Err("minute, second, and microsecond must be 0 when hour is 24".into());
            }
            hour = 0;
            day += 1;
            if day > dim {
                day = 1;
                month += 1;
                if month > 12 {
                    month = 1;
                    year += 1;
                }
            }
        }
    }
    let date = Date::new(year.into(), month.into(), day.into())?;
    check_time(hour, minute, second, microsecond)?;
    Ok(DateTime {
        date,
        hour,
        minute,
        second,
        microsecond,
        offset_us,
    })
}

/// `date.fromisoformat(string)` for a string without surrogates.
pub fn date_fromisoformat(string: &str) -> Result<Date, String> {
    let b = Buf(string.as_bytes());
    let parsed = match b.0.len() {
        7 | 8 | 10 => parse_date(&b, b.0.len()).ok(),
        _ => None,
    };
    let (year, month, day) = parsed
        .ok_or_else(|| format!("Invalid isoformat string: {}", crate::py::repr_str(string)))?;
    Date::new(year.into(), month.into(), day.into())
}

impl DateTime {
    /// `self.astimezone(timezone.utc).date()` for an aware datetime; the
    /// error is the `OverflowError` message.
    pub fn utc_date(&self) -> Result<Date, String> {
        let offset = self.offset_us.unwrap_or(0);
        let micros =
            (i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second))
                * 1_000_000
                + i64::from(self.microsecond)
                - offset;
        let days = micros.div_euclid(86_400 * 1_000_000);
        self.date
            .add_days(days)
            .ok_or_else(|| "date value out of range".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<DateTime, String> {
        datetime_fromisoformat(&text.chars().map(u32::from).collect::<Vec<_>>())
    }

    #[test]
    fn download_timestamps_parse_with_their_offset() {
        let dt = parse("2026-10-24T01:02:03.456789+00:00").unwrap();
        assert_eq!(dt.offset_us, Some(0));
        assert_eq!(dt.utc_date().unwrap().isoformat(), "2026-10-24");
        let dt = parse("2026-10-24T01:02:03+10:00").unwrap();
        assert_eq!(dt.utc_date().unwrap().isoformat(), "2026-10-23");
        assert_eq!(parse("2026-10-24").unwrap().offset_us, None);
    }

    #[test]
    fn errors_match_cpython() {
        assert_eq!(
            parse("2026-10-2x").unwrap_err(),
            "Invalid isoformat string: '2026-10-2x'"
        );
        assert_eq!(
            parse("2026-10-24T10:00+24:00").unwrap_err(),
            "hour must be in 0..23, not 24"
        );
        assert_eq!(
            parse("2026-02-30T00:00").unwrap_err(),
            "day 30 must be in range 1..28 for month 2 in year 2026"
        );
        assert_eq!(
            parse("2026-10-24T24:00:01").unwrap_err(),
            "minute, second, and microsecond must be 0 when hour is 24"
        );
        assert_eq!(
            parse("9999-12-31T24:00").unwrap_err(),
            "year must be in 1..9999, not 10000"
        );
        assert_eq!(
            parse("0001-01-01T00:00+01:00")
                .unwrap()
                .utc_date()
                .unwrap_err(),
            "date value out of range"
        );
        assert_eq!(parse("2026-W01-1").unwrap().date.isoformat(), "2025-12-29");
        assert_eq!(
            date_fromisoformat("20261024").unwrap().isoformat(),
            "2026-10-24"
        );
    }
}
