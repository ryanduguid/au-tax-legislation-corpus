//! `fadden compare_builds`: ported from tests/corpus/test_compare_builds.py.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::compare_builds;
use tax_radar_au::pyjson::{Json, Layout, dumps, loads_standard};

const ACT: &str = "C2099A00001";
const INSTRUMENT: &str = "F2099L00002";

struct Row<'a> {
    rid: &'a str,
    ordinal: u32,
    section: Option<&'a str>,
    text: &'a str,
    heading: &'a str,
    container: Option<&'a str>,
    compilation: u32,
}

fn row(rid: &str, ordinal: u32, section: Option<&str>, text: &str) -> Value {
    make(Row {
        rid,
        ordinal,
        section,
        text,
        heading: "Heading",
        container: None,
        compilation: 1,
    })
}

fn make(r: Row) -> Value {
    json!({
        "register_id": r.rid, "act": "Sample Act 2099", "compilation_number": r.compilation.to_string(),
        "compilation_date": format!("2026-0{}-01", r.compilation),
        "row_id": format!("{}:{:04}:{}", r.rid, r.ordinal, r.section.unwrap_or("-")),
        "section": r.section, "heading": r.heading, "container": r.container, "text": r.text,
    })
}

fn write_build(root: &Path, titles: &[(&str, Vec<Value>)]) {
    for (rid, rows) in titles {
        let directory = root.join("markdown").join(rid);
        std::fs::create_dir_all(&directory).unwrap();
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(directory.join("sections.jsonl"), text).unwrap();
    }
}

fn as_json(rows: &[Value]) -> Vec<Json> {
    rows.iter()
        .map(|r| loads_standard(&r.to_string()).unwrap())
        .collect()
}

fn report(old: &[Value], new: &[Value]) -> Value {
    let built = compare_builds::compare_title(&as_json(old), &as_json(new)).unwrap();
    serde_json::from_str(&dumps(&built, Layout::Canonical)).unwrap()
}

fn sections(places: &Value, field: &str) -> Vec<Value> {
    places
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[field].clone())
        .collect()
}

struct Builds {
    _dir: tempfile::TempDir,
    old: PathBuf,
    new: PathBuf,
}

fn builds() -> Builds {
    let dir = tempfile::tempdir().unwrap();
    Builds {
        old: dir.path().join("old"),
        new: dir.path().join("new"),
        _dir: dir,
    }
}

fn run(b: &Builds, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("compare_builds")
        .arg(&b.old)
        .arg(&b.new)
        .args(args)
        .output()
        .unwrap();
    let text = |x: &[u8]| String::from_utf8_lossy(x).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn an_inserted_section_does_not_shift_its_neighbours_into_changes() {
    let b = builds();
    let new_row = |ordinal, section, text| {
        make(Row {
            rid: ACT,
            ordinal,
            section: Some(section),
            text,
            heading: "Heading",
            container: None,
            compilation: 2,
        })
    };
    write_build(
        &b.old,
        &[(
            ACT,
            vec![row(ACT, 1, Some("1"), "one"), row(ACT, 2, Some("2"), "two")],
        )],
    );
    write_build(
        &b.new,
        &[(
            ACT,
            vec![
                new_row(1, "1", "one"),
                new_row(2, "1A", "inserted"),
                new_row(3, "2", "two"),
            ],
        )],
    );
    let old = compare_builds::load_rows(&b.old.to_string_lossy(), ACT).unwrap();
    let new = compare_builds::load_rows(&b.new.to_string_lossy(), ACT).unwrap();
    let r: Value = serde_json::from_str(&dumps(
        &compare_builds::compare_title(&old, &new).unwrap(),
        Layout::Canonical,
    ))
    .unwrap();
    assert_eq!(sections(&r["added"], "section"), [json!("1A")]);
    assert_eq!(r["removed"], json!([]));
    assert_eq!(r["changed"], json!([]));
    assert_eq!(
        r["old_compilation"],
        json!({"number": "1", "date": "2026-01-01"})
    );
    assert_eq!(
        r["new_compilation"],
        json!({"number": "2", "date": "2026-02-01"})
    );
}

#[test]
fn text_and_heading_changes_are_named() {
    let renamed = |ordinal, section, text| {
        make(Row {
            rid: ACT,
            ordinal,
            section: Some(section),
            text,
            heading: "Renamed",
            container: None,
            compilation: 1,
        })
    };
    let r = report(
        &[
            row(ACT, 1, Some("1"), "one"),
            row(ACT, 2, Some("2"), "two"),
            row(ACT, 3, Some("3"), "three"),
            row(ACT, 4, Some("4"), "gone"),
        ],
        &[
            row(ACT, 1, Some("1"), "one amended"),
            renamed(2, "2", "two"),
            renamed(3, "3", "three amended"),
        ],
    );
    let changes: Vec<(Value, Value)> = r["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["section"].clone(), p["change"].clone()))
        .collect();
    assert_eq!(
        changes,
        [
            (json!("1"), json!("text")),
            (json!("2"), json!("heading")),
            (json!("3"), json!("text and heading"))
        ]
    );
    assert_eq!(sections(&r["removed"], "section"), [json!("4")]);
}

#[test]
fn schedules_restart_numbering_and_repeated_labels_match_in_order() {
    let schedule = |ordinal, text| {
        make(Row {
            rid: ACT,
            ordinal,
            section: Some("1"),
            text,
            heading: "Heading",
            container: Some("Schedule 1"),
            compilation: 1,
        })
    };
    let r = report(
        &[
            row(ACT, 1, Some("1"), "body one"),
            schedule(2, "schedule one"),
            schedule(3, "schedule one again"),
        ],
        &[
            row(ACT, 1, Some("1"), "body one"),
            schedule(2, "schedule one"),
            schedule(3, "schedule one again, amended"),
        ],
    );
    assert_eq!(r["changed"].as_array().unwrap().len(), 1);
    assert_eq!(r["changed"][0]["container"], "Schedule 1");
    assert_eq!(
        (r["added"].clone(), r["removed"].clone()),
        (json!([]), json!([]))
    );
}

#[test]
fn rows_without_a_section_label_match_on_their_heading() {
    let headed = |ordinal, text, heading| {
        make(Row {
            rid: ACT,
            ordinal,
            section: None,
            text,
            heading,
            container: None,
            compilation: 1,
        })
    };
    let r = report(
        &[headed(1, "intro", "Introductory material")],
        &[
            headed(1, "intro amended", "Introductory material"),
            headed(2, "table", "Table of rates"),
        ],
    );
    assert_eq!(
        sections(&r["changed"], "heading"),
        [json!("Introductory material")]
    );
    assert_eq!(sections(&r["added"], "heading"), [json!("Table of rates")]);
}

#[test]
fn text_report_lists_changes_and_titles_in_one_build() {
    let b = builds();
    write_build(
        &b.old,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "one")]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let amended = make(Row {
        rid: ACT,
        ordinal: 1,
        section: Some("1"),
        text: "one amended",
        heading: "Heading",
        container: None,
        compilation: 2,
    });
    write_build(
        &b.new,
        &[
            (ACT, vec![amended]),
            ("C2099A00003", vec![row("C2099A00003", 1, Some("1"), "new")]),
        ],
    );
    let (code, out, err) = run(&b, &[]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("C2099A00001: compilation 1 (2026-01-01) to 2 (2026-02-01)"));
    assert!(out.contains("  changed (text): s 1"));
    assert!(out.contains("1 titles compared, 1 with section changes; 1 only in the old build, 1 only in the new build."));
    assert!(out.contains("read both compilations on the Register"));
}

#[test]
fn json_and_title_filter() {
    let b = builds();
    write_build(
        &b.old,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "one")]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let reheaded = make(Row {
        rid: ACT,
        ordinal: 1,
        section: Some("1"),
        text: "one",
        heading: "New heading",
        container: None,
        compilation: 1,
    });
    write_build(
        &b.new,
        &[
            (ACT, vec![reheaded]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let (code, out, _) = run(&b, &["--title", ACT, "--json"]);
    assert_eq!(code, 0);
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines
            .iter()
            .map(|l| l["register_id"].clone())
            .collect::<Vec<_>>(),
        [json!(ACT)]
    );
    assert_eq!(lines[0]["changed"][0]["change"], "heading");
}

#[test]
fn json_lists_titles_present_in_only_one_build() {
    let b = builds();
    write_build(&b.old, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    write_build(
        &b.new,
        &[(INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")])],
    );
    let (code, out, _) = run(&b, &["--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "{\"register_id\": \"C2099A00001\", \"only_in\": \"old\"}\n{\"register_id\": \"F2099L00002\", \"only_in\": \"new\"}\n"
    );
}

#[test]
fn unusable_input_returns_one_with_the_reason() {
    let b = builds();
    write_build(&b.old, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    write_build(&b.new, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    for (message, args) in [
        ("not in both builds", ["--title", INSTRUMENT]),
        (
            "invalid Federal Register identifier",
            ["--title", "../escape"],
        ),
    ] {
        let (code, out, err) = run(&b, &args);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains(message), "{err}");
    }
    std::fs::write(
        b.new.join("markdown").join(ACT).join("sections.jsonl"),
        "[\"not a row\"]\n",
    )
    .unwrap();
    let (code, _, err) = run(&b, &[]);
    assert_eq!(code, 1);
    assert!(err.contains("is not a sections.jsonl row"));
}

#[test]
fn directories_that_are_not_titles_are_ignored() {
    let b = builds();
    write_build(&b.new, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    std::fs::create_dir(b.new.join("markdown").join("notes")).unwrap();
    std::fs::create_dir(b.new.join("markdown").join(INSTRUMENT)).unwrap();
    let found = compare_builds::titles(&b.new.to_string_lossy()).unwrap();
    assert_eq!(found.into_iter().collect::<Vec<_>>(), [ACT]);
}

#[test]
fn blank_lines_between_rows_are_skipped() {
    let b = builds();
    let directory = b.old.join("markdown").join(ACT);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("sections.jsonl"),
        format!("\n{}\n  \n", row(ACT, 1, Some("1"), "one")),
    )
    .unwrap();
    let rows = compare_builds::load_rows(&b.old.to_string_lossy(), ACT).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("text").and_then(Json::as_str), Some("one"));
}

#[test]
fn an_empty_title_has_no_compilation() {
    let r = report(&[], &[row(ACT, 1, Some("1"), "one")]);
    assert_eq!(r["old_compilation"], Value::Null);
    assert_eq!(sections(&r["added"], "section"), [json!("1")]);
}

#[test]
fn text_report_lists_added_and_removed_sections_and_skips_unchanged_titles() {
    let b = builds();
    write_build(
        &b.old,
        &[
            (
                ACT,
                vec![
                    row(ACT, 1, Some("1"), "one"),
                    row(ACT, 2, Some("2"), "gone"),
                ],
            ),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let table = make(Row {
        rid: ACT,
        ordinal: 2,
        section: None,
        text: "new",
        heading: "Table",
        container: Some("Part 2"),
        compilation: 1,
    });
    write_build(
        &b.new,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "one"), table]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let (code, out, err) = run(&b, &[]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("  added: Part 2, Table"));
    assert!(out.contains("  removed: s 2"));
    assert!(!out.contains(&format!("{INSTRUMENT}:")));
    assert!(out.contains("2 titles compared, 1 with section changes;"));
}

#[test]
fn python_value_semantics_decide_matches_and_failures() {
    // A container of 1 and of true are one dict key in Python; a list is unhashable.
    let mut a = row(ACT, 1, Some("1"), "one");
    a["container"] = json!(1);
    let mut b_row = row(ACT, 1, Some("1"), "one");
    b_row["container"] = json!(true);
    let r = report(&[a.clone()], &[b_row]);
    assert_eq!(
        (r["added"].clone(), r["removed"].clone()),
        (json!([]), json!([]))
    );
    a["container"] = json!(["x"]);
    let error = compare_builds::compare_title(&as_json(&[a]), &[]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot use 'tuple' as a dict key (unhashable type: 'list')"
    );
}
