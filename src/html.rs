//! The HTML 5 character references used by CPython's html.unescape.
use regex::Regex;
use std::sync::LazyLock;

mod entities {
    include!("html_entities.rs");
}

pub fn escape(text: &str, quote: bool) -> String {
    let text = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    if quote {
        text.replace('"', "&quot;").replace('\'', "&#x27;")
    } else {
        text
    }
}

/// `name in html.entities.html5`: a name with or without its semicolon.
pub fn is_html5_entity(name: &str) -> bool {
    entities::HTML5
        .binary_search_by_key(&name, |(key, _)| key)
        .is_ok()
}

pub fn unescape(text: &str) -> String {
    static CHARREF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"&(#(?:[0-9]+|[xX][0-9a-fA-F]+);?|[^\t\n\x0c <&#;]{1,32};?)").unwrap()
    });
    CHARREF
        .replace_all(text, |c: &regex::Captures<'_>| {
            let s = &c[1];
            if let Some(number) = s.strip_prefix('#') {
                let number = number.trim_end_matches(';');
                let parsed = if number.starts_with(['x', 'X']) {
                    u32::from_str_radix(&number[1..], 16)
                } else {
                    number.parse()
                };
                let n = parsed.unwrap_or(0x110000);
                if let Some((_, value)) = entities::INVALID.iter().find(|(cp, _)| *cp == n) {
                    return value.to_string();
                }
                if (0xd800..=0xdfff).contains(&n) || n > 0x10ffff {
                    return "\u{fffd}".into();
                }
                if (0xfdd0..=0xfdef).contains(&n)
                    || n & 0xffff >= 0xfffe
                    || matches!(n, 1..=8 | 11 | 14..=31 | 127..=159)
                {
                    return String::new();
                }
                return char::from_u32(n).unwrap().to_string();
            }
            for end in (1..=s.len()).rev().filter(|i| s.is_char_boundary(*i)) {
                if let Ok(i) = entities::HTML5.binary_search_by_key(&&s[..end], |(key, _)| key) {
                    return format!("{}{}", entities::HTML5[i].1, &s[end..]);
                }
            }
            c[0].to_string()
        })
        .into_owned()
}
