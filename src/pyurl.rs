//! `urllib.parse.urlsplit` and `urljoin` of CPython 3.15 for `str` URLs, with `.hostname`.
//!
//! The checks use the scheme, netloc, path and hostname; the `ValueError`s
//! urlsplit raises for malformed brackets or NFKC tricks are returned as
//! errors, which the callers report as Python reported the traceback.
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

fn c0_or_space(c: char) -> bool {
    (c as u32) <= 0x20
}

/// `_urlsplit`: components absent from the URL are `None`, as CPython 3.15 keeps them.
struct Parts {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

fn split_parts(url: &str) -> Result<Parts, String> {
    let mut url: String = url
        .trim_start_matches(c0_or_space)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = None;
    if let Some(i) = url.find(':')
        && i > 0
        && url.as_bytes()[0].is_ascii_alphabetic()
        && url[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        scheme = Some(url[..i].to_ascii_lowercase());
        url = url[i + 1..].to_owned();
    }
    let mut netloc = None;
    if url.starts_with("//") {
        let rest = &url[2..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let found = rest[..end].to_owned();
        url = rest[end..].to_owned();
        if found.contains('[') != found.contains(']') {
            return Err("Invalid IPv6 URL".into());
        }
        if found.contains('[') && found.contains(']') {
            check_bracketed_netloc(&found)?;
        }
        netloc = Some(found);
    }
    let mut fragment = None;
    if let Some((before, after)) = url.split_once('#') {
        fragment = Some(after.to_owned());
        url = before.to_owned();
    }
    let mut query = None;
    if let Some((before, after)) = url.split_once('?') {
        query = Some(after.to_owned());
        url = before.to_owned();
    }
    check_netloc(netloc.as_deref().unwrap_or(""))?;
    Ok(Parts {
        scheme,
        netloc,
        path: url,
        query,
        fragment,
    })
}

pub fn urlsplit(url: &str) -> Result<Split, String> {
    let parts = split_parts(url)?;
    Ok(Split {
        scheme: parts.scheme.unwrap_or_default(),
        netloc: parts.netloc.unwrap_or_default(),
        path: parts.path,
        query: parts.query.unwrap_or_default(),
        fragment: parts.fragment.unwrap_or_default(),
    })
}

const USES_RELATIVE: [&str; 20] = [
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];

const USES_NETLOC: [&str; 27] = [
    "",
    "ftp",
    "http",
    "gopher",
    "nntp",
    "telnet",
    "imap",
    "wais",
    "file",
    "mms",
    "https",
    "shttp",
    "snews",
    "prospero",
    "rtsp",
    "rtsps",
    "rtspu",
    "rsync",
    "svn",
    "svn+ssh",
    "sftp",
    "nfs",
    "git",
    "git+ssh",
    "ws",
    "wss",
    "itms-services",
];

fn unsplit(
    scheme: Option<&str>,
    netloc: Option<&str>,
    path: &str,
    query: Option<&str>,
    fragment: Option<&str>,
) -> String {
    let mut url = path.to_owned();
    if let Some(netloc) = netloc {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{netloc}{url}");
    } else if url.starts_with("//") {
        url = format!("//{url}");
    }
    if let Some(scheme) = scheme.filter(|s| !s.is_empty()) {
        url = format!("{scheme}:{url}");
    }
    if let Some(query) = query {
        url = format!("{url}?{query}");
    }
    if let Some(fragment) = fragment {
        url = format!("{url}#{fragment}");
    }
    url
}

/// `urllib.parse.urlunsplit(parts)` for a `urlsplit` result: empty components are dropped,
/// except the empty netloc a netloc scheme keeps before an absolute path.
pub fn urlunsplit(parts: &Split) -> String {
    let some = |s: &str| (!s.is_empty()).then_some(s.to_owned());
    let netloc = if parts.netloc.is_empty() {
        (USES_NETLOC.contains(&parts.scheme.as_str())
            && !parts.scheme.is_empty()
            && (parts.path.is_empty() || parts.path.starts_with('/')))
        .then(String::new)
    } else {
        Some(parts.netloc.clone())
    };
    unsplit(
        some(&parts.scheme).as_deref(),
        netloc.as_deref(),
        &parts.path,
        some(&parts.query).as_deref(),
        some(&parts.fragment).as_deref(),
    )
}

/// `urllib.parse.urljoin(base, url)` of CPython 3.15.
pub fn urljoin(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_owned());
    }
    if url.is_empty() {
        return Ok(base.to_owned());
    }
    let b = split_parts(base)?;
    let Parts {
        scheme,
        mut netloc,
        mut path,
        mut query,
        mut fragment,
    } = split_parts(url)?;
    let scheme = scheme.or(b.scheme.clone());
    let named = scheme.as_deref().filter(|s| !s.is_empty());
    if scheme != b.scheme || named.is_some_and(|s| !USES_RELATIVE.contains(&s)) {
        return Ok(url.to_owned());
    }
    if named.is_none_or(|s| USES_NETLOC.contains(&s)) {
        if netloc.as_deref().is_some_and(|n| !n.is_empty()) {
            return Ok(unsplit(
                scheme.as_deref(),
                netloc.as_deref(),
                &path,
                query.as_deref(),
                fragment.as_deref(),
            ));
        }
        netloc = b.netloc.clone();
    }
    if path.is_empty() {
        path = b.path.clone();
        if query.is_none() {
            query = b.query.clone();
            if fragment.is_none() {
                fragment = b.fragment.clone();
            }
        }
        return Ok(unsplit(
            scheme.as_deref(),
            netloc.as_deref(),
            &path,
            query.as_deref(),
            fragment.as_deref(),
        ));
    }
    let mut base_parts: Vec<&str> = b.path.split('/').collect();
    if base_parts.last() != Some(&"") {
        base_parts.pop();
    }
    let segments: Vec<&str> = if path.starts_with('/') {
        path.split('/').collect()
    } else {
        let mut all = base_parts;
        all.extend(path.split('/'));
        // segments[1:-1] = filter(None, segments[1:-1])
        if all.len() > 2 {
            let last = all.len() - 1;
            let mut kept = vec![all[0]];
            kept.extend(all[1..last].iter().filter(|s| !s.is_empty()));
            kept.push(all[last]);
            kept
        } else {
            all
        }
    };
    let mut resolved: Vec<&str> = Vec::new();
    for seg in &segments {
        match *seg {
            ".." => {
                resolved.pop();
            }
            "." => {}
            _ => resolved.push(seg),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    Ok(unsplit(
        scheme.as_deref(),
        netloc.as_deref(),
        if joined.is_empty() { "/" } else { &joined },
        query.as_deref(),
        fragment.as_deref(),
    ))
}

fn check_netloc(netloc: &str) -> Result<(), String> {
    if netloc.is_empty() || netloc.is_ascii() {
        return Ok(());
    }
    let n: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let normalised: String = n.nfkc().collect();
    if n == normalised {
        return Ok(());
    }
    if normalised.contains(['/', '?', '#', '@', ':']) {
        return Err(format!(
            "netloc '{netloc}' contains invalid characters under NFKC normalization"
        ));
    }
    Ok(())
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match host_and_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err("Invalid IPv6 URL".into());
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err("Invalid IPv6 URL".into());
            }
            hostname
        }
        None => host_and_port
            .split_once(':')
            .map_or(host_and_port, |(h, _)| h),
    };
    if hostname.starts_with(['v', 'V']) {
        let valid = hostname[1..].split_once('.').is_some_and(|(hex, rest)| {
            !hex.is_empty()
                && hex.chars().all(|c| c.is_ascii_hexdigit())
                && !rest.is_empty()
                && !rest.contains('\n')
        });
        return if valid {
            Ok(())
        } else {
            Err("IPvFuture address is invalid".into())
        };
    }
    // ipaddress.ip_address: an IPv6 address may carry a %zone; IPv4 is refused in brackets.
    // shortcut: Rust's address grammar stands in for ipaddress's; both refuse leading-zero
    // IPv4 octets and accept embedded IPv4 in IPv6.
    let address = hostname.split_once('%').map_or(hostname, |(a, _)| a);
    if address.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("An IPv4 address cannot be in brackets".into());
    }
    if address.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            crate::py::repr_str(hostname)
        ));
    }
    Ok(())
}

/// `.hostname` of a split URL: lower-cased, without user information, port or brackets.
pub fn hostname(netloc: &str) -> Option<String> {
    if netloc.is_empty() {
        return None;
    }
    let hostinfo = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = match hostinfo.split_once('[') {
        Some((_, bracketed)) => bracketed.split_once(']').map_or(bracketed, |(h, _)| h),
        None => hostinfo.split_once(':').map_or(hostinfo, |(h, _)| h),
    };
    if host.is_empty() {
        return None;
    }
    let (address, zone) = host
        .split_once('%')
        .map_or((host, None), |(a, z)| (a, Some(z)));
    Some(match zone {
        Some(zone) => format!("{}%{zone}", address.to_lowercase()),
        None => address.to_lowercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values from CPython 3.15's urljoin.
    #[test]
    fn joins_like_urllib() {
        for (base, url, expected) in [
            (
                "https://duguid.com.au/rates/example/",
                "rates.csv",
                "https://duguid.com.au/rates/example/rates.csv",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "../",
                "https://duguid.com.au/rates/",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "../../../../x",
                "https://duguid.com.au/x",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "//cdn.example.test/x",
                "https://cdn.example.test/x",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "?q=1",
                "https://duguid.com.au/rates/example/?q=1",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "#f",
                "https://duguid.com.au/rates/example/#f",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "mailto:x@y",
                "mailto:x@y",
            ),
            (
                "https://duguid.com.au/rates/example/",
                "HTTP://Example.TEST/A",
                "HTTP://Example.TEST/A",
            ),
            ("https://a.test/b/c", "./", "https://a.test/b/"),
            ("https://a.test/b/c", "..", "https://a.test/"),
            ("https://a.test/b/c?x#y", "", "https://a.test/b/c?x#y"),
            ("https://a.test/b/c?x#y", "#z", "https://a.test/b/c?x#z"),
            ("https://a.test/b/c?x#y", "?", "https://a.test/b/c?"),
            ("https://a.test", "x", "https://a.test/x"),
            ("https://a.test/b/", "a//b/./c/..", "https://a.test/b/a/b/"),
            ("http://a/b/c/d;p?q", "g;x?y#s", "http://a/b/c/g;x?y#s"),
            ("http://a/b/c/d;p?q", "../../../g", "http://a/g"),
            ("http://a/b/c/d;p?q", "/./g", "http://a/g"),
            ("", "x", "x"),
            ("foo:bar", "x", "x"),
            ("https://a.test/b", "ftp://h/x", "ftp://h/x"),
            ("https://a.test/b", "https:x", "https://a.test/x"),
            ("https://a.test/b", "tel:1", "tel:1"),
        ] {
            assert_eq!(urljoin(base, url).unwrap(), expected, "{base} + {url}");
        }
        assert!(urljoin("https://a.test/", "http://[::1").is_err());
    }

    #[test]
    fn splits_like_urllib() {
        let s = urlsplit(" \tMAILTO:ryan@example.test?subject=x").unwrap();
        assert_eq!(
            (s.scheme.as_str(), s.path.as_str(), s.query.as_str()),
            ("mailto", "ryan@example.test", "subject=x")
        );
        let s = urlsplit("https://User@WWW.LinkedIn.com:443/in/x/#f").unwrap();
        assert_eq!(hostname(&s.netloc).as_deref(), Some("www.linkedin.com"));
        assert_eq!(s.path, "/in/x/");
        assert!(urlsplit("https://[::1/x").is_err());
        assert!(urlsplit("https://[1.2.3.4]/").is_err());
        assert!(urlsplit("https://[fe80::1%25eth0]/").is_ok());
        assert_eq!(urlsplit("1http:x").unwrap().scheme, "");
        assert!(urlsplit("https://ex\u{2100}ample/").is_err());
    }
}
