//! `fadden/pii_patterns.py`'s detectors for the ported stages: personal names
//! beside registration numbers, and digests of contact details (email, phone,
//! tax file number) that never carry the identifier itself. The patterns are
//! the Python module's, unchanged; the Python distribution stages still use it.
use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::Exc;
use crate::py;
use crate::pyjson::{self, Json};
use crate::pyre;
use crate::pyvalue;

const TOKEN: &str = "[A-Z][A-Za-z'\u{2019}-]{1,20}";
const REGNO: &str = r"\b\d{8}\b";
const EMAIL: &str = r"\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b";
const PHONE: &str = concat!(
    r"(?<!\d)(?<!\d\.)(?:",
    r"\(0\d\)[\s-]?\d{4}[\s-]?\d{4}",
    r"|0[2-8][\s-]?\d{4}[\s-]?\d{4}",
    r"|04\d{2}[\s-]?\d{3}[\s-]?\d{3}",
    r"|\+61[\s-]?(?:\(0\)[\s-]?)?\d(?:[\s-]?\d){8}",
    r"|\+61[\s-]?\(0?\d\)[\s-]?\d(?:[\s-]?\d){7}",
    r"|1[38]00[\s-]?\d{3}[\s-]?\d{3}",
    r"|13[\s-]?\d{2}[\s-]?\d{2}",
    r")(?!\d)",
);
const TFN: &str =
    "\\b(?:tax file number|TFN)\\b\\s*(?:[.:\u{2013}-]\\s*)?(\\d(?:[\\s-]?\\d){7,8})(?![\\s-]?\\d)";
const TFN_BARE: &str = r"(?<![\d$])(?<![\d$][\s-])(\d{3}([\s-]?)\d{3}\2\d{3})(?![\s-]?\d)";
const STATUTORY: &str = concat!(
    r"\b(Act|Regulation|Schedule|Division|Subdivision|Part|Chapter|Section|",
    r"Commissioner|Minister|Treasurer|Commonwealth|Australian|Australia|Board|",
    r"Tax|Taxation|Income|Superannuation|Court|Tribunal|Determination|Notice|",
    r"Instrument|Amendment|January|February|March|April|May|June|July|August|",
    r"September|October|November|December)\b",
);
/// Weighted mod-11 check-digit sum every issued nine-digit TFN satisfies.
const TFN_WEIGHTS: [u32; 9] = [1, 4, 3, 7, 5, 8, 6, 9, 10];

fn name_pattern() -> String {
    format!(r"\b{TOKEN},?\s+{TOKEN}(?:\s+{TOKEN})?\b")
}

/// `is_statutory(candidate)`: a NAME match that is legislative vocabulary.
fn is_statutory(candidate: &str) -> bool {
    if pyre::re(STATUTORY, 0).is_match(candidate) {
        return true;
    }
    let all_caps = candidate
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|t| t.len() > 1 && t.chars().all(|c| c.is_ascii_uppercase()));
    all_caps && pyre::re(STATUTORY, pyre::I).is_match(candidate)
}

/// `private_person_registration_details(text)`: the NAME matches that survive
/// the statutory filter, and the 8-digit registration numbers.
pub fn private_person_registration_details(text: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let names = pyre::re(&name_pattern(), 0)
        .captures_iter(text)
        .map(|c| c.get(0).map_or("", |m| m.as_str()).to_string())
        .filter(|name| !is_statutory(name))
        .collect();
    let numbers = pyre::re(REGNO, 0).findall(text).into_iter().collect();
    (names, numbers)
}

/// `has_private_person_registration_pair(text)`.
pub fn has_private_person_registration_pair(text: &str) -> bool {
    let (names, numbers) = private_person_registration_details(text);
    !names.is_empty() && !numbers.is_empty()
}

fn plausible_tfn(value: &str) -> bool {
    let digits: Vec<u32> = value.chars().filter_map(py::decimal_value).collect();
    digits.len() == 9
        && digits
            .iter()
            .zip(TFN_WEIGHTS)
            .map(|(d, w)| d * w)
            .sum::<u32>()
            % 11
            == 0
}

/// `_contact_value(kind, match)`: the normalised identifier a digest covers.
fn contact_value(kind: &str, value: &str) -> String {
    if kind == "email" {
        return py::casefold(py::strip(value));
    }
    let digits: String = value.chars().filter(|&c| py::is_decimal(c)).collect();
    if kind == "phone" && value.starts_with("+61") {
        // International notation of a national number gets the same digest.
        let rest: String = digits.chars().skip(2).collect();
        return if rest.starts_with('0') {
            rest
        } else {
            format!("0{rest}")
        };
    }
    digits
}

/// `contact_fingerprints(text)`: `(kind, sha256)` for each contact match, in
/// pattern order, never the identifier. A bare nine-digit run counts only
/// outside a labelled TFN and with a valid check digit.
pub fn contact_fingerprints(text: &str) -> Vec<(&'static str, String)> {
    let labelled: Vec<(usize, usize)> = pyre::re(TFN, pyre::I)
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| (m.start(), m.end())))
        .collect();
    let mut found = Vec::new();
    for (kind, pattern, flags) in [
        ("email", EMAIL, 0),
        ("phone", PHONE, 0),
        ("tfn", TFN, pyre::I),
        ("tfn", TFN_BARE, 0),
    ] {
        for c in pyre::re(pattern, flags).captures_iter(text) {
            let whole = c.get(0).expect("group 0");
            let value = if kind == "tfn" {
                c.get(1).expect("group 1 always takes part")
            } else {
                whole
            };
            if pattern == TFN_BARE {
                let inside = labelled
                    .iter()
                    .any(|&(s, e)| s <= value.start() && value.end() <= e);
                if inside || !plausible_tfn(value.as_str()) {
                    continue;
                }
            }
            let normalised = contact_value(kind, value.as_str());
            let digest: String = Sha256::digest(normalised.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            found.push((kind, digest));
        }
    }
    found
}

/// One approved organisational contact: `(kind, sha256, register_id)`.
pub type Approved = (String, String, String);

/// `load_contact_allowlist(path)`: the hashed organisational-contact policy,
/// strictly validated.
pub fn load_contact_allowlist(path: &str) -> Result<Vec<Approved>, Exc> {
    let text = crate::pyio::read_text(path)?;
    let document = pyjson::loads_standard(&text).map_err(super::json_error)?;
    let has_keys = |value: &Json, keys: &[&str]| value.has_exactly(keys);
    if !has_keys(&document, &["schema_version", "entries"]) {
        return Err(Exc::value("invalid contact allowlist document"));
    }
    let entries = match document.at("entries") {
        Json::Array(items)
            if pyvalue::py_eq(document.at("schema_version"), &Json::Int(1.into())) =>
        {
            items
        }
        _ => return Err(Exc::value("unsupported contact allowlist schema")),
    };
    let sha = pyre::re(r"[0-9a-f]{64}\Z", 0);
    let rid_re = pyre::re(r"[A-Z]\d{4}[A-Z]\d{5}\Z", 0);
    let full =
        |re: &pyre::Re, value: &Json| value.as_str().is_some_and(|s| re.match_at(s, 0).is_some());
    let mut approved: Vec<Approved> = Vec::new();
    for entry in entries {
        if !has_keys(entry, &["kind", "sha256", "register_id", "reason"]) {
            return Err(Exc::value("invalid contact allowlist entry"));
        }
        let kind = entry.at("kind");
        if matches!(kind, Json::Array(_) | Json::Object(_)) {
            return Err(Exc::Type(format!(
                "cannot use '{0}' as a set element (unhashable type: '{0}')",
                pyvalue::type_name(kind)
            )));
        }
        if !matches!(kind.as_str(), Some("email" | "phone")) {
            return Err(Exc::value("invalid contact allowlist kind"));
        }
        if !full(&sha, entry.at("sha256")) {
            return Err(Exc::value("invalid contact allowlist digest"));
        }
        if !full(&rid_re, entry.at("register_id")) {
            return Err(Exc::value("invalid contact allowlist register id"));
        }
        let reason = entry.at("reason");
        let blank = match reason {
            Json::Str(s) => s
                .code_points()
                .iter()
                .all(|&cp| char::from_u32(cp).is_some_and(py::is_space)),
            _ => true,
        };
        if blank {
            return Err(Exc::value("invalid contact allowlist reason"));
        }
        let reason_text = pyvalue::points_lossy(&pyvalue::str_points(reason));
        if !contact_fingerprints(&reason_text).is_empty() {
            return Err(Exc::value(
                "contact allowlist reason contains a raw identifier",
            ));
        }
        let key = (
            kind.as_str().unwrap_or_default().to_string(),
            entry.at("sha256").as_str().unwrap_or_default().to_string(),
            entry
                .at("register_id")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        if approved.contains(&key) {
            return Err(Exc::value("duplicate contact allowlist entry"));
        }
        approved.push(key);
    }
    Ok(approved)
}

/// The fuzz target's invariants for one input (fuzz/fuzz_targets): every
/// reported name and registration number occurs in the text, and every
/// contact fingerprint names a known kind and carries a SHA-256 digest, never
/// the identifier. The seed corpus replays through this in the unit tests.
pub fn check_one_input(data: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(data);
    let (names, numbers) = private_person_registration_details(&text);
    if !names
        .iter()
        .chain(&numbers)
        .all(|found| text.contains(found.as_str()))
    {
        return Err("a reported match is not in the text".into());
    }
    for (kind, digest) in contact_fingerprints(&text) {
        let hex = digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !matches!(kind, "email" | "phone" | "tfn") || !hex {
            return Err(format!("malformed fingerprint ({kind:?}, {digest:?})"));
        }
    }
    Ok(())
}

/// A contact `(kind, sha256, register_id)` that no allowlist entry approves.
pub type Unapproved = (&'static str, String, String);

/// `unapproved_contact_fingerprints(text, register_id, approved)`, sorted: a
/// tax file number is never approved.
pub fn unapproved_contact_fingerprints(
    text: &str,
    register_id: &str,
    approved: &[Approved],
) -> Vec<Unapproved> {
    let mut found: Vec<Unapproved> = contact_fingerprints(text)
        .into_iter()
        .filter(|(kind, digest)| {
            *kind == "tfn"
                || !approved
                    .iter()
                    .any(|(k, d, r)| k == kind && d == digest && r == register_id)
        })
        .map(|(kind, digest)| (kind, digest, register_id.to_string()))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// Whether a privacy check could not read a file: `OSError` or `UnicodeError`.
pub fn unreadable(error: &Exc) -> bool {
    match error {
        Exc::Os(_) => true,
        Exc::Value { class, .. } => class.starts_with("Unicode"),
        _ => false,
    }
}

/// `privacy_findings_in_file(path, register_id, approved)`: whether the file
/// pairs a name with a registration number, and its unapproved contacts. A
/// file that is not UTF-8, or carries C0 or C1 controls other than tab, line
/// endings and form feed, is refused as `UnicodeError`.
pub fn privacy_findings_in_file(
    path: &str,
    register_id: &str,
    approved: &[Approved],
) -> Result<(bool, Vec<Unapproved>), Exc> {
    let text = crate::pyio::read_text(path)?;
    let binary = text.chars().any(|c| {
        let cp = u32::from(c);
        (cp < 32 && !matches!(c, '\t' | '\n' | '\r' | '\x0c')) || (127..=159).contains(&cp)
    });
    if binary {
        return Err(Exc::Value {
            class: "UnicodeError",
            message: "redistributed file contains binary control bytes".into(),
        });
    }
    Ok((
        has_private_person_registration_pair(&text),
        unapproved_contact_fingerprints(&text, register_id, approved),
    ))
}
