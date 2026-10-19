//! `fadden rates`: ported from the rate tests in tests/corpus/test_regressions.py
//! and tests/corpus/test_output_security.py. Every row is fabricated.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::rates;
use tax_radar_au::pyjson::Json;

fn p(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

fn s(points: &[u32]) -> String {
    points
        .iter()
        .map(|&cp| char::from_u32(cp).unwrap())
        .collect()
}

fn strings(values: Vec<Vec<u32>>) -> Vec<String> {
    values.iter().map(|v| s(v)).collect()
}

#[test]
fn sentence_boundaries_and_percentages_remain_precise() {
    let text = "The first rule applies.  Next sentence carries 10%.\n\nThird sentence is here.";
    assert_eq!(
        strings(rates::sentences(&p(text))),
        [
            "The first rule applies.",
            "Next sentence carries 10%.",
            "Third sentence is here."
        ]
    );
    assert_eq!(
        strings(rates::percentage_values(&p("10% and 12.5 %"))),
        ["10%", "12.5 %"]
    );
}

#[test]
fn amount_scanners_keep_tax_formats_and_ownership_tests() {
    assert_eq!(
        strings(rates::money_values(&p("$1,234.50 and $ 20"))),
        ["$1,234.50", "$ 20"]
    );
    let row = p("| $1,000 | 12.5 % |");
    assert!(rates::is_rate_table(&[row.as_slice()]));
    assert!(rates::is_ownership_test(&p(
        "A 75% voting interest is required."
    )));
    assert!(rates::is_ownership_test(&p(
        "The company is a 100% subsidiary."
    )));
    assert!(!rates::is_ownership_test(&p("The tax rate is 10%.")));
}

#[test]
fn numeric_scanners_complete_on_long_digit_runs() {
    let digits = "9".repeat(200_000);
    assert!(rates::percentage_values(&p(&format!("{digits}."))).is_empty());
    assert_eq!(rates::money_values(&p(&format!("${digits}."))).len(), 1);
    assert!(rates::is_ownership_test(&p(&format!("{digits}% stake"))));
}

#[test]
fn scanner_edges() {
    assert_eq!(strings(rates::money_values(&p("$ abc and $5"))), ["$5"]);
    assert!(rates::is_ownership_test(&p(
        "It must pass the continuity of ownership test."
    )));
    let act = Json::str("A New Tax System (Goods and Services Tax) Act 1999");
    assert_eq!(
        rates::topic_for(&act, &Json::str("Taxable supplies"), &[]),
        "gst and indirect"
    );
    let text = p("| a |\ntext\n| b |\n| c |");
    let blocks: Vec<Vec<String>> = rates::table_blocks(&text)
        .into_iter()
        .map(|b| b.into_iter().map(s).collect())
        .collect();
    assert_eq!(blocks, [vec!["| a |"], vec!["| b |", "| c |"]]);
}

fn row() -> Value {
    json!({
        "register_id": "C2099A00001", "act": "Zulu Tax Example Act",
        "collection": "Act", "compilation_number": "1",
        "compilation_date": "2099-01-01", "section": "1",
        "heading": "Example rate", "register_page": "https://example.test/source",
        "version_is_current": true, "source_url": "https://example.test/source.epub",
        "authorised": false, "licence": "Synthetic test licence",
        "licence_url": "https://example.test/licence",
        "attribution": "Fabricated source for a regression test",
        "text": "The example tax rate is 10% of the base amount.",
    })
}

fn with(base: &Value, changes: Value) -> Value {
    let mut out = base.clone();
    for (k, v) in changes.as_object().unwrap() {
        out[k] = v.clone();
    }
    out
}

/// A source checkout: `<tmp>/proj/fadden` beside `pyproject.toml`, `VERSION`
/// and the corpus root `<tmp>/proj/corpus`.
struct Checkout {
    _dir: tempfile::TempDir,
    project: PathBuf,
    root: PathBuf,
}

fn checkout() -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(project.join("fadden")).unwrap();
    std::fs::write(project.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(project.join("VERSION"), "x\n").unwrap();
    let root = project.join("corpus");
    std::fs::create_dir(&root).unwrap();
    Checkout {
        project,
        root,
        _dir: dir,
    }
}

fn run(module_dir: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("rates")
        .env("FADDEN_MODULE_DIR", module_dir)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

fn write_corpus(c: &Checkout, rows: &[Value], unlisted: &[&str], trailing: &str) {
    let mut grouped: Vec<(String, Vec<&Value>)> = Vec::new();
    for r in rows {
        let rid = r["register_id"].as_str().unwrap().to_string();
        match grouped.iter_mut().find(|(k, _)| *k == rid) {
            Some((_, group)) => group.push(r),
            None => grouped.push((rid, vec![r])),
        }
    }
    for (rid, group) in &grouped {
        let folder = c.root.join("markdown").join(rid);
        std::fs::create_dir_all(&folder).unwrap();
        let text: String = group.iter().map(|r| format!("{r}\n")).collect::<String>() + trailing;
        std::fs::write(folder.join("sections.jsonl"), text).unwrap();
    }
    let titles: Vec<Value> = grouped
        .iter()
        .map(|(rid, _)| json!({"register_id": rid}))
        .collect();
    std::fs::write(
        c.root.join("sources.json"),
        json!({"titles": titles}).to_string(),
    )
    .unwrap();
    for rid in unlisted {
        std::fs::create_dir_all(c.root.join("markdown").join(rid)).unwrap();
    }
}

/// Run the stage over fabricated sections and return its outputs.
fn export_with(
    rows: &[Value],
    unlisted: &[&str],
    trailing: &str,
) -> Result<(Vec<Value>, String), String> {
    let c = checkout();
    write_corpus(&c, rows, unlisted, trailing);
    let (code, _, err) = run(&c.project.join("fadden"));
    if code != 0 {
        return Err(err);
    }
    let read = |name: &str| std::fs::read_to_string(c.root.join("rates").join(name)).unwrap();
    let records = read("rates.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Ok((records, read("RATES.md").replace("\r\n", "\n")))
}

fn export(rows: &[Value]) -> (Vec<Value>, String) {
    export_with(rows, &[], "").unwrap()
}

#[test]
fn a_title_directory_sources_json_does_not_list_stops_the_export() {
    export_with(&[row()], &[".cache"], "").unwrap();
    let error = export_with(&[row()], &["C2020A00099"], "").unwrap_err();
    assert!(error.starts_with("RuntimeError: "), "{error}");
    assert!(error.contains("C2020A00099"));
}

#[test]
fn rate_rows_carry_the_source_status_and_attribution() {
    let (stale, stale_md) = export(&[with(&row(), json!({"version_is_current": false}))]);
    assert_eq!(stale.len(), 1);
    for key in [
        "source_url",
        "licence",
        "licence_url",
        "authorised",
        "attribution",
    ] {
        assert_eq!(stale[0][key], row()[key], "{key}");
    }
    assert_eq!(stale[0]["version_is_current"], false);
    assert!(stale_md.contains("already stale"));
    assert!(
        stale_md.contains("Zulu Tax Example Act (C2099A00001): compilation 1 of 2099\\-01\\-01")
    );

    let (current, current_md) = export(&[row()]);
    assert_eq!(current[0]["version_is_current"], true);
    assert_eq!(current[0]["attribution"], row()["attribution"]);
    assert!(!current_md.contains("already stale"));
}

#[test]
fn one_decimal_statutory_fractions_reach_the_index() {
    let (short, _) = export(&[with(
        &row(),
        json!({"text": "The adjustment factor is 0.5 of the base amount."}),
    )]);
    let (long, _) = export(&[with(
        &row(),
        json!({"text": "The adjustment factor is 0.50 of the base amount."}),
    )]);
    assert_eq!((short.len(), long.len()), (1, 1));
    assert_eq!(short[0]["kind"], "factor");
    assert_eq!(short[0]["amounts"], json!(["0.5"]));
    assert_eq!(long[0]["amounts"], json!(["0.50"]));
    let (plain, _) = export(&[with(
        &row(),
        json!({"text": "The document was tabled on day 0.5 of the sitting week."}),
    )]);
    assert!(plain.is_empty());
}

#[test]
fn rate_ids_are_documented_as_snapshot_ordinals() {
    let (first, markdown) = export(&[row()]);
    let (expanded, _) = export(&[
        row(),
        with(
            &row(),
            json!({"register_id": "C2099A00002", "act": "Alpha Tax Example Act",
                   "text": "The example tax rate is 20% of the base amount."}),
        ),
    ]);
    let moved = expanded.iter().find(|r| r["act"] == row()["act"]).unwrap();
    assert_eq!(first[0]["rate_id"], "R00001");
    assert_eq!(moved["rate_id"], "R00002");
    assert!(markdown.contains("ordinal within this snapshot, not a persistent"));
    assert!(markdown.contains("cite the register id, section and snapshot"));
}

const TABLE: &str = "| Band | Rate |\n|---|---|\n| $0 to $18,200 | 0% |";

#[test]
fn tables_long_sections_and_blank_lines_reach_the_index() {
    let mut rows: Vec<Value> = (0..41)
        .map(|n| {
            with(
                &row(),
                json!({"section": n.to_string(), "heading": "Income tax rates",
                       "text": format!("Rates for the year.\n{TABLE}\nBelow the table.")}),
            )
        })
        .collect();
    rows.extend((0..121).map(|n| {
        with(
            &row(),
            json!({"section": format!("P{n}"), "act": "Income Tax Rates Act",
                   "text": format!("The income tax rate is {}% of the base amount.", n % 99)}),
        )
    }));
    rows.push(with(
        &row(),
        json!({"register_id": "F2099L00001", "collection": "LegislativeInstrument",
               "heading": "Instrument rates", "text": TABLE}),
    ));
    let (records, markdown) = export_with(&rows, &[], "\n").unwrap();
    assert_eq!(records.iter().filter(|r| r["kind"] == "table").count(), 42);
    assert_eq!(
        records
            .iter()
            .filter(|r| r["topic"] == "income tax rates" && r["kind"] == "table")
            .count(),
        41
    );
    for needle in [
        "### Rate tables",
        "_1 further tables in rates.jsonl._",
        "_1 further provisions in rates.jsonl._",
        "**Zulu Tax Example Act** _(instrument)_ s 1: Instrument rates",
    ] {
        assert!(markdown.contains(needle), "{needle}");
    }
}

#[test]
fn sentences_without_a_qualifying_number_are_skipped() {
    let text = "The example tax rate is 10% of the base amount\n\n\n\
                Short one\n\nNothing numeric is said in this sentence at all.\n\
                | $1 |\n| 2% |\n\n\
                Pay $100 to the person named in the notice.";
    let (records, _) = export(&[with(&row(), json!({"text": text}))]);
    let contents: Vec<&Value> = records.iter().map(|r| &r["content"]).collect();
    assert_eq!(
        contents,
        [&json!("The example tax rate is 10% of the base amount")]
    );
}

#[test]
fn rate_rows_must_belong_to_their_directory_even_without_rate_content() {
    let c = checkout();
    let title = c.root.join("markdown").join("C2099A00001");
    std::fs::create_dir_all(&title).unwrap();
    std::fs::write(
        c.root.join("sources.json"),
        r#"{"titles": [{"register_id": "C2099A00001"}]}"#,
    )
    .unwrap();
    std::fs::write(
        title.join("sections.jsonl"),
        "{\"register_id\": \"C2099A00002\", \"text\": \"No rate here.\"}\n",
    )
    .unwrap();
    let (code, out, err) = run(&c.project.join("fadden"));
    assert_eq!((code, out.as_str()), (1, ""));
    assert_eq!(
        err,
        "ValueError: section register ID does not match its title directory\n"
    );
    assert_eq!(std::fs::read_dir(c.root.join("rates")).unwrap().count(), 0);
}

#[test]
fn an_installed_location_is_refused_before_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, err) = run(&dir.path().join("fadden"));
    assert_eq!(code, 1);
    assert!(
        err.starts_with("RuntimeError: corpus stages require a source checkout"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_deployed_build_directory_writes_beside_it() {
    // <root>/build is the deployed layout: the corpus root is its parent.
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("build");
    std::fs::create_dir(&build).unwrap();
    let folder = dir.path().join("markdown").join("C2099A00001");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("sections.jsonl"), format!("{}\n", row())).unwrap();
    std::fs::write(
        dir.path().join("sources.json"),
        r#"{"titles": [{"register_id": "C2099A00001"}]}"#,
    )
    .unwrap();
    let (code, out, err) = run(&build);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        out.starts_with("entries: 1  titles: 1\nby collection: {'Act': 1}\nby kind: {'rate': 1}\n"),
        "{out}"
    );
    assert!(dir.path().join("rates").join("RATES.md").is_file());
}
