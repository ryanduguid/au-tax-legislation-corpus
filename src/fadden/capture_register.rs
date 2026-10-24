//! `fadden/capture_register.py`: capture immutable live metadata evidence from
//! the Federal Register.
//!
//! The capture owns the whole transaction: it projects a rich corpus manifest,
//! asks the Register for every title through a [`RegisterSession`], keeps the
//! exact bounded response bytes, and publishes the baseline, the capture
//! record, the observation and the evidence into one absent directory after
//! re-reading and proving the staged graph. Tests replace only the session.
//!
//! The publication workflow still runs the Python stage; this port is
//! verified against it before any workflow change.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{Read, Seek};

use sha2::{Digest, Sha256};

use super::corpus_paths::{self, abspath};
use super::http::{self, Failure, Http};
use super::iso::{self, TextRefusal, Zone};
use super::publication_bundles::bundle_bytes as json_bytes;
use super::{Exc, Run};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, DecodeError, Json, PyStr};
use crate::pyos::{self, OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyvalue::{self, py_eq};

pub const SOURCE_API: &str = "https://api.prod.legislation.gov.au/v1/";
pub const REGISTER_SITE: &str = "https://www.legislation.gov.au";
pub const CAPTURE_SCHEMA: &str = "au-tax-register-capture.v1";
pub const OBSERVATION_SCHEMA: &str = "au-tax-register-observation.v4";
pub const OBSERVATION_SCOPE: &str = "au-primary-tax-legislation.v4";
const SELECT_FIELDS: &str =
    "titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt";
pub const ROW_FIELDS: [&str; 7] = [
    "titleId",
    "start",
    "compilationNumber",
    "registerId",
    "isCurrent",
    "status",
    "registeredAt",
];
/// The response headers a capture keeps, sorted.
pub const RETAINED_HEADER_NAMES: [&str; 4] =
    ["content-type", "date", "odata-version", "x-frl-version"];
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ATTEMPTS: u32 = http::TRIES;
const REQUEST_DELAY_SECONDS: f64 = 1.5;
pub const OBSERVATION_STATES: [&str; 5] = [
    "UNCHANGED",
    "SUPERSEDED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "NO_LONGER_IN_FORCE",
    "LOOKUP_FAILED",
];
pub const FAILURE_CATEGORIES: [&str; 10] = [
    "TRANSPORT_ERROR",
    "HTTP_STATUS",
    "RESPONSE_TOO_LARGE",
    "UNSUPPORTED_MEDIA_TYPE",
    "INVALID_JSON",
    "INVALID_ODATA_SHAPE",
    "IDENTITY_MISMATCH",
    "INCONSISTENT_CHRONOLOGY",
    "INVALID_EXCHANGE",
    "NO_VERSION_EVIDENCE",
];
const COLLECTIONS: [&str; 3] = ["Act", "LegislativeInstrument", "NotifiableInstrument"];
const CORPUS: &str = "Commonwealth tax statutes and legislative instruments";
const SOURCE: &str = "Federal Register of Legislation";

const CLASS: &str = "fadden.capture_register.CaptureRegisterError";

/// `CaptureRegisterError(message)`.
pub fn capture_error(message: impl Into<String>) -> Exc {
    Exc::Value {
        class: CLASS,
        message: message.into(),
    }
}

/// The message of a `CaptureRegisterError`, if `error` is one.
pub fn capture_message(error: &Exc) -> Option<&str> {
    match error {
        Exc::Value { class, message } if *class == CLASS => Some(message),
        _ => None,
    }
}

pub fn odata_context() -> String {
    format!("{SOURCE_API}$metadata#Versions({SELECT_FIELDS})")
}

type P = Vec<u32>;

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
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

fn is(value: &Json, text: &str) -> bool {
    py_eq(value, &Json::str(text))
}

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or(&Json::Null)
}

/// `isinstance(value, int) and not isinstance(value, bool)`, and its value.
fn int(value: &Json) -> Option<i64> {
    match value {
        Json::Int(i) => i64::try_from(i).ok().or(Some(i64::MAX)),
        _ => None,
    }
}

pub fn sha256_id(content: &[u8]) -> String {
    let digest: String = Sha256::digest(content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{digest}")
}

/// `_required_text(value, field)`.
fn required_text(value: &Json, field: &str) -> Result<P, Exc> {
    iso::stripped(value).map_err(|refusal| match refusal {
        TextRefusal::Empty => capture_error(format!("{field} must be a non-empty string.")),
        TextRefusal::Control => {
            capture_error(format!("{field} must not contain control characters."))
        }
    })
}

/// `_date(value, field)`.
fn date(value: &Json, field: &str) -> Result<P, Exc> {
    let text = required_text(value, field)?;
    if !iso::iso_date(&text) {
        return Err(capture_error(format!(
            "{field} must be an ISO calendar date."
        )));
    }
    Ok(text)
}

/// `_utc_timestamp(value, field)`.
fn utc_timestamp(value: &Json, field: &str) -> Result<P, Exc> {
    let text = required_text(value, field)?;
    if iso::instant(&text, 6, Zone::Utc).is_none() {
        return Err(capture_error(format!(
            "{field} must be a UTC timestamp ending in Z."
        )));
    }
    Ok(text)
}

/// `_version_timestamp(value, field)`: the text and the date of its instant.
fn version_timestamp(value: &Json, field: &str) -> Result<(P, crate::py::Date), Exc> {
    let text = required_text(value, field)?;
    let date = iso::instant(&text, 7, Zone::Any)
        .ok_or_else(|| capture_error(format!("{field} is invalid.")))?;
    Ok((text, date))
}

/// `_start_date(value)`: the calendar date of a version's start.
fn start_date(value: &Json) -> Result<String, Exc> {
    Ok(version_timestamp(value, "Register version start")?
        .1
        .isoformat())
}

/// `register_id(value)` from corpus_paths, a `ValueError` on refusal.
fn register_id(value: &Json) -> Result<String, Exc> {
    corpus_paths::register_id(value)
}

fn value_message(error: Exc) -> String {
    match error {
        Exc::Value { message, .. } => message,
        other => other.to_string(),
    }
}

// ----------------------------------------------------------- test hooks

/// A filesystem step of the capture a test can fail or observe, as the
/// Python tests patch `os`, `open` and the module's helpers.
#[doc(hidden)]
#[derive(Debug)]
pub enum Step<'a> {
    /// `os.lstat(path)`: an error fails it.
    Lstat(&'a str),
    /// Whether `path` carries reparse metadata: any answer marks it.
    Reparse(&'a str),
    /// Reading an opened file: an error fails `os.fstat` or the read.
    Read(&'a str),
    /// Comparing two identities of `path`: any answer makes them differ.
    Identity(&'a str),
    /// Listing a directory.
    List(&'a str),
    /// `_write_new(path)`.
    Write(&'a str),
    /// `os.mkdir(path, 0o700)`.
    Mkdir(&'a str),
    /// `os.rename(staging, output)`.
    Promote(&'a str, &'a str),
    /// `shutil.rmtree(staging)`.
    Remove(&'a str),
    /// `_same_location(output, manifest)`: any answer makes them the same.
    SameLocation,
    /// About to validate the staged graph; the hook may change it.
    Validate(&'a str),
    /// The staged graph validated; the hook may race the promotion.
    Validated(&'a str),
}

type FaultHook = Box<dyn FnMut(Step) -> Option<OsError>>;
type OpenHook = Box<dyn FnMut(&str) -> Option<String>>;

thread_local! {
    static FAULT_HOOK: RefCell<Option<FaultHook>> = const { RefCell::new(None) };
    static OPEN_HOOK: RefCell<Option<OpenHook>> = const { RefCell::new(None) };
}

/// Run `hook(step)` at each [`Step`] on this thread. For tests.
#[doc(hidden)]
pub fn set_fault_hook(hook: impl FnMut(Step) -> Option<OsError> + 'static) {
    FAULT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// Open the path `hook` names instead of the one asked for, as patching
/// `builtins.open` did. For tests.
#[doc(hidden)]
pub fn set_open_hook(hook: impl FnMut(&str) -> Option<String> + 'static) {
    OPEN_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// Remove both hooks on this thread. For tests.
#[doc(hidden)]
pub fn clear_hooks() {
    FAULT_HOOK.with(|h| *h.borrow_mut() = None);
    OPEN_HOOK.with(|h| *h.borrow_mut() = None);
}

fn injected(step: Step) -> Option<OsError> {
    FAULT_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(step)))
}

fn io_fault(step: Step) -> std::io::Result<()> {
    injected(step).map_or(Ok(()), |error| Err(std::io::Error::other(error.text)))
}

/// `os.lstat(path)`.
fn lstat(path: &str) -> Result<std::fs::Metadata, OsError> {
    injected(Step::Lstat(path)).map_or_else(|| pyos::lstat(path), Err)
}

/// [`corpus_paths::ordinary`], with any reparse metadata a test marks.
fn ordinary(path: &str, details: &std::fs::Metadata, directory: bool) -> bool {
    corpus_paths::ordinary(path, details, directory) && injected(Step::Reparse(path)).is_none()
}

fn same_location(output: &str, manifest: &str) -> Result<bool, Exc> {
    if injected(Step::SameLocation).is_some() {
        return Ok(true);
    }
    corpus_paths::same_location(output, manifest)
}

// ------------------------------------------------------------ the session

/// `isinstance(exchange.body, bytes)`, or not.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Absent,
    Bytes(Vec<u8>),
    /// A value of another type, which a session must never return.
    Other,
}

/// One bounded exchange a [`RegisterSession`] returns. Its fields keep the
/// types a session handed over, so a malformed exchange is judged as Python
/// judged it.
#[derive(Debug, Clone)]
pub struct RegisterExchange {
    pub checked_at: Json,
    pub status: Json,
    /// The header mapping's items in order, or `None` for a value that is not
    /// a mapping.
    pub headers: Option<Vec<(Json, Json)>>,
    pub body: Body,
    pub attempts: Json,
    pub error_category: Json,
}

/// The complete external-I/O seam of [`capture_register_run`].
pub trait RegisterSession {
    fn observed_at(&self) -> Json;
    /// One already-retried, bounded exchange for `url`.
    fn get(&mut self, url: &str) -> Result<RegisterExchange, Exc>;
}

/// `_HttpsRegisterSession`: the paced HTTPS adapter for the exact Register host.
pub struct HttpsSession {
    http: Box<dyn Http>,
    observed_at: String,
    last_request_started: Option<f64>,
}

impl HttpsSession {
    pub fn new(mut http: Box<dyn Http>) -> HttpsSession {
        let observed_at = http.utc_now();
        HttpsSession {
            http,
            observed_at,
            last_request_started: None,
        }
    }

    fn pace(&mut self) {
        let mut now = self.http.monotonic();
        if let Some(last) = self.last_request_started {
            let remaining = REQUEST_DELAY_SECONDS - (now - last);
            if remaining > 0.0 {
                self.http.sleep(remaining);
                now = self.http.monotonic();
            }
        }
        self.last_request_started = Some(now);
    }

    fn exchange(
        &mut self,
        status: Option<u16>,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        attempt: u32,
        transport: bool,
    ) -> RegisterExchange {
        RegisterExchange {
            checked_at: Json::str(&self.http.utc_now()),
            status: status.map_or(Json::Null, |s| Json::Int(s.into())),
            headers: Some(
                headers
                    .into_iter()
                    .map(|(k, v)| (Json::str(&k), Json::str(&v)))
                    .collect(),
            ),
            body: body.map_or(Body::Absent, Body::Bytes),
            attempts: Json::Int(attempt.into()),
            error_category: if transport {
                Json::str("TRANSPORT_ERROR")
            } else {
                Json::Null
            },
        }
    }
}

/// `_request(url)`: refuse a URL outside the fixed API boundary.
fn check_request_url(url: &str) -> Result<(), Exc> {
    let parts = crate::pyurl::urlsplit(url)
        .map_err(|_| capture_error("Register request URL is invalid."))?;
    let port = crate::pyurl::port(&parts.netloc)
        .map_err(|_| capture_error("Register request URL is invalid."))?;
    if parts.scheme != "https"
        || crate::pyurl::hostname(&parts.netloc).as_deref() != Some("api.prod.legislation.gov.au")
        || port.is_some()
        || parts.netloc.contains('@')
        || !parts.fragment.is_empty()
        || parts.path != "/v1/versions"
    {
        return Err(capture_error(
            "Register request URL is outside the fixed API boundary.",
        ));
    }
    Ok(())
}

/// `_selected_response_headers(headers)`: only the contract headers, a
/// repeated name kept under its case-swapped spelling.
fn selected_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut selected: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (name, value) in headers {
        let lower = name.to_lowercase();
        if !RETAINED_HEADER_NAMES.contains(&lower.as_str()) {
            continue;
        }
        let key = if seen.contains(&lower) {
            name.chars()
                .flat_map(|c| {
                    if c.is_lowercase() {
                        c.to_uppercase().collect::<Vec<_>>()
                    } else {
                        c.to_lowercase().collect()
                    }
                })
                .collect()
        } else {
            name
        };
        seen.insert(lower);
        match selected.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => selected.push((key, value)),
        }
    }
    selected
}

fn retryable(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

impl RegisterSession for HttpsSession {
    fn observed_at(&self) -> Json {
        Json::str(&self.observed_at)
    }

    fn get(&mut self, url: &str) -> Result<RegisterExchange, Exc> {
        check_request_url(url)?;
        let mut request = http::Request::default();
        for attempt in 1..=MAX_ATTEMPTS {
            http::attempt(self.http.as_mut(), attempt);
            self.pace();
            match self
                .http
                .exchange(url, MAX_RESPONSE_BYTES + 1, &mut request)
            {
                Ok(response) => {
                    if retryable(response.status) && attempt < MAX_ATTEMPTS {
                        continue;
                    }
                    let headers = selected_headers(response.headers);
                    return Ok(self.exchange(
                        Some(response.status),
                        headers,
                        response.body,
                        attempt,
                        false,
                    ));
                }
                Err(Failure::Uncaught { class, message }) => {
                    return Err(http::uncaught(class, message));
                }
                Err(Failure::Transport(_) | Failure::Status(_)) => {
                    if attempt < MAX_ATTEMPTS {
                        continue;
                    }
                    return Ok(self.exchange(None, Vec::new(), None, attempt, true));
                }
            }
        }
        unreachable!("the final attempt always returns")
    }
}

// ---------------------------------------------------------------- inputs

fn lstat_ok(path: &str) -> Option<std::fs::Metadata> {
    lstat(path).ok()
}

/// Whether a file is regular, with its `(st_dev, st_ino)`.
type Identity = (bool, (u64, u64));

/// One `os.lstat(path)` with the regular-file identity it carries, so the
/// identity a later open is compared with is the object that was inspected.
fn lstat_identified(path: &str) -> Option<(std::fs::Metadata, Identity)> {
    if injected(Step::Lstat(path)).is_some() {
        return None;
    }
    let (details, identity) = pyos::lstat_identity(path).ok()?;
    let regular = details.file_type().is_file();
    Some((details, (regular, identity)))
}

/// `_require_ordinary_directory(path, label)`.
fn require_ordinary_directory(path: &str, label: &str) -> Result<(), Exc> {
    match lstat_ok(path) {
        Some(details) if ordinary(path, &details, true) => Ok(()),
        _ => Err(capture_error(format!(
            "{label} must be an ordinary directory."
        ))),
    }
}

/// `_require_ordinary_ancestors(path, label)`: `path` and each parent up to
/// the root are ordinary directories.
fn require_ordinary_ancestors(path: &PyPath, label: &str) -> Result<(), Exc> {
    let mut current = path.clone();
    loop {
        let text = current.display();
        match lstat_ok(&text) {
            Some(details) if ordinary(&text, &details, true) => {}
            _ => {
                return Err(capture_error(format!(
                    "{label} must have ordinary ancestors."
                )));
            }
        }
        let parent = current.parent();
        if parent.same(&current) {
            return Ok(());
        }
        current = parent;
    }
}

/// `_same_regular_file_identity(expected, observed)` for two looks at `path`.
fn identical(path: &str, expected: &(bool, (u64, u64)), observed: &(bool, (u64, u64))) -> bool {
    expected.0 && observed.0 && expected.1 == observed.1 && injected(Step::Identity(path)).is_none()
}

fn handle_identity(file: &std::fs::File) -> std::io::Result<(bool, (u64, u64))> {
    Ok((file.metadata()?.is_file(), pyos::handle_identity(file)?))
}

/// A file read twice through one handle, with the identities around it.
struct StableRead {
    opened: (bool, (u64, u64)),
    content: Vec<u8>,
    confirmed: Vec<u8>,
    read: (bool, (u64, u64)),
    read_size: u64,
    finished: (bool, (u64, u64)),
}

/// Open `path`, refuse a different object than `expected` (`changed`), read
/// up to `limit` bytes, read again from the start, and stat it again.
fn read_twice(
    path: &str,
    expected: &(bool, (u64, u64)),
    limit: u64,
    changed: &str,
) -> Result<Result<StableRead, Exc>, std::io::Error> {
    let target = OPEN_HOOK
        .with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(path)))
        .unwrap_or_else(|| path.to_string());
    let mut file = std::fs::File::open(&target)?;
    io_fault(Step::Read(path))?;
    let opened = handle_identity(&file)?;
    if !identical(path, expected, &opened) {
        return Ok(Err(capture_error(changed)));
    }
    let mut content = Vec::new();
    (&mut file).take(limit).read_to_end(&mut content)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut confirmed = Vec::new();
    (&mut file).take(limit).read_to_end(&mut confirmed)?;
    let read = handle_identity(&file)?;
    let read_size = file.metadata()?.len();
    drop(file);
    let (final_details, final_identity) = pyos::lstat_identity(path)?;
    let finished = (final_details.file_type().is_file(), final_identity);
    Ok(Ok(StableRead {
        opened,
        content,
        confirmed,
        read,
        read_size,
        finished,
    }))
}

/// `_load_manifest(path)`: the manifest's rows, read from one stable regular file.
fn load_manifest(path: &PyPath) -> Result<Vec<Json>, Exc> {
    let text = path.display();
    require_ordinary_ancestors(&path.parent(), "manifest")?;
    let (details, expected) = match lstat_identified(&text) {
        Some((details, expected)) if ordinary(&text, &details, false) => (details, expected),
        _ => return Err(capture_error("manifest must be an ordinary file.")),
    };
    if details.len() > MAX_MANIFEST_BYTES {
        return Err(capture_error("manifest exceeds the 8 MiB size limit."));
    }
    let unreadable = || capture_error("manifest could not be read.");
    let read = read_twice(
        &text,
        &expected,
        MAX_MANIFEST_BYTES + 1,
        "manifest changed during capture.",
    )
    .map_err(|_| unreadable())??;
    if !(identical(&text, &read.opened, &read.read)
        && identical(&text, &read.read, &read.finished)
        && read.content == read.confirmed
        && read.content.len() as u64 == read.read_size)
    {
        return Err(capture_error("manifest changed during capture."));
    }
    require_ordinary_ancestors(&path.parent(), "manifest")?;
    if read.content.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(capture_error("manifest exceeds the 8 MiB size limit."));
    }
    let source = String::from_utf8(read.content)
        .map_err(|_| capture_error("manifest must be strict UTF-8."))?;
    let value = pyjson::loads(&source).map_err(|error| match error {
        DecodeError::Duplicate => capture_error("manifest contains a duplicate JSON member."),
        DecodeError::Invalid(message) if !message.starts_with("Exceeds the limit") => {
            capture_error("manifest is invalid JSON.")
        }
        other => super::json_error(other),
    })?;
    let Json::Array(items) = value else {
        return Err(capture_error("manifest must be a non-empty array."));
    };
    if items.is_empty() {
        return Err(capture_error("manifest must be a non-empty array."));
    }
    for (index, item) in items.iter().enumerate() {
        if !matches!(item, Json::Object(_)) {
            return Err(capture_error(format!(
                "manifest title {} must be an object.",
                index + 1
            )));
        }
    }
    Ok(items)
}

/// `_project_baseline(rows)`: the monitor baseline the capture is checked against.
fn project_baseline(rows: &[Json]) -> Result<Json, Exc> {
    let mut titles: Vec<(String, Json)> = Vec::new();
    let mut retrieved_max: Option<P> = None;
    for (index, row) in rows.iter().enumerate() {
        let index = index + 1;
        let field = |name: &str| format!("manifest title {index} {name}");
        let title_id = register_id(get(row, "id")).map_err(|e| capture_error(value_message(e)))?;
        if titles.iter().any(|(id, _)| *id == title_id) {
            return Err(capture_error(
                "manifest contains duplicate Register identifiers.",
            ));
        }
        let collection = required_text(get(row, "collection"), &field("collection"))?;
        if !COLLECTIONS.iter().any(|c| pts(c) == collection) {
            return Err(capture_error(format!(
                "manifest title {index} collection is unsupported."
            )));
        }
        let compilation_date = date(get(row, "versionStart"), &field("versionStart"))?;
        let source_url = required_text(get(row, "sourceUrl"), &field("sourceUrl"))?;
        let compilation_text = pyvalue::points_lossy(&compilation_date);
        let expected_source_url = format!(
            "{REGISTER_SITE}/{title_id}/{compilation_text}/{compilation_text}/text/original/epub"
        );
        if source_url != pts(&expected_source_url) {
            return Err(capture_error(format!(
                "manifest title {index} sourceUrl does not match its title and version."
            )));
        }
        let Some(raw_number) = row.get("compilationNumber") else {
            return Err(capture_error(format!(
                "manifest title {index} compilationNumber is required."
            )));
        };
        let compilation_number = match raw_number {
            Json::Null => Json::Null,
            other => text_json(required_text(other, &field("compilationNumber"))?),
        };
        let current = match row.get("version_is_current") {
            None => true,
            Some(Json::Bool(b)) => *b,
            Some(_) => {
                return Err(capture_error(format!(
                    "manifest title {index} version_is_current must be a boolean."
                )));
            }
        };
        let current_start = match get(row, "current_version_start") {
            Json::Null => None,
            other => Some(date(other, &field("current_version_start"))?),
        };
        if !current && current_start.is_none() {
            return Err(capture_error(format!(
                "manifest title {index} current_version_start is required when not current."
            )));
        }
        let name = required_text(get(row, "name"), &field("name"))?;
        let retrieved = date(get(row, "retrieved"), &field("retrieved"))?;
        if retrieved_max.as_ref().is_none_or(|max| retrieved > *max) {
            retrieved_max = Some(retrieved.clone());
        }
        titles.push((
            title_id.clone(),
            obj(vec![
                ("register_id", Json::str(&title_id)),
                ("name", text_json(name)),
                ("collection", text_json(collection)),
                ("compilation_number", compilation_number),
                ("compilation_date", text_json(compilation_date)),
                ("version_is_current", Json::Bool(current)),
                (
                    "current_version_start",
                    current_start.map_or(Json::Null, text_json),
                ),
                ("retrieved", text_json(retrieved)),
                ("source_url", text_json(source_url)),
                (
                    "register_page",
                    Json::str(&format!("{REGISTER_SITE}/{title_id}/latest/text")),
                ),
            ]),
        ));
    }
    titles.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(obj(vec![
        ("corpus", Json::str(CORPUS)),
        ("retrieved", text_json(retrieved_max.unwrap_or_default())),
        ("source", Json::str(SOURCE)),
        ("source_api", Json::str(SOURCE_API)),
        (
            "titles",
            Json::Array(titles.into_iter().map(|(_, t)| t).collect()),
        ),
    ]))
}

/// `urllib.parse.quote(text, safe="")`.
fn quote(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn versions_url(pairs: &[(&str, String)]) -> String {
    let query: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote(k), quote(v)))
        .collect();
    format!("{SOURCE_API}versions?{}", query.join("&"))
}

/// `_current_url(title_id)`.
pub fn current_url(title_id: &str) -> String {
    versions_url(&[
        ("$top", "1".into()),
        (
            "$filter",
            format!("titleId eq '{title_id}' and isCurrent eq true"),
        ),
        ("$select", SELECT_FIELDS.into()),
    ])
}

/// `_history_url(title_id)`.
pub fn history_url(title_id: &str) -> String {
    versions_url(&[
        ("$top", "1".into()),
        ("$filter", format!("titleId eq '{title_id}'")),
        ("$orderby", "start desc".into()),
        ("$select", SELECT_FIELDS.into()),
    ])
}

// ------------------------------------------------------------- exchanges

/// `_normalised_headers(headers)`: the contract headers under lower-case
/// names, sorted, each value non-empty stripped text.
fn normalised_headers(headers: &[(Json, Json)]) -> Result<Vec<(String, P)>, Exc> {
    let mut selected: Vec<(String, P)> = Vec::new();
    for (name, value) in headers {
        let Json::Str(name) = name else {
            return Err(capture_error("response header names must be strings."));
        };
        let lower = pyvalue::points_lossy(&name.code_points()).to_lowercase();
        if RETAINED_HEADER_NAMES.contains(&lower.as_str()) {
            if selected.iter().any(|(k, _)| *k == lower) {
                return Err(capture_error(
                    "response contains an ambiguous selected header.",
                ));
            }
            let text = required_text(value, &format!("response header {lower}"))?;
            selected.push((lower, text));
        }
    }
    selected.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(selected)
}

fn headers_json(headers: &[(String, P)]) -> Json {
    Json::Object(
        headers
            .iter()
            .map(|(k, v)| (PyStr::Text(k.clone()), text_json(v.clone())))
            .collect(),
    )
}

/// `content_type.split(";", 1)[0].strip().lower()`.
fn media_type(content_type: &[u32]) -> String {
    let text = pyvalue::points_lossy(content_type);
    let first = text.split(';').next().unwrap_or("");
    crate::py::strip(first).to_lowercase()
}

/// `_validated_row(row, title_id, role)`, the failure category on refusal.
fn validated_row(row: &Json, title_id: &str, role: &str) -> Result<(), &'static str> {
    const SHAPE: &str = "INVALID_ODATA_SHAPE";
    if !row.has_exactly(&ROW_FIELDS) {
        return Err(SHAPE);
    }
    if !is(get(row, "titleId"), title_id) {
        return Err("IDENTITY_MISMATCH");
    }
    if start_date(get(row, "start")).is_err() {
        return Err(SHAPE);
    }
    let Json::Bool(current) = get(row, "isCurrent") else {
        return Err(SHAPE);
    };
    let status_ok = ["InForce", "Ceased", "Repealed", "NeverEffective"]
        .iter()
        .any(|s| matches!(get(row, "status"), Json::Str(v) if v.as_str() == Some(s)));
    if !status_ok || (role == "current" && !current) || (role == "history" && *current) {
        return Err(SHAPE);
    }
    let document_id = get(row, "registerId");
    let compilation_number = get(row, "compilationNumber");
    let registered_at = get(row, "registeredAt");
    if matches!(document_id, Json::Null) {
        if !matches!(registered_at, Json::Null) {
            return Err(SHAPE);
        }
        if !matches!(compilation_number, Json::Null)
            && required_text(compilation_number, "Register compilation number").is_err()
        {
            return Err(SHAPE);
        }
    } else {
        if register_id(document_id).is_err() {
            return Err(SHAPE);
        }
        if !matches!(compilation_number, Json::Null)
            && required_text(compilation_number, "Register compilation number").is_err()
        {
            return Err(SHAPE);
        }
        if version_timestamp(registered_at, "Register registration timestamp").is_err() {
            return Err(SHAPE);
        }
    }
    Ok(())
}

/// `_parse_odata_rows(body, title_id, role)`.
fn parse_odata_rows(
    body: &[u8],
    title_id: &str,
    role: &str,
) -> Result<Result<Vec<Json>, &'static str>, Exc> {
    let Ok(text) = std::str::from_utf8(body) else {
        return Ok(Err("INVALID_JSON"));
    };
    let document = match pyjson::loads(text) {
        Ok(document) => document,
        Err(DecodeError::Duplicate) => return Ok(Err("INVALID_JSON")),
        Err(DecodeError::Invalid(message)) if !message.starts_with("Exceeds the limit") => {
            return Ok(Err("INVALID_JSON"));
        }
        // Python parsed deeper, to its C-stack limit, then refused the shape.
        Err(DecodeError::Recursion(_)) => return Ok(Err("INVALID_ODATA_SHAPE")),
        Err(other) => return Err(super::json_error(other)),
    };
    if !document.has_exactly(&["@odata.context", "value"])
        || !is(get(&document, "@odata.context"), &odata_context())
    {
        return Ok(Err("INVALID_ODATA_SHAPE"));
    }
    let Json::Array(values) = get(&document, "value") else {
        return Ok(Err("INVALID_ODATA_SHAPE"));
    };
    if values.len() > 1 {
        return Ok(Err("INVALID_ODATA_SHAPE"));
    }
    for row in values {
        if let Err(category) = validated_row(row, title_id, role) {
            return Ok(Err(category));
        }
    }
    Ok(Ok(values.clone()))
}

/// `_EvaluatedExchange`.
struct Evaluated {
    request: Json,
    rows: Option<Vec<Json>>,
    error_category: Option<&'static str>,
    retained_body: Option<Vec<u8>>,
    media_type: Option<String>,
    response_sha256: Option<String>,
    evidence_path: Option<String>,
    checked_at: P,
}

/// `_evaluate_exchange(exchange, role=..., url=..., title_id=...)`.
fn evaluate_exchange(
    exchange: RegisterExchange,
    role: &str,
    url: &str,
    title_id: &str,
) -> Result<Evaluated, Exc> {
    let attempts_ok = int(&exchange.attempts).is_some_and(|a| (1..=3).contains(&a));
    let checked = (|| -> Result<(P, Vec<(String, P)>), Exc> {
        let checked_at = utc_timestamp(&exchange.checked_at, "exchange checked_at")?;
        if !attempts_ok {
            return Err(capture_error("invalid attempt count"));
        }
        let headers = exchange
            .headers
            .as_deref()
            .ok_or_else(|| capture_error("headers is not a mapping"))?;
        Ok((checked_at, normalised_headers(headers)?))
    })();
    let (checked_at, headers, mut problem) = match checked {
        Ok((checked_at, headers)) => (checked_at, headers, None),
        Err(error) if capture_message(&error).is_some() => (
            pts("1970-01-01T00:00:00Z"),
            Vec::new(),
            Some("INVALID_EXCHANGE"),
        ),
        Err(error) => return Err(error),
    };

    let mut response_sha256 = None;
    let mut evidence_path = None;
    let mut response_length = None;
    let mut retained_body = None;
    let mut media = None;
    let mut rows = None;
    let status = int(&exchange.status);
    let transport = &exchange.error_category;
    if problem.is_none() {
        if matches!(exchange.status, Json::Null) {
            problem = if is(transport, "TRANSPORT_ERROR") && exchange.body == Body::Absent {
                Some("TRANSPORT_ERROR")
            } else {
                Some("INVALID_EXCHANGE")
            };
        } else if !status.is_some_and(|s| (100..=599).contains(&s))
            || !matches!(transport, Json::Null)
        {
            problem = Some("INVALID_EXCHANGE");
        } else if status != Some(200) {
            problem = Some("HTTP_STATUS");
        } else {
            match &exchange.body {
                Body::Bytes(body) if body.len() > MAX_RESPONSE_BYTES => {
                    problem = Some("RESPONSE_TOO_LARGE");
                }
                Body::Bytes(body) => {
                    let digest = sha256_id(body);
                    response_length = Some(body.len());
                    evidence_path = Some(format!(
                        "evidence/sha256-{}.json",
                        digest.trim_start_matches("sha256:")
                    ));
                    response_sha256 = Some(digest);
                    retained_body = Some(body.clone());
                    media = headers
                        .iter()
                        .find(|(k, _)| k == "content-type")
                        .map(|(_, v)| media_type(v));
                    if media.as_deref() != Some("application/json") {
                        problem = Some("UNSUPPORTED_MEDIA_TYPE");
                    } else {
                        match parse_odata_rows(body, title_id, role)? {
                            Ok(parsed) => rows = Some(parsed),
                            Err(category) => problem = Some(category),
                        }
                    }
                }
                _ => problem = Some("INVALID_EXCHANGE"),
            }
        }
    }
    let attempt_count = match int(&exchange.attempts) {
        Some(a) if (1..=i64::from(MAX_ATTEMPTS)).contains(&a) => a,
        _ => 0,
    };
    let request = obj(vec![
        ("role", Json::str(role)),
        ("url", Json::str(url)),
        ("checked_at", text_json(checked_at.clone())),
        (
            "http_status",
            match &exchange.status {
                Json::Int(i) => Json::Int(i.clone()),
                _ => Json::Null,
            },
        ),
        (
            "transport_error_category",
            if problem == Some("TRANSPORT_ERROR") {
                Json::str("TRANSPORT_ERROR")
            } else {
                Json::Null
            },
        ),
        ("attempt_count", Json::Int(attempt_count.into())),
        ("response_headers", headers_json(&headers)),
        (
            "response_length",
            response_length.map_or(Json::Null, |n| Json::Int(n.into())),
        ),
        (
            "response_sha256",
            response_sha256.as_deref().map_or(Json::Null, Json::str),
        ),
        (
            "evidence_path",
            evidence_path.as_deref().map_or(Json::Null, Json::str),
        ),
    ]);
    Ok(Evaluated {
        request,
        rows,
        error_category: problem,
        retained_body,
        media_type: media,
        response_sha256,
        evidence_path,
        checked_at,
    })
}

/// `_retain_exchange_evidence(evaluated, evidence)`.
fn retain(evaluated: &Evaluated, evidence: &mut Vec<(String, Vec<u8>)>) -> Result<(), Exc> {
    let (Some(path), Some(body)) = (&evaluated.evidence_path, &evaluated.retained_body) else {
        return Ok(());
    };
    match evidence.iter().find(|(p, _)| p == path) {
        Some((_, prior)) if prior != body => {
            Err(capture_error("response digest collision detected"))
        }
        Some(_) => Ok(()),
        None => {
            evidence.push((path.clone(), body.clone()));
            Ok(())
        }
    }
}

fn opt(value: Option<String>) -> Json {
    value.as_deref().map_or(Json::Null, Json::str)
}

/// `_observe_title(title, session, evidence)`: the capture result and the
/// observation for one title.
fn observe_title(
    title: &Json,
    session: &mut dyn RegisterSession,
    evidence: &mut Vec<(String, Vec<u8>)>,
) -> Result<(Json, Json), Exc> {
    let title_id = get(title, "register_id")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let url = current_url(&title_id);
    let current = evaluate_exchange(session.get(&url)?, "current", &url, &title_id)?;
    retain(&current, evidence)?;
    let mut requests = vec![current.request.clone()];
    let mut last_checked = current.checked_at.clone();
    let mut state = "LOOKUP_FAILED";
    let mut error_category = current.error_category;
    let (mut observed_number, mut observed_date, mut observed_document, mut current_start) =
        (None::<Json>, None::<String>, None::<Json>, None::<String>);
    if let (None, Some(rows)) = (error_category, &current.rows) {
        if rows.is_empty() {
            let url = history_url(&title_id);
            let history = evaluate_exchange(session.get(&url)?, "history", &url, &title_id)?;
            retain(&history, evidence)?;
            requests.push(history.request.clone());
            last_checked = history.checked_at.clone();
            if history.error_category.is_some() {
                error_category = history.error_category;
            } else if history.rows.as_ref().is_none_or(Vec::is_empty) {
                error_category = Some("NO_VERSION_EVIDENCE");
            } else {
                state = "NO_LONGER_IN_FORCE";
                error_category = None;
            }
        } else if !is(get(&rows[0], "status"), "InForce") {
            // A repealed or ceased title is its own current row, so the status
            // is the in-force fact.
            state = "NO_LONGER_IN_FORCE";
            error_category = None;
        } else {
            let row = &rows[0];
            let current_date = start_date(get(row, "start"))?;
            let title_date = get(title, "compilation_date").as_str().unwrap_or_default();
            let title_number = get(title, "compilation_number");
            let number = get(row, "compilationNumber");
            if current_date.as_str() < title_date {
                error_category = Some("INCONSISTENT_CHRONOLOGY");
            } else if matches!(get(row, "registerId"), Json::Null) {
                state = "CURRENT_NO_PUBLISHED_COMPILATION";
                current_start = Some(current_date);
                error_category = None;
            } else if py_eq(number, title_number) && current_date == title_date {
                state = "UNCHANGED";
                error_category = None;
            } else if !matches!(number, Json::Null)
                && !py_eq(number, title_number)
                && current_date.as_str() > title_date
            {
                state = "SUPERSEDED";
                error_category = None;
                observed_number = Some(number.clone());
                observed_date = Some(current_date);
                observed_document = Some(get(row, "registerId").clone());
            } else {
                error_category = Some("INCONSISTENT_CHRONOLOGY");
            }
        }
    }
    let collection = get(title, "collection").clone();
    let result = obj(vec![
        ("register_id", Json::str(&title_id)),
        ("collection", collection.clone()),
        ("checked_at", text_json(last_checked.clone())),
        ("state", Json::str(state)),
        ("error_category", opt(error_category.map(str::to_string))),
        ("requests", Json::Array(requests)),
    ]);
    let result_sha256 = sha256_id(&json_bytes(&result)?);
    let hex = result_sha256.trim_start_matches("sha256:");
    let observation = obj(vec![
        ("register_id", Json::str(&title_id)),
        ("collection", collection),
        ("state", Json::str(state)),
        (
            "evidence_id",
            Json::str(&format!("frl:{title_id}:{}", &hex[..32])),
        ),
        (
            "observed_compilation_number",
            observed_number.unwrap_or(Json::Null),
        ),
        ("observed_compilation_date", opt(observed_date)),
        (
            "observed_register_document_id",
            observed_document.unwrap_or(Json::Null),
        ),
        ("current_version_start", opt(current_start)),
        ("evidence_url", get(title, "register_page").clone()),
        ("checked_at", text_json(last_checked)),
        ("error_category", opt(error_category.map(str::to_string))),
        ("capture_result_sha256", Json::str(&result_sha256)),
        (
            "primary_response_sha256",
            opt(current.response_sha256.clone()),
        ),
        (
            "primary_response_media_type",
            if current.response_sha256.is_some() {
                opt(current.media_type.clone())
            } else {
                Json::Null
            },
        ),
    ]);
    Ok((result, observation))
}

// ---------------------------------------------------- the staged graph

/// `_require_staged_directory(path)`.
fn require_staged_directory(path: &str) -> Result<(), Exc> {
    let details =
        lstat_ok(path).ok_or_else(|| capture_error("staged capture directory is unavailable"))?;
    if !ordinary(path, &details, true) {
        return Err(capture_error(
            "staged capture contains a non-ordinary directory",
        ));
    }
    Ok(())
}

/// `_read_staged_file(path)`: the bytes of one stable, ordinary staged file.
fn read_staged_file(path: &str) -> Result<Vec<u8>, Exc> {
    let (details, expected) = lstat_identified(path)
        .ok_or_else(|| capture_error("staged capture file is unavailable"))?;
    if !ordinary(path, &details, false) {
        return Err(capture_error("staged capture contains a non-ordinary file"));
    }
    let unreadable = || capture_error("staged capture file could not be read");
    let read = read_twice(
        path,
        &expected,
        details.len() + 1,
        "staged capture file changed during validation",
    )
    .map_err(|_| unreadable())??;
    if !(identical(path, &read.opened, &read.read)
        && identical(path, &read.read, &read.finished)
        && read.content == read.confirmed
        && read.content.len() as u64 == read.read_size
        && read.read_size == details.len())
    {
        return Err(capture_error(
            "staged capture file changed during validation",
        ));
    }
    Ok(read.content)
}

/// `_load_generated_object(path)`: canonical JSON object bytes and value.
fn load_generated_object(path: &str) -> Result<(Vec<u8>, Json), Exc> {
    let content = read_staged_file(path)?;
    let invalid = || capture_error("staged capture JSON is invalid");
    let text = std::str::from_utf8(&content).map_err(|_| invalid())?;
    let document = pyjson::loads(text).map_err(|error| match error {
        DecodeError::Duplicate => invalid(),
        DecodeError::Invalid(message) if !message.starts_with("Exceeds the limit") => invalid(),
        other => super::json_error(other),
    })?;
    if !matches!(document, Json::Object(_)) || content != json_bytes(&document)? {
        return Err(capture_error("staged capture JSON bytes are non-canonical"));
    }
    Ok((content, document))
}

fn require_exact_fields(value: &Json, fields: &[&str]) -> Result<(), Exc> {
    if !value.has_exactly(fields) {
        return Err(capture_error("staged capture object shape is invalid"));
    }
    Ok(())
}

/// `value in {set of strings}`: a list or dict cannot be hashed.
fn member(value: &Json, set: &[&str]) -> Result<bool, Exc> {
    match value {
        Json::Array(_) | Json::Object(_) => Err(Exc::Type("unhashable type".into())),
        _ => Ok(set.iter().any(|s| is(value, s))),
    }
}

/// Entry names of a directory; a name that is not Unicode never matches.
fn entry_names(path: &str) -> std::io::Result<Vec<Option<String>>> {
    io_fault(Step::List(path))?;
    std::fs::read_dir(path)?
        .map(|entry| entry.map(|e| e.file_name().into_string().ok()))
        .collect()
}

fn object_keys(value: &Json) -> Vec<String> {
    match value {
        Json::Object(pairs) => pairs
            .iter()
            .map(|(k, _)| pyvalue::points_lossy(&k.code_points()))
            .collect(),
        _ => Vec::new(),
    }
}

const TITLE_FIELDS: [&str; 10] = [
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
const REQUEST_FIELDS: [&str; 10] = [
    "role",
    "url",
    "checked_at",
    "http_status",
    "transport_error_category",
    "attempt_count",
    "response_headers",
    "response_length",
    "response_sha256",
    "evidence_path",
];
const ITEM_FIELDS: [&str; 14] = [
    "register_id",
    "collection",
    "state",
    "evidence_id",
    "observed_compilation_number",
    "observed_compilation_date",
    "observed_register_document_id",
    "current_version_start",
    "evidence_url",
    "checked_at",
    "error_category",
    "capture_result_sha256",
    "primary_response_sha256",
    "primary_response_media_type",
];

/// `_validate_staged_graph_inner(staging)`.
fn validate_staged_graph_inner(staging: &PyPath) -> Result<(), Exc> {
    let root = staging.display();
    require_staged_directory(&root)?;
    let names =
        entry_names(&root).map_err(|_| capture_error("staged capture layout could not be read"))?;
    let expected: BTreeSet<Option<String>> = [
        "monitor-baseline.json",
        "register-capture.json",
        "register-observation.json",
        "evidence",
    ]
    .iter()
    .map(|n| Some((*n).to_string()))
    .collect();
    if names.into_iter().collect::<BTreeSet<_>>() != expected {
        return Err(capture_error("staged capture root layout is invalid"));
    }
    let evidence_directory = staging.join("evidence").display();
    require_staged_directory(&evidence_directory)?;
    let (baseline_content, baseline) =
        load_generated_object(&staging.join("monitor-baseline.json").display())?;
    let (capture_content, capture) =
        load_generated_object(&staging.join("register-capture.json").display())?;
    let (_, observation) =
        load_generated_object(&staging.join("register-observation.json").display())?;

    require_exact_fields(
        &baseline,
        &["corpus", "retrieved", "source", "source_api", "titles"],
    )?;
    let titles = match get(&baseline, "titles") {
        Json::Array(items) if !items.is_empty() => Some(items),
        _ => None,
    };
    let (true, true, true, Some(titles)) = (
        is(get(&baseline, "corpus"), CORPUS),
        is(get(&baseline, "source"), SOURCE),
        is(get(&baseline, "source_api"), SOURCE_API),
        titles,
    ) else {
        return Err(capture_error("staged capture baseline is invalid"));
    };
    date(get(&baseline, "retrieved"), "baseline retrieved")?;
    let mut title_order: Vec<String> = Vec::new();
    for title in titles {
        if !matches!(title, Json::Object(_)) {
            return Err(capture_error("staged capture baseline title is invalid"));
        }
        require_exact_fields(title, &TITLE_FIELDS)?;
        let title_id = register_id(get(title, "register_id"))
            .map_err(|_| capture_error("staged capture baseline identity is invalid"))?;
        if title_order.contains(&title_id) {
            return Err(capture_error(
                "staged capture baseline identity is duplicated",
            ));
        }
        if !member(get(title, "collection"), &COLLECTIONS)? {
            return Err(capture_error(
                "staged capture baseline collection is invalid",
            ));
        }
        let compilation_date = date(get(title, "compilation_date"), "baseline compilation date")?;
        date(get(title, "retrieved"), "baseline title retrieved")?;
        let Json::Bool(is_current) = get(title, "version_is_current") else {
            return Err(capture_error(
                "staged capture baseline current flag is invalid",
            ));
        };
        let current_start = get(title, "current_version_start");
        if !matches!(current_start, Json::Null) {
            date(current_start, "baseline current version start")?;
        }
        if !is_current && matches!(current_start, Json::Null) {
            return Err(capture_error(
                "staged capture baseline current date is missing",
            ));
        }
        required_text(get(title, "name"), "baseline title name")?;
        let number = get(title, "compilation_number");
        if !matches!(number, Json::Null) {
            required_text(number, "baseline compilation number")?;
        }
        let compilation_text = pyvalue::points_lossy(&compilation_date);
        let expected_source = format!(
            "{REGISTER_SITE}/{title_id}/{compilation_text}/{compilation_text}/text/original/epub"
        );
        if !is(get(title, "source_url"), &expected_source) {
            return Err(capture_error(
                "staged capture baseline source URL is invalid",
            ));
        }
        if !is(
            get(title, "register_page"),
            &format!("{REGISTER_SITE}/{title_id}/latest/text"),
        ) {
            return Err(capture_error("staged capture baseline page URL is invalid"));
        }
        title_order.push(title_id);
    }
    let mut sorted = title_order.clone();
    sorted.sort();
    if title_order != sorted {
        return Err(capture_error("staged capture baseline order is invalid"));
    }
    // Every retrieved date is a validated ISO date, so text order is date order.
    let retrieved_max = titles
        .iter()
        .filter_map(|t| match get(t, "retrieved") {
            Json::Str(s) => Some(s.code_points()),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    if !py_eq(get(&baseline, "retrieved"), &text_json(retrieved_max)) {
        return Err(capture_error(
            "staged capture baseline retrieval date is invalid",
        ));
    }
    let title_of = |id: &str| -> &Json {
        titles
            .iter()
            .find(|t| is(get(t, "register_id"), id))
            .expect("a validated title")
    };

    require_exact_fields(
        &capture,
        &[
            "schema_version",
            "mode",
            "observed_at",
            "source_api",
            "baseline_sha256",
            "expected_register_ids",
            "complete",
            "results",
        ],
    )?;
    let order_json = Json::Array(title_order.iter().map(|id| Json::str(id)).collect());
    let results = match get(&capture, "results") {
        Json::Array(items) if items.len() == title_order.len() => Some(items),
        _ => None,
    };
    let consistent = is(get(&capture, "schema_version"), CAPTURE_SCHEMA)
        && is(get(&capture, "mode"), "live")
        && is(get(&capture, "source_api"), SOURCE_API)
        && is(
            get(&capture, "baseline_sha256"),
            &sha256_id(&baseline_content),
        )
        && py_eq(get(&capture, "expected_register_ids"), &order_json)
        && matches!(get(&capture, "complete"), Json::Bool(true));
    let (true, Some(results)) = (consistent, results) else {
        return Err(capture_error("staged capture manifest is inconsistent"));
    };
    utc_timestamp(get(&capture, "observed_at"), "capture observed_at")?;

    let mut declared: Vec<(String, (String, i64))> = Vec::new();
    for (index, result) in results.iter().enumerate() {
        if !matches!(result, Json::Object(_)) {
            return Err(capture_error("staged capture result is invalid"));
        }
        require_exact_fields(
            result,
            &[
                "register_id",
                "collection",
                "checked_at",
                "state",
                "error_category",
                "requests",
            ],
        )?;
        let title_id = &title_order[index];
        if !is(get(result, "register_id"), title_id) {
            return Err(capture_error("staged capture result identity is invalid"));
        }
        let title = title_of(title_id);
        if !py_eq(get(result, "collection"), get(title, "collection")) {
            return Err(capture_error("staged capture result collection is invalid"));
        }
        let state = get(result, "state");
        if !member(state, &OBSERVATION_STATES)? {
            return Err(capture_error("staged capture result state is invalid"));
        }
        let failed = member(get(result, "error_category"), &FAILURE_CATEGORIES)?;
        if is(state, "LOOKUP_FAILED") != failed {
            return Err(capture_error(
                "staged capture result failure category is invalid",
            ));
        }
        let requests = match get(result, "requests") {
            Json::Array(items) if (1..=2).contains(&items.len()) => items,
            _ => return Err(capture_error("staged capture request list is invalid")),
        };
        let roles: Vec<&Json> = requests
            .iter()
            .filter(|r| matches!(r, Json::Object(_)))
            .map(|r| get(r, "role"))
            .collect();
        let want: &[&str] = if requests.len() == 1 {
            &["current"]
        } else {
            &["current", "history"]
        };
        if roles.len() != want.len() || roles.iter().zip(want).any(|(r, w)| !is(r, w)) {
            return Err(capture_error("staged capture request roles are invalid"));
        }
        for request in requests {
            require_exact_fields(request, &REQUEST_FIELDS)?;
            let expected_url = if is(get(request, "role"), "current") {
                current_url(title_id)
            } else {
                history_url(title_id)
            };
            if !is(get(request, "url"), &expected_url) {
                return Err(capture_error("staged capture request URL is invalid"));
            }
            utc_timestamp(get(request, "checked_at"), "request checked_at")?;
            let headers = get(request, "response_headers");
            let metadata_ok = int(get(request, "attempt_count"))
                .is_some_and(|a| (0..=i64::from(MAX_ATTEMPTS)).contains(&a))
                && matches!(headers, Json::Object(_))
                && object_keys(headers)
                    .iter()
                    .all(|k| RETAINED_HEADER_NAMES.contains(&k.as_str()));
            if !metadata_ok {
                return Err(capture_error("staged capture request metadata is invalid"));
            }
            let Json::Object(pairs) = headers else {
                unreachable!("checked above")
            };
            let items: Vec<(Json, Json)> = pairs
                .iter()
                .map(|(k, v)| (Json::Str(k.clone()), v.clone()))
                .collect();
            let canonical = normalised_headers(&items).is_ok_and(|normal| {
                // Dict equality: the same members, in any order.
                normal.len() == pairs.len()
                    && normal.iter().all(|(k, v)| {
                        pairs.iter().any(|(pk, pv)| {
                            pk.as_str() == Some(k) && py_eq(pv, &text_json(v.clone()))
                        })
                    })
            });
            if !canonical {
                return Err(capture_error("staged capture request headers are invalid"));
            }
            let digest = get(request, "response_sha256");
            let evidence_path = get(request, "evidence_path");
            let response_length = get(request, "response_length");
            if matches!(digest, Json::Null) {
                if !matches!(evidence_path, Json::Null) || !matches!(response_length, Json::Null) {
                    return Err(capture_error(
                        "staged capture empty response declaration is invalid",
                    ));
                }
                continue;
            }
            let digest_text = digest.as_str().filter(|d| monitor_sha256(d));
            let length =
                int(response_length).filter(|n| (0..=MAX_RESPONSE_BYTES as i64).contains(n));
            let (Some(digest_text), true, Some(length)) = (
                digest_text,
                py_eq(get(request, "http_status"), &Json::Int(200.into())),
                length,
            ) else {
                return Err(capture_error(
                    "staged capture response declaration is invalid",
                ));
            };
            let expected_path = format!(
                "evidence/sha256-{}.json",
                digest_text.trim_start_matches("sha256:")
            );
            if !is(evidence_path, &expected_path) {
                return Err(capture_error("staged capture evidence path is invalid"));
            }
            let declaration = (digest_text.to_string(), length);
            match declared.iter().find(|(p, _)| *p == expected_path) {
                Some((_, prior)) if *prior != declaration => {
                    return Err(capture_error(
                        "staged capture evidence declarations are inconsistent",
                    ));
                }
                Some(_) => {}
                None => declared.push((expected_path, declaration)),
            }
        }
        let last = requests.last().expect("one or two requests");
        if !py_eq(get(result, "checked_at"), get(last, "checked_at")) {
            return Err(capture_error("staged capture result timestamp is invalid"));
        }
    }

    let files = entry_names(&evidence_directory)
        .map_err(|_| capture_error("staged capture evidence layout could not be read"))?;
    let actual: BTreeSet<Option<String>> = files
        .iter()
        .map(|n| n.as_ref().map(|n| format!("evidence/{n}")))
        .collect();
    let wanted: BTreeSet<Option<String>> = declared.iter().map(|(p, _)| Some(p.clone())).collect();
    if actual != wanted {
        return Err(capture_error(
            "staged capture evidence inventory is invalid",
        ));
    }
    for name in files.into_iter().flatten() {
        let content = read_staged_file(&pypath::join(&evidence_directory, &name))?;
        let (digest, length) = &declared
            .iter()
            .find(|(p, _)| *p == format!("evidence/{name}"))
            .expect("the inventory matched")
            .1;
        if content.len() as i64 != *length || sha256_id(&content) != *digest {
            return Err(capture_error("staged capture evidence digest is invalid"));
        }
    }

    require_exact_fields(
        &observation,
        &[
            "schema_version",
            "mode",
            "observed_at",
            "scope_id",
            "baseline_sha256",
            "capture_sha256",
            "expected_register_ids",
            "complete",
            "run_status",
            "observations",
        ],
    )?;
    let blocked = results.iter().any(|r| is(get(r, "state"), "LOOKUP_FAILED"));
    let items = match get(&observation, "observations") {
        Json::Array(items) if items.len() == title_order.len() => Some(items),
        _ => None,
    };
    let consistent = is(get(&observation, "schema_version"), OBSERVATION_SCHEMA)
        && is(get(&observation, "mode"), "live")
        && py_eq(
            get(&observation, "observed_at"),
            get(&capture, "observed_at"),
        )
        && is(get(&observation, "scope_id"), OBSERVATION_SCOPE)
        && py_eq(
            get(&observation, "baseline_sha256"),
            get(&capture, "baseline_sha256"),
        )
        && is(
            get(&observation, "capture_sha256"),
            &sha256_id(&capture_content),
        )
        && py_eq(get(&observation, "expected_register_ids"), &order_json)
        && matches!(get(&observation, "complete"), Json::Bool(true))
        && is(
            get(&observation, "run_status"),
            if blocked { "BLOCKED" } else { "VERIFIED" },
        );
    let (true, Some(items)) = (consistent, items) else {
        return Err(capture_error("staged capture observation is inconsistent"));
    };
    for (index, item) in items.iter().enumerate() {
        if !matches!(item, Json::Object(_)) {
            return Err(capture_error("staged capture observation item is invalid"));
        }
        require_exact_fields(item, &ITEM_FIELDS)?;
        let title_id = &title_order[index];
        let result = &results[index];
        let title = title_of(title_id);
        let result_sha256 = sha256_id(&json_bytes(result)?);
        let hex = result_sha256.trim_start_matches("sha256:");
        let field = |name: &str| get(item, name);
        if !(is(field("register_id"), title_id)
            && py_eq(field("collection"), get(title, "collection"))
            && py_eq(field("state"), get(result, "state"))
            && py_eq(field("checked_at"), get(result, "checked_at"))
            && py_eq(field("error_category"), get(result, "error_category"))
            && py_eq(field("evidence_url"), get(title, "register_page"))
            && is(field("capture_result_sha256"), &result_sha256)
            && is(
                field("evidence_id"),
                &format!("frl:{title_id}:{}", &hex[..32]),
            ))
        {
            return Err(capture_error(
                "staged capture observation item is inconsistent",
            ));
        }
        let Json::Array(requests) = get(result, "requests") else {
            unreachable!("validated above")
        };
        let primary = &requests[0];
        if !py_eq(
            field("primary_response_sha256"),
            get(primary, "response_sha256"),
        ) {
            return Err(capture_error(
                "staged capture primary digest is inconsistent",
            ));
        }
        let content_type = get(get(primary, "response_headers"), "content-type");
        let expected_media = match (get(primary, "response_sha256"), content_type) {
            (Json::Null, _) | (_, Json::Null) => Json::Null,
            (_, Json::Str(s)) => Json::str(&media_type(&s.code_points())),
            _ => Json::Null,
        };
        if !py_eq(field("primary_response_media_type"), &expected_media) {
            return Err(capture_error(
                "staged capture primary media type is inconsistent",
            ));
        }
        let state = pyvalue::points_lossy(&match field("state") {
            Json::Str(s) => s.code_points(),
            _ => Vec::new(),
        });
        let required: &[&str] = match state.as_str() {
            "SUPERSEDED" => &[
                "observed_compilation_number",
                "observed_compilation_date",
                "observed_register_document_id",
            ],
            "CURRENT_NO_PUBLISHED_COMPILATION" => &["current_version_start"],
            "LOOKUP_FAILED" => &["error_category"],
            _ => &[],
        };
        let conditional = [
            "observed_compilation_number",
            "observed_compilation_date",
            "observed_register_document_id",
            "current_version_start",
            "error_category",
        ];
        if conditional
            .iter()
            .any(|f| !required.contains(f) && !matches!(field(f), Json::Null))
        {
            return Err(capture_error(
                "staged capture state fields are inconsistent",
            ));
        }
        if required.iter().any(|f| matches!(field(f), Json::Null)) {
            return Err(capture_error(
                "staged capture required state field is missing",
            ));
        }
        let observed_date = field("observed_compilation_date");
        if !matches!(observed_date, Json::Null) {
            date(observed_date, "observed compilation date")?;
        }
        let start = field("current_version_start");
        if !matches!(start, Json::Null) {
            date(start, "observed current version start")?;
        }
        let document = field("observed_register_document_id");
        if !matches!(document, Json::Null) && register_id(document).is_err() {
            return Err(capture_error("staged capture document identity is invalid"));
        }
    }
    Ok(())
}

/// `sha256:` and 64 lowercase hex digits, as `SHA256_ID` matches.
fn monitor_sha256(text: &str) -> bool {
    super::monitor_contract::sha256_id(&pts(text))
}

/// `_validate_staged_graph(staging)`: re-read and prove the whole staged graph.
#[doc(hidden)]
pub fn validate_staged_graph(staging: &PyPath) -> Result<(), Exc> {
    match validate_staged_graph_inner(staging) {
        Ok(()) => Ok(()),
        Err(error) => match capture_message(&error) {
            Some(message) if message.starts_with("staged capture") => Err(error),
            Some(_) => Err(capture_error("staged capture graph is invalid")),
            None => match error {
                Exc::Os(_) | Exc::Type(_) | Exc::Value { .. } | Exc::UnicodeEncode(_) => {
                    Err(capture_error("staged capture graph is invalid"))
                }
                other => Err(other),
            },
        },
    }
}

/// `validate_capture_graph(capture_dir)`: validate one existing complete
/// capture graph, its path resolved strictly.
pub fn validate_capture_graph(capture_dir: &str) -> Result<(), Exc> {
    std::fs::metadata(capture_dir)
        .map_err(|e| OsError::new(&e, Some(capture_dir), Style::Native))?;
    let resolved = pypath::realpath(capture_dir)?;
    validate_staged_graph(&PyPath::new(&resolved))
}

// ----------------------------------------------------------- publishing

/// `_require_absent_destination(path)`.
fn require_absent_destination(path: &str) -> Result<(), Exc> {
    match lstat(path) {
        Err(error) if error.not_found() => Ok(()),
        Err(_) => Err(capture_error("capture destination could not be inspected.")),
        Ok(_) => Err(capture_error("capture destination must not exist.")),
    }
}

fn safe_output_leaf(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 100
        && name.is_ascii()
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `_preflight_output(output, manifest)`: whether the one permitted missing
/// output parent must be created.
fn preflight_output(output: &PyPath, manifest: &str) -> Result<bool, Exc> {
    if !safe_output_leaf(output.name()) {
        return Err(capture_error("capture destination name is unsafe."));
    }
    let output_text = output.display();
    require_absent_destination(&output_text)?;
    if same_location(&output_text, manifest)? {
        return Err(capture_error(
            "capture destination must not replace its manifest input.",
        ));
    }
    let parent = output.parent();
    match lstat(&parent.display()) {
        Err(error) if error.not_found() => {
            if !safe_output_leaf(parent.name()) {
                return Err(capture_error("capture output parent name is unsafe."));
            }
            if lstat(&parent.parent().display()).is_err() {
                return Err(capture_error(
                    "capture permits only one missing output parent.",
                ));
            }
            require_ordinary_ancestors(&parent.parent(), "capture output")?;
            Ok(true)
        }
        Err(_) => Err(capture_error(
            "capture output parent could not be inspected.",
        )),
        Ok(_) => {
            require_ordinary_directory(&parent.display(), "capture output parent")?;
            require_ordinary_ancestors(&parent.parent(), "capture output")?;
            Ok(false)
        }
    }
}

/// `_remove_owned_staging(staging, parent=..., prefix=...)`.
#[doc(hidden)]
pub fn remove_owned_staging(staging: &PyPath, parent: &PyPath, prefix: &str) -> Result<(), Exc> {
    if !staging.parent().same(parent) || !staging.name().starts_with(prefix) {
        return Err(capture_error(
            "refusing to remove staging outside the capture boundary",
        ));
    }
    let text = staging.display();
    let details = match lstat(&text) {
        Ok(details) => details,
        Err(error) if error.not_found() => return Ok(()),
        Err(_) => return Err(capture_error("capture staging could not be inspected")),
    };
    if !ordinary(&text, &details, true) {
        return Err(capture_error(
            "capture staging is no longer an ordinary directory",
        ));
    }
    io_fault(Step::Remove(&text))
        .and_then(|()| std::fs::remove_dir_all(&text))
        .map_err(|_| capture_error("capture staging could not be removed"))
}

/// `os.mkdir(path, 0o700)`, private on Windows too.
fn mkdir_private(path: &str) -> std::io::Result<()> {
    io_fault(Step::Mkdir(path))?;
    pyos::mkdir_private(path)
}

fn write_new(path: &str, content: &[u8]) -> std::io::Result<()> {
    io_fault(Step::Write(path))?;
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)?;
    std::io::Write::write_all(&mut file, content)
}

/// `_rename_no_replace(staging, output)`: never replaces a destination on
/// Windows or Linux; elsewhere `os.rename`, as the Python stage does.
fn promote(staging: &str, output: &str) -> Result<(), OsError> {
    if let Some(error) = injected(Step::Promote(staging, output)) {
        return Err(error);
    }
    #[cfg(any(windows, target_os = "linux"))]
    {
        pyos::rename_no_replace(staging, output)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::fs::rename(staging, output)
            .map_err(|e| OsError::two(&e, staging, output, Style::Native))
    }
}

/// The three contract paths a capture writes.
#[derive(Debug)]
pub struct CapturePaths {
    pub baseline: String,
    pub capture: String,
    pub observation: String,
}

/// `capture_register_run(manifest, destination, session=...)`: capture
/// complete Register evidence into one new immutable directory.
pub fn capture_register_run(
    manifest_path: &str,
    destination: &str,
    session: &mut dyn RegisterSession,
) -> Result<CapturePaths, Exc> {
    let observed_at = utc_timestamp(&session.observed_at(), "session observed_at")?;
    let manifest = PyPath::new(&abspath(manifest_path));
    let output = PyPath::new(&abspath(destination));
    let baseline = project_baseline(&load_manifest(&manifest)?)?;
    let create_output_parent = preflight_output(&output, &manifest.display())?;

    let baseline_content = json_bytes(&baseline)?;
    let baseline_sha256 = sha256_id(&baseline_content);
    let mut evidence: Vec<(String, Vec<u8>)> = Vec::new();
    let mut results = Vec::new();
    let mut observations = Vec::new();
    let Json::Array(titles) = get(&baseline, "titles") else {
        unreachable!("project_baseline returns titles")
    };
    for title in titles {
        let (result, item) = observe_title(title, session, &mut evidence)?;
        results.push(result);
        observations.push(item);
    }
    let expected_ids: Vec<Json> = titles
        .iter()
        .map(|t| get(t, "register_id").clone())
        .collect();
    let complete = results.len() == expected_ids.len();
    let blocked = observations
        .iter()
        .any(|o| is(get(o, "state"), "LOOKUP_FAILED"));
    let capture = obj(vec![
        ("schema_version", Json::str(CAPTURE_SCHEMA)),
        ("mode", Json::str("live")),
        ("observed_at", text_json(observed_at.clone())),
        ("source_api", Json::str(SOURCE_API)),
        ("baseline_sha256", Json::str(&baseline_sha256)),
        ("expected_register_ids", Json::Array(expected_ids.clone())),
        ("complete", Json::Bool(complete)),
        ("results", Json::Array(results)),
    ]);
    let capture_content = json_bytes(&capture)?;
    let observation = obj(vec![
        ("schema_version", Json::str(OBSERVATION_SCHEMA)),
        ("mode", Json::str("live")),
        ("observed_at", text_json(observed_at)),
        ("scope_id", Json::str(OBSERVATION_SCOPE)),
        ("baseline_sha256", Json::str(&baseline_sha256)),
        ("capture_sha256", Json::str(&sha256_id(&capture_content))),
        ("expected_register_ids", Json::Array(expected_ids)),
        ("complete", Json::Bool(complete)),
        (
            "run_status",
            Json::str(if complete && !blocked {
                "VERIFIED"
            } else {
                "BLOCKED"
            }),
        ),
        ("observations", Json::Array(observations)),
    ]);
    let observation_content = json_bytes(&observation)?;

    let parent = output.parent();
    let parent_text = parent.display();
    if create_output_parent && mkdir_private(&parent_text).is_err() {
        return Err(capture_error("capture output parent could not be created."));
    }
    require_ordinary_directory(&parent_text, "capture output parent")?;
    require_ordinary_ancestors(&parent.parent(), "capture output")?;
    let output_text = output.display();
    require_absent_destination(&output_text)?;

    let prefix = format!(".{}.register-capture-", output.name());
    let staging = parent.join(&format!("{prefix}{}.tmp", pyos::try_random_hex(32)?));
    let staging_text = staging.display();
    let created = std::cell::Cell::new(false);
    let outcome = (|| -> Result<(), Exc> {
        let written = (|| -> std::io::Result<()> {
            mkdir_private(&staging_text)?;
            created.set(true);
            mkdir_private(&staging.join("evidence").display())?;
            write_new(
                &staging.join("monitor-baseline.json").display(),
                &baseline_content,
            )?;
            for (relative, content) in &evidence {
                write_new(&pypath::join(&staging_text, relative), content)?;
            }
            write_new(
                &staging.join("register-capture.json").display(),
                &capture_content,
            )?;
            write_new(
                &staging.join("register-observation.json").display(),
                &observation_content,
            )
        })();
        written.map_err(|_| capture_error("capture output could not be written."))?;
        injected(Step::Validate(&staging_text));
        validate_staged_graph(&staging)?;
        injected(Step::Validated(&staging_text));
        require_ordinary_directory(&parent_text, "capture output parent")?;
        require_absent_destination(&output_text)?;
        promote(&staging_text, &output_text)
            .map_err(|_| capture_error("capture output could not be promoted."))
    })();
    // A cleanup failure replaces any earlier one, as in Python's `finally`;
    // only a staging directory this run created is its to remove.
    if created.get() {
        remove_owned_staging(&staging, &parent, &prefix)?;
    }
    outcome?;
    Ok(CapturePaths {
        baseline: output.join("monitor-baseline.json").display(),
        capture: output.join("register-capture.json").display(),
        observation: output.join("register-observation.json").display(),
    })
}

fn parser() -> Parser {
    Parser::new("capture_register")
        .description("Capture complete live Federal Register metadata evidence.")
        .arg(Action::positional("manifest").help("rich corpus manifest JSON"))
        .arg(
            Action::option("--out")
                .required()
                .help("new immutable capture directory"),
        )
}

/// `python -m fadden capture_register manifest --out DIR`.
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    main_with(argv, run, &mut |connect| connect())
}

/// Makes the session [`main_with`] captures with, given the network's.
pub type SessionFactory =
    dyn FnMut(&dyn Fn() -> Box<dyn RegisterSession>) -> Box<dyn RegisterSession>;

/// [`main`] with the session `session` makes from the network's; tests hand
/// in their own, as patching `_HttpsRegisterSession` did.
#[doc(hidden)]
pub fn main_with(
    argv: &[String],
    run: &mut Run,
    session: &mut SessionFactory,
) -> Result<i32, Exit> {
    let parser = parser();
    let args = parser.parse(argv)?;
    let arg = |name: &str| args.str(name).unwrap_or_default();
    let connect = || -> Box<dyn RegisterSession> { Box::new(HttpsSession::new(http::connect())) };
    let mut session = session(&connect);
    match capture_register_run(&arg("manifest"), &arg("out"), session.as_mut()) {
        Ok(paths) => {
            run.print(&format!("baseline: {}", paths.baseline));
            run.print(&format!("capture: {}", paths.capture));
            run.print(&format!("observation: {}", paths.observation));
            Ok(0)
        }
        Err(Exc::Value {
            class: CLASS,
            message,
        }) => Err(parser.error(&message)),
        Err(error) => Ok(run.uncaught(&error)),
    }
}
