//! `fadden export_live_evidence_bundles`: ported from
//! tests/corpus/test_live_evidence_bundle_export.py and the refusal paths of
//! test_live_evidence_paths.py and test_live_evidence_publication_paths.py.
//! Publication is Windows only, so tests that publish run on Windows; the
//! snapshot, the graph proof and the candidate checks run everywhere. The
//! Python stage remains what the publication workflow runs, so its own tests
//! stay too.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::live_evidence::{
    self as live, LiveEvidenceExport, Step, export_live_evidence_bundles, live_message,
};
use tax_radar_au::fadden::{Exc, Run};
use tax_radar_au::pyjson;
use tax_radar_au::pyos::OsError;

const DEFAULT_RESPONSE: &[u8] = b"{\"@odata.context\":\"https://api.prod.legislation.gov.au/v1/\
$metadata#Versions(titleId,start,compilationNumber,registerId,\
isCurrent,status,registeredAt)\",\"value\":[{\"titleId\":\"F2022L00347\",\
\"start\":\"2026-08-18T00:00:00\",\"compilationNumber\":\"20\",\
\"registerId\":\"F2026C00838\",\"isCurrent\":true,\"status\":\"InForce\",\
\"registeredAt\":\"2026-08-27T17:31:41.1234567+10:00\"}]}";
#[cfg(windows)]
const BUNDLE: &str = "bundle-frl-f2022l00347-f2026c00838-r1.json";

/// A change to a capture's baseline, result and observation before they are written.
type Mutation = Box<dyn FnOnce(&mut Value, &mut Value, &mut Value)>;
/// A change to a capture directory.
type GraphChange = fn(&Path);
/// A fault hook.
type Hook = Box<dyn FnMut(Step) -> Option<OsError>>;

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

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn message(error: Exc) -> String {
    match live_message(&error) {
        Some(text) => text.to_string(),
        None => panic!("not a LiveEvidenceBundleError: {error:?}"),
    }
}

/// `LiveEvidenceBundleContractTests._write_capture`: one superseded title.
fn write_capture(
    root: &Path,
    response: Option<&[u8]>,
    mutate: Option<Mutation>,
) -> (PathBuf, String) {
    let capture = root.join("capture");
    std::fs::create_dir_all(capture.join("evidence")).unwrap();
    let register_id = "F2022L00347";
    let response = response.unwrap_or(DEFAULT_RESPONSE);
    let response_sha256 = sha256_id(response);
    let evidence_path = format!("evidence/sha256-{}.json", &response_sha256[7..]);
    std::fs::write(capture.join(&evidence_path), response).unwrap();
    let mut baseline = json!({
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": "2026-08-05",
        "source": "Federal Register of Legislation",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "titles": [{
            "register_id": register_id,
            "name": "Taxation Administration Regulations 2017",
            "collection": "LegislativeInstrument",
            "compilation_number": "19",
            "compilation_date": "2026-08-05",
            "version_is_current": true,
            "current_version_start": null,
            "retrieved": "2026-08-05",
            "source_url": "https://www.legislation.gov.au/F2022L00347/2026-08-05/2026-08-05/text/original/epub",
            "register_page": "https://www.legislation.gov.au/F2022L00347/latest/text"
        }]
    });
    let current_url = "https://api.prod.legislation.gov.au/v1/versions?%24top=1&\
        %24filter=titleId%20eq%20%27F2022L00347%27%20and%20isCurrent%20eq%20true&\
        %24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt";
    let mut result = json!({
        "register_id": register_id,
        "collection": "LegislativeInstrument",
        "checked_at": "2026-08-28T00:00:00Z",
        "state": "SUPERSEDED",
        "error_category": null,
        "requests": [{
            "role": "current",
            "url": current_url,
            "checked_at": "2026-08-28T00:00:00Z",
            "http_status": 200,
            "transport_error_category": null,
            "attempt_count": 1,
            "response_headers": {"content-type": "application/json", "odata-version": "4.0"},
            "response_length": response.len(),
            "response_sha256": response_sha256,
            "evidence_path": evidence_path
        }]
    });
    let result_sha256 = sha256_id(&json_bytes(&result));
    let mut observation = json!({
        "schema_version": "au-tax-register-observation.v4",
        "mode": "live",
        "observed_at": "2026-08-28T00:00:00Z",
        "scope_id": "au-primary-tax-legislation.v4",
        "baseline_sha256": "",
        "capture_sha256": "",
        "expected_register_ids": [register_id],
        "complete": true,
        "run_status": "VERIFIED",
        "observations": [{
            "register_id": register_id,
            "collection": "LegislativeInstrument",
            "state": "SUPERSEDED",
            "evidence_id": format!("frl:{register_id}:{}", &result_sha256[7..39]),
            "observed_compilation_number": "20",
            "observed_compilation_date": "2026-08-18",
            "observed_register_document_id": "F2026C00838",
            "current_version_start": null,
            "evidence_url": "https://www.legislation.gov.au/F2022L00347/latest/text",
            "checked_at": "2026-08-28T00:00:00Z",
            "error_category": null,
            "capture_result_sha256": result_sha256,
            "primary_response_sha256": response_sha256,
            "primary_response_media_type": "application/json"
        }]
    });
    if let Some(mutate) = mutate {
        mutate(&mut baseline, &mut result, &mut observation);
    }
    let baseline_bytes = json_bytes(&baseline);
    std::fs::write(capture.join("monitor-baseline.json"), &baseline_bytes).unwrap();
    observation["baseline_sha256"] = json!(sha256_id(&baseline_bytes));
    let capture_document = json!({
        "schema_version": "au-tax-register-capture.v1",
        "mode": "live",
        "observed_at": "2026-08-28T00:00:00Z",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "baseline_sha256": sha256_id(&baseline_bytes),
        "expected_register_ids": [register_id],
        "complete": true,
        "results": [result]
    });
    let capture_bytes = json_bytes(&capture_document);
    std::fs::write(capture.join("register-capture.json"), &capture_bytes).unwrap();
    observation["capture_sha256"] = json!(sha256_id(&capture_bytes));
    let observation_bytes = json_bytes(&observation);
    std::fs::write(
        capture.join("register-observation.json"),
        &observation_bytes,
    )
    .unwrap();
    (capture, sha256_id(&observation_bytes))
}

fn make_capture(root: &Path) -> PathBuf {
    write_capture(root, None, None).0
}

fn documents(capture: &Path) -> (Value, Value, Value) {
    let read =
        |name: &str| serde_json::from_slice(&std::fs::read(capture.join(name)).unwrap()).unwrap();
    (
        read("monitor-baseline.json"),
        read("register-capture.json"),
        read("register-observation.json"),
    )
}

/// `_write_documents`: rewrite the graph with every digest recomputed.
fn write_documents(
    capture: &Path,
    baseline: &Value,
    register: &mut Value,
    observation: &mut Value,
) {
    let baseline_bytes = json_bytes(baseline);
    register["baseline_sha256"] = json!(sha256_id(&baseline_bytes));
    let register_bytes = json_bytes(register);
    observation["baseline_sha256"] = json!(sha256_id(&baseline_bytes));
    observation["capture_sha256"] = json!(sha256_id(&register_bytes));
    let results = register["results"].as_array().unwrap().clone();
    for (result, item) in results
        .iter()
        .zip(observation["observations"].as_array_mut().unwrap())
    {
        let digest = sha256_id(&json_bytes(result));
        item["capture_result_sha256"] = json!(digest);
        item["evidence_id"] = json!(format!(
            "frl:{}:{}",
            item["register_id"].as_str().unwrap(),
            &digest[7..39]
        ));
        item["primary_response_sha256"] = result["requests"][0]["response_sha256"].clone();
        let content_type = result["requests"][0]["response_headers"]["content-type"].as_str();
        item["primary_response_media_type"] = content_type.map_or(Value::Null, |c| {
            json!(c.split(';').next().unwrap().trim().to_lowercase())
        });
    }
    std::fs::write(capture.join("monitor-baseline.json"), baseline_bytes).unwrap();
    std::fs::write(capture.join("register-capture.json"), register_bytes).unwrap();
    std::fs::write(
        capture.join("register-observation.json"),
        json_bytes(observation),
    )
    .unwrap();
}

#[cfg(windows)]
fn set_non_candidate_state(capture: &Path, state: &str) {
    let (baseline, mut register, mut observation) = documents(capture);
    register["results"][0]["state"] = json!(state);
    register["results"][0]["error_category"] = Value::Null;
    let item = &mut observation["observations"][0];
    item["state"] = json!(state);
    item["error_category"] = Value::Null;
    for field in [
        "observed_compilation_number",
        "observed_compilation_date",
        "observed_register_document_id",
    ] {
        item[field] = Value::Null;
    }
    item["current_version_start"] = if state == "CURRENT_NO_PUBLISHED_COMPILATION" {
        json!("2026-08-18")
    } else {
        Value::Null
    };
    write_documents(capture, &baseline, &mut register, &mut observation);
}

/// Replace bytes of the retained response and redeclare it everywhere.
fn replace_raw(capture: &Path, from: &str, to: &str) {
    let (baseline, mut register, mut observation) = documents(capture);
    let request = &mut register["results"][0]["requests"][0];
    let evidence = capture.join(request["evidence_path"].as_str().unwrap());
    let raw = String::from_utf8(std::fs::read(&evidence).unwrap())
        .unwrap()
        .replacen(from, to, 1)
        .into_bytes();
    std::fs::remove_file(&evidence).unwrap();
    let digest = sha256_id(&raw);
    let name = format!("sha256-{}.json", &digest[7..]);
    std::fs::write(capture.join("evidence").join(&name), &raw).unwrap();
    request["response_sha256"] = json!(digest);
    request["response_length"] = json!(raw.len());
    request["evidence_path"] = json!(format!("evidence/{name}"));
    write_documents(capture, &baseline, &mut register, &mut observation);
}

fn append_valid_superseded_candidate(capture: &Path) {
    let (mut baseline, mut register, mut observation) = documents(capture);
    let (register_id, document_id) = ("F2022L00348", "F2026C00839");
    let raw = String::from_utf8(DEFAULT_RESPONSE.to_vec())
        .unwrap()
        .replace("F2022L00347", register_id)
        .replace("F2026C00838", document_id)
        .into_bytes();
    let digest = sha256_id(&raw);
    let name = format!("sha256-{}.json", &digest[7..]);
    std::fs::write(capture.join("evidence").join(&name), &raw).unwrap();
    let swap = |v: &Value| json!(v.as_str().unwrap().replace("F2022L00347", register_id));
    let mut title = baseline["titles"][0].clone();
    title["register_id"] = json!(register_id);
    title["source_url"] = swap(&title["source_url"]);
    title["register_page"] = swap(&title["register_page"]);
    let mut result = register["results"][0].clone();
    result["register_id"] = json!(register_id);
    let request = &mut result["requests"][0];
    request["url"] = swap(&request["url"]);
    request["response_length"] = json!(raw.len());
    request["response_sha256"] = json!(digest);
    request["evidence_path"] = json!(format!("evidence/{name}"));
    let mut item = observation["observations"][0].clone();
    item["register_id"] = json!(register_id);
    item["observed_register_document_id"] = json!(document_id);
    item["evidence_url"] = swap(&item["evidence_url"]);
    baseline["titles"].as_array_mut().unwrap().push(title);
    register["expected_register_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!(register_id));
    register["results"].as_array_mut().unwrap().push(result);
    observation["expected_register_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!(register_id));
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .push(item);
    write_documents(capture, &baseline, &mut register, &mut observation);
}

fn export(capture: &Path, output: &Path) -> Result<LiveEvidenceExport, Exc> {
    export_live_evidence_bundles(s(capture), s(output))
}

fn with_hooks<T>(hook: Hook, body: impl FnOnce() -> T) -> T {
    live::set_fault_hook(hook);
    let outcome = body();
    live::clear_hooks();
    outcome
}

fn simulated(text: &str) -> OsError {
    OsError {
        kind: std::io::ErrorKind::Other,
        text: text.into(),
        strerror: text.into(),
    }
}

#[cfg(windows)]
fn owned_staging(parent: &Path, output: &Path) -> Vec<PathBuf> {
    let prefix = format!(
        ".{}.live-evidence-",
        output.file_name().unwrap().to_str().unwrap()
    );
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&prefix)
        })
        .collect();
    found.sort();
    found
}

/// Make a directory link, or return false where this Windows account lacks
/// the privilege, so the test is skipped.
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<bool> {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(target, link);
    match made {
        Ok(()) => Ok(true),
        Err(error) if cfg!(windows) && error.raw_os_error() == Some(1314) => {
            eprintln!("skipped: symbolic links are unavailable to this account ({error})");
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

// ----------------------------------------------------------- the contract

#[cfg(windows)]
#[test]
fn exports_the_live_only_contract_and_reviewed_golden_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, observation_digest) = write_capture(dir.path(), None, None);
    let output = dir.path().join("output");
    let result = export(&capture, &output).unwrap();
    assert_eq!(
        result.release_tag,
        format!("live-evidence-v2-{}", &observation_digest[7..])
    );
    assert_eq!(result.candidates, [s(&output.join(BUNDLE)).to_string()]);
    let actual = std::fs::read(&result.candidates[0]).unwrap();
    let bundle: Value = serde_json::from_slice(&actual).unwrap();
    assert_eq!(
        bundle["rights"],
        json!({
            "mode": "metadata-only",
            "attribution": "Based on content from the Federal Register of Legislation at 2026-08-28. For the latest information on Australian Government legislation please go to https://www.legislation.gov.au. Changes: selected and reformatted Federal Register metadata into a bounded evidence bundle and factual source update; no legislation text is reproduced.",
            "licence_url": "https://creativecommons.org/licenses/by/4.0/"
        })
    );
    let golden = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/corpus/fixtures/live-evidence/evidence-bundle.v2.json"),
    )
    .unwrap();
    assert_eq!(actual, golden);
    assert!(!actual.contains(&b'\r'));
    assert!(actual.ends_with(b"}\n"));
    let keys: Vec<&str> = bundle
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "schema_version",
            "producer",
            "run",
            "baseline_title",
            "capture_result",
            "observation",
            "rights",
            "primary_response_base64"
        ]
    );
}

#[cfg(windows)]
#[test]
fn accepts_a_raw_response_at_the_256_kib_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut response = DEFAULT_RESPONSE.to_vec();
    response.resize(256 * 1024, b' ');
    let (capture, _) = write_capture(dir.path(), Some(&response), None);
    assert_eq!(
        export(&capture, &dir.path().join("output"))
            .unwrap()
            .candidates
            .len(),
        1
    );
}

#[test]
fn rejects_a_raw_response_over_the_256_kib_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut response = DEFAULT_RESPONSE.to_vec();
    response.resize(256 * 1024 + 1, b' ');
    let (capture, _) = write_capture(dir.path(), Some(&response), None);
    assert_eq!(
        message(export(&capture, &dir.path().join("output")).unwrap_err()),
        "capture graph is not a verified Stage 3A graph."
    );
}

#[test]
fn rejects_a_serialised_bundle_over_the_1_mib_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = write_capture(
        dir.path(),
        None,
        Some(Box::new(|baseline, _, _| {
            baseline["titles"][0]["name"] = json!("x".repeat(1024 * 1024))
        })),
    );
    assert_eq!(
        message(export(&capture, &dir.path().join("output")).unwrap_err()),
        "serialised bundle exceeds the 1 MiB limit."
    );
}

#[test]
fn a_complete_graph_refuses_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = write_capture(
        dir.path(),
        None,
        Some(Box::new(|baseline, result, observation| {
            baseline["titles"][0]["unexpected_title"] = json!("must not be emitted");
            result["unexpected_result"] = json!("must not be emitted");
            result["requests"][0]["unexpected_request"] = json!("must not be emitted");
            observation["observations"][0]["unexpected_observation"] = json!("must not be emitted");
        })),
    );
    assert_eq!(
        message(export(&capture, &dir.path().join("output")).unwrap_err()),
        "capture graph is not a verified Stage 3A graph."
    );
}

#[test]
fn version_timestamps_accept_seven_fraction_digits_only() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    replace_raw(
        &capture,
        "\"registeredAt\":\"2026-08-27T17:31:41.1234567+10:00\"",
        "\"registeredAt\":\"2026-08-27T17:31:41.12345678+10:00\"",
    );
    assert_eq!(
        message(export(&capture, &dir.path().join("output")).unwrap_err()),
        "raw current registration timestamp is invalid."
    );
}

// --------------------------------------------------------------- the graph

#[test]
fn complete_graph_failures_are_table_driven() {
    let cases: Vec<(GraphChange, &str)> = vec![
        (
            |capture| {
                let (_, mut register, mut observation) = documents(capture);
                register["complete"] = json!(false);
                observation["complete"] = json!(false);
                std::fs::write(capture.join("register-capture.json"), json_bytes(&register))
                    .unwrap();
                std::fs::write(
                    capture.join("register-observation.json"),
                    json_bytes(&observation),
                )
                .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (_, _, mut observation) = documents(capture);
                observation["run_status"] = json!("BLOCKED");
                std::fs::write(
                    capture.join("register-observation.json"),
                    json_bytes(&observation),
                )
                .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (baseline, mut register, mut observation) = documents(capture);
                register["results"][0]["state"] = json!("LOOKUP_FAILED");
                register["results"][0]["error_category"] = json!("INVALID_JSON");
                let item = &mut observation["observations"][0];
                item["state"] = json!("LOOKUP_FAILED");
                item["error_category"] = json!("INVALID_JSON");
                for field in [
                    "observed_compilation_number",
                    "observed_compilation_date",
                    "observed_register_document_id",
                    "current_version_start",
                ] {
                    item[field] = Value::Null;
                }
                observation["run_status"] = json!("BLOCKED");
                write_documents(capture, &baseline, &mut register, &mut observation);
            },
            "capture must be a verified live Stage 3A graph.",
        ),
        (
            |capture| {
                let (mut baseline, _, _) = documents(capture);
                baseline["titles"] = json!([]);
                std::fs::write(capture.join("monitor-baseline.json"), json_bytes(&baseline))
                    .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (mut baseline, _, _) = documents(capture);
                let first = baseline["titles"][0].clone();
                baseline["titles"].as_array_mut().unwrap().push(first);
                std::fs::write(capture.join("monitor-baseline.json"), json_bytes(&baseline))
                    .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (_, mut register, _) = documents(capture);
                register["baseline_sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
                std::fs::write(capture.join("register-capture.json"), json_bytes(&register))
                    .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (_, mut register, _) = documents(capture);
                let length = register["results"][0]["requests"][0]["response_length"]
                    .as_u64()
                    .unwrap();
                register["results"][0]["requests"][0]["response_length"] = json!(length + 1);
                std::fs::write(capture.join("register-capture.json"), json_bytes(&register))
                    .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (_, _, mut observation) = documents(capture);
                observation["observations"][0]["capture_result_sha256"] =
                    json!(format!("sha256:{}", "0".repeat(64)));
                std::fs::write(
                    capture.join("register-observation.json"),
                    json_bytes(&observation),
                )
                .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                let (_, mut register, _) = documents(capture);
                register["results"][0]["requests"][0]["evidence_path"] = json!("../outside.json");
                std::fs::write(capture.join("register-capture.json"), json_bytes(&register))
                    .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| {
                std::fs::write(
                    capture
                        .join("evidence")
                        .join(format!("sha256-{}.json", "0".repeat(64))),
                    b"extra",
                )
                .unwrap();
            },
            "capture graph is not a verified Stage 3A graph.",
        ),
        (
            |capture| std::fs::write(capture.join("evidence").join("notes.txt"), b"extra").unwrap(),
            "capture evidence layout is invalid.",
        ),
        (
            |capture| std::fs::write(capture.join("extra.json"), b"{}").unwrap(),
            "capture input layout is invalid.",
        ),
    ];
    for (mutate, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        mutate(&capture);
        let output = dir.path().join("output");
        assert_eq!(message(export(&capture, &output).unwrap_err()), expected);
        assert!(!output.exists());
    }
}

#[test]
fn capture_directory_swap_never_exports_an_external_graph() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let capture = make_capture(&root);
    let (external, _) = write_capture(
        &root.join("external"),
        None,
        Some(Box::new(|baseline, _, _| {
            baseline["titles"][0]["name"] = json!("External capture bytes")
        })),
    );
    let retained = root.join("retained-capture");
    let mut swapped = false;
    let (from, link) = (capture.clone(), capture.clone());
    let outcome = with_hooks(
        Box::new(move |step| {
            if matches!(step, Step::Copy(_)) && !swapped {
                swapped = true;
                if std::fs::rename(&from, &retained).is_err() {
                    return Some(simulated("directory replacement was blocked"));
                }
                if !symlink_dir(&external, &link).unwrap_or(false) {
                    return Some(simulated("directory replacement was blocked"));
                }
            }
            None
        }),
        || export(&capture, &root.join("output")),
    );
    match outcome {
        Err(error) => {
            assert!(live_message(&error).is_some(), "{error:?}");
            assert!(!root.join("output").exists());
        }
        Ok(result) => {
            let bundle: Value =
                serde_json::from_slice(&std::fs::read(&result.candidates[0]).unwrap()).unwrap();
            assert_ne!(bundle["baseline_title"]["name"], "External capture bytes");
        }
    }
}

#[test]
fn evidence_directory_swap_never_copies_external_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let capture = make_capture(&root);
    let outside = root.join("outside-evidence");
    std::fs::create_dir(&outside).unwrap();
    let name = std::fs::read_dir(capture.join("evidence"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    std::fs::write(outside.join(&name), b"external").unwrap();
    let retained = root.join("retained-evidence");
    let evidence = capture.join("evidence");
    let mut swapped = false;
    let outcome = with_hooks(
        Box::new(move |step| {
            if let Step::Copy(path) = step
                && Path::new(path).parent() == Some(evidence.as_path())
                && !swapped
            {
                swapped = true;
                if std::fs::rename(&evidence, &retained).is_err()
                    || !symlink_dir(&outside, &evidence).unwrap_or(false)
                {
                    return Some(simulated("directory replacement was blocked"));
                }
            }
            None
        }),
        || export(&capture, &root.join("output")),
    );
    // Off Windows the held descriptor still reads the original directory, and
    // the final identity check sees the swap.
    match outcome {
        Err(error) => {
            assert!(live_message(&error).is_some(), "{error:?}");
            assert!(!root.join("output").exists());
        }
        Ok(result) => {
            let bundle: Value =
                serde_json::from_slice(&std::fs::read(&result.candidates[0]).unwrap()).unwrap();
            assert!(!bundle.to_string().contains("ZXh0ZXJuYWw"));
        }
    }
}

#[test]
fn one_inconsistent_superseded_candidate_blocks_all_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    append_valid_superseded_candidate(&capture);
    let (baseline, mut register, mut observation) = documents(&capture);
    let request = &mut register["results"][1]["requests"][0];
    let evidence = capture.join(request["evidence_path"].as_str().unwrap());
    let raw = String::from_utf8(std::fs::read(&evidence).unwrap())
        .unwrap()
        .replace(
            "\"registeredAt\":\"2026-08-27T17:31:41.1234567+10:00\"",
            "\"registeredAt\":\"2026-08-27\"",
        )
        .into_bytes();
    std::fs::remove_file(&evidence).unwrap();
    let digest = sha256_id(&raw);
    let name = format!("sha256-{}.json", &digest[7..]);
    std::fs::write(capture.join("evidence").join(&name), &raw).unwrap();
    request["response_length"] = json!(raw.len());
    request["response_sha256"] = json!(digest);
    request["evidence_path"] = json!(format!("evidence/{name}"));
    write_documents(&capture, &baseline, &mut register, &mut observation);
    let output = dir.path().join("output");
    assert_eq!(
        message(export(&capture, &output).unwrap_err()),
        "raw current registration timestamp is invalid."
    );
    assert!(!output.exists());
}

#[test]
fn raw_row_relationships_are_table_driven() {
    for (from, to, expected) in [
        (
            "\"titleId\":\"F2022L00347\"",
            "\"titleId\":\"F2022L00348\"",
            "retained response is not the declared current row.",
        ),
        (
            "\"start\":\"2026-08-18",
            "\"start\":\"2026-08-19",
            "retained response does not prove the observed compilation.",
        ),
        (
            "\"compilationNumber\":\"20\"",
            "\"compilationNumber\":\"\"",
            "raw current compilation number must be non-empty text.",
        ),
        (
            "\"registerId\":\"F2026C00838\"",
            "\"registerId\":\"F2026C00839\"",
            "retained response does not prove the observed compilation.",
        ),
        (
            "\"start\":\"2026-08-18T00:00:00\",",
            "\"start\":\"2026-08-18T00:00:00\",\"start\":\"2026-08-18T00:00:00\",",
            "retained response is not unambiguous OData JSON.",
        ),
        (
            "\"registeredAt\":\"2026-08-27T17:31:41.1234567+10:00\"",
            "\"registeredAt\":\"2026-08-27\"",
            "raw current registration timestamp is invalid.",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        replace_raw(&capture, from, to);
        assert_eq!(
            message(export(&capture, &dir.path().join("output")).unwrap_err()),
            expected,
            "{to}"
        );
    }
}

#[cfg(windows)]
#[test]
fn non_candidate_states_create_no_asset() {
    for state in [
        "UNCHANGED",
        "CURRENT_NO_PUBLISHED_COMPILATION",
        "NO_LONGER_IN_FORCE",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        set_non_candidate_state(&capture, state);
        let output = dir.path().join("output");
        let result = export(&capture, &output).unwrap();
        assert!(result.candidates.is_empty());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
    }
}

#[cfg(windows)]
#[test]
fn registration_is_independent_from_compilation_start_and_previous_number_may_be_null() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    let (mut baseline, mut register, mut observation) = documents(&capture);
    baseline["titles"][0]["compilation_number"] = Value::Null;
    write_documents(&capture, &baseline, &mut register, &mut observation);
    let result = export(&capture, &dir.path().join("output")).unwrap();
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(&result.candidates[0]).unwrap()).unwrap();
    assert_eq!(
        bundle["observation"]["observed_compilation_date"],
        "2026-08-18"
    );
    assert_eq!(bundle["baseline_title"]["compilation_number"], Value::Null);
}

// ------------------------------------------------------------ filesystem

#[cfg(windows)]
#[test]
fn absent_output_promotes_one_private_directory() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("new-parent").join("output");
    let result = export(&make_capture(dir.path()), &output).unwrap();
    assert_eq!(result.candidates, [s(&output.join(BUNDLE)).to_string()]);
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
    assert!(owned_staging(output.parent().unwrap(), &output).is_empty());
    for path in [output.parent().unwrap().to_path_buf(), output.clone()] {
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

#[test]
fn only_one_safe_missing_output_parent_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("first").join("second").join("output");
    assert_eq!(
        message(export(&make_capture(dir.path()), &output).unwrap_err()),
        "live evidence permits only one missing output parent."
    );
    assert!(!dir.path().join("first").exists());
}

#[test]
fn output_may_not_collide_with_capture_input() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    let before = std::fs::read(capture.join("monitor-baseline.json")).unwrap();
    for output in [
        capture.clone(),
        capture.join("inner"),
        capture.join("evidence"),
    ] {
        assert_eq!(
            message(export(&capture, &output).unwrap_err()),
            "live evidence output would replace a capture input."
        );
    }
    assert_eq!(
        std::fs::read(capture.join("monitor-baseline.json")).unwrap(),
        before
    );
}

#[test]
fn existing_output_and_linked_ancestor_fail_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let capture = make_capture(root);
    let existing = root.join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("keep"), "preserve").unwrap();
    assert_eq!(
        message(export(&capture, &existing).unwrap_err()),
        "live evidence output directory must not already exist."
    );
    assert_eq!(
        std::fs::read_to_string(existing.join("keep")).unwrap(),
        "preserve"
    );
    let linked = root.join("linked");
    if !symlink_dir(root, &linked).unwrap() {
        return;
    }
    assert_eq!(
        message(export(&capture, &linked.join("output")).unwrap_err()),
        "live evidence output parent must be an ordinary directory."
    );
    assert!(!root.join("output").exists());
}

#[test]
fn non_windows_rejects_before_creating_output_parent_or_staging() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    let output = dir.path().join("new-parent").join("output");
    live::set_not_windows(true);
    let outcome = export(&capture, &output);
    live::clear_hooks();
    assert_eq!(
        message(outcome.unwrap_err()),
        "identity-bound live evidence publication is supported only on Windows."
    );
    assert!(!output.parent().unwrap().exists());
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["capture"]);
}

#[cfg(windows)]
#[test]
fn write_failure_removes_only_owned_staging_and_preserves_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let output = root.join("output");
    let neighbour = root.join(".output.not-ours");
    std::fs::create_dir(&neighbour).unwrap();
    let capture = make_capture(root);
    let outcome = with_hooks(
        Box::new(|step| matches!(step, Step::Write(_)).then(|| simulated("disk full"))),
        || export(&capture, &output),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "live evidence output could not be written."
    );
    assert!(!output.exists());
    assert!(neighbour.is_dir());
    assert!(owned_staging(root, &output).is_empty());
}

#[cfg(windows)]
#[test]
fn reread_failure_never_promotes_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let output = root.join("output");
    let capture = make_capture(root);
    let outcome = with_hooks(
        Box::new(|step| {
            matches!(step, Step::Reread(_)).then(|| simulated("staged output changed"))
        }),
        || export(&capture, &output),
    );
    assert_eq!(message(outcome.unwrap_err()), "staged output changed");
    assert!(!output.exists());
    assert!(owned_staging(root, &output).is_empty());
}

#[cfg(windows)]
#[test]
fn promotion_race_cannot_overwrite_a_new_destination() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let output = root.join("output");
    let capture = make_capture(&root);
    let racer = output.clone();
    let outcome = with_hooks(
        Box::new(move |step| {
            if let Step::Validated(_) = step {
                std::fs::create_dir(&racer).unwrap();
                std::fs::write(racer.join("competitor"), "keep").unwrap();
            }
            None
        }),
        || export(&capture, &output),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "live evidence output directory must not already exist."
    );
    assert_eq!(
        std::fs::read_to_string(output.join("competitor")).unwrap(),
        "keep"
    );
    assert!(owned_staging(&root, &output).is_empty());
}

/// The handle-bound rename itself refuses a destination that appears after
/// every check.
#[cfg(windows)]
#[test]
fn the_handle_bound_rename_never_replaces_a_destination() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let output = root.join("output");
    let capture = make_capture(&root);
    let racer = output.clone();
    let outcome = with_hooks(
        Box::new(move |step| {
            if let Step::Promote(_) = step {
                std::fs::create_dir(&racer).unwrap();
            }
            None
        }),
        || export(&capture, &output),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "live evidence output could not be promoted."
    );
    assert!(output.is_dir() && std::fs::read_dir(&output).unwrap().next().is_none());
    assert!(owned_staging(&root, &output).is_empty());
}

#[cfg(windows)]
#[test]
fn replacing_staging_at_the_promotion_boundary_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let output = root.join("output");
    let capture = make_capture(&root);
    let retained = root.join("retained-staging");
    let staging_root = root.clone();
    let output_name = output.clone();
    let outcome = with_hooks(
        Box::new(move |step| {
            if let Step::Promote(_) = step {
                let staging = owned_staging(&staging_root, &output_name).pop().unwrap();
                let blocked = std::fs::rename(&staging, &retained).is_err();
                assert!(blocked, "a held staging directory was renamed");
            }
            None
        }),
        || export(&capture, &output),
    );
    // The rename attempt failed, so the exporter's own staging was promoted.
    let result = outcome.unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert!(!root.join("retained-staging").exists());
}

#[cfg(windows)]
#[test]
fn replacement_attempts_at_cleanup_cannot_remove_another_object() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let output = root.join("output");
    let capture = make_capture(&root);
    let (retained_dir, retained_child) = (root.join("retained"), root.join("retained-child.json"));
    let blocked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = blocked.clone();
    let staging_root = root.clone();
    let outcome = with_hooks(
        Box::new(move |step| match step {
            Step::Reread(_) => Some(simulated("staged output changed")),
            // The held staging directory cannot be renamed before its cleanup.
            Step::CleanupStaging(path) => {
                record
                    .borrow_mut()
                    .push(std::fs::rename(path, &retained_dir).is_err());
                None
            }
            // Nor can a held child be replaced between its validation and deletion.
            Step::Delete("live evidence staging child") => {
                let staging = std::fs::read_dir(&staging_root)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| {
                        p.file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .starts_with(".output.live-evidence-")
                    })
                    .unwrap();
                let child = std::fs::read_dir(&staging)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                record
                    .borrow_mut()
                    .push(std::fs::rename(&child, &retained_child).is_err());
                None
            }
            _ => None,
        }),
        || export(&capture, &output),
    );
    assert_eq!(message(outcome.unwrap_err()), "staged output changed");
    assert_eq!(*blocked.borrow(), [true, true]);
    assert!(!root.join("retained").exists());
    assert!(!root.join("retained-child.json").exists());
    assert!(!output.exists());
    assert!(owned_staging(&root, &output).is_empty());
}

#[cfg(windows)]
#[test]
fn an_unexpected_staging_child_stops_cleanup_without_deleting_any_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let output = root.join("output");
    let capture = make_capture(&root);
    let outcome = with_hooks(
        Box::new(|step| {
            if let Step::Write(path) = step {
                std::fs::write(
                    Path::new(path).parent().unwrap().join("unexpected.json"),
                    b"preserve",
                )
                .unwrap();
            }
            None
        }),
        || export(&capture, &output),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "live evidence staging changed before cleanup."
    );
    let staging = owned_staging(&root, &output);
    assert_eq!(staging.len(), 1);
    assert_eq!(
        std::fs::read(staging[0].join("unexpected.json")).unwrap(),
        b"preserve"
    );
    assert_eq!(std::fs::read_dir(&staging[0]).unwrap().count(), 2);
    assert!(!output.exists());
}

#[cfg(windows)]
#[test]
fn a_cleanup_failure_is_reported_without_official_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let output = root.join("output");
    let capture = make_capture(root);
    let outcome = with_hooks(
        Box::new(|step| match step {
            Step::Write(_) => Some(simulated("disk full")),
            Step::Delete("live evidence staging") => Some(simulated("busy")),
            _ => None,
        }),
        || export(&capture, &output),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "live evidence staging could not be removed."
    );
    assert!(!output.exists());
}

#[cfg(windows)]
#[test]
fn a_held_staging_directory_cannot_be_renamed_away() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let output = root.join("output");
    let capture = make_capture(&root);
    let retained = root.join("retained-staging");
    let outcome = with_hooks(
        Box::new(move |step| {
            if let Step::Write(path) = step {
                let blocked =
                    std::fs::rename(Path::new(path).parent().unwrap(), &retained).is_err();
                assert!(blocked);
            }
            None
        }),
        || export(&capture, &output),
    );
    assert!(outcome.is_ok());
    assert!(output.is_dir());
}

#[cfg(windows)]
#[test]
fn zero_candidates_still_promotes_one_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    set_non_candidate_state(&capture, "UNCHANGED");
    let output = dir.path().join("output");
    assert!(export(&capture, &output).unwrap().candidates.is_empty());
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
}

#[test]
fn producer_version_requires_strict_semver_and_package_equality() {
    for (version, package, valid) in [
        ("01.2.3\n", "01.2.3", false),
        ("1.2.3 \n", "1.2.3", false),
        ("1.2.3\n", "1.2.4", false),
        ("1.2.3\r\n", "1.2.3", true),
        ("1.0.0-rc.1+b.2", "1.0.0-rc.1+b.2", true),
        ("1.2", "1.2", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("VERSION"), version).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            format!("[project]\nversion = \"{package}\"\n"),
        )
        .unwrap();
        live::set_version_dir(Some(s(&project)));
        live::set_not_windows(true);
        let outcome = export(&capture, &dir.path().join("output"));
        live::clear_hooks();
        let expected = if valid {
            "identity-bound live evidence publication is supported only on Windows."
        } else {
            "producer version must be strict Semantic Version 2.0.0."
        };
        assert_eq!(
            message(outcome.unwrap_err()),
            expected,
            "{version:?} {package}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    live::set_version_dir(Some(s(&dir.path().join("absent"))));
    let outcome = export(&capture, &dir.path().join("output"));
    live::clear_hooks();
    assert_eq!(
        message(outcome.unwrap_err()),
        "producer version could not be read."
    );
}

#[test]
fn snapshot_cleanup_errors_are_bounded_before_publication() {
    for (hook, expected) in [
        (
            Box::new(|step: Step| {
                matches!(step, Step::Close(_)).then(|| simulated("raw pinned handle close"))
            }) as Hook,
            "capture snapshot cleanup failed.",
        ),
        (
            Box::new(|step: Step| {
                matches!(step, Step::TempCleanup(_)).then(|| simulated("raw temporary cleanup"))
            }) as Hook,
            "live evidence snapshot cleanup failed.",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        let output = dir.path().join("output");
        let outcome = with_hooks(hook, || export(&capture, &output));
        assert_eq!(message(outcome.unwrap_err()), expected);
        assert!(!output.exists());
    }
}

#[test]
fn snapshot_refusals() {
    let cases: Vec<(Hook, &str)> = vec![
        (
            Box::new(|step| matches!(step, Step::Pin(_)).then(|| simulated("x"))),
            "capture input could not be pinned.",
        ),
        (
            Box::new(|step| matches!(step, Step::List(_)).then(|| simulated("x"))),
            "capture input inventory changed.",
        ),
        (
            Box::new(|step| matches!(step, Step::Snapshot(_)).then(|| simulated("x"))),
            "capture snapshot could not be created.",
        ),
        (
            Box::new(|step| matches!(step, Step::Copy(_)).then(|| simulated("x"))),
            "capture snapshot source file could not be copied.",
        ),
    ];
    for (hook, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        let outcome = with_hooks(hook, || export(&capture, &dir.path().join("output")));
        assert_eq!(message(outcome.unwrap_err()), expected);
    }
    // A source file with a second link is not ordinary.
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    std::fs::hard_link(
        capture.join("monitor-baseline.json"),
        dir.path().join("second-link"),
    )
    .unwrap();
    assert_eq!(
        message(export(&capture, &dir.path().join("output")).unwrap_err()),
        "capture snapshot source file is not ordinary."
    );
    // A source changed after its copy.
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    let target = capture.join("evidence");
    let outcome = with_hooks(
        Box::new(move |step| {
            if let Step::Copied(_) = step {
                std::fs::write(target.join("sha256-0000000000000000000000000000000000000000000000000000000000000000.json"), b"late").unwrap();
            }
            None
        }),
        || export(&capture, &dir.path().join("output")),
    );
    assert_eq!(
        message(outcome.unwrap_err()),
        "capture input inventory changed."
    );
}

#[test]
fn preflight_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    for (output, expected) in [
        (
            dir.path().join(".hidden"),
            "live evidence output name is unsafe.",
        ),
        (
            dir.path().join(".parent").join("output"),
            "live evidence output parent name is unsafe.",
        ),
    ] {
        assert_eq!(message(export(&capture, &output).unwrap_err()), expected);
    }
    // Windows reports a missing path under a file; POSIX reports ENOTDIR, which
    // Python's lstat does not treat as FileNotFoundError.
    std::fs::write(dir.path().join("a-file"), "").unwrap();
    assert_eq!(
        message(export(&capture, &dir.path().join("a-file").join("output")).unwrap_err()),
        if cfg!(windows) {
            "live evidence output parent must be an ordinary directory."
        } else {
            "live evidence output could not be inspected."
        }
    );
}

// ------------------------------------------------------------------- CLI

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|a| a.to_string()).collect()
}

#[test]
fn the_command_reports_errors_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let capture = make_capture(dir.path());
    let mut run = Run::default();
    let code = live::main(&argv(&[s(&capture), "--out", s(&capture)]), &mut run).unwrap();
    assert_eq!(code, 1);
    assert_eq!(
        String::from_utf8(run.stderr).unwrap(),
        "error: live evidence output would replace a capture input.\n"
    );
    match live::main(&argv(&["capture-only"]), &mut Run::default()) {
        Err(tax_radar_au::argparse::Exit::Error(text)) => assert_eq!(
            text,
            "usage: export_live_evidence_bundles [-h] --out OUTPUT_DIR capture_dir\n\
             export_live_evidence_bundles: error: the following arguments are required: --out\n"
        ),
        other => panic!("{other:?}"),
    }
}

#[cfg(windows)]
#[test]
fn the_command_reports_the_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, observation_digest) = write_capture(dir.path(), None, None);
    let mut run = Run::default();
    let code = live::main(
        &argv(&[s(&capture), "--out", s(&dir.path().join("output"))]),
        &mut run,
    )
    .unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        format!(
            "candidate_count=1\nrelease_tag=live-evidence-v2-{}\n",
            &observation_digest[7..]
        )
    );
}
