//! Compare a baseline, an observation and a source map into an impact queue,
//! and validate a technical-review decision against an intact queue.
use std::collections::{BTreeSet, HashMap, HashSet};

use super::persist::write_queue_files;
use super::util::{
    MAX_JSON_BYTES, SourceSnapshot, load_json, load_json_exact, safe_markdown, sha256_json,
};
use super::{Error, Result};
use crate::py;
use crate::pyjson::{Json, Layout, PyStr, dumps};
use crate::pypath::PyPath;
use crate::pyurl;

const OBSERVATION_STATES: [&str; 5] = [
    "UNCHANGED",
    "SUPERSEDED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "NO_LONGER_IN_FORCE",
    "LOOKUP_FAILED",
];
const ALLOWED_DECISIONS: [&str; 4] = [
    "AWAIT_PRIMARY_TEXT",
    "NO_WORKFLOW_CHANGE",
    "UPDATE_CANDIDATE",
    "ESCALATE_TECHNICAL_REVIEW",
];
const CONTENT_KINDS: [&str; 2] = ["metadata-response", "source-document"];
const CONTENT_MEDIA_TYPES: [&str; 4] = [
    "application/json",
    "application/pdf",
    "application/xhtml+xml",
    "text/html",
];

pub const QUEUE_FIELDS: [&str; 9] = [
    "schema_version",
    "run_id",
    "queue_digest",
    "mode",
    "run_status",
    "source_digests",
    "baseline",
    "observation",
    "items",
];
const SOURCE_DIGEST_FIELDS: [&str; 3] = ["baseline", "mapping", "observation"];
const BASELINE_QUEUE_FIELDS: [&str; 3] = ["id", "retrieved", "source"];
const OBSERVATION_QUEUE_FIELDS: [&str; 3] = ["id", "observed_at", "complete"];
const ITEM_FIELDS: [&str; 7] = [
    "item_id",
    "state",
    "change_kind",
    "source",
    "impact_candidates",
    "mapping_status",
    "limitations",
];
const SOURCE_FIELDS: [&str; 6] = [
    "register_id",
    "collection",
    "title",
    "baseline_compilation",
    "observed_compilation",
    "evidence_url",
];
const CANDIDATE_FIELDS: [&str; 5] = [
    "mapping_id",
    "skill_ref",
    "owner_role",
    "review_question",
    "mapping_basis",
];
const DECISION_FIELDS: [&str; 6] = [
    "schema_version",
    "run_id",
    "queue_digest",
    "reviewer_ref",
    "reviewed_at",
    "decisions",
];
const DECISION_ENTRY_FIELDS: [&str; 4] = ["item_id", "decision", "rationale", "evidence_note"];
const MAPPING_STATUSES: [&str; 3] = ["MAPPED", "UNMAPPED_SOURCE", "NOT_EVALUATED"];

/// The state and mapping statuses each change kind admits.
fn change_kind_rule(kind: &str) -> Option<(&'static str, &'static [&'static str])> {
    const EVALUATED: &[&str] = &["MAPPED", "UNMAPPED_SOURCE"];
    Some(match kind {
        "INCOMPLETE_SCOPE" | "MISSING_OBSERVATION" => ("BLOCKED", &["NOT_EVALUATED"]),
        "SUPERSEDED" | "NO_LONGER_IN_FORCE" => ("OPEN", EVALUATED),
        "CURRENT_NO_PUBLISHED_COMPILATION" | "LOOKUP_FAILED" | "BASELINE_NOT_CURRENT" => {
            ("BLOCKED", EVALUATED)
        }
        _ => return None,
    })
}

struct BaselineTitle {
    register_id: String,
    collection: String,
    name: String,
    compilation_number: String,
    compilation_date: String,
    version_is_current: bool,
    register_page: String,
}

/// `value` is a `str` that equals `expected`.
fn is_text(value: &Json, expected: &str) -> bool {
    value.as_str() == Some(expected)
}

/// `isinstance(value, str) and value in allowed`.
fn one_of<'a>(value: &'a Json, allowed: &[&str]) -> Option<&'a str> {
    value.as_str().filter(|s| allowed.contains(s))
}

/// A non-empty string without control characters or surrogates, stripped.
pub fn non_empty(value: &Json, field: &str) -> Result<String> {
    let Json::Str(text) = value else {
        return Err(Error::monitor(format!(
            "{field} must be a non-empty string."
        )));
    };
    let points = text.code_points();
    let space = |cp: &u32| char::from_u32(*cp).is_some_and(py::is_space);
    let start = points.iter().position(|cp| !space(cp));
    let Some(start) = start else {
        return Err(Error::monitor(format!(
            "{field} must be a non-empty string."
        )));
    };
    let end = points
        .iter()
        .rposition(|cp| !space(cp))
        .map_or(start, |i| i + 1);
    let stripped = &points[start..end];
    if stripped.iter().any(|&cp| cp < 32 || cp == 127) {
        return Err(Error::monitor(format!(
            "{field} must not contain control characters."
        )));
    }
    if stripped.iter().any(|cp| (0xd800..=0xdfff).contains(cp)) {
        return Err(Error::monitor(format!(
            "{field} must not contain surrogate code points."
        )));
    }
    Ok(stripped
        .iter()
        .filter_map(|&cp| char::from_u32(cp))
        .collect())
}

fn sha256_id(value: &Json, field: &str) -> Result<String> {
    let text = non_empty(value, field)?;
    let valid = text.len() == 71
        && text.starts_with("sha256:")
        && text[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(Error::monitor(format!(
            "{field} must be a lowercase sha256 identifier."
        )));
    }
    Ok(text)
}

/// Whether `chars` matches `\d{4}-\d{2}-\d{2}` with Python's Unicode `\d`.
fn date_shape(chars: &[char]) -> bool {
    chars.len() == 10
        && chars.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == '-',
            _ => py::is_decimal(c),
        })
}

fn iso_date(value: &Json, field: &str) -> Result<String> {
    let text = non_empty(value, field)?;
    let chars: Vec<char> = text.chars().collect();
    if !date_shape(&chars) || py::Date::fromisoformat(&text).is_none() {
        return Err(Error::monitor(format!("{field} must be an ISO date.")));
    }
    Ok(text)
}

fn iso_date_nullable(value: &Json, field: &str) -> Result<Option<String>> {
    if matches!(value, Json::Null) {
        return Ok(None);
    }
    iso_date(value, field).map(Some)
}

/// A parsed timestamp: microseconds of local time and the UTC offset in seconds.
#[derive(Clone, Copy)]
struct Timestamp {
    local_micros: i128,
    offset: Option<i64>,
}

impl Timestamp {
    fn utc_micros(&self) -> i128 {
        self.local_micros - i128::from(self.offset.unwrap_or(0)) * 1_000_000
    }
}

/// The value of a run of decimal digits, ASCII or not, as `int()` reads it.
fn digits_value(chars: &[char]) -> i64 {
    chars.iter().fold(0, |value, &c| {
        value * 10 + i64::from(py::decimal_value(c).unwrap_or(0))
    })
}

/// Parse the one timestamp grammar the radar accepts.
///
/// `TIMESTAMP_PATTERN` is matched first (its `\d` is Unicode), then the text
/// is parsed as `datetime.strptime` does with `%Y-%m-%dT%H:%M:%S[.%f][%z]`:
/// its directives accept only some fields with non-ASCII digits.
fn parse_timestamp(text: &str) -> Option<Timestamp> {
    let c: Vec<char> = text.chars().collect();
    let d = |i: usize| c.get(i).is_some_and(|ch| py::is_decimal(*ch));
    let shape = c.len() >= 19
        && date_shape(&c[..10])
        && matches!(c[10], 'T' | 't' | ' ')
        && d(11)
        && d(12)
        && c[13] == ':'
        && d(14)
        && d(15)
        && c[16] == ':'
        && d(17)
        && d(18);
    if !shape {
        return None;
    }
    let mut i = 19;
    let mut fraction: Option<&[char]> = None;
    if c.get(i) == Some(&'.') {
        let n = c[i + 1..]
            .iter()
            .take_while(|ch| py::is_decimal(**ch))
            .count();
        if !(1..=6).contains(&n) {
            return None;
        }
        fraction = Some(&c[i + 1..i + 1 + n]);
        i += 1 + n;
    }
    let offset: Option<&[char]> = match &c[i..] {
        [] => None,
        ['Z'] => Some(&c[i..]),
        ['+' | '-', h1, h2, ':', m1, m2] => {
            if ![h1, h2, m1, m2].iter().all(|ch| py::is_decimal(**ch)) {
                return None;
            }
            Some(&c[i..])
        }
        _ => return None,
    };
    // strptime's directives, field by field.
    let ascii = |ch: char, range: std::ops::RangeInclusive<char>| range.contains(&ch);
    let pair = |at: usize| (c[at], c[at + 1]);
    let month_ok = match pair(5) {
        ('1', b) => ascii(b, '0'..='2'),
        ('0', b) => ascii(b, '1'..='9'),
        _ => false,
    };
    let day_ok = match pair(8) {
        ('3', b) => ascii(b, '0'..='1'),
        ('1' | '2', b) => py::is_decimal(b),
        ('0', b) => ascii(b, '1'..='9'),
        _ => false,
    };
    let hour_ok = match pair(11) {
        ('2', b) => ascii(b, '0'..='3'),
        ('0' | '1', b) => py::is_decimal(b),
        _ => false,
    };
    let minute_ok = ascii(c[14], '0'..='5');
    let second_ok = match pair(17) {
        ('6', b) => ascii(b, '0'..='1'),
        (a, _) => ascii(a, '0'..='5'),
    };
    let fraction_ok = fraction.is_none_or(|f| f.iter().all(char::is_ascii_digit));
    let offset_ok = match offset {
        Some([_, _, _, ':', m1, _]) => ascii(*m1, '0'..='5'),
        _ => true,
    };
    if !(month_ok && day_ok && hour_ok && minute_ok && second_ok && fraction_ok && offset_ok) {
        return None;
    }
    let date = py::Date::new(
        digits_value(&c[..4]),
        digits_value(&c[5..7]),
        digits_value(&c[8..10]),
    )
    .ok()?;
    let second = digits_value(&c[17..19]);
    if second > 59 {
        return None;
    }
    let micros = fraction.map_or(0, |f| {
        let padded: String = f
            .iter()
            .chain(std::iter::repeat_n(&'0', 6 - f.len()))
            .collect();
        padded.parse::<i64>().unwrap_or(0)
    });
    let offset = match offset {
        None => None,
        Some(['Z']) => Some(0),
        Some(z) => {
            let seconds = digits_value(&z[1..3]) * 3600 + digits_value(&z[4..6]) * 60;
            if seconds >= 86_400 {
                return None;
            }
            Some(if z[0] == '-' { -seconds } else { seconds })
        }
    };
    let clock = digits_value(&c[11..13]) * 3600 + digits_value(&c[14..16]) * 60 + second;
    let local_micros =
        (i128::from(date.ordinal()) * 86_400 + i128::from(clock)) * 1_000_000 + i128::from(micros);
    Some(Timestamp {
        local_micros,
        offset,
    })
}

fn timestamp(text: &str, field: &str) -> Result<Timestamp> {
    parse_timestamp(text)
        .ok_or_else(|| Error::monitor(format!("{field} must be an ISO 8601 timestamp.")))
}

fn iso_timestamp(value: &Json, field: &str) -> Result<String> {
    let text = non_empty(value, field)?;
    if timestamp(&text, field)?.offset.is_none() {
        return Err(Error::monitor(format!(
            "{field} must include an explicit UTC offset or Z."
        )));
    }
    Ok(text)
}

/// `urlsplit(url).port`'s validation: `Err` where Python raises ValueError.
fn check_port(netloc: &str) -> std::result::Result<(), ()> {
    let hostinfo = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let port = match hostinfo.split_once('[') {
        Some((_, bracketed)) => bracketed
            .split_once(']')
            .map_or("", |(_, rest)| rest.split_once(':').map_or("", |(_, p)| p)),
        None => hostinfo.split_once(':').map_or("", |(_, p)| p),
    };
    if port.is_empty() {
        return Ok(());
    }
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    let digits = port.trim_start_matches('0');
    if digits.len() > 5 || digits.parse::<u32>().unwrap_or(0) > 65_535 {
        return Err(());
    }
    Ok(())
}

fn https_url(value: &Json, field: &str) -> Result<String> {
    let text = non_empty(value, field)?;
    let refuse = || Error::monitor(format!("{field} must be an https URL."));
    let parsed = pyurl::urlsplit(&text).map_err(|_| refuse())?;
    check_port(&parsed.netloc).map_err(|()| refuse())?;
    let invalid_authority = pyurl::hostname(&parsed.netloc).is_none()
        || parsed.netloc.contains('@')
        || parsed.netloc.contains('\\')
        || parsed.netloc.chars().any(py::is_space);
    if !parsed.scheme.eq_ignore_ascii_case("https") || invalid_authority {
        return Err(refuse());
    }
    Ok(text)
}

fn load_baseline(snapshot: &SourceSnapshot) -> Result<(Vec<BaselineTitle>, Json)> {
    let raw = load_json_exact(
        snapshot,
        &["corpus", "retrieved", "source", "source_api", "titles"],
        "baseline source index",
    )?;
    let Some(Json::Array(raw_titles)) = raw
        .get("titles")
        .filter(|_| is_text(raw.at("source"), "Federal Register of Legislation"))
    else {
        return Err(Error::monitor(
            "Baseline must be a Federal Register title index with a titles list.",
        ));
    };
    iso_date(raw.at("retrieved"), "baseline retrieved")?;
    https_url(raw.at("source_api"), "baseline source_api")?;
    let expected = [
        "register_id",
        "name",
        "collection",
        "compilation_number",
        "compilation_date",
        "version_is_current",
        "current_version_start",
        "retrieved",
        "source_url",
        "register_page",
    ];
    let mut titles = Vec::new();
    let mut seen = HashSet::new();
    for (index, raw_title) in raw_titles.iter().enumerate().map(|(i, t)| (i + 1, t)) {
        if !raw_title.has_exactly(&expected) {
            return Err(Error::monitor(format!(
                "Baseline title {index} has an invalid shape."
            )));
        }
        let field = |name: &str| format!("title {index} {name}");
        let register_id = non_empty(raw_title.at("register_id"), &field("register_id"))?;
        let collection = non_empty(raw_title.at("collection"), &field("collection"))?;
        let name = non_empty(raw_title.at("name"), &field("name"))?;
        let compilation_number = non_empty(
            raw_title.at("compilation_number"),
            &field("compilation_number"),
        )?;
        let compilation_date =
            iso_date(raw_title.at("compilation_date"), &field("compilation_date"))?;
        iso_date_nullable(
            raw_title.at("current_version_start"),
            &field("current_version_start"),
        )?;
        https_url(raw_title.at("source_url"), &field("source_url"))?;
        let register_page = https_url(raw_title.at("register_page"), &field("register_page"))?;
        let Json::Bool(version_is_current) = *raw_title.at("version_is_current") else {
            return Err(Error::monitor(format!(
                "title {index} version_is_current must be a boolean."
            )));
        };
        if !seen.insert(register_id.clone()) {
            return Err(Error::monitor(
                "Baseline source index contains duplicate register IDs.",
            ));
        }
        titles.push(BaselineTitle {
            register_id,
            collection,
            name,
            compilation_number,
            compilation_date,
            version_is_current,
            register_page,
        });
    }
    if titles.is_empty() {
        return Err(Error::monitor("Baseline source index has no titles."));
    }
    Ok((titles, raw))
}

/// One observed title, with the values the queue carries forward.
struct Observed {
    register_id: String,
    collection: String,
    state: String,
    evidence_url: Json,
    number: Json,
    date: Json,
    document_id: Json,
}

struct Observation {
    observed_at: Json,
    complete: bool,
    items: Vec<Observed>,
}

fn load_observation(
    snapshot: &SourceSnapshot,
    expected_ids: &HashSet<String>,
) -> Result<Observation> {
    const V1: &str = "au-tax-register-observation.v1";
    const V2: &str = "au-tax-register-observation.v2";
    const V3: &str = "au-tax-register-observation.v3";
    let pre_raw = load_json(snapshot, "Register observation")?;
    let version = match &pre_raw {
        Json::Object(_) => pre_raw.get("schema_version"),
        _ => None,
    };
    let Some(version) = version else {
        return Err(Error::monitor(
            "Register observation must be a JSON object with schema_version.",
        ));
    };
    let version = match version.as_str() {
        Some(v @ (V1 | V2 | V3)) => v,
        _ => {
            return Err(Error::monitor(
                "Only au-tax-register-observation.v1, au-tax-register-observation.v2 and \
                 au-tax-register-observation.v3 in synthetic mode are supported.",
            ));
        }
    };
    let mut top = vec![
        "schema_version",
        "mode",
        "observed_at",
        "expected_register_ids",
        "complete",
        "observations",
    ];
    if version != V1 {
        top.push("scope_id");
    }
    let raw = load_json_exact(snapshot, &top, "Register observation")?;
    if version != V1 {
        non_empty(raw.at("scope_id"), "scope_id")?;
    }
    if !is_text(raw.at("mode"), "synthetic") {
        return Err(Error::monitor("Only synthetic mode is supported."));
    }
    iso_timestamp(raw.at("observed_at"), "observed_at")?;
    let ids = match raw.at("expected_register_ids") {
        Json::Array(items) if items.iter().all(|i| matches!(i, Json::Str(_))) => items,
        _ => {
            return Err(Error::monitor(
                "Observation expected_register_ids must be a list of strings.",
            ));
        }
    };
    let cleaned = ids
        .iter()
        .map(|item| non_empty(item, "expected_register_ids item"))
        .collect::<Result<Vec<_>>>()?;
    let unique: HashSet<String> = cleaned.iter().cloned().collect();
    if unique.len() != cleaned.len() {
        return Err(Error::monitor(
            "Observation expected_register_ids must not contain duplicates.",
        ));
    }
    if &unique != expected_ids {
        return Err(Error::monitor(
            "Observation expected_register_ids must exactly match the baseline scope.",
        ));
    }
    let (Json::Bool(complete), Json::Array(raw_items)) =
        (raw.at("complete"), raw.at("observations"))
    else {
        return Err(Error::monitor(
            "Observation complete/observations fields are invalid.",
        ));
    };

    let mut required = vec![
        "register_id",
        "collection",
        "state",
        "evidence_url",
        "checked_at",
        "observed_compilation_number",
        "observed_compilation_date",
        "observed_register_document_id",
        "current_version_start",
        "error_category",
    ];
    if version != V1 {
        required.push("evidence_id");
    }
    if version == V3 {
        required.extend(["content_sha256", "content_kind", "content_media_type"]);
    }

    let mut seen = HashSet::new();
    let mut seen_evidence_ids = HashSet::new();
    let mut items = Vec::new();
    for (index, item) in raw_items.iter().enumerate().map(|(i, t)| (i + 1, t)) {
        if !item.has_exactly(&required) {
            return Err(Error::monitor(format!(
                "Observation item {index} has an invalid shape."
            )));
        }
        let field = |name: &str| format!("observation {index} {name}");
        let register_id = non_empty(item.at("register_id"), &field("register_id"))?;
        if !expected_ids.contains(&register_id) {
            return Err(Error::monitor(
                "Observation contains a register ID outside the baseline scope.",
            ));
        }
        if !seen.insert(register_id.clone()) {
            return Err(Error::monitor(
                "Observation contains duplicate register IDs.",
            ));
        }
        let collection = non_empty(item.at("collection"), &field("collection"))?;
        if version != V1 {
            let evidence_id = non_empty(item.at("evidence_id"), &field("evidence_id"))?;
            if !seen_evidence_ids.insert(evidence_id) {
                return Err(Error::monitor(format!(
                    "Observation item {index} has duplicate evidence_id."
                )));
            }
        }
        if version == V3 {
            sha256_id(item.at("content_sha256"), &field("content_sha256"))?;
            let kind = non_empty(item.at("content_kind"), &field("content_kind"))?;
            if !CONTENT_KINDS.contains(&kind.as_str()) {
                return Err(Error::monitor(format!(
                    "observation {index} content_kind has an unsupported value."
                )));
            }
            let media = non_empty(item.at("content_media_type"), &field("content_media_type"))?;
            if !CONTENT_MEDIA_TYPES.contains(&media.as_str()) {
                return Err(Error::monitor(format!(
                    "observation {index} content_media_type has an unsupported value."
                )));
            }
        }
        let Some(state) = one_of(item.at("state"), &OBSERVATION_STATES) else {
            return Err(Error::monitor(format!(
                "Observation {index} has an unsupported state."
            )));
        };
        let state_fields: &[&str] = match state {
            "SUPERSEDED" => &[
                "observed_compilation_number",
                "observed_compilation_date",
                "observed_register_document_id",
            ],
            "CURRENT_NO_PUBLISHED_COMPILATION" => &["current_version_start"],
            "LOOKUP_FAILED" => &["error_category"],
            _ => &[],
        };
        for name in [
            "observed_register_document_id",
            "observed_compilation_number",
            "observed_compilation_date",
            "current_version_start",
            "error_category",
        ] {
            if !state_fields.contains(&name) && !matches!(item.at(name), Json::Null) {
                return Err(Error::monitor(format!("{state} must have a null {name}.")));
            }
        }
        https_url(item.at("evidence_url"), &field("evidence_url"))?;
        iso_timestamp(item.at("checked_at"), &field("checked_at"))?;
        match state {
            "SUPERSEDED" => {
                for name in [
                    "observed_compilation_number",
                    "observed_compilation_date",
                    "observed_register_document_id",
                ] {
                    non_empty(item.at(name), &field(name))?;
                }
                iso_date(
                    item.at("observed_compilation_date"),
                    &field("observed_compilation_date"),
                )?;
            }
            "CURRENT_NO_PUBLISHED_COMPILATION" => {
                iso_date(
                    item.at("current_version_start"),
                    &field("current_version_start"),
                )?;
            }
            "LOOKUP_FAILED" => {
                non_empty(item.at("error_category"), &field("error_category"))?;
            }
            _ => {}
        }
        items.push(Observed {
            register_id,
            collection,
            state: state.to_string(),
            evidence_url: item.at("evidence_url").clone(),
            number: item.at("observed_compilation_number").clone(),
            date: item.at("observed_compilation_date").clone(),
            document_id: item.at("observed_register_document_id").clone(),
        });
    }
    if *complete && &seen != expected_ids {
        return Err(Error::monitor(
            "A complete observation must cover every expected register ID exactly once.",
        ));
    }
    Ok(Observation {
        observed_at: raw.at("observed_at").clone(),
        complete: *complete,
        items,
    })
}

struct Mapping {
    mapping_id: String,
    register_id: String,
    collection: String,
    skill_ref: String,
    owner_role: String,
    review_question: String,
}

fn load_mapping(snapshot: &SourceSnapshot) -> Result<Vec<Mapping>> {
    let raw = load_json_exact(
        snapshot,
        &["schema_version", "mapping_version", "entries"],
        "source-to-skill map",
    )?;
    let Json::Array(entries) = raw.at("entries") else {
        return Err(Error::monitor(
            "Source-to-skill map has an unsupported schema.",
        ));
    };
    if !is_text(raw.at("schema_version"), "au-tax-source-skill-map.v1") {
        return Err(Error::monitor(
            "Source-to-skill map has an unsupported schema.",
        ));
    }
    let required = [
        "mapping_id",
        "register_id",
        "collection",
        "source_kind",
        "skill_ref",
        "skill_path",
        "owner_role",
        "review_question",
    ];
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for (index, item) in entries.iter().enumerate().map(|(i, t)| (i + 1, t)) {
        let Json::Object(pairs) = item else {
            return Err(Error::monitor(format!(
                "Mapping entry {index} has an invalid shape."
            )));
        };
        if !item.has_exactly(&required) {
            return Err(Error::monitor(format!(
                "Mapping entry {index} has an invalid shape."
            )));
        }
        let mut cleaned: HashMap<&str, String> = HashMap::new();
        for (key, value) in pairs {
            let key = key.as_str().unwrap_or_default();
            cleaned.insert(key, non_empty(value, &format!("mapping {index} {key}"))?);
        }
        let mut take = |key: &str| cleaned.remove(key).unwrap_or_default();
        let mapping = Mapping {
            mapping_id: take("mapping_id"),
            register_id: take("register_id"),
            collection: take("collection"),
            skill_ref: take("skill_ref"),
            owner_role: take("owner_role"),
            review_question: take("review_question"),
        };
        if !seen.insert(mapping.mapping_id.clone()) {
            return Err(Error::monitor(
                "Source-to-skill map contains duplicate mapping IDs.",
            ));
        }
        result.push(mapping);
    }
    Ok(result)
}

fn candidate(mapping: &Mapping) -> Json {
    Json::obj([
        ("mapping_id", Json::str(&mapping.mapping_id)),
        ("skill_ref", Json::str(&mapping.skill_ref)),
        ("owner_role", Json::str(&mapping.owner_role)),
        ("review_question", Json::str(&mapping.review_question)),
        (
            "mapping_basis",
            Json::str("exact_register_id_and_collection"),
        ),
    ])
}

fn source_bound_item_id(digests: &Json, change_kind: &str, identity: Json) -> String {
    let id = sha256_json(&Json::obj([
        ("change_kind", Json::str(change_kind)),
        ("identity", identity),
        ("sources", digests.clone()),
    ]));
    format!("impact:{}", &id[..24])
}

fn run_status_for(items: &[Json]) -> &'static str {
    if items
        .iter()
        .any(|item| is_text(item.at("state"), "BLOCKED"))
    {
        "BLOCKED"
    } else if items.is_empty() {
        "NO_CHANGE_DETECTED"
    } else {
        "REVIEW_REQUIRED"
    }
}

fn str_list(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|v| Json::str(v)).collect())
}

fn compilation(title: &BaselineTitle) -> Json {
    Json::obj([
        ("number", Json::str(&title.compilation_number)),
        ("date", Json::str(&title.compilation_date)),
    ])
}

/// Build the impact queue from the three sources, each read exactly once.
pub fn compare(
    baseline_path: &PyPath,
    observation_path: &PyPath,
    mapping_path: &PyPath,
) -> Result<Json> {
    let baseline_source =
        SourceSnapshot::capture(baseline_path, "baseline source index", MAX_JSON_BYTES)?;
    let (mut titles, baseline_raw) = load_baseline(&baseline_source)?;
    let expected_ids: HashSet<String> = titles.iter().map(|t| t.register_id.clone()).collect();
    let observation_source =
        SourceSnapshot::capture(observation_path, "Register observation", MAX_JSON_BYTES)?;
    let observation = load_observation(&observation_source, &expected_ids)?;
    let mapping_source =
        SourceSnapshot::capture(mapping_path, "source-to-skill map", MAX_JSON_BYTES)?;
    let mappings = load_mapping(&mapping_source)?;
    let digests = Json::obj([
        ("baseline", Json::str(&baseline_source.sha256)),
        ("observation", Json::str(&observation_source.sha256)),
        ("mapping", Json::str(&mapping_source.sha256)),
    ]);
    let observations: HashMap<&str, &Observed> = observation
        .items
        .iter()
        .map(|item| (item.register_id.as_str(), item))
        .collect();
    let mut items: Vec<Json> = Vec::new();
    if !observation.complete {
        items.push(Json::obj([
            (
                "item_id",
                Json::str(&source_bound_item_id(
                    &digests,
                    "INCOMPLETE_SCOPE",
                    Json::obj([("scope", Json::str("observation"))]),
                )),
            ),
            ("state", Json::str("BLOCKED")),
            ("change_kind", Json::str("INCOMPLETE_SCOPE")),
            ("source", Json::Null),
            ("impact_candidates", Json::Array(Vec::new())),
            ("mapping_status", Json::str("NOT_EVALUATED")),
            (
                "limitations",
                str_list(&[
                    "The observation scope is incomplete; no unchanged result can be relied on.",
                ]),
            ),
        ]));
    }
    titles.sort_by(|a, b| (&a.register_id, &a.collection).cmp(&(&b.register_id, &b.collection)));
    for title in &titles {
        let Some(observed) = observations.get(title.register_id.as_str()) else {
            let identity = Json::obj([
                ("register_id", Json::str(&title.register_id)),
                ("collection", Json::str(&title.collection)),
            ]);
            items.push(Json::obj([
                (
                    "item_id",
                    Json::str(&source_bound_item_id(
                        &digests,
                        "MISSING_OBSERVATION",
                        identity,
                    )),
                ),
                ("state", Json::str("BLOCKED")),
                ("change_kind", Json::str("MISSING_OBSERVATION")),
                (
                    "source",
                    Json::obj([
                        ("register_id", Json::str(&title.register_id)),
                        ("collection", Json::str(&title.collection)),
                        ("title", Json::str(&title.name)),
                        ("baseline_compilation", compilation(title)),
                        ("observed_compilation", Json::Null),
                        ("evidence_url", Json::str(&title.register_page)),
                    ]),
                ),
                ("impact_candidates", Json::Array(Vec::new())),
                ("mapping_status", Json::str("NOT_EVALUATED")),
                (
                    "limitations",
                    str_list(&[
                        "The expected source was not observed; currency cannot be assessed.",
                    ]),
                ),
            ]));
            continue;
        };
        if observed.collection != title.collection {
            return Err(Error::monitor(format!(
                "Observation collection does not match baseline for {}.",
                title.register_id
            )));
        }
        if observed.state == "UNCHANGED" && title.version_is_current {
            continue;
        }
        let blocked = matches!(
            observed.state.as_str(),
            "CURRENT_NO_PUBLISHED_COMPILATION" | "LOOKUP_FAILED"
        ) || !title.version_is_current;
        let candidates: Vec<Json> = mappings
            .iter()
            .filter(|m| m.register_id == title.register_id && m.collection == title.collection)
            .map(candidate)
            .collect();
        let change_kind = if title.version_is_current {
            observed.state.as_str()
        } else {
            "BASELINE_NOT_CURRENT"
        };
        let observed_compilation = if observed.state == "SUPERSEDED" {
            Json::obj([
                ("number", observed.number.clone()),
                ("date", observed.date.clone()),
                ("document_id", observed.document_id.clone()),
            ])
        } else {
            Json::Null
        };
        let identity = Json::obj([
            ("register_id", Json::str(&title.register_id)),
            ("collection", Json::str(&title.collection)),
            ("observed_state", Json::str(&observed.state)),
        ]);
        let mapping_status = if candidates.is_empty() {
            "UNMAPPED_SOURCE"
        } else {
            "MAPPED"
        };
        items.push(Json::obj([
            (
                "item_id",
                Json::str(&source_bound_item_id(&digests, change_kind, identity)),
            ),
            ("state", Json::str(if blocked { "BLOCKED" } else { "OPEN" })),
            ("change_kind", Json::str(change_kind)),
            (
                "source",
                Json::obj([
                    ("register_id", Json::str(&title.register_id)),
                    ("collection", Json::str(&title.collection)),
                    ("title", Json::str(&title.name)),
                    ("baseline_compilation", compilation(title)),
                    ("observed_compilation", observed_compilation),
                    ("evidence_url", observed.evidence_url.clone()),
                ]),
            ),
            ("impact_candidates", Json::Array(candidates)),
            ("mapping_status", Json::str(mapping_status)),
            (
                "limitations",
                str_list(&[
                    "A source-version state does not establish the legal effect of a change.",
                    "This item is not tax advice and does not update any workflow.",
                ]),
            ),
        ]));
    }
    let key = |item: &Json| {
        (
            !is_text(item.at("state"), "BLOCKED"),
            item.at("change_kind")
                .as_str()
                .unwrap_or_default()
                .to_string(),
            item.at("item_id").as_str().unwrap_or_default().to_string(),
        )
    };
    items.sort_by_key(key);
    let run_status = run_status_for(&items);
    let run_id = format!("sha256:{}", sha256_json(&digests));
    let prefixed = |s: &str| Json::str(&format!("sha256:{s}"));
    let mut queue = Json::obj([
        ("schema_version", Json::str("au-tax-impact-queue.v2")),
        ("run_id", Json::str(&run_id)),
        ("mode", Json::str("synthetic")),
        ("run_status", Json::str(run_status)),
        (
            "source_digests",
            Json::obj([
                ("baseline", prefixed(&baseline_source.sha256)),
                ("observation", prefixed(&observation_source.sha256)),
                ("mapping", prefixed(&mapping_source.sha256)),
            ]),
        ),
        (
            "baseline",
            Json::obj([
                ("id", prefixed(&baseline_source.sha256)),
                ("retrieved", baseline_raw.at("retrieved").clone()),
                ("source", baseline_raw.at("source").clone()),
            ]),
        ),
        (
            "observation",
            Json::obj([
                ("id", prefixed(&observation_source.sha256)),
                ("observed_at", observation.observed_at.clone()),
                ("complete", Json::Bool(observation.complete)),
            ]),
        ),
        ("items", Json::Array(items)),
    ]);
    let digest = format!("sha256:{}", sha256_json(&queue));
    if let Json::Object(pairs) = &mut queue {
        pairs.push((PyStr::Text("queue_digest".into()), Json::str(&digest)));
    }
    Ok(queue)
}

/// `f"{value}"` for the scalar values a validated queue holds.
pub fn py_format(value: &Json) -> String {
    match value {
        Json::Str(s) => s.as_str().unwrap_or_default().to_string(),
        Json::Bool(true) => "True".into(),
        Json::Bool(false) => "False".into(),
        Json::Null => "None".into(),
        other => dumps(other, Layout::Canonical),
    }
}

fn md(value: &Json) -> String {
    safe_markdown(value.as_str().unwrap_or_default())
}

pub fn render_markdown(queue: &Json) -> String {
    let mut lines: Vec<String> = vec![
        "# AU Tax Change Impact Queue".into(),
        String::new(),
        format!("**Run status: {}**", py_format(queue.at("run_status"))),
        format!("Queue digest: `{}`", py_format(queue.at("queue_digest"))),
        String::new(),
        "This is a synthetic metadata-review queue. It does not establish current law, legal effect, tax advice, a workflow update, or a client action.".into(),
        String::new(),
        format!("- Baseline source: {}", md(queue.at("baseline").at("source"))),
        format!("- Baseline retrieved: {}", py_format(queue.at("baseline").at("retrieved"))),
        format!("- Observation complete: {}", py_format(queue.at("observation").at("complete"))),
        String::new(),
        "## Open items".into(),
        String::new(),
    ];
    let items = match queue.at("items") {
        Json::Array(items) => items.as_slice(),
        _ => &[],
    };
    if items.is_empty() {
        lines.push("No changed items were identified within the complete synthetic observation scope. This is not a statement about live law.".into());
    }
    for item in items {
        lines.push(format!(
            "### {}: {}",
            py_format(item.at("state")),
            py_format(item.at("change_kind"))
        ));
        lines.push(String::new());
        let source = item.at("source");
        if !matches!(source, Json::Null) {
            let baseline = source.at("baseline_compilation");
            lines.push(format!(
                "- Source: {} ({}, {})",
                md(source.at("title")),
                md(source.at("register_id")),
                md(source.at("collection"))
            ));
            lines.push(format!(
                "- Baseline compilation: {} dated {}",
                md(baseline.at("number")),
                py_format(baseline.at("date"))
            ));
            lines.push(format!("- Evidence: {}", md(source.at("evidence_url"))));
            let observed = source.at("observed_compilation");
            if observed.truthy() {
                lines.push(format!(
                    "- Observed compilation: {} dated {} ({})",
                    md(observed.at("number")),
                    py_format(observed.at("date")),
                    md(observed.at("document_id"))
                ));
            }
        }
        lines.push(format!(
            "- Mapping status: {}",
            py_format(item.at("mapping_status"))
        ));
        if let Json::Array(candidates) = item.at("impact_candidates") {
            for candidate in candidates {
                lines.push(format!(
                    "- Review candidate ({}): {}",
                    md(candidate.at("skill_ref")),
                    md(candidate.at("review_question"))
                ));
            }
        }
        if let Json::Array(limitations) = item.at("limitations") {
            for limitation in limitations {
                lines.push(format!("- Limitation: {}", md(limitation)));
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Stage and commit the JSON/Markdown pair, restoring the old pair on failure.
pub fn write_queue(queue: &Json, output_dir: &PyPath) -> Result<(PyPath, PyPath)> {
    write_queue_files(
        &format!("{}\n", dumps(queue, Layout::Indented)),
        &render_markdown(queue),
        output_dir,
        "impact-queue",
        false,
    )
}

fn validate_compilation(value: &Json, field: &str, observed: bool) -> Result<()> {
    let shape: &[&str] = if observed {
        &["number", "date", "document_id"]
    } else {
        &["number", "date"]
    };
    if !value.has_exactly(shape) {
        return Err(Error::monitor(format!("{field} has an invalid shape.")));
    }
    non_empty(value.at("number"), &format!("{field} number"))?;
    iso_date(value.at("date"), &format!("{field} date"))?;
    if observed {
        non_empty(value.at("document_id"), &format!("{field} document_id"))?;
    }
    Ok(())
}

fn validate_source(value: &Json, item_index: usize, change_kind: &str) -> Result<()> {
    let field = format!("Impact queue item {item_index} source");
    if change_kind == "INCOMPLETE_SCOPE" {
        if !matches!(value, Json::Null) {
            return Err(Error::monitor(format!(
                "{field} must be null for INCOMPLETE_SCOPE."
            )));
        }
        return Ok(());
    }
    if !value.has_exactly(&SOURCE_FIELDS) {
        return Err(Error::monitor(format!("{field} has an invalid shape.")));
    }
    for name in ["register_id", "collection", "title"] {
        non_empty(value.at(name), &format!("{field} {name}"))?;
    }
    https_url(value.at("evidence_url"), &format!("{field} evidence_url"))?;
    validate_compilation(
        value.at("baseline_compilation"),
        &format!("{field} baseline_compilation"),
        false,
    )?;
    let observed = value.at("observed_compilation");
    let has_observed = !matches!(observed, Json::Null);
    if has_observed {
        validate_compilation(observed, &format!("{field} observed_compilation"), true)?;
    }
    if change_kind == "SUPERSEDED" && !has_observed {
        return Err(Error::monitor(format!(
            "{field} must include observed_compilation for SUPERSEDED."
        )));
    }
    if !matches!(change_kind, "SUPERSEDED" | "BASELINE_NOT_CURRENT") && has_observed {
        return Err(Error::monitor(format!(
            "{field} observed_compilation is unsupported for {change_kind}."
        )));
    }
    Ok(())
}

fn validate_candidates(value: &Json, item_index: usize) -> Result<usize> {
    let Json::Array(candidates) = value else {
        return Err(Error::monitor(format!(
            "Impact queue item {item_index} impact_candidates must be a list."
        )));
    };
    let mut mapping_ids = HashSet::new();
    for (candidate_index, candidate) in candidates.iter().enumerate().map(|(i, c)| (i + 1, c)) {
        let field = format!("Impact queue item {item_index} candidate {candidate_index}");
        if !candidate.has_exactly(&CANDIDATE_FIELDS) {
            return Err(Error::monitor(format!("{field} has an invalid shape.")));
        }
        for name in ["mapping_id", "skill_ref", "owner_role", "review_question"] {
            non_empty(candidate.at(name), &format!("{field} {name}"))?;
        }
        if !is_text(
            candidate.at("mapping_basis"),
            "exact_register_id_and_collection",
        ) {
            return Err(Error::monitor(format!(
                "{field} has an unsupported mapping_basis."
            )));
        }
        // Validated above as a string without surrogates.
        let mapping_id = candidate.at("mapping_id").as_str().unwrap_or_default();
        if !mapping_ids.insert(mapping_id) {
            return Err(Error::monitor(format!(
                "Impact queue item {item_index} contains duplicate candidate mapping IDs."
            )));
        }
    }
    Ok(candidates.len())
}

fn validate_queue_evidence(queue: &Json) -> Result<(BTreeSet<String>, String)> {
    if !is_text(queue.at("schema_version"), "au-tax-impact-queue.v2") {
        return Err(Error::monitor(
            "Queue or decision schema version is unsupported.",
        ));
    }
    if !is_text(queue.at("mode"), "synthetic") {
        return Err(Error::monitor(
            "Only synthetic impact queues can be validated.",
        ));
    }
    sha256_id(queue.at("run_id"), "Impact queue run_id")?;
    sha256_id(queue.at("queue_digest"), "Impact queue queue_digest")?;
    if one_of(
        queue.at("run_status"),
        &["BLOCKED", "REVIEW_REQUIRED", "NO_CHANGE_DETECTED"],
    )
    .is_none()
    {
        return Err(Error::monitor(
            "Impact queue has an unsupported run_status.",
        ));
    }
    let digests = queue.at("source_digests");
    if !digests.has_exactly(&SOURCE_DIGEST_FIELDS) {
        return Err(Error::monitor(
            "Impact queue source_digests has an invalid shape.",
        ));
    }
    for name in SOURCE_DIGEST_FIELDS {
        sha256_id(
            digests.at(name),
            &format!("Impact queue source_digests {name}"),
        )?;
    }
    let baseline = queue.at("baseline");
    if !baseline.has_exactly(&BASELINE_QUEUE_FIELDS) {
        return Err(Error::monitor(
            "Impact queue baseline has an invalid shape.",
        ));
    }
    sha256_id(baseline.at("id"), "Impact queue baseline id")?;
    iso_date(baseline.at("retrieved"), "Impact queue baseline retrieved")?;
    non_empty(baseline.at("source"), "Impact queue baseline source")?;
    if baseline.at("id") != digests.at("baseline") {
        return Err(Error::monitor(
            "Impact queue baseline id must equal the baseline source digest.",
        ));
    }
    let observation = queue.at("observation");
    if !observation.has_exactly(&OBSERVATION_QUEUE_FIELDS) {
        return Err(Error::monitor(
            "Impact queue observation has an invalid shape.",
        ));
    }
    sha256_id(observation.at("id"), "Impact queue observation id")?;
    let observed_at = iso_timestamp(observation.at("observed_at"), "impact queue observed_at")?;
    let Json::Bool(complete) = *observation.at("complete") else {
        return Err(Error::monitor(
            "Impact queue observation complete must be a boolean.",
        ));
    };
    if observation.at("id") != digests.at("observation") {
        return Err(Error::monitor(
            "Impact queue observation id must equal the observation source digest.",
        ));
    }
    let Json::Array(items) = queue.at("items") else {
        return Err(Error::monitor("Impact queue items must be a list."));
    };
    let mut open_items = BTreeSet::new();
    let mut queue_item_ids = HashSet::new();
    let mut incomplete_scope_count = 0;
    let mut missing_observation_count = 0;
    for (index, item) in items.iter().enumerate().map(|(i, t)| (i + 1, t)) {
        if !item.has_exactly(&ITEM_FIELDS) {
            return Err(Error::monitor(format!(
                "Impact queue item {index} has an invalid shape."
            )));
        }
        let item_id = non_empty(
            item.at("item_id"),
            &format!("impact queue item {index} item_id"),
        )?;
        if !queue_item_ids.insert(item_id.clone()) {
            return Err(Error::monitor("Impact queue contains duplicate item IDs."));
        }
        let Some(state) = one_of(item.at("state"), &["OPEN", "BLOCKED"]) else {
            return Err(Error::monitor(format!(
                "Impact queue item {index} has an unsupported state."
            )));
        };
        let rule = item
            .at("change_kind")
            .as_str()
            .and_then(|kind| Some((kind, change_kind_rule(kind)?)));
        let Some((change_kind, (expected_state, supported))) = rule else {
            return Err(Error::monitor(format!(
                "Impact queue item {index} has an unsupported change_kind."
            )));
        };
        match change_kind {
            "INCOMPLETE_SCOPE" => incomplete_scope_count += 1,
            "MISSING_OBSERVATION" => missing_observation_count += 1,
            _ => {}
        }
        validate_source(item.at("source"), index, change_kind)?;
        let candidate_count = validate_candidates(item.at("impact_candidates"), index)?;
        let Some(mapping_status) = one_of(item.at("mapping_status"), &MAPPING_STATUSES) else {
            return Err(Error::monitor(format!(
                "Impact queue item {index} has an unsupported mapping_status."
            )));
        };
        if mapping_status == "MAPPED" && candidate_count == 0 {
            return Err(Error::monitor(format!(
                "Impact queue item {index} cannot be MAPPED without a candidate."
            )));
        }
        if mapping_status != "MAPPED" && candidate_count > 0 {
            return Err(Error::monitor(format!(
                "Impact queue item {index} with candidates must be MAPPED."
            )));
        }
        if state != expected_state {
            return Err(Error::monitor(format!(
                "Impact queue item {index} change_kind must remain {expected_state}."
            )));
        }
        if !supported.contains(&mapping_status) {
            return Err(Error::monitor(format!(
                "Impact queue item {index} mapping_status is unsupported for its change_kind."
            )));
        }
        let Json::Array(limitations) = item.at("limitations") else {
            return Err(Error::monitor(format!(
                "Impact queue item {index} limitations must be a list."
            )));
        };
        for (limitation_index, limitation) in
            limitations.iter().enumerate().map(|(i, l)| (i + 1, l))
        {
            non_empty(
                limitation,
                &format!("impact queue item {index} limitation {limitation_index}"),
            )?;
        }
        if state == "OPEN" {
            open_items.insert(item_id);
        }
    }
    let expected_incomplete = usize::from(!complete);
    if incomplete_scope_count != expected_incomplete || (complete && missing_observation_count > 0)
    {
        return Err(Error::monitor(
            "Impact queue observation complete is inconsistent with its scope items.",
        ));
    }
    if queue.at("run_status").as_str() != Some(run_status_for(items)) {
        return Err(Error::monitor(
            "Impact queue run_status does not match its items.",
        ));
    }
    Ok((open_items, observed_at))
}

/// Validate a loaded queue completely and return its open items and observation time.
///
/// Every field is validated before it is hashed, then the run ID and queue
/// digest are recomputed, so an edited item cannot keep the digest of the
/// queue it came from.
pub fn verify_queue_integrity(queue: &Json) -> Result<(BTreeSet<String>, String)> {
    let (open_items, observed_at) = validate_queue_evidence(queue)?;
    let digests = queue.at("source_digests");
    let raw = |name: &str| {
        let text = digests.at(name).as_str().unwrap_or_default();
        Json::str(text.strip_prefix("sha256:").unwrap_or(text))
    };
    let expected_run_id = format!(
        "sha256:{}",
        sha256_json(&Json::obj([
            ("baseline", raw("baseline")),
            ("mapping", raw("mapping")),
            ("observation", raw("observation")),
        ]))
    );
    if queue.at("run_id").as_str() != Some(expected_run_id.as_str()) {
        return Err(Error::monitor(
            "Impact queue run_id does not match its exact source digests.",
        ));
    }
    let Json::Object(pairs) = queue else {
        unreachable!("validated as an object")
    };
    let payload = Json::Object(
        pairs
            .iter()
            .filter(|(k, _)| k.as_str() != Some("queue_digest"))
            .cloned()
            .collect(),
    );
    let expected_digest = format!("sha256:{}", sha256_json(&payload));
    if queue.at("queue_digest").as_str() != Some(expected_digest.as_str()) {
        return Err(Error::monitor(
            "Impact queue queue_digest does not match its complete review evidence.",
        ));
    }
    Ok((open_items, observed_at))
}

/// Refuse a Markdown companion that does not render exactly from the JSON queue.
pub fn verify_queue_markdown(queue_path: &PyPath, queue: &Json) -> Result<()> {
    let markdown_path = queue_path.with_suffix(".md").map_err(Error::monitor)?;
    if !crate::pyos::exists(&markdown_path.display()) {
        return Ok(());
    }
    let expected = render_markdown(queue);
    // Allow CRLF bytes while preserving read_text's universal newline handling.
    let limit = (expected.len() + expected.matches('\n').count()) as u64;
    let mismatch = "Queue Markdown does not match the JSON evidence; regenerate the pair.";
    let source = match SourceSnapshot::capture(&markdown_path, "Queue Markdown", limit) {
        Ok(source) => source,
        Err(Error::Monitor {
            too_large: true, ..
        }) => return Err(Error::monitor(mismatch)),
        Err(_) => {
            return Err(Error::monitor(
                "Queue Markdown could not be read; regenerate the pair.",
            ));
        }
    };
    let Ok(markdown) = source.text("Queue Markdown") else {
        return Err(Error::monitor(
            "Queue Markdown could not be read; regenerate the pair.",
        ));
    };
    if py::universal_newlines(&markdown) != expected {
        return Err(Error::monitor(mismatch));
    }
    Ok(())
}

pub fn validate_review(queue_path: &PyPath, decision_path: &PyPath) -> Result<Json> {
    let queue = load_json_exact(
        &SourceSnapshot::capture(queue_path, "impact queue", MAX_JSON_BYTES)?,
        &QUEUE_FIELDS,
        "impact queue",
    )?;
    let decision = load_json_exact(
        &SourceSnapshot::capture(decision_path, "technical review decision", MAX_JSON_BYTES)?,
        &DECISION_FIELDS,
        "technical review decision",
    )?;
    if !is_text(decision.at("schema_version"), "au-tax-technical-review.v2") {
        return Err(Error::monitor(
            "Queue or decision schema version is unsupported.",
        ));
    }
    let (open_items, observed_at) = verify_queue_integrity(&queue)?;
    sha256_id(decision.at("run_id"), "Technical review run_id")?;
    sha256_id(decision.at("queue_digest"), "Technical review queue_digest")?;
    if queue.at("run_id") != decision.at("run_id") {
        return Err(Error::monitor(
            "Technical review decision must refer to the exact queue run_id.",
        ));
    }
    if queue.at("queue_digest") != decision.at("queue_digest") {
        return Err(Error::monitor(
            "Technical review decision must refer to the exact queue_digest.",
        ));
    }
    non_empty(decision.at("reviewer_ref"), "technical review reviewer_ref")?;
    let reviewed_at = iso_timestamp(decision.at("reviewed_at"), "technical review reviewed_at")?;
    let reviewed = timestamp(&reviewed_at, "technical review reviewed_at")?;
    let observed = timestamp(&observed_at, "impact queue observed_at")?;
    if reviewed.utc_micros() < observed.utc_micros() {
        return Err(Error::monitor(
            "Technical review reviewed_at cannot predate the queue observation.",
        ));
    }
    let entries = match decision.at("decisions") {
        Json::Array(entries) if !entries.is_empty() => entries,
        _ => {
            return Err(Error::monitor(
                "Technical review decision must include at least one decision.",
            ));
        }
    };
    let mut seen = BTreeSet::new();
    for item in entries {
        if !item.has_exactly(&DECISION_ENTRY_FIELDS) {
            return Err(Error::monitor(
                "Each technical decision must contain exactly item_id, decision, rationale, and evidence_note.",
            ));
        }
        let item_id = non_empty(item.at("item_id"), "technical decision item_id")?;
        if !open_items.contains(&item_id) || seen.contains(&item_id) {
            return Err(Error::monitor(
                "Technical decision references an unknown, blocked, or duplicate item.",
            ));
        }
        if one_of(item.at("decision"), &ALLOWED_DECISIONS).is_none() {
            return Err(Error::monitor("Technical decision is not allowlisted."));
        }
        non_empty(item.at("rationale"), "technical decision rationale")?;
        non_empty(item.at("evidence_note"), "technical decision evidence_note")?;
        seen.insert(item_id);
    }
    let undecided_count = open_items.difference(&seen).count();
    verify_queue_markdown(queue_path, &queue)?;
    let status = if undecided_count == 0 {
        "DECISION_RECORDED"
    } else {
        "PARTIAL_DECISION_RECORDED"
    };
    Ok(Json::obj([
        (
            "schema_version",
            Json::str("au-tax-review-decision-validation.v2"),
        ),
        ("run_id", queue.at("run_id").clone()),
        ("queue_digest", queue.at("queue_digest").clone()),
        ("mode", Json::str("synthetic")),
        ("status", Json::str(status)),
        ("decision_count", Json::Int(seen.len().into())),
        ("undecided_count", Json::Int(undecided_count.into())),
        (
            "limitation",
            Json::str(
                "Validation records structurally valid human decisions only; it does not establish legal effect, change a skill, notify anyone, or produce tax advice.",
            ),
        ),
    ]))
}
