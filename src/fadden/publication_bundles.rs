//! `fadden/export_publication_bundles.py`: source-only publisher evidence
//! bundles from verified monitor inputs.
//!
//! The exporter snapshots both inputs once, projects them through the monitor
//! contract and publishes every bundle in one no-replace directory rename: an
//! existing output is never replaced, repaired or deleted.
use std::cell::RefCell;
use std::collections::BTreeSet;

use super::monitor_contract::{self, COLLECTIONS, CONTENT_KINDS, CONTENT_MEDIA_TYPES};
use super::{Exc, Run, corpus_paths};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{Json, PyStr};
use crate::pyos::{self, OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyvalue::{self, dumps_with};
use crate::radar::util::{MAX_JSON_BYTES, SourceSnapshot, load_json};

const CLASS: &str = "fadden.export_publication_bundles.PublicationBundleError";

/// `PublicationBundleError(message)`.
pub fn bundle_error(message: impl Into<String>) -> Exc {
    Exc::Value {
        class: CLASS,
        message: message.into(),
    }
}

type P = Vec<u32>;

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

fn shown(points: &[u32]) -> String {
    pyvalue::points_lossy(points)
}

/// `re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,N}")`, over code points.
fn safe_name(text: &[u32], max_rest: usize, lower_only: bool, min_rest: usize) -> bool {
    let first = |cp: u32| match char::from_u32(cp) {
        Some(c) if lower_only => c.is_ascii_lowercase() || c.is_ascii_digit(),
        Some(c) => c.is_ascii_alphanumeric(),
        None => false,
    };
    let rest = |cp: u32| first(cp) || matches!(char::from_u32(cp), Some('.' | '_' | '-'));
    let Some((&head, tail)) = text.split_first() else {
        return false;
    };
    first(head) && (min_rest..=max_rest).contains(&tail.len()) && tail.iter().all(|&cp| rest(cp))
}

/// `SAFE_OUTPUT_LEAF.fullmatch(name)`.
fn safe_output_leaf(name: &str) -> bool {
    safe_name(&pts(name), 99, false, 0)
}

/// `_required_text(value, field)`: non-empty, trimmed text without controls.
fn required_text(value: Option<&Json>, field: &str) -> Result<P, Exc> {
    let text = match value {
        Some(Json::Str(s)) => s.code_points(),
        _ => Vec::new(),
    };
    let is_space = |cp: u32| char::from_u32(cp).is_some_and(crate::py::is_space);
    if text.is_empty()
        || is_space(text[0])
        || is_space(text[text.len() - 1])
        || text.iter().any(|&cp| cp < 32 || cp == 127)
    {
        return Err(bundle_error(format!(
            "{field} must be non-empty, trimmed text."
        )));
    }
    Ok(text)
}

fn utf16_length(text: &[u32]) -> usize {
    text.iter().map(|&cp| if cp > 0xFFFF { 2 } else { 1 }).sum()
}

/// `_publisher_text(value, field, maximum)`: XML-safe text within the
/// publisher's UTF-16 length limit.
fn publisher_text(value: Option<&Json>, field: &str, maximum: usize) -> Result<P, Exc> {
    let text = required_text(value, field)?;
    let xml_safe = text.iter().all(|&cp| {
        matches!(cp, 0x09 | 0x0A | 0x0D)
            || (0x20..=0xD7FF).contains(&cp)
            || (0xE000..=0xFFFD).contains(&cp)
            || (0x10000..=0x10FFFF).contains(&cp)
    });
    if utf16_length(&text) > maximum || !xml_safe {
        return Err(bundle_error(format!(
            "{field} must fit the publisher's XML text limit of {maximum} characters."
        )));
    }
    Ok(text)
}

/// `_publisher_identifier(value, field)`.
fn publisher_identifier(value: String, field: &str) -> Result<String, Exc> {
    if !safe_name(&pts(&value), 79, true, 2) {
        return Err(bundle_error(format!(
            "{field} is not a safe publisher identifier."
        )));
    }
    Ok(value)
}

/// `_publisher_https_url(value, field, expected_path=...)`: a Register URL
/// for exactly one title's latest text.
fn publisher_https_url(value: Option<&Json>, field: &str, expected_path: &str) -> Result<P, Exc> {
    let text = required_text(value, field)?;
    let invalid = || bundle_error(format!("{field} must be a permitted HTTPS URL."));
    // A lone surrogate cannot be encoded as UTF-8.
    let url: String = text
        .iter()
        .map(|&cp| char::from_u32(cp).ok_or_else(invalid))
        .collect::<Result<_, _>>()?;
    let parts = crate::pyurl::urlsplit(&url).map_err(|_| invalid())?;
    let port = crate::pyurl::port(&parts.netloc).map_err(|_| invalid())?;
    let hostname = crate::pyurl::hostname(&parts.netloc);
    if utf16_length(&text) > 2048
        || parts.scheme.to_lowercase() != "https"
        || hostname.is_none_or(|h| h.to_lowercase() != "www.legislation.gov.au")
        || port.is_some_and(|p| p != 443)
        || !parts.query.is_empty()
        || !parts.fragment.is_empty()
        || parts.netloc.contains('@')
        || parts.netloc.contains('\\')
        || parts.netloc.chars().any(crate::py::is_space)
        || parts.path != expected_path
    {
        return Err(invalid());
    }
    Ok(text)
}

/// `_safe_upstream_id(value, field)`.
fn safe_upstream_id(value: Option<&Json>, field: &str) -> Result<P, Exc> {
    let text = required_text(value, field)?;
    if !safe_name(&text, 79, false, 0) {
        return Err(bundle_error(format!("{field} is not a safe identifier.")));
    }
    Ok(text)
}

/// `_input_digest(value, field)`: `sha256:` and a lowercase SHA-256 digest.
fn input_digest(value: &str, field: &str) -> Result<String, Exc> {
    let text = required_text(Some(&Json::str(value)), field)?;
    let hex = text.len() == 64
        && text
            .iter()
            .all(|&cp| matches!(char::from_u32(cp), Some('0'..='9' | 'a'..='f')));
    if !hex {
        return Err(bundle_error(format!(
            "{field} must be a lowercase SHA-256 digest."
        )));
    }
    Ok(format!("sha256:{value}"))
}

/// `_required_member(value, field, label)`.
fn required_member<'a>(value: &'a Json, field: &str, label: &str) -> Result<&'a Json, Exc> {
    value
        .get(field)
        .ok_or_else(|| bundle_error(format!("{label} is missing {field}.")))
}

/// `_iso_date(value, field)`: `date.fromisoformat(text).isoformat() == text`,
/// which holds only for ASCII `YYYY-MM-DD`.
fn iso_date(value: Option<&Json>, field: &str) -> Result<P, Exc> {
    let text = required_text(value, field)?;
    let ascii: Option<String> = text
        .iter()
        .map(|&cp| char::from_u32(cp).filter(char::is_ascii))
        .collect();
    let valid = ascii.is_some_and(|s| {
        let b = s.as_bytes();
        b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && [0..4, 5..7, 8..10]
                .iter()
                .all(|r| b[r.clone()].iter().all(u8::is_ascii_digit))
            && crate::py::Date::new(
                s[0..4].parse().unwrap_or(0),
                s[5..7].parse().unwrap_or(0),
                s[8..10].parse().unwrap_or(0),
            )
            .is_ok()
    });
    if !valid {
        return Err(bundle_error(format!(
            "{field} must be an ISO calendar date."
        )));
    }
    Ok(text)
}

fn text_json(points: P) -> Json {
    Json::Str(PyStr::from_code_points(points))
}

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (PyStr::Text(k.into()), v))
            .collect(),
    )
}

/// `observation["observed_at"]`, a `KeyError` when absent.
fn observed_at(observation: &Json) -> Result<Json, Exc> {
    observation
        .get("observed_at")
        .cloned()
        .ok_or_else(|| Exc::Key("'observed_at'".into()))
}

fn in_set(text: &[u32], set: &[&str]) -> bool {
    set.iter().any(|s| pts(s) == text)
}

/// `build_publication_bundles(baseline, observation, ...)`: deterministic
/// bundles for every superseded title of a complete v3 observation.
pub fn build_publication_bundles(
    baseline: &Json,
    observation: &Json,
    baseline_sha256: &str,
    observation_facts_sha256: &str,
    producer_version: &Json,
) -> Result<Vec<Json>, Exc> {
    if !matches!(baseline, Json::Object(_)) || !matches!(observation, Json::Object(_)) {
        return Err(bundle_error("baseline and observation must be objects."));
    }
    if observation.get("schema_version").and_then(Json::as_str)
        != Some("au-tax-register-observation.v3")
    {
        return Err(bundle_error(
            "observation must use au-tax-register-observation.v3.",
        ));
    }
    if observation.get("mode").and_then(Json::as_str) != Some("synthetic") {
        return Err(bundle_error("observation mode must be synthetic."));
    }
    if !matches!(observation.get("complete"), Some(Json::Bool(true))) {
        return Err(bundle_error(
            "observation must cover the complete baseline scope.",
        ));
    }
    let (Some(Json::Array(raw_titles)), Some(Json::Array(raw_observations))) =
        (baseline.get("titles"), observation.get("observations"))
    else {
        return Err(bundle_error(
            "baseline titles and observations must be arrays.",
        ));
    };
    let Some(Json::Array(expected_register_ids)) = observation.get("expected_register_ids") else {
        return Err(bundle_error("expected_register_ids must be an array."));
    };

    let mut titles: Vec<(P, &Json)> = Vec::new();
    for (index, raw_title) in raw_titles.iter().enumerate() {
        let index = index + 1;
        if !matches!(raw_title, Json::Object(_)) {
            return Err(bundle_error(format!(
                "baseline title {index} must be an object."
            )));
        }
        let label = format!("baseline title {index}");
        let register_id = safe_upstream_id(
            Some(required_member(raw_title, "register_id", &label)?),
            &format!("{label} register_id"),
        )?;
        if titles.iter().any(|(id, _)| *id == register_id) {
            return Err(bundle_error(
                "baseline contains duplicate register identifiers.",
            ));
        }
        titles.push((register_id, raw_title));
    }
    let title_ids: BTreeSet<P> = titles.iter().map(|(id, _)| id.clone()).collect();
    let expected: Option<Vec<P>> = expected_register_ids
        .iter()
        .map(|v| match v {
            Json::Str(s) => Some(s.code_points()),
            _ => None,
        })
        .collect();
    let scope_matches = expected.is_some_and(|ids| {
        let unique: BTreeSet<P> = ids.iter().cloned().collect();
        unique.len() == ids.len() && unique == title_ids
    });
    if !scope_matches {
        return Err(bundle_error(
            "observation scope does not match the baseline.",
        ));
    }

    let baseline_digest = input_digest(baseline_sha256, "baseline_sha256")?;
    let facts_digest = input_digest(observation_facts_sha256, "observation_facts_sha256")?;
    let version = publisher_text(Some(producer_version), "producer_version", 100)?;
    let mut bundles: Vec<(String, Json)> = Vec::new();
    let mut seen_register_ids: BTreeSet<P> = BTreeSet::new();

    for (index, observed) in raw_observations.iter().enumerate() {
        let index = index + 1;
        if !matches!(observed, Json::Object(_)) {
            return Err(bundle_error(format!(
                "observation {index} must be an object."
            )));
        }
        let label = format!("observation {index}");
        let member = |name: &str| required_member(observed, name, &label).map(Some);
        let field = |name: &str| format!("{label} {name}");
        let register_id = safe_upstream_id(member("register_id")?, &field("register_id"))?;
        if !seen_register_ids.insert(register_id.clone()) {
            return Err(bundle_error(
                "observation contains duplicate register identifiers.",
            ));
        }
        let Some((_, title)) = titles.iter().find(|(id, _)| *id == register_id) else {
            return Err(bundle_error(
                "observation contains an identifier outside the baseline.",
            ));
        };
        let state = required_text(member("state")?, &field("state"))?;
        if state == pts("UNCHANGED") {
            continue;
        }
        if state != pts("SUPERSEDED") {
            return Err(bundle_error(format!(
                "observation state {} cannot support a publication bundle.",
                shown(&state)
            )));
        }
        let collection = required_text(member("collection")?, &field("collection"))?;
        let title_collection = match title.get("collection") {
            Some(Json::Str(s)) => Some(s.code_points()),
            _ => None,
        };
        if title_collection.as_ref() != Some(&collection) {
            return Err(bundle_error(format!(
                "observation {index} collection does not match the baseline."
            )));
        }
        if !in_set(&collection, &COLLECTIONS) {
            return Err(bundle_error(format!(
                "observation {index} collection is unsupported."
            )));
        }
        let document_id = safe_upstream_id(
            member("observed_register_document_id")?,
            &field("observed_register_document_id"),
        )?;
        let compilation_number = publisher_text(
            member("observed_compilation_number")?,
            &field("observed_compilation_number"),
            80,
        )?;
        let compilation_date = iso_date(
            member("observed_compilation_date")?,
            &field("observed_compilation_date"),
        )?;
        let content_sha256 = required_text(member("content_sha256")?, &field("content_sha256"))?;
        if !monitor_contract::sha256_id(&content_sha256) {
            return Err(bundle_error(format!(
                "observation {index} content_sha256 is invalid."
            )));
        }
        let content_kind = required_text(member("content_kind")?, &field("content_kind"))?;
        if !in_set(&content_kind, &CONTENT_KINDS) {
            return Err(bundle_error(format!(
                "observation {index} content_kind is unsupported."
            )));
        }
        let content_media_type =
            required_text(member("content_media_type")?, &field("content_media_type"))?;
        if !in_set(&content_media_type, &CONTENT_MEDIA_TYPES) {
            return Err(bundle_error(format!(
                "observation {index} content_media_type is unsupported."
            )));
        }
        let title_name = publisher_text(title.get("name"), "baseline title name", 200)?;
        let previous_number = publisher_text(
            title.get("compilation_number"),
            "baseline title compilation_number",
            80,
        )?;
        let previous_date = iso_date(
            title.get("compilation_date"),
            "baseline title compilation_date",
        )?;
        if compilation_number == previous_number || compilation_date <= previous_date {
            return Err(bundle_error(format!(
                "observation {index} is not a newer published compilation."
            )));
        }
        // Safe identifiers and ISO dates are ASCII.
        let (register_text, document_text) = (shown(&register_id), shown(&document_id));
        let published_at = format!("{}T00:00:00Z", shown(&compilation_date));
        let identity = format!(
            "{}-{}",
            register_text.to_ascii_lowercase(),
            document_text.to_ascii_lowercase()
        );
        let bundle_id =
            publisher_identifier(format!("bundle-frl-{identity}-r1"), "generated bundle_id")?;
        let development_id =
            publisher_identifier(format!("dev-frl-{identity}"), "generated development_id")?;
        let source_id = publisher_identifier(
            format!("frl-{}", document_text.to_ascii_lowercase()),
            "generated source_id",
        )?;
        if bundles.iter().any(|(id, _)| *id == bundle_id) {
            return Err(bundle_error(
                "observations produce duplicate bundle identities.",
            ));
        }
        // The dict literal's values, in Python's evaluation order.
        let generated_at = observed_at(observation)?;
        let canonical_url = publisher_https_url(
            member("evidence_url")?,
            &field("evidence_url"),
            &format!("/{register_text}/latest/text"),
        )?;
        let retrieved_at = required_text(member("checked_at")?, &field("checked_at"))?;
        let evidence_id = safe_upstream_id(member("evidence_id")?, &field("evidence_id"))?;
        let updated_at = observed_at(observation)?;
        let bundle = obj(vec![
            ("schema_version", Json::str("evidence-bundle.v1")),
            ("bundle_id", Json::str(&bundle_id)),
            ("development_id", Json::str(&development_id)),
            ("mode", Json::str("synthetic")),
            ("generated_at", generated_at),
            (
                "producer",
                obj(vec![
                    ("name", Json::str("tax-radar-au")),
                    ("version", text_json(version.clone())),
                    ("baseline_sha256", Json::str(&baseline_digest)),
                    ("observation_facts_sha256", Json::str(&facts_digest)),
                ]),
            ),
            (
                "development",
                obj(vec![
                    ("title", text_json(title_name.clone())),
                    ("authority_status", Json::str("in-force")),
                    ("evidence_status", Json::str("verified")),
                    ("publication_status", Json::str("source-only")),
                    ("published_at", Json::str(&published_at)),
                    ("effective_at", Json::Null),
                    ("topics", Json::Array(Vec::new())),
                    ("affected_practice_areas", Json::Array(Vec::new())),
                ]),
            ),
            (
                "source_event",
                obj(vec![
                    ("kind", Json::str("compilation-superseded")),
                    ("register_id", text_json(register_id.clone())),
                    ("collection", text_json(collection)),
                    (
                        "previous_compilation",
                        obj(vec![
                            ("number", text_json(previous_number)),
                            ("date", text_json(previous_date)),
                        ]),
                    ),
                    (
                        "current_compilation",
                        obj(vec![
                            ("number", text_json(compilation_number)),
                            ("date", text_json(compilation_date)),
                            ("register_document_id", text_json(document_id)),
                        ]),
                    ),
                ]),
            ),
            (
                "sources",
                Json::Array(vec![obj(vec![
                    ("source_id", Json::str(&source_id)),
                    ("publisher", Json::str("Federal Register of Legislation")),
                    ("document_class", Json::str("legislation")),
                    ("title", text_json(title_name)),
                    ("canonical_url", text_json(canonical_url)),
                    ("published_at", Json::str(&published_at)),
                    ("retrieved_at", text_json(retrieved_at)),
                    ("evidence_id", text_json(evidence_id)),
                    ("content_sha256", text_json(content_sha256)),
                    ("content_kind", text_json(content_kind)),
                    ("content_media_type", text_json(content_media_type)),
                    (
                        "rights",
                        obj(vec![
                            ("mode", Json::str("metadata-only")),
                            (
                                "attribution",
                                Json::str("Synthetic Federal Register fixture"),
                            ),
                            ("licence_url", Json::Null),
                        ]),
                    ),
                    ("evidence", Json::Array(Vec::new())),
                ])]),
            ),
            (
                "revision",
                obj(vec![
                    ("number", Json::Int(1.into())),
                    ("updated_at", updated_at),
                    (
                        "change_note",
                        Json::str("Initial source-only evidence bundle"),
                    ),
                    ("replaces_bundle_id", Json::Null),
                ]),
            ),
        ]);
        bundles.push((bundle_id, bundle));
    }
    if seen_register_ids != title_ids {
        return Err(bundle_error(
            "observation does not cover every baseline title.",
        ));
    }
    bundles.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(bundles.into_iter().map(|(_, b)| b).collect())
}

/// `bundle_bytes(bundle)`: the contract's reviewed byte representation.
pub fn bundle_bytes(bundle: &Json) -> Result<Vec<u8>, Exc> {
    let mut text = dumps_with(bundle, Some(2), false, false);
    text.push(u32::from('\n'));
    if let Some(error) = pyvalue::utf8_strict_error(&text) {
        return Err(Exc::UnicodeEncode(error));
    }
    Ok(text
        .into_iter()
        .filter_map(char::from_u32)
        .collect::<String>()
        .into_bytes())
}

// ----------------------------------------------------------- test hooks

/// A filesystem step a test can fail.
#[doc(hidden)]
#[derive(Debug)]
pub enum Step<'a> {
    /// `os.lstat(path)`.
    Lstat(&'a str),
    /// `os.mkdir(path, 0o700)`.
    Mkdir(&'a str),
    /// Writing one bundle into staging.
    Write(&'a str),
    /// The no-replace rename of staging onto the output.
    Promote(&'a str, &'a str),
    /// Removing the exporter's staging directory.
    Cleanup(&'a str),
}

type FaultHook = Box<dyn FnMut(Step) -> Option<OsError>>;

thread_local! {
    static FAULT_HOOK: RefCell<Option<FaultHook>> = const { RefCell::new(None) };
    static VERSION_DIR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `hook(step)` before each [`Step`] on this thread; an error it returns
/// fails that step. For tests.
#[doc(hidden)]
pub fn set_fault_hook(hook: impl FnMut(Step) -> Option<OsError> + 'static) {
    FAULT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// Read `VERSION` from `directory` on this thread instead of the checkout's
/// root, as patching `VERSION_PATH` did; `None` restores the default. For tests.
#[doc(hidden)]
pub fn set_version_dir(directory: Option<&str>) {
    VERSION_DIR.with(|d| *d.borrow_mut() = directory.map(str::to_string));
}

fn fault(step: Step) -> Result<(), OsError> {
    match FAULT_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(step))) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn lstat(path: &str) -> Result<std::fs::Metadata, OsError> {
    fault(Step::Lstat(path))?;
    pyos::lstat(path)
}

// ------------------------------------------------------------ the export

/// `_producer_version()`: the repository `VERSION`, else the installed
/// distribution's version.
fn producer_version() -> Result<Json, Exc> {
    let root = match VERSION_DIR.with(|d| d.borrow().clone()) {
        Some(directory) => PyPath::new(&directory),
        None => corpus_paths::script_dir("export_publication_bundles.py")?.parent(),
    };
    let path = root.join("VERSION").display();
    match crate::pyio::read_text(&path) {
        Ok(text) => return Ok(Json::Str(PyStr::from_code_points(trim(&text)))),
        Err(crate::pyio::ReadError::Os(error)) if !error.not_found() => {
            return Err(Exc::Os(error));
        }
        Err(crate::pyio::ReadError::Os(_)) => {}
        Err(error) => return Err(error.into()),
    }
    installed_version(&root.display())
        .map(|version| Json::str(&version))
        .ok_or_else(|| bundle_error("producer version metadata is unavailable."))
}

fn trim(text: &str) -> P {
    let points = pts(text);
    let is_space = |cp: &u32| char::from_u32(*cp).is_some_and(crate::py::is_space);
    let start = points.iter().position(|cp| !is_space(cp));
    let end = points.iter().rposition(|cp| !is_space(cp));
    match (start, end) {
        (Some(s), Some(e)) => points[s..=e].to_vec(),
        _ => Vec::new(),
    }
}

/// `importlib.metadata.version("tax-radar-au")` for a wheel installed beside
/// the package: the `Version` of its `.dist-info/METADATA`.
fn installed_version(site: &str) -> Option<String> {
    let mut found: Vec<String> = std::fs::read_dir(site)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            // importlib.metadata's lookup: the lowercased stem before its first
            // `-`, with runs of `-`, `_` and `.` read as one `_`.
            let lower = name.to_lowercase();
            let Some(stem) = lower.strip_suffix(".dist-info") else {
                return false;
            };
            let project = stem.split('-').next().unwrap_or_default();
            let normalized: Vec<&str> = project
                .split(['-', '_', '.'])
                .filter(|part| !part.is_empty())
                .collect();
            normalized.join("_") == "tax_radar_au"
        })
        .collect();
    found.sort();
    let metadata = std::fs::read_to_string(pypath::join(
        &pypath::join(site, found.first()?),
        "METADATA",
    ))
    .ok()?;
    metadata
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix("Version:"))
        .map(|v| v.trim().to_string())
}

fn abspath(path: &str) -> String {
    let text = PyPath::new(path).display();
    if pypath::isabs(&text) {
        return pypath::normpath(&text);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    pypath::normpath(&pypath::join(&cwd.to_string_lossy(), &text))
}

/// Whether an `lstat` result is an ordinary entry of the wanted kind.
fn ordinary(path: &str, details: &std::fs::Metadata, directory: bool) -> bool {
    let kind = details.file_type();
    (if directory {
        kind.is_dir()
    } else {
        kind.is_file()
    }) && !pyos::islink(path)
        && !pyos::isjunction(path)
        && !pyos::is_reparse_point(details)
}

/// `_require_regular_input(path, label)`.
fn require_regular_input(path: &str, label: &str) -> Result<(), Exc> {
    let details = match lstat(path) {
        Ok(details) => details,
        Err(error) if error.not_found() => {
            return Err(bundle_error(format!("{label} does not exist: {path}.")));
        }
        Err(error) => {
            return Err(bundle_error(format!(
                "{label} cannot be inspected: {path} ({error})."
            )));
        }
    };
    if !ordinary(path, &details, false) {
        return Err(bundle_error(format!(
            "{label} must be an ordinary file: {path}."
        )));
    }
    Ok(())
}

/// `_require_ordinary_parent(path)`.
fn require_ordinary_parent(path: &str) -> Result<(), Exc> {
    let details = match lstat(path) {
        Ok(details) => details,
        Err(error) if error.not_found() => {
            return Err(bundle_error(format!(
                "publication bundle output parent does not exist: {path}."
            )));
        }
        Err(error) => {
            return Err(bundle_error(format!(
                "publication bundle output parent cannot be inspected: {path} ({error})."
            )));
        }
    };
    if !ordinary(path, &details, true) {
        return Err(bundle_error(format!(
            "publication bundle output parent must be an ordinary directory: {path}."
        )));
    }
    Ok(())
}

/// `_output_parent_needs_creation(path)`.
fn output_parent_needs_creation(path: &PyPath) -> Result<bool, Exc> {
    let text = path.display();
    let details = match lstat(&text) {
        Ok(details) => details,
        Err(error) if error.not_found() => {
            if !safe_output_leaf(path.name()) {
                return Err(bundle_error(
                    "publication bundle output parent name is unsafe.",
                ));
            }
            require_ordinary_parent(&path.parent().display())?;
            return Ok(true);
        }
        Err(error) => {
            return Err(bundle_error(format!(
                "publication bundle output parent cannot be inspected: {text} ({error})."
            )));
        }
    };
    if !ordinary(&text, &details, true) {
        return Err(bundle_error(format!(
            "publication bundle output parent must be an ordinary directory: {text}."
        )));
    }
    Ok(false)
}

/// `_require_absent(path)`.
fn require_absent(path: &str) -> Result<(), Exc> {
    match lstat(path) {
        Err(error) if error.not_found() => Ok(()),
        Err(error) => Err(bundle_error(format!(
            "publication bundle destination cannot be inspected: {path} ({error})."
        ))),
        Ok(_) => Err(bundle_error(format!(
            "publication bundle destination must not exist: {path}."
        ))),
    }
}

/// `os.mkdir(path, 0o700)`.
fn mkdir_private(path: &str) -> Result<(), OsError> {
    fault(Step::Mkdir(path))?;
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    builder
        .create(path)
        .map_err(|e| OsError::new(&e, Some(path), Style::Native))
}

/// `_write_bundle(path, content)`: `open(path, "xb")` and one write.
fn write_bundle(path: &str, content: &[u8]) -> Result<(), OsError> {
    fault(Step::Write(path))?;
    let mut file = pyos::create_new(path, false)?;
    std::io::Write::write_all(&mut file, content).map_err(|e| OsError::new(&e, None, Style::Errno))
}

/// `_promote_no_replace(staging, output)`: the platform's no-replace rename.
fn promote_no_replace(staging: &str, output: &str) -> Result<(), OsError> {
    fault(Step::Promote(staging, output))?;
    rename_no_replace(staging, output)
}

#[cfg(windows)]
fn rename_no_replace(staging: &str, output: &str) -> Result<(), OsError> {
    pyos::rename_new(staging, output)
}

#[cfg(target_os = "linux")]
fn rename_no_replace(staging: &str, output: &str) -> Result<(), OsError> {
    use std::os::unix::ffi::OsStrExt;
    let c = |s: &str| std::ffi::CString::new(std::ffi::OsStr::new(s).as_bytes());
    let (Ok(from), Ok(to)) = (c(staging), c(output)) else {
        let error = std::io::Error::from_raw_os_error(libc::EINVAL);
        return Err(OsError::new(&error, Some(output), Style::Errno));
    };
    // Never fall back to an overwriting rename.
    // SAFETY: both paths are NUL-terminated and outlive the call.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        return Err(OsError::new(&error, Some(output), Style::Errno));
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rename_no_replace(_staging: &str, _output: &str) -> Result<(), OsError> {
    let error = std::io::Error::from_raw_os_error(libc::ENOTSUP);
    let mut error = OsError::new(&error, None, Style::Errno);
    error.text = format!(
        "[Errno {}] no-replace directory publication is unavailable",
        libc::ENOTSUP
    );
    Err(error)
}

/// `_remove_owned_staging(staging, parent=..., prefix=...)`.
#[doc(hidden)]
pub fn remove_owned_staging(staging: &PyPath, parent: &PyPath, prefix: &str) -> Result<(), Exc> {
    if !staging.parent().same(parent) || !staging.name().starts_with(prefix) {
        return Err(Exc::Runtime(
            "refusing to remove a staging path outside the exporter boundary".into(),
        ));
    }
    let text = staging.display();
    let details = match lstat(&text) {
        Ok(details) => details,
        Err(error) if error.not_found() => return Ok(()),
        Err(error) => return Err(Exc::Os(error)),
    };
    if !ordinary(&text, &details, true) {
        return Err(bundle_error(format!(
            "publication bundle staging path is no longer an ordinary directory: {text}."
        )));
    }
    std::fs::remove_dir_all(&text)
        .map_err(|e| Exc::Os(OsError::new(&e, Some(&text), Style::Native)))
}

fn same_location(left: &str, right: &str) -> Result<bool, Exc> {
    Ok(pypath::normcase(&pypath::realpath(left)?) == pypath::normcase(&pypath::realpath(right)?))
}

/// The error a snapshot, projection or version lookup raised, as the export
/// reports it.
fn input_error(error: Exc) -> Exc {
    match error {
        Exc::Value { class, message }
            if class == CLASS || class == "fadden.export_monitor_contract.ContractError" =>
        {
            bundle_error(message)
        }
        Exc::Os(error) => bundle_error(format!(
            "publication bundle inputs could not be read: {error}."
        )),
        other => other,
    }
}

/// A radar `MonitorError` as the export reports it.
fn monitor_error(error: crate::radar::Error) -> Exc {
    match error {
        crate::radar::Error::Monitor { message, .. } => bundle_error(message),
        crate::radar::Error::Os(error) => Exc::Os(error),
    }
}

/// `export_publication_bundles(sources, facts, output_dir)`: validate exact
/// input snapshots and publish every bundle in one rename.
pub fn export_publication_bundles(
    sources_path: &str,
    facts_path: &str,
    output_dir: &str,
) -> Result<Vec<String>, Exc> {
    let sources = abspath(sources_path);
    let facts = abspath(facts_path);
    let output = PyPath::new(&abspath(output_dir));
    let output_text = output.display();
    if !safe_output_leaf(output.name()) {
        return Err(bundle_error("publication bundle output name is unsafe."));
    }
    require_regular_input(&sources, "sources input")?;
    require_regular_input(&facts, "observation facts input")?;
    if same_location(&output_text, &sources)? || same_location(&output_text, &facts)? {
        return Err(bundle_error(
            "publication bundle output would replace an input.",
        ));
    }
    let parent = output.parent();
    let create_output_parent = output_parent_needs_creation(&parent)?;
    require_absent(&output_text)?;

    let bundles = (|| -> Result<Vec<Json>, Exc> {
        let capture = |path: &str, label: &str| {
            SourceSnapshot::capture(&PyPath::new(path), label, MAX_JSON_BYTES)
                .map_err(monitor_error)
        };
        let source_snapshot = capture(&sources, "sources input")?;
        let facts_snapshot = capture(&facts, "observation facts input")?;
        let source_document =
            load_json(&source_snapshot, "sources input").map_err(monitor_error)?;
        let facts_document =
            load_json(&facts_snapshot, "observation facts input").map_err(monitor_error)?;
        let baseline = monitor_contract::project_baseline(&source_document)?;
        let observation = monitor_contract::project_observation(&baseline, &facts_document)?;
        let version = producer_version()?;
        build_publication_bundles(
            &baseline,
            &observation,
            &source_snapshot.sha256,
            &facts_snapshot.sha256,
            &version,
        )
    })()
    .map_err(input_error)?;

    let parent_text = parent.display();
    if create_output_parent {
        mkdir_private(&parent_text).map_err(|error| {
            bundle_error(format!(
                "publication bundle output parent could not be created: {parent_text} ({error})."
            ))
        })?;
        require_ordinary_parent(&parent_text)?;
    }

    let prefix = format!(".{}.publication-bundles-", output.name());
    let staging = parent.join(&format!("{prefix}{}.tmp", pyos::random_hex(32)));
    let staging_text = staging.display();
    let mut created_staging = false;
    let outcome = (|| -> Result<(), Exc> {
        let written = (|| -> Result<(), Exc> {
            mkdir_private(&staging_text)?;
            created_staging = true;
            for bundle in &bundles {
                let id = bundle
                    .get("bundle_id")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let destination = staging.join(&format!("{id}.json")).display();
                write_bundle(&destination, &bundle_bytes(bundle)?)?;
            }
            Ok(())
        })();
        written.map_err(|error| match error {
            Exc::Os(error) => bundle_error(format!(
                "publication bundle output could not be written: {error}."
            )),
            other => other,
        })?;
        require_absent(&output_text)?;
        promote_no_replace(&staging_text, &output_text).map_err(|error| {
            bundle_error(format!(
                "publication bundle output could not be promoted: {error}."
            ))
        })
    })();
    // A normal exit always created staging; only a failure can skip it.
    if created_staging {
        let cleanup = fault(Step::Cleanup(&staging_text))
            .map_err(Exc::Os)
            .and_then(|()| remove_owned_staging(&staging, &parent, &prefix));
        if let (Ok(()), Err(error)) = (&outcome, cleanup) {
            return Err(error);
        }
    }
    outcome?;
    Ok(bundles
        .iter()
        .map(|bundle| {
            let id = bundle
                .get("bundle_id")
                .and_then(Json::as_str)
                .unwrap_or_default();
            output.join(&format!("{id}.json")).display()
        })
        .collect())
}

fn parser() -> Parser {
    Parser::new("export_publication_bundles")
        .description("Export complete v3 monitor observations as publication evidence bundles.")
        .arg(Action::positional("sources").help("completed corpus sources.json"))
        .arg(Action::positional("facts").help("complete v3 observation facts JSON"))
        .arg(
            Action::option("--out")
                .required()
                .help("new immutable publication bundle directory"),
        )
}

/// `python -m fadden export_publication_bundles sources facts --out DIR`.
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let parser = parser();
    let args = parser.parse(argv)?;
    let arg = |name: &str| args.str(name).unwrap_or_default();
    match export_publication_bundles(&arg("sources"), &arg("facts"), &arg("out")) {
        Ok(paths) => {
            for path in paths {
                run.print(&format!("publication evidence bundle: {path}"));
            }
            Ok(0)
        }
        Err(Exc::Value {
            class: CLASS,
            message,
        }) => Err(parser.error(&message)),
        Err(error) => Ok(run.uncaught(&error)),
    }
}
