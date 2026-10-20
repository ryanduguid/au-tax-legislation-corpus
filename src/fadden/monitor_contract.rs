//! `fadden/export_monitor_contract.py`: project corpus metadata into the
//! monitor's fixed, review-only inputs.
//!
//! A producer-side adapter: it never calls the Federal Register, calculates an
//! impact or writes a review decision. The caller supplies structured
//! observation facts and receives the monitor's reduced baseline and its exact
//! observation contract, published together under a directory lock, with the
//! previous pair restored if either replacement fails.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::{Exc, Run};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::py;
use crate::pyjson::{self, DecodeError, Json, PyStr};
use crate::pyos::{self, OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyvalue::{self, dumps_with};

pub const COLLECTIONS: [&str; 3] = ["Act", "LegislativeInstrument", "NotifiableInstrument"];
const OBSERVATION_STATES: [&str; 5] = [
    "UNCHANGED",
    "SUPERSEDED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "NO_LONGER_IN_FORCE",
    "LOOKUP_FAILED",
];
const BASELINE_FIELDS: [&str; 10] = [
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
const OBSERVATION_FIELDS: [&str; 10] = [
    "register_id",
    "collection",
    "state",
    "observed_compilation_number",
    "observed_compilation_date",
    "observed_register_document_id",
    "current_version_start",
    "evidence_url",
    "checked_at",
    "error_category",
];
pub const CONTENT_KINDS: [&str; 2] = ["metadata-response", "source-document"];
pub const CONTENT_MEDIA_TYPES: [&str; 4] = [
    "application/json",
    "application/pdf",
    "application/xhtml+xml",
    "text/html",
];
const PUBLISH_LOCK_FILENAME: &str = ".monitor-contract.publish.lock";
const PUBLISH_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const PUBLISH_LOCK_RETRY: Duration = Duration::from_millis(50);

type P = Vec<u32>;

/// `ContractError(message)`.
pub fn contract_error(message: impl Into<String>) -> Exc {
    Exc::Value {
        class: "fadden.export_monitor_contract.ContractError",
        message: message.into(),
    }
}

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

fn is_space(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(py::is_space)
}

/// The text of a `str` value, its code points kept.
fn points(value: &Json) -> Option<P> {
    match value {
        Json::Str(s) => Some(s.code_points()),
        _ => None,
    }
}

/// `_non_empty(value, field)`: the stripped text of a non-empty `str`
/// without control characters.
fn non_empty(value: &Json, field: &str) -> Result<P, Exc> {
    let empty = || contract_error(format!("{field} must be a non-empty string."));
    let text = points(value).ok_or_else(empty)?;
    let start = text.iter().position(|&cp| !is_space(cp));
    let end = text.iter().rposition(|&cp| !is_space(cp));
    let (Some(start), Some(end)) = (start, end) else {
        return Err(empty());
    };
    let text = text[start..=end].to_vec();
    if text.iter().any(|&cp| cp < 32 || cp == 127) {
        return Err(contract_error(format!(
            "{field} must not contain control characters."
        )));
    }
    Ok(text)
}

fn text_json(points: P) -> Json {
    Json::Str(PyStr::from_code_points(points))
}

/// Whether `points` are `\d` digits, as `re` matches them in a `str`.
fn digits(points: &[u32]) -> bool {
    points
        .iter()
        .all(|&cp| char::from_u32(cp).is_some_and(py::is_decimal))
}

/// `YYYY-MM-DD` as `re.fullmatch(r"\d{4}-\d{2}-\d{2}")` and a valid
/// `date.fromisoformat`, which takes only ASCII digits.
fn iso_date_points(text: &[u32]) -> bool {
    if text.len() != 10 || text[4] != u32::from('-') || text[7] != u32::from('-') {
        return false;
    }
    let (y, m, d) = (&text[0..4], &text[5..7], &text[8..10]);
    if !digits(y) || !digits(m) || !digits(d) {
        return false;
    }
    let ascii = |part: &[u32]| -> Option<i64> {
        part.iter()
            .map(|&cp| char::from_u32(cp).filter(char::is_ascii_digit))
            .collect::<Option<String>>()
            .and_then(|s| s.parse().ok())
    };
    match (ascii(y), ascii(m), ascii(d)) {
        (Some(y), Some(m), Some(d)) => py::Date::new(y, m, d).is_ok(),
        _ => false,
    }
}

/// `_date(value, field, nullable=...)`.
fn date(value: &Json, field: &str, nullable: bool) -> Result<Option<P>, Exc> {
    if nullable && matches!(value, Json::Null) {
        return Ok(None);
    }
    let text = non_empty(value, field)?;
    if !iso_date_points(&text) {
        return Err(contract_error(format!(
            "{field} must be an ISO calendar date."
        )));
    }
    Ok(Some(text))
}

fn required_date(value: &Json, field: &str) -> Result<P, Exc> {
    Ok(date(value, field, false)?.expect("a date that is not nullable"))
}

/// `_utc_timestamp(value, field)`: `YYYY-MM-DDTHH:MM:SS[.ffffff]Z`, a real instant.
fn utc_timestamp(value: &Json, field: &str) -> Result<P, Exc> {
    let text = non_empty(value, field)?;
    let invalid = || {
        contract_error(format!(
            "{field} must be an explicit UTC timestamp ending in Z."
        ))
    };
    let at = |i: usize, c: char| text.get(i) == Some(&u32::from(c));
    let shaped = text.len() >= 20
        && at(4, '-')
        && at(7, '-')
        && at(10, 'T')
        && at(13, ':')
        && at(16, ':')
        && text.last() == Some(&u32::from('Z'))
        && digits(&text[0..4])
        && digits(&text[5..7])
        && digits(&text[8..10])
        && digits(&text[11..13])
        && digits(&text[14..16])
        && digits(&text[17..19])
        && (text.len() == 20
            || (at(19, '.')
                && (22..=27).contains(&text.len())
                && digits(&text[20..text.len() - 1])));
    if !shaped || !iso_date_points(&text[0..10]) {
        return Err(invalid());
    }
    // datetime.fromisoformat takes ASCII digits only.
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        text[range]
            .iter()
            .map(|&cp| char::from_u32(cp).filter(char::is_ascii_digit))
            .collect::<Option<String>>()
            .and_then(|s| s.parse().ok())
    };
    let fraction = if text.len() > 20 {
        number(20..text.len() - 1)
    } else {
        Some(0)
    };
    let valid = match (number(11..13), number(14..16), number(17..19), fraction) {
        (Some(h), Some(m), Some(s), Some(_)) if h < 24 => m < 60 && s < 60,
        // 24:00:00 is midnight of the next day, which must still be a date.
        (Some(24), Some(0), Some(0), Some(0)) => text[0..10] != pts("9999-12-31")[..],
        _ => false,
    };
    if !valid {
        return Err(invalid());
    }
    Ok(text)
}

/// `_https_url(value, field)`: an `https` URL with a host and no credentials.
fn https_url(value: &Json, field: &str) -> Result<P, Exc> {
    let text = non_empty(value, field)?;
    let invalid = || contract_error(format!("{field} must be an https URL."));
    // A lone surrogate is no URL delimiter; it parses as any other character.
    let shown: String = text
        .iter()
        .map(|&cp| char::from_u32(cp).unwrap_or('\u{fffd}'))
        .collect();
    let parts = crate::pyurl::urlsplit(&shown).map_err(|_| invalid())?;
    let hostname = crate::pyurl::hostname(&parts.netloc);
    crate::pyurl::port(&parts.netloc).map_err(|_| invalid())?;
    if parts.scheme.to_lowercase() != "https"
        || hostname.is_none_or(|h| h.is_empty())
        || parts.netloc.contains('@')
        || parts.netloc.contains('\\')
        || parts.netloc.chars().any(py::is_space)
    {
        return Err(invalid());
    }
    Ok(text)
}

fn object<'a>(value: &'a Json, field: &str) -> Result<&'a [(PyStr, Json)], Exc> {
    match value {
        Json::Object(pairs) => Ok(pairs),
        _ => Err(contract_error(format!("{field} must be an object."))),
    }
}

fn member<'a>(pairs: &'a [(PyStr, Json)], name: &str) -> Option<&'a Json> {
    pairs
        .iter()
        .find(|(k, _)| k.as_str() == Some(name))
        .map(|(_, v)| v)
}

/// The member names of an object, as code points.
fn names(pairs: &[(PyStr, Json)]) -> BTreeSet<P> {
    pairs.iter().map(|(k, _)| k.code_points()).collect()
}

fn name_set(names: &[&str]) -> BTreeSet<P> {
    names.iter().map(|n| pts(n)).collect()
}

/// `_load_json(path, label)`: an object, duplicate members refused.
fn load_json(path: &str, label: &str) -> Result<Json, Exc> {
    let text = match crate::pyio::read_text(path) {
        Ok(text) => text,
        Err(crate::pyio::ReadError::Os(error)) => {
            return Err(contract_error(format!(
                "{label} is unreadable JSON: {}",
                error.text
            )));
        }
        // json.load lets a UnicodeDecodeError through.
        Err(error) => return Err(error.into()),
    };
    let value = pyjson::loads(&text).map_err(|error| match error {
        DecodeError::Duplicate => {
            contract_error(format!("{label} contains duplicate JSON members."))
        }
        DecodeError::Invalid(message) if !message.starts_with("Exceeds the limit") => {
            contract_error(format!("{label} is unreadable JSON: {message}"))
        }
        other => super::json_error(other),
    })?;
    object(&value, label)?;
    Ok(value)
}

/// `project_baseline(sources)`: the monitor's exact baseline from a corpus
/// `sources.json`.
pub fn project_baseline(sources: &Json) -> Result<Json, Exc> {
    let source = object(sources, "sources")?;
    for field in ["corpus", "retrieved", "source", "source_api", "titles"] {
        if member(source, field).is_none() {
            return Err(contract_error(format!("sources is missing {field}.")));
        }
    }
    let get = |name: &str| member(source, name).expect("checked above");
    let corpus = non_empty(get("corpus"), "sources corpus")?;
    let retrieved = required_date(get("retrieved"), "sources retrieved")?;
    let source_name = non_empty(get("source"), "sources source")?;
    if source_name != pts("Federal Register of Legislation") {
        return Err(contract_error(
            "sources source must be Federal Register of Legislation.",
        ));
    }
    let source_api = https_url(get("source_api"), "sources source_api")?;
    let raw_titles = match get("titles") {
        Json::Array(items) if !items.is_empty() => items,
        _ => {
            return Err(contract_error("sources titles must be a non-empty list."));
        }
    };
    let mut titles: Vec<(P, P, Json)> = Vec::new();
    let mut seen: BTreeSet<P> = BTreeSet::new();
    for (index, raw) in raw_titles.iter().enumerate() {
        let index = index + 1;
        let title = object(raw, &format!("sources title {index}"))?;
        let present = names(title);
        let mut missing: Vec<&str> = BASELINE_FIELDS
            .iter()
            .copied()
            .filter(|f| !present.contains(&pts(f)))
            .collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            return Err(contract_error(format!(
                "sources title {index} is missing required field(s): {}.",
                missing.join(", ")
            )));
        }
        let field = |name: &str| member(title, name).expect("checked above");
        let label = |name: &str| format!("sources title {index} {name}");
        let register_id = non_empty(field("register_id"), &label("register_id"))?;
        if !seen.insert(register_id.clone()) {
            return Err(contract_error(
                "sources contains duplicate register_id values.",
            ));
        }
        let collection = non_empty(field("collection"), &label("collection"))?;
        if !COLLECTIONS.iter().any(|c| pts(c) == collection) {
            return Err(contract_error(format!(
                "sources title {index} collection is unsupported."
            )));
        }
        let Json::Bool(current) = field("version_is_current") else {
            return Err(contract_error(format!(
                "sources title {index} version_is_current must be a boolean."
            )));
        };
        let current_start = date(
            field("current_version_start"),
            &label("current_version_start"),
            true,
        )?;
        if !current && current_start.is_none() {
            return Err(contract_error(format!(
                "sources title {index} current_version_start is required when version_is_current is false."
            )));
        }
        let projected = Json::Object(vec![
            (
                PyStr::Text("register_id".into()),
                text_json(register_id.clone()),
            ),
            (
                PyStr::Text("name".into()),
                text_json(non_empty(field("name"), &label("name"))?),
            ),
            (
                PyStr::Text("collection".into()),
                text_json(collection.clone()),
            ),
            (
                PyStr::Text("compilation_number".into()),
                text_json(non_empty(
                    field("compilation_number"),
                    &label("compilation_number"),
                )?),
            ),
            (
                PyStr::Text("compilation_date".into()),
                text_json(required_date(
                    field("compilation_date"),
                    &label("compilation_date"),
                )?),
            ),
            (
                PyStr::Text("version_is_current".into()),
                Json::Bool(*current),
            ),
            (
                PyStr::Text("current_version_start".into()),
                current_start.map_or(Json::Null, text_json),
            ),
            (
                PyStr::Text("retrieved".into()),
                text_json(required_date(field("retrieved"), &label("retrieved"))?),
            ),
            (
                PyStr::Text("source_url".into()),
                text_json(https_url(field("source_url"), &label("source_url"))?),
            ),
            (
                PyStr::Text("register_page".into()),
                text_json(https_url(field("register_page"), &label("register_page"))?),
            ),
        ]);
        titles.push((register_id, collection, projected));
    }
    titles.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    Ok(Json::Object(vec![
        (PyStr::Text("corpus".into()), text_json(corpus)),
        (PyStr::Text("retrieved".into()), text_json(retrieved)),
        (
            PyStr::Text("source".into()),
            Json::str("Federal Register of Legislation"),
        ),
        (PyStr::Text("source_api".into()), text_json(source_api)),
        (
            PyStr::Text("titles".into()),
            Json::Array(titles.into_iter().map(|t| t.2).collect()),
        ),
    ]))
}

/// One observation facts schema: its fields and what the observation carries.
struct Schema {
    top: &'static [&'static str],
    evidence_id: bool,
    content: bool,
    observation: &'static str,
}

fn schema(version: &[u32]) -> Option<Schema> {
    let v = pyvalue::points_lossy(version);
    match v.as_str() {
        "au-tax-register-observation-facts.v1" => Some(Schema {
            top: &["schema_version", "observed_at", "complete", "observations"],
            evidence_id: false,
            content: false,
            observation: "au-tax-register-observation.v1",
        }),
        "au-tax-register-observation-facts.v2" => Some(Schema {
            top: &[
                "schema_version",
                "observed_at",
                "scope_id",
                "complete",
                "observations",
            ],
            evidence_id: true,
            content: false,
            observation: "au-tax-register-observation.v2",
        }),
        "au-tax-register-observation-facts.v3" => Some(Schema {
            top: &[
                "schema_version",
                "observed_at",
                "scope_id",
                "complete",
                "observations",
            ],
            evidence_id: true,
            content: true,
            observation: "au-tax-register-observation.v3",
        }),
        _ => None,
    }
}

/// The conditional fields a state requires, in the order checked.
const CONDITIONAL: [&str; 5] = [
    "current_version_start",
    "error_category",
    "observed_compilation_date",
    "observed_compilation_number",
    "observed_register_document_id",
];

fn required_by(state: &str) -> &'static [&'static str] {
    match state {
        "SUPERSEDED" => &[
            "observed_compilation_number",
            "observed_compilation_date",
            "observed_register_document_id",
        ],
        "CURRENT_NO_PUBLISHED_COMPILATION" => &["current_version_start"],
        "LOOKUP_FAILED" => &["error_category"],
        _ => &[],
    }
}

fn in_set(value: &[u32], set: &[&str]) -> bool {
    set.iter().any(|s| pts(s) == value)
}

/// `project_observation(baseline, facts)`: validate structured facts and
/// produce the monitor's exact observation.
pub fn project_observation(baseline: &Json, facts: &Json) -> Result<Json, Exc> {
    let projected_baseline = project_baseline(baseline)?;
    let raw = object(facts, "observation facts")?;
    let Some(version) = member(raw, "schema_version") else {
        return Err(contract_error(
            "observation facts is missing schema_version.",
        ));
    };
    let version = non_empty(version, "observation facts schema_version")?;
    let Some(schema) = schema(&version) else {
        return Err(contract_error(
            "observation facts schema_version is unsupported.",
        ));
    };
    if names(raw) != name_set(schema.top) {
        return Err(contract_error("observation facts has an invalid shape."));
    }
    let get = |name: &str| member(raw, name).expect("the shape was checked");
    let scope_id = if schema.evidence_id {
        Some(non_empty(get("scope_id"), "observation facts scope_id")?)
    } else {
        None
    };
    let observed_at = utc_timestamp(get("observed_at"), "observation facts observed_at")?;
    let (Json::Bool(complete), Json::Array(items)) = (get("complete"), get("observations")) else {
        return Err(contract_error(
            "observation facts complete/observations fields are invalid.",
        ));
    };
    let Json::Object(baseline_pairs) = &projected_baseline else {
        unreachable!("project_baseline returns an object")
    };
    let Some(Json::Array(baseline_titles)) = member(baseline_pairs, "titles") else {
        unreachable!("project_baseline returns titles")
    };
    let expected: Vec<(P, P)> = baseline_titles
        .iter()
        .map(|t| {
            let field = |n: &str| t.get(n).and_then(points).unwrap_or_default();
            (field("register_id"), field("collection"))
        })
        .collect();
    let mut item_fields: Vec<&str> = OBSERVATION_FIELDS.to_vec();
    if schema.evidence_id {
        item_fields.push("evidence_id");
    }
    if schema.content {
        item_fields.extend(["content_sha256", "content_kind", "content_media_type"]);
    }
    let item_fields = name_set(&item_fields);
    let mut observations: Vec<(P, P, Json)> = Vec::new();
    let mut seen: BTreeSet<P> = BTreeSet::new();
    let mut seen_evidence: BTreeSet<P> = BTreeSet::new();
    for (index, value) in items.iter().enumerate() {
        let index = index + 1;
        let item = object(value, &format!("observation fact {index}"))?;
        if names(item) != item_fields {
            return Err(contract_error(format!(
                "observation fact {index} has an invalid shape."
            )));
        }
        let field = |name: &str| member(item, name).expect("the shape was checked");
        let label = |name: &str| format!("observation fact {index} {name}");
        let register_id = non_empty(field("register_id"), &label("register_id"))?;
        let Some((_, baseline_collection)) = expected.iter().find(|(id, _)| *id == register_id)
        else {
            return Err(contract_error(
                "observation facts contains a register_id outside the baseline scope.",
            ));
        };
        if !seen.insert(register_id.clone()) {
            return Err(contract_error(
                "observation facts contains duplicate register_id values.",
            ));
        }
        let collection = non_empty(field("collection"), &label("collection"))?;
        if collection != *baseline_collection {
            return Err(contract_error(format!(
                "observation fact {index} collection does not match baseline."
            )));
        }
        let state = non_empty(field("state"), &label("state"))?;
        if !in_set(&state, &OBSERVATION_STATES) {
            return Err(contract_error(format!(
                "observation fact {index} state is unsupported."
            )));
        }
        let state_text = pyvalue::points_lossy(&state);
        // Python checked these in set (string hash) order, which varied from
        // run to run; the binary checks them alphabetically.
        let required = required_by(&state_text);
        for name in CONDITIONAL {
            if !required.contains(&name) && !matches!(field(name), Json::Null) {
                return Err(contract_error(format!(
                    "{state_text} requires null {name}."
                )));
            }
        }
        let mut projected: Vec<(PyStr, Json)> = vec![
            (
                PyStr::Text("register_id".into()),
                text_json(register_id.clone()),
            ),
            (
                PyStr::Text("collection".into()),
                text_json(collection.clone()),
            ),
            (PyStr::Text("state".into()), text_json(state.clone())),
            (
                PyStr::Text("observed_compilation_number".into()),
                Json::Null,
            ),
            (PyStr::Text("observed_compilation_date".into()), Json::Null),
            (
                PyStr::Text("observed_register_document_id".into()),
                Json::Null,
            ),
            (PyStr::Text("current_version_start".into()), Json::Null),
            (
                PyStr::Text("evidence_url".into()),
                text_json(https_url(field("evidence_url"), &label("evidence_url"))?),
            ),
            (
                PyStr::Text("checked_at".into()),
                text_json(utc_timestamp(field("checked_at"), &label("checked_at"))?),
            ),
            (PyStr::Text("error_category".into()), Json::Null),
        ];
        let mut set = |name: &str, value: Json| match projected
            .iter_mut()
            .find(|(k, _)| k.as_str() == Some(name))
        {
            Some(slot) => slot.1 = value,
            None => projected.push((PyStr::Text(name.into()), value)),
        };
        if schema.evidence_id {
            let evidence_id = non_empty(field("evidence_id"), &label("evidence_id"))?;
            if !seen_evidence.insert(evidence_id.clone()) {
                return Err(contract_error(format!(
                    "observation fact {index} contains duplicate evidence_id."
                )));
            }
            set("evidence_id", text_json(evidence_id));
        }
        if schema.content {
            let digest = non_empty(field("content_sha256"), &label("content_sha256"))?;
            if !sha256_id(&digest) {
                return Err(contract_error(format!(
                    "observation fact {index} content_sha256 is invalid."
                )));
            }
            let kind = non_empty(field("content_kind"), &label("content_kind"))?;
            if !in_set(&kind, &CONTENT_KINDS) {
                return Err(contract_error(format!(
                    "observation fact {index} content_kind is unsupported."
                )));
            }
            let media = non_empty(field("content_media_type"), &label("content_media_type"))?;
            if !in_set(&media, &CONTENT_MEDIA_TYPES) {
                return Err(contract_error(format!(
                    "observation fact {index} content_media_type is unsupported."
                )));
            }
            set("content_sha256", text_json(digest));
            set("content_kind", text_json(kind));
            set("content_media_type", text_json(media));
        }
        match state_text.as_str() {
            "SUPERSEDED" => {
                set(
                    "observed_compilation_number",
                    text_json(non_empty(
                        field("observed_compilation_number"),
                        &label("observed_compilation_number"),
                    )?),
                );
                set(
                    "observed_compilation_date",
                    text_json(required_date(
                        field("observed_compilation_date"),
                        &label("observed_compilation_date"),
                    )?),
                );
                set(
                    "observed_register_document_id",
                    text_json(non_empty(
                        field("observed_register_document_id"),
                        &label("observed_register_document_id"),
                    )?),
                );
            }
            "CURRENT_NO_PUBLISHED_COMPILATION" => set(
                "current_version_start",
                text_json(required_date(
                    field("current_version_start"),
                    &label("current_version_start"),
                )?),
            ),
            "LOOKUP_FAILED" => set(
                "error_category",
                text_json(non_empty(
                    field("error_category"),
                    &label("error_category"),
                )?),
            ),
            _ => {}
        }
        observations.push((register_id, collection, Json::Object(projected)));
    }
    let expected_ids: BTreeSet<P> = expected.iter().map(|(id, _)| id.clone()).collect();
    if *complete && seen != expected_ids {
        return Err(contract_error(
            "a complete observation must cover every baseline register_id exactly once.",
        ));
    }
    observations.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut result: Vec<(PyStr, Json)> = vec![
        (
            PyStr::Text("schema_version".into()),
            Json::str(schema.observation),
        ),
        (PyStr::Text("mode".into()), Json::str("synthetic")),
        (PyStr::Text("observed_at".into()), text_json(observed_at)),
    ];
    if let Some(scope_id) = scope_id {
        result.push((PyStr::Text("scope_id".into()), text_json(scope_id)));
    }
    result.push((
        PyStr::Text("expected_register_ids".into()),
        Json::Array(expected_ids.into_iter().map(text_json).collect()),
    ));
    result.push((PyStr::Text("complete".into()), Json::Bool(*complete)));
    result.push((
        PyStr::Text("observations".into()),
        Json::Array(observations.into_iter().map(|o| o.2).collect()),
    ));
    Ok(Json::Object(result))
}

/// `sha256:` and 64 lowercase hex digits.
pub fn sha256_id(text: &[u32]) -> bool {
    text.len() == 71
        && text[..7] == pts("sha256:")[..]
        && text[7..]
            .iter()
            .all(|&cp| matches!(char::from_u32(cp), Some('0'..='9' | 'a'..='f')))
}

/// The first chunk `json.dump(value, f, indent=2, sort_keys=True,
/// ensure_ascii=False)` hands a UTF-8 text file that cannot encode, as the
/// `UnicodeEncodeError` text that file raises.
fn first_unencodable(value: &Json) -> Option<String> {
    fn quoted(s: &PyStr) -> P {
        pyvalue::dumps_line(&Json::Str(s.clone()), false)
    }
    fn walk(value: &Json, level: usize) -> Option<String> {
        match value {
            Json::Object(pairs) => {
                let mut sorted: Vec<&(PyStr, Json)> = pairs.iter().collect();
                sorted.sort_by_key(|(k, _)| k.code_points());
                for (k, v) in sorted {
                    if let Some(error) = pyvalue::utf8_strict_error(&quoted(k)) {
                        return Some(error);
                    }
                    let found = match v {
                        Json::Str(s) => pyvalue::utf8_strict_error(&quoted(s)),
                        other => walk(other, level + 1),
                    };
                    if found.is_some() {
                        return found;
                    }
                }
                None
            }
            Json::Array(items) => {
                let indent: P = std::iter::once(u32::from('\n'))
                    .chain(std::iter::repeat_n(u32::from(' '), 2 * (level + 1)))
                    .collect();
                for (i, item) in items.iter().enumerate() {
                    let found = match item {
                        Json::Str(s) => {
                            let mut chunk = pts(if i == 0 { "[" } else { "," });
                            chunk.extend(&indent);
                            chunk.extend(quoted(s));
                            pyvalue::utf8_strict_error(&chunk)
                        }
                        other => walk(other, level + 1),
                    };
                    if found.is_some() {
                        return found;
                    }
                }
                None
            }
            _ => None,
        }
    }
    walk(value, 0)
}

// ----------------------------------------------------------- test hooks

/// A filesystem step a test can fail.
#[doc(hidden)]
#[derive(Debug)]
pub enum Step<'a> {
    /// Writing the staged copy of an output: its destination.
    Stage(&'a str),
    /// Writing the lock's token: the lock file.
    LockToken(&'a str),
    /// `os.replace(source, destination)`.
    Replace(&'a str, &'a str),
}

type FaultHook = Box<dyn FnMut(Step) -> Option<OsError>>;

thread_local! {
    static FAULT_HOOK: RefCell<Option<FaultHook>> = const { RefCell::new(None) };
    static LOCK_TIMEOUT: std::cell::Cell<Duration> = const { std::cell::Cell::new(PUBLISH_LOCK_TIMEOUT) };
}

/// Run `hook(step)` before each [`Step`] on this thread; an error it returns
/// fails that step. For tests.
#[doc(hidden)]
pub fn set_fault_hook(hook: impl FnMut(Step) -> Option<OsError> + 'static) {
    FAULT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// How long this thread waits for another writer's lock. For tests.
#[doc(hidden)]
pub fn set_lock_timeout(timeout: Duration) {
    LOCK_TIMEOUT.with(|t| t.set(timeout));
}

fn fault(step: Step) -> Result<(), Exc> {
    match FAULT_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(step))) {
        Some(error) => Err(Exc::Os(error)),
        None => Ok(()),
    }
}

/// `os.replace(src, dst)`.
fn replace(src: &str, dst: &str) -> Result<(), Exc> {
    fault(Step::Replace(src, dst))?;
    Ok(pyos::replace(src, dst)?)
}

// ------------------------------------------------------------ publishing

/// `path.unlink()`, a missing file ignored.
fn remove(path: &str) -> Result<(), Exc> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Exc::Os(OsError::new(&e, Some(path), Style::Native))),
    }
}

/// A `finally` that removes `paths`: the first removal error replaces the
/// outcome, as an exception raised in Python's `finally` does.
fn remove_all<'a, T>(
    outcome: Result<T, Exc>,
    paths: impl IntoIterator<Item = &'a String>,
) -> Result<T, Exc> {
    for path in paths {
        remove(path)?;
    }
    outcome
}

fn exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

fn lexists(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// `_validate_existing_destination(path)`: absent or an ordinary file.
fn validate_existing_destination(path: &str) -> Result<(), Exc> {
    let details = match pyos::lstat(path) {
        Ok(details) => details,
        Err(error) if error.not_found() => return Ok(()),
        Err(error) => {
            return Err(contract_error(format!(
                "monitor output path cannot be inspected: {path} ({error})."
            )));
        }
    };
    if !details.file_type().is_file()
        || pyos::islink(path)
        || pyos::isjunction(path)
        || pyos::is_reparse_point(&details)
    {
        return Err(contract_error(format!(
            "monitor output path must be absent or a regular file: {path}."
        )));
    }
    Ok(())
}

/// `_write_staged(path, value)`: a new private sibling holding the JSON.
fn write_staged(path: &str, value: &Json) -> Result<String, Exc> {
    fault(Step::Stage(path))?;
    let (parent, name) = pypath::split(path);
    let staged = pypath::join(
        &parent,
        &format!(".{name}.monitor-contract-{}.tmp", pyos::random_hex(32)),
    );
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&staged)
        .map_err(|e| OsError::new(&e, Some(&staged), Style::Errno))?;
    if let Some(error) = first_unencodable(value) {
        drop(file);
        let _ = std::fs::remove_file(&staged);
        return Err(Exc::UnicodeEncode(error));
    }
    let mut text = dumps_with(value, Some(2), false, true);
    text.push(u32::from('\n'));
    let bytes: String = text.into_iter().filter_map(char::from_u32).collect();
    let written = std::io::Write::write_all(&mut file, bytes.as_bytes())
        .map_err(|e| Exc::Os(OsError::new(&e, None, Style::Errno)));
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(staged)
}

/// `_OutputDirectoryLock`: an exclusive, recoverable lock file for one output
/// directory.
struct OutputLock {
    path: String,
    token: Option<Vec<u8>>,
    recovery_required: bool,
}

impl OutputLock {
    fn acquire(directory: &str) -> Result<OutputLock, Exc> {
        let path = pypath::join(directory, PUBLISH_LOCK_FILENAME);
        let deadline = Instant::now() + LOCK_TIMEOUT.with(std::cell::Cell::get);
        loop {
            match pyos::create_new(&path, true) {
                Ok(mut file) => {
                    let token =
                        format!("pid={} token={}", std::process::id(), pyos::random_hex(32))
                            .into_bytes();
                    let written = fault(Step::LockToken(&path)).and_then(|()| {
                        std::io::Write::write_all(&mut file, &token)
                            .map_err(|e| Exc::Os(OsError::new(&e, None, Style::Errno)))
                    });
                    if let Err(error) = written {
                        // Close before removing: Windows refuses to delete an open file.
                        drop(file);
                        remove(&path)?;
                        return Err(error);
                    }
                    return Ok(OutputLock {
                        path,
                        token: Some(token),
                        recovery_required: false,
                    });
                }
                Err(error) if error.kind == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(contract_error(format!(
                            "monitor output directory is locked by another writer or an interrupted publication: {path}. \
                             Wait for an active writer, or recover the output pair before removing its lock."
                        )));
                    }
                    std::thread::sleep(PUBLISH_LOCK_RETRY);
                }
                Err(error) => return Err(Exc::Os(error)),
            }
        }
    }

    /// `__exit__`: remove the lock this writer owns unless recovery needs it.
    /// `outcome` is the body's result, which a cleanup failure never hides.
    fn release<T>(self, outcome: Result<T, Exc>) -> Result<T, Exc> {
        if self.recovery_required {
            return outcome;
        }
        let Some(token) = &self.token else {
            return outcome;
        };
        let cleanup = std::fs::read(&self.path)
            .map_err(|e| OsError::new(&e, Some(&self.path), Style::Errno))
            .and_then(|held| {
                if held == *token {
                    remove(&self.path).map_err(|e| match e {
                        Exc::Os(error) => error,
                        _ => unreachable!("remove raises OSError"),
                    })
                } else {
                    Ok(())
                }
            });
        match (outcome, cleanup) {
            (outcome, Ok(())) => outcome,
            (Err(error), Err(_)) => Err(error),
            (Ok(_), Err(_)) => Err(contract_error(
                "monitor output was published, but its owned lock could not be removed",
            )),
        }
    }
}

/// `_publish(staged, destinations)`: replace both outputs under the lock,
/// restoring the previous pair if either replacement fails.
fn publish(staged: &[(&str, String)], destinations: &[(&str, String)]) -> Result<(), Exc> {
    let result = (|| -> Result<(), Exc> {
        let directory = pypath::split(&destinations[0].1).0;
        let mut lock = OutputLock::acquire(&directory)?;
        let body = (|| -> Result<(), Exc> {
            for (_, destination) in destinations {
                validate_existing_destination(destination)?;
            }
            let mut backups: Vec<(&str, Option<String>)> = Vec::new();
            let mut promoted: Vec<&str> = Vec::new();
            lock.recovery_required = true;
            let attempt = (|| -> Result<(), Exc> {
                for ((name, destination), (_, staged_path)) in destinations.iter().zip(staged) {
                    let (parent, leaf) = pypath::split(destination);
                    let backup = pypath::join(
                        &parent,
                        &format!(".{leaf}.monitor-contract-{}.bak", pyos::random_hex(32)),
                    );
                    if exists(destination) {
                        if lexists(&backup) {
                            return Err(contract_error(
                                "monitor backup staging path already exists.",
                            ));
                        }
                        backups.push((name, Some(backup.clone())));
                        replace(destination, &backup)?;
                    } else {
                        backups.push((name, None));
                    }
                    promoted.push(name);
                    replace(staged_path, destination)?;
                }
                Ok(())
            })();
            let outcome = match attempt {
                Ok(()) => {
                    lock.recovery_required = false;
                    Ok(())
                }
                Err(error) => {
                    let mut rollback_failed = false;
                    for slot in backups.iter_mut().rev() {
                        let (name, previous) = slot;
                        let destination = &destinations
                            .iter()
                            .find(|(n, _)| n == name)
                            .expect("a known output")
                            .1;
                        let step = (|| -> Result<(), Exc> {
                            match previous {
                                None => {
                                    if promoted.contains(name) {
                                        remove(destination)?;
                                    }
                                    Ok(())
                                }
                                Some(backup) if exists(backup) => {
                                    replace(backup, destination)?;
                                    *previous = None;
                                    Ok(())
                                }
                                Some(_) if !promoted.contains(name) && exists(destination) => {
                                    validate_existing_destination(destination)?;
                                    *previous = None;
                                    Ok(())
                                }
                                Some(_) => {
                                    Err(contract_error("monitor rollback state is uncertain."))
                                }
                            }
                        })();
                        rollback_failed |= step.is_err();
                    }
                    if rollback_failed {
                        Err(contract_error(
                            "monitor output publication rollback failed; retain the lock and any remaining .bak recovery files for operator recovery.",
                        ))
                    } else {
                        lock.recovery_required = false;
                        Err(error)
                    }
                }
            };
            if lock.recovery_required {
                return outcome;
            }
            remove_all(outcome, backups.iter().filter_map(|(_, b)| b.as_ref()))
        })();
        lock.release(body)
    })();
    remove_all(result, staged.iter().map(|(_, path)| path))
}

/// `publish_pair(sources, facts, output_directory)`: project, serialise and
/// replace both monitor inputs as one recoverable pair.
pub fn publish_pair(
    sources: &str,
    facts: &str,
    output_directory: &str,
) -> Result<(String, String), Exc> {
    let directory = pypath::realpath(output_directory)?;
    let baseline_path = pypath::join(&directory, "monitor-baseline.json");
    let observation_path = pypath::join(&directory, "register-observation.json");
    let sources = pypath::realpath(sources)?;
    let facts = pypath::realpath(facts)?;
    for (label, path) in [("sources", &sources), ("observation facts", &facts)] {
        let path_is = |other: &str| PyPath::new(path).same(&PyPath::new(other));
        if path_is(&baseline_path) || path_is(&observation_path) {
            return Err(contract_error(format!(
                "monitor output would replace an input ({label}): {path}."
            )));
        }
    }
    validate_existing_destination(&baseline_path)?;
    validate_existing_destination(&observation_path)?;
    let baseline = project_baseline(&load_json(&sources, "sources")?)?;
    let observation = project_observation(&baseline, &load_json(&facts, "observation facts")?)?;
    pyos::makedirs(&pypath::PyPath::new(&directory))?;
    let mut staged: Vec<(&str, String)> = Vec::new();
    for (name, path, value) in [
        ("baseline", &baseline_path, &baseline),
        ("observation", &observation_path, &observation),
    ] {
        match write_staged(path, value) {
            Ok(written) => staged.push((name, written)),
            Err(error) => return remove_all(Err(error), staged.iter().map(|(_, path)| path)),
        }
    }
    publish(
        &staged,
        &[
            ("baseline", baseline_path.clone()),
            ("observation", observation_path.clone()),
        ],
    )?;
    Ok((baseline_path, observation_path))
}

fn parser() -> Parser {
    Parser::new("export_monitor_contract")
        .description("Project corpus sources and observation facts into monitor v1 inputs.")
        .arg(Action::positional("sources").help("completed corpus sources.json"))
        .arg(Action::positional("facts").help("structured observation facts JSON"))
        .arg(
            Action::option("--out")
                .required()
                .help("directory for the monitor input pair"),
        )
}

/// `python -m fadden export_monitor_contract sources facts --out DIR`.
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let parser = parser();
    let args = parser.parse(argv)?;
    let arg = |name: &str| args.str(name).unwrap_or_default();
    match publish_pair(&arg("sources"), &arg("facts"), &arg("out")) {
        Ok((baseline, observation)) => {
            run.print(&format!("monitor baseline: {baseline}"));
            run.print(&format!("register observation: {observation}"));
            Ok(0)
        }
        Err(Exc::Value {
            class: "fadden.export_monitor_contract.ContractError",
            message,
        }) => Err(parser.error(&message)),
        Err(error) => Ok(run.uncaught(&error)),
    }
}
