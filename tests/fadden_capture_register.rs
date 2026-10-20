//! `fadden capture_register`: ported from tests/corpus/test_live_register_capture.py
//! and test_capture_register_paths.py. A [`MemorySession`] stands in for the
//! Register, as the Python tests' session does; the production session runs
//! against a scripted [`Http`]. The Python stage remains what the publication
//! workflow runs, so its own tests stay too.
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::capture_register::{
    self as capture, Body, HttpsSession, RegisterExchange, RegisterSession, Step,
    capture_register_run, validate_capture_graph,
};
use tax_radar_au::fadden::http::{Exchange, Failure, Http, Response};
use tax_radar_au::fadden::{Exc, Run};
use tax_radar_au::pyjson::{self, Json};
use tax_radar_au::pyos::OsError;
use tax_radar_au::pypath::PyPath;

const CURRENT_URL: &str = "https://api.prod.legislation.gov.au/v1/versions?%24top=1&\
    %24filter=titleId%20eq%20%27C2004A00467%27%20and%20isCurrent%20eq%20true&\
    %24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt";
const INSTRUMENT_URL: &str = "https://api.prod.legislation.gov.au/v1/versions?%24top=1&\
    %24filter=titleId%20eq%20%27F2020L01498%27%20and%20isCurrent%20eq%20true&\
    %24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt";
const HISTORY_URL: &str = "https://api.prod.legislation.gov.au/v1/versions?%24top=1&\
    %24filter=titleId%20eq%20%27C2004A00467%27&%24orderby=start%20desc&\
    %24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt";
const CURRENT_BODY: &[u8] = b"{\"@odata.context\":\"https://api.prod.legislation.gov.au/v1/\
$metadata#Versions(titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)\",\
\"value\":[{\"titleId\":\"C2004A00467\",\"start\":\"2026-06-30T00:00:00\",\
\"isCurrent\":true,\"status\":\"InForce\",\"registerId\":\"C2026C00290\",\
\"registeredAt\":\"2026-07-14T15:44:31.0377770\",\"compilationNumber\":\"32\"}]}";
const CONTENT_TYPE: &str =
    "application/json; odata.metadata=minimal; odata.streaming=true; charset=utf-8";
/// A change to one graph document.
type Edit = Box<dyn FnOnce(&mut Value)>;
/// A fault hook.
type Hook = Box<dyn FnMut(Step) -> Option<OsError>>;

const CONTEXT: &str = "https://api.prod.legislation.gov.au/v1/$metadata#Versions(titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)";

// ---------------------------------------------------------------- fixtures

fn current_url_for(title_id: &str) -> String {
    CURRENT_URL.replace("C2004A00467", title_id)
}

fn history_url_for(title_id: &str) -> String {
    HISTORY_URL.replace("C2004A00467", title_id)
}

/// The capture's JSON byte representation of a fixture value.
fn json_bytes(value: &Value) -> Vec<u8> {
    let json = pyjson::loads(&value.to_string()).unwrap();
    tax_radar_au::fadden::publication_bundles::bundle_bytes(&json).unwrap()
}

fn sha256_id(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn manifest_row(changes: Value) -> Value {
    let mut row = json!({
        "id": "C2004A00467",
        "name": "A New Tax System (Australian Business Number) Act 1999",
        "collection": "Act",
        "versionStart": "2026-06-30",
        "compilationNumber": "32",
        "retrieved": "2026-08-04",
        "sourceUrl": "https://www.legislation.gov.au/C2004A00467/2026-06-30/2026-06-30/text/original/epub"
    });
    for (key, value) in changes.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

fn instrument_row(changes: Value) -> Value {
    let mut row = manifest_row(json!({
        "id": "F2020L01498",
        "name": "A New Tax System (Australian Business Number) Regulations 2020",
        "collection": "LegislativeInstrument",
        "versionStart": "2025-10-04",
        "compilationNumber": "2",
        "sourceUrl": "https://www.legislation.gov.au/F2020L01498/2025-10-04/2025-10-04/text/original/epub"
    }));
    for (key, value) in changes.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

/// A one-row versions document with the fields in the Register's order.
fn version_body(
    title_id: &str,
    start: &str,
    number: Option<&str>,
    document: Option<&str>,
    current: bool,
    status: &str,
) -> Vec<u8> {
    let registered = document.map(|_| "2026-08-20T01:02:03.1234567");
    json!({
        "@odata.context": CONTEXT,
        "value": [{
            "titleId": title_id,
            "start": format!("{start}T00:00:00"),
            "isCurrent": current,
            "status": status,
            "registerId": document,
            "registeredAt": registered,
            "compilationNumber": number
        }]
    })
    .to_string()
    .into_bytes()
}

fn empty_versions_body() -> Vec<u8> {
    json!({"@odata.context": CONTEXT, "value": []})
        .to_string()
        .into_bytes()
}

fn headers(pairs: &[(&str, &str)]) -> Option<Vec<(Json, Json)>> {
    Some(
        pairs
            .iter()
            .map(|(k, v)| (Json::str(k), Json::str(v)))
            .collect(),
    )
}

fn exchange_with(
    body: Body,
    status: Json,
    checked_at: &str,
    headers: Option<Vec<(Json, Json)>>,
    attempts: i64,
    error_category: Json,
) -> RegisterExchange {
    RegisterExchange {
        checked_at: Json::str(checked_at),
        status,
        headers,
        body,
        attempts: Json::Int(attempts.into()),
        error_category,
    }
}

fn successful_exchange(body: &[u8], checked_at: &str) -> RegisterExchange {
    exchange_with(
        Body::Bytes(body.to_vec()),
        Json::Int(200.into()),
        checked_at,
        headers(&[("Content-Type", CONTENT_TYPE), ("OData-Version", "4.0")]),
        1,
        Json::Null,
    )
}

fn transport_failure(checked_at: &str) -> RegisterExchange {
    exchange_with(
        Body::Absent,
        Json::Null,
        checked_at,
        headers(&[]),
        3,
        Json::str("TRANSPORT_ERROR"),
    )
}

/// The Python tests' deterministic replacement for the Register.
struct MemorySession {
    exchanges: HashMap<String, VecDeque<RegisterExchange>>,
    urls: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl MemorySession {
    fn new(exchanges: Vec<(&str, Vec<RegisterExchange>)>) -> MemorySession {
        MemorySession {
            exchanges: exchanges
                .into_iter()
                .map(|(url, items)| (url.to_string(), items.into()))
                .collect(),
            urls: Default::default(),
        }
    }

    fn one(url: &str, exchange: RegisterExchange) -> MemorySession {
        MemorySession::new(vec![(url, vec![exchange])])
    }

    fn urls(&self) -> Vec<String> {
        self.urls.borrow().clone()
    }
}

impl RegisterSession for MemorySession {
    fn observed_at(&self) -> Json {
        Json::str("2026-08-29T00:00:00Z")
    }

    fn get(&mut self, url: &str) -> Result<RegisterExchange, Exc> {
        self.urls.borrow_mut().push(url.to_string());
        self.exchanges
            .get_mut(url)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| Exc::Runtime(format!("unexpected Register request: {url}")))
    }
}

struct Work {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Work {
    fn new() -> Work {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        Work { _dir: dir, root }
    }

    fn manifest(&self, rows: &Value) -> PathBuf {
        let path = self.root.join("manifest.json");
        std::fs::write(&path, json_bytes(rows)).unwrap();
        path
    }

    fn manifest_bytes(&self, content: &[u8]) -> PathBuf {
        let path = self.root.join("manifest.json");
        std::fs::write(&path, content).unwrap();
        path
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn message(error: Exc) -> String {
    match capture::capture_message(&error) {
        Some(text) => text.to_string(),
        None => panic!("not a CaptureRegisterError: {error:?}"),
    }
}

fn read_json(path: &str) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Capture one manifest, returning the capture, the observation and the
/// evidence files by name.
fn capture_documents(
    session: &mut MemorySession,
    rows: &Value,
) -> (Value, Value, Vec<(String, Vec<u8>)>) {
    let w = Work::new();
    let manifest = w.manifest(rows);
    let paths = capture_register_run(s(&manifest), s(&w.root.join("capture")), session).unwrap();
    let mut evidence: Vec<(String, Vec<u8>)> = std::fs::read_dir(w.root.join("capture/evidence"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().into_string().unwrap(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    evidence.sort();
    (
        read_json(&paths.capture),
        read_json(&paths.observation),
        evidence,
    )
}

fn one_title(exchange: RegisterExchange) -> (Value, Value, Vec<(String, Vec<u8>)>) {
    capture_documents(
        &mut MemorySession::one(CURRENT_URL, exchange),
        &json!([manifest_row(json!({}))]),
    )
}

fn simulated(text: &str) -> OsError {
    OsError {
        kind: std::io::ErrorKind::Other,
        text: text.into(),
        strerror: text.into(),
    }
}

fn with_hooks<T>(
    hook: impl FnMut(Step) -> Option<OsError> + 'static,
    body: impl FnOnce() -> T,
) -> T {
    capture::set_fault_hook(hook);
    let outcome = body();
    capture::clear_hooks();
    outcome
}

fn staging_left(root: &Path) -> bool {
    std::fs::read_dir(root).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_str()
            .unwrap()
            .contains(".register-capture-")
    })
}

// ------------------------------------------------------------ the graph

#[test]
fn source_qualified_foreign_identifiers_fail_before_acquisition() {
    for identifier in ["ato:C2004A00467", "frl:C2004A00467", "[2026] FCA 1"] {
        let w = Work::new();
        let manifest = w.manifest(&json!([manifest_row(json!({"id": identifier}))]));
        let mut session = MemorySession::new(vec![]);
        let output = w.root.join("new-parent").join("capture");
        let error = capture_register_run(s(&manifest), s(&output), &mut session).unwrap_err();
        assert_eq!(message(error), "invalid Federal Register identifier");
        assert!(session.urls().is_empty());
        assert!(!output.parent().unwrap().exists());
    }
}

#[test]
fn same_native_id_cannot_substitute_a_different_source_or_view() {
    let original = manifest_row(json!({}))["sourceUrl"]
        .as_str()
        .unwrap()
        .to_string();
    for url in [
        original.replace("www.legislation.gov.au", "www.ato.gov.au"),
        format!("{original}?source=ato"),
        format!("{original}#section"),
        original.replace("C2004A00467", "C2004A00468"),
    ] {
        let w = Work::new();
        let manifest = w.manifest(&json!([manifest_row(json!({"sourceUrl": url}))]));
        let mut session = MemorySession::new(vec![]);
        let output = w.root.join("new-parent").join("capture");
        let error = capture_register_run(s(&manifest), s(&output), &mut session).unwrap_err();
        assert_eq!(
            message(error),
            "manifest title 1 sourceUrl does not match its title and version."
        );
        assert!(session.urls().is_empty());
        assert!(!output.parent().unwrap().exists());
    }
}

#[test]
fn single_unchanged_title_writes_complete_immutable_graph() {
    let mut row = manifest_row(json!({}));
    row["unrelatedRichField"] = json!({"ignored": true});
    let mut session = MemorySession::one(
        CURRENT_URL,
        exchange_with(
            Body::Bytes(CURRENT_BODY.to_vec()),
            Json::Int(200.into()),
            "2026-08-29T00:00:01Z",
            headers(&[
                ("Content-Type", CONTENT_TYPE),
                ("OData-Version", "4.0"),
                ("Server", "must-not-be-retained"),
            ]),
            1,
            Json::Null,
        ),
    );
    let w = Work::new();
    let manifest = w.manifest(&json!([row]));
    let destination = w.root.join("capture");
    let paths = capture_register_run(s(&manifest), s(&destination), &mut session).unwrap();
    assert_eq!(
        paths.baseline,
        s(&destination.join("monitor-baseline.json"))
    );
    assert_eq!(paths.capture, s(&destination.join("register-capture.json")));
    assert_eq!(
        paths.observation,
        s(&destination.join("register-observation.json"))
    );
    assert_eq!(session.urls(), [CURRENT_URL]);

    let baseline = json!({
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": "2026-08-04",
        "source": "Federal Register of Legislation",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "titles": [{
            "register_id": "C2004A00467",
            "name": "A New Tax System (Australian Business Number) Act 1999",
            "collection": "Act",
            "compilation_number": "32",
            "compilation_date": "2026-06-30",
            "version_is_current": true,
            "current_version_start": null,
            "retrieved": "2026-08-04",
            "source_url": "https://www.legislation.gov.au/C2004A00467/2026-06-30/2026-06-30/text/original/epub",
            "register_page": "https://www.legislation.gov.au/C2004A00467/latest/text"
        }]
    });
    let baseline_content = json_bytes(&baseline);
    assert_eq!(std::fs::read(&paths.baseline).unwrap(), baseline_content);

    let body_digest = sha256_id(CURRENT_BODY);
    let body_hex = body_digest.trim_start_matches("sha256:");
    assert_eq!(
        std::fs::read(
            destination
                .join("evidence")
                .join(format!("sha256-{body_hex}.json"))
        )
        .unwrap(),
        CURRENT_BODY
    );
    let result = json!({
        "register_id": "C2004A00467",
        "collection": "Act",
        "checked_at": "2026-08-29T00:00:01Z",
        "state": "UNCHANGED",
        "error_category": null,
        "requests": [{
            "role": "current",
            "url": CURRENT_URL,
            "checked_at": "2026-08-29T00:00:01Z",
            "http_status": 200,
            "transport_error_category": null,
            "attempt_count": 1,
            "response_headers": {"content-type": CONTENT_TYPE, "odata-version": "4.0"},
            "response_length": CURRENT_BODY.len(),
            "response_sha256": body_digest,
            "evidence_path": format!("evidence/sha256-{body_hex}.json")
        }]
    });
    let capture_doc = json!({
        "schema_version": "au-tax-register-capture.v1",
        "mode": "live",
        "observed_at": "2026-08-29T00:00:00Z",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "baseline_sha256": sha256_id(&baseline_content),
        "expected_register_ids": ["C2004A00467"],
        "complete": true,
        "results": [result]
    });
    let capture_content = json_bytes(&capture_doc);
    assert_eq!(std::fs::read(&paths.capture).unwrap(), capture_content);
    let result_digest = sha256_id(&json_bytes(&result));
    let observation = json!({
        "schema_version": "au-tax-register-observation.v4",
        "mode": "live",
        "observed_at": "2026-08-29T00:00:00Z",
        "scope_id": "au-primary-tax-legislation.v4",
        "baseline_sha256": sha256_id(&baseline_content),
        "capture_sha256": sha256_id(&capture_content),
        "expected_register_ids": ["C2004A00467"],
        "complete": true,
        "run_status": "VERIFIED",
        "observations": [{
            "register_id": "C2004A00467",
            "collection": "Act",
            "state": "UNCHANGED",
            "evidence_id": format!("frl:C2004A00467:{}", &result_digest[7..39]),
            "observed_compilation_number": null,
            "observed_compilation_date": null,
            "observed_register_document_id": null,
            "current_version_start": null,
            "evidence_url": "https://www.legislation.gov.au/C2004A00467/latest/text",
            "checked_at": "2026-08-29T00:00:01Z",
            "error_category": null,
            "capture_result_sha256": result_digest,
            "primary_response_sha256": body_digest,
            "primary_response_media_type": "application/json"
        }]
    });
    assert_eq!(
        std::fs::read(&paths.observation).unwrap(),
        json_bytes(&observation)
    );
}

// ------------------------------------------------------------ the manifest

#[test]
fn projects_rich_rows_in_canonical_order_with_latest_retrieval() {
    let act = manifest_row(json!({"unrelated": {"deep": [1, 2, 3]}}));
    let instrument = instrument_row(json!({
        "retrieved": "2026-08-03",
        "version_is_current": false,
        "current_version_start": "2025-10-05",
        "generatedField": "ignored"
    }));
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(CURRENT_BODY, "2026-08-29T00:00:02Z")],
        ),
        (
            INSTRUMENT_URL,
            vec![successful_exchange(
                &version_body(
                    "F2020L01498",
                    "2025-10-04",
                    Some("2"),
                    Some("F2025C00987"),
                    true,
                    "InForce",
                ),
                "2026-08-29T00:00:01Z",
            )],
        ),
    ]);
    let w = Work::new();
    let manifest = w.manifest(&json!([instrument, act]));
    let paths =
        capture_register_run(s(&manifest), s(&w.root.join("capture")), &mut session).unwrap();
    let baseline = read_json(&paths.baseline);
    assert_eq!(baseline["retrieved"], "2026-08-04");
    let ids: Vec<&str> = baseline["titles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["register_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["C2004A00467", "F2020L01498"]);
    assert_eq!(baseline["titles"][0].as_object().unwrap().len(), 10);
    assert_eq!(baseline["titles"][0]["version_is_current"], true);
    assert_eq!(baseline["titles"][0]["current_version_start"], Value::Null);
    assert_eq!(baseline["titles"][1]["version_is_current"], false);
    assert_eq!(baseline["titles"][1]["current_version_start"], "2025-10-05");
    assert_eq!(session.urls(), [CURRENT_URL, INSTRUMENT_URL]);
}

#[test]
fn projects_a_legacy_unnumbered_compilation_without_inventing_a_number() {
    let title_id = "C2004A00460";
    let row = manifest_row(json!({
        "id": title_id,
        "name": "A New Tax System (Goods and Services Tax Imposition\u{2014}Customs) Act 1999",
        "versionStart": "2005-07-01",
        "compilationNumber": null,
        "sourceUrl": format!("https://www.legislation.gov.au/{title_id}/2005-07-01/2005-07-01/text/original/epub")
    }));
    let mut body: Value = serde_json::from_slice(&version_body(
        title_id,
        "2005-07-01",
        None,
        Some("C2005C00389"),
        true,
        "InForce",
    ))
    .unwrap();
    body["value"][0]["registeredAt"] = json!("2005-07-01T10:15:01");
    let url = current_url_for(title_id);
    let mut session = MemorySession::one(
        &url,
        successful_exchange(body.to_string().as_bytes(), "2026-08-29T00:00:03Z"),
    );
    let w = Work::new();
    let manifest = w.manifest(&json!([row]));
    let paths =
        capture_register_run(s(&manifest), s(&w.root.join("capture")), &mut session).unwrap();
    assert_eq!(
        read_json(&paths.baseline)["titles"][0]["compilation_number"],
        Value::Null
    );
    let observation = read_json(&paths.observation);
    assert_eq!(observation["observations"][0]["state"], "UNCHANGED");
    assert_eq!(observation["run_status"], "VERIFIED");
}

#[test]
fn rejects_ambiguous_or_invalid_manifest_before_output_preflight() {
    let valid = manifest_row(json!({}));
    let mut without_number = valid.clone();
    without_number
        .as_object_mut()
        .unwrap()
        .remove("compilationNumber");
    let duplicate_member = String::from_utf8(json_bytes(&json!([valid])))
        .unwrap()
        .replacen(
            "\"id\": \"C2004A00467\",",
            "\"id\": \"C2004A00467\",\n    \"id\": \"C2004A00467\",",
            1,
        );
    let nested_duplicate = String::from_utf8(json_bytes(&json!([manifest_row(
        json!({"unrelated": {"one": 1}})
    )])))
    .unwrap()
    .replacen("\"one\": 1", "\"one\": 1, \"one\": 2", 1);
    let mut oversized = b"[\"".to_vec();
    oversized.extend(std::iter::repeat_n(b'x', 8 * 1024 * 1024));
    oversized.extend(b"\"]");
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (b"[]\n".to_vec(), "manifest must be a non-empty array."),
        (b"{}\n".to_vec(), "manifest must be a non-empty array."),
        (
            json_bytes(&json!([without_number])),
            "manifest title 1 compilationNumber is required.",
        ),
        (oversized, "manifest exceeds the 8 MiB size limit."),
        (b"[\xff]".to_vec(), "manifest must be strict UTF-8."),
        (
            duplicate_member.into_bytes(),
            "manifest contains a duplicate JSON member.",
        ),
        (
            nested_duplicate.into_bytes(),
            "manifest contains a duplicate JSON member.",
        ),
        (
            json_bytes(&json!([valid, valid])),
            "manifest contains duplicate Register identifiers.",
        ),
        (
            json_bytes(&json!([manifest_row(json!({"collection": "Regulation"}))])),
            "manifest title 1 collection is unsupported.",
        ),
        (
            json_bytes(&json!([manifest_row(json!({"retrieved": "2026-8-4"}))])),
            "manifest title 1 retrieved must be an ISO calendar date.",
        ),
        (
            json_bytes(&json!([manifest_row(json!({"name": "Tax Act\u{1}"}))])),
            "manifest title 1 name must not contain control characters.",
        ),
        (
            json_bytes(&json!([manifest_row(json!({"id": "../../escape"}))])),
            "invalid Federal Register identifier",
        ),
        (
            json_bytes(&json!([manifest_row(json!({"version_is_current": false}))])),
            "manifest title 1 current_version_start is required when not current.",
        ),
        (
            json_bytes(&json!([manifest_row(
                json!({"version_is_current": "false"})
            )])),
            "manifest title 1 version_is_current must be a boolean.",
        ),
    ];
    for (index, (content, expected)) in cases.into_iter().enumerate() {
        let w = Work::new();
        let manifest = w.manifest_bytes(&content);
        let missing = w.root.join(format!("missing-{index}"));
        let error = capture_register_run(
            s(&manifest),
            s(&missing.join("capture")),
            &mut MemorySession::new(vec![]),
        )
        .unwrap_err();
        assert_eq!(message(error), expected);
        assert!(!missing.exists());
    }
}

#[test]
fn accepts_only_the_exact_register_document_source_url() {
    let path = "/C2004A00467/2026-06-30/2026-06-30/text/original/epub";
    for url in [
        format!("http://www.legislation.gov.au{path}"),
        format!("https://www.legislation.gov.au.evil.example{path}"),
        format!("https://user@www.legislation.gov.au{path}"),
        format!("https://www.legislation.gov.au:443{path}"),
        format!("https://www.legislation.gov.au{path}?download=1"),
        format!("https://www.legislation.gov.au{path}#text"),
        "https://www.legislation.gov.au/C2004A00468/2026-06-30/2026-06-30/text/original/epub"
            .into(),
        "https://www.legislation.gov.au/C2004A00467/2026-06-29/2026-06-29/text/original/epub"
            .into(),
        "https://www.legislation.gov.au/C2004A00467%2F2026-06-30/2026-06-30/text/original/epub"
            .into(),
    ] {
        let w = Work::new();
        let manifest = w.manifest(&json!([manifest_row(json!({"sourceUrl": url}))]));
        let destination = w.root.join("capture");
        let error = capture_register_run(
            s(&manifest),
            s(&destination),
            &mut MemorySession::new(vec![]),
        )
        .unwrap_err();
        assert!(message(error).contains("sourceUrl does not match"), "{url}");
        assert!(!destination.exists());
    }
}

#[test]
fn manifest_must_be_an_ordinary_file() {
    let w = Work::new();
    let directory = w.root.join("manifest.json");
    std::fs::create_dir(&directory).unwrap();
    let error = capture_register_run(
        s(&directory),
        s(&w.root.join("capture")),
        &mut MemorySession::new(vec![]),
    )
    .unwrap_err();
    assert_eq!(message(error), "manifest must be an ordinary file.");
    std::fs::remove_dir(&directory).unwrap();
    let error = capture_register_run(
        s(&directory),
        s(&w.root.join("capture")),
        &mut MemorySession::new(vec![]),
    )
    .unwrap_err();
    assert_eq!(message(error), "manifest must be an ordinary file.");
}

/// Make a symbolic link, or return false where this Windows account lacks
/// the privilege, so the test is skipped.
fn symlink(target: &Path, link: &Path, dir: bool) -> bool {
    #[cfg(unix)]
    let made = {
        let _ = dir;
        std::os::unix::fs::symlink(target, link)
    };
    #[cfg(windows)]
    let made = if dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match made {
        Ok(()) => true,
        Err(error) if cfg!(windows) && error.raw_os_error() == Some(1314) => {
            eprintln!("skipped: symbolic links are unavailable to this account ({error})");
            false
        }
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn manifest_symbolic_link_is_rejected_without_touching_target() {
    let w = Work::new();
    let target = w.root.join("target.json");
    let content = json_bytes(&json!([manifest_row(json!({}))]));
    std::fs::write(&target, &content).unwrap();
    let link = w.root.join("manifest.json");
    if !symlink(&target, &link, false) {
        return;
    }
    let error = capture_register_run(
        s(&link),
        s(&w.root.join("capture")),
        &mut MemorySession::new(vec![]),
    )
    .unwrap_err();
    assert_eq!(message(error), "manifest must be an ordinary file.");
    assert_eq!(std::fs::read(&target).unwrap(), content);
}

#[test]
fn manifest_path_swap_cannot_substitute_a_different_regular_file() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let alternate = w.root.join("alternate.json");
    std::fs::write(&alternate, json_bytes(&json!([instrument_row(json!({}))]))).unwrap();
    let (from, to) = (s(&manifest).to_string(), s(&alternate).to_string());
    capture::set_open_hook(move |path| (path == from).then(|| to.clone()));
    let outcome = capture_register_run(
        s(&manifest),
        s(&w.root.join("capture")),
        &mut MemorySession::new(vec![]),
    );
    capture::clear_hooks();
    assert_eq!(
        message(outcome.unwrap_err()),
        "manifest changed during capture."
    );
    assert!(!w.root.join("capture").exists());
}

#[test]
fn unreadable_or_malformed_manifests_are_refused() {
    for (content, expected) in [
        (&b"["[..], "manifest is invalid JSON."),
        (b"[1]", "manifest title 1 must be an object."),
    ] {
        let w = Work::new();
        let manifest = w.manifest_bytes(content);
        let error = capture_register_run(
            s(&manifest),
            s(&w.root.join("capture")),
            &mut MemorySession::new(vec![]),
        )
        .unwrap_err();
        assert_eq!(message(error), expected);
    }
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let read_failure = with_hooks(
        |step| matches!(step, Step::Read(_)).then(|| simulated("[Errno 5] io")),
        || {
            capture_register_run(
                s(&manifest),
                s(&w.root.join("capture")),
                &mut MemorySession::new(vec![]),
            )
        },
    );
    assert_eq!(
        message(read_failure.unwrap_err()),
        "manifest could not be read."
    );
    let mut calls = 0;
    let replaced = with_hooks(
        move |step| {
            if matches!(step, Step::Identity(_)) {
                calls += 1;
                return (calls == 2).then(|| simulated("changed"));
            }
            None
        },
        || {
            capture_register_run(
                s(&manifest),
                s(&w.root.join("capture")),
                &mut MemorySession::new(vec![]),
            )
        },
    );
    assert_eq!(
        message(replaced.unwrap_err()),
        "manifest changed during capture."
    );
}

// ---------------------------------------------------------------- states

#[test]
fn timestamps_follow_the_contract_fraction_widths() {
    // Seven fraction digits and an offset on a version start.
    let mut body: Value = serde_json::from_slice(CURRENT_BODY).unwrap();
    body["value"][0]["start"] = json!("2026-06-30T00:00:00.1234567+10:00");
    body["value"][0]["registeredAt"] = json!("2026-08-27T17:31:41.1234567+10:00");
    let (capture_doc, _, _) = one_title(successful_exchange(
        body.to_string().as_bytes(),
        "2026-08-29T00:00:01Z",
    ));
    assert_eq!(capture_doc["results"][0]["state"], "UNCHANGED");
    // Eight are too many.
    body["value"][0]["registeredAt"] = json!("2026-08-27T17:31:41.12345678+10:00");
    let (capture_doc, _, _) = one_title(successful_exchange(
        body.to_string().as_bytes(),
        "2026-08-29T00:00:01Z",
    ));
    assert_eq!(
        capture_doc["results"][0]["error_category"],
        "INVALID_ODATA_SHAPE"
    );
    // One to six on a UTC check time.
    for fraction in ["1", "12", "1234", "12345", "123456"] {
        let stamp = format!("2026-08-27T07:31:41.{fraction}Z");
        let (capture_doc, _, _) = one_title(successful_exchange(CURRENT_BODY, &stamp));
        assert_eq!(capture_doc["results"][0]["checked_at"], json!(stamp));
    }
    // 24:00 is next midnight; offsets follow CPython 3.15.
    for (start, valid) in [
        ("2026-06-29T24:00:00", true),
        ("2026-06-30T00:00:00+23:59", true),
        ("2026-06-30T00:00:00+24:00", false),
        ("2026-06-30T00:00:00+12:60", false),
        ("2026-06-30T00:00:00.0000001", true),
    ] {
        body["value"][0]["start"] = json!(start);
        body["value"][0]["registeredAt"] = json!("2026-07-14T15:44:31.0377770");
        let (capture_doc, _, _) = one_title(successful_exchange(
            body.to_string().as_bytes(),
            "2026-08-29T00:00:01Z",
        ));
        let state = &capture_doc["results"][0]["state"];
        assert_eq!(state == "UNCHANGED", valid, "{start}: {state}");
    }
}

#[test]
fn derives_every_source_state_and_run_decision() {
    struct Case {
        row: Value,
        url: String,
        exchanges: Vec<(String, RegisterExchange)>,
        state: &'static str,
        run_status: &'static str,
        conditional: Value,
    }
    let nulls = |changes: Value| {
        let mut all = json!({
            "observed_compilation_number": null,
            "observed_compilation_date": null,
            "observed_register_document_id": null,
            "current_version_start": null,
            "error_category": null
        });
        for (k, v) in changes.as_object().unwrap() {
            all[k] = v.clone();
        }
        all
    };
    let row_for = |id: &str, name: &str, start: &str, number: Value, extra: Value| {
        let mut row = instrument_row(json!({
            "id": id,
            "name": name,
            "versionStart": start,
            "compilationNumber": number,
            "sourceUrl": format!("https://www.legislation.gov.au/{id}/{start}/{start}/text/original/epub")
        }));
        for (k, v) in extra.as_object().unwrap() {
            row[k] = v.clone();
        }
        row
    };
    let cases = vec![
        Case {
            row: manifest_row(json!({})),
            url: CURRENT_URL.into(),
            exchanges: vec![(
                CURRENT_URL.into(),
                successful_exchange(CURRENT_BODY, "2026-08-29T01:00:01Z"),
            )],
            state: "UNCHANGED",
            run_status: "VERIFIED",
            conditional: nulls(json!({})),
        },
        Case {
            row: instrument_row(json!({})),
            url: INSTRUMENT_URL.into(),
            exchanges: vec![(
                INSTRUMENT_URL.into(),
                successful_exchange(
                    &version_body(
                        "F2020L01498",
                        "2026-08-01",
                        Some("3"),
                        Some("F2026C00001"),
                        true,
                        "InForce",
                    ),
                    "2026-08-29T01:00:02Z",
                ),
            )],
            state: "SUPERSEDED",
            run_status: "VERIFIED",
            conditional: nulls(json!({
                "observed_compilation_number": "3",
                "observed_compilation_date": "2026-08-01",
                "observed_register_document_id": "F2026C00001"
            })),
        },
        Case {
            row: row_for(
                "F2022L00764",
                "ASIC Corporations Superannuation Trustees Instrument 2022/497",
                "2023-09-01",
                json!("2"),
                json!({"version_is_current": false, "current_version_start": "2026-07-07"}),
            ),
            url: current_url_for("F2022L00764"),
            exchanges: vec![(
                current_url_for("F2022L00764"),
                successful_exchange(
                    &version_body("F2022L00764", "2026-07-07", None, None, true, "InForce"),
                    "2026-08-29T01:00:03Z",
                ),
            )],
            state: "CURRENT_NO_PUBLISHED_COMPILATION",
            run_status: "VERIFIED",
            conditional: nulls(json!({"current_version_start": "2026-07-07"})),
        },
        Case {
            row: row_for(
                "F2006B07691",
                "Commonwealth Places Mirror Taxes Queensland Notice 2002",
                "1997-10-06",
                json!("0"),
                json!({"retrieved": "2026-08-03"}),
            ),
            url: current_url_for("F2006B07691"),
            exchanges: vec![
                (
                    current_url_for("F2006B07691"),
                    successful_exchange(&empty_versions_body(), "2026-08-29T01:00:04Z"),
                ),
                (
                    history_url_for("F2006B07691"),
                    successful_exchange(
                        &version_body(
                            "F2006B07691",
                            "1997-10-06",
                            Some("0"),
                            Some("F2006B07691"),
                            false,
                            "Ceased",
                        ),
                        "2026-08-29T01:00:05Z",
                    ),
                ),
            ],
            state: "NO_LONGER_IN_FORCE",
            run_status: "VERIFIED",
            conditional: nulls(json!({})),
        },
        Case {
            row: row_for(
                "F2016L01256",
                "Repealed Instrument 2016",
                "2016-07-29",
                Value::Null,
                json!({}),
            ),
            url: current_url_for("F2016L01256"),
            exchanges: vec![(
                current_url_for("F2016L01256"),
                successful_exchange(
                    &version_body("F2016L01256", "2026-08-12", None, None, true, "Repealed"),
                    "2026-08-29T01:00:06Z",
                ),
            )],
            state: "NO_LONGER_IN_FORCE",
            run_status: "VERIFIED",
            conditional: nulls(json!({})),
        },
        Case {
            row: row_for(
                "F2016L01523",
                "Repealed Instrument With Compilation 2016",
                "2016-07-29",
                json!("1"),
                json!({}),
            ),
            url: current_url_for("F2016L01523"),
            exchanges: vec![(
                current_url_for("F2016L01523"),
                successful_exchange(
                    &version_body(
                        "F2016L01523",
                        "2026-08-15",
                        Some("2"),
                        Some("F2026C00777"),
                        true,
                        "Repealed",
                    ),
                    "2026-08-29T01:00:07Z",
                ),
            )],
            state: "NO_LONGER_IN_FORCE",
            run_status: "VERIFIED",
            conditional: nulls(json!({})),
        },
        Case {
            row: row_for(
                "F2025L00178",
                "Family Law Superannuation Regulations 2025",
                "2025-04-01",
                json!("1"),
                json!({}),
            ),
            url: current_url_for("F2025L00178"),
            exchanges: vec![(
                current_url_for("F2025L00178"),
                transport_failure("2026-08-29T01:00:06Z"),
            )],
            state: "LOOKUP_FAILED",
            run_status: "BLOCKED",
            conditional: nulls(json!({"error_category": "TRANSPORT_ERROR"})),
        },
    ];
    for case in cases {
        let mut by_url: Vec<(&str, Vec<RegisterExchange>)> = Vec::new();
        for (url, exchange) in &case.exchanges {
            by_url.push((url.as_str(), vec![exchange.clone()]));
        }
        let mut session = MemorySession::new(by_url);
        let (_, observation, _) = capture_documents(&mut session, &json!([case.row]));
        assert_eq!(observation["complete"], true);
        assert_eq!(observation["run_status"], case.run_status, "{}", case.url);
        let item = &observation["observations"][0];
        assert_eq!(item["state"], case.state, "{}", case.url);
        for (field, expected) in case.conditional.as_object().unwrap() {
            assert_eq!(&item[field], expected, "{field}");
        }
    }
}

#[test]
fn empty_current_result_uses_the_exact_ordered_history_query() {
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(
                &empty_versions_body(),
                "2026-08-29T02:00:01Z",
            )],
        ),
        (
            HISTORY_URL,
            vec![successful_exchange(
                &version_body(
                    "C2004A00467",
                    "2025-01-01",
                    Some("31"),
                    Some("C2025C00001"),
                    false,
                    "Repealed",
                ),
                "2026-08-29T02:00:02Z",
            )],
        ),
    ]);
    let (_, observation, _) = capture_documents(&mut session, &json!([manifest_row(json!({}))]));
    assert_eq!(session.urls(), [CURRENT_URL, HISTORY_URL]);
    assert_eq!(
        observation["observations"][0]["state"],
        "NO_LONGER_IN_FORCE"
    );
}

#[test]
fn lookup_failure_does_not_prevent_later_scope_attempts() {
    let mut session = MemorySession::new(vec![
        (CURRENT_URL, vec![transport_failure("2026-08-29T03:00:01Z")]),
        (
            INSTRUMENT_URL,
            vec![successful_exchange(
                &version_body(
                    "F2020L01498",
                    "2025-10-04",
                    Some("2"),
                    Some("F2025C00987"),
                    true,
                    "InForce",
                ),
                "2026-08-29T03:00:02Z",
            )],
        ),
    ]);
    let (capture_doc, observation, _) = capture_documents(
        &mut session,
        &json!([instrument_row(json!({})), manifest_row(json!({}))]),
    );
    assert_eq!(session.urls(), [CURRENT_URL, INSTRUMENT_URL]);
    assert_eq!(capture_doc["complete"], true);
    assert_eq!(observation["complete"], true);
    assert_eq!(observation["run_status"], "BLOCKED");
    let states: Vec<&str> = observation["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["LOOKUP_FAILED", "UNCHANGED"]);
}

#[test]
fn invalid_responses_use_fixed_fail_closed_categories() {
    let valid: Value = serde_json::from_slice(CURRENT_BODY).unwrap();
    let with_row = |changes: Value, remove: Option<&str>, extra: bool| -> Vec<u8> {
        let mut row = valid["value"][0].clone();
        for (k, v) in changes.as_object().unwrap() {
            row[k] = v.clone();
        }
        if let Some(key) = remove {
            row.as_object_mut().unwrap().remove(key);
        }
        if extra {
            row["unexpected"] = json!("member");
        }
        json!({"@odata.context": valid["@odata.context"], "value": [row]})
            .to_string()
            .into_bytes()
    };
    let ok = |body: &[u8]| successful_exchange(body, "2026-08-29T04:00:00Z");
    let with =
        |body: Body,
         status: Json,
         h: Option<Vec<(Json, Json)>>,
         attempts: i64,
         category: Json,
         checked: &str| { exchange_with(body, status, checked, h, attempts, category) };
    let default_headers = || headers(&[("Content-Type", CONTENT_TYPE), ("OData-Version", "4.0")]);
    let row0 = valid["value"][0].clone();
    let two_rows = json!({"@odata.context": CONTEXT, "value": [row0, row0]}).to_string();
    let envelope =
        json!({"@odata.context": CONTEXT, "value": [row0], "unexpected": true}).to_string();
    let wrong_context =
        json!({"@odata.context": "https://example.invalid/$metadata", "value": [row0]}).to_string();
    let duplicate = String::from_utf8(CURRENT_BODY.to_vec()).unwrap().replacen(
        "\"titleId\":\"C2004A00467\",",
        "\"titleId\":\"C2004A00467\",\"titleId\":\"C2004A00467\",",
        1,
    );
    let status = |n: i64| Json::Int(n.into());
    let t = "2026-08-29T04:00:00Z";
    let cases: Vec<(RegisterExchange, &str)> = vec![
        (
            with(
                Body::Absent,
                Json::Null,
                headers(&[]),
                1,
                Json::str("TRANSPORT_ERROR"),
                t,
            ),
            "TRANSPORT_ERROR",
        ),
        (
            with(
                Body::Bytes(b"not retained".to_vec()),
                status(503),
                default_headers(),
                1,
                Json::Null,
                t,
            ),
            "HTTP_STATUS",
        ),
        (ok(&vec![b'x'; 256 * 1024 + 1]), "RESPONSE_TOO_LARGE"),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                headers(&[("Content-Type", "text/html; charset=utf-8")]),
                1,
                Json::Null,
                t,
            ),
            "UNSUPPORTED_MEDIA_TYPE",
        ),
        (ok(b"\xff"), "INVALID_JSON"),
        (ok(b"{\"value\":"), "INVALID_JSON"),
        (ok(duplicate.as_bytes()), "INVALID_JSON"),
        (ok(envelope.as_bytes()), "INVALID_ODATA_SHAPE"),
        (ok(wrong_context.as_bytes()), "INVALID_ODATA_SHAPE"),
        (ok(two_rows.as_bytes()), "INVALID_ODATA_SHAPE"),
        (
            ok(&with_row(json!({}), Some("status"), false)),
            "INVALID_ODATA_SHAPE",
        ),
        (ok(&with_row(json!({}), None, true)), "INVALID_ODATA_SHAPE"),
        (
            ok(&with_row(json!({"titleId": "C2004A00468"}), None, false)),
            "IDENTITY_MISMATCH",
        ),
        (
            ok(&with_row(json!({"isCurrent": "true"}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"status": "Current"}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"status": []}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"start": "not-a-date"}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"start": "2026-06-30"}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"compilationNumber": ""}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"compilationNumber": 32}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(
                json!({"registerId": "../../document"}),
                None,
                false,
            )),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"registeredAt": null}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(
                json!({"registeredAt": "2026-07-14"}),
                None,
                false,
            )),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"registerId": null}), None, false)),
            "INVALID_ODATA_SHAPE",
        ),
        (
            ok(&with_row(json!({"compilationNumber": "33"}), None, false)),
            "INCONSISTENT_CHRONOLOGY",
        ),
        (
            ok(&with_row(
                json!({"start": "2026-07-01T00:00:00"}),
                None,
                false,
            )),
            "INCONSISTENT_CHRONOLOGY",
        ),
        (
            ok(&with_row(
                json!({"start": "2026-06-29T00:00:00", "compilationNumber": "33"}),
                None,
                false,
            )),
            "INCONSISTENT_CHRONOLOGY",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                default_headers(),
                1,
                Json::Null,
                "local-time",
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                default_headers(),
                0,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                default_headers(),
                4,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                headers(&[("Content-Type", "application/json\r\nInjected: yes")]),
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                headers(&[
                    ("Content-Type", "application/json"),
                    ("content-type", "text/html"),
                ]),
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                default_headers(),
                1,
                Json::str("TRANSPORT_ERROR"),
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Absent,
                Json::Null,
                headers(&[]),
                1,
                Json::str("socket exploded"),
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        // A header name that is not text, a non-mapping, a body of the wrong type,
        // a Boolean status and a Boolean attempt count.
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                Some(vec![(Json::Int(1.into()), Json::str("x"))]),
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                None,
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Other,
                status(200),
                default_headers(),
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Absent,
                status(200),
                default_headers(),
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                Json::Bool(true),
                default_headers(),
                1,
                Json::Null,
                t,
            ),
            "INVALID_EXCHANGE",
        ),
        (
            exchange_with(
                Body::Bytes(CURRENT_BODY.to_vec()),
                status(200),
                t,
                default_headers(),
                1,
                Json::Null,
            )
            .tap(|e| e.attempts = Json::Bool(true)),
            "INVALID_EXCHANGE",
        ),
    ];
    for (exchange, expected) in cases {
        let (capture_doc, observation, _) = one_title(exchange);
        let result = &capture_doc["results"][0];
        let item = &observation["observations"][0];
        assert_eq!(result["state"], "LOOKUP_FAILED", "{expected}");
        assert_eq!(result["error_category"], expected);
        assert_eq!(item["state"], "LOOKUP_FAILED");
        assert_eq!(item["error_category"], expected);
        assert_eq!(observation["run_status"], "BLOCKED");
    }
}

trait Tap: Sized {
    fn tap(mut self, change: impl FnOnce(&mut Self)) -> Self {
        change(&mut self);
        self
    }
}

impl Tap for RegisterExchange {}

#[test]
fn rows_must_match_their_request_role() {
    let valid: Value = serde_json::from_slice(CURRENT_BODY).unwrap();
    let body = |row: Value| {
        json!({"@odata.context": CONTEXT, "value": [row]})
            .to_string()
            .into_bytes()
    };
    // A history row on the current query.
    let mut row = valid["value"][0].clone();
    row["isCurrent"] = json!(false);
    let (capture_doc, _, _) = one_title(successful_exchange(&body(row), "2026-08-29T04:00:00Z"));
    assert_eq!(
        capture_doc["results"][0]["error_category"],
        "INVALID_ODATA_SHAPE"
    );
    // A current row on the history query.
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(
                &empty_versions_body(),
                "2026-08-29T04:00:00Z",
            )],
        ),
        (
            HISTORY_URL,
            vec![successful_exchange(CURRENT_BODY, "2026-08-29T04:00:01Z")],
        ),
    ]);
    let (capture_doc, _, _) = capture_documents(&mut session, &json!([manifest_row(json!({}))]));
    assert_eq!(
        capture_doc["results"][0]["error_category"],
        "INVALID_ODATA_SHAPE"
    );
    // A blank compilation number without a document.
    let mut row = valid["value"][0].clone();
    row["registerId"] = Value::Null;
    row["registeredAt"] = Value::Null;
    row["compilationNumber"] = json!(" ");
    let (capture_doc, _, _) = one_title(successful_exchange(&body(row), "2026-08-29T04:00:00Z"));
    assert_eq!(
        capture_doc["results"][0]["error_category"],
        "INVALID_ODATA_SHAPE"
    );
}

#[test]
fn a_success_without_a_media_type_fails_closed() {
    let (capture_doc, observation, evidence) = one_title(exchange_with(
        Body::Bytes(CURRENT_BODY.to_vec()),
        Json::Int(200.into()),
        "2026-08-29T14:00:01Z",
        headers(&[("OData-Version", "4.0")]),
        1,
        Json::Null,
    ));
    assert_eq!(
        capture_doc["results"][0]["error_category"],
        "UNSUPPORTED_MEDIA_TYPE"
    );
    assert_eq!(
        observation["observations"][0]["primary_response_media_type"],
        Value::Null
    );
    assert_eq!(evidence.len(), 1);
}

#[test]
fn a_failed_history_lookup_names_its_own_category() {
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(
                &empty_versions_body(),
                "2026-08-29T14:00:01Z",
            )],
        ),
        (
            HISTORY_URL,
            vec![exchange_with(
                Body::Absent,
                Json::Int(500.into()),
                "2026-08-29T14:00:03Z",
                headers(&[]),
                3,
                Json::Null,
            )],
        ),
    ]);
    let (_, observation, _) = capture_documents(&mut session, &json!([manifest_row(json!({}))]));
    let item = &observation["observations"][0];
    assert_eq!(
        (item["state"].as_str(), item["error_category"].as_str()),
        (Some("LOOKUP_FAILED"), Some("HTTP_STATUS"))
    );
}

#[test]
fn no_current_or_historical_version_is_insufficient_evidence() {
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(
                &empty_versions_body(),
                "2026-08-29T05:00:01Z",
            )],
        ),
        (
            HISTORY_URL,
            vec![successful_exchange(
                &empty_versions_body(),
                "2026-08-29T05:00:02Z",
            )],
        ),
    ]);
    let (capture_doc, observation, evidence) =
        capture_documents(&mut session, &json!([manifest_row(json!({}))]));
    assert_eq!(
        capture_doc["results"][0]["error_category"],
        "NO_VERSION_EVIDENCE"
    );
    assert_eq!(observation["observations"][0]["state"], "LOOKUP_FAILED");
    assert_eq!(evidence.len(), 1);
}

#[test]
fn bounded_malformed_200_body_is_retained_as_failure_evidence() {
    let malformed = b"{\"value\":";
    let (capture_doc, observation, evidence) =
        one_title(successful_exchange(malformed, "2026-08-29T06:00:01Z"));
    let request = &capture_doc["results"][0]["requests"][0];
    let digest = sha256_id(malformed);
    assert_eq!(request["response_sha256"], json!(digest));
    assert_eq!(request["response_length"], malformed.len());
    assert_eq!(
        request["evidence_path"],
        json!(format!("evidence/sha256-{}.json", &digest[7..]))
    );
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].1, malformed);
    let item = &observation["observations"][0];
    assert_eq!(item["primary_response_sha256"], json!(digest));
    assert_eq!(item["primary_response_media_type"], "application/json");
}

#[test]
fn non_200_body_is_neither_retained_nor_disclosed() {
    let secret = b"PRIVATE-SERVER-BODY-MUST-NOT-APPEAR";
    let (capture_doc, observation, evidence) = one_title(exchange_with(
        Body::Bytes(secret.to_vec()),
        Json::Int(503.into()),
        "2026-08-29T07:00:01Z",
        headers(&[("Content-Type", "text/plain"), ("Server", "ignored")]),
        3,
        Json::Null,
    ));
    let request = &capture_doc["results"][0]["requests"][0];
    assert_eq!(request["response_length"], Value::Null);
    assert_eq!(request["response_sha256"], Value::Null);
    assert_eq!(request["evidence_path"], Value::Null);
    assert!(evidence.is_empty());
    let item = &observation["observations"][0];
    assert_eq!(item["primary_response_sha256"], Value::Null);
    assert_eq!(item["primary_response_media_type"], Value::Null);
    let serialised = json!([capture_doc, observation]).to_string();
    assert!(!serialised.contains("PRIVATE-SERVER-BODY"));
}

// ------------------------------------------------- the production session

/// A scripted network for [`HttpsSession`]: exchanges in order, a clock that
/// moves only when slept, and fixed timestamps.
struct Net {
    effects: VecDeque<Result<Exchange, Failure>>,
    calls: std::rc::Rc<std::cell::RefCell<Vec<(String, usize)>>>,
    sleeps: std::rc::Rc<std::cell::RefCell<Vec<f64>>>,
    now: f64,
    timestamps: VecDeque<String>,
}

impl Http for Net {
    fn get(&mut self, _url: &str, _limit: Option<usize>) -> Result<Response, Failure> {
        unreachable!("the capture never follows redirects")
    }

    fn exchange(&mut self, url: &str, limit: usize) -> Result<Exchange, Failure> {
        self.calls.borrow_mut().push((url.to_string(), limit));
        let effect = self
            .effects
            .pop_front()
            .expect("an unexpected production HTTP attempt");
        effect.map(|mut e| {
            if let Some(body) = &mut e.body {
                body.truncate(limit);
            }
            e
        })
    }

    fn sleep(&mut self, seconds: f64) {
        self.sleeps.borrow_mut().push(seconds);
        self.now += seconds;
    }

    fn today(&mut self) -> String {
        unreachable!()
    }

    fn monotonic(&mut self) -> f64 {
        self.now
    }

    fn utc_now(&mut self) -> String {
        self.timestamps.pop_front().expect("a timestamp")
    }
}

type Calls = std::rc::Rc<std::cell::RefCell<Vec<(String, usize)>>>;
type Sleeps = std::rc::Rc<std::cell::RefCell<Vec<f64>>>;

fn production(
    effects: Vec<Result<Exchange, Failure>>,
    timestamps: &[&str],
) -> (HttpsSession, Calls, Sleeps) {
    let calls: Calls = Default::default();
    let sleeps: Sleeps = Default::default();
    let net = Net {
        effects: effects.into(),
        calls: calls.clone(),
        sleeps: sleeps.clone(),
        now: 0.0,
        timestamps: timestamps.iter().map(|t| t.to_string()).collect(),
    };
    (HttpsSession::new(Box::new(net)), calls, sleeps)
}

fn response(status: u16, pairs: &[(&str, &str)], body: Option<&[u8]>) -> Result<Exchange, Failure> {
    Ok(Exchange {
        status,
        headers: pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.map(<[u8]>::to_vec),
    })
}

#[test]
fn retries_transport_and_retryable_status_with_fixed_delays() {
    let (mut session, calls, sleeps) = production(
        vec![
            Err(Failure::Transport("URLError".into())),
            response(
                503,
                &[("Content-Type", "text/plain"), ("Server", "ignored")],
                None,
            ),
            response(
                200,
                &[
                    ("Content-Type", CONTENT_TYPE),
                    ("OData-Version", "4.0"),
                    ("X-Frl-Version", "2026.08.13-releaseyaml.1"),
                    ("Server", "must-not-be-retained"),
                ],
                Some(CURRENT_BODY),
            ),
        ],
        &["2026-08-29T08:00:00Z", "2026-08-29T08:00:13Z"],
    );
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let paths =
        capture_register_run(s(&manifest), s(&w.root.join("capture")), &mut session).unwrap();
    assert_eq!(*sleeps.borrow(), [6.0, 6.0]);
    assert_eq!(
        *calls.borrow(),
        vec![(CURRENT_URL.to_string(), 256 * 1024 + 1); 3]
    );
    let capture_doc = read_json(&paths.capture);
    let observation = read_json(&paths.observation);
    assert_eq!(observation["run_status"], "VERIFIED");
    assert_eq!(observation["observed_at"], "2026-08-29T08:00:00Z");
    let request = &capture_doc["results"][0]["requests"][0];
    assert_eq!(request["attempt_count"], 3);
    assert_eq!(request["checked_at"], "2026-08-29T08:00:13Z");
    assert_eq!(
        request["response_headers"],
        json!({
            "content-type": CONTENT_TYPE,
            "odata-version": "4.0",
            "x-frl-version": "2026.08.13-releaseyaml.1"
        })
    );
    assert_eq!(
        std::fs::read_dir(w.root.join("capture/evidence"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn paces_separate_register_requests_without_wall_clock_sleep() {
    let second = version_body(
        "F2020L01498",
        "2025-10-04",
        Some("2"),
        Some("F2025C00987"),
        true,
        "InForce",
    );
    let (mut session, calls, sleeps) = production(
        vec![
            response(200, &[("Content-Type", CONTENT_TYPE)], Some(CURRENT_BODY)),
            response(200, &[("Content-Type", CONTENT_TYPE)], Some(&second)),
        ],
        &[
            "2026-08-29T09:00:00Z",
            "2026-08-29T09:00:01Z",
            "2026-08-29T09:00:03Z",
        ],
    );
    let w = Work::new();
    let manifest = w.manifest(&json!([instrument_row(json!({})), manifest_row(json!({}))]));
    let paths =
        capture_register_run(s(&manifest), s(&w.root.join("capture")), &mut session).unwrap();
    assert_eq!(*sleeps.borrow(), [1.5]);
    let urls: Vec<String> = calls.borrow().iter().map(|(u, _)| u.clone()).collect();
    assert_eq!(urls, [CURRENT_URL, INSTRUMENT_URL]);
    assert_eq!(read_json(&paths.observation)["run_status"], "VERIFIED");
}

#[test]
fn redirect_is_not_followed_retried_or_retained() {
    let (mut session, calls, sleeps) = production(
        vec![response(
            302,
            &[
                ("Location", "https://evil.example/private"),
                ("Content-Type", "text/html"),
            ],
            None,
        )],
        &["2026-08-29T10:00:00Z", "2026-08-29T10:00:01Z"],
    );
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let paths =
        capture_register_run(s(&manifest), s(&w.root.join("capture")), &mut session).unwrap();
    assert_eq!(calls.borrow().len(), 1);
    assert!(sleeps.borrow().is_empty());
    assert_eq!(
        std::fs::read_dir(w.root.join("capture/evidence"))
            .unwrap()
            .count(),
        0
    );
    let capture_doc = read_json(&paths.capture);
    assert_eq!(capture_doc["results"][0]["error_category"], "HTTP_STATUS");
    assert!(!capture_doc.to_string().contains("evil.example"));
}

#[test]
fn reads_only_one_byte_beyond_the_response_limit() {
    let oversized = vec![b'x'; 256 * 1024 + 100];
    let (mut session, calls, _) = production(
        vec![response(
            200,
            &[("Content-Type", CONTENT_TYPE)],
            Some(&oversized),
        )],
        &["2026-08-29T11:00:00Z", "2026-08-29T11:00:01Z"],
    );
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let paths =
        capture_register_run(s(&manifest), s(&w.root.join("capture")), &mut session).unwrap();
    assert_eq!(calls.borrow()[0].1, 256 * 1024 + 1);
    assert_eq!(
        read_json(&paths.capture)["results"][0]["error_category"],
        "RESPONSE_TOO_LARGE"
    );
    assert_eq!(
        std::fs::read_dir(w.root.join("capture/evidence"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn production_session_failures_on_every_attempt_and_its_url_boundary() {
    let (mut session, _, _) = production(
        vec![Err(Failure::Transport("URLError".into())); 3],
        &["2026-08-29T11:00:00Z", "2026-08-29T11:00:01Z"],
    );
    let exchange = session.get(CURRENT_URL).unwrap();
    assert_eq!(
        (exchange.status, exchange.error_category, exchange.attempts),
        (
            Json::Null,
            Json::str("TRANSPORT_ERROR"),
            Json::Int(3.into())
        )
    );
    let (mut session, _, _) = production(
        vec![
            response(503, &[], None),
            response(503, &[], None),
            response(503, &[], None),
        ],
        &["2026-08-29T11:00:00Z", "2026-08-29T11:00:01Z"],
    );
    let exchange = session.get(CURRENT_URL).unwrap();
    assert_eq!(
        (exchange.status, exchange.attempts),
        (Json::Int(503.into()), Json::Int(3.into()))
    );
    let (mut session, _, _) = production(vec![], &["2026-08-29T11:00:00Z"]);
    assert_eq!(
        message(
            session
                .get("https://api.prod.legislation.gov.au:port/v1/versions")
                .unwrap_err()
        ),
        "Register request URL is invalid."
    );
    assert_eq!(
        message(session.get("https://example.test/v1/versions").unwrap_err()),
        "Register request URL is outside the fixed API boundary."
    );
    // A repeated contract header keeps its case-swapped spelling, which is
    // then an ambiguous selected header.
    let (mut session, _, _) = production(
        vec![response(
            200,
            &[
                ("Content-Type", CONTENT_TYPE),
                ("content-type", "b"),
                ("Server", "x"),
            ],
            Some(CURRENT_BODY),
        )],
        &["2026-08-29T11:00:00Z", "2026-08-29T11:00:01Z"],
    );
    let exchange = session.get(CURRENT_URL).unwrap();
    assert_eq!(
        exchange.headers.unwrap(),
        vec![
            (Json::str("Content-Type"), Json::str(CONTENT_TYPE)),
            (Json::str("CONTENT-TYPE"), Json::str("b"))
        ]
    );
}

#[test]
fn the_clock_reads_whole_utc_seconds() {
    assert_eq!(
        tax_radar_au::fadden::http::utc_text(0),
        "1970-01-01T00:00:00Z"
    );
    assert_eq!(
        tax_radar_au::fadden::http::utc_text(4_102_444_799),
        "2099-12-31T23:59:59Z"
    );
    assert_eq!(
        tax_radar_au::fadden::http::utc_text(951_782_400),
        "2000-02-29T00:00:00Z"
    );
}

// --------------------------------------------------------- evidence graph

#[test]
fn identical_failed_response_bytes_share_one_evidence_object() {
    let malformed = b"{\"same-malformed-response\":";
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(malformed, "2026-08-29T12:00:01Z")],
        ),
        (
            INSTRUMENT_URL,
            vec![successful_exchange(malformed, "2026-08-29T12:00:02Z")],
        ),
    ]);
    let (capture_doc, _, evidence) = capture_documents(
        &mut session,
        &json!([instrument_row(json!({})), manifest_row(json!({}))]),
    );
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].1, malformed);
    let declared: Vec<&Value> = capture_doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["requests"][0]["response_sha256"])
        .collect();
    assert_eq!(declared, [&json!(sha256_id(malformed)); 2]);
}

#[test]
fn repeated_inputs_exchanges_and_timestamps_are_byte_deterministic() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let mut outputs = Vec::new();
    for index in 0..2 {
        let destination = w.root.join(format!("capture-{index}"));
        let mut session = MemorySession::one(
            CURRENT_URL,
            successful_exchange(CURRENT_BODY, "2026-08-29T13:00:01Z"),
        );
        capture_register_run(s(&manifest), s(&destination), &mut session).unwrap();
        let mut files = Vec::new();
        for name in [
            "monitor-baseline.json",
            "register-capture.json",
            "register-observation.json",
        ] {
            files.push(std::fs::read(destination.join(name)).unwrap());
        }
        outputs.push(files);
    }
    assert_eq!(outputs[0], outputs[1]);
}

fn rewrite(path: &Path, change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut document: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    change(&mut document);
    let content = json_bytes(&document);
    std::fs::write(path, &content).unwrap();
    content
}

#[test]
fn final_revalidation_blocks_every_cross_file_tamper() {
    let mutators: Vec<fn(&Path)> = vec![
        |staging| {
            let path = staging.join("monitor-baseline.json");
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b' ');
            std::fs::write(&path, bytes).unwrap();
        },
        |staging| {
            rewrite(&staging.join("register-capture.json"), |d| {
                d["baseline_sha256"] = json!(format!("sha256:{}", "0".repeat(64)))
            });
        },
        |staging| {
            rewrite(&staging.join("register-observation.json"), |d| {
                d["capture_sha256"] = json!(format!("sha256:{}", "0".repeat(64)))
            });
        },
        |staging| {
            let path = std::fs::read_dir(staging.join("evidence"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.extend(b"tamper");
            std::fs::write(&path, bytes).unwrap();
        },
        |staging| {
            std::fs::write(
                staging
                    .join("evidence")
                    .join(format!("sha256-{}.json", "0".repeat(64))),
                b"extra",
            )
            .unwrap();
        },
        |staging| {
            let content = rewrite(&staging.join("register-capture.json"), |d| {
                d["results"][0]["requests"][0]["evidence_path"] = json!("../outside.json")
            });
            rewrite(&staging.join("register-observation.json"), |d| {
                d["capture_sha256"] = json!(sha256_id(&content))
            });
        },
    ];
    for mutate in mutators {
        let w = Work::new();
        let manifest = w.manifest(&json!([manifest_row(json!({}))]));
        let destination = w.root.join("capture");
        let mut session = MemorySession::one(
            CURRENT_URL,
            successful_exchange(CURRENT_BODY, "2026-08-29T14:00:01Z"),
        );
        let outcome = with_hooks(
            move |step| {
                if let Step::Validate(staging) = step {
                    mutate(Path::new(staging));
                }
                None
            },
            || capture_register_run(s(&manifest), s(&destination), &mut session),
        );
        assert!(message(outcome.unwrap_err()).starts_with("staged capture"));
        assert!(!destination.exists());
        assert!(!staging_left(&w.root));
    }
}

#[test]
fn revalidation_checks_every_declaration_of_deduplicated_evidence() {
    let malformed = b"{\"shared-invalid-response\":";
    let w = Work::new();
    let manifest = w.manifest(&json!([instrument_row(json!({})), manifest_row(json!({}))]));
    let destination = w.root.join("capture");
    let mut session = MemorySession::new(vec![
        (
            CURRENT_URL,
            vec![successful_exchange(malformed, "2026-08-29T14:30:01Z")],
        ),
        (
            INSTRUMENT_URL,
            vec![successful_exchange(malformed, "2026-08-29T14:30:02Z")],
        ),
    ]);
    let outcome = with_hooks(
        |step| {
            if let Step::Validate(staging) = step {
                let staging = Path::new(staging);
                let mut first_result = Value::Null;
                let content = rewrite(&staging.join("register-capture.json"), |d| {
                    let length = d["results"][0]["requests"][0]["response_length"]
                        .as_u64()
                        .unwrap();
                    d["results"][0]["requests"][0]["response_length"] = json!(length + 1);
                    first_result = d["results"][0].clone();
                });
                let digest = sha256_id(&json_bytes(&first_result));
                rewrite(&staging.join("register-observation.json"), |d| {
                    d["observations"][0]["capture_result_sha256"] = json!(digest);
                    d["observations"][0]["evidence_id"] =
                        json!(format!("frl:C2004A00467:{}", &digest[7..39]));
                    d["capture_sha256"] = json!(sha256_id(&content));
                });
            }
            None
        },
        || capture_register_run(s(&manifest), s(&destination), &mut session),
    );
    assert!(message(outcome.unwrap_err()).starts_with("staged capture"));
    assert!(!destination.exists());
}

#[test]
fn revalidation_binds_each_read_to_the_staged_file_it_inspected() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let destination = w.root.join("capture");
    let alternate = w.root.join("alternate-capture.json");
    let alternate_for_hook = alternate.clone();
    let mut session = MemorySession::one(
        CURRENT_URL,
        successful_exchange(CURRENT_BODY, "2026-08-29T14:45:01Z"),
    );
    capture::set_open_hook({
        let alternate = s(&alternate).to_string();
        move |path| {
            path.ends_with("register-capture.json")
                .then(|| alternate.clone())
        }
    });
    let outcome = with_hooks(
        move |step| {
            if let Step::Validate(staging) = step {
                let capture_path = Path::new(staging).join("register-capture.json");
                let bytes = std::fs::read(&capture_path).unwrap();
                std::fs::write(&alternate_for_hook, &bytes).unwrap();
                let mut changed = bytes;
                changed.push(b' ');
                std::fs::write(&capture_path, changed).unwrap();
            }
            None
        },
        || capture_register_run(s(&manifest), s(&destination), &mut session),
    );
    assert!(message(outcome.unwrap_err()).starts_with("staged capture"));
    assert!(!destination.exists());
}

fn capture_graph(root: &Path, body: &[u8]) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let manifest = root.join("manifest.json");
    std::fs::write(&manifest, json_bytes(&json!([manifest_row(json!({}))]))).unwrap();
    let destination = root.join("capture");
    let mut session = MemorySession::one(
        CURRENT_URL,
        successful_exchange(body, "2026-08-29T14:00:01Z"),
    );
    capture_register_run(s(&manifest), s(&destination), &mut session).unwrap();
    destination
}

#[test]
fn public_graph_validator_accepts_generated_graph_and_rejects_mutation() {
    let w = Work::new();
    let graph = capture_graph(&w.root, CURRENT_BODY);
    validate_capture_graph(s(&graph)).unwrap();
    let baseline = graph.join("monitor-baseline.json");
    let mut bytes = std::fs::read(&baseline).unwrap();
    bytes.push(b' ');
    std::fs::write(&baseline, bytes).unwrap();
    assert_eq!(
        message(validate_capture_graph(s(&graph)).unwrap_err()),
        "staged capture JSON bytes are non-canonical"
    );
}

/// A fresh copy of a generated graph, edited, and the validator's verdict.
fn refused(source: &Path, name: &str, change: impl FnOnce(&mut Value)) -> String {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("capture");
    std::fs::create_dir_all(graph.join("evidence")).unwrap();
    for entry in [
        "monitor-baseline.json",
        "register-capture.json",
        "register-observation.json",
    ] {
        std::fs::copy(source.join(entry), graph.join(entry)).unwrap();
    }
    for entry in std::fs::read_dir(source.join("evidence")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), graph.join("evidence").join(entry.file_name())).unwrap();
    }
    rewrite(&graph.join(name), change);
    message(validate_capture_graph(s(&graph)).unwrap_err())
}

#[test]
fn staged_graph_refusals() {
    let w = Work::new();
    let unchanged = capture_graph(&w.root.join("unchanged"), CURRENT_BODY);
    let superseded = capture_graph(
        &w.root.join("superseded"),
        &version_body(
            "C2004A00467",
            "2026-07-30",
            Some("33"),
            Some("C2026C00300"),
            true,
            "InForce",
        ),
    );
    let baseline = "monitor-baseline.json";
    let capture_file = "register-capture.json";
    let observation = "register-observation.json";
    let cases: Vec<(&Path, &str, Edit, &str)> = vec![
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0] = json!("title")),
            "staged capture baseline title is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["register_id"] = json!("bad")),
            "staged capture baseline identity is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["collection"] = json!("Gazette")),
            "staged capture baseline collection is invalid",
        ),
        // An unhashable collection is Python's TypeError.
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["collection"] = json!(["Act"])),
            "staged capture graph is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["version_is_current"] = json!("yes")),
            "staged capture baseline current flag is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["version_is_current"] = json!(false)),
            "staged capture baseline current date is missing",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["source_url"] = json!("https://example.test/")),
            "staged capture baseline source URL is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["titles"][0]["register_page"] = json!("https://example.test/")),
            "staged capture baseline page URL is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| {
                let mut extra = d["titles"][0].clone();
                extra["register_id"] = json!("C2004A00466");
                extra["source_url"] = json!(
                    extra["source_url"]
                        .as_str()
                        .unwrap()
                        .replace("C2004A00467", "C2004A00466")
                );
                extra["register_page"] = json!(
                    extra["register_page"]
                        .as_str()
                        .unwrap()
                        .replace("C2004A00467", "C2004A00466")
                );
                d["titles"].as_array_mut().unwrap().push(extra);
            }),
            "staged capture baseline order is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["retrieved"] = json!("2026-01-01")),
            "staged capture baseline retrieval date is invalid",
        ),
        (
            &unchanged,
            baseline,
            Box::new(|d| d["retrieved"] = json!("not a date")),
            "staged capture graph is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0] = json!("result")),
            "staged capture result is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["register_id"] = json!("C2004A00466")),
            "staged capture result identity is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["collection"] = json!("LegislativeInstrument")),
            "staged capture result collection is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["state"] = json!("REPEALED")),
            "staged capture result state is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["state"] = json!({"x": 1})),
            "staged capture graph is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["state"] = json!("LOOKUP_FAILED")),
            "staged capture result failure category is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["requests"] = json!([])),
            "staged capture request list is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["requests"][0]["role"] = json!("history")),
            "staged capture request roles are invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| {
                d["results"][0]["requests"][0]["url"] = json!(HISTORY_URL.replace("%24top=1", "x"))
            }),
            "staged capture request URL is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["requests"][0]["attempt_count"] = json!(9)),
            "staged capture request metadata is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| {
                d["results"][0]["requests"][0]["response_headers"] =
                    json!({"odata-version": " 4.0"})
            }),
            "staged capture request headers are invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["requests"][0]["response_sha256"] = Value::Null),
            "staged capture empty response declaration is invalid",
        ),
        (
            &unchanged,
            capture_file,
            Box::new(|d| d["results"][0]["checked_at"] = json!("2026-08-29T14:00:02Z")),
            "staged capture result timestamp is invalid",
        ),
        (
            &unchanged,
            observation,
            Box::new(|d| d["observations"][0] = json!("item")),
            "staged capture observation item is invalid",
        ),
        (
            &unchanged,
            observation,
            Box::new(|d| d["observations"][0]["primary_response_sha256"] = Value::Null),
            "staged capture primary digest is inconsistent",
        ),
        (
            &unchanged,
            observation,
            Box::new(|d| d["observations"][0]["primary_response_media_type"] = json!("text/html")),
            "staged capture primary media type is inconsistent",
        ),
        (
            &unchanged,
            observation,
            Box::new(|d| d["observations"][0]["observed_compilation_number"] = json!("33")),
            "staged capture state fields are inconsistent",
        ),
        (
            &superseded,
            observation,
            Box::new(|d| d["observations"][0]["observed_compilation_number"] = Value::Null),
            "staged capture required state field is missing",
        ),
        (
            &superseded,
            observation,
            Box::new(|d| d["observations"][0]["observed_register_document_id"] = json!("bad")),
            "staged capture document identity is invalid",
        ),
    ];
    for (source, name, change, expected) in cases {
        assert_eq!(refused(source, name, change), expected);
    }
}

#[test]
fn layout_and_file_refusals() {
    let w = Work::new();
    let graph = capture_graph(&w.root.join("one"), CURRENT_BODY);
    std::fs::write(graph.join("notes.txt"), "extra").unwrap();
    assert_eq!(
        message(validate_capture_graph(s(&graph)).unwrap_err()),
        "staged capture root layout is invalid"
    );

    let graph = capture_graph(&w.root.join("two"), CURRENT_BODY);
    std::fs::write(graph.join("monitor-baseline.json"), b"{").unwrap();
    assert_eq!(
        message(validate_capture_graph(s(&graph)).unwrap_err()),
        "staged capture JSON is invalid"
    );

    let graph = capture_graph(&w.root.join("three"), CURRENT_BODY);
    std::fs::remove_file(graph.join("monitor-baseline.json")).unwrap();
    std::fs::create_dir(graph.join("monitor-baseline.json")).unwrap();
    assert_eq!(
        message(validate_capture_graph(s(&graph)).unwrap_err()),
        "staged capture contains a non-ordinary file"
    );

    let root = PyPath::new(s(&w.root));
    assert_eq!(
        message(capture::validate_staged_graph(&root.join("absent")).unwrap_err()),
        "staged capture directory is unavailable"
    );
    std::fs::write(w.root.join("a-file"), "").unwrap();
    assert_eq!(
        message(capture::validate_staged_graph(&root.join("a-file")).unwrap_err()),
        "staged capture contains a non-ordinary directory"
    );
}

#[test]
fn read_failures_and_races_are_refused() {
    let w = Work::new();
    let graph = capture_graph(&w.root.join("graph"), CURRENT_BODY);
    let staged = PyPath::new(s(&graph));
    let cases: Vec<(Hook, &str)> = vec![
        (
            Box::new(|step| match step {
                Step::Lstat(path) if path.ends_with("monitor-baseline.json") => {
                    Some(simulated("denied"))
                }
                _ => None,
            }),
            "staged capture file is unavailable",
        ),
        (
            Box::new(|step| matches!(step, Step::Read(_)).then(|| simulated("io"))),
            "staged capture file could not be read",
        ),
        (
            Box::new({
                let mut calls = 0;
                move |step| {
                    if matches!(step, Step::Identity(_)) {
                        calls += 1;
                        return (calls == 2).then(|| simulated("changed"));
                    }
                    None
                }
            }),
            "staged capture file changed during validation",
        ),
        (
            Box::new(|step| match step {
                Step::List(path) if path.ends_with("capture") => Some(simulated("denied")),
                _ => None,
            }),
            "staged capture layout could not be read",
        ),
        (
            Box::new(|step| match step {
                Step::List(path) if path.ends_with("evidence") => Some(simulated("denied")),
                _ => None,
            }),
            "staged capture evidence layout could not be read",
        ),
    ];
    for (hook, expected) in cases {
        let outcome = with_hooks(hook, || capture::validate_staged_graph(&staged));
        assert_eq!(message(outcome.unwrap_err()), expected);
    }
}

// ------------------------------------------------------------ filesystem

fn session() -> MemorySession {
    MemorySession::one(
        CURRENT_URL,
        successful_exchange(CURRENT_BODY, "2026-08-29T15:00:01Z"),
    )
}

#[test]
fn creates_exactly_one_missing_output_parent_after_validation() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let parent = w.root.join("new-parent");
    let destination = parent.join("capture");
    let paths = capture_register_run(s(&manifest), s(&destination), &mut session()).unwrap();
    assert!(parent.is_dir());
    assert_eq!(paths.capture, s(&destination.join("register-capture.json")));
    let deep = w.root.join("missing-one").join("missing-two");
    let error =
        capture_register_run(s(&manifest), s(&deep.join("capture")), &mut session()).unwrap_err();
    assert_eq!(
        message(error),
        "capture permits only one missing output parent."
    );
    assert!(!w.root.join("missing-one").exists());
}

#[test]
fn invalid_manifest_never_creates_the_permitted_missing_parent() {
    let w = Work::new();
    let manifest = w.manifest_bytes(b"[]");
    let parent = w.root.join("new-parent");
    let error =
        capture_register_run(s(&manifest), s(&parent.join("capture")), &mut session()).unwrap_err();
    assert_eq!(message(error), "manifest must be a non-empty array.");
    assert!(!parent.exists());
}

#[test]
fn rejects_unsafe_or_existing_destinations_without_mutation() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    for name in [".hidden", "bad name", "-option"] {
        let destination = w.root.join(name);
        let error =
            capture_register_run(s(&manifest), s(&destination), &mut session()).unwrap_err();
        assert_eq!(message(error), "capture destination name is unsafe.");
        assert!(!destination.exists());
    }
    let existing_file = w.root.join("existing-file");
    std::fs::write(&existing_file, b"prior bytes").unwrap();
    let existing_directory = w.root.join("existing-directory");
    std::fs::create_dir(&existing_directory).unwrap();
    std::fs::write(existing_directory.join("sentinel.txt"), b"prior directory").unwrap();
    for destination in [&existing_file, &existing_directory, &manifest] {
        let error = capture_register_run(s(&manifest), s(destination), &mut session()).unwrap_err();
        assert_eq!(message(error), "capture destination must not exist.");
    }
    assert_eq!(std::fs::read(&existing_file).unwrap(), b"prior bytes");
    assert_eq!(
        std::fs::read(existing_directory.join("sentinel.txt")).unwrap(),
        b"prior directory"
    );
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        json_bytes(&json!([manifest_row(json!({}))]))
    );
}

#[test]
fn rejects_linked_manifest_ancestor_and_output_parent() {
    let w = Work::new();
    let real_source = w.root.join("real-source");
    std::fs::create_dir(&real_source).unwrap();
    std::fs::write(
        real_source.join("manifest.json"),
        json_bytes(&json!([manifest_row(json!({}))])),
    )
    .unwrap();
    let source_link = w.root.join("source-link");
    let real_output = w.root.join("real-output");
    std::fs::create_dir(&real_output).unwrap();
    let output_link = w.root.join("output-link");
    if !symlink(&real_source, &source_link, true) || !symlink(&real_output, &output_link, true) {
        return;
    }
    let error = capture_register_run(
        s(&source_link.join("manifest.json")),
        s(&w.root.join("capture-source")),
        &mut session(),
    )
    .unwrap_err();
    assert_eq!(message(error), "manifest must have ordinary ancestors.");
    let error = capture_register_run(
        s(&real_source.join("manifest.json")),
        s(&output_link.join("capture")),
        &mut session(),
    )
    .unwrap_err();
    assert_eq!(
        message(error),
        "capture output parent must be an ordinary directory."
    );
    assert_eq!(std::fs::read_dir(&real_output).unwrap().count(), 0);
}

#[test]
fn reparse_output_parent_is_rejected_before_network_or_write() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let output_parent = w.root.join("output");
    std::fs::create_dir(&output_parent).unwrap();
    let marked = s(&output_parent).to_string();
    let mut session = MemorySession::new(vec![]);
    let outcome = with_hooks(
        move |step| match step {
            Step::Reparse(path) if path == marked => Some(simulated("reparse")),
            _ => None,
        },
        || {
            capture_register_run(
                s(&manifest),
                s(&output_parent.join("capture")),
                &mut session,
            )
        },
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture output parent must be an ordinary directory."
    );
    assert_eq!(std::fs::read_dir(&output_parent).unwrap().count(), 0);
}

#[test]
fn raced_destination_is_preserved_and_staging_is_removed() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let destination = w.root.join("capture");
    let racer = destination.clone();
    let outcome = with_hooks(
        move |step| {
            if let Step::Validated(_) = step {
                std::fs::create_dir(&racer).unwrap();
                std::fs::write(racer.join("other-writer.txt"), b"preserve me").unwrap();
            }
            None
        },
        || capture_register_run(s(&manifest), s(&destination), &mut session()),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture destination must not exist."
    );
    assert_eq!(
        std::fs::read(destination.join("other-writer.txt")).unwrap(),
        b"preserve me"
    );
    assert!(!staging_left(&w.root));
}

#[test]
fn write_and_promotion_failures_remove_only_owned_staging() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    std::fs::write(w.root.join("unrelated.txt"), b"keep").unwrap();
    let destination = w.root.join("write-failure");
    let outcome = with_hooks(
        |step| matches!(step, Step::Write(_)).then(|| simulated("private write detail")),
        || capture_register_run(s(&manifest), s(&destination), &mut session()),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture output could not be written."
    );
    assert!(!destination.exists());
    let destination = w.root.join("promotion-failure");
    let outcome = with_hooks(
        |step| matches!(step, Step::Promote(..)).then(|| simulated("private rename detail")),
        || capture_register_run(s(&manifest), s(&destination), &mut session()),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture output could not be promoted."
    );
    assert!(!destination.exists());
    assert!(!staging_left(&w.root));
    assert_eq!(
        std::fs::read(w.root.join("unrelated.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn preflight_refusals() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let output = w.root.join("capture");
    let mut session = MemorySession::new(vec![]);
    let run = |hook: Hook, output: &Path, session: &mut MemorySession| {
        message(
            with_hooks(hook, || {
                capture_register_run(s(&manifest), s(output), session)
            })
            .unwrap_err(),
        )
    };
    let target = s(&output).to_string();
    assert_eq!(
        run(
            Box::new(
                move |step| matches!(step, Step::Lstat(p) if p == target).then(|| simulated("x"))
            ),
            &output,
            &mut session
        ),
        "capture destination could not be inspected."
    );
    assert_eq!(
        run(
            Box::new(|step| matches!(step, Step::SameLocation).then(|| simulated("x"))),
            &output,
            &mut session
        ),
        "capture destination must not replace its manifest input."
    );
    assert_eq!(
        run(
            Box::new(|_| None),
            &w.root.join(".hidden").join("capture"),
            &mut session
        ),
        "capture output parent name is unsafe."
    );
    // The manifest's two ancestor checks look at the directory first.
    let parent = s(&w.root).to_string();
    let mut looks = 0;
    assert_eq!(
        run(
            Box::new(move |step| {
                if matches!(step, Step::Lstat(p) if p == parent) {
                    looks += 1;
                    return (looks == 3).then(|| simulated("denied"));
                }
                None
            }),
            &output,
            &mut session
        ),
        "capture output parent could not be inspected."
    );
    assert!(session.urls().is_empty());
}

#[test]
fn staging_removal_refusals() {
    let w = Work::new();
    let root = PyPath::new(s(&w.root));
    let prefix = ".capture.register-capture-";
    let staging = root.join(&format!("{prefix}x.tmp"));
    assert_eq!(
        message(capture::remove_owned_staging(&root.join("other.tmp"), &root, prefix).unwrap_err()),
        "refusing to remove staging outside the capture boundary"
    );
    let outcome = with_hooks(
        |step| matches!(step, Step::Lstat(_)).then(|| simulated("x")),
        || capture::remove_owned_staging(&staging, &root, prefix),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture staging could not be inspected"
    );
    std::fs::write(staging.display(), "").unwrap();
    assert_eq!(
        message(capture::remove_owned_staging(&staging, &root, prefix).unwrap_err()),
        "capture staging is no longer an ordinary directory"
    );
    std::fs::remove_file(staging.display()).unwrap();
    std::fs::create_dir(staging.display()).unwrap();
    let outcome = with_hooks(
        |step| matches!(step, Step::Remove(_)).then(|| simulated("busy")),
        || capture::remove_owned_staging(&staging, &root, prefix),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture staging could not be removed"
    );
}

#[test]
fn a_missing_output_parent_that_cannot_be_created_is_reported() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let mut session = session();
    let outcome = with_hooks(
        |step| matches!(step, Step::Mkdir(_)).then(|| simulated("x")),
        || {
            capture_register_run(
                s(&manifest),
                s(&w.root.join("new-parent").join("capture")),
                &mut session,
            )
        },
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture output parent could not be created."
    );
    assert!(!w.root.join("new-parent").exists());
}

/// A capture's directories carry the protected DACL CPython 3.13 and later
/// give `os.mkdir(path, 0o700)`.
#[cfg(windows)]
#[test]
fn new_directories_are_private_as_python_makes_them() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let parent = w.root.join("new-parent");
    capture_register_run(s(&manifest), s(&parent.join("capture")), &mut session()).unwrap();
    for path in [
        parent.clone(),
        parent.join("capture"),
        parent.join("capture").join("evidence"),
    ] {
        let done = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Acl -LiteralPath '{}').Sddl", s(&path)),
            ])
            .output()
            .unwrap();
        let sddl = String::from_utf8_lossy(&done.stdout).trim().to_string();
        assert_eq!(
            &sddl[sddl.find("D:").unwrap()..],
            "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
            "{}",
            s(&path)
        );
    }
}

// ------------------------------------------------------------------- CLI

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|a| a.to_string()).collect()
}

#[test]
fn cli_captures_and_reports_the_three_contract_paths() {
    let w = Work::new();
    let manifest = w.manifest(&json!([manifest_row(json!({}))]));
    let destination = w.root.join("capture");
    let mut run = Run::default();
    let code = capture::main_with(
        &argv(&[s(&manifest), "--out", s(&destination)]),
        &mut run,
        &mut |_| {
            Box::new(MemorySession::one(
                CURRENT_URL,
                successful_exchange(CURRENT_BODY, "2026-08-29T16:00:01Z"),
            ))
        },
    )
    .unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        format!(
            "baseline: {}\ncapture: {}\nobservation: {}\n",
            s(&destination.join("monitor-baseline.json")),
            s(&destination.join("register-capture.json")),
            s(&destination.join("register-observation.json"))
        )
    );
}

#[test]
fn cli_uses_exit_two_and_a_bounded_error_for_invalid_input() {
    let w = Work::new();
    let manifest = w.manifest_bytes(b"[]");
    let outcome = capture::main_with(
        &argv(&[s(&manifest), "--out", s(&w.root.join("capture"))]),
        &mut Run::default(),
        &mut |_| Box::new(MemorySession::new(vec![])),
    );
    match outcome {
        Err(tax_radar_au::argparse::Exit::Error(text)) => {
            assert_eq!(
                text,
                "usage: capture_register [-h] --out OUT manifest\n\
                 capture_register: error: manifest must be a non-empty array.\n"
            );
            assert!(!text.contains(s(&w.root)));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn cli_requires_manifest_and_output_arguments() {
    let outcome = capture::main_with(&argv(&[]), &mut Run::default(), &mut |_| {
        Box::new(MemorySession::new(vec![]))
    });
    assert!(matches!(
        outcome,
        Err(tax_radar_au::argparse::Exit::Error(_))
    ));
}
