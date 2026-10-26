//! `fadden search`: ported from tests/corpus/test_search.py. Every title, row
//! and definition is fabricated.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::search;
use tax_radar_au::pyjson::{Json, Layout, dumps, loads_standard};

const ACT: &str = "C2099A00001";
const OTHER: &str = "C2099A00002";
const INSTRUMENT: &str = "F2099L00003";
const NBH: &str = "\u{2011}";

fn dictionary() -> String {
    [
        "(1) In this Act:".to_string(),
        format!("*widget has the meaning given by section 2{NBH}1."),
        "approved gadget means a gadget that:".into(),
        "- (a) is listed in the register; and".into(),
        "- (b) is not a *widget.".into(),
        "but does not include a toy.".into(),
        format!("R&D entity has the meaning given by section 3{NBH}1."),
        "lodgment day: a day on which the office is open.".into(),
        "Note 1: This note is not a definition.".into(),
        "However: this is not one either.".into(),
        "sample levy, in relation to a widget, means the levy imposed by this Act.".into(),
        "\u{201c}quoted term\u{201d} includes a term in curly quotes.".into(),
        "(2) In this Act, a reference to a gadget means one in Australia.".into(),
    ]
    .join("\n\n")
}

fn row(
    rid: &str,
    ordinal: u32,
    section: Option<&str>,
    text: &str,
    heading: &str,
    container: Option<&str>,
) -> Value {
    json!({
        "register_id": rid, "act": "fabricated", "collection": "Act",
        "compilation_number": "1", "compilation_date": "2099-07-01",
        "version_is_current": true,
        "row_id": format!("{rid}:{ordinal:04}:{}", section.unwrap_or("-")),
        "section": section, "heading": heading, "container": container,
        "kind": "section", "granularity": "section", "text": text,
        "source_url": format!("https://example.invalid/{rid}.epub"),
        "register_page": format!("https://example.invalid/{rid}"),
        "authorised": false,
    })
}

type Titles = BTreeMap<&'static str, (&'static str, Vec<Value>)>;
type Mutation = Box<dyn Fn(&mut Titles)>;
type Cited = (String, String, Option<String>);
type Counts = &'static [(&'static str, i64)];

fn sample_titles() -> Titles {
    let mut titles = BTreeMap::new();
    titles.insert(ACT, ("Sample Levy Act 2099", vec![
        row(ACT, 1, Some("1"), "This Act is the Sample Levy Act 2099. Divisions 2 and 3 apply, and Subdivision 2-A sets the rules.", "1 Short title", None),
        row(ACT, 2, Some(&format!("2{NBH}1")), &format!("A widget attracts the sample levy. See section 3{NBH}1 and section 5 of the Other Sample Act 2099."), &format!("2{NBH}1 Widgets"), None),
        row(ACT, 3, Some(&format!("3{NBH}1")), &format!("Research entities are covered by paragraph 2{NBH}1(1)(a) of this Act and section 9 of the ITAA 1997. The \u{fb01}nancial year matters."), &format!("3{NBH}1 Research"), None),
        row(ACT, 4, Some("4"), &dictionary(), "4 Definitions", None),
        row(ACT, 5, Some("1"), "A gadget means a thing in this schedule only.", "1 Schedule item", Some("Schedule 1")),
    ]));
    titles.insert(OTHER, ("Other Sample Act 2099", vec![
        row(OTHER, 1, Some("5"), &format!("Refer to section 2{NBH}1 of the Sample Levy Act 2099. See Subdivision 2-A of the Sample Levy Act 2099."), "5 Cross reference", None),
        row(OTHER, 2, Some("6"), "widget means a small device.", "6 Interpretation", None),
    ]));
    titles.insert(
        INSTRUMENT,
        (
            "Sample Levy Rules 2099",
            vec![row(
                INSTRUMENT,
                1,
                Some("1"),
                "Section 1 of the Act and section 1 in Schedule 1 apply.",
                "1 Application",
                None,
            )],
        ),
    );
    titles
}

fn act_structure() -> BTreeMap<usize, Vec<String>> {
    BTreeMap::from([
        (1, vec!["## Part 1\u{2014}Preliminary".to_string()]),
        (
            2,
            vec![
                "## Part 2\u{2014}Levy".into(),
                "### Division 2\u{2014}Widgets".into(),
                format!("#### Subdivision 2{NBH}A\u{2014}Basic rules"),
            ],
        ),
        (
            3,
            vec![
                "### Division 3\u{2014}Research".into(),
                "#### Subdivision B\u{2014}Lettered".into(),
            ],
        ),
        (4, vec!["### Division 6.1\u{2014}Decimal".into()]),
        (5, vec!["## Schedule 1\u{2014}Items".into()]),
    ])
}

fn markdown(name: &str, rows: &[Value], structure: &BTreeMap<usize, Vec<String>>) -> String {
    let mut lines = vec![format!("# {name}")];
    for (ordinal, entry) in rows.iter().enumerate().map(|(i, r)| (i + 1, r)) {
        lines.extend(structure.get(&ordinal).cloned().unwrap_or_default());
        lines.push(format!("##### {}", entry["heading"].as_str().unwrap()));
        lines.push(entry["text"].as_str().unwrap().to_string());
    }
    lines.join("\n\n") + "\n"
}

fn write_corpus(root: &Path, titles: &Titles, jsonl_rows: &[(&str, i64)]) {
    let mut listed = Vec::new();
    for (rid, (name, rows)) in titles {
        let directory = root.join("markdown").join(rid);
        std::fs::create_dir_all(&directory).unwrap();
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(directory.join("sections.jsonl"), text).unwrap();
        let structure = if *rid == ACT {
            act_structure()
        } else {
            BTreeMap::new()
        };
        std::fs::write(
            directory.join(format!("{rid}.md")),
            markdown(name, rows, &structure),
        )
        .unwrap();
        let count = jsonl_rows
            .iter()
            .find(|(r, _)| r == rid)
            .map_or(rows.len() as i64, |(_, n)| *n);
        listed.push(json!({
            "register_id": rid, "name": name, "collection": "Act",
            "compilation_number": "1", "compilation_date": "2099-07-01",
            "version_is_current": *rid != OTHER, "retrieved": "2099-08-01",
            "jsonl_rows": count,
            "register_page": format!("https://example.invalid/{rid}"),
            "source_url": format!("https://example.invalid/{rid}.epub"),
        }));
    }
    std::fs::write(
        root.join("sources.json"),
        json!({"retrieved": "2099-08-01", "titles": listed}).to_string(),
    )
    .unwrap();
}

struct Case {
    _dir: tempfile::TempDir,
    root: PathBuf,
    out: PathBuf,
}

fn case() -> Case {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("corpus");
    std::fs::create_dir(&root).unwrap();
    let out = dir.path().join("index.sqlite");
    Case {
        root,
        out,
        _dir: dir,
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn build(c: &Case) -> [usize; 4] {
    search::build(&s(&c.root), &s(&c.out)).unwrap()
}

fn built(c: &Case) -> rusqlite::Connection {
    write_corpus(&c.root, &sample_titles(), &[]);
    build(c);
    search::open_index(&s(&c.out)).unwrap()
}

fn value(json: &Json) -> Value {
    serde_json::from_str(&dumps(json, Layout::Canonical)).unwrap()
}

fn values(items: Vec<Json>) -> Vec<Value> {
    items.iter().map(value).collect()
}

fn refused(c: &Case, pattern: &str) {
    let error = search::build(&s(&c.root), &s(&c.out))
        .unwrap_err()
        .to_string();
    assert!(
        regex::Regex::new(pattern).unwrap().is_match(&error),
        "{error:?} !~ {pattern:?}"
    );
    assert!(!c.out.exists());
    let names: Vec<String> = std::fs::read_dir(c.out.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["corpus"]);
}

#[test]
fn build_records_counts_and_source_digests() {
    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    let counts = search::build(&s(&c.root), &s(&c.out)).unwrap();
    assert_eq!((counts[0], counts[1]), (3, 8));
    let connection = search::open_index(&s(&c.out)).unwrap();
    let meta: BTreeMap<String, String> = connection
        .prepare("SELECT key, value FROM meta")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    use sha2::Digest;
    let digest = |p: &Path| -> String {
        sha2::Sha256::digest(std::fs::read(p).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    assert_eq!(meta["sources_sha256"], digest(&c.root.join("sources.json")));
    assert_eq!(meta["corpus_retrieved"], "2099-08-01");
    let sections: String = connection
        .query_row(
            "SELECT sections_sha256 FROM titles WHERE register_id = ?",
            [ACT],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        sections,
        digest(&c.root.join("markdown").join(ACT).join("sections.jsonl"))
    );
    let hidden: Vec<String> = std::fs::read_dir(c.out.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(hidden.is_empty());
}

#[test]
fn an_existing_destination_or_missing_parent_is_refused() {
    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    std::fs::write(&c.out, b"earlier index").unwrap();
    let error = search::build(&s(&c.root), &s(&c.out))
        .unwrap_err()
        .to_string();
    assert!(error.contains("must not already exist"));
    assert_eq!(std::fs::read(&c.out).unwrap(), b"earlier index");
    let absent = c.out.parent().unwrap().join("absent").join("index.sqlite");
    assert!(
        search::build(&s(&c.root), &s(&absent))
            .unwrap_err()
            .to_string()
            .contains("parent directory")
    );
}

#[test]
fn integrity_failures_are_refused_before_anything_is_written() {
    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    std::fs::create_dir(c.root.join("markdown").join("C2099A00009")).unwrap();
    refused(&c, "C2099A00009");

    let cases: Vec<(Mutation, Counts, &str)> = vec![
        (
            Box::new(|t: &mut Titles| {
                let first = t.get(OTHER).unwrap().1[0].clone();
                t.get_mut(OTHER).unwrap().1 = vec![first.clone(), first];
            }),
            &[],
            "expected 'C2099A00002:0002:5'",
        ),
        (
            Box::new(|_t: &mut Titles| {}),
            &[(ACT, 4)],
            "holds 5 rows; sources.json records 4",
        ),
        (
            Box::new(|t: &mut Titles| t.get_mut(OTHER).unwrap().1[0]["register_id"] = json!(ACT)),
            &[],
            "names another title",
        ),
        (
            Box::new(|t: &mut Titles| {
                t.get_mut(OTHER).unwrap().1[1]["compilation_number"] = json!("2")
            }),
            &[],
            "compilation_number differs",
        ),
        (
            Box::new(|t: &mut Titles| {
                t.get_mut(OTHER).unwrap().1[1]
                    .as_object_mut()
                    .unwrap()
                    .remove("authorised");
            }),
            &[],
            "authorised: false",
        ),
    ];
    for (mutate, rows, pattern) in cases {
        let c = case();
        let mut titles = sample_titles();
        mutate(&mut titles);
        write_corpus(&c.root, &titles, rows);
        refused(&c, pattern);
    }
}

#[test]
fn malformed_unreadable_or_foreign_files_are_refused() {
    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    let path = c.root.join("markdown").join(OTHER).join("sections.jsonl");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{original}{{not json\n")).unwrap();
    refused(&c, "sections.jsonl:3 is not JSON");
    std::fs::write(&path, b"\n[1]\n").unwrap();
    refused(&c, "sections.jsonl:2 is not a sections.jsonl row");
    std::fs::write(&path, b"\xff\n").unwrap();
    refused(&c, "is not UTF-8");
    std::fs::remove_file(&path).unwrap();
    refused(&c, "cannot read sections.jsonl");

    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    std::fs::remove_file(
        c.root
            .join("markdown")
            .join(OTHER)
            .join(format!("{OTHER}.md")),
    )
    .unwrap();
    refused(&c, &format!("cannot read {OTHER}.md"));
    std::fs::write(c.root.join("sources.json"), r#"{"titles": {}}"#).unwrap();
    refused(&c, "not a finished corpus inventory");
    std::fs::remove_file(c.root.join("sources.json")).unwrap();
    refused(&c, "cannot read sources.json");
}

#[test]
fn query_folds_compatibility_forms_and_ranks_section_matches_first() {
    let c = case();
    let connection = built(&c);
    let ids = |words: &[&str]| -> Vec<Value> {
        values(
            search::query(
                &connection,
                &words.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
                None,
                10,
            )
            .unwrap(),
        )
    };
    assert!(
        ids(&["financial"])
            .iter()
            .any(|h| h["row_id"] == format!("{ACT}:0003:3{NBH}1"))
    );
    assert_eq!(ids(&["2-1"])[0]["row_id"], format!("{ACT}:0002:2{NBH}1"));
    let hits = ids(&["3-1"]);
    assert_eq!(hits[0]["section"], format!("3{NBH}1"));
    assert!(hits.len() > 1);
    for words in [
        vec!["AND("],
        vec!["\"widget"],
        vec!["widget*"],
        vec!["NEAR(widget", "levy)"],
    ] {
        ids(&words);
    }
    let error = search::query(&connection, &["()".into(), "\"".into()], None, 10)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no searchable words"));
}

#[test]
fn hits_carry_provenance_and_respect_title_and_limit() {
    let c = case();
    let connection = built(&c);
    let hits = values(search::query(&connection, &["widget".into()], Some(OTHER), 10).unwrap());
    assert!(hits.iter().all(|h| h["register_id"] == OTHER));
    assert_eq!(hits[0]["authorised"], false);
    assert_eq!(hits[0]["version_is_current"], false);
    for field in [
        "act",
        "compilation_number",
        "compilation_date",
        "retrieved",
        "register_page",
        "source_url",
        "sections_sha256",
        "snippet",
        "text",
    ] {
        assert!(hits[0].get(field).is_some(), "{field}");
    }
    assert_eq!(
        search::query(&connection, &["widget".into()], None, 1)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn definitions_split_on_their_opening_words() {
    let found = search::definitions(&dictionary());
    let terms: Vec<&str> = found.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        terms,
        [
            "widget",
            "approved gadget",
            "R&D entity",
            "lodgment day",
            "sample levy",
            "quoted term"
        ]
    );
    let approved = &found[1].1;
    assert_eq!(
        approved.split("\n\n").collect::<Vec<_>>(),
        [
            "approved gadget means a gadget that:",
            "- (a) is listed in the register; and",
            "- (b) is not a *widget.",
            "but does not include a toy."
        ]
    );
    assert!(found[3].1.starts_with("lodgment day: a day"));
    assert!(!found[5].1.contains("(2)"));
    assert_eq!(
        search::definitions(
            "In this Act:\n\n- (a) stray item\n\n\n\nplain text\n\nthing means an item."
        ),
        [("thing".to_string(), "thing means an item.".to_string())]
    );
    let lead = "In this Division, unless the contrary intention appears, thing means an item.";
    assert_eq!(
        search::definitions(lead),
        [("thing".to_string(), lead.to_string())]
    );
}

#[test]
fn define_matches_folded_terms_and_prefixes() {
    let c = case();
    let connection = built(&c);
    let found = values(search::define(&connection, "WIDGET", None).unwrap());
    let places: Vec<(Value, Value)> = found
        .iter()
        .map(|d| (d["register_id"].clone(), d["section"].clone()))
        .collect();
    assert_eq!(
        places,
        [(json!(ACT), json!("4")), (json!(OTHER), json!("6"))]
    );
    assert_eq!(found[0]["term"], "widget");
    let terms = |t: &str, title: Option<&str>| -> Vec<Value> {
        values(search::define(&connection, t, title).unwrap())
            .into_iter()
            .map(|d| d["term"].clone())
            .collect()
    };
    assert_eq!(terms("r&d entity", None), [json!("R&D entity")]);
    assert_eq!(
        values(search::define(&connection, "widget", Some(OTHER)).unwrap()).len(),
        1
    );
    assert_eq!(terms("approved", None), [json!("approved gadget")]);
    assert!(terms("approved_", None).is_empty());
    assert!(terms("approved%", None).is_empty());
    assert!(terms("gadget", None).is_empty());
}

fn cited(text: &str) -> Vec<Cited> {
    search::citations(text)
        .into_iter()
        .map(|(k, key, act, _)| (k, key, act))
        .collect()
}

fn t(kind: &str, key: &str, act: Option<&str>) -> Cited {
    (kind.into(), key.into(), act.map(str::to_string))
}

#[test]
fn citations_name_the_section_and_any_other_instrument() {
    let text = format!(
        "section 8{NBH}1, paragraph 8-1(2)(a) of this Act, section 5 of the Other Sample Act 2099, subsection 995-1(1) of the *ITAA 1997 but includes \
         the rest, section 12-35 in Schedule 1 to the Taxation Administration Act 1953, section 2 of the A New Tax System (Goods and Services Tax) Act 1999, \
         section 3 of the Act (with changes), section 4 in Schedule 2 and subsection (3)."
    );
    let got: Vec<(String, Option<String>)> =
        cited(&text).into_iter().map(|(_, k, a)| (k, a)).collect();
    let expected: Vec<(String, Option<String>)> = [
        ("8-1", None),
        ("8-1", None),
        ("5", Some("Other Sample Act 2099")),
        ("995-1", Some("ITAA 1997")),
        ("12-35", Some("Taxation Administration Act 1953")),
        (
            "2",
            Some("A New Tax System (Goods and Services Tax) Act 1999"),
        ),
        ("3", Some("Act")),
        ("4", None),
    ]
    .into_iter()
    .map(|(k, a)| (k.to_string(), a.map(str::to_string)))
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn lists_and_lookalikes_are_read_as_python_read_them() {
    let cases: Vec<(String, Vec<Cited>)> = vec![
        (
            "sections 8-1 and 8-5 apply".into(),
            vec![t("section", "8-1", None), t("section", "8-5", None)],
        ),
        (
            format!("sections 8{NBH}1, 8{NBH}5 and 8{NBH}10 of the ITAA 1997."),
            vec![
                t("section", "8-1", Some("ITAA 1997")),
                t("section", "8-5", Some("ITAA 1997")),
                t("section", "8-10", Some("ITAA 1997")),
            ],
        ),
        (
            "subsections 6-5(2), 6-10(4), or 6-15(1)".into(),
            vec![
                t("section", "6-5", None),
                t("section", "6-10", None),
                t("section", "6-15", None),
            ],
        ),
        (
            "Section 23 and section 23AA".into(),
            vec![t("section", "23", None), t("section", "23AA", None)],
        ),
        (
            "Divisions 40, 43 or 83A of this Act".into(),
            vec![
                t("division", "40", None),
                t("division", "43", None),
                t("division", "83A", None),
            ],
        ),
        (
            format!("Subdivisions 20-A and 20{NBH}B"),
            vec![
                t("subdivision", "20-A", None),
                t("subdivision", "20-B", None),
            ],
        ),
        (
            "Division 40 and Subdivision 20-A".into(),
            vec![t("division", "40", None), t("subdivision", "20-A", None)],
        ),
        ("section 10.".into(), vec![t("section", "10", None)]),
        ("section 8-10.".into(), vec![t("section", "8-10", None)]),
        (
            "section 123 and Division 10A".into(),
            vec![t("section", "123", None), t("division", "10A", None)],
        ),
        (
            "sections 8-1 to 8-10".into(),
            vec![t("section", "8-1", None)],
        ),
        (
            "section 8-1 and 2 other provisions".into(),
            vec![t("section", "8-1", None)],
        ),
        (
            "section 23 and 2-1 apply".into(),
            vec![t("section", "23", None)],
        ),
        (
            "subsections 8-1(1) and (2)".into(),
            vec![t("section", "8-1", None)],
        ),
        (
            "section 8-1, the Commissioner".into(),
            vec![t("section", "8-1", None)],
        ),
        (
            "Subdivision B of Division 3".into(),
            vec![t("division", "3", None)],
        ),
        ("Division 6.1 applies".into(), vec![]),
        (
            "Division 8-1, Subdivision 40 and section 20-A".into(),
            vec![],
        ),
        ("this Division, the Subdivision and Part 3-1".into(), vec![]),
        ("the divisional manager 4 and subsectional 5".into(), vec![]),
    ];
    for (text, expected) in cases {
        assert_eq!(cited(&text), expected, "{text}");
    }
    for keyword in ["section", "Division"] {
        for label in ["10.1", "123.45", "10A.1"] {
            assert!(
                cited(&format!("{keyword} {label} applies.")).is_empty(),
                "{keyword} {label}"
            );
        }
    }
    for label in ["8-10.1".to_string(), format!("8{NBH}10.1")] {
        assert!(cited(&format!("section {label} applies.")).is_empty());
    }
}

#[test]
fn structure_follows_the_markdown_headings() {
    let c = case();
    let connection = built(&c);
    let places: Vec<(Option<String>, Option<String>)> = connection
        .prepare("SELECT division_key, subdivision_key FROM rows WHERE register_id = ? ORDER BY id")
        .unwrap()
        .query_map([ACT], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let o = |d: Option<&str>, s: Option<&str>| (d.map(str::to_string), s.map(str::to_string));
    assert_eq!(
        places,
        [
            o(None, None),
            o(Some("2"), Some("2-A")),
            o(Some("3"), None),
            o(None, None),
            o(None, None)
        ]
    );
    let rows: Vec<Json> = ["Introductory material", "1  Short\ntitle", "1 Short title"]
        .iter()
        .map(|h| loads_standard(&json!({"heading": h}).to_string()).unwrap())
        .collect();
    let (found, failure) =
        search::structure(&rows, "### Division 7A\u{2014}Loans\n##### 1 Short title\n");
    assert!(failure.is_none());
    assert_eq!(found, [o(None, None), o(Some("7A"), None), o(None, None)]);
}

#[test]
fn refs_resolve_within_and_across_titles() {
    let c = case();
    let connection = built(&c);
    let report = value(&search::refs(&connection, &format!("{ACT}:0002:2{NBH}1")).unwrap());
    assert_eq!(
        (report["division"].clone(), report["subdivision"].clone()),
        (json!("2"), json!("2-A"))
    );
    let cites: Vec<(Value, Vec<Value>)> = report["cites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["register_id"].clone(),
                c["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["row_id"].clone())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        cites,
        [
            (json!(ACT), vec![json!(format!("{ACT}:0003:3{NBH}1"))]),
            (json!(OTHER), vec![json!(format!("{OTHER}:0001:5"))])
        ]
    );
    let mut cited_by: Vec<(String, String)> = report["cited_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["row_id"].as_str().unwrap().into(),
                c["kind"].as_str().unwrap().into(),
            )
        })
        .collect();
    cited_by.sort();
    cited_by.dedup();
    let pair = |r: String, k: &str| (r, k.to_string());
    assert_eq!(
        cited_by,
        [
            pair(format!("{ACT}:0001:1"), "division"),
            pair(format!("{ACT}:0001:1"), "subdivision"),
            pair(format!("{ACT}:0003:3{NBH}1"), "section"),
            pair(format!("{ACT}:0004:4"), "section"),
            pair(format!("{OTHER}:0001:5"), "section"),
            pair(format!("{OTHER}:0001:5"), "subdivision"),
        ]
    );
    let cites =
        value(&search::refs(&connection, &format!("{ACT}:0001:1")).unwrap())["cites"].clone();
    let got: Vec<(Value, Value, Vec<Value>)> = cites
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["kind"].clone(),
                c["key"].clone(),
                c["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["row_id"].clone())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                json!("division"),
                json!("2"),
                vec![json!(format!("{ACT}:0002:2{NBH}1"))]
            ),
            (
                json!("division"),
                json!("3"),
                vec![json!(format!("{ACT}:0003:3{NBH}1"))]
            ),
            (
                json!("subdivision"),
                json!("2-A"),
                vec![json!(format!("{ACT}:0002:2{NBH}1"))]
            ),
        ]
    );
    let across = value(&search::refs(&connection, &format!("{OTHER}:0001:5")).unwrap());
    let got: Vec<(Value, Value, usize)> = across["cites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["kind"].clone(),
                c["register_id"].clone(),
                c["rows"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (json!("section"), json!(ACT), 1),
            (json!("subdivision"), json!(ACT), 1)
        ]
    );
    assert_eq!(
        across["cites"][0]["rows"][0]["row_id"],
        format!("{ACT}:0002:2{NBH}1")
    );
}

#[test]
fn unresolved_and_repeated_targets_are_reported_as_they_are() {
    let c = case();
    let connection = built(&c);
    let report = value(&search::refs(&connection, &format!("{ACT}:0003:3{NBH}1")).unwrap());
    let unresolved: Vec<(Value, Value)> = report["cites"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["register_id"].is_null())
        .map(|c| (c["cited_act"].clone(), c["rows"].clone()))
        .collect();
    assert_eq!(unresolved, [(json!("ITAA 1997"), json!([]))]);
    let rules = value(&search::refs(&connection, &format!("{INSTRUMENT}:0001:1")).unwrap());
    let acts: Vec<Value> = rules["cites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["cited_act"].clone())
        .collect();
    assert_eq!(acts, [json!("Act"), Value::Null]);
    assert_eq!(
        rules["cites"][1]["rows"][0]["row_id"],
        format!("{INSTRUMENT}:0001:1")
    );
    assert!(
        search::refs(&connection, "C2099A00001:0099:9")
            .unwrap_err()
            .to_string()
            .contains("not in the index")
    );
    assert_eq!(
        value(&search::refs(&connection, &format!("{ACT}:0001:1")).unwrap())["cited_by"],
        json!([])
    );
}

fn rows_with_division(c: &Case, rows: Vec<Value>, heading_at: usize, division: &str) {
    let mut titles = Titles::new();
    titles.insert(ACT, ("Sample Levy Act 2099", rows.clone()));
    write_corpus(&c.root, &titles, &[]);
    let structure = BTreeMap::from([(
        heading_at,
        vec![format!("### Division {division}\u{2014}Items")],
    )]);
    std::fs::write(
        c.root.join("markdown").join(ACT).join(format!("{ACT}.md")),
        markdown("Sample Levy Act 2099", &rows, &structure),
    )
    .unwrap();
}

#[test]
fn decimal_divisions_create_no_references() {
    let c = case();
    rows_with_division(
        &c,
        vec![
            row(ACT, 1, Some("1"), "Division 10.1 applies.", "1 Scope", None),
            row(ACT, 2, Some("2"), "Text.", "2 Item", None),
        ],
        2,
        "1",
    );
    build(&c);
    let connection = search::open_index(&s(&c.out)).unwrap();
    assert_eq!(
        value(&search::refs(&connection, &format!("{ACT}:0001:1")).unwrap())["cites"],
        json!([])
    );
    assert_eq!(
        value(&search::refs(&connection, &format!("{ACT}:0002:2")).unwrap())["cited_by"],
        json!([])
    );
}

fn cli(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("search")
        .args(args)
        .current_dir(cwd)
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
fn text_output_shortens_a_long_target_list() {
    let c = case();
    let mut rows = vec![row(
        ACT,
        1,
        Some("1"),
        "Division 5 applies.",
        "1 Scope",
        None,
    )];
    for n in 2..9 {
        rows.push(row(
            ACT,
            n,
            Some(&format!("5-{n}")),
            "Text.",
            &format!("5-{n} Item"),
            None,
        ));
    }
    rows_with_division(&c, rows, 2, "5");
    build(&c);
    let dir = c.out.parent().unwrap();
    let (code, out, _) = cli(&["refs", "index.sqlite", &format!("{ACT}:0001:1")], dir);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("cites division 5 (Division 5): {ACT}:0002:5-2, ")));
    assert!(out.contains("and 2 more"));
    let (_, out, _) = cli(&["refs", "index.sqlite", &format!("{ACT}:0002:5-2")], dir);
    assert!(out.contains(&format!("cited by {ACT}:0001:1 (1 Scope): Division 5")));
}

#[test]
fn commands_print_provenance_and_the_notice() {
    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    let dir = c.out.parent().unwrap().to_path_buf();
    let (code, out, _) = cli(&["build", "corpus", "--out", "index.sqlite"], &dir);
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "indexed 3 titles, 8 rows, 7 definitions and 13 section references\n"
        )
    );
    for argv in [
        vec!["query", "index.sqlite", "widget"],
        vec!["define", "index.sqlite", "approved", "gadget"],
        vec!["refs", "index.sqlite", &format!("{ACT}:0002:2{NBH}1")],
    ] {
        let (code, out, _) = cli(&argv, &dir);
        assert_eq!(code, 0);
        assert!(out.contains(search::NOTICE));
        assert!(out.contains("2099-08-01"));
    }
    let (_, out, _) = cli(&["define", "index.sqlite", "gadget"], &dir);
    assert!(out.contains("not a finding that it has its ordinary meaning"));
    let (_, out, _) = cli(&["--help"], &dir);
    assert!(out.contains("{build,query,define,refs}"));
}

#[test]
fn json_output_is_one_object_per_line() {
    let c = case();
    built(&c);
    let dir = c.out.parent().unwrap();
    let (code, out, _) = cli(&["query", "index.sqlite", "widget", "--json"], dir);
    assert_eq!(code, 0);
    let hits: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!hits.is_empty() && hits.iter().all(|h| h["authorised"] == false));
    let (_, out, _) = cli(
        &["refs", "index.sqlite", &format!("{ACT}:0001:1"), "--json"],
        dir,
    );
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["cited_by"],
        json!([])
    );
    let (_, out, _) = cli(&["define", "index.sqlite", "widget", "--json"], dir);
    let rids: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["register_id"].clone())
        .collect();
    assert_eq!(rids, [json!(ACT), json!(OTHER)]);
    assert_eq!(
        cli(&["define", "index.sqlite", "*"], dir),
        (1, String::new(), "error: the term is empty\n".into())
    );
}

#[test]
fn errors_exit_one_without_a_traceback() {
    let c = case();
    std::fs::write(&c.out, b"not sqlite").unwrap();
    let dir = c.out.parent().unwrap();
    for argv in [
        vec!["query", "index.sqlite", "widget"],
        vec!["query", "absent.sqlite", "widget"],
        vec!["build", "corpus", "--out", "index.sqlite"],
    ] {
        let (code, out, err) = cli(&argv, dir);
        assert_eq!((code, out.as_str()), (1, ""), "{argv:?}");
        assert!(err.starts_with("error: "));
    }
}

#[test]
fn an_index_from_another_schema_is_refused() {
    let c = case();
    built(&c);
    let connection = rusqlite::Connection::open(&c.out).unwrap();
    connection
        .execute("UPDATE meta SET value = '0' WHERE key = 'schema'", [])
        .unwrap();
    drop(connection);
    assert!(
        search::open_index(&s(&c.out))
            .unwrap_err()
            .to_string()
            .contains("different schema")
    );
}

#[test]
fn a_failed_write_leaves_no_staging_file() {
    // The Python test made os.link fail; here a value SQLite cannot bind fails
    // the write after the staging file exists, which takes the same cleanup.
    let c = case();
    write_corpus(&c.root, &sample_titles(), &[]);
    let path = c.root.join("sources.json");
    let mut sources: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    sources["titles"][0]["collection"] = json!([]);
    std::fs::write(&path, sources.to_string()).unwrap();
    refused(&c, "type 'list' is not supported");
}
