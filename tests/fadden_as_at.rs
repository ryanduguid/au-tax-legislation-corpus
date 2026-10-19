//! `fadden as_at`: ported from tests/corpus/test_as_at.py.
use std::path::Path;
use std::process::Command;

use tax_radar_au::fadden::as_at::{
    self, AFTER_RETRIEVAL, BEFORE_COMPILATION, CAPTURED, SUPERSEDED,
};
use tax_radar_au::pyjson::{Json, loads_standard};

fn current() -> Json {
    loads_standard(r#"{"id": "C2099A00001", "name": "Sample Act 2099", "versionStart": "2026-07-01", "retrieved": "2026-08-04"}"#).unwrap()
}

fn superseded() -> Json {
    loads_standard(
        r#"{"id": "F2099L00002", "name": "Sample Instrument 2099", "versionStart": "2023-09-01",
            "retrieved": "2026-08-04", "version_is_current": false, "current_version_start": "2026-07-07"}"#,
    )
    .unwrap()
}

/// A title with one member set (or removed when `value` is None).
fn with(title: &Json, key: &str, value: Option<&str>) -> Json {
    let Json::Object(mut pairs) = title.clone() else {
        unreachable!()
    };
    pairs.retain(|(k, _)| k.as_str() != Some(key));
    if let Some(value) = value {
        pairs.push((
            tax_radar_au::pyjson::PyStr::Text(key.into()),
            loads_standard(value).unwrap(),
        ));
    }
    Json::Object(pairs)
}

#[test]
fn a_current_title_is_captured_from_its_start_to_its_retrieval() {
    assert_eq!(as_at::classify(&current(), "2026-07-01").unwrap(), CAPTURED);
    assert_eq!(as_at::classify(&current(), "2026-08-04").unwrap(), CAPTURED);
}

#[test]
fn the_day_before_the_compilation_is_outside_it() {
    assert_eq!(
        as_at::classify(&current(), "2026-06-30").unwrap(),
        BEFORE_COMPILATION
    );
    assert_eq!(
        as_at::classify(&superseded(), "2023-08-31").unwrap(),
        BEFORE_COMPILATION
    );
}

#[test]
fn the_day_after_retrieval_is_after_retrieval() {
    assert_eq!(
        as_at::classify(&current(), "2026-08-05").unwrap(),
        AFTER_RETRIEVAL
    );
}

#[test]
fn a_superseded_title_stops_the_day_its_successor_starts_and_stays_superseded() {
    assert_eq!(
        as_at::classify(&superseded(), "2026-07-06").unwrap(),
        CAPTURED
    );
    assert_eq!(
        as_at::classify(&superseded(), "2026-07-07").unwrap(),
        SUPERSEDED
    );
    assert_eq!(
        as_at::classify(&superseded(), "2027-01-01").unwrap(),
        SUPERSEDED
    );
}

#[test]
fn dates_that_cannot_all_be_true_are_refused() {
    let broken = [
        with(&current(), "versionStart", Some("\"2026-99-01\"")),
        with(&current(), "retrieved", Some("\"4 August 2026\"")),
        with(&current(), "versionStart", Some("\"2026-09-01\"")),
        with(
            &superseded(),
            "current_version_start",
            Some("\"2023-09-01\""),
        ),
        with(
            &superseded(),
            "current_version_start",
            Some("\"2026-09-01\""),
        ),
        with(&superseded(), "current_version_start", None),
        with(&superseded(), "version_is_current", Some("\"false\"")),
        with(&current(), "version_is_current", Some("null")),
    ];
    for title in broken {
        let error = as_at::classify(&title, "2026-07-15").unwrap_err();
        assert!(error.is_value(), "{error}");
    }
}

#[test]
fn the_committed_manifest_is_consistent() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fadden")
        .join("manifest_md.json");
    let Json::Array(titles) = loads_standard(&std::fs::read_to_string(path).unwrap()).unwrap()
    else {
        panic!("the manifest is a list");
    };
    let mut statuses: Vec<&str> = titles
        .iter()
        .map(|t| as_at::classify(t, "2026-08-01").unwrap())
        .collect();
    statuses.sort_unstable();
    statuses.dedup();
    assert_eq!(statuses, [CAPTURED, SUPERSEDED]);
}

const ROW: &str = "(1) You can deduct from your assessable income any loss or outgoing";

#[test]
fn quotations_match_word_for_word_up_to_nfkc() {
    assert!(as_at::quote_in_text("deduct from your assessable income", ROW).unwrap());
    assert!(!as_at::quote_in_text("deduct from assessable income", ROW).unwrap());
    assert!(!as_at::quote_in_text("Deduct from your assessable income", ROW).unwrap());
    let row = "section 8\u{2011}1 of the Income Tax Assessment Act 1997";
    assert!(as_at::quote_in_text("section 8-1 of the Income Tax Assessment Act", row).unwrap());
    // Documented, not accidental: a ligature in the row matches plain letters.
    let row = "deduct from your \u{fb01}nancial year income";
    assert!(as_at::quote_in_text("deduct from your financial year", row).unwrap());
    for quote in ["", "   "] {
        assert!(as_at::quote_in_text(quote, ROW).is_err());
    }
}

fn run(args: &[&str], manifest: &[Json]) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest_md.json");
    let text = format!(
        "[{}]",
        manifest
            .iter()
            .map(|t| tax_radar_au::pyjson::dumps(t, tax_radar_au::pyjson::Layout::Canonical))
            .collect::<Vec<_>>()
            .join(", ")
    );
    std::fs::write(&path, text).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("as_at")
        .args(args)
        .arg("--manifest")
        .arg(&path)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn the_summary_counts_every_status() {
    let (code, out, _) = run(&["2026-07-10"], &[current(), superseded()]);
    assert_eq!(code, 0);
    assert!(out.contains("captured: 1"));
    assert!(out.contains("superseded: 1 (a later version applies; read the Register)"));
    assert!(out.contains("not a finding that the text was the law then"));
}

#[test]
fn json_names_each_title_and_its_window() {
    let (code, out, _) = run(&["2025-01-01", "--json"], &[superseded()]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "{\"register_id\": \"F2099L00002\", \"name\": \"Sample Instrument 2099\", \"status\": \"captured\", \
         \"compilation_date\": \"2023-09-01\", \"superseded_from\": \"2026-07-07\", \"retrieved\": \"2026-08-04\"}\n"
    );
}

#[test]
fn an_inconsistent_manifest_exits_one_with_the_reason() {
    let (code, out, err) = run(
        &["2026-07-10"],
        &[with(&current(), "retrieved", Some("\"bad\""))],
    );
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("C2099A00001: retrieved is not a YYYY-MM-DD date"));
}

#[test]
fn a_date_that_is_not_yyyy_mm_dd_is_a_usage_error() {
    for date in ["10/07/2026", "20260710", "2026-W28-5", "2026-7-10"] {
        let (code, _, err) = run(&[date], &[current()]);
        assert_eq!(code, 2, "{date}");
        assert!(err.contains("date must be YYYY-MM-DD"));
    }
}
