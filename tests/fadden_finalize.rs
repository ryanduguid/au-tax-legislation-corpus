//! `fadden finalize`: ported from tests/corpus/test_finalize_phases.py (since removed) and the
//! finalize tests in tests/corpus/test_regressions.py (FinalizePiiSummaryTests,
//! FinalizeMissingTitleTests). Every title is fabricated. Python points
//! `SCRATCH` and `ROOT` at separate directories; here the stage runs from a
//! source checkout, `<tmp>/proj/fadden` beside the corpus root `<tmp>/proj/corpus`,
//! which gives the same separation. The phase functions Python calls directly
//! are private to `finalize.rs`, so their results are read from what
//! `finalize::run` writes and prints.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::{Exc, Run, finalize};

/// The staleness checker finalize copies to the corpus root.
const SUPPORT: [&str; 3] = ["check_current.py", "corpus_paths.py", "http_fetch.py"];
/// The builder stages the Python finalize also copied into build/.
const BUILDER: [&str; 5] = [
    "discover.py",
    "versions.py",
    "download.py",
    "extract.py",
    "finalize.py",
];
/// `OUTPUT_HASHES` in the removed test_finalize_phases.py; README.md now has its
/// "Build record" section.
const OUTPUT_HASHES: [(&str, &str); 4] = [
    (
        "INDEX.md",
        "c80623ba5f390136de52680cf543da0a5ff19d318c91cb1b09613e28eac8b740",
    ),
    (
        "LICENCE-NOTICE.md",
        "736c818cd50e48d1e3052ffd58e9fd238c803a9949c617e9973a85f6753ba915",
    ),
    (
        "README.md",
        "e407a8238190af5d3846b7a14b32d6461b4fe0ea98334c1f6e85bae97b9d65fb",
    ),
    (
        "sources.json",
        "44fb7fea960c7aaf98ce2abc9d642eaaae08872cfc062418950e271921349a0b",
    ),
];
const STDOUT: &str = "titles=2 (acts=1 instruments=1) rows=3 words=33 epub=0.0 MB \
                      missing=1 whole_act=0\n\
                      by collection: {'Act': 1, 'LegislativeInstrument': 1}\n";
const DATE: &str = "2026-08-04";

/// A file as `read_text` gives it: universal newlines.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn canonical_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The module directory and the corpus root the stage reads and writes.
struct Tree {
    _dir: tempfile::TempDir,
    base: PathBuf,
    scratch: PathBuf,
    root: PathBuf,
}

impl Tree {
    /// A source checkout: `<tmp>/proj/fadden` and `<tmp>/proj/corpus`.
    fn checkout() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let project = base.join("proj");
        let scratch = project.join("fadden");
        let root = project.join("corpus");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(project.join("pyproject.toml"), "x\n").unwrap();
        std::fs::write(project.join("VERSION"), "x\n").unwrap();
        Tree {
            _dir: dir,
            base,
            scratch,
            root,
        }
    }

    /// A deployed build: `<tmp>/build` with the corpus root `<tmp>`.
    fn deployed() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let scratch = base.join("build");
        std::fs::create_dir(&scratch).unwrap();
        Tree {
            _dir: dir,
            root: base.clone(),
            base,
            scratch,
        }
    }

    fn write_json(&self, name: &str, value: &Value) {
        std::fs::write(self.scratch.join(name), value.to_string()).unwrap();
    }

    fn support_scripts(&self) {
        for name in SUPPORT {
            std::fs::write(self.scratch.join(name), format!("support:{name}\n")).unwrap();
        }
    }

    fn sections(&self, register_id: &str, text: &str) {
        let folder = self.root.join("markdown").join(register_id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("sections.jsonl"), text).unwrap();
    }

    /// `finalize.main(retrieved)`: its outcome and what it printed.
    fn run(&self, retrieved: &str) -> (Result<(), Exc>, String) {
        let mut run = Run::default();
        let outcome = finalize::run(&self.scratch.to_string_lossy(), retrieved, &mut run);
        (outcome, String::from_utf8_lossy(&run.stdout).into_owned())
    }

    fn sources(&self) -> Value {
        serde_json::from_str(&read(&self.root.join("sources.json"))).unwrap()
    }

    fn previous_outputs(&self) {
        for (name, _) in OUTPUT_HASHES {
            std::fs::write(self.root.join(name), "previous\n").unwrap();
        }
    }

    fn assert_previous_outputs(&self) {
        for (name, _) in OUTPUT_HASHES {
            assert_eq!(read(&self.root.join(name)), "previous\n", "{name}");
        }
    }
}

/// `write_fixture(scratch, root)` in test_finalize_phases.py.
fn phase_fixture() -> Tree {
    let tree = Tree::checkout();
    let act = json!({
        "id": "C2026A00001", "name": "Zulu Tax Act | Example", "collection": "Act",
        "versionStart": "2026-07-01", "compilationNumber": "7", "epub": "C2026A00001.epub",
        "bytes": 1000, "sourceUrl": "https://example.test/C2026A00001",
        "markdown": "C2026A00001/C2026A00001.md", "retrieved": "2026-08-01", "sections": 2,
        "granularity": "section", "words": 11, "endnotes": true,
        "long_title": "An Act used only as a fabricated test fixture",
    });
    let instrument = json!({
        "id": "F2026L00002", "name": "Alpha Superannuation Instrument",
        "collection": "LegislativeInstrument", "versionStart": "2026-06-01",
        "compilationNumber": "2", "epub": "F2026L00002.epub", "bytes": 2000,
        "sourceUrl": "https://example.test/F2026L00002",
        "markdown": "F2026L00002/F2026L00002.md", "retrieved": "2026-08-02", "sections": 1,
        "granularity": "table_block", "words": 22, "endnotes": false, "long_title": null,
        "version_is_current": false, "current_version_start": "2026-08-03",
    });
    let missing = json!({
        "id": "F2026N00003", "name": "Missing Medicare Levy Notice",
        "collection": "NotifiableInstrument", "versionStart": "2026-05-01", "epub": null,
        "bytes": 0, "sourceUrl": "https://example.test/F2026N00003",
        "reason": "current_version_has_no_document",
    });
    tree.write_json("manifest_md.json", &json!([act, instrument]));
    tree.write_json("manifest_raw.json", &json!([act, instrument, missing]));
    tree.write_json(
        "pii_flagged.json",
        &json!([{"names_est": 123}, {"names_est": 78}]),
    );
    tree.support_scripts();
    let line = |v: Value| format!("{v}\n");
    tree.sections(
        "C2026A00001",
        &(line(json!({"section": "1", "kind": "section", "text": "one"}))
            + &line(json!({"section": null, "kind": "container", "text": "two"}))),
    );
    tree.sections(
        "F2026L00002",
        &line(json!({"section": null, "kind": "introductory", "text": "three"})),
    );
    tree
}

fn runtime_error(outcome: Result<(), Exc>) -> String {
    let error = outcome.expect_err("expected the stage to fail");
    assert_eq!(error.class(), "RuntimeError", "{error}");
    error.to_string()
}

/// test_publication_documents_preserve_exact_canonical_bytes,
/// test_finish_publication_phase_preserves_readme_scripts_and_report and
/// test_main_keeps_output_stdout_and_support_script_bytes_exact.
#[test]
fn the_phase_fixture_publishes_pythons_bytes() {
    let tree = phase_fixture();
    let (outcome, stdout) = tree.run(DATE);
    outcome.unwrap();
    for (name, digest) in OUTPUT_HASHES {
        assert_eq!(
            canonical_hash(&read(&tree.root.join(name))),
            digest,
            "{name}"
        );
    }
    assert_eq!(stdout, STDOUT);
    for name in SUPPORT {
        assert_eq!(
            std::fs::read(tree.root.join(name)).unwrap(),
            std::fs::read(tree.scratch.join(name)).unwrap(),
            "{name}"
        );
    }
    let built: Vec<_> = std::fs::read_dir(tree.root.join("build"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(built, ["build-record.json"]);
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest_of(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap();
    json!({"bytes": bytes.len(), "sha256": sha256_hex(&bytes)})
}

/// The build record: what it states, and that every digest is of the bytes on disk.
#[test]
fn the_build_record_describes_the_finalisation() {
    let tree = phase_fixture();
    tree.run(DATE).0.unwrap();
    let record: Value =
        serde_json::from_str(&read(&tree.root.join("build/build-record.json"))).unwrap();
    assert_eq!(record["schema"], "au-tax-corpus-build-record.v1");
    assert_eq!(record["scope"], "finalize");
    let at = record["finalized_at"].as_str().unwrap();
    assert!(
        at.len() == 20 && at.ends_with('Z') && at.as_bytes()[10] == b'T',
        "{at}"
    );
    assert_eq!(
        record["source"],
        json!({
            "repository": "https://github.com/ryanduguid/au-tax-legislation-corpus",
            "revision": null, "dirty": null,
            "reason": "the binary carries no verified record of the source it was built from",
        })
    );
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        record["implementation"],
        json!({
            "language": "rust",
            "program": exe.file_name().unwrap().to_string_lossy(),
            "version": env!("CARGO_PKG_VERSION"),
            "sha256": sha256_hex(&std::fs::read(&exe).unwrap()),
            "cargo_lock_sha256": null,
            "cargo_lock_reason": "the binary carries no record of the lockfile it was built with",
        })
    );
    let input = |root: &str, path: &str, file: &Path| {
        let mut entry = json!({"root": root, "path": path});
        entry
            .as_object_mut()
            .unwrap()
            .extend(digest_of(file).as_object().unwrap().clone());
        entry
    };
    let sections = |rid: &str| {
        let path = format!("markdown/{rid}/sections.jsonl");
        input("corpus", &path, &tree.root.join(&path))
    };
    let module = |name: &str| input("module", name, &tree.scratch.join(name));
    assert_eq!(
        record["inputs"],
        json!([
            sections("C2026A00001"),
            sections("F2026L00002"),
            module("manifest_md.json"),
            module("manifest_raw.json"),
            module("pii_flagged.json"),
        ])
    );
    assert_eq!(record["lineage"]["known"], json!([]));
    let tools: Vec<Value> = SUPPORT
        .iter()
        .map(|name| {
            let mut entry = json!({"name": name});
            entry.as_object_mut().unwrap().extend(
                digest_of(&tree.root.join(name))
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            entry
        })
        .collect();
    assert_eq!(
        record["shipped_tools"],
        json!({"requires": "Python 3.14 or later, standard library only", "files": tools})
    );
    assert_eq!(
        record["environment"]["platform"],
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    );
    assert!(read(&tree.root.join("README.md")).contains("\n## Build record\n"));
}

/// An earlier finalize's builder copies in build/ stop the stage before it
/// changes anything; the copies stay.
#[test]
fn a_build_holding_the_python_builder_is_refused_before_any_output() {
    for name in BUILDER {
        let tree = phase_fixture();
        tree.previous_outputs();
        let build = tree.root.join("build");
        std::fs::create_dir(&build).unwrap();
        std::fs::write(build.join(name), "old\n").unwrap();
        let message = runtime_error(tree.run(DATE).0);
        assert!(
            message.contains(&format!(
                "holds builder scripts an earlier finalize copied there ({name})"
            )),
            "{message}"
        );
        tree.assert_previous_outputs();
        assert_eq!(read(&build.join(name)), "old\n");
        assert!(!build.join("build-record.json").exists());
    }
}

/// A rerun discards the earlier record before its first write, so a run that
/// fails part way leaves none.
#[test]
fn a_failed_rerun_leaves_no_record() {
    let tree = phase_fixture();
    tree.run(DATE).0.unwrap();
    let record = tree.root.join("build/build-record.json");
    assert!(record.exists());
    // README.md is written after sources.json, INDEX.md and LICENCE-NOTICE.md.
    std::fs::remove_file(tree.root.join("README.md")).unwrap();
    std::fs::create_dir(tree.root.join("README.md")).unwrap();
    assert!(tree.run(DATE).0.is_err());
    assert!(!record.exists());
}

/// A record path that cannot be removed (here a directory) stops the stage
/// before it changes anything, rather than leaving that record beside new
/// outputs.
#[test]
fn a_record_that_cannot_be_discarded_stops_the_stage_first() {
    let tree = phase_fixture();
    tree.previous_outputs();
    std::fs::create_dir_all(tree.root.join("build/build-record.json")).unwrap();
    assert!(tree.run(DATE).0.is_err());
    tree.assert_previous_outputs();
    assert!(tree.root.join("build/build-record.json").is_dir());
}

/// A build/ the stage cannot list stops it before any output, rather than
/// counting its builder copies as absent. Skipped where the permission bits
/// do not bind (as root).
#[cfg(unix)]
#[test]
fn an_unreadable_build_stops_the_stage_first() {
    use std::os::unix::fs::PermissionsExt;
    let tree = phase_fixture();
    tree.previous_outputs();
    let build = tree.root.join("build");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(build.join("finalize.py"), "old\n").unwrap();
    std::fs::set_permissions(&build, std::fs::Permissions::from_mode(0o000)).unwrap();
    let binds = std::fs::symlink_metadata(build.join("finalize.py")).is_err();
    let outcome = tree.run(DATE).0;
    std::fs::set_permissions(&build, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !binds {
        return;
    }
    assert!(outcome.is_err());
    tree.assert_previous_outputs();
    assert_eq!(read(&build.join("finalize.py")), "old\n");
}

/// With a directory where a checker goes, `shutil.copy` puts the checker
/// inside it; the record names that file and the bytes installed there.
#[test]
fn the_record_names_a_checker_copied_into_a_directory() {
    let tree = phase_fixture();
    std::fs::create_dir(tree.root.join("check_current.py")).unwrap();
    tree.run(DATE).0.unwrap();
    let record: Value =
        serde_json::from_str(&read(&tree.root.join("build/build-record.json"))).unwrap();
    let first = &record["shipped_tools"]["files"][0];
    assert_eq!(first["name"], "check_current.py/check_current.py");
    let installed = tree.root.join("check_current.py/check_current.py");
    assert_eq!(first["sha256"], digest_of(&installed)["sha256"]);
}

/// Totals that cannot be printed (an EPUB byte count past a float) fail the
/// stage before the record is installed; the other outputs are written, as
/// Python writes them before its report.
#[test]
fn a_report_that_fails_leaves_no_record() {
    let tree = phase_fixture();
    let md = std::fs::read_to_string(tree.scratch.join("manifest_md.json")).unwrap();
    let huge = format!("1{}", "0".repeat(400));
    let md = md.replacen("\"bytes\":1000", &format!("\"bytes\":{huge}"), 1);
    std::fs::write(tree.scratch.join("manifest_md.json"), &md).unwrap();
    let error = tree.run(DATE).0.expect_err("expected an overflow");
    assert_eq!(error.class(), "OverflowError", "{error}");
    assert!(tree.root.join("README.md").exists());
    assert!(!tree.root.join("build/build-record.json").exists());
}

/// test_retrieval_inventory_and_assembly_are_direct_exact_phases, read from
/// sources.json and the report.
#[test]
fn inventory_assembly_matches_python() {
    let tree = phase_fixture();
    let (outcome, stdout) = tree.run(DATE);
    outcome.unwrap();
    assert_eq!(stdout, STDOUT);
    let sources = tree.sources();
    let ids = |key: &str| -> Vec<Value> {
        sources[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["register_id"].clone())
            .collect()
    };
    assert_eq!(ids("titles_without_epub"), [json!("F2026N00003")]);
    assert_eq!(ids("titles"), [json!("F2026L00002"), json!("C2026A00001")]);
    let rows: Vec<&Value> = sources["titles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["jsonl_rows"])
        .collect();
    assert_eq!(rows, [&json!(1), &json!(2)]);
    let counts = &sources["counts"];
    let by_collection = counts["by_collection"].as_object().unwrap();
    assert_eq!(
        by_collection.keys().collect::<Vec<_>>(),
        ["LegislativeInstrument", "Act"]
    );
    assert_eq!(
        counts["by_collection"],
        json!({
            "LegislativeInstrument": {"titles": 1, "rows": 1, "words": 22},
            "Act": {"titles": 1, "rows": 2, "words": 11},
        })
    );
    // rows_by_kind {"introductory": 1, "section": 1, "container": 1}; the
    // section count is the one row with a section id.
    for (key, value) in [
        ("jsonl_rows", 3),
        ("rows_with_section_id", 1),
        ("words_body_only", 33),
        ("epub_bytes", 3000),
        ("acts", 1),
        ("instruments", 1),
        ("rows_introductory", 1),
        ("rows_container", 1),
        ("rows_unnumbered", 0),
    ] {
        assert_eq!(counts[key], value, "{key}");
    }
}

#[test]
fn pii_summary_rounds_the_committed_scan() {
    let tree = phase_fixture();
    let committed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fadden")
        .join("pii_flagged.json");
    std::fs::copy(committed, tree.scratch.join("pii_flagged.json")).unwrap();
    tree.run(DATE).0.unwrap();
    // The committed scan: 12 titles, 5,404 name mentions, rounded to 5,400.
    let readme = read(&tree.root.join("README.md"));
    assert!(readme.contains("**12 titles name people.**"), "{readme}");
    assert!(readme.contains("about 5,400 name mentions"), "{readme}");

    std::fs::remove_file(tree.scratch.join("pii_flagged.json")).unwrap();
    assert!(runtime_error(tree.run(DATE).0).contains("pii_flagged.json"));

    tree.write_json(
        "pii_flagged.json",
        &json!([
            {"register_id": "F2023N00096", "names_est": 130},
            {"register_id": "F2021N00219", "names_est": 220},
        ]),
    );
    tree.run(DATE).0.unwrap();
    let readme = read(&tree.root.join("README.md"));
    assert!(readme.contains("**2 titles name people.**"), "{readme}");
    assert!(readme.contains("about 350 name mentions"), "{readme}");
}

#[test]
fn a_missing_pii_scan_leaves_publication_files_unchanged() {
    let tree = phase_fixture();
    std::fs::remove_file(tree.scratch.join("pii_flagged.json")).unwrap();
    tree.previous_outputs();
    assert!(runtime_error(tree.run(DATE).0).contains("pii_flagged.json"));
    tree.assert_previous_outputs();
}

#[test]
fn unlisted_markdown_entries_stop_finalize() {
    // test_an_unlisted_title_directory_stops_finalize_before_any_write.
    let tree = phase_fixture();
    let markdown = tree.root.join("markdown");
    std::fs::create_dir(markdown.join("C2020A00099")).unwrap();
    std::fs::create_dir(markdown.join(".cache")).unwrap();
    tree.previous_outputs();
    let message = runtime_error(tree.run(DATE).0);
    let pattern = regex::Regex::new(r"does not list, .*: C2020A00099\. ").unwrap();
    assert!(pattern.is_match(&message), "{message}");
    tree.assert_previous_outputs();
    assert!(markdown.join("C2020A00099").is_dir());

    // test_an_unlisted_entry_of_any_type_stops_finalize.
    let tree = phase_fixture();
    let markdown = tree.root.join("markdown");
    std::fs::write(markdown.join("C2020A00098"), "stale\n").unwrap();
    let target = tree.base.join("missing");
    let link = markdown.join("C2020A00097");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&target, &link).is_ok();
    // Windows without the symlink privilege; the file case still runs.
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&target, &link).is_ok();
    let expected = if linked {
        "C2020A00097, C2020A00098"
    } else {
        "C2020A00098"
    };
    let message = runtime_error(tree.run(DATE).0);
    assert!(message.contains(&format!(": {expected}. ")), "{message}");
}

#[test]
fn an_unlisted_collection_gets_its_own_index_section() {
    let tree = Tree::checkout();
    let gazette = json!({
        "id": "C2026G00004", "name": "Gazette Notice", "collection": "Gazette",
        "epub": "C2026G00004.epub", "bytes": 10, "markdown": "C2026G00004/C2026G00004.md",
        "words": 3,
    });
    tree.write_json("manifest_md.json", &json!([gazette]));
    tree.write_json("manifest_raw.json", &json!([gazette]));
    tree.write_json("pii_flagged.json", &json!([]));
    tree.support_scripts();
    tree.sections(
        "C2026G00004",
        &format!(
            "\n{}\n\n",
            json!({"section": "1", "kind": "section", "text": "notice"})
        ),
    );
    let (outcome, stdout) = tree.run(DATE);
    outcome.unwrap();
    assert_eq!(tree.sources()["counts"]["jsonl_rows"], 1);
    assert!(stdout.contains(" rows=1 "), "{stdout}");
    let index = read(&tree.root.join("INDEX.md"));
    assert!(index.contains("## Other collections (1)"), "{index}");
    assert!(
        index.contains(
            "| [Gazette Notice](markdown/C2026G00004/C2026G00004.md) | Gazette | C2026G00004 | 1 |"
        ),
        "{index}"
    );
    assert!(!index.contains("Titles with no EPUB available"), "{index}");
}

/// test_regressions.py::FinalizeMissingTitleTests, from the deployed build
/// directory as Python runs it.
#[test]
fn a_missing_title_reports_its_recorded_reason() {
    let tree = Tree::deployed();
    tree.support_scripts();
    let ok = json!({
        "id": "C2004A00001", "name": "Example Tax Act", "collection": "Act", "status": "ok",
        "versionStart": "2026-01-01", "compilationNumber": "1", "epub": "C2004A00001.epub",
        "bytes": 100, "sourceUrl": "https://example.test/C2004A00001",
        "markdown": "C2004A00001/C2004A00001.md", "retrieved": "2026-08-03", "sections": 1,
        "granularity": "section", "words": 4, "endnotes": false,
    });
    let missing = json!({
        "id": "F2020L01498", "name": "Unpublished Instrument",
        "collection": "LegislativeInstrument", "epub": null, "bytes": 0, "status": "no_epub",
        "reason": "current_version_has_no_document", "versionStart": "2026-03-01",
        "sourceUrl": "https://example.test/F2020L01498",
    });
    tree.write_json("manifest_md.json", &json!([ok]));
    tree.write_json("manifest_raw.json", &json!([ok, missing]));
    std::fs::write(tree.scratch.join("pii_flagged.json"), "[]\n").unwrap();
    tree.sections(
        "C2004A00001",
        &format!(
            "{}\n",
            json!({"register_id": "C2004A00001", "section": "1", "kind": "section",
                   "text": "The rate is 10%."})
        ),
    );
    tree.run("2026-08-03").0.unwrap();
    let entry = &tree.sources()["titles_without_epub"][0];
    assert_eq!(entry["reason"], "current_version_has_no_document");
    assert!(entry.get("http_code").is_none());
    let index = read(&tree.root.join("INDEX.md"));
    assert!(index.contains("| Reason |"), "{index}");
    assert!(
        index.contains(
            "| Unpublished Instrument | LegislativeInstrument | F2020L01498 \
             | current\\_version\\_has\\_no\\_document |"
        ),
        "{index}"
    );
    assert!(!index.contains("| None | None |"), "{index}");
}
