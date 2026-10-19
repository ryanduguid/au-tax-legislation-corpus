//! exposure(): ported from tests/radar/test_exposure.py, the exposure cases of
//! test_radar_paths.py and the exposure budgets of test_security_remediation.py.
mod common;

use std::path::{Path, PathBuf};

use common::*;
use tax_radar_au::radar::{exposure, monitor, persist};
use tempfile::tempdir;

fn profiles_sample() -> PathBuf {
    sample("profiles", "sample-client-profiles.json")
}

fn workpaper_map() -> PathBuf {
    sample("mappings", "sample-workpaper-map.json")
}

fn skill_map() -> PathBuf {
    sample("mappings", "sample-source-skill-map.json")
}

/// Write the queue pair for the samples, optionally with another map or observation.
fn queue_file(dir: &Path, mapping: &Path, observation: Option<&Value>, name: &str) -> PathBuf {
    let observation_path = match observation {
        Some(doc) => write(&dir.join(format!("{name}-observation.json")), doc),
        None => sample("observations", "sample-register-observation.json"),
    };
    let queue = compare_paths(
        &sample("baseline", "sample-sources.json"),
        &observation_path,
        mapping,
    )
    .unwrap();
    write_queue(&queue, &dir.join(name))
}

fn default_queue(dir: &Path) -> PathBuf {
    queue_file(dir, &workpaper_map(), None, "queue")
}

fn profiles_file(dir: &Path, profiles: Value) -> PathBuf {
    write(
        &dir.join("profiles.json"),
        &json!({"schema_version": "au-tax-client-profiles.v1", "profiles_version": "test.1", "profiles": profiles}),
    )
}

fn report(queue: &Path, profiles: &Path) -> tax_radar_au::radar::Result<Value> {
    exposure::exposure(&py(queue), &py(profiles)).map(|r| value(&r))
}

fn markdown(report: &Value) -> String {
    exposure::render_exposure_markdown(&radar_json(report))
}

#[test]
fn sample_profiles_match_by_exact_skill_reference() {
    let dir = tempdir().unwrap();
    let report = report(&default_queue(dir.path()), &profiles_sample()).unwrap();
    let items = report["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["exposure_status"], "PROFILES_MATCHED");
    assert_eq!(
        items[0]["matches"],
        json!([
            {
                "profile_id": "FAB-CLIENT-001",
                "matched_skill_refs": ["bas-preparation", "cashflow-forecast"],
                "mapping_ids": ["map:sample-consumption-tax-to-bas", "map:sample-consumption-tax-to-cash-assumptions"],
            },
            {
                "profile_id": "FAB-CLIENT-002",
                "matched_skill_refs": ["bas-preparation"],
                "mapping_ids": ["map:sample-consumption-tax-to-bas"],
            },
        ])
    );
    assert_eq!(
        report["profiles_without_candidate_match"],
        json!(["FAB-CLIENT-003"])
    );
    use sha2::Digest;
    let digest: String = sha2::Sha256::digest(std::fs::read(profiles_sample()).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        report["profiles"]["file_sha256"],
        json!(format!("sha256:{digest}"))
    );
    assert_eq!(report["queue"]["mode"], "synthetic");
}

#[test]
fn the_digest_is_deterministic_and_binds_every_input() {
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let first = report(&queue, &profiles_sample()).unwrap();
    assert_eq!(first, report(&queue, &profiles_sample()).unwrap());
    let reformatted = write(
        &dir.path().join("reformatted.json"),
        &load(&profiles_sample()),
    );
    let third = report(&queue, &reformatted).unwrap();
    assert_eq!(third["items"], first["items"]);
    assert_ne!(
        third["profiles"]["file_sha256"],
        first["profiles"]["file_sha256"]
    );
    assert_ne!(third["exposure_digest"], first["exposure_digest"]);
}

#[test]
fn an_edited_queue_item_with_its_old_digest_is_refused() {
    let dir = tempdir().unwrap();
    let queue_path = default_queue(dir.path());
    let mut queue = load(&queue_path);
    queue["items"][0]["impact_candidates"][0]["skill_ref"] = json!("payroll-tax-contractors");
    std::fs::write(
        &queue_path,
        serde_json::to_string_pretty(&queue).unwrap() + "\n",
    )
    .unwrap();
    std::fs::remove_file(queue_path.with_extension("md")).unwrap();
    refused(
        report(&queue_path, &profiles_sample()),
        "queue_digest does not match",
    );
}

#[test]
fn a_stale_markdown_companion_is_refused() {
    let dir = tempdir().unwrap();
    let queue_path = default_queue(dir.path());
    let companion = queue_path.with_extension("md");
    let mut bytes = std::fs::read(&companion).unwrap();
    bytes.extend_from_slice(b"edited\n");
    std::fs::write(&companion, bytes).unwrap();
    refused(
        report(&queue_path, &profiles_sample()),
        "Markdown does not match",
    );
}

#[test]
fn markdown_line_endings_and_absent_companion_are_accepted() {
    for line_ending in [None, Some("\n"), Some("\r\n"), Some("\r")] {
        let dir = tempdir().unwrap();
        let queue_path = queue_file(dir.path(), &skill_map(), None, "queue");
        let decision = sample("decisions", "sample-technical-review.json");
        let expected_exposure = report(&queue_path, &profiles_sample()).unwrap();
        let review = || {
            monitor::validate_review(&py(&queue_path), &py(&decision))
                .map(|v| value(&v))
                .unwrap()
        };
        let expected_review = review();
        let companion = queue_path.with_extension("md");
        match line_ending {
            None => std::fs::remove_file(&companion).unwrap(),
            Some(ending) => {
                let content = text(&std::fs::read(&companion).unwrap());
                std::fs::write(&companion, content.replace('\n', ending)).unwrap();
            }
        }
        assert_eq!(
            report(&queue_path, &profiles_sample()).unwrap(),
            expected_exposure
        );
        assert_eq!(review(), expected_review);
    }
}

#[test]
fn unreadable_markdown_companions_keep_the_read_error() {
    for kind in ["directory", "invalid-utf8"] {
        let dir = tempdir().unwrap();
        let queue_path = default_queue(dir.path());
        let companion = queue_path.with_extension("md");
        if kind == "directory" {
            std::fs::remove_file(&companion).unwrap();
            std::fs::create_dir(&companion).unwrap();
        } else {
            std::fs::write(&companion, b"\xff").unwrap();
        }
        refused(
            report(&queue_path, &profiles_sample()),
            r"^Queue Markdown could not be read; regenerate the pair\.$",
        );
    }
}

#[test]
fn blocked_and_not_evaluated_items_keep_their_state() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    observation["complete"] = json!(false);
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    let entry = &mut observation["observations"][0];
    clear_compilation(entry);
    entry["state"] = json!("LOOKUP_FAILED");
    entry["error_category"] = json!("timeout");
    let report = report(
        &queue_file(dir.path(), &workpaper_map(), Some(&observation), "queue"),
        &profiles_sample(),
    )
    .unwrap();
    let by_kind = |kind: &str| {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["change_kind"] == kind)
            .unwrap()
            .clone()
    };
    assert_eq!(report["queue"]["run_status"], "BLOCKED");
    assert_eq!(report["queue"]["observation_complete"], false);
    assert_eq!(
        by_kind("INCOMPLETE_SCOPE")["exposure_status"],
        "NOT_EVALUATED"
    );
    assert_eq!(by_kind("INCOMPLETE_SCOPE")["source"], Value::Null);
    assert_eq!(
        by_kind("MISSING_OBSERVATION")["exposure_status"],
        "NOT_EVALUATED"
    );
    let lookup = by_kind("LOOKUP_FAILED");
    assert_eq!(lookup["state"], "BLOCKED");
    assert_eq!(lookup["exposure_status"], "PROFILES_MATCHED");
    let ids: Vec<&Value> = lookup["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["profile_id"])
        .collect();
    assert_eq!(ids, [&json!("FAB-CLIENT-001"), &json!("FAB-CLIENT-002")]);
    assert!(markdown(&report).contains("- Source: none; the observation scope was incomplete."));
}

#[test]
fn unmapped_and_unmatched_items_are_distinguished() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    let entry = &mut observation["observations"][1];
    entry["state"] = json!("SUPERSEDED");
    entry["observed_compilation_number"] = json!("2");
    entry["observed_compilation_date"] = json!("2099-08-01");
    entry["observed_register_document_id"] = json!("F2099C00002");
    let queue_path = queue_file(dir.path(), &skill_map(), Some(&observation), "queue");
    let profiles = profiles_file(
        dir.path(),
        json!([{"profile_id": "FAB-9", "skill_refs": ["cashflow-forecast"]}]),
    );
    let report = report(&queue_path, &profiles).unwrap();
    let statuses: Vec<(String, String)> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["source"]["register_id"].as_str().unwrap().into(),
                i["exposure_status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(statuses.contains(&("C2099A00001".into(), "NO_PROFILE_MATCH".into())));
    assert!(statuses.contains(&("F2099L00001".into(), "UNMAPPED_SOURCE".into())));
    assert_eq!(statuses.len(), 2);
    assert_eq!(report["profiles_without_candidate_match"], json!(["FAB-9"]));
    assert!(markdown(&report).contains("- No profile lists a candidate skill for this item."));
}

#[test]
fn malformed_profiles_are_refused_without_echoing_values() {
    let cases = [
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": ["bas-preparation"], "name": "x"}]"#,
            "must contain exactly",
        ),
        (
            r#"[{"profile_id": "Jane Citizen", "skill_refs": ["bas-preparation"]}]"#,
            "no spaces",
        ),
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": ["bas-preparation"]}, {"profile_id": "FAB-1", "skill_refs": ["bas-preparation"]}]"#,
            "repeats an earlier profile_id",
        ),
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": [" bas-preparation"]}]"#,
            "without surrounding spaces",
        ),
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": ["bas-\ud800"]}]"#,
            "lone surrogates",
        ),
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": ["bas-preparation", "bas-preparation"]}]"#,
            "repeats a skill reference",
        ),
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": []}]"#,
            "must list 1 to",
        ),
        (
            r#"[{"profile_id": "FAB-1", "skill_refs": ["private-\u007fref"]}]"#,
            "control characters",
        ),
        ("[]", "non-empty profiles list"),
    ];
    for (profiles, pattern) in cases {
        let dir = tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            format!(r#"{{"schema_version": "au-tax-client-profiles.v1", "profiles_version": "test.1", "profiles": {profiles}}}"#),
        )
        .unwrap();
        let text = refused(report(&default_queue(dir.path()), &path), pattern);
        assert!(!text.contains("Jane") && !text.contains("private-"));
    }
}

#[test]
fn unsupported_schema_and_versions_are_refused() {
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let mut doc = load(&profiles_sample());
    doc["schema_version"] = json!("au-tax-client-profiles.v0");
    refused(
        report(&queue, &write(&dir.path().join("p.json"), &doc)),
        "unsupported schema",
    );

    let mut doc = load(&profiles_sample());
    doc["profiles_version"] = json!("vSURROGATE");
    let path = dir.path().join("p.json");
    std::fs::write(&path, doc.to_string().replace("SURROGATE", "\\ud800")).unwrap();
    refused(report(&queue, &path), "profiles_version must be");
}

#[test]
fn oversized_profiles_and_too_many_profiles_are_refused() {
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let large = dir.path().join("large.json");
    std::fs::write(&large, vec![b' '; 4_000_001]).unwrap();
    refused(
        report(&queue, &large),
        "^client profiles exceeds 4000000 bytes.$",
    );

    let many: Vec<Value> = (0..10_001)
        .map(|i| json!({"profile_id": format!("FAB-{i}"), "skill_refs": ["bas-preparation"]}))
        .collect();
    let profiles = profiles_file(dir.path(), Value::Array(many));
    refused(
        report(&queue, &profiles),
        "^Client profiles exceed 10000 profiles.$",
    );
}

#[test]
fn a_file_larger_than_its_reported_size_is_read_only_to_the_limit() {
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let large = dir.path().join("large.json");
    std::fs::write(&large, vec![b' '; 4_500_000]).unwrap();
    // A growing or size-zero virtual file reports less than it holds.
    tax_radar_au::radar::util::set_size_hook(
        |path, size| if path.name() == "large.json" { 0 } else { size },
    );
    let result = report(&queue, &large);
    tax_radar_au::radar::util::clear_hooks();
    refused(result, "exceeds 4000000 bytes");
}

#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_the_profiles_is_refused_without_blocking() {
    let dir = tempdir().unwrap();
    let fifo = dir.path().join("profiles.json");
    let name = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: the path is a valid NUL-terminated string.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let queue = default_queue(dir.path());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(report(&queue, &fifo).map_err(|e| e.to_string()));
    });
    let outcome = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("opening the FIFO blocked");
    assert!(outcome.unwrap_err().contains("must be a regular file"));
}

#[test]
fn the_report_never_replaces_an_input() {
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let out = dir.path().join("out");
    let profiles = write(&out.join("client-exposure.json"), &load(&profiles_sample()));
    let built = exposure::exposure(&py(&queue), &py(&profiles)).unwrap();
    refused(
        exposure::write_exposure(&built, &py(&out), &py(&queue), &py(&profiles)),
        "would replace an input",
    );
    assert_eq!(load(&profiles), load(&profiles_sample()));
}

#[test]
fn profiles_have_no_markdown_companion() {
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let out = dir.path().join("out");
    let profiles = write(&out.join("client-exposure.txt"), &load(&profiles_sample()));
    let original = std::fs::read(&profiles).unwrap();
    let built = exposure::exposure(&py(&queue), &py(&profiles)).unwrap();
    let (json_path, markdown_path) =
        exposure::write_exposure(&built, &py(&out), &py(&queue), &py(&profiles)).unwrap();
    assert_eq!(std::fs::read(&profiles).unwrap(), original);
    assert_eq!(load(&json_path.fs()), value(&built));
    assert_eq!(
        text(&std::fs::read(markdown_path.fs()).unwrap()),
        exposure::render_exposure_markdown(&built)
    );
}

#[test]
fn the_queue_markdown_companion_is_protected() {
    let dir = tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let queue = out.join("client-exposure.txt");
    std::fs::copy(default_queue(dir.path()), &queue).unwrap();
    let built = exposure::exposure(&py(&queue), &py(&profiles_sample())).unwrap();
    refused(
        exposure::write_exposure(&built, &py(&out), &py(&queue), &py(&profiles_sample())),
        "would replace an input file",
    );
    assert!(!out.join("client-exposure.md").exists());
}

#[cfg(unix)]
#[test]
fn the_report_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let built = exposure::exposure(&py(&queue), &py(&profiles_sample())).unwrap();
    let (a, b) = exposure::write_exposure(
        &built,
        &py(&dir.path().join("out")),
        &py(&queue),
        &py(&profiles_sample()),
    )
    .unwrap();
    for path in [a, b] {
        assert_eq!(
            std::fs::metadata(path.fs()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn output_stems_are_package_names_not_paths() {
    let dir = tempdir().unwrap();
    refused(persist::output_paths(&py(dir.path()), "../escape"), "stem");
}

#[test]
fn empty_queues_and_complete_matches_are_stated() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    clear_compilation(&mut observation["observations"][0]);
    observation["observations"][0]["state"] = json!("UNCHANGED");
    let queue = queue_file(dir.path(), &workpaper_map(), Some(&observation), "empty");
    let report_value = report(&queue, &profiles_sample()).unwrap();
    assert_eq!(report_value["items"], json!([]));
    assert!(markdown(&report_value).contains("The queue holds no items, so no profile is listed."));

    let profiles = profiles_file(
        dir.path(),
        json!([{"profile_id": "FAB-A", "skill_refs": ["bas-preparation"]}]),
    );
    let report_value = report(&default_queue(dir.path()), &profiles).unwrap();
    assert_eq!(report_value["profiles_without_candidate_match"], json!([]));
    assert!(markdown(&report_value).contains("Every profile matched at least one candidate."));
}

/// A baseline of `titles` superseded titles, each mapped `per_title` times to one skill.
fn wide_queue(dir: &Path, titles: usize, per_title: usize) -> PathBuf {
    let mut baseline = payload("baseline", "sample-sources.json");
    let template = baseline["titles"][0].clone();
    let mut observation = payload("observations", "sample-register-observation.json");
    let observed = observation["observations"][0].clone();
    let mut title_rows = Vec::new();
    let mut observed_rows = Vec::new();
    let mut entries = Vec::new();
    for t in 0..titles {
        let id = format!("C2099A{t:05}");
        let mut title = template.clone();
        title["register_id"] = json!(id);
        title_rows.push(title);
        let mut row = observed.clone();
        row["register_id"] = json!(id);
        observed_rows.push(row);
        for m in 0..per_title {
            entries.push(json!({
                "mapping_id": format!("map:{t}-{m}"), "register_id": id, "collection": "Act",
                "source_kind": "legislation", "skill_ref": "bas-preparation",
                "skill_path": ".claude/skills/bas/SKILL.md", "owner_role": "reviewer",
                "review_question": "Assess the change.",
            }));
        }
    }
    baseline["titles"] = Value::Array(title_rows);
    observation["expected_register_ids"] = Value::Array(
        observed_rows
            .iter()
            .map(|r| r["register_id"].clone())
            .collect(),
    );
    observation["observations"] = Value::Array(observed_rows);
    let mapping = json!({"schema_version": "au-tax-source-skill-map.v1", "mapping_version": "wide", "entries": entries});
    let queue = compare_paths(
        &write(&dir.join("b.json"), &baseline),
        &write(&dir.join("o.json"), &observation),
        &write(&dir.join("m.json"), &mapping),
    )
    .unwrap();
    write_queue(&queue, &dir.join("wide"))
}

fn many_profiles(count: usize, refs: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|i| {
                let mut skills = vec![json!("bas-preparation")];
                skills.extend((1..refs).map(|r| json!(format!("skill-{r}"))));
                json!({"profile_id": format!("FAB-{i:05}"), "skill_refs": skills})
            })
            .collect(),
    )
}

#[test]
fn exposure_aggregate_budgets_block_the_complete_report() {
    // The Python suite lowered each limit to 1; these inputs exceed the real ones.
    let dir = tempdir().unwrap();
    let queue = default_queue(dir.path());
    let profiles = profiles_file(dir.path(), many_profiles(1_001, 200));
    let text = refused(
        report(&queue, &profiles),
        "^Profiles exceed 200000 skill associations; split the profiles file.$",
    );
    assert!(!text.contains("FAB-"));

    let work = wide_queue(&dir.path().join("work"), 1, 101);
    let profiles = profiles_file(dir.path(), many_profiles(10_000, 1));
    refused(
        report(&work, &profiles),
        r"^Exposure exceeds 1000000 candidate/profile visits; split the inputs\.$",
    );

    let matches = wide_queue(&dir.path().join("matches"), 21, 1);
    refused(
        report(&matches, &profiles),
        r"^Exposure exceeds 200000 profile matches; split the profiles file\.$",
    );
}
