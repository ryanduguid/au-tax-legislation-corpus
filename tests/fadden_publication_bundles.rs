//! `fadden export_publication_bundles`: ported from
//! tests/corpus/test_publication_bundle_export.py and
//! test_publication_bundle_paths.py. The check that a live export leaves the
//! synthetic fixture unchanged stays with the Python live exporter.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::monitor_contract::{project_baseline, project_observation};
use tax_radar_au::fadden::publication_bundles::{
    self as exporter, Step, build_publication_bundles, bundle_bytes, export_publication_bundles,
};
use tax_radar_au::fadden::{Exc, Run};
use tax_radar_au::pyjson::{self, Json};
use tax_radar_au::pyos::OsError;
use tax_radar_au::pypath::PyPath;

/// A change to the projected baseline and observation.
type Mutation = fn(&mut Value, &mut Value);

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RID: &str = "C2099A00001";
const BUNDLE: &str = "bundle-frl-c2099a00001-c2099c00002-r1.json";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/fixtures/publication")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap()
}

fn producer_version() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("VERSION"))
        .unwrap()
        .trim()
        .to_string()
}

fn j(value: &Value) -> Json {
    pyjson::loads(&value.to_string()).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn message(error: Exc) -> String {
    match error {
        Exc::Value {
            class: "fadden.export_publication_bundles.PublicationBundleError",
            message,
        } => message,
        other => panic!("not a PublicationBundleError: {other:?}"),
    }
}

/// The projected baseline and observation of the fixture inputs, as JSON
/// values a test can change.
fn projected() -> (Value, Value) {
    let sources: Value = serde_json::from_slice(&fixture("sample-sources.json")).unwrap();
    let facts: Value =
        serde_json::from_slice(&fixture("sample-observation-facts-v3.json")).unwrap();
    let baseline = project_baseline(&j(&sources)).unwrap();
    let observation = project_observation(&j(&sources), &j(&facts)).unwrap();
    let back = |value: &Json| -> Value {
        serde_json::from_str(&pyjson::dumps(value, pyjson::Layout::Canonical)).unwrap()
    };
    (back(&baseline), back(&observation))
}

fn build_with(
    baseline: &Value,
    observation: &Value,
    baseline_digest: &str,
    version: &str,
) -> Result<Vec<Value>, Exc> {
    build_publication_bundles(
        &j(baseline),
        &j(observation),
        baseline_digest,
        DIGEST,
        &Json::str(version),
    )
    .map(|bundles| {
        bundles
            .iter()
            .map(|b| serde_json::from_slice(&bundle_bytes(b).unwrap()).unwrap())
            .collect()
    })
}

fn build(baseline: &Value, observation: &Value) -> Result<Vec<Value>, Exc> {
    build_with(baseline, observation, DIGEST, "1.0.0")
}

fn refused(mutate: impl FnOnce(&mut Value, &mut Value)) -> String {
    let (mut baseline, mut observation) = projected();
    mutate(&mut baseline, &mut observation);
    message(build(&baseline, &observation).unwrap_err())
}

fn add_title(baseline: &mut Value, observation: &mut Value, register_id: &str, superseded: bool) {
    let mut title = baseline["titles"][0].clone();
    title["register_id"] = json!(register_id);
    baseline["titles"].as_array_mut().unwrap().push(title);
    let mut item = observation["observations"][0].clone();
    item["register_id"] = json!(register_id);
    if !superseded {
        item["state"] = json!("UNCHANGED");
        for field in [
            "observed_compilation_number",
            "observed_compilation_date",
            "observed_register_document_id",
        ] {
            item[field] = Value::Null;
        }
    }
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .push(item);
    observation["expected_register_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!(register_id));
}

/// The bundle the exporter builds from the fixture inputs, as bytes.
fn one_bundle_bytes() -> Vec<u8> {
    let sources: Value = serde_json::from_slice(&fixture("sample-sources.json")).unwrap();
    let facts: Value =
        serde_json::from_slice(&fixture("sample-observation-facts-v3.json")).unwrap();
    let baseline = project_baseline(&j(&sources)).unwrap();
    let observation = project_observation(&j(&sources), &j(&facts)).unwrap();
    let bundles = build_publication_bundles(
        &baseline,
        &observation,
        &sha256(&fixture("sample-sources.json")),
        &sha256(&fixture("sample-observation-facts-v3.json")),
        &Json::str(&producer_version()),
    )
    .unwrap();
    assert_eq!(bundles.len(), 1);
    bundle_bytes(&bundles[0]).unwrap()
}

fn simulated(errno: i32, text: &str) -> OsError {
    OsError {
        kind: std::io::Error::from_raw_os_error(errno).kind(),
        text: format!("[Errno {errno}] {text}"),
        strerror: text.into(),
    }
}

// ---------------------------------------------------------------- building

#[test]
fn artificial_payload_has_the_content_digest_declared_by_the_observation() {
    let digest = format!(
        "sha256:{}",
        sha256(&fixture("sample-evidence-payload.json"))
    );
    assert_eq!(
        digest,
        "sha256:cace4b38f998a09ea28ebe1448e0756da72ee05d750e27f8278c15a66d0b5dc3"
    );
    let facts: Value =
        serde_json::from_slice(&fixture("sample-observation-facts-v3.json")).unwrap();
    assert_eq!(facts["observations"][0]["content_sha256"], json!(digest));
}

#[test]
fn builds_one_source_only_bundle_from_a_supported_observation() {
    let bundle: Value = serde_json::from_slice(&one_bundle_bytes()).unwrap();
    assert_eq!(bundle["schema_version"], "evidence-bundle.v1");
    assert_eq!(bundle["bundle_id"], "bundle-frl-c2099a00001-c2099c00002-r1");
    assert_eq!(bundle["development_id"], "dev-frl-c2099a00001-c2099c00002");
    assert_eq!(bundle["mode"], "synthetic");
    assert_eq!(bundle["development"]["authority_status"], "in-force");
    assert_eq!(
        bundle["development"]["published_at"],
        "2026-08-05T00:00:00Z"
    );
    assert_eq!(bundle["development"]["topics"], json!([]));
    assert_eq!(bundle["development"]["affected_practice_areas"], json!([]));
    assert_eq!(bundle["source_event"]["kind"], "compilation-superseded");
    assert_eq!(
        bundle["sources"][0]["content_sha256"],
        "sha256:cace4b38f998a09ea28ebe1448e0756da72ee05d750e27f8278c15a66d0b5dc3"
    );
    assert_eq!(bundle["sources"][0]["rights"]["mode"], "metadata-only");
    assert_eq!(bundle["sources"][0]["evidence"], json!([]));
}

#[test]
fn generated_bundle_matches_the_reviewed_golden_bytes() {
    assert_eq!(one_bundle_bytes(), fixture("evidence-bundle.v1.json"));
}

#[test]
fn refuses_incomplete_or_non_v3_observations() {
    assert_eq!(
        refused(|_, o| o["complete"] = json!(false)),
        "observation must cover the complete baseline scope."
    );
    assert_eq!(
        refused(|_, o| o["schema_version"] = json!("au-tax-register-observation.v2")),
        "observation must use au-tax-register-observation.v3."
    );
}

#[test]
fn refuses_every_unsupported_changed_state() {
    for state in [
        "LOOKUP_FAILED",
        "CURRENT_NO_PUBLISHED_COMPILATION",
        "NO_LONGER_IN_FORCE",
    ] {
        assert_eq!(
            refused(|_, o| o["observations"][0]["state"] = json!(state)),
            format!("observation state {state} cannot support a publication bundle.")
        );
    }
}

#[test]
fn refuses_mismatched_collection_or_missing_content_evidence() {
    assert_eq!(
        refused(|_, o| o["observations"][0]["collection"] = json!("LegislativeInstrument")),
        "observation 1 collection does not match the baseline."
    );
    for field in ["content_sha256", "content_kind", "content_media_type"] {
        assert_eq!(
            refused(|_, o| {
                o["observations"][0].as_object_mut().unwrap().remove(field);
            }),
            format!("observation 1 is missing {field}.")
        );
    }
}

#[test]
fn refuses_an_observation_that_is_not_a_newer_compilation() {
    let not_newer = "observation 1 is not a newer published compilation.";
    assert_eq!(
        refused(|_, o| o["observations"][0]["observed_compilation_number"] = json!("1")),
        not_newer
    );
    assert_eq!(
        refused(|_, o| o["observations"][0]["observed_compilation_date"] = json!("2026-07-01")),
        not_newer
    );
}

#[test]
fn refuses_values_outside_the_publisher_contract() {
    let cases: [(Mutation, &str); 8] = [
        (
            |b, _| b["titles"][0]["name"] = json!("x".repeat(201)),
            "baseline title name must fit the publisher's XML text limit of 200 characters.",
        ),
        (
            |b, _| b["titles"][0]["name"] = json!("Invalid \u{fffe} title"),
            "baseline title name must fit the publisher's XML text limit of 200 characters.",
        ),
        (
            |b, _| b["titles"][0]["compilation_number"] = json!("1".repeat(81)),
            "baseline title compilation_number must fit the publisher's XML text limit of 80 characters.",
        ),
        (
            |_, o| o["observations"][0]["observed_compilation_number"] = json!("2".repeat(81)),
            "observation 1 observed_compilation_number must fit the publisher's XML text limit of 80 characters.",
        ),
        (
            |_, o| {
                o["observations"][0]["evidence_url"] = json!(format!(
                    "https://www.legislation.gov.au/{}",
                    "x".repeat(2025)
                ))
            },
            "observation 1 evidence_url must be a permitted HTTPS URL.",
        ),
        (
            |_, o| {
                o["observations"][0]["evidence_url"] =
                    json!("https://www.legislation.gov.au/C2099A99999/latest/text")
            },
            "observation 1 evidence_url must be a permitted HTTPS URL.",
        ),
        (
            |_, o| {
                o["observations"][0]["evidence_url"] =
                    json!("https://www.legislation.gov.au:8443/C2099A00001/latest/text")
            },
            "observation 1 evidence_url must be a permitted HTTPS URL.",
        ),
        (
            |_, o| o["observations"][0]["content_kind"] = json!("summary"),
            "observation 1 content_kind is unsupported.",
        ),
    ];
    for (mutate, expected) in cases {
        assert_eq!(refused(mutate), expected);
    }
    // A lone surrogate cannot be encoded as UTF-8, so is not a permitted URL.
    let (baseline, observation) = projected();
    let text = observation
        .to_string()
        .replace("/latest/text\"", "/latest/text\\ud800\"");
    let observation = pyjson::loads(&text).unwrap();
    let error = build_publication_bundles(
        &j(&baseline),
        &observation,
        DIGEST,
        DIGEST,
        &Json::str("1.0.0"),
    )
    .unwrap_err();
    assert_eq!(
        message(error),
        "observation 1 evidence_url must be a permitted HTTPS URL."
    );
    // The default port, an upper-case host and an empty query pass, as they
    // pass Python's check of urlsplit's parts.
    for url in [
        "https://WWW.legislation.gov.au:443/C2099A00001/latest/text",
        "https://www.legislation.gov.au/C2099A00001/latest/text?",
    ] {
        let (baseline, mut observation) = projected();
        observation["observations"][0]["evidence_url"] = json!(url);
        assert_eq!(build(&baseline, &observation).unwrap().len(), 1, "{url}");
    }
}

#[test]
fn refuses_generated_identifiers_that_exceed_the_publisher_contract() {
    let text = refused(|b, o| {
        let (register_id, document_id) = ("A".repeat(40), "C".repeat(40));
        b["titles"][0]["register_id"] = json!(register_id);
        o["expected_register_ids"] = json!([register_id]);
        o["observations"][0]["register_id"] = json!(register_id);
        o["observations"][0]["observed_register_document_id"] = json!(document_id);
        o["observations"][0]["evidence_url"] = json!(format!(
            "https://www.legislation.gov.au/{register_id}/latest/text"
        ));
    });
    assert_eq!(
        text,
        "generated bundle_id is not a safe publisher identifier."
    );
}

#[test]
fn refuses_a_duplicate_output_identity() {
    assert_eq!(
        refused(|_, o| {
            let first = o["observations"][0].clone();
            o["observations"].as_array_mut().unwrap().push(first);
        }),
        "observation contains duplicate register identifiers."
    );
    assert_eq!(
        refused(|b, o| add_title(b, o, &RID.to_lowercase(), true)),
        "observations produce duplicate bundle identities."
    );
}

#[test]
fn reversing_supported_observations_keeps_sorted_bundle_bytes() {
    let (mut baseline, mut observation) = projected();
    let mut second_title = baseline["titles"][0].clone();
    second_title["register_id"] = json!("C2099A00002");
    second_title["name"] = json!("Sample Income Tax Act 2099");
    baseline["titles"]
        .as_array_mut()
        .unwrap()
        .push(second_title);
    let mut second = observation["observations"][0].clone();
    second["register_id"] = json!("C2099A00002");
    second["observed_register_document_id"] = json!("C2099C00003");
    second["evidence_id"] = json!("ev-c2099a00002-01");
    second["evidence_url"] = json!("https://www.legislation.gov.au/C2099A00002/latest/text");
    observation["expected_register_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("C2099A00002"));
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let forward = build(&baseline, &observation).unwrap();
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(build(&baseline, &observation).unwrap(), forward);
    assert_eq!(forward.len(), 2);
}

#[test]
fn document_shape_refusals() {
    let cases: [(Mutation, &str); 9] = [
        (
            |_, o| o["mode"] = json!("live"),
            "observation mode must be synthetic.",
        ),
        (
            |b, _| b["titles"] = json!({}),
            "baseline titles and observations must be arrays.",
        ),
        (
            |_, o| o["expected_register_ids"] = json!(RID),
            "expected_register_ids must be an array.",
        ),
        (
            |b, _| b["titles"].as_array_mut().unwrap().push(json!("title")),
            "baseline title 2 must be an object.",
        ),
        (
            |b, _| {
                let first = b["titles"][0].clone();
                b["titles"].as_array_mut().unwrap().push(first);
            },
            "baseline contains duplicate register identifiers.",
        ),
        (
            |_, o| o["expected_register_ids"] = json!([]),
            "observation scope does not match the baseline.",
        ),
        (
            |_, o| {
                o["observations"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("item"))
            },
            "observation 2 must be an object.",
        ),
        (
            |_, o| o["observations"][0]["register_id"] = json!("C2099A00009"),
            "observation contains an identifier outside the baseline.",
        ),
        (
            |b, o| {
                add_title(b, o, "C2099A00002", false);
                o["observations"].as_array_mut().unwrap().pop();
            },
            "observation does not cover every baseline title.",
        ),
    ];
    for (mutate, expected) in cases {
        assert_eq!(refused(mutate), expected);
    }
    let (_, observation) = projected();
    let error = build_publication_bundles(
        &Json::Array(Vec::new()),
        &j(&observation),
        DIGEST,
        DIGEST,
        &Json::str("1.0.0"),
    )
    .unwrap_err();
    assert_eq!(message(error), "baseline and observation must be objects.");
}

#[test]
fn field_refusals() {
    let cases: [(Mutation, &str); 7] = [
        (
            |_, o| o["observations"][0]["state"] = json!(" SUPERSEDED"),
            "observation 1 state must be non-empty, trimmed text.",
        ),
        (
            |_, o| o["observations"][0]["register_id"] = json!("C2099/A00001"),
            "observation 1 register_id is not a safe identifier.",
        ),
        (
            |b, o| {
                b["titles"][0]["collection"] = json!("Gazette");
                o["observations"][0]["collection"] = json!("Gazette");
            },
            "observation 1 collection is unsupported.",
        ),
        (
            |_, o| {
                o["observations"][0]["content_sha256"] = json!(format!("sha256:{}", "A".repeat(64)))
            },
            "observation 1 content_sha256 is invalid.",
        ),
        (
            |_, o| o["observations"][0]["content_media_type"] = json!("text/plain"),
            "observation 1 content_media_type is unsupported.",
        ),
        (
            |_, o| o["observations"][0]["observed_compilation_date"] = json!("2026-02-30"),
            "observation 1 observed_compilation_date must be an ISO calendar date.",
        ),
        (
            |_, o| o["observations"][0]["observed_compilation_date"] = json!("20260805"),
            "observation 1 observed_compilation_date must be an ISO calendar date.",
        ),
    ];
    for (mutate, expected) in cases {
        assert_eq!(refused(mutate), expected);
    }
    let (baseline, observation) = projected();
    assert_eq!(
        message(build_with(&baseline, &observation, &"A".repeat(64), "1.0.0").unwrap_err()),
        "baseline_sha256 must be a lowercase SHA-256 digest."
    );
    assert_eq!(
        message(build_with(&baseline, &observation, DIGEST, " 1.0").unwrap_err()),
        "producer_version must be non-empty, trimmed text."
    );
}

#[test]
fn unchanged_titles_produce_no_bundle() {
    let (mut baseline, mut observation) = projected();
    add_title(&mut baseline, &mut observation, "C2099A00002", false);
    let ids: Vec<Value> = build(&baseline, &observation)
        .unwrap()
        .iter()
        .map(|b| b["source_event"]["register_id"].clone())
        .collect();
    assert_eq!(ids, [json!(RID)]);
}

// ---------------------------------------------------------------- the export

struct Work {
    _dir: tempfile::TempDir,
    root: PathBuf,
    sources: PathBuf,
    facts: PathBuf,
}

impl Work {
    fn new() -> Work {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let work = Work {
            sources: root.join("sources.json"),
            facts: root.join("facts.json"),
            root,
            _dir: dir,
        };
        std::fs::write(&work.sources, fixture("sample-sources.json")).unwrap();
        std::fs::write(&work.facts, fixture("sample-observation-facts-v3.json")).unwrap();
        work
    }

    fn export_to(&self, output: &Path) -> Result<Vec<String>, Exc> {
        export_publication_bundles(s(&self.sources), s(&self.facts), s(output))
    }

    fn export(&self) -> Result<Vec<String>, Exc> {
        self.export_to(&self.root.join("bundles"))
    }

    fn refused(&self, output: &Path) -> String {
        message(self.export_to(output).unwrap_err())
    }

    fn staging_left(&self) -> bool {
        std::fs::read_dir(&self.root).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .contains(".publication-bundles-")
        })
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn with_faults<T>(
    hook: impl FnMut(Step) -> Option<OsError> + 'static,
    body: impl FnOnce() -> T,
) -> T {
    exporter::set_fault_hook(hook);
    let outcome = body();
    exporter::set_fault_hook(|_| None);
    outcome
}

#[test]
fn exports_all_files_as_one_new_immutable_directory() {
    let w = Work::new();
    let output = w.root.join("bundles");
    let written = w.export().unwrap();
    let expected = output.join(BUNDLE);
    assert_eq!(written, [s(&expected).to_string()]);
    assert_eq!(std::fs::read(&expected).unwrap(), one_bundle_bytes());
    assert!(!w.staging_left());
    // An existing output is never replaced, repaired or deleted.
    let text = message(w.export().unwrap_err());
    assert!(
        text.starts_with("publication bundle destination must not exist: "),
        "{text}"
    );
    assert_eq!(std::fs::read(&expected).unwrap(), one_bundle_bytes());
}

#[test]
fn creates_one_missing_output_parent_after_validating_inputs() {
    let w = Work::new();
    let parent = w.root.join("build");
    let written = w.export_to(&parent.join("bundles")).unwrap();
    assert_eq!(written.len(), 1);
    assert!(Path::new(&written[0]).is_file());
    assert!(parent.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&parent).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}

#[test]
fn invalid_input_does_not_create_a_missing_output_parent() {
    let w = Work::new();
    std::fs::write(
        &w.facts,
        b"{\"schema_version\":\"one\",\"schema_version\":\"two\"}\n",
    )
    .unwrap();
    let parent = w.root.join("build");
    let text = w.refused(&parent.join("bundles"));
    assert!(
        text.starts_with("observation facts input contains duplicate JSON members: "),
        "{text}"
    );
    assert!(!parent.exists());
}

#[test]
fn refuses_existing_destinations_without_mutating_them() {
    for populated in [false, true] {
        let w = Work::new();
        let output = w.root.join("bundles");
        std::fs::create_dir(&output).unwrap();
        if populated {
            std::fs::write(output.join("keep.txt"), b"keep").unwrap();
        }
        assert!(
            w.refused(&output)
                .starts_with("publication bundle destination must not exist: ")
        );
        let names: Vec<_> = std::fs::read_dir(&output).unwrap().collect();
        assert_eq!(names.len(), usize::from(populated));
        if populated {
            assert_eq!(std::fs::read(output.join("keep.txt")).unwrap(), b"keep");
        }
    }
}

#[test]
fn refuses_an_input_alias_or_non_regular_input() {
    let w = Work::new();
    let before = std::fs::read(&w.sources).unwrap();
    assert_eq!(
        w.refused(&w.sources),
        "publication bundle output would replace an input."
    );
    assert_eq!(std::fs::read(&w.sources).unwrap(), before);
    let directory = w.root.join("directory-input");
    std::fs::create_dir(&directory).unwrap();
    let text = message(
        export_publication_bundles(s(&directory), s(&w.facts), s(&w.root.join("bundles")))
            .unwrap_err(),
    );
    assert!(
        text.starts_with("sources input must be an ordinary file: "),
        "{text}"
    );
    assert!(!w.root.join("bundles").exists());
}

#[test]
fn refuses_a_destination_link_without_touching_its_target() {
    let w = Work::new();
    let target = w.root.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), b"keep").unwrap();
    let output = w.root.join("bundles");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &output);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&target, &output);
    if let Err(error) = made {
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: directory links are unavailable ({error})");
        return;
    }
    assert!(
        w.refused(&output)
            .starts_with("publication bundle destination must not exist: ")
    );
    assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"keep");
}

#[test]
fn staged_write_failure_removes_only_exporter_owned_staging() {
    let w = Work::new();
    std::fs::write(w.root.join("unrelated.txt"), b"keep").unwrap();
    let outcome = with_faults(
        |step| matches!(step, Step::Write(_)).then(|| simulated(5, "injected write failure")),
        || w.export(),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "publication bundle output could not be written: [Errno 5] injected write failure."
    );
    assert!(!w.root.join("bundles").exists());
    assert_eq!(
        std::fs::read(w.root.join("unrelated.txt")).unwrap(),
        b"keep"
    );
    assert!(!w.staging_left());
}

#[test]
fn promotion_failure_removes_only_exporter_owned_staging() {
    let w = Work::new();
    std::fs::write(w.root.join("unrelated.txt"), b"keep").unwrap();
    let outcome = with_faults(
        |step| {
            matches!(step, Step::Promote(..)).then(|| simulated(5, "injected promotion failure"))
        },
        || w.export(),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "publication bundle output could not be promoted: [Errno 5] injected promotion failure."
    );
    assert!(!w.root.join("bundles").exists());
    assert_eq!(
        std::fs::read(w.root.join("unrelated.txt")).unwrap(),
        b"keep"
    );
    assert!(!w.staging_left());
}

#[test]
fn an_output_created_after_the_check_is_never_replaced() {
    let w = Work::new();
    let output = w.root.join("bundles");
    let racer = output.clone();
    let outcome = with_faults(
        move |step| {
            if let Step::Promote(..) = step {
                std::fs::create_dir(&racer).unwrap();
            }
            None
        },
        || w.export(),
    );
    let text = message(outcome.unwrap_err());
    assert!(
        text.starts_with("publication bundle output could not be promoted: "),
        "{text}"
    );
    assert!(output.is_dir() && std::fs::read_dir(&output).unwrap().next().is_none());
    assert!(!w.staging_left());
}

#[test]
fn unsafe_names_and_missing_inputs_are_refused() {
    let w = Work::new();
    assert_eq!(
        w.refused(&w.root.join(".hidden")),
        "publication bundle output name is unsafe."
    );
    assert_eq!(
        w.refused(&w.root.join("x".repeat(101))),
        "publication bundle output name is unsafe."
    );
    assert_eq!(
        w.refused(&w.root.join(".parent").join("bundles")),
        "publication bundle output parent name is unsafe."
    );
    assert!(
        w.refused(&w.root.join("a").join("b").join("bundles"))
            .starts_with("publication bundle output parent does not exist: ")
    );
    let a_file = w.root.join("a-file");
    std::fs::write(&a_file, b"").unwrap();
    assert!(
        w.refused(&a_file.join("bundles"))
            .starts_with("publication bundle output parent must be an ordinary directory: ")
    );
    std::fs::remove_file(&w.facts).unwrap();
    let text = message(w.export().unwrap_err());
    assert_eq!(
        text,
        format!("observation facts input does not exist: {}.", s(&w.facts))
    );
}

#[test]
fn a_linked_output_grandparent_is_refused() {
    let w = Work::new();
    std::fs::create_dir(w.root.join("a")).unwrap();
    let linked = w.root.join("linked");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(w.root.join("a"), &linked);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(w.root.join("a"), &linked);
    if let Err(error) = made {
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: symbolic links are unavailable ({error})");
        return;
    }
    assert!(
        w.refused(&linked.join("child").join("bundles"))
            .starts_with("publication bundle output parent must be an ordinary directory: ")
    );
    assert!(!w.root.join("a").join("child").exists());
}

#[test]
fn inspection_failures_are_reported() {
    let w = Work::new();
    let cases = [
        (
            "sources input cannot be inspected: ",
            w.sources.clone(),
            w.root.join("bundles"),
        ),
        (
            "publication bundle output parent cannot be inspected: ",
            w.root.clone(),
            w.root.join("bundles"),
        ),
        (
            "publication bundle destination cannot be inspected: ",
            w.root.join("bundles"),
            w.root.join("bundles"),
        ),
        (
            "publication bundle output parent cannot be inspected: ",
            w.root.clone(),
            w.root.join("new").join("bundles"),
        ),
    ];
    for (expected, target, output) in cases {
        let denied = s(&target).to_string();
        let outcome = with_faults(
            move |step| match step {
                Step::Lstat(path) if path == denied => Some(simulated(13, "denied")),
                _ => None,
            },
            || w.export_to(&output),
        );
        let text = message(outcome.unwrap_err());
        assert!(text.starts_with(expected), "{text}");
        assert!(text.ends_with(" ([Errno 13] denied)."), "{text}");
    }
}

#[test]
fn contract_input_and_version_failures_are_bounded() {
    let w = Work::new();
    let mut facts: Value =
        serde_json::from_slice(&fixture("sample-observation-facts-v3.json")).unwrap();
    let item = &mut facts["observations"][0];
    item["state"] = json!("NO_LONGER_IN_FORCE");
    for field in [
        "observed_compilation_number",
        "observed_compilation_date",
        "observed_register_document_id",
    ] {
        item[field] = Value::Null;
    }
    std::fs::write(&w.facts, facts.to_string()).unwrap();
    assert_eq!(
        message(w.export().unwrap_err()),
        "observation state NO_LONGER_IN_FORCE cannot support a publication bundle."
    );
    std::fs::write(&w.facts, fixture("sample-observation-facts-v3.json")).unwrap();
    // VERSION names a directory: a read error that is not a missing file.
    let version_root = w.root.join("version-root");
    std::fs::create_dir_all(version_root.join("VERSION")).unwrap();
    exporter::set_version_dir(Some(s(&version_root)));
    let outcome = w.export();
    exporter::set_version_dir(None);
    let text = message(outcome.unwrap_err());
    assert!(
        text.starts_with("publication bundle inputs could not be read: "),
        "{text}"
    );
    assert!(!w.root.join("bundles").exists());
}

#[test]
fn version_metadata_falls_back_to_the_installed_package() {
    let w = Work::new();
    let site = w.root.join("site");
    let info = site.join("Tax_Radar.AU-9.9.9.dist-info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(
        info.join("METADATA"),
        "Metadata-Version: 2.1\nName: tax-radar-au\nVersion: 9.9.9\n\nVersion: 0\n",
    )
    .unwrap();
    exporter::set_version_dir(Some(s(&site)));
    let written = w.export();
    std::fs::remove_dir_all(&info).unwrap();
    let missing = w.export_to(&w.root.join("other"));
    exporter::set_version_dir(None);
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(&written.unwrap()[0]).unwrap()).unwrap();
    assert_eq!(bundle["producer"]["version"], "9.9.9");
    assert_eq!(
        message(missing.unwrap_err()),
        "producer version metadata is unavailable."
    );
}

#[test]
fn a_missing_output_parent_that_cannot_be_created_is_reported() {
    let w = Work::new();
    let outcome = with_faults(
        |step| matches!(step, Step::Mkdir(_)).then(|| simulated(13, "denied")),
        || w.export_to(&w.root.join("new").join("bundles")),
    );
    let text = message(outcome.unwrap_err());
    assert_eq!(
        text,
        format!(
            "publication bundle output parent could not be created: {} ([Errno 13] denied).",
            s(&w.root.join("new"))
        )
    );
}

#[test]
fn staging_cleanup_failures() {
    let w = Work::new();
    let root = PyPath::new(s(&w.root));
    match exporter::remove_owned_staging(&root.join("other"), &root, ".x") {
        Err(Exc::Runtime(text)) => assert_eq!(
            text,
            "refusing to remove a staging path outside the exporter boundary"
        ),
        other => panic!("{other:?}"),
    }
    let outcome = with_faults(
        |step| matches!(step, Step::Cleanup(_)).then(|| simulated(5, "cleanup failed")),
        || w.export(),
    );
    match outcome.unwrap_err() {
        Exc::Os(error) => assert_eq!(error.text, "[Errno 5] cleanup failed"),
        other => panic!("{other:?}"),
    }
    assert!(w.root.join("bundles").is_dir());
    // A cleanup failure after an earlier failure keeps the earlier one.
    let outcome = with_faults(
        |step| match step {
            Step::Write(_) => Some(simulated(5, "write failed")),
            Step::Cleanup(_) => Some(simulated(5, "cleanup failed")),
            _ => None,
        },
        || w.export_to(&w.root.join("second")),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "publication bundle output could not be written: [Errno 5] write failed."
    );
}

#[test]
fn main_reports_each_bundle_and_bounds_errors() {
    let w = Work::new();
    let out = w.root.join("bundles");
    let argv: Vec<String> = [s(&w.sources), s(&w.facts), "--out", s(&out)]
        .iter()
        .map(|a| a.to_string())
        .collect();
    let mut run = Run::default();
    assert_eq!(exporter::main(&argv, &mut run).unwrap(), 0);
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        format!("publication evidence bundle: {}\n", s(&out.join(BUNDLE)))
    );
    match exporter::main(&argv, &mut Run::default()) {
        Err(tax_radar_au::argparse::Exit::Error(text)) => {
            assert!(
                text.starts_with(
                    "usage: export_publication_bundles [-h] --out OUT sources facts\n\
                 export_publication_bundles: error: publication bundle destination must not exist: "
                ),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// Off Windows and Linux there is no no-replace directory rename; the export
/// refuses rather than falling back to one that overwrites.
#[cfg(target_os = "linux")]
#[test]
fn linux_promotes_with_renameat2_and_never_replaces() {
    let w = Work::new();
    let output = w.root.join("bundles");
    let racer = output.clone();
    let outcome = with_faults(
        move |step| {
            if let Step::Promote(..) = step {
                std::fs::create_dir(&racer).unwrap();
            }
            None
        },
        || w.export(),
    );
    let text = message(outcome.unwrap_err());
    assert!(
        text.starts_with(
            "publication bundle output could not be promoted: [Errno 17] File exists: "
        ),
        "{text}"
    );
}

// Ported from PublicationRecoveryTests in tests/corpus/test_output_security.py.

#[test]
fn publisher_url_rejects_other_hosts_ports_and_suffixes() {
    let path = "/C2099A00001/latest/text";
    for url in [
        format!("https://example.test{path}"),
        format!("https://www.legislation.gov.au:444{path}"),
        format!("https://www.legislation.gov.au{path}?redirect=other"),
        format!("https://www.legislation.gov.au{path}#other"),
        format!("https://user@www.legislation.gov.au{path}"),
        format!("http://www.legislation.gov.au{path}"),
        format!("https://www.legislation.gov.au{path}/"),
    ] {
        let (baseline, mut observation) = projected();
        observation["observations"][0]["evidence_url"] = json!(url);
        assert_eq!(
            message(build(&baseline, &observation).unwrap_err()),
            "observation 1 evidence_url must be a permitted HTTPS URL.",
            "{url}"
        );
    }
}

#[test]
fn staging_collision_preserves_the_existing_directory() {
    let w = Work::new();
    let staging = w.root.join(".output.publication-bundles-fixed.tmp");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("unrelated.txt"), b"keep").unwrap();
    exporter_fixed_hex(Some("fixed"));
    let outcome = w.export_to(&w.root.join("output"));
    exporter_fixed_hex(None);
    let text = message(outcome.unwrap_err());
    assert!(
        text.starts_with("publication bundle output could not be written: "),
        "{text}"
    );
    assert_eq!(
        std::fs::read(staging.join("unrelated.txt")).unwrap(),
        b"keep"
    );
    assert!(!w.root.join("output").exists());
}

fn exporter_fixed_hex(value: Option<&str>) {
    tax_radar_au::pyos::set_fixed_hex(value);
}

#[test]
fn linked_staging_is_not_recursively_removed() {
    let w = Work::new();
    let target = w.root.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), b"keep").unwrap();
    let staging = w.root.join(".output.publication-bundles-test.tmp");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &staging);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&target, &staging);
    if let Err(error) = made {
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: directory links are unavailable ({error})");
        return;
    }
    let root = PyPath::new(s(&w.root));
    let outcome = exporter::remove_owned_staging(
        &root.join(".output.publication-bundles-test.tmp"),
        &root,
        ".output.publication-bundles-",
    );
    let text = message(outcome.unwrap_err());
    assert!(
        text.starts_with("publication bundle staging path is no longer an ordinary directory: "),
        "{text}"
    );
    assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"keep");
}

/// A staging directory carrying a third-party reparse tag is refused, not
/// removed with its contents.
#[cfg(windows)]
#[test]
fn reparse_staging_is_not_recursively_removed() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    let w = Work::new();
    let staging = w.root.join(".output.publication-bundles-test.tmp");
    std::fs::create_dir(&staging).unwrap();
    let handle = std::fs::OpenOptions::new()
        .access_mode(0x4000_0000) // GENERIC_WRITE
        .custom_flags(0x0020_0000 | 0x0200_0000) // OPEN_REPARSE_POINT | BACKUP_SEMANTICS
        .open(&staging)
        .unwrap();
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0x0000_1234u32.to_le_bytes());
    buffer.extend_from_slice(&4u16.to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&[0x5a; 16]);
    buffer.extend_from_slice(b"test");
    let mut returned = 0u32;
    // SAFETY: the handle is open, the buffer outlives the call and its length is exact.
    let ok = unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            handle.as_raw_handle(),
            windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    drop(handle);
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: cannot set a reparse point ({error})");
        return;
    }
    let root = PyPath::new(s(&w.root));
    let outcome = exporter::remove_owned_staging(
        &root.join(".output.publication-bundles-test.tmp"),
        &root,
        ".output.publication-bundles-",
    );
    assert!(message(outcome.unwrap_err()).contains("no longer an ordinary directory"));
    assert!(staging.symlink_metadata().is_ok());
}

// Added for the Part A final review.

#[test]
fn reversing_observations_keeps_the_exact_bundle_bytes() {
    let (mut baseline, mut observation) = projected();
    let mut second_title = baseline["titles"][0].clone();
    second_title["register_id"] = json!("C2099A00002");
    baseline["titles"]
        .as_array_mut()
        .unwrap()
        .push(second_title);
    let mut second = observation["observations"][0].clone();
    second["register_id"] = json!("C2099A00002");
    second["observed_register_document_id"] = json!("C2099C00003");
    second["evidence_id"] = json!("ev-c2099a00002-01");
    second["evidence_url"] = json!("https://www.legislation.gov.au/C2099A00002/latest/text");
    observation["expected_register_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("C2099A00002"));
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let bytes = |observation: &Value| -> Vec<Vec<u8>> {
        build_publication_bundles(
            &j(&baseline),
            &j(observation),
            DIGEST,
            DIGEST,
            &Json::str("1.0.0"),
        )
        .unwrap()
        .iter()
        .map(|b| bundle_bytes(b).unwrap())
        .collect()
    };
    let forward = bytes(&observation);
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(bytes(&observation), forward);
}

#[test]
fn a_competing_empty_destination_keeps_its_identity() {
    let w = Work::new();
    let output = w.root.join("bundles");
    let racer = output.clone();
    let identity = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen = identity.clone();
    let outcome = with_faults(
        move |step| {
            if let Step::Promote(..) = step {
                std::fs::create_dir(&racer).unwrap();
                *seen.borrow_mut() = Some(tax_radar_au::pyos::path_identity(s(&racer)).unwrap());
            }
            None
        },
        || w.export(),
    );
    assert!(
        message(outcome.unwrap_err())
            .starts_with("publication bundle output could not be promoted: ")
    );
    assert_eq!(
        Some(tax_radar_au::pyos::path_identity(s(&output)).unwrap()),
        *identity.borrow()
    );
    assert!(std::fs::read_dir(&output).unwrap().next().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn a_c_library_without_renameat2_is_refused() {
    let w = Work::new();
    exporter::set_renameat2_missing(true);
    let outcome = w.export();
    exporter::set_renameat2_missing(false);
    assert_eq!(
        message(outcome.unwrap_err()),
        "publication bundle output could not be promoted: [Errno 95] renameat2 is unavailable."
    );
    assert!(!w.root.join("bundles").exists());
    assert!(!w.staging_left());
}

/// The producer version an export records from a distribution's metadata.
fn version_from_metadata(metadata: &[u8]) -> Result<Value, Exc> {
    let w = Work::new();
    let site = w.root.join("site");
    let info = site.join("tax_radar_au-9.9.9.dist-info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("METADATA"), metadata).unwrap();
    exporter::set_version_dir(Some(s(&site)));
    let outcome = w.export();
    exporter::set_version_dir(None);
    let written = outcome?;
    let bundle: Value = serde_json::from_slice(&std::fs::read(&written[0]).unwrap()).unwrap();
    Ok(bundle["producer"]["version"].clone())
}

#[test]
fn installed_metadata_headers_read_as_the_email_parser_reads_them() {
    // Header names are case-insensitive.
    assert_eq!(
        version_from_metadata(b"Metadata-Version: 2.1\nversion: 9.9.9\n").unwrap(),
        "9.9.9"
    );
    // The value keeps trailing blanks, which the publisher's text rule refuses;
    // a Version after the header block is body text, so there is none.
    for metadata in [
        &b"Version: 9.9.9 \n"[..],
        b"Name: tax-radar-au\n\nVersion: 9\n",
    ] {
        assert_eq!(
            message(version_from_metadata(metadata).unwrap_err()),
            "producer_version must be non-empty, trimmed text.",
            "{}",
            String::from_utf8_lossy(metadata)
        );
    }
    match version_from_metadata(b"Version: \xff\n").unwrap_err() {
        Exc::Value {
            class: "UnicodeDecodeError",
            ..
        } => {}
        other => panic!("{other:?}"),
    }
}

/// New directories carry the protected DACL CPython 3.13 and later give
/// `os.mkdir(path, 0o700)`, not the parent's inherited permissions.
#[cfg(windows)]
#[test]
fn new_directories_are_private_as_python_makes_them() {
    let w = Work::new();
    let parent = w.root.join("build");
    w.export_to(&parent.join("bundles")).unwrap();
    for path in [parent.clone(), parent.join("bundles")] {
        let done = std::process::Command::new("powershell")
            // PowerShell 7's module path (inherited from a pwsh parent, as on CI)
            // stops Windows PowerShell loading its own modules.
            .env_remove("PSModulePath")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Acl -LiteralPath '{}').Sddl", s(&path)),
            ])
            .output()
            .unwrap();
        let sddl = String::from_utf8_lossy(&done.stdout).trim().to_string();
        let dacl = &sddl[sddl.find("D:").unwrap()..];
        assert_eq!(
            dacl,
            "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
            "{}",
            s(&path)
        );
    }
}
