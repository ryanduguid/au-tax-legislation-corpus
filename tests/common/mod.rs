//! Fixtures shared by the radar test binaries: the shipped samples, JSON
//! helpers and the queue and decision builders the Python suite used.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub use serde_json::{Value, json};
use tax_radar_au::pyjson;
use tax_radar_au::pypath::PyPath;
use tax_radar_au::radar::{self, monitor, util};

pub fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples")
}

pub fn sample(kind: &str, name: &str) -> PathBuf {
    samples().join(kind).join(name)
}

pub fn py(path: &Path) -> PyPath {
    PyPath::new(&path.to_string_lossy())
}

pub fn payload(kind: &str, name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(sample(kind, name)).unwrap()).unwrap()
}

pub fn load(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// `json.dumps(payload)` written as UTF-8.
pub fn write(path: &Path, payload: &Value) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_string(payload).unwrap()).unwrap();
    path.to_path_buf()
}

/// A radar value as plain JSON, for assertions and mutation.
pub fn value(json: &pyjson::Json) -> Value {
    serde_json::from_str(&pyjson::dumps(json, pyjson::Layout::Canonical)).unwrap()
}

pub fn radar_json(value: &Value) -> pyjson::Json {
    pyjson::loads(&serde_json::to_string(value).unwrap()).unwrap()
}

pub fn sha256_json(value: &Value) -> String {
    util::sha256_json(&radar_json(value))
}

pub fn message<T: std::fmt::Debug>(result: radar::Result<T>) -> String {
    result.expect_err("expected a refusal").to_string()
}

/// Assert that a refusal's message matches a regular expression.
pub fn refused<T: std::fmt::Debug>(result: radar::Result<T>, pattern: &str) -> String {
    let text = message(result);
    let re = regex::Regex::new(pattern).unwrap();
    assert!(re.is_match(&text), "{text:?} does not match {pattern:?}");
    text
}

pub fn compare_paths(baseline: &Path, observation: &Path, mapping: &Path) -> radar::Result<Value> {
    monitor::compare(&py(baseline), &py(observation), &py(mapping)).map(|q| value(&q))
}

/// `compare()` over the shipped samples.
pub fn queue() -> Value {
    compare_paths(
        &sample("baseline", "sample-sources.json"),
        &sample("observations", "sample-register-observation.json"),
        &sample("mappings", "sample-source-skill-map.json"),
    )
    .unwrap()
}

pub fn expected_queue_digest(queue: &Value) -> String {
    let mut payload = queue.clone();
    payload.as_object_mut().unwrap().remove("queue_digest");
    format!("sha256:{}", sha256_json(&payload))
}

pub fn matching_decision(queue: &Value) -> Value {
    let open = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["state"] == "OPEN")
        .unwrap();
    json!({
        "schema_version": "au-tax-technical-review.v2",
        "run_id": queue["run_id"],
        "queue_digest": queue["queue_digest"],
        "reviewer_ref": "demo-tax-reviewer",
        "reviewed_at": "2026-08-09T00:00:00Z",
        "decisions": [{
            "item_id": open["item_id"],
            "decision": "ESCALATE_TECHNICAL_REVIEW",
            "rationale": "Synthetic fixture requires technical-tax assessment.",
            "evidence_note": "Reviewed the fabricated source metadata and fixture link.",
        }],
    })
}

pub fn matching_decision_for_all_open_items(queue: &Value) -> Value {
    let mut decision = matching_decision(queue);
    let template = decision["decisions"][0].clone();
    let entries: Vec<Value> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["state"] == "OPEN")
        .map(|item| {
            let mut entry = template.clone();
            entry["item_id"] = item["item_id"].clone();
            entry
        })
        .collect();
    assert!(!entries.is_empty());
    decision["decisions"] = Value::Array(entries);
    decision
}

pub fn validate_texts(dir: &Path, queue: &str, decision: &str) -> radar::Result<Value> {
    let queue_path = dir.join("impact-queue.json");
    let decision_path = dir.join("technical-review.json");
    std::fs::write(&queue_path, queue).unwrap();
    std::fs::write(&decision_path, decision).unwrap();
    monitor::validate_review(&py(&queue_path), &py(&decision_path)).map(|v| value(&v))
}

pub fn validate_payloads(dir: &Path, queue: &Value, decision: &Value) -> radar::Result<Value> {
    validate_texts(dir, &queue.to_string(), &decision.to_string())
}

/// `compare()` over the samples with any of the three documents replaced.
pub fn compare_fixtures(
    dir: &Path,
    baseline: Option<&Value>,
    observation: Option<&Value>,
    mapping: Option<&Value>,
) -> radar::Result<Value> {
    let docs = [
        ("baseline", baseline, ("baseline", "sample-sources.json")),
        (
            "observation",
            observation,
            ("observations", "sample-register-observation.json"),
        ),
        (
            "mapping",
            mapping,
            ("mappings", "sample-source-skill-map.json"),
        ),
    ];
    let mut paths = Vec::new();
    for (name, replacement, (kind, file)) in docs {
        let doc = replacement.cloned().unwrap_or_else(|| payload(kind, file));
        paths.push(write(&dir.join(format!("{name}.json")), &doc));
    }
    compare_paths(&paths[0], &paths[1], &paths[2])
}

pub fn only<'a>(queue: &'a Value, change_kind: &str) -> &'a Value {
    let matches: Vec<&Value> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["change_kind"] == change_kind)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one {change_kind} item in {queue}"
    );
    matches[0]
}

pub fn clear_compilation(entry: &mut Value) {
    for field in [
        "observed_compilation_number",
        "observed_compilation_date",
        "observed_register_document_id",
    ] {
        entry[field] = Value::Null;
    }
}

pub fn v3_observation() -> Value {
    let mut observation = payload("observations", "sample-register-observation-v2.json");
    observation["schema_version"] = json!("au-tax-register-observation.v3");
    observation["scope_id"] = json!("au-primary-tax-legislation.v3");
    for item in observation["observations"].as_array_mut().unwrap() {
        item["content_sha256"] = json!(format!("sha256:{}", "a".repeat(64)));
        item["content_kind"] = json!("metadata-response");
        item["content_media_type"] = json!("application/json");
    }
    observation
}

/// Write a queue pair with `write_queue` and return its JSON path.
pub fn write_queue(queue: &Value, dir: &Path) -> PathBuf {
    let (json, _) = monitor::write_queue(&radar_json(queue), &py(dir)).unwrap();
    json.fs()
}

/// Run the built binary.
pub fn cli(args: &[&str], cwd: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tax-radar-au"));
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    command.output().unwrap()
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

pub fn compare_argv(out: &Path) -> Vec<String> {
    vec![
        "compare".into(),
        "--baseline".into(),
        sample("baseline", "sample-sources.json")
            .to_string_lossy()
            .into(),
        "--observation".into(),
        sample("observations", "sample-register-observation.json")
            .to_string_lossy()
            .into(),
        "--map".into(),
        sample("mappings", "sample-source-skill-map.json")
            .to_string_lossy()
            .into(),
        "--out".into(),
        out.to_string_lossy().into(),
    ]
}

pub fn run(args: &[String], cwd: Option<&Path>) -> Output {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    cli(&refs, cwd)
}

/// The sorted names in a directory.
pub fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

pub fn partials(dir: &Path) -> Vec<String> {
    names(dir)
        .into_iter()
        .filter(|n| n.ends_with(".partial"))
        .collect()
}
