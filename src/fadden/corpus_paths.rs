//! `fadden/corpus_paths.py`: the filesystem boundaries the ported stages use.
use super::Exc;
use crate::pyjson::Json;
use crate::pypath;

/// `register_id(value)`: `[A-Z][0-9]{4}[A-Z][0-9]{5}`.
pub fn register_id(value: &Json) -> Result<String, Exc> {
    let valid = value.as_str().filter(|s| {
        let b = s.as_bytes();
        b.len() == 11
            && b[0].is_ascii_uppercase()
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[5].is_ascii_uppercase()
            && b[6..].iter().all(u8::is_ascii_digit)
    });
    valid
        .map(str::to_string)
        .ok_or_else(|| Exc::value("invalid Federal Register identifier"))
}

pub fn register_id_str(value: &str) -> Result<String, Exc> {
    register_id(&Json::str(value))
}

/// `child(root, *parts)`: a realpath-contained descendant of `root`.
pub fn child(root: &str, parts: &[&str]) -> Result<String, Exc> {
    let root_path = pypath::realpath(root)?;
    let mut joined = root_path.clone();
    for part in parts {
        joined = pypath::join(&joined, part);
    }
    let candidate = pypath::realpath(&joined)?;
    let sep = pypath::SEP;
    let prefix = if root_path.ends_with(sep) {
        root_path.clone()
    } else {
        format!("{root_path}{sep}")
    };
    if candidate == root_path || !candidate.starts_with(&prefix) {
        return Err(Exc::value("path escapes the corpus root"));
    }
    Ok(candidate)
}
