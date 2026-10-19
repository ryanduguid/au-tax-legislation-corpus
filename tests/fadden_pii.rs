//! The PII detectors and `fadden pii_scan` / `pii_scan2`: ported from
//! PiiNameGateTests in tests/corpus/test_regressions.py (which still covers
//! the Python module the distribution stages use) and the scan tests in
//! tests/corpus/test_pipeline_paths.py. Every row and contact is fabricated.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::pii;

const ALL_CAPS_ROW: &str = "| SMITH, John | 12345678 | s 30-15 |\n\
                            | McDonald, Anne | 23456789 | s 30-15 |\n\
                            | O'Brien, Patrick | 34567890 | s 30-20 |";

fn kinds(text: &str) -> Vec<&'static str> {
    pii::contact_fingerprints(text)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

#[test]
fn caps_mac_and_apostrophe_surnames_are_visible_to_the_gate() {
    let (names, numbers) = pii::private_person_registration_details(ALL_CAPS_ROW);
    for name in ["SMITH, John", "McDonald, Anne", "O'Brien, Patrick"] {
        assert!(names.contains(name), "{name}");
    }
    assert!(numbers.len() >= 3);
}

#[test]
fn statutory_vocabulary_is_filtered_case_insensitively() {
    let (names, _) = pii::private_person_registration_details(
        "TAXATION ADMINISTRATION PROVISIONS 12345678 23456789 34567890",
    );
    assert!(names.is_empty());
    assert!(
        pii::private_person_registration_details("Deputy Commissioner of Taxation")
            .0
            .is_empty()
    );
    assert!(pii::has_private_person_registration_pair(
        "| Smith, John | 12345678 | s 30-15 |"
    ));
    assert!(!pii::has_private_person_registration_pair(
        "Deputy Commissioner of Taxation referred to section 12345678."
    ));
}

#[test]
fn contact_fingerprints_are_normalised_and_tfn_shaped() {
    let first = pii::contact_fingerprints(
        "Email Privacy.Review@Example.Test or phone (02) 1234 5678. Tax file number: 123 456 789.",
    );
    let second = pii::contact_fingerprints(
        "email privacy.review@example.test or phone 02 1234 5678. tax file number 123456789.",
    );
    assert_eq!(first, second);
    assert_eq!(
        kinds("Email a@b.test or phone (02) 1234 5678. Tax file number: 123 456 789."),
        ["email", "phone", "tfn"]
    );
    assert!(kinds("The expression tax file number 7/subsection 2 is a heading.").is_empty());
}

#[test]
fn phone_gate_sees_unspaced_international_and_short_code_forms() {
    for text in [
        "0412345678",
        "0212345678",
        "0412 345 678",
        "+61 2 1234 5678",
        "+61412345678",
        "+61 (0)2 1234 5678",
        "+61 (02) 1234 5678",
        "13 24 68",
        "132468",
        "1300 123 456",
        "1800123456",
        "Ph.13 28 61",
    ] {
        assert!(kinds(text).contains(&"phone"), "{text}");
    }
    for text in [
        "s 12345678",
        "section 123 456",
        "the year 2026",
        "$1,234,567",
        "a $1 300 000 000 appropriation",
        "$100 000 000",
        "ABN 12 345 678 901",
        "factor 0.134567",
        "a rate of 1.130000",
    ] {
        assert!(!kinds(text).contains(&"phone"), "{text}");
    }
    assert_eq!(
        pii::contact_fingerprints("(02) 1234 5678"),
        pii::contact_fingerprints("+61 2 1234 5678")
    );
}

#[test]
fn tfn_gate_sees_abbreviated_labels_and_bare_nine_digit_runs() {
    let labelled = pii::contact_fingerprints("TFN: 123 456 782");
    assert_eq!(
        labelled.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        ["tfn"]
    );
    for text in [
        "Tax file number: 123456782",
        "TFN 123-456-782",
        "123 456 782",
        "123456782",
    ] {
        assert_eq!(pii::contact_fingerprints(text), labelled, "{text}");
    }
    for text in [
        "section 123 456",
        "s 12345678",
        "the year 2026",
        "$1,234,567",
        "123 456 789",
        "$123 456 782",
        "worth $100 000 000 in total",
        "ABN 12 345 678 901",
    ] {
        assert!(pii::contact_fingerprints(text).is_empty(), "{text}");
    }
}

fn policy(dir: &Path, document: Value) -> Result<Vec<pii::Approved>, String> {
    let path = dir.join("policy.json");
    std::fs::write(&path, document.to_string()).unwrap();
    pii::load_contact_allowlist(&path.to_string_lossy()).map_err(|e| e.to_string())
}

#[test]
fn contact_allowlist_is_strictly_validated() {
    let dir = tempfile::tempdir().unwrap();
    let (kind, digest) = pii::contact_fingerprints("Tax file number: 123 456 789.").remove(0);
    assert_eq!(kind, "tfn");
    let entry = |kind: &str, sha: &str, reason: &str| json!({"kind": kind, "sha256": sha, "register_id": "C2004A00001", "reason": reason});
    let zeros = "0".repeat(64);
    let doc = |entries: Value| json!({"schema_version": 1, "entries": entries});
    for (document, message) in [
        (
            doc(json!([entry("tfn", &digest, "must never be accepted")])),
            "allowlist kind",
        ),
        (
            doc(json!([entry(
                "email",
                &zeros,
                "reviewed privacy-review@example.test"
            )])),
            "raw identifier",
        ),
        (
            doc(json!([
                entry("email", &zeros, "office"),
                entry("email", &zeros, "office")
            ])),
            "duplicate",
        ),
        (doc(json!([entry("email", "ABC", "office")])), "digest"),
        (doc(json!([entry("email", &zeros, "  ")])), "reason"),
        (json!({"schema_version": 2, "entries": []}), "unsupported"),
        (json!({"entries": []}), "invalid contact allowlist document"),
    ] {
        let error = policy(dir.path(), document).unwrap_err();
        assert!(error.contains(message), "{error:?} lacks {message:?}");
    }
    let approved = policy(dir.path(), doc(json!([entry("email", &zeros, "office")]))).unwrap();
    assert_eq!(
        approved,
        [("email".into(), zeros.clone(), "C2004A00001".into())]
    );
    let list_kind =
        doc(json!([{"kind": [], "sha256": zeros, "register_id": "C2004A00001", "reason": "x"}]));
    assert_eq!(
        policy(dir.path(), list_kind).unwrap_err(),
        "cannot use 'list' as a set element (unhashable type: 'list')"
    );
}

// ------------------------------------------------------------- the scans

const FLAGGED: &str = "F2026N00001";
const PLAIN: &str = "C2004A00001";

/// A deployed layout: `<base>/build` holds the module files, `<base>` the corpus.
struct Deployed {
    _dir: tempfile::TempDir,
    base: PathBuf,
    build: PathBuf,
}

fn deployed() -> Deployed {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let build = base.join("build");
    std::fs::create_dir(&build).unwrap();
    Deployed {
        _dir: dir,
        base,
        build,
    }
}

fn write_rows(d: &Deployed, rid: &str, rows: &[Value]) {
    let folder = d.base.join("markdown").join(rid);
    std::fs::create_dir_all(&folder).unwrap();
    let text: Vec<String> = rows.iter().map(Value::to_string).collect();
    std::fs::write(folder.join("sections.jsonl"), text.join("\n") + "\n\n").unwrap();
}

fn scan(d: &Deployed, stage: &str) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg(stage)
        .env("FADDEN_MODULE_DIR", &d.build)
        .output()
        .unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
    )
}

#[test]
fn the_first_scan_flags_only_a_register_shaped_title() {
    let d = deployed();
    std::fs::write(
        d.build.join("manifest_md.json"),
        json!([{"id": FLAGGED, "name": "All Caps Register", "collection": "NotifiableInstrument"}])
            .to_string(),
    )
    .unwrap();
    write_rows(
        &d,
        FLAGGED,
        &[
            json!({"row_id": "r1", "text": ALL_CAPS_ROW}),
            json!({"row_id": "r2", "text": "Signed, SMITH, John 12345678."}),
        ],
    );
    write_rows(
        &d,
        PLAIN,
        &[json!({"row_id": "r1", "text": "The Commissioner may determine."})],
    );
    let (code, stdout) = scan(&d, "pii_scan");
    assert_eq!(code, 0);
    let flagged: Value =
        serde_json::from_str(&std::fs::read_to_string(d.build.join("pii_flagged.json")).unwrap())
            .unwrap();
    let f = &flagged[0];
    assert_eq!(flagged.as_array().unwrap().len(), 1);
    assert_eq!(
        (
            &f["register_id"],
            &f["rows_total"],
            &f["rows_flagged"],
            &f["row_ids"]
        ),
        (&json!(FLAGGED), &json!(2), &json!(1), &json!(["r1"]))
    );
    assert!(stdout.contains("titles carrying person-name rows: 1"));
    assert!(
        regex::Regex::new(r"F2026N00001 +2 +1 +\d+  All Caps Register")
            .unwrap()
            .is_match(&stdout),
        "{stdout}"
    );
}

fn second_scan_inputs(d: &Deployed, entries: Value) {
    std::fs::write(
        d.build.join("pii_flagged.json"),
        json!([{"register_id": FLAGGED}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        d.build.join("pii_contact_allowlist.json"),
        json!({"schema_version": 1, "entries": entries}).to_string(),
    )
    .unwrap();
}

#[test]
fn the_second_scan_reports_weak_pairs_and_caps_its_examples() {
    let d = deployed();
    second_scan_inputs(&d, json!([]));
    let contacts: Vec<String> = (0..6).map(|n| format!("person{n}@example.test")).collect();
    write_rows(
        &d,
        FLAGGED,
        &[json!({"row_id": "known", "text": ALL_CAPS_ROW})],
    );
    write_rows(
        &d,
        PLAIN,
        &[
            json!({"row_id": "weak", "text": "Agent Jane Citizen 12345678"}),
            json!({"row_id": "contacts", "text": contacts.join(" ")}),
        ],
    );
    let (code, stdout) = scan(&d, "pii_scan2");
    assert_eq!(code, 1);
    assert!(
        regex::Regex::new(r"C2004A00001 +weak +names=1 nums=1")
            .unwrap()
            .is_match(&stdout),
        "{stdout}"
    );
    assert!(
        !regex::Regex::new(r"F2026N00001 +known")
            .unwrap()
            .is_match(&stdout)
    );
    assert!(stdout.contains("unapproved contact details: {'email': 6}"));
    assert!(!stdout.contains("@example.test"));
    // Five examples are kept and three printed, each a digest prefix.
    let examples = stdout.lines().find(|l| l.contains("sha256[:16]")).unwrap();
    assert_eq!(examples.matches("('C2004A00001', 'contacts', '").count(), 3);
}

#[test]
fn the_second_scan_passes_an_approved_contact() {
    let d = deployed();
    let contact = "official@example.test";
    let digest = pii::contact_fingerprints(contact).remove(0).1;
    second_scan_inputs(
        &d,
        json!([{"kind": "email", "sha256": digest, "register_id": PLAIN, "reason": "office"}]),
    );
    write_rows(&d, PLAIN, &[json!({"row_id": "r1", "text": contact})]);
    let (code, stdout) = scan(&d, "pii_scan2");
    assert_eq!(code, 0);
    assert!(stdout.contains("contact details across the whole corpus: {'email': 1}"));
    assert!(stdout.contains("unapproved contact details: none"));
}

#[test]
fn the_first_scan_records_the_register_title_before_finalization() {
    let d = deployed();
    std::fs::write(
        d.build.join("manifest_md.json"),
        json!([
            {"id": FLAGGED, "name": "All Caps Register", "collection": "NotifiableInstrument"},
            {"id": PLAIN, "name": "Ordinary Tax Act", "collection": "Act"},
        ])
        .to_string(),
    )
    .unwrap();
    write_rows(
        &d,
        FLAGGED,
        &[json!({"row_id": "F2026N00001-1", "text": ALL_CAPS_ROW})],
    );
    write_rows(
        &d,
        PLAIN,
        &[json!({"row_id": "C2004A00001-1", "text": "The Commissioner may determine the rate."})],
    );
    assert_eq!(scan(&d, "pii_scan").0, 0);
    let flagged: Value =
        serde_json::from_str(&std::fs::read_to_string(d.build.join("pii_flagged.json")).unwrap())
            .unwrap();
    assert_eq!(flagged.as_array().unwrap().len(), 1);
    assert_eq!(flagged[0]["register_id"], FLAGGED);
    assert_eq!(flagged[0]["name"], "All Caps Register");
    assert_eq!(flagged[0]["collection"], "NotifiableInstrument");
    assert!(flagged[0]["names_est"].as_u64().unwrap() >= 3);
}

#[test]
fn the_second_scan_does_not_copy_contact_identifiers_into_logs() {
    use sha2::{Digest, Sha256};
    let d = deployed();
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fadden")
        .join("pii_contact_allowlist.json");
    std::fs::copy(shipped, d.build.join("pii_contact_allowlist.json")).unwrap();
    std::fs::write(d.build.join("pii_flagged.json"), "[]\n").unwrap();
    let contact = "privacy-review@example.test";
    write_rows(
        &d,
        PLAIN,
        &[json!({"row_id": format!("{PLAIN}:0001:-"), "text": contact})],
    );
    let (code, logged) = scan(&d, "pii_scan2");
    assert_eq!(code, 1);
    assert!(!logged.contains(contact));
    let digest: String = Sha256::digest(contact.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(logged.contains(&digest[..16]));
    assert!(logged.contains(&format!("{PLAIN}:0001:-")));
}

#[test]
fn the_second_scan_accepts_only_a_title_bound_organisational_contact() {
    use sha2::{Digest, Sha256};
    let d = deployed();
    let contact = "OFFICIAL@example.test";
    let digest: String = Sha256::digest(contact.to_lowercase().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    second_scan_inputs(
        &d,
        json!([{"kind": "email", "sha256": digest, "register_id": PLAIN, "reason": "synthetic organisational contact"}]),
    );
    std::fs::write(d.build.join("pii_flagged.json"), "[]\n").unwrap();
    write_rows(
        &d,
        PLAIN,
        &[json!({"row_id": format!("{PLAIN}:0001:-"), "text": contact})],
    );
    assert_eq!(scan(&d, "pii_scan2").0, 0);
    // The same contact in another title is not approved.
    write_rows(&d, FLAGGED, &[json!({"row_id": "x", "text": contact})]);
    assert_eq!(scan(&d, "pii_scan2").0, 1);
}
