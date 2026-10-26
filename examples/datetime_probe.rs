//! Differential probe for `pydatetime` against CPython's `datetime`.
//!
//! Each stdin line is a string as comma-separated hex code points, or `d:`
//! and the same for `date.fromisoformat`. Each stdout line is the parsed
//! value, `E<message>` for the ValueError, or the UTC date and `O<message>`
//! for the OverflowError of `astimezone(timezone.utc).date()`.
use std::io::{BufRead, Write};
use tax_radar_au::pydatetime::{date_fromisoformat, datetime_fromisoformat};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line.expect("stdin line");
        let (date_only, hex) = match line.strip_prefix("d:") {
            Some(rest) => (true, rest),
            None => (false, line.as_str()),
        };
        let points: Vec<u32> = hex
            .split(',')
            .filter(|h| !h.is_empty())
            .map(|h| u32::from_str_radix(h, 16).expect("hex"))
            .collect();
        let answer = if date_only {
            let text: String = points.iter().filter_map(|&cp| char::from_u32(cp)).collect();
            match date_fromisoformat(&text) {
                Ok(date) => date.isoformat(),
                Err(message) => format!("E{message}"),
            }
        } else {
            match datetime_fromisoformat(&points) {
                Err(message) => format!("E{message}"),
                Ok(dt) => {
                    let utc = match dt.offset_us {
                        None => String::from("-"),
                        Some(_) => match dt.utc_date() {
                            Ok(date) => date.isoformat(),
                            Err(message) => format!("O{message}"),
                        },
                    };
                    format!(
                        "{} {} {} {} {} {:?} {utc}",
                        dt.date.isoformat(),
                        dt.hour,
                        dt.minute,
                        dt.second,
                        dt.microsecond,
                        dt.offset_us
                    )
                }
            }
        };
        writeln!(stdout, "{answer}").expect("stdout");
    }
}
