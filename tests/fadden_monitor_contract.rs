//! `fadden export_monitor_contract`: ported from tests/corpus/test_monitor_contract.py
//! and test_monitor_contract_paths.py. The documentation check and the
//! cross-system test against the radar binary remain in the Python file.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use tax_radar_au::fadden::monitor_contract::{
    self as contract, Step, project_baseline, project_observation, publish_pair,
};
use tax_radar_au::fadden::{Exc, Run};
use tax_radar_au::pyjson::{self, Json};
use tax_radar_au::pyos::OsError;

/// A change to one fixture document.
type Mutation = fn(&mut Value);

const LOCK: &str = ".monitor-contract.publish.lock";

fn sources_document() -> Value {
    json!({
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": "2026-08-20",
        "source": "Federal Register of Legislation",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "licence": "CC BY 4.0",
        "titles": [
            {
                "register_id": "F2099L00001",
                "name": "Sample Instrument",
                "collection": "LegislativeInstrument",
                "compilation_number": "1",
                "compilation_date": "2099-07-01",
                "version_is_current": true,
                "current_version_start": null,
                "retrieved": "2026-08-20",
                "source_url": "https://example.test/F2099L00001/1",
                "register_page": "https://example.test/F2099L00001/latest/text",
                "words": 42
            },
            {
                "register_id": "C2099A00001",
                "name": "Sample Act",
                "collection": "Act",
                "compilation_number": "2",
                "compilation_date": "2099-08-01",
                "version_is_current": true,
                "current_version_start": null,
                "retrieved": "2026-08-20",
                "source_url": "https://example.test/C2099A00001/2",
                "register_page": "https://example.test/C2099A00001/latest/text",
                "sections_jsonl": "markdown/C2099A00001/sections.jsonl"
            }
        ]
    })
}

fn observation_facts() -> Value {
    json!({
        "schema_version": "au-tax-register-observation-facts.v1",
        "observed_at": "2026-08-21T10:00:00Z",
        "complete": true,
        "observations": [
            {
                "register_id": "F2099L00001",
                "collection": "LegislativeInstrument",
                "state": "UNCHANGED",
                "observed_compilation_number": null,
                "observed_compilation_date": null,
                "observed_register_document_id": null,
                "current_version_start": null,
                "evidence_url": "https://example.test/F2099L00001/latest/text",
                "checked_at": "2026-08-21T10:00:01Z",
                "error_category": null
            },
            {
                "register_id": "C2099A00001",
                "collection": "Act",
                "state": "SUPERSEDED",
                "observed_compilation_number": "3",
                "observed_compilation_date": "2099-09-01",
                "observed_register_document_id": "C2099C00003",
                "current_version_start": null,
                "evidence_url": "https://example.test/C2099A00001/latest/text",
                "checked_at": "2026-08-21T10:00:02Z",
                "error_category": null
            }
        ]
    })
}

fn observation_facts_v2() -> Value {
    let mut facts = observation_facts();
    facts["schema_version"] = json!("au-tax-register-observation-facts.v2");
    facts["scope_id"] = json!("au-primary-tax-legislation.v2");
    for (item, id) in facts["observations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(["EV-F2099L00001-01", "EV-C2099A00001-01"])
    {
        item["evidence_id"] = json!(id);
    }
    facts
}

fn observation_facts_v3() -> Value {
    let mut facts = observation_facts_v2();
    facts["schema_version"] = json!("au-tax-register-observation-facts.v3");
    facts["scope_id"] = json!("au-primary-tax-legislation.v3");
    for item in facts["observations"].as_array_mut().unwrap() {
        item["content_sha256"] = json!(format!("sha256:{}", "a".repeat(64)));
        item["content_kind"] = json!("metadata-response");
        item["content_media_type"] = json!("application/json");
    }
    facts
}

fn j(value: &Value) -> Json {
    pyjson::loads(&value.to_string()).unwrap()
}

/// The value as `json.dumps(sort_keys=True)` writes it: member order sorted.
fn v(value: &Json) -> Value {
    serde_json::from_str(&pyjson::dumps(value, pyjson::Layout::Canonical)).unwrap()
}

fn message(error: Exc) -> String {
    match error {
        Exc::Value {
            class: "fadden.export_monitor_contract.ContractError",
            message,
        } => message,
        other => panic!("not a ContractError: {other:?}"),
    }
}

fn baseline() -> Json {
    project_baseline(&j(&sources_document())).unwrap()
}

fn observe(facts: &Value) -> Result<Value, String> {
    project_observation(&baseline(), &j(facts))
        .map(|o| v(&o))
        .map_err(message)
}

fn baseline_refusal(mutate: impl FnOnce(&mut Value)) -> String {
    let mut sources = sources_document();
    mutate(&mut sources);
    message(project_baseline(&j(&sources)).unwrap_err())
}

fn simulated(text: &str) -> OsError {
    OsError {
        kind: std::io::ErrorKind::Other,
        text: text.into(),
        strerror: text.into(),
    }
}

/// A temporary directory holding the fixture inputs.
struct Work {
    _dir: tempfile::TempDir,
    root: PathBuf,
    sources: PathBuf,
    facts: PathBuf,
    out: PathBuf,
}

impl Work {
    fn new() -> Work {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let root = PathBuf::from(
            root.to_str()
                .unwrap()
                .strip_prefix(r"\\?\")
                .unwrap_or(root.to_str().unwrap()),
        );
        let work = Work {
            sources: root.join("sources.json"),
            facts: root.join("facts.json"),
            out: root.join("out"),
            root,
            _dir: dir,
        };
        work.write(&work.sources, &sources_document());
        work.write(&work.facts, &observation_facts());
        work
    }

    fn write(&self, path: &Path, value: &Value) {
        std::fs::write(path, value.to_string()).unwrap();
    }

    fn publish(&self) -> Result<(String, String), Exc> {
        publish_pair(s(&self.sources), s(&self.facts), s(&self.out))
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.out)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn leftovers(&self) -> Vec<String> {
        self.names()
            .into_iter()
            .filter(|n| n.contains(".monitor-contract-"))
            .collect()
    }

    fn pair(&self) -> (Vec<u8>, Vec<u8>) {
        (
            std::fs::read(self.out.join("monitor-baseline.json")).unwrap(),
            std::fs::read(self.out.join("register-observation.json")).unwrap(),
        )
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

// ------------------------------------------------------------ projection

#[test]
fn projects_rich_sources_to_the_monitor_exact_baseline_shape() {
    let baseline = v(&baseline());
    let keys: Vec<&str> = baseline
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["corpus", "retrieved", "source", "source_api", "titles"]
    );
    let ids: Vec<&str> = baseline["titles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["register_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["C2099A00001", "F2099L00001"]);
    let fields: Vec<&str> = baseline["titles"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        [
            "collection",
            "compilation_date",
            "compilation_number",
            "current_version_start",
            "name",
            "register_id",
            "register_page",
            "retrieved",
            "source_url",
            "version_is_current"
        ]
    );
}

#[test]
fn builds_the_exact_observation_v1_contract_with_sorted_scope() {
    let observation = observe(&observation_facts()).unwrap();
    assert_eq!(
        observation["schema_version"],
        "au-tax-register-observation.v1"
    );
    assert_eq!(observation["mode"], "synthetic");
    assert_eq!(
        observation["expected_register_ids"],
        json!(["C2099A00001", "F2099L00001"])
    );
    let ids: Vec<&str> = observation["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["register_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["C2099A00001", "F2099L00001"]);
    assert!(observation.get("scope_id").is_none());
}

#[test]
fn refuses_a_complete_observation_with_a_missing_baseline_title() {
    let mut facts = observation_facts();
    facts["observations"].as_array_mut().unwrap().pop();
    assert_eq!(
        observe(&facts).unwrap_err(),
        "a complete observation must cover every baseline register_id exactly once."
    );
    facts["complete"] = json!(false);
    assert_eq!(observe(&facts).unwrap()["complete"], false);
}

#[test]
fn refuses_a_state_specific_field_that_would_make_evidence_ambiguous() {
    let mut facts = observation_facts();
    facts["observations"][0]["current_version_start"] = json!("2099-08-20");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "UNCHANGED requires null current_version_start."
    );
}

/// Python checked a state's forbidden fields in set (string-hash) order, so
/// with several present it named any one of them; the binary names the first
/// alphabetically.
#[test]
fn several_forbidden_fields_name_the_first_alphabetically() {
    let mut facts = observation_facts();
    facts["observations"][0]["observed_register_document_id"] = json!("x");
    facts["observations"][0]["error_category"] = json!("x");
    facts["observations"][0]["current_version_start"] = json!("2099-08-20");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "UNCHANGED requires null current_version_start."
    );
}

#[test]
fn refuses_non_utc_timestamps_bad_https_and_duplicate_titles() {
    assert_eq!(
        baseline_refusal(|s| {
            let first = s["titles"][0].clone();
            s["titles"].as_array_mut().unwrap().push(first);
        }),
        "sources contains duplicate register_id values."
    );
    let mut facts = observation_facts();
    facts["observed_at"] = json!("2026-08-21T10:00:00+10:00");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation facts observed_at must be an explicit UTC timestamp ending in Z."
    );
    let mut facts = observation_facts();
    facts["observations"][0]["evidence_url"] = json!("http://example.test/not-secure");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation fact 1 evidence_url must be an https URL."
    );
}

#[test]
fn timestamps_follow_datetime_fromisoformat() {
    for (value, valid) in [
        ("2026-08-21T10:00:00.5Z", true),
        ("2026-08-21T10:00:00.123456Z", true),
        ("2026-08-21T24:00:00Z", true),
        ("2026-08-21T24:00:00.000Z", true),
        ("2026-08-21T24:00:01Z", false),
        ("2026-08-21T24:00:00.000001Z", false),
        ("9999-12-31T24:00:00Z", false),
        ("2026-08-21T23:59:60Z", false),
        ("2026-08-21T10:00:00.1234567Z", false),
        ("2099-02-30T00:00:00Z", false),
        ("2026-08-21T1\u{0660}:00:00Z", false),
        ("2026-08-21T10:00:00z", false),
    ] {
        let mut facts = observation_facts();
        facts["observed_at"] = json!(value);
        assert_eq!(observe(&facts).is_ok(), valid, "{value}");
    }
}

#[test]
fn refuses_duplicate_observations_and_collection_mismatches() {
    let mut facts = observation_facts();
    let first = facts["observations"][0].clone();
    facts["observations"].as_array_mut().unwrap().push(first);
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation facts contains duplicate register_id values."
    );
    let mut facts = observation_facts();
    facts["observations"][0]["collection"] = json!("Act");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation fact 1 collection does not match baseline."
    );
}

#[test]
fn state_specific_fields_cover_no_document_and_lookup_failure() {
    let mut facts = observation_facts();
    let first = facts["observations"][0].as_object_mut().unwrap();
    first.insert("state".into(), json!("CURRENT_NO_PUBLISHED_COMPILATION"));
    first.insert("current_version_start".into(), json!("2099-08-20"));
    let second = facts["observations"][1].as_object_mut().unwrap();
    second.insert("state".into(), json!("LOOKUP_FAILED"));
    for field in [
        "observed_compilation_number",
        "observed_compilation_date",
        "observed_register_document_id",
    ] {
        second.insert(field.into(), Value::Null);
    }
    second.insert("error_category".into(), json!("register_unavailable"));
    let projected = observe(&facts).unwrap();
    let states: Vec<&str> = projected["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["LOOKUP_FAILED", "CURRENT_NO_PUBLISHED_COMPILATION"]
    );
}

#[test]
fn project_observation_v2_nominal() {
    let projected = observe(&observation_facts_v2()).unwrap();
    assert_eq!(
        projected["schema_version"],
        "au-tax-register-observation.v2"
    );
    assert_eq!(projected["scope_id"], "au-primary-tax-legislation.v2");
    assert_eq!(projected["complete"], true);
    assert_eq!(
        projected["observations"][0]["evidence_id"],
        "EV-C2099A00001-01"
    );
    assert_eq!(
        projected["observations"][1]["evidence_id"],
        "EV-F2099L00001-01"
    );
}

#[test]
fn project_observation_v2_requires_scope_and_evidence_ids() {
    let mut facts = observation_facts_v2();
    facts.as_object_mut().unwrap().remove("scope_id");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation facts has an invalid shape."
    );
    let mut facts = observation_facts_v2();
    facts["scope_id"] = json!("   ");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation facts scope_id must be a non-empty string."
    );
    let mut facts = observation_facts_v2();
    facts["observations"][1]["evidence_id"] = facts["observations"][0]["evidence_id"].clone();
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation fact 2 contains duplicate evidence_id."
    );
    let mut facts = observation_facts_v2();
    facts["observations"][0]
        .as_object_mut()
        .unwrap()
        .remove("evidence_id");
    assert_eq!(
        observe(&facts).unwrap_err(),
        "observation fact 1 has an invalid shape."
    );
}

#[test]
fn project_observation_v3_preserves_content_evidence() {
    let projected = observe(&observation_facts_v3()).unwrap();
    assert_eq!(
        projected["schema_version"],
        "au-tax-register-observation.v3"
    );
    assert_eq!(projected["mode"], "synthetic");
    assert_eq!(projected["scope_id"], "au-primary-tax-legislation.v3");
    let first = &projected["observations"][0];
    assert_eq!(
        first["content_sha256"],
        format!("sha256:{}", "a".repeat(64))
    );
    assert_eq!(first["content_kind"], "metadata-response");
    assert_eq!(first["content_media_type"], "application/json");
}

#[test]
fn project_observation_v3_rejects_invalid_content_evidence() {
    for (field, value, expected) in [
        (
            "content_sha256",
            format!("sha256:{}", "A".repeat(64)),
            "content_sha256 is invalid.",
        ),
        (
            "content_sha256",
            format!("sha256:{}", "a".repeat(63)),
            "content_sha256 is invalid.",
        ),
        (
            "content_kind",
            "web-page".into(),
            "content_kind is unsupported.",
        ),
        (
            "content_media_type",
            "application/json; charset=utf-8".into(),
            "content_media_type is unsupported.",
        ),
    ] {
        let mut facts = observation_facts_v3();
        facts["observations"][0][field] = json!(value);
        assert_eq!(
            observe(&facts).unwrap_err(),
            format!("observation fact 1 {expected}")
        );
    }
}

#[test]
fn scalar_refusals() {
    let title = |field: &str, value: &str| {
        let (field, value) = (field.to_string(), value.to_string());
        baseline_refusal(move |s| s["titles"][0][&field] = json!(value))
    };
    assert_eq!(
        title("compilation_date", "2099-7-1"),
        "sources title 1 compilation_date must be an ISO calendar date."
    );
    assert_eq!(
        title("compilation_date", "2099-02-30"),
        "sources title 1 compilation_date must be an ISO calendar date."
    );
    assert_eq!(
        title("compilation_date", "\u{0662}099-02-01"),
        "sources title 1 compilation_date must be an ISO calendar date."
    );
    assert_eq!(
        title("source_url", "https://[::1"),
        "sources title 1 source_url must be an https URL."
    );
    assert_eq!(
        title("source_url", "https://user@example.test/"),
        "sources title 1 source_url must be an https URL."
    );
    assert_eq!(
        title("source_url", "https://example.test:99999/"),
        "sources title 1 source_url must be an https URL."
    );
    assert_eq!(
        baseline_refusal(|s| s["titles"][0] = json!(["not", "an", "object"])),
        "sources title 1 must be an object."
    );
    // Whitespace around a value is trimmed, not refused.
    let mut sources = sources_document();
    sources["titles"][0]["name"] = json!("  Sample Instrument\u{3000}");
    let projected = v(&project_baseline(&j(&sources)).unwrap());
    assert_eq!(projected["titles"][1]["name"], "Sample Instrument");
}

#[test]
fn baseline_refusals() {
    let cases: [(Mutation, &str); 7] = [
        (
            |s| {
                s.as_object_mut().unwrap().remove("source_api");
            },
            "sources is missing source_api.",
        ),
        (
            |s| s["source"] = json!("Somewhere else"),
            "sources source must be Federal Register of Legislation.",
        ),
        (
            |s| s["titles"] = json!([]),
            "sources titles must be a non-empty list.",
        ),
        (
            |s| {
                s["titles"][0].as_object_mut().unwrap().remove("name");
            },
            "sources title 1 is missing required field(s): name.",
        ),
        (
            |s| s["titles"][0]["collection"] = json!("Gazette"),
            "sources title 1 collection is unsupported.",
        ),
        (
            |s| s["titles"][0]["version_is_current"] = json!("yes"),
            "sources title 1 version_is_current must be a boolean.",
        ),
        (
            |s| s["titles"][0]["version_is_current"] = json!(false),
            "sources title 1 current_version_start is required when version_is_current is false.",
        ),
    ];
    for (mutate, expected) in cases {
        assert_eq!(baseline_refusal(mutate), expected);
    }
}

#[test]
fn observation_refusals() {
    let cases: [(Mutation, &str); 4] = [
        (
            |f| {
                f.as_object_mut().unwrap().remove("schema_version");
            },
            "observation facts is missing schema_version.",
        ),
        (
            |f| f["complete"] = json!("yes"),
            "observation facts complete/observations fields are invalid.",
        ),
        (
            |f| f["observations"][0]["register_id"] = json!("C2099A00099"),
            "observation facts contains a register_id outside the baseline scope.",
        ),
        (
            |f| f["observations"][0]["state"] = json!("REPEALED"),
            "observation fact 1 state is unsupported.",
        ),
    ];
    for (mutate, expected) in cases {
        let mut facts = observation_facts();
        mutate(&mut facts);
        assert_eq!(observe(&facts).unwrap_err(), expected);
    }
}

#[test]
fn rejects_monitor_control_characters_in_text_and_urls() {
    let source_cases: [Mutation; 5] = [
        |d| d["corpus"] = json!("corpus\u{1}"),
        |d| d["source"] = json!("Federal Register\u{7f}"),
        |d| d["source_api"] = json!("https://example.test/a\u{1f}b"),
        |d| d["titles"][0]["name"] = json!("name\u{1}"),
        |d| d["titles"][0]["source_url"] = json!("https://example.test/\u{7f}"),
    ];
    for mutate in source_cases {
        assert!(baseline_refusal(mutate).ends_with("must not contain control characters."));
    }
    let fact_cases: [Mutation; 5] = [
        |f| f["schema_version"] = json!("au-tax-register-observation-facts.v1\u{1}"),
        |f| f["observations"][0]["state"] = json!("UNCHANGED\u{1}"),
        |f| f["observations"][0]["evidence_url"] = json!("https://example.test/\u{7f}"),
        |f| f["observations"][1]["observed_compilation_number"] = json!("3\u{1}"),
        |f| f["observations"][1]["observed_register_document_id"] = json!("C2099C00003\u{7f}"),
    ];
    for mutate in fact_cases {
        let mut facts = observation_facts();
        mutate(&mut facts);
        assert!(
            observe(&facts)
                .unwrap_err()
                .ends_with("must not contain control characters.")
        );
    }
}

// ------------------------------------------------------------ publishing

#[test]
fn main_publishes_the_named_pair() {
    let w = Work::new();
    let mut run = Run::default();
    let argv: Vec<String> = [s(&w.sources), s(&w.facts), "--out", s(&w.out)]
        .iter()
        .map(|a| a.to_string())
        .collect();
    assert_eq!(contract::main(&argv, &mut run).unwrap(), 0);
    let baseline = w.out.join("monitor-baseline.json");
    let observation = w.out.join("register-observation.json");
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        format!(
            "monitor baseline: {}\nregister observation: {}\n",
            s(&baseline),
            s(&observation)
        )
    );
    // json.dump(indent=2, sort_keys=True, ensure_ascii=False) and a newline.
    let text = std::fs::read_to_string(&baseline).unwrap();
    assert!(text.starts_with("{\n  \"corpus\": "), "{text}");
    assert!(text.ends_with("}\n") && !text.contains('\r'));
    assert_eq!(
        read_json(&observation)["observations"][0]["state"],
        "SUPERSEDED"
    );
    assert!(w.leftovers().is_empty());
    assert!(!w.out.join(LOCK).exists());
}

#[test]
fn a_contract_error_is_a_usage_error() {
    let w = Work::new();
    let mut sources = sources_document();
    sources["source"] = json!("Somewhere else");
    w.write(&w.sources, &sources);
    let argv: Vec<String> = [s(&w.sources), s(&w.facts), "--out", s(&w.out)]
        .iter()
        .map(|a| a.to_string())
        .collect();
    match contract::main(&argv, &mut Run::default()) {
        Err(tax_radar_au::argparse::Exit::Error(text)) => assert_eq!(
            text,
            "usage: export_monitor_contract [-h] --out OUT sources facts\n\
             export_monitor_contract: error: sources source must be Federal Register of Legislation.\n"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_missing_input_is_unreadable_json() {
    let w = Work::new();
    std::fs::remove_file(&w.sources).unwrap();
    let text = message(w.publish().unwrap_err());
    assert!(
        text.starts_with("sources is unreadable JSON: [Errno 2] "),
        "{text}"
    );
}

#[test]
fn preflight_refuses_every_input_output_collision_without_changing_bytes() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let original = b"{\"must\":\"remain byte-for-byte unchanged\"}\n";
    for input in ["sources", "facts"] {
        for output in ["monitor-baseline.json", "register-observation.json"] {
            let collision = w.out.join(output);
            std::fs::write(&collision, original).unwrap();
            let mut shown = s(&collision).to_string();
            if cfg!(windows) {
                // pathlib compares Windows paths without regard to case.
                shown = shown.to_uppercase();
            }
            let (sources, facts) = if input == "sources" {
                (shown.as_str(), s(&w.facts))
            } else {
                (s(&w.sources), shown.as_str())
            };
            let text = message(publish_pair(sources, facts, s(&w.out)).unwrap_err());
            assert!(
                text.starts_with("monitor output would replace an input"),
                "{text}"
            );
            assert_eq!(std::fs::read(&collision).unwrap(), original);
            std::fs::remove_file(&collision).unwrap();
        }
    }
}

#[test]
fn preflight_refuses_an_existing_output_directory_without_mutation() {
    let w = Work::new();
    let destination = w.out.join("monitor-baseline.json");
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(
        destination.join("must-remain"),
        b"directory contents must remain",
    )
    .unwrap();
    let text = message(w.publish().unwrap_err());
    assert!(text.contains("regular file"), "{text}");
    assert_eq!(
        std::fs::read(destination.join("must-remain")).unwrap(),
        b"directory contents must remain"
    );
    assert!(!w.out.join("register-observation.json").exists());
    assert!(w.leftovers().is_empty());
}

#[test]
fn preflight_refuses_an_existing_output_symlink_without_mutation() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let target = w.root.join("outside.json");
    std::fs::write(&target, b"outside bytes must remain").unwrap();
    let destination = w.out.join("register-observation.json");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &destination);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&target, &destination);
    if let Err(error) = made {
        // ERROR_PRIVILEGE_NOT_HELD: any other failure is a broken test.
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: symbolic links are unavailable to this account ({error})");
        return;
    }
    let text = message(w.publish().unwrap_err());
    assert!(text.contains("regular file"), "{text}");
    assert!(
        destination
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"outside bytes must remain"
    );
    assert!(!w.out.join("monitor-baseline.json").exists());
    assert!(w.leftovers().is_empty());
}

#[cfg(windows)]
#[test]
fn preflight_refuses_an_existing_output_junction_without_mutation() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let outside = w.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let destination = w.out.join("monitor-baseline.json");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&destination)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(made.status.success());
    let text = message(w.publish().unwrap_err());
    assert!(text.contains("regular file"), "{text}");
    assert!(outside.is_dir() && std::fs::read_dir(&outside).unwrap().next().is_none());
    assert!(!w.out.join("register-observation.json").exists());
    assert!(w.leftovers().is_empty());
}

/// A regular file carrying a third-party reparse tag: neither a symbolic link
/// nor a junction.
#[cfg(windows)]
#[test]
fn preflight_refuses_an_existing_output_reparse_point_without_mutation() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let destination = w.out.join("monitor-baseline.json");
    std::fs::write(&destination, b"existing bytes must remain").unwrap();
    let handle = std::fs::OpenOptions::new()
        .access_mode(0x4000_0000) // GENERIC_WRITE
        .custom_flags(0x0020_0000 | 0x0200_0000) // OPEN_REPARSE_POINT | BACKUP_SEMANTICS
        .open(&destination)
        .unwrap();
    // REPARSE_GUID_DATA_BUFFER: tag, data length, reserved, GUID, then data.
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
    let text = message(w.publish().unwrap_err());
    assert!(text.contains("regular file"), "{text}");
    assert!(!w.out.join("register-observation.json").exists());
    assert!(w.leftovers().is_empty());
}

#[cfg(unix)]
#[test]
fn an_uninspectable_destination_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    std::fs::set_permissions(&w.out, std::fs::Permissions::from_mode(0o000)).unwrap();
    let outcome = w.publish();
    std::fs::set_permissions(&w.out, std::fs::Permissions::from_mode(0o700)).unwrap();
    if std::fs::symlink_metadata(w.out.join("x")).is_err() && outcome.is_ok() {
        return; // A privileged account reads through the mode.
    }
    let text = message(outcome.unwrap_err());
    assert!(
        text.starts_with("monitor output path cannot be inspected: "),
        "{text}"
    );
    assert!(text.contains("[Errno 13] Permission denied"), "{text}");
}

#[test]
fn rejects_duplicate_json_members_at_top_and_nested_levels() {
    let w = Work::new();
    std::fs::write(
        &w.sources,
        sources_document().to_string().replacen(
            "\"corpus\":\"Commonwealth tax statutes and legislative instruments\"",
            "\"corpus\":\"first\",\"corpus\":\"second\"",
            1,
        ),
    )
    .unwrap();
    assert_eq!(
        message(w.publish().unwrap_err()),
        "sources contains duplicate JSON members."
    );
    w.write(&w.sources, &sources_document());
    std::fs::write(
        &w.facts,
        observation_facts().to_string().replacen(
            "\"register_id\":\"F2099L00001\"",
            "\"register_id\":\"F2099L00001\",\"register_id\":\"F2099L00001\"",
            1,
        ),
    )
    .unwrap();
    assert_eq!(
        message(w.publish().unwrap_err()),
        "observation facts contains duplicate JSON members."
    );
    assert!(!w.out.exists());
}

#[test]
fn a_lone_surrogate_fails_its_staged_write_and_leaves_nothing_behind() {
    let w = Work::new();
    std::fs::write(
        &w.sources,
        sources_document()
            .to_string()
            .replacen("Sample Act", "Sample \\ud800Act", 1),
    )
    .unwrap();
    match w.publish().unwrap_err() {
        Exc::UnicodeEncode(text) => assert_eq!(
            text,
            "'utf-8' codec can't encode character '\\ud800' in position 8: surrogates not allowed"
        ),
        other => panic!("{other:?}"),
    }
    assert!(w.names().is_empty());
}

#[test]
fn staging_failure_removes_a_previous_temp_file() {
    let w = Work::new();
    let mut calls = 0;
    contract::set_fault_hook(move |step| match step {
        Step::Stage(_) => {
            calls += 1;
            (calls == 2).then(|| simulated("simulated second staging failure"))
        }
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "simulated second staging failure"
    );
    assert!(w.names().is_empty());
}

#[test]
fn live_and_aged_output_locks_fail_closed_without_mutation() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let lock = w.out.join(LOCK);
    std::fs::write(&lock, "live writer").unwrap();
    contract::set_lock_timeout(Duration::ZERO);
    let first = w.publish();
    let aged = std::time::SystemTime::now() - Duration::from_secs(600);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(aged)
        .unwrap();
    let second = w.publish();
    contract::set_lock_timeout(Duration::from_secs(30));
    for outcome in [first, second] {
        let text = message(outcome.unwrap_err());
        assert!(text.contains("locked by another writer"), "{text}");
    }
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "live writer");
    assert_eq!(w.names(), [LOCK]);
}

#[test]
fn competing_writers_leave_an_aged_lock_untouched() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let lock = w.out.join(LOCK);
    std::fs::write(&lock, "failed writer requires operator recovery").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let writers: Vec<_> = (0..2)
        .map(|_| {
            let (barrier, sources, facts, out) = (
                barrier.clone(),
                w.sources.clone(),
                w.facts.clone(),
                w.out.clone(),
            );
            std::thread::spawn(move || {
                contract::set_lock_timeout(Duration::ZERO);
                barrier.wait();
                publish_pair(s(&sources), s(&facts), s(&out)).map_err(message)
            })
        })
        .collect();
    for writer in writers {
        let text = writer.join().unwrap().unwrap_err();
        assert!(text.contains("locked by another writer"), "{text}");
    }
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "failed writer requires operator recovery"
    );
    assert_eq!(w.names(), [LOCK]);
}

fn variant(w: &Work, name: &str, corpus: &str, observed_at: &str) -> (PathBuf, PathBuf) {
    let (sources, facts) = (
        w.root.join(format!("sources-{name}.json")),
        w.root.join(format!("facts-{name}.json")),
    );
    let mut source = sources_document();
    source["corpus"] = json!(corpus);
    let mut observation = observation_facts();
    observation["observed_at"] = json!(observed_at);
    w.write(&sources, &source);
    w.write(&facts, &observation);
    (sources, facts)
}

fn named(path: &str) -> &str {
    Path::new(path).file_name().unwrap().to_str().unwrap()
}

#[test]
fn concurrent_writers_cannot_interleave_a_published_pair() {
    let w = Work::new();
    let (sources_a, facts_a) = variant(&w, "a", "writer A", "2026-08-21T10:01:00Z");
    let (sources_b, facts_b) = variant(&w, "b", "writer B", "2026-08-21T10:02:00Z");
    let (promoted_tx, promoted_rx) = mpsc::channel();
    let (allow_tx, allow_rx) = mpsc::channel::<()>();
    let reached = Arc::new(AtomicBool::new(false));
    let out = w.out.clone();
    let first = std::thread::spawn(move || {
        contract::set_fault_hook(move |step| {
            if let Step::Replace(source, destination) = step
                && source.ends_with(".tmp")
                && named(destination) == "monitor-baseline.json"
            {
                promoted_tx.send(()).unwrap();
                allow_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            None
        });
        publish_pair(s(&sources_a), s(&facts_a), s(&out)).map_err(message)
    });
    promoted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let out = w.out.clone();
    let flag = reached.clone();
    let second = std::thread::spawn(move || {
        contract::set_fault_hook(move |step| {
            if matches!(step, Step::Replace(..)) {
                flag.store(true, Ordering::SeqCst);
            }
            None
        });
        publish_pair(s(&sources_b), s(&facts_b), s(&out)).map_err(message)
    });
    std::thread::sleep(Duration::from_millis(250));
    assert!(!reached.load(Ordering::SeqCst));
    allow_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert_eq!(
        read_json(&w.out.join("monitor-baseline.json"))["corpus"],
        "writer B"
    );
    assert_eq!(
        read_json(&w.out.join("register-observation.json"))["observed_at"],
        "2026-08-21T10:02:00Z"
    );
}

#[test]
fn second_writer_cannot_enter_while_first_writer_rolls_back() {
    let w = Work::new();
    w.publish().unwrap();
    let (sources_a, facts_a) = variant(&w, "a", "writer A", "2026-08-21T10:01:00Z");
    let (sources_b, facts_b) = variant(&w, "b", "writer B", "2026-08-21T10:02:00Z");
    let (ready_tx, ready_rx) = mpsc::channel();
    let (fail_tx, fail_rx) = mpsc::channel::<()>();
    let (rollback_tx, rollback_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let reached = Arc::new(AtomicBool::new(false));
    let out = w.out.clone();
    let first = std::thread::spawn(move || {
        contract::set_fault_hook(move |step| {
            let Step::Replace(source, destination) = step else {
                return None;
            };
            if source.ends_with(".tmp") && named(destination) == "register-observation.json" {
                ready_tx.send(()).unwrap();
                fail_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                return Some(simulated("simulated second promotion failure"));
            }
            if source.ends_with(".bak") && named(destination) == "monitor-baseline.json" {
                rollback_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            None
        });
        publish_pair(s(&sources_a), s(&facts_a), s(&out))
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    fail_tx.send(()).unwrap();
    rollback_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let out = w.out.clone();
    let flag = reached.clone();
    let second = std::thread::spawn(move || {
        contract::set_fault_hook(move |step| {
            if matches!(step, Step::Replace(..)) {
                flag.store(true, Ordering::SeqCst);
            }
            None
        });
        publish_pair(s(&sources_b), s(&facts_b), s(&out)).map_err(message)
    });
    std::thread::sleep(Duration::from_millis(250));
    assert!(!reached.load(Ordering::SeqCst));
    resume_tx.send(()).unwrap();
    match first.join().unwrap().unwrap_err() {
        Exc::Os(error) => assert_eq!(error.text, "simulated second promotion failure"),
        other => panic!("{other:?}"),
    }
    second.join().unwrap().unwrap();
    assert_eq!(
        read_json(&w.out.join("monitor-baseline.json"))["corpus"],
        "writer B"
    );
}

#[test]
fn rollback_failure_retains_prior_bytes_and_blocks_publishers() {
    let w = Work::new();
    w.publish().unwrap();
    let prior = w.pair();
    let mut changed = sources_document();
    changed["corpus"] = json!("attempted replacement");
    w.write(&w.sources, &changed);
    contract::set_fault_hook(|step| match step {
        Step::Replace(source, destination)
            if source.ends_with(".tmp") && named(destination) == "register-observation.json" =>
        {
            Some(simulated("simulated observation promotion failure"))
        }
        Step::Replace(source, _) if source.ends_with(".bak") => {
            Some(simulated("simulated rollback restoration failure"))
        }
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    let text = message(outcome.unwrap_err());
    assert!(text.contains("rollback failed"), "{text}");
    let backups: Vec<Vec<u8>> = w
        .names()
        .iter()
        .filter(|n| n.ends_with(".bak"))
        .map(|n| std::fs::read(w.out.join(n)).unwrap())
        .collect();
    assert_eq!(backups.len(), 2);
    assert!(backups.contains(&prior.0) && backups.contains(&prior.1));
    assert!(w.out.join(LOCK).exists());
    contract::set_lock_timeout(Duration::ZERO);
    let blocked = w.publish();
    contract::set_lock_timeout(Duration::from_secs(30));
    assert!(message(blocked.unwrap_err()).contains("locked by another writer"));
    assert_eq!(w.names().iter().filter(|n| n.ends_with(".bak")).count(), 2);
}

#[test]
fn serialises_deterministically_and_restores_both_previous_files_on_publish_failure() {
    let w = Work::new();
    w.publish().unwrap();
    let first = w.pair();
    w.publish().unwrap();
    assert_eq!(w.pair(), first);
    let mut calls = 0;
    contract::set_fault_hook(move |step| {
        if matches!(step, Step::Replace(..)) {
            calls += 1;
            if calls == 4 {
                return Some(simulated("simulated second promotion failure"));
            }
        }
        None
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "simulated second promotion failure"
    );
    assert_eq!(w.pair(), first);
    assert!(w.leftovers().is_empty());
    assert!(!w.out.join(LOCK).exists());
}

#[test]
fn a_first_publication_failure_removes_the_promoted_file() {
    let w = Work::new();
    contract::set_fault_hook(|step| match step {
        Step::Replace(_, destination) if named(destination) == "register-observation.json" => {
            Some(simulated("simulated observation failure"))
        }
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "simulated observation failure"
    );
    assert!(w.names().is_empty());
}

#[test]
fn a_failure_to_park_the_second_file_restores_the_first() {
    let w = Work::new();
    w.publish().unwrap();
    let before = w.pair();
    let mut changed = sources_document();
    changed["retrieved"] = json!("2026-08-22");
    w.write(&w.sources, &changed);
    contract::set_fault_hook(|step| match step {
        Step::Replace(source, _) if named(source) == "register-observation.json" => {
            Some(simulated("simulated parking failure"))
        }
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "simulated parking failure"
    );
    assert_eq!(w.pair(), before);
    assert_eq!(
        w.names(),
        ["monitor-baseline.json", "register-observation.json"]
    );
}

#[test]
fn a_backup_lost_before_rollback_retains_the_lock() {
    let w = Work::new();
    w.publish().unwrap();
    let out = w.out.clone();
    contract::set_fault_hook(move |step| match step {
        Step::Replace(source, destination)
            if source.ends_with(".tmp") && named(destination) == "register-observation.json" =>
        {
            for entry in std::fs::read_dir(&out).unwrap() {
                let name = entry.unwrap().file_name().into_string().unwrap();
                if name.starts_with(".register-observation.json.") && name.ends_with(".bak") {
                    std::fs::remove_file(out.join(name)).unwrap();
                }
            }
            Some(simulated("simulated promotion failure"))
        }
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    let text = message(outcome.unwrap_err());
    assert!(text.contains("rollback failed; retain the lock"), "{text}");
    assert!(w.names().contains(&LOCK.to_string()));
}

#[test]
fn a_lock_token_that_cannot_be_written_releases_the_lock() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    contract::set_fault_hook(|step| match step {
        Step::LockToken(_) => Some(simulated("[Errno 28] no space")),
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(outcome.unwrap_err().to_string(), "[Errno 28] no space");
    assert!(w.names().is_empty());
}

#[test]
fn a_lock_lost_during_a_failure_keeps_the_failure() {
    let w = Work::new();
    let out = w.out.clone();
    contract::set_fault_hook(move |step| match step {
        Step::Replace(..) => {
            std::fs::remove_file(out.join(LOCK)).unwrap();
            Some(simulated("publication failed"))
        }
        _ => None,
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(outcome.unwrap_err().to_string(), "publication failed");
    assert!(w.names().is_empty());
}

#[test]
fn a_lock_lost_after_publication_is_a_contract_error() {
    let w = Work::new();
    let out = w.out.clone();
    let mut calls = 0;
    contract::set_fault_hook(move |step| {
        if matches!(step, Step::Replace(..)) {
            calls += 1;
            if calls == 2 {
                std::fs::remove_file(out.join(LOCK)).unwrap();
            }
        }
        None
    });
    let outcome = w.publish();
    contract::set_fault_hook(|_| None);
    assert_eq!(
        message(outcome.unwrap_err()),
        "monitor output was published, but its owned lock could not be removed"
    );
    assert_eq!(
        w.names(),
        ["monitor-baseline.json", "register-observation.json"]
    );
}

// Ported from PublicationRecoveryTests in tests/corpus/test_output_security.py.

#[test]
fn producer_fractional_timestamp_grammar_is_portable() {
    for length in 1..=6 {
        let mut facts = observation_facts();
        facts["observations"][0]["checked_at"] =
            json!(format!("2099-01-01T12:34:56.{}Z", "1".repeat(length)));
        assert!(observe(&facts).is_ok(), "{length}");
    }
}

#[test]
fn backup_collision_preserves_a_dangling_link() {
    let w = Work::new();
    w.publish().unwrap();
    let prior = w.pair();
    let collision = w
        .out
        .join(".monitor-baseline.json.monitor-contract-fixed.bak");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(w.root.join("missing"), &collision);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(w.root.join("missing"), &collision);
    if let Err(error) = made {
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: symbolic links are unavailable to this account ({error})");
        return;
    }
    tax_radar_au::pyos::set_fixed_hex(Some("fixed"));
    let outcome = w.publish();
    tax_radar_au::pyos::set_fixed_hex(None);
    assert_eq!(
        message(outcome.unwrap_err()),
        "monitor backup staging path already exists."
    );
    assert!(
        collision
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(w.pair(), prior);
    assert!(!w.out.join(LOCK).exists());
    assert!(!w.names().iter().any(|n| n.ends_with(".tmp")));
}

#[test]
fn monitor_staging_collision_preserves_unowned_bytes() {
    let w = Work::new();
    std::fs::create_dir(&w.out).unwrap();
    let collision = w
        .out
        .join(".monitor-baseline.json.monitor-contract-fixed.tmp");
    std::fs::write(&collision, b"unowned").unwrap();
    tax_radar_au::pyos::set_fixed_hex(Some("fixed"));
    let outcome = w.publish();
    tax_radar_au::pyos::set_fixed_hex(None);
    match outcome.unwrap_err() {
        Exc::Os(error) => {
            assert_eq!(error.kind, std::io::ErrorKind::AlreadyExists);
            assert!(
                error.text.starts_with("[Errno 17] File exists: "),
                "{}",
                error.text
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&collision).unwrap(), b"unowned");
}

/// An interruption after a rename has completed, at each of the 4 renames,
/// still restores the previous pair.
#[test]
fn monitor_interruptions_after_each_rename_restore_the_old_pair() {
    for interrupt_at in 1..=4 {
        let w = Work::new();
        w.publish().unwrap();
        let prior = w.pair();
        let mut changed = sources_document();
        changed["corpus"] = json!("replacement");
        w.write(&w.sources, &changed);
        let mut calls = 0;
        contract::set_fault_hook(move |step| {
            let Step::Replace(source, destination) = step else {
                return None;
            };
            calls += 1;
            if calls != interrupt_at {
                return None;
            }
            std::fs::rename(source, destination).unwrap();
            Some(simulated("interrupted after a completed rename"))
        });
        let outcome = w.publish();
        contract::set_fault_hook(|_| None);
        assert_eq!(
            outcome.unwrap_err().to_string(),
            "interrupted after a completed rename"
        );
        assert_eq!(w.pair(), prior, "rename {interrupt_at}");
        assert_eq!(
            w.names(),
            ["monitor-baseline.json", "register-observation.json"],
            "rename {interrupt_at}"
        );
    }
}
