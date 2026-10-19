//! compare(), queue validation and validate_review(): ported from tests/radar/test_monitor.py.
mod common;

use std::cell::Cell;
use std::rc::Rc;

use common::*;
use sha2::{Digest, Sha256};
use tax_radar_au::radar::monitor;
use tax_radar_au::radar::persist::{self, Fault, Step};
use tax_radar_au::radar::util;
use tempfile::tempdir;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn item0(queue: &mut Value) -> &mut Value {
    &mut queue["items"][0]
}

fn independent_changed_item(
    template: &Value,
    change_kind: &str,
    state: &str,
    mapping_status: &str,
    observed_compilation: bool,
) -> Value {
    let mut item = template.clone();
    item["item_id"] = json!(format!(
        "impact:independent-{}-{}-{}",
        change_kind.to_lowercase(),
        mapping_status.to_lowercase(),
        if observed_compilation {
            "observed"
        } else {
            "null"
        }
    ));
    item["change_kind"] = json!(change_kind);
    item["state"] = json!(state);
    if !observed_compilation {
        item["source"]["observed_compilation"] = Value::Null;
    }
    item["mapping_status"] = json!(mapping_status);
    if mapping_status != "MAPPED" {
        item["impact_candidates"] = json!([]);
    }
    item
}

fn independent_scope_item(template: &Value, change_kind: &str, suffix: &str) -> Value {
    let mut item = template.clone();
    item["item_id"] = json!(format!(
        "impact:independent-{}-{suffix}",
        change_kind.to_lowercase()
    ));
    item["state"] = json!("BLOCKED");
    item["change_kind"] = json!(change_kind);
    item["impact_candidates"] = json!([]);
    item["mapping_status"] = json!("NOT_EVALUATED");
    item["limitations"] = json!(["Synthetic scope evidence requires operator resolution."]);
    if change_kind == "INCOMPLETE_SCOPE" {
        item["source"] = Value::Null;
    } else {
        item["source"]["observed_compilation"] = Value::Null;
    }
    item
}

fn bind_independent_queue(queue: &Value, complete: bool, items: Vec<Value>) -> (Value, Value) {
    let mut queue = queue.clone();
    queue["observation"]["complete"] = json!(complete);
    let status = if items.iter().any(|i| i["state"] == "BLOCKED") {
        "BLOCKED"
    } else if items.is_empty() {
        "NO_CHANGE_DETECTED"
    } else {
        "REVIEW_REQUIRED"
    };
    queue["items"] = Value::Array(items);
    queue["run_status"] = json!(status);
    queue["queue_digest"] = json!(expected_queue_digest(&queue));
    let decision = matching_decision_for_all_open_items(&queue);
    (queue, decision)
}

/// Serialise a payload while retaining a conflicting first JSON member.
fn with_conflicting_duplicate(
    text: &str,
    key: &str,
    original: &Value,
    conflicting: &str,
) -> String {
    let member = format!("{}:{}", json!(key), original);
    assert_eq!(text.matches(&member).count(), 1, "{member}");
    text.replacen(
        &member,
        &format!("{}:{},{member}", json!(key), json!(conflicting)),
        1,
    )
}

#[test]
fn queue_digest_is_present_and_deterministic() {
    let first = queue();
    let second = queue();
    assert_eq!(first["queue_digest"], json!(expected_queue_digest(&first)));
    assert_eq!(second["queue_digest"], first["queue_digest"]);
}

#[test]
fn queue_digest_and_source_digests_include_the_mapping_snapshot() {
    let dir = tempdir().unwrap();
    let original = payload("mappings", "sample-source-skill-map.json");
    let mut changed = original.clone();
    changed["mapping_version"] = json!("2026-08-demo.1-changed-bytes");
    std::fs::create_dir(dir.path().join("first")).unwrap();
    std::fs::create_dir(dir.path().join("second")).unwrap();
    let first = compare_fixtures(&dir.path().join("first"), None, None, Some(&original)).unwrap();
    let second = compare_fixtures(&dir.path().join("second"), None, None, Some(&changed)).unwrap();
    let names: Vec<&String> = first["source_digests"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(names.len(), 3);
    assert_ne!(
        first["source_digests"]["mapping"],
        second["source_digests"]["mapping"]
    );
    assert_ne!(first["run_id"], second["run_id"]);
    assert_ne!(first["queue_digest"], second["queue_digest"]);
}

#[test]
fn queue_digest_rejects_tampered_review_evidence() {
    type Mutation = fn(&mut Value);
    let fields: [(&str, Mutation); 11] = [
        ("source.title", |i| {
            i["source"]["title"] = json!("Sample Consumption Tax Act 2099 amended")
        }),
        ("source.evidence_url", |i| {
            i["source"]["evidence_url"] = json!("https://example.test/replaced-evidence")
        }),
        ("baseline number", |i| {
            i["source"]["baseline_compilation"]["number"] = json!("999")
        }),
        ("baseline date", |i| {
            i["source"]["baseline_compilation"]["date"] = json!("2099-07-02")
        }),
        ("observed number", |i| {
            i["source"]["observed_compilation"]["number"] = json!("999")
        }),
        ("observed date", |i| {
            i["source"]["observed_compilation"]["date"] = json!("2099-08-02")
        }),
        ("document id", |i| {
            i["source"]["observed_compilation"]["document_id"] = json!("C2099C00999")
        }),
        ("impact_candidates", |i| {
            let mut added = i["impact_candidates"][0].clone();
            added["mapping_id"] = json!("map:sample-consumption-tax-to-bas:added-after-review");
            i["impact_candidates"].as_array_mut().unwrap().push(added);
        }),
        ("review_question", |i| {
            i["impact_candidates"][0]["review_question"] = json!("Changed.")
        }),
        ("mapping_status", |i| {
            i["mapping_status"] = json!("UNMAPPED_SOURCE")
        }),
        ("limitations", |i| {
            i["limitations"]
                .as_array_mut()
                .unwrap()
                .push(json!("Additional synthetic limitation."))
        }),
    ];
    for (name, mutate) in fields {
        let dir = tempdir().unwrap();
        let mut queue = queue();
        let decision = matching_decision(&queue);
        mutate(item0(&mut queue));
        assert!(
            validate_payloads(dir.path(), &queue, &decision).is_err(),
            "{name}"
        );
    }
}

#[test]
fn exact_queue_schemas_reject_additional_properties_before_digesting() {
    // The Python test also replaced sha256_json to prove no digest ran first;
    // here the exact-shape message itself shows validation stopped there.
    let locations: [fn(&mut Value) -> &mut Value; 9] = [
        |q| q,
        |q| &mut q["source_digests"],
        |q| &mut q["baseline"],
        |q| &mut q["observation"],
        |q| &mut q["items"][0],
        |q| &mut q["items"][0]["source"],
        |q| &mut q["items"][0]["source"]["baseline_compilation"],
        |q| &mut q["items"][0]["source"]["observed_compilation"],
        |q| &mut q["items"][0]["impact_candidates"][0],
    ];
    for target in locations {
        let dir = tempdir().unwrap();
        let mut queue = queue();
        let decision = matching_decision(&queue);
        target(&mut queue)["unexpected"] = json!("must be rejected");
        refused(
            validate_payloads(dir.path(), &queue, &decision),
            "invalid shape|must contain exactly",
        );
    }
}

#[test]
fn exact_queue_limitations_schema_is_enforced() {
    for (value, message) in [
        (json!("not-a-list"), "limitations must be a list"),
        (json!([""]), "non-empty string"),
        (json!([7]), "non-empty string"),
    ] {
        let dir = tempdir().unwrap();
        let mut queue = queue();
        let decision = matching_decision(&queue);
        queue["items"][0]["limitations"] = value;
        refused(validate_payloads(dir.path(), &queue, &decision), message);
    }
}

#[test]
fn exact_queue_rejects_a_missing_mapping_digest() {
    let dir = tempdir().unwrap();
    let mut queue = queue();
    let decision = matching_decision(&queue);
    queue["source_digests"]
        .as_object_mut()
        .unwrap()
        .remove("mapping");
    refused(
        validate_payloads(dir.path(), &queue, &decision),
        "source_digests.*invalid shape",
    );
}

#[test]
fn exact_queue_rejects_non_string_values_cleanly() {
    let dir = tempdir().unwrap();
    let mut q = queue();
    let decision = matching_decision(&q);
    q["run_status"] = json!(["REVIEW_REQUIRED"]);
    refused(
        validate_payloads(dir.path(), &q, &decision),
        "unsupported run_status",
    );

    let mut q = queue();
    q["items"][0]["impact_candidates"][0]["mapping_id"] = json!(["map:bad"]);
    refused(
        validate_payloads(dir.path(), &q, &decision),
        "non-empty string",
    );
}

#[test]
fn tamper_additional_review_evidence_is_rejected() {
    let locations: [fn(&mut Value) -> &mut Value; 4] = [
        |q| &mut q["items"][0],
        |q| &mut q["items"][0]["source"],
        |q| &mut q["items"][0]["impact_candidates"][0],
        |q| q,
    ];
    for target in locations {
        let dir = tempdir().unwrap();
        let mut queue = queue();
        let decision = matching_decision(&queue);
        target(&mut queue)["unreviewed_property"] = json!("not in the decision");
        assert!(validate_payloads(dir.path(), &queue, &decision).is_err());
    }
}

#[test]
fn tamper_with_unchanged_run_id_is_rejected_by_queue_digest() {
    let dir = tempdir().unwrap();
    let mut queue = queue();
    let decision = matching_decision(&queue);
    let original_run_id = queue["run_id"].clone();
    queue["items"][0]["source"]["title"] = json!("changed after review");
    queue["queue_digest"] = json!(expected_queue_digest(&queue));
    assert_eq!(queue["run_id"], original_run_id);
    refused(
        validate_payloads(dir.path(), &queue, &decision),
        "decision.*queue_digest",
    );
}

#[test]
fn queue_digest_is_required_in_the_decision() {
    for missing in [true, false] {
        let dir = tempdir().unwrap();
        let queue = queue();
        let mut decision = matching_decision(&queue);
        if missing {
            decision.as_object_mut().unwrap().remove("queue_digest");
        } else {
            decision["queue_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
        }
        refused(
            validate_payloads(dir.path(), &queue, &decision),
            "queue_digest",
        );
    }
}

#[test]
fn exact_queue_decision_schemas_reject_additional_properties() {
    for entry in [false, true] {
        let dir = tempdir().unwrap();
        let queue = queue();
        let mut decision = matching_decision(&queue);
        let target = if entry {
            &mut decision["decisions"][0]
        } else {
            &mut decision
        };
        target["unexpected"] = json!("must be rejected");
        refused(validate_payloads(dir.path(), &queue, &decision), "exactly");
    }
}

#[test]
fn queue_digest_validation_receipt_records_both_identifiers() {
    let dir = tempdir().unwrap();
    let queue = queue();
    let receipt = validate_payloads(dir.path(), &queue, &matching_decision(&queue)).unwrap();
    assert_eq!(receipt["status"], "DECISION_RECORDED");
    assert_eq!(receipt["run_id"], queue["run_id"]);
    assert_eq!(receipt["queue_digest"], queue["queue_digest"]);
}

#[test]
fn duplicate_json_members_are_rejected_before_canonicalisation() {
    let queue = queue();
    let decision = matching_decision(&queue);
    let queue_text = queue.to_string();
    let decision_text = decision.to_string();
    let cases = [
        (
            with_conflicting_duplicate(&queue_text, "run_status", &queue["run_status"], "BLOCKED"),
            decision_text.clone(),
        ),
        (
            with_conflicting_duplicate(
                &queue_text,
                "title",
                &queue["items"][0]["source"]["title"],
                "Conflicting first-consumer title",
            ),
            decision_text.clone(),
        ),
        (
            queue_text.clone(),
            with_conflicting_duplicate(
                &decision_text,
                "reviewer_ref",
                &decision["reviewer_ref"],
                "conflicting-first-reviewer",
            ),
        ),
        (
            queue_text.clone(),
            with_conflicting_duplicate(
                &decision_text,
                "item_id",
                &decision["decisions"][0]["item_id"],
                "impact:conflicting-first-item",
            ),
        ),
    ];
    for (queue_text, decision_text) in cases {
        let dir = tempdir().unwrap();
        refused(
            validate_texts(dir.path(), &queue_text, &decision_text),
            "duplicate JSON members",
        );
    }
}

#[test]
fn supported_queue_matrix_accepts_every_changed_kind_and_mapping_shape() {
    let original = queue();
    let anchor = original["items"][0].clone();
    for mapping_status in ["MAPPED", "UNMAPPED_SOURCE"] {
        for (kind, state, observed) in [
            ("SUPERSEDED", "OPEN", true),
            ("CURRENT_NO_PUBLISHED_COMPILATION", "BLOCKED", false),
            ("NO_LONGER_IN_FORCE", "OPEN", false),
            ("LOOKUP_FAILED", "BLOCKED", false),
            ("BASELINE_NOT_CURRENT", "BLOCKED", true),
            ("BASELINE_NOT_CURRENT", "BLOCKED", false),
        ] {
            let dir = tempdir().unwrap();
            let independent =
                independent_changed_item(&anchor, kind, state, mapping_status, observed);
            let (queue, decision) =
                bind_independent_queue(&original, true, vec![anchor.clone(), independent]);
            let receipt = validate_payloads(dir.path(), &queue, &decision).unwrap();
            assert_eq!(
                receipt["status"], "DECISION_RECORDED",
                "{kind} {mapping_status}"
            );
        }
    }
}

#[test]
fn supported_queue_matrix_accepts_scope_kinds_only_in_incomplete_scope() {
    let original = queue();
    let anchor = original["items"][0].clone();
    for with_missing in [false, true] {
        let dir = tempdir().unwrap();
        let mut items = vec![
            anchor.clone(),
            independent_scope_item(&anchor, "INCOMPLETE_SCOPE", "required"),
        ];
        if with_missing {
            items.push(independent_scope_item(
                &anchor,
                "MISSING_OBSERVATION",
                "missing",
            ));
        }
        let (queue, decision) = bind_independent_queue(&original, false, items);
        let receipt = validate_payloads(dir.path(), &queue, &decision).unwrap();
        assert_eq!(receipt["status"], "DECISION_RECORDED");
    }
}

#[test]
fn observation_scope_matrix_rejects_writer_impossible_scope_items() {
    let original = queue();
    let anchor = original["items"][0].clone();
    let incomplete = independent_scope_item(&anchor, "INCOMPLETE_SCOPE", "one");
    let missing = independent_scope_item(&anchor, "MISSING_OBSERVATION", "one");
    let cases = [
        (false, vec![anchor.clone()]),
        (
            false,
            vec![
                anchor.clone(),
                incomplete.clone(),
                independent_scope_item(&anchor, "INCOMPLETE_SCOPE", "two"),
            ],
        ),
        (true, vec![anchor.clone(), incomplete]),
        (true, vec![anchor.clone(), missing]),
    ];
    for (complete, items) in cases {
        let dir = tempdir().unwrap();
        let (queue, decision) = bind_independent_queue(&original, complete, items);
        refused(
            validate_payloads(dir.path(), &queue, &decision),
            "observation complete.*scope items",
        );
    }
}

#[test]
fn non_scope_mapping_matrix_rejects_not_evaluated() {
    let original = queue();
    let anchor = original["items"][0].clone();
    for (kind, state, observed) in [
        ("SUPERSEDED", "OPEN", true),
        ("CURRENT_NO_PUBLISHED_COMPILATION", "BLOCKED", false),
        ("NO_LONGER_IN_FORCE", "OPEN", false),
        ("LOOKUP_FAILED", "BLOCKED", false),
        ("BASELINE_NOT_CURRENT", "BLOCKED", false),
    ] {
        let dir = tempdir().unwrap();
        let independent = independent_changed_item(&anchor, kind, state, "NOT_EVALUATED", observed);
        let (queue, decision) =
            bind_independent_queue(&original, true, vec![anchor.clone(), independent]);
        refused(
            validate_payloads(dir.path(), &queue, &decision),
            "mapping_status.*unsupported",
        );
    }
}

#[test]
fn superseded_source_creates_an_open_exactly_mapped_review_item() {
    let queue = queue();
    assert_eq!(queue["mode"], "synthetic");
    assert_eq!(queue["run_status"], "REVIEW_REQUIRED");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    let item = &queue["items"][0];
    assert_eq!(item["change_kind"], "SUPERSEDED");
    assert_eq!(item["state"], "OPEN");
    assert_eq!(item["mapping_status"], "MAPPED");
    assert_eq!(
        item["impact_candidates"][0]["mapping_basis"],
        "exact_register_id_and_collection"
    );
}

#[test]
fn parsing_and_every_provenance_id_use_the_same_source_bytes() {
    let dir = tempdir().unwrap();
    let sources = [
        ("baseline", sample("baseline", "sample-sources.json")),
        (
            "observation",
            sample("observations", "sample-register-observation.json"),
        ),
        (
            "mapping",
            sample("mappings", "sample-source-skill-map.json"),
        ),
    ];
    let mut digests = serde_json::Map::new();
    let mut paths = Vec::new();
    for (name, source) in &sources {
        let content = std::fs::read(source).unwrap();
        let path = dir.path().join(format!("{name}.json"));
        std::fs::write(&path, &content).unwrap();
        digests.insert(name.to_string(), json!(sha256_hex(&content)));
        paths.push(path);
    }
    // Each file is replaced as soon as its bytes have been decoded for parsing.
    util::set_read_hook(|path| std::fs::write(path.fs(), b"{}\n").unwrap());
    let queue = compare_paths(&paths[0], &paths[1], &paths[2]);
    util::clear_hooks();
    let queue = queue.unwrap();
    let digests = Value::Object(digests);
    assert_eq!(
        queue["run_id"],
        json!(format!("sha256:{}", sha256_json(&digests)))
    );
    assert_eq!(
        queue["baseline"]["id"],
        json!(format!("sha256:{}", digests["baseline"].as_str().unwrap()))
    );
    assert_eq!(
        queue["observation"]["id"],
        json!(format!(
            "sha256:{}",
            digests["observation"].as_str().unwrap()
        ))
    );
    let item = only(&queue, "SUPERSEDED");
    let expected = sha256_json(&json!({
        "change_kind": "SUPERSEDED",
        "identity": {
            "register_id": item["source"]["register_id"],
            "collection": item["source"]["collection"],
            "observed_state": "SUPERSEDED",
        },
        "sources": digests,
    }));
    assert_eq!(
        item["item_id"],
        json!(format!("impact:{}", &expected[..24]))
    );
    assert_eq!(item["mapping_status"], "MAPPED");
    assert!(paths.iter().all(|p| std::fs::read(p).unwrap() == b"{}\n"));
}

#[test]
fn every_item_kind_is_bound_to_all_three_source_snapshots() {
    for shape in ["changed", "incomplete", "missing"] {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        if shape != "changed" {
            observation["complete"] = json!(false);
        }
        if shape == "missing" {
            observation["observations"]
                .as_array_mut()
                .unwrap()
                .remove(0);
        }
        let mut mapping = payload("mappings", "sample-source-skill-map.json");
        let first = compare_fixtures(dir.path(), None, Some(&observation), Some(&mapping)).unwrap();
        mapping["mapping_version"] = json!("2026-08-demo.1-same-rules-new-source");
        let second =
            compare_fixtures(dir.path(), None, Some(&observation), Some(&mapping)).unwrap();
        let ids = |q: &Value| -> Vec<(String, String)> {
            q["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    (
                        i["change_kind"].as_str().unwrap().into(),
                        i["item_id"].as_str().unwrap().into(),
                    )
                })
                .collect()
        };
        let (a, b) = (ids(&first), ids(&second));
        assert_eq!(
            a.iter().map(|x| &x.0).collect::<Vec<_>>(),
            b.iter().map(|x| &x.0).collect::<Vec<_>>()
        );
        assert!(a.iter().zip(&b).all(|(x, y)| x.1 != y.1), "{shape}");
    }
}

#[test]
fn incomplete_scope_blocks_even_when_every_observed_source_is_unchanged() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    observation["complete"] = json!(false);
    for entry in observation["observations"].as_array_mut().unwrap() {
        entry["state"] = json!("UNCHANGED");
        clear_compilation(entry);
    }
    let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
    assert_eq!(queue["run_status"], "BLOCKED");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    let item = only(&queue, "INCOMPLETE_SCOPE");
    assert_eq!(item["state"], "BLOCKED");
    assert_eq!(item["mapping_status"], "NOT_EVALUATED");
}

#[test]
fn a_baseline_title_with_no_observation_is_blocked_not_dropped() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    observation["complete"] = json!(false);
    observation["observations"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["register_id"] != "F2099L00001");
    let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
    let missing = only(&queue, "MISSING_OBSERVATION");
    assert_eq!(missing["state"], "BLOCKED");
    assert_eq!(missing["source"]["register_id"], "F2099L00001");
    assert_eq!(missing["mapping_status"], "NOT_EVALUATED");
    assert_eq!(queue["run_status"], "BLOCKED");
}

#[test]
fn an_unresolved_register_state_is_blocked_not_open() {
    for (state, field, value) in [
        (
            "CURRENT_NO_PUBLISHED_COMPILATION",
            "current_version_start",
            "2099-09-01",
        ),
        ("LOOKUP_FAILED", "error_category", "register_unavailable"),
    ] {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        let entry = &mut observation["observations"][0];
        clear_compilation(entry);
        entry["state"] = json!(state);
        entry[field] = json!(value);
        let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
        assert_eq!(only(&queue, state)["state"], "BLOCKED");
        assert_eq!(queue["run_status"], "BLOCKED");
    }
}

#[test]
fn a_title_no_longer_in_force_is_open_for_human_review() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    clear_compilation(&mut observation["observations"][0]);
    observation["observations"][0]["state"] = json!("NO_LONGER_IN_FORCE");
    let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
    let item = only(&queue, "NO_LONGER_IN_FORCE");
    assert_eq!(item["state"], "OPEN");
    assert_eq!(item["source"]["observed_compilation"], Value::Null);
    assert_eq!(queue["run_status"], "REVIEW_REQUIRED");
}

#[test]
fn a_stale_baseline_row_blocks_even_an_unchanged_observation() {
    let dir = tempdir().unwrap();
    let mut baseline = payload("baseline", "sample-sources.json");
    baseline["titles"][1]["version_is_current"] = json!(false);
    let queue = compare_fixtures(dir.path(), Some(&baseline), None, None).unwrap();
    let item = only(&queue, "BASELINE_NOT_CURRENT");
    assert_eq!(item["state"], "BLOCKED");
    assert_eq!(item["source"]["register_id"], "F2099L00001");
    assert_eq!(queue["run_status"], "BLOCKED");
}

#[test]
fn mappings_are_by_exact_register_id_and_collection() {
    let dir = tempdir().unwrap();
    let mut mapping = payload("mappings", "sample-source-skill-map.json");
    mapping["entries"][0]["collection"] = json!("LegislativeInstrument");
    let queue = compare_fixtures(dir.path(), None, None, Some(&mapping)).unwrap();
    let item = only(&queue, "SUPERSEDED");
    assert_eq!(item["mapping_status"], "UNMAPPED_SOURCE");
    assert_eq!(item["impact_candidates"], json!([]));
    assert_eq!(item["state"], "OPEN");

    let mut mapping = payload("mappings", "sample-source-skill-map.json");
    mapping["entries"] = json!([]);
    let queue = compare_fixtures(dir.path(), None, None, Some(&mapping)).unwrap();
    let item = only(&queue, "SUPERSEDED");
    assert_eq!(item["mapping_status"], "UNMAPPED_SOURCE");
    assert_eq!(queue["run_status"], "REVIEW_REQUIRED");

    let queue = compare_fixtures(dir.path(), None, None, None).unwrap();
    let item = only(&queue, "SUPERSEDED");
    assert_eq!(item["mapping_status"], "MAPPED");
    assert_eq!(
        item["impact_candidates"][0]["mapping_id"],
        "map:sample-consumption-tax-to-bas"
    );
    assert_eq!(item["impact_candidates"][0]["skill_ref"], "bas-preparation");
}

#[test]
fn observation_refusals() {
    type Mutation = fn(&mut Value);
    let cases: Vec<(Mutation, &str)> = vec![
        (
            |o| o["observations"][0]["collection"] = json!("LegislativeInstrument"),
            "collection does not match baseline",
        ),
        (
            |o| o["observations"][0]["observed_compilation_number"] = Value::Null,
            "observed_compilation_number",
        ),
        (
            |o| o["observations"][0]["observed_compilation_date"] = Value::Null,
            "observed_compilation_date",
        ),
        (
            |o| o["observations"][0]["observed_register_document_id"] = Value::Null,
            "observed_register_document_id",
        ),
        (
            |o| {
                o["observations"][0]["state"] = json!("CURRENT_NO_PUBLISHED_COMPILATION");
                o["observations"][0]["current_version_start"] = json!("2099-09-01");
            },
            "null observed_register_document_id",
        ),
        (|o| o["mode"] = json!("live"), "synthetic mode"),
        (
            |o| o["observations"][0]["checked_at"] = json!("last tuesday"),
            "ISO 8601 timestamp",
        ),
        (
            |o| {
                clear_compilation(&mut o["observations"][0]);
                o["observations"][0]["state"] = json!("LOOKUP_FAILED");
            },
            "error_category",
        ),
        (
            |o| {
                o["expected_register_ids"] = json!(["C2099A00001"]);
                o["complete"] = json!(false);
                o["observations"].as_array_mut().unwrap().truncate(1);
            },
            "exactly match the baseline scope",
        ),
        (
            |o| o["observations"].as_array_mut().unwrap().truncate(1),
            "cover every expected",
        ),
        (
            |o| {
                o["complete"] = json!(false);
                let mut extra = o["observations"][0].clone();
                extra["register_id"] = json!("C2099A99999");
                o["observations"].as_array_mut().unwrap().push(extra);
            },
            "outside the baseline scope",
        ),
        (
            |o| {
                let first = o["expected_register_ids"][0].clone();
                o["expected_register_ids"]
                    .as_array_mut()
                    .unwrap()
                    .push(first);
            },
            "must not contain duplicates",
        ),
        (
            |o| o["expected_register_ids"] = json!([["C2099A00001"], "F2099L00001"]),
            "list of strings",
        ),
        (
            |o| o["observations"][0]["state"] = json!(["SUPERSEDED"]),
            "unsupported state",
        ),
        (
            |o| o["observed_at"] = json!("2026-08-08+10:00"),
            "ISO 8601 timestamp",
        ),
    ];
    for (mutate, pattern) in cases {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        mutate(&mut observation);
        refused(
            compare_fixtures(dir.path(), None, Some(&observation), None),
            pattern,
        );
    }
}

#[test]
fn observation_states_reject_contradictory_fields() {
    for (state, field, value) in [
        ("UNCHANGED", "observed_compilation_number", "2"),
        ("SUPERSEDED", "current_version_start", "2099-09-01"),
        (
            "CURRENT_NO_PUBLISHED_COMPILATION",
            "error_category",
            "timeout",
        ),
        (
            "NO_LONGER_IN_FORCE",
            "observed_register_document_id",
            "C2099C00002",
        ),
        ("LOOKUP_FAILED", "current_version_start", "2099-09-01"),
    ] {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        let entry = &mut observation["observations"][0];
        clear_compilation(entry);
        entry["state"] = json!(state);
        entry["current_version_start"] = Value::Null;
        entry["error_category"] = Value::Null;
        match state {
            "SUPERSEDED" => {
                entry["observed_compilation_number"] = json!("2");
                entry["observed_compilation_date"] = json!("2099-08-01");
                entry["observed_register_document_id"] = json!("C2099C00002");
            }
            "CURRENT_NO_PUBLISHED_COMPILATION" => {
                entry["current_version_start"] = json!("2099-09-01")
            }
            "LOOKUP_FAILED" => entry["error_category"] = json!("register_unavailable"),
            _ => {}
        }
        entry[field] = json!(value);
        refused(
            compare_fixtures(dir.path(), None, Some(&observation), None),
            &format!("{state} must have a null {field}"),
        );
    }
}

#[test]
fn a_map_reusing_a_mapping_id_is_rejected() {
    let dir = tempdir().unwrap();
    let mut mapping = payload("mappings", "sample-source-skill-map.json");
    let first = mapping["entries"][0].clone();
    mapping["entries"].as_array_mut().unwrap().push(first);
    refused(
        compare_fixtures(dir.path(), None, None, Some(&mapping)),
        "duplicate mapping IDs",
    );
}

#[test]
fn unreadable_inputs_are_blocked_not_a_traceback() {
    let dir = tempdir().unwrap();
    let observation = sample("observations", "sample-register-observation.json");
    let mapping = sample("mappings", "sample-source-skill-map.json");
    let bad = dir.path().join("baseline.json");
    std::fs::write(&bad, b"{\"source\": \"\xff\xfe not utf-8\"}").unwrap();
    refused(
        compare_paths(&bad, &observation, &mapping),
        "could not be read as UTF-8",
    );

    let directory = dir.path().join("baseline-directory");
    std::fs::create_dir(&directory).unwrap();
    // IsADirectoryError on POSIX and PermissionError on Windows.
    let errno = if cfg!(windows) {
        "[Errno 13]"
    } else {
        "[Errno 21] Is a directory"
    };
    let text = message(compare_paths(&directory, &observation, &mapping));
    assert!(
        text.contains("could not be read: ") && text.contains(errno),
        "{text}"
    );
}

#[test]
fn baseline_refusals() {
    type Mutation = fn(&mut Value);
    let cases: Vec<(Mutation, &str)> = vec![
        (|b| b["titles"] = json!([]), "no titles"),
        (
            |b| {
                let mut reused = b["titles"][0].clone();
                reused["collection"] = json!("LegislativeInstrument");
                b["titles"].as_array_mut().unwrap().push(reused);
            },
            "duplicate register IDs",
        ),
        (
            |b| b["titles"][0]["compilation_number"] = json!("1\n## Injected heading"),
            "control characters",
        ),
    ];
    for (mutate, pattern) in cases {
        let dir = tempdir().unwrap();
        let mut baseline = payload("baseline", "sample-sources.json");
        mutate(&mut baseline);
        refused(
            compare_fixtures(dir.path(), Some(&baseline), None, None),
            pattern,
        );
    }
}

#[test]
fn surrogate_source_metadata_is_rejected() {
    for surrogate in ["\\ud800", "\\udfff"] {
        let dir = tempdir().unwrap();
        let mut baseline = payload("baseline", "sample-sources.json");
        baseline["titles"][0]["name"] = json!("Fabricated SURROGATE");
        let text = baseline.to_string().replace("SURROGATE", surrogate);
        let path = dir.path().join("baseline.json");
        std::fs::write(&path, text).unwrap();
        let result = compare_paths(
            &path,
            &sample("observations", "sample-register-observation.json"),
            &sample("mappings", "sample-source-skill-map.json"),
        );
        refused(result, "surrogate");
        assert!(!dir.path().join("impact-queue.json").exists());
    }
}

#[test]
fn markdown_keeps_limits_visible_and_escapes_source_text() {
    let mut queue = queue();
    queue["items"][0]["source"]["title"] = json!("Demo | [not a link]");
    let markdown = monitor::render_markdown(&radar_json(&queue));
    assert!(markdown.contains("This is a synthetic metadata-review queue"));
    assert!(markdown.contains("Demo \\| \\[not a link\\]"));
    assert!(markdown.contains("does not establish the legal effect"));

    queue["items"][0]["source"]["title"] = json!("Evil <script>alert(1)</script> Act");
    let markdown = monitor::render_markdown(&radar_json(&queue));
    assert!(!markdown.contains("<script>") && !markdown.contains("</script>"));
    assert!(markdown.contains("Evil \\<script\\>alert\\(1\\)\\<\\/script\\> Act"));
}

#[test]
fn markdown_escapes_observed_compilation_metadata() {
    let mut queue = queue();
    let source = &mut queue["items"][0]["source"];
    source["baseline_compilation"]["number"] = json!("1 | [not a link]");
    source["observed_compilation"]["number"] = json!("2`code");
    source["observed_compilation"]["document_id"] = json!("C2099C00002`injected");
    source["evidence_url"] = json!("https://example.test/C2099A00001`tick");
    let markdown = monitor::render_markdown(&radar_json(&queue));
    assert!(markdown.contains("1 \\| \\[not a link\\]"));
    assert!(markdown.contains("2\\`code"));
    assert!(markdown.contains("C2099C00002\\`injected"));
    assert!(markdown.contains("https\\:\\/\\/example\\.test\\/C2099A00001\\`tick"));
}

#[test]
fn queue_writes_and_human_decision_is_structurally_valid() {
    let dir = tempdir().unwrap();
    let path = write_queue(&queue(), &dir.path().join("test-output"));
    let validation = monitor::validate_review(
        &py(&path),
        &py(&sample("decisions", "sample-technical-review.json")),
    )
    .map(|v| value(&v))
    .unwrap();
    assert_eq!(validation["status"], "DECISION_RECORDED");
    assert_eq!(validation["decision_count"], 1);
    assert_eq!(validation["mode"], "synthetic");
}

#[test]
fn interrupted_pair_write_restores_the_previous_pair() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("queue");
    let json_path = write_queue(&queue(), &output);
    let mut replacement = queue();
    replacement["baseline"]["source"] = json!("Replacement source");
    replacement["queue_digest"] = json!(expected_queue_digest(&replacement));
    // The process is interrupted before the Markdown commit.
    let interrupted = Rc::new(Cell::new(false));
    let flag = interrupted.clone();
    persist::set_hook(move |step, _src, dst| {
        if step == Step::Replace && !flag.get() && dst.ends_with(".md") {
            flag.set(true);
            return Fault::Fail(std::io::ErrorKind::Interrupted);
        }
        Fault::Proceed
    });
    let result = monitor::write_queue(&radar_json(&replacement), &py(&output));
    persist::clear_hook();
    assert!(result.is_err() && interrupted.get());
    let decision = dir.path().join("decision.json");
    write(&decision, &matching_decision(&replacement));
    refused(
        monitor::validate_review(&py(&json_path), &py(&decision)),
        "queue_digest",
    );
    assert_eq!(load(&json_path), queue());
    assert_eq!(
        text(&std::fs::read(output.join("impact-queue.md")).unwrap()),
        monitor::render_markdown(&radar_json(&queue()))
    );
    write(&decision, &matching_decision(&queue()));
    let validation = monitor::validate_review(&py(&json_path), &py(&decision))
        .map(|v| value(&v))
        .unwrap();
    assert_eq!(validation["status"], "DECISION_RECORDED");
}

#[test]
fn second_queue_commit_failure_restores_the_previous_pair() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("queue");
    write_queue(&queue(), &output);
    let names_ = ["impact-queue.json", "impact-queue.md"];
    let previous: Vec<Vec<u8>> = names_
        .iter()
        .map(|n| std::fs::read(output.join(n)).unwrap())
        .collect();
    let mut replacement = queue();
    replacement["baseline"]["source"] = json!("Replacement source");
    let failed = Rc::new(Cell::new(false));
    let flag = failed.clone();
    persist::set_hook(move |step, src, dst| {
        if step == Step::Replace
            && !flag.get()
            && dst.ends_with("impact-queue.md")
            && src.ends_with(".partial")
        {
            flag.set(true);
            return Fault::Fail(std::io::ErrorKind::PermissionDenied);
        }
        Fault::Proceed
    });
    let result = monitor::write_queue(&radar_json(&replacement), &py(&output));
    persist::clear_hook();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("simulated Replace failure")
    );
    assert!(failed.get());
    let now: Vec<Vec<u8>> = names_
        .iter()
        .map(|n| std::fs::read(output.join(n)).unwrap())
        .collect();
    assert_eq!(now, previous);
    assert_eq!(names(&output), names_);
}

fn written_queue_and_decision(
    dir: &std::path::Path,
    change: fn(&mut Value),
) -> (std::path::PathBuf, std::path::PathBuf) {
    let json_path = write_queue(&queue(), &dir.join("queue"));
    let mut decision = payload("decisions", "sample-technical-review.json");
    change(&mut decision);
    (json_path, write(&dir.join("decision.json"), &decision))
}

#[test]
fn decision_refusals() {
    type Mutation = fn(&mut Value);
    let cases: Vec<(Mutation, &str)> = vec![
        (
            |d| d["decisions"][0]["decision"] = json!("AUTO_UPDATE_SKILL"),
            "allowlisted",
        ),
        (|d| d["reviewer_ref"] = json!("   "), "reviewer_ref"),
        (
            |d| d["reviewed_at"] = json!("last tuesday"),
            "ISO 8601 timestamp",
        ),
        (
            |d| d["reviewed_at"] = json!("2026-08-07T23:59:59Z"),
            "cannot predate",
        ),
        (
            |d| d["run_id"] = json!(format!("sha256:{}", "0".repeat(64))),
            "exact queue run_id",
        ),
        (
            |d| d["decisions"][0]["item_id"] = json!("impact:never-in-this-queue"),
            "unknown, blocked, or duplicate",
        ),
        (
            |d| {
                let first = d["decisions"][0].clone();
                d["decisions"].as_array_mut().unwrap().push(first);
            },
            "unknown, blocked, or duplicate",
        ),
        (
            |d| d["decisions"][0]["decision"] = json!(["adopt"]),
            "not allowlisted",
        ),
        (
            |d| d["decisions"][0]["item_id"] = json!(["impact:64ea8458e99ade934803959f"]),
            "non-empty string",
        ),
    ];
    for (mutate, pattern) in cases {
        let dir = tempdir().unwrap();
        let (queue_path, decision) = written_queue_and_decision(dir.path(), mutate);
        refused(
            monitor::validate_review(&py(&queue_path), &py(&decision)),
            pattern,
        );
    }
}

#[test]
fn timestamp_grammar_is_pinned() {
    let accepted = |text: &str| {
        let dir = tempdir().unwrap();
        let mut queue = queue();
        queue["observation"]["observed_at"] = json!(text);
        queue["queue_digest"] = json!(expected_queue_digest(&queue));
        let mut decision = matching_decision(&queue);
        decision["reviewed_at"] = json!("2026-08-09T00:00:00.5Z");
        validate_payloads(dir.path(), &queue, &decision)
    };
    for value in [
        "2026-08-08T00:00:00Z",
        "2026-08-08T00:00:00.5Z",
        "2026-08-08T00:00:00.123456+10:00",
        "2026-08-08 00:00:00+00:00",
        "2026-08-08t00:00:00Z",
        "2026-08-08T10:00:00+10:00",
        "2026-08-08T00:00:00.000000Z",
    ] {
        assert_eq!(
            accepted(value).unwrap()["status"],
            "DECISION_RECORDED",
            "{value}"
        );
    }
    for value in [
        "2026-08-08T10:00:00",
        "2026-08-08 10:00:00",
        "2026-08-08T10:00:00.123456",
    ] {
        refused(accepted(value), "explicit UTC offset");
    }
    for value in [
        "2026-08-08",
        "2026-08-08Z",
        "2026-08-08+10:00",
        "2026-08-08-05:00",
        "20260808T000000Z",
        "2026-W32-6T00:00:00Z",
        "2026-08-08T00:00:00+00",
        "2026-08-08T00:00:00z",
        "2026-08-08X00:00:00Z",
        "2026-08-08/00:00:00Z",
        "2026-02-30T00:00:00Z",
    ] {
        refused(
            accepted(value),
            "^impact queue observed_at must be an ISO 8601 timestamp.$",
        );
    }
}

#[test]
fn iso_dates_reject_the_forms_only_newer_interpreters_accept() {
    for value in ["20990701", "2099-W27-1", "2099-182", "2026-02-30"] {
        let dir = tempdir().unwrap();
        let mut baseline = payload("baseline", "sample-sources.json");
        baseline["titles"][0]["compilation_date"] = json!(value);
        refused(
            compare_fixtures(dir.path(), Some(&baseline), None, None),
            "^title 1 compilation_date must be an ISO date.$",
        );
    }
}

#[test]
fn partial_technical_review_remains_explicit() {
    let dir = tempdir().unwrap();
    let mut queue = queue();
    let mut second = queue["items"][0].clone();
    second["item_id"] = json!("impact:second-open-item");
    queue["items"].as_array_mut().unwrap().push(second);
    queue["queue_digest"] = json!(expected_queue_digest(&queue));
    let validation = validate_payloads(dir.path(), &queue, &matching_decision(&queue)).unwrap();
    assert_eq!(validation["status"], "PARTIAL_DECISION_RECORDED");
    assert_eq!(validation["undecided_count"], 1);
}

#[test]
fn https_urls_fail_with_a_domain_error() {
    for value in [
        "https://[malformed",
        "https://example.test:not-a-port/path",
        "https://example.test:65536/path",
        "https://exa mple.test/path",
        "https://example.test\\@other.test/path",
        "http://example.test/x",
        "ftp://example.test/x",
    ] {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        observation["observations"][0]["evidence_url"] = json!(value);
        refused(
            compare_fixtures(dir.path(), None, Some(&observation), None),
            "^observation 1 evidence_url must be an https URL.$",
        );
    }
}

#[test]
fn stored_queue_shape_refusals() {
    type Mutation = fn(&mut Value);
    let cases: Vec<(Mutation, &str)> = vec![
        (
            |q| q["items"][0]["state"] = json!(["OPEN"]),
            "unsupported state",
        ),
        (
            |q| q["mode"] = json!("live"),
            "Only synthetic impact queues",
        ),
        (
            |q| {
                let first = q["items"][0].clone();
                q["items"].as_array_mut().unwrap().push(first);
            },
            "duplicate item IDs",
        ),
        (
            |q| q["run_status"] = json!("NO_CHANGE_DETECTED"),
            "run_status does not match",
        ),
        (|q| q["items"] = json!(["not-an-item"]), "invalid shape"),
    ];
    for (mutate, pattern) in cases {
        let dir = tempdir().unwrap();
        let mut queue = queue();
        mutate(&mut queue);
        let bad = write(&dir.path().join("queue.json"), &queue);
        refused(
            monitor::validate_review(
                &py(&bad),
                &py(&sample("decisions", "sample-technical-review.json")),
            ),
            pattern,
        );
    }
}

#[test]
fn a_blocked_item_cannot_be_signed_off() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    clear_compilation(&mut observation["observations"][0]);
    observation["observations"][0]["state"] = json!("LOOKUP_FAILED");
    observation["observations"][0]["error_category"] = json!("register_unavailable");
    let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
    let decision = json!({
        "schema_version": "au-tax-technical-review.v2",
        "run_id": queue["run_id"],
        "queue_digest": queue["queue_digest"],
        "reviewer_ref": "demo-tax-reviewer",
        "reviewed_at": "2026-08-09T00:00:00Z",
        "decisions": [{
            "item_id": queue["items"][0]["item_id"],
            "decision": "ESCALATE_TECHNICAL_REVIEW",
            "rationale": "The source lookup is blocked.",
            "evidence_note": "Reviewed the synthetic lookup failure.",
        }],
    });
    refused(
        validate_payloads(dir.path(), &queue, &decision),
        "unknown, blocked, or duplicate",
    );
}

#[test]
fn samples_ship_with_the_repository() {
    for (kind, name) in [
        ("baseline", "sample-sources.json"),
        ("observations", "sample-register-observation.json"),
        ("mappings", "sample-source-skill-map.json"),
        ("decisions", "sample-technical-review.json"),
    ] {
        assert!(sample(kind, name).is_file());
    }
}

#[test]
fn decision_files_are_accepted_from_any_directory() {
    let dir = tempdir().unwrap();
    let json_path = write_queue(&queue(), &dir.path().join("queue"));
    let decision = dir.path().join("a-human-technical-review.json");
    std::fs::copy(
        sample("decisions", "sample-technical-review.json"),
        &decision,
    )
    .unwrap();
    let validation = monitor::validate_review(&py(&json_path), &py(&decision))
        .map(|v| value(&v))
        .unwrap();
    assert_eq!(validation["status"], "DECISION_RECORDED");
}

#[test]
fn outputs_write_relative_to_the_current_directory() {
    let dir = tempdir().unwrap();
    let output = cli(
        &compare_argv(std::path::Path::new("build/relative-out"))
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        Some(dir.path()),
    );
    assert_eq!(output.status.code(), Some(0));
    let markdown =
        std::fs::read_to_string(dir.path().join("build/relative-out/impact-queue.md")).unwrap();
    assert!(markdown.starts_with("# AU Tax Change Impact Queue"));
}

#[test]
fn the_radar_has_no_network_client() {
    // The Python suite walked the package's imports; here the crate's sources
    // and its manifest are checked for any network client.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    for client in [
        "reqwest",
        "ureq",
        "hyper",
        "tokio",
        "curl",
        "native-tls",
        "rustls",
        "mio",
    ] {
        assert!(
            !manifest.contains(&format!("\n{client} ")),
            "Cargo.toml depends on {client}"
        );
    }
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                // std::net's address types parse bracketed URL hosts, as
                // Python's ipaddress did for urllib.parse; sockets are refused.
                for forbidden in ["TcpStream", "UdpSocket", "TcpListener", "ToSocketAddrs"] {
                    assert!(
                        !source.contains(forbidden),
                        "{} uses {forbidden}",
                        path.display()
                    );
                }
            }
        }
    }
}

#[test]
fn padded_identifiers_the_loader_accepted_still_map() {
    for field in ["register_id", "collection"] {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        let padded = format!(
            "{} ",
            observation["observations"][0][field].as_str().unwrap()
        );
        observation["observations"][0][field] = json!(padded);
        let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
        assert!(
            queue["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["change_kind"] != "MISSING_OBSERVATION")
        );
    }
}

#[test]
fn nominal_v2_and_v3_observations_load_and_compare() {
    for observation in [
        payload("observations", "sample-register-observation-v2.json"),
        v3_observation(),
    ] {
        let dir = tempdir().unwrap();
        let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
        assert_eq!(queue["schema_version"], "au-tax-impact-queue.v2");
        assert_eq!(queue["run_status"], "REVIEW_REQUIRED");
        assert_eq!(queue["observation"]["complete"], true);
        assert_eq!(queue["items"].as_array().unwrap().len(), 1);
        assert_eq!(queue["items"][0]["change_kind"], "SUPERSEDED");
    }
}

#[test]
fn v3_observation_rejects_invalid_content_evidence() {
    for (field, value) in [
        ("content_sha256", format!("sha256:{}", "A".repeat(64))),
        ("content_sha256", format!("sha256:{}", "a".repeat(63))),
        ("content_kind", "web-page".into()),
        (
            "content_media_type",
            "application/json; charset=utf-8".into(),
        ),
    ] {
        let dir = tempdir().unwrap();
        let mut observation = v3_observation();
        observation["observations"][0][field] = json!(value);
        refused(
            compare_fixtures(dir.path(), None, Some(&observation), None),
            field,
        );
    }
}

#[test]
fn v2_observation_scope_and_evidence_rules() {
    type Mutation = fn(&mut Value);
    let cases: Vec<(Mutation, &str)> = vec![
        (
            |o| {
                o.as_object_mut().unwrap().remove("scope_id");
            },
            "must contain exactly",
        ),
        (
            |o| o["scope_id"] = json!("   "),
            "scope_id must be a non-empty string",
        ),
        (
            |o| o["observations"][1]["evidence_id"] = o["observations"][0]["evidence_id"].clone(),
            "duplicate evidence_id",
        ),
        (
            |o| {
                o["observations"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("evidence_id");
            },
            "invalid shape",
        ),
    ];
    for (mutate, pattern) in cases {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation-v2.json");
        mutate(&mut observation);
        refused(
            compare_fixtures(dir.path(), None, Some(&observation), None),
            pattern,
        );
    }
}

#[test]
fn unsupported_observation_schema_version_is_rejected() {
    for version in [
        json!("au-tax-register-observation.v99"),
        json!([]),
        json!({}),
    ] {
        let dir = tempdir().unwrap();
        let mut observation = payload("observations", "sample-register-observation.json");
        observation["schema_version"] = version;
        refused(
            compare_fixtures(dir.path(), None, Some(&observation), None),
            r"Only au-tax-register-observation\.v1, au-tax-register-observation\.v2 and au-tax-register-observation\.v3",
        );
    }
}
