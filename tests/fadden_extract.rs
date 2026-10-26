//! `fadden extract`: ported from the stage tests in tests/corpus/test_extract_paths.py
//! (ExtractStageTests), test_regressions.py (ExtractPipelineTests and
//! FailureHandlingTests), test_rowspan_regressions.py (RowspanPipelineTests),
//! test_audit_regressions.py and test_security_remediation.py. Every title is
//! fabricated. As in Python, which points `extract.__file__` at
//! `build/extract.py`, the stage runs as deployed: the module directory is
//! `<tmp>/build` and the corpus root is `<tmp>`.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::extract_doc::{self, Block};
use tax_radar_au::fadden::{Exc, Run, epub, extract};
use tax_radar_au::pyzlib::crc32;

const REGISTER_ID: &str = "F2025L00281";

/// `VolumeGateTests.VOLUME_1` and `VOLUME_2` in test_regressions.py.
const VOLUME_1: &str = "<html><body>\
    <p class=\"ActHead4\"><span class=\"CharSectno\">1</span> Short title</p>\
    <p class=\"subsection\">This instrument is the Demo Approval 2025.</p>\
    </body></html>";
const VOLUME_2: &str = "<html><body>\
    <p class=\"MadeunderText\">Demo Approval 2025</p>\
    <p>Compilation No. 1</p>\
    <p>This compilation is in 2 volumes</p>\
    <p>Volume 2: Schedule 1</p>\
    <p>Endnotes</p>\
    <p class=\"ContentsHead\">Contents</p>\
    <p class=\"TOC6\">Schedule 1 Valuation factors</p>\
    <p class=\"Scheduletitle\">Schedule 1 Valuation factors</p>\
    <p class=\"ScheduleHeading\">Table 3B Lump sum valuation factors</p>\
    <table><tr><td>Age</td><td>Factor</td></tr>\
    <tr><td>55</td><td>12.5</td></tr></table>\
    </body></html>";
/// `LEADING` in test_rowspan_regressions.py.
const LEADING: &str = "<table><thead><tr><th>Category</th><th>Rate</th></tr></thead>\
    <tbody><tr><td rowspan=\"2\">A</td><td>5%</td></tr>\
    <tr><td>10%</td></tr></tbody></table>";

/// A ZIP archive of (name, method, stored bytes, uncompressed bytes) members,
/// laid out as `zipfile` writes one.
fn zip(members: &[(&str, u16, &[u8], &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for &(name, method, data, plain) in members {
        let name = name.as_bytes();
        let sizes = [crc32(plain, 0), data.len() as u32, plain.len() as u32];
        let offset = out.len() as u32;
        out.extend(b"PK\x03\x04");
        for v in [20u16, 0, method, 0, 33] {
            out.extend(v.to_le_bytes());
        }
        sizes.iter().for_each(|v| out.extend(v.to_le_bytes()));
        for v in [name.len() as u16, 0] {
            out.extend(v.to_le_bytes());
        }
        out.extend(name);
        out.extend(data);
        central.extend(b"PK\x01\x02");
        for v in [20u16, 20, 0, method, 0, 33] {
            central.extend(v.to_le_bytes());
        }
        sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
        for v in [name.len() as u16, 0, 0, 0, 0] {
            central.extend(v.to_le_bytes());
        }
        for v in [0u32, offset] {
            central.extend(v.to_le_bytes());
        }
        central.extend(name);
    }
    let (start, size, count) = (out.len() as u32, central.len() as u32, members.len() as u16);
    out.extend(central);
    out.extend(b"PK\x05\x06");
    for v in [0u16, 0, count, count] {
        out.extend(v.to_le_bytes());
    }
    for v in [size, start] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}

/// Stored members, as `ZipFile.writestr` writes them by default.
fn stored(members: &[(String, String)]) -> Vec<u8> {
    let entries: Vec<(&str, u16, &[u8], &[u8])> = members
        .iter()
        .map(|(n, d)| (n.as_str(), 0, d.as_bytes(), d.as_bytes()))
        .collect();
    zip(&entries)
}

/// `security.epub_bytes(*members)` and `VolumeGateTests.epub_bytes(*volumes)`:
/// each document as `document_N.xhtml`.
fn documents(html: &[&str]) -> Vec<u8> {
    let members: Vec<(String, String)> = html
        .iter()
        .enumerate()
        .map(|(i, h)| (format!("document_{}.xhtml", i + 1), h.to_string()))
        .collect();
    stored(&members)
}

/// `test_rowspan_regressions.epub_bytes(*tables)`.
fn rowspan_epub(tables: &[&str]) -> Vec<u8> {
    let mut members = vec![("mimetype".to_string(), "application/epub+zip".to_string())];
    for (i, table) in tables.iter().enumerate() {
        members.push((
            format!("OEBPS/document_{}.xhtml", i + 1),
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                 <html xmlns=\"http://www.w3.org/1999/xhtml\"><body>\
                 <p class=\"ActHead5\"><span class=\"CharSectno\">1</span> Synthetic rates</p>\
                 {table}</body></html>"
            ),
        ));
    }
    stored(&members)
}

fn nested_html(depth: usize) -> String {
    format!(
        "{}A{}",
        "<table><tr><td>".repeat(depth),
        "</td></tr></table>".repeat(depth)
    )
}

/// A file as `read_text` gives it: universal newlines.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// A deployed build: `<tmp>/build` holds the manifest, `<tmp>/epub` the EPUBs.
struct Tree {
    _dir: tempfile::TempDir,
    base: PathBuf,
    build: PathBuf,
}

impl Tree {
    fn new() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let build = base.join("build");
        std::fs::create_dir(&build).unwrap();
        std::fs::create_dir(base.join("epub")).unwrap();
        Tree {
            _dir: dir,
            base,
            build,
        }
    }

    fn epub(&self, rid: &str) -> PathBuf {
        self.base.join("epub").join(format!("{rid}.epub"))
    }

    fn manifest(&self, titles: &[Value]) {
        std::fs::write(
            self.build.join("manifest_raw.json"),
            Value::Array(titles.to_vec()).to_string(),
        )
        .unwrap();
    }

    fn manifest_entries(&self) -> Vec<Value> {
        serde_json::from_str(&read(&self.build.join("manifest_raw.json"))).unwrap()
    }

    /// `ExtractStageTests.title`: an EPUB of the documents and its entry.
    fn title(&self, rid: &str, html: &[&str], fields: Value) -> Value {
        std::fs::write(self.epub(rid), documents(html)).unwrap();
        let mut entry = json!({"id": rid, "name": format!("Synthetic {rid}"),
                               "epub": format!("{rid}.epub"), "versionStart": "2099-01-01",
                               "collection": "LegislativeInstrument"});
        for (k, v) in fields.as_object().unwrap() {
            entry[k] = v.clone();
        }
        entry
    }

    /// `extract.main(retrieved)`: its outcome and what it printed.
    fn run(&self, retrieved: Option<&str>) -> (Result<(), Exc>, String) {
        let mut run = Run::default();
        let outcome = extract::run(&self.build.to_string_lossy(), retrieved, &mut run);
        (outcome, String::from_utf8_lossy(&run.stdout).into_owned())
    }

    fn title_dir(&self, rid: &str) -> PathBuf {
        self.base.join("markdown").join(rid)
    }

    fn rows(&self, rid: &str) -> Vec<Value> {
        read(&self.title_dir(rid).join("sections.jsonl"))
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// `ExtractPipelineTests._outputs`: the markdown and the rows.
    fn outputs(&self) -> (String, Vec<Value>) {
        let markdown = read(
            &self
                .title_dir(REGISTER_ID)
                .join(format!("{REGISTER_ID}.md")),
        );
        (markdown, self.rows(REGISTER_ID))
    }

    fn manifest_md(&self) -> PathBuf {
        self.build.join("manifest_md.json")
    }
}

/// `ExtractPipelineTests._fixture`: the 2-volume EPUB and its manifest.
fn pipeline() -> Tree {
    let tree = Tree::new();
    std::fs::write(tree.epub(REGISTER_ID), documents(&[VOLUME_1, VOLUME_2])).unwrap();
    tree.manifest(&[json!({
        "id": REGISTER_ID, "name": "Demo Approval 2025",
        "epub": format!("{REGISTER_ID}.epub"), "versionStart": "2025-01-01",
        "compilationNumber": "1", "collection": "LegislativeInstrument",
        "sourceUrl": format!("https://example.test/{REGISTER_ID}"),
        "version_is_current": false, "current_version_start": "2026-03-01",
    })]);
    tree
}

/// The RuntimeError a run raised, as its message.
fn runtime_error(outcome: Result<(), Exc>) -> String {
    let error = outcome.expect_err("expected the stage to fail");
    assert_eq!(error.class(), "RuntimeError", "{error}");
    error.to_string()
}

fn set_mtime(path: &Path, seconds: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        .unwrap();
}

/// A failing title placed before the pipeline's own.
fn fail_first(tree: &Tree, epub: Option<Vec<u8>>) {
    let manifest = tree.manifest_entries();
    let mut failed = manifest[0].clone();
    failed["id"] = json!("F2025L00280");
    failed["epub"] = json!("F2025L00280.epub");
    if let Some(bytes) = epub {
        std::fs::write(tree.epub("F2025L00280"), bytes).unwrap();
    }
    let mut titles = vec![failed];
    titles.extend(manifest);
    tree.manifest(&titles);
}

#[test]
fn bad_entries_are_each_named_and_refuse_the_manifest() {
    // Python also patches table_split to fail for F2099L00006; no input trips
    // the chunking guard (see test_the_completeness_guard_reports_a_dropped_line),
    // so that title succeeds here and 3 failures remain, not 4.
    let tree = Tree::new();
    let good = tree.title(
        "F2099L00004",
        &["<p>1 Name of instrument</p><p>Text.</p>"],
        json!({}),
    );
    std::fs::write(
        tree.base.join("epub").join("F2099L00004.epub.meta.json"),
        json!({"versionStart": "2098-01-01", "fetched_at": "2098-01-01T00:00:00Z"}).to_string(),
    )
    .unwrap();
    let naive = tree.title(
        "F2099L00005",
        &["<p>Text.</p>"],
        json!({"fetched_at": "2099-01-01T00:00:00"}),
    );
    let unsplittable = tree.title("F2099L00006", &["<p>Plain prose.</p>"], json!({}));
    tree.manifest(&[
        json!({"id": "C2099A00009", "name": "Wrong file", "epub": "other.epub"}),
        json!({"name": "No id", "epub": "x.epub"}),
        good,
        naive,
        unsplittable,
    ]);
    let (outcome, _) = tree.run(None);
    let message = runtime_error(outcome);
    assert!(
        message.contains("extraction incomplete for 3 title"),
        "{message}"
    );
    for rid in ["C2099A00009", "?", "F2099L00005"] {
        assert!(message.contains(rid), "{rid}: {message}");
    }
    assert!(
        tree.title_dir("F2099L00004")
            .join("sections.jsonl")
            .is_file()
    );
    assert!(!tree.manifest_md().exists());
}

#[test]
fn a_mismatched_epub_path_is_refused_before_reading() {
    let tree = Tree::new();
    tree.manifest(&[json!({
        "id": "F2020L01498", "name": "Example Instrument",
        "epub": "../../unrelated.epub", "versionStart": "2026-01-01",
        "compilationNumber": "1", "sourceUrl": "https://example.test/F2020L01498",
    })]);
    let (outcome, _) = tree.run(Some("2026-08-07"));
    assert!(runtime_error(outcome).contains("refusing to write manifest_md"));
    assert!(!tree.manifest_md().exists());
    assert!(!tree.title_dir("F2020L01498").exists());
}

#[test]
fn a_missing_epub_is_reported_and_later_titles_still_written() {
    let tree = pipeline();
    fail_first(&tree, None);
    let (outcome, out) = tree.run(None);
    assert!(runtime_error(outcome).contains("refusing to write manifest_md"));
    assert!(out.contains("FAIL F2025L00280"), "{out}");
    assert!(tree.title_dir(REGISTER_ID).join("sections.jsonl").is_file());
    assert!(!tree.manifest_md().exists());
}

#[test]
fn a_depth_failure_is_reported_and_keeps_the_previous_manifest() {
    let tree = pipeline();
    std::fs::write(tree.epub(REGISTER_ID), rowspan_epub(&[LEADING])).unwrap();
    tree.run(Some("2026-10-12")).0.unwrap();
    let previous = std::fs::read(tree.manifest_md()).unwrap();
    fail_first(&tree, Some(rowspan_epub(&[&nested_html(65)])));
    let (outcome, out) = tree.run(Some("2026-10-12"));
    assert!(runtime_error(outcome).contains("refusing to write manifest_md"));
    assert!(out.contains("FAIL F2025L00280"), "{out}");
    assert!(out.contains("depth budget"), "{out}");
    assert!(out.contains(REGISTER_ID), "{out}");
    assert!(tree.title_dir(REGISTER_ID).join("sections.jsonl").is_file());
    assert_eq!(std::fs::read(tree.manifest_md()).unwrap(), previous);
}

#[test]
fn an_overlap_failure_is_reported_and_keeps_any_prior_manifest() {
    for prior in [false, true] {
        let tree = pipeline();
        std::fs::write(tree.epub(REGISTER_ID), rowspan_epub(&[LEADING])).unwrap();
        fail_first(
            &tree,
            Some(rowspan_epub(&[
                "<table><tr><td>A</td><td rowspan=\"2\">B</td></tr>\
                 <tr><td colspan=\"2\">C</td></tr></table>",
            ])),
        );
        let previous = b"{\"previous\": true}\n";
        if prior {
            std::fs::write(tree.manifest_md(), previous).unwrap();
        }
        let (outcome, out) = tree.run(Some("2026-10-12"));
        assert!(runtime_error(outcome).contains("refusing to write manifest_md"));
        assert!(out.contains("FAIL F2025L00280"), "{out}");
        assert!(out.contains(REGISTER_ID), "{out}");
        assert!(tree.title_dir(REGISTER_ID).join("sections.jsonl").is_file());
        if prior {
            assert_eq!(std::fs::read(tree.manifest_md()).unwrap(), previous);
        } else {
            assert!(!tree.manifest_md().exists());
        }
    }
}

#[test]
fn extracted_rows_feed_rates() {
    let tree = pipeline();
    std::fs::write(tree.epub(REGISTER_ID), rowspan_epub(&[LEADING])).unwrap();
    tree.run(Some("2026-10-12")).0.unwrap();
    let (markdown, rows) = tree.outputs();
    let expected = "|  | 10% |";
    assert!(markdown.contains(expected), "{markdown}");
    let section = rows
        .iter()
        .find(|r| r["text"].as_str().unwrap().contains("10%"))
        .unwrap();
    assert!(section["text"].as_str().unwrap().contains(expected));
    assert_eq!(section["authorised"], false);
    assert_eq!(section["compilation_number"], "1");
    std::fs::write(
        tree.base.join("sources.json"),
        json!({"titles": [{"register_id": REGISTER_ID}]}).to_string(),
    )
    .unwrap();
    let done = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("rates")
        .env("FADDEN_MODULE_DIR", &tree.build)
        .output()
        .unwrap();
    assert_eq!(
        done.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let indexed: Vec<Value> = read(&tree.base.join("rates").join("rates.jsonl"))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let record = indexed.iter().find(|r| r["kind"] == "table").unwrap();
    let content = record["content"].as_str().unwrap();
    assert!(content.contains(expected), "{content}");
    assert_eq!(content.matches("5%").count(), 1);
    assert_eq!(content.matches("10%").count(), 1);
    for key in [
        "register_id",
        "compilation_number",
        "source_url",
        "authorised",
        "attribution",
    ] {
        assert_eq!(record[key], section[key], "{key}");
    }
}

#[test]
fn a_matching_sidecar_dates_the_retrieval() {
    let tree = pipeline();
    let epub = tree.epub(REGISTER_ID);
    std::fs::write(
        epub.with_extension("epub.meta.json"),
        json!({"versionStart": "2025-01-01", "fetched_at": "2026-08-04T23:15:00+00:00"})
            .to_string(),
    )
    .unwrap();
    set_mtime(&epub, 0);
    tree.run(None).0.unwrap();
    let (markdown, rows) = tree.outputs();
    assert!(markdown.contains("retrieved: \"2026-08-04\""), "{markdown}");
    assert!(
        rows.iter()
            .all(|r| r["attribution"].as_str().unwrap().contains("2026-08-04"))
    );
}

#[test]
fn the_retrieved_argument_overrides_a_restored_mtime() {
    let tree = pipeline();
    // 2026-02-03 12:00 UTC: the local date is 2026-02-03 within 12 hours of UTC.
    set_mtime(&tree.epub(REGISTER_ID), 1_770_120_000);
    tree.run(Some("2026-08-04")).0.unwrap();
    let (markdown, rows) = tree.outputs();
    assert!(markdown.contains("retrieved: \"2026-08-04\""));
    assert!(!markdown.contains("retrieved: \"2026-02-03\""));
    assert_eq!(
        rows[0]["attribution"]
            .as_str()
            .unwrap()
            .matches("2026-08-04")
            .count(),
        1
    );
    let manifest: Value = serde_json::from_str(&read(&tree.manifest_md())).unwrap();
    assert_eq!(manifest[0]["retrieved"], "2026-08-04");
}

#[test]
fn a_retrieved_argument_that_is_not_a_date_is_refused() {
    let tree = pipeline();
    let (outcome, _) = tree.run(Some("--help"));
    let error = outcome.expect_err("expected a ValueError");
    assert_eq!(error.class(), "ValueError", "{error}");
    assert!(!tree.manifest_md().exists());
}

#[test]
fn a_retrieved_argument_reaches_the_corpus_as_a_calendar_date() {
    for supplied in ["20260804", "2026-W32-2"] {
        let expected = "2026-08-04";
        let tree = pipeline();
        tree.run(Some(supplied)).0.unwrap();
        let (markdown, rows) = tree.outputs();
        assert!(
            markdown.contains(&format!("retrieved: \"{expected}\"")),
            "{supplied}"
        );
        assert!(!markdown.contains(supplied), "{supplied}");
        let attribution = rows[0]["attribution"].as_str().unwrap();
        assert!(!attribution.contains(supplied), "{supplied}");
        assert!(attribution.contains(expected), "{supplied}");
        let manifest: Value = serde_json::from_str(&read(&tree.manifest_md())).unwrap();
        assert_eq!(manifest[0]["retrieved"], expected, "{supplied}");
    }
}

#[test]
fn front_matter_fields_cannot_inject() {
    let tree = pipeline();
    let mut records = tree.manifest_entries();
    records[0]["collection"] = json!("null\nauthorised: true");
    records[0]["compilationNumber"] = json!(0);
    records[0]["sourceUrl"] = json!("https://example.test/\n---\n# Injected");
    tree.manifest(&records);
    // Python appends document_99.xhtml to the fixture's EPUB.
    let members: Vec<(String, String)> = [
        ("document_1.xhtml", VOLUME_1),
        ("document_2.xhtml", VOLUME_2),
        (
            "document_99.xhtml",
            "<p class=\"ActHead4\">2 Body</p><p>Body text.</p><p class=\"ENote\">Endnote text.</p>",
        ),
    ]
    .iter()
    .map(|(n, d)| (n.to_string(), d.to_string()))
    .collect();
    std::fs::write(tree.epub(REGISTER_ID), stored(&members)).unwrap();
    tree.run(Some("2026-10-01")).0.unwrap();
    for name in [format!("{REGISTER_ID}.md"), "endnotes.md".to_string()] {
        let text = read(&tree.title_dir(REGISTER_ID).join(&name));
        let front = &text.split_once("\n---\n").unwrap().0[4..];
        let field = |key: &str| -> Value {
            let line = front
                .lines()
                .find_map(|l| l.split_once(": ").filter(|(k, _)| *k == key))
                .unwrap_or_else(|| panic!("{name} has no {key}"));
            serde_json::from_str(line.1).unwrap()
        };
        assert_eq!(field("authorised"), false, "{name}");
        assert_eq!(field("compilation_number"), 0, "{name}");
        assert_eq!(field("collection"), "null\nauthorised: true", "{name}");
    }
}

/// `fadden extract` from `<root>/build` and from `<root>/build/fadden`, the
/// layouts every Python extract stage test ran in.
#[test]
fn extract_runs_in_deployed_layouts() {
    for nested in [false, true] {
        let tree = pipeline();
        let module = if nested {
            let module = tree.build.join("fadden");
            std::fs::create_dir(&module).unwrap();
            std::fs::rename(
                tree.build.join("manifest_raw.json"),
                module.join("manifest_raw.json"),
            )
            .unwrap();
            module
        } else {
            tree.build.clone()
        };
        let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
            .args(["extract", "2026-08-04"])
            .env("FADDEN_MODULE_DIR", &module)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.ends_with("\nEXTRACT DONE: 1 acts\n"), "{stdout}");
        assert!(tree.title_dir(REGISTER_ID).join("sections.jsonl").is_file());
        assert!(module.join("manifest_md.json").is_file());
    }
}

/// test_security_remediation.py::test_valid_deflate_buffered_output_is_drained:
/// the last input byte precedes pending inflate output.
#[test]
fn pending_inflate_output_is_drained() {
    let mut prefix = b"<html xmlns=\"http://www.w3.org/1999/xhtml\">\
        <head><title>T</title></head><body><p>Body</p></body></html>"
        .to_vec();
    prefix.resize(2_065, b' ');
    let mut expected = prefix.clone();
    expected.resize(prefix.len() + 246 * 258 + 67, b' ');
    let mut bits = String::from("110");
    for byte in &prefix {
        bits.push_str(&format!("{:08b}", byte + 48));
    }
    bits.push_str(&"1100010100000".repeat(246));
    bits.push_str("0010101");
    bits.push_str("0000");
    bits.push_str("00000");
    bits.push_str("0000000");
    let raw: Vec<u8> = bits
        .as_bytes()
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .map(|(j, &bit)| u8::from(bit == b'1') << j)
                .sum()
        })
        .collect();
    assert_eq!((expected.len(), raw.len()), (65_600, 2_468));
    let payload = zip(&[("document_1.xhtml", 8, &raw, &expected)]);
    let archive = epub::from_bytes(payload.clone()).unwrap();
    assert_eq!(archive.read("document_1.xhtml").unwrap(), expected);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drained.epub");
    std::fs::write(&path, payload).unwrap();
    let blocks = extract_doc::epub_blocks(&path.to_string_lossy()).unwrap();
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, Block::P { text, .. } if text == "Body"))
    );
}

/// The EPUB-member half of
/// test_rowspan_regressions.py::test_empty_row_budget_is_shared_across_nested_tables_and_epub_members.
/// Python lowers MAX_TABLE_CELLS to 5 and splits 6 empty rows across 2
/// members; this splits 1,000,001 across 2 at the real budget.
#[test]
fn empty_row_budget_is_shared_across_epub_members() {
    let first = format!("<table>{}</table>", "<tr/>".repeat(600_000));
    let second = format!("<table>{}</table>", "<tr/>".repeat(400_001));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("budget.epub");
    let wanted = (extract_doc::MAX_TABLE_CELLS + 1) as usize;
    assert_eq!(600_000 + 400_001, wanted);
    for (tables, fails) in [
        (vec![first.as_str()], false),
        (vec![first.as_str(), second.as_str()], true),
    ] {
        std::fs::write(&path, rowspan_epub(&tables)).unwrap();
        match extract_doc::epub_blocks(&path.to_string_lossy()) {
            Err(Exc::Value {
                class: "ValueError",
                message,
            }) if fails => assert!(message.contains("cell budget"), "{message}"),
            Ok(_) if !fails => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
}
