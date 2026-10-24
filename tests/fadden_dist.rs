//! `fadden dist` and `fadden dist_verify`: ported from DistributionTests,
//! DistTableBlockParagraphTests and DistVerifyExitStatusTests in
//! tests/corpus/test_regressions.py and from tests/corpus/test_dist_paths.py.
//! Every title, row and contact is fabricated.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::{dist, dist_verify};
use tax_radar_au::pyjson::{self, Json};
use tax_radar_au::pyos::OsError;

const PUBLIC_ID: &str = "C2004A00001";
const PRIVATE_ID: &str = "C2004A00002";
const PUBLIC_NAME: &str = "Public Tax Act";
const PRIVATE_NAME: &str = "Private Disciplinary Register";
const ALL_CAPS_ROW: &str = "| SMITH, John | 12345678 | s 30-15 |\n\
                            | McDonald, Anne | 23456789 | s 30-15 |\n\
                            | O'Brien, Patrick | 34567890 | s 30-20 |";

fn pyjson_value(value: &Value) -> Json {
    pyjson::loads_standard(&value.to_string()).unwrap()
}

/// A deployed corpus: the corpus files in the base directory and the module
/// files (the flagged list and the shipped contact allowlist) in `build/`.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    build: PathBuf,
}

impl Fixture {
    fn dist(&self) -> PathBuf {
        self.root.join("dist")
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let build = root.join("build");
    std::fs::create_dir(&build).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fadden")
            .join("pii_contact_allowlist.json"),
        build.join("pii_contact_allowlist.json"),
    )
    .unwrap();
    write(
        &build.join("pii_flagged.json"),
        &json!([{"register_id": PRIVATE_ID, "name": PRIVATE_NAME, "rows_total": 1, "rows_flagged": 1, "names_est": 3, "words": 3}])
            .to_string(),
    );
    let public = json!({"register_id": PUBLIC_ID, "act": PUBLIC_NAME, "collection": "Act", "section": "1",
                        "heading": "Public rate", "kind": "section", "text": "The public rate is 10%."});
    let private = json!({"register_id": PRIVATE_ID, "act": PRIVATE_NAME, "collection": "NotifiableInstrument",
                         "section": "1", "heading": "Private register", "kind": "section", "text": "Private row with 20%."});
    let mut titles = Vec::new();
    for row in [&public, &private] {
        let rid = row["register_id"].as_str().unwrap();
        write(
            &root.join("markdown").join(rid).join("sections.jsonl"),
            &format!("{row}\n"),
        );
        write(
            &root.join("markdown").join(rid).join(format!("{rid}.md")),
            &format!("# {}\n", row["act"].as_str().unwrap()),
        );
        titles.push(json!({
            "register_id": rid, "name": row["act"], "collection": row["collection"], "keywords_in_name": ["Tax"],
            "jsonl_rows": 1, "words": row["text"].as_str().unwrap().split_whitespace().count(),
            "granularity": "section", "endnotes": false, "version_is_current": true,
            "epub": format!("epub/{rid}.epub"), "markdown": format!("markdown/{rid}/{rid}.md"),
            "sections_jsonl": format!("markdown/{rid}/sections.jsonl"),
        }));
    }
    let sources = json!({
        "titles": titles,
        "counts": {"titles": 2, "acts": 1, "instruments": 1, "jsonl_rows": 2, "words_body_only": 10, "epub_bytes": 200},
        "titles_without_epub": [], "titles_not_current_version": [],
    });
    write(&root.join("sources.json"), &sources.to_string());
    write(&root.join("LICENCE-NOTICE.md"), "licence\n");
    write(
        &root.join("INDEX.md"),
        "# Index\n\n2 titles (1 Acts, 1 instruments), 2 retrieval rows, 10 words.\n\n\
         | Collection | Titles | Rows | Words |\n|---|---|---|---|\n\
         | Acts | 1 | 1 | 5 |\n| Notifiable instruments | 1 | 1 | 5 |\n\n\
         ## Acts (1)\n| [Public Tax Act](markdown/C2004A00001) | C2004A00001 |\n\n\
         ## Notifiable instruments (1)\n| [Private Disciplinary Register](markdown/C2004A00002) | C2004A00002 |\n",
    );
    write(
        &root.join("README.md"),
        "2 in-force principal titles covering tax.\n\n\
         1 Acts and 1 legislative and notifiable instruments. 2 retrieval rows, 10 words.\n\n\
         Each title is stored as:\n\n\
         - `epub/<register_id>.epub` - the file exactly as the Register served it\n\
         - `markdown/<register_id>/<register_id>.md` - full text\n\n\
         **Eleven titles name people.** The corpus carries about 3 name mentions\n\
         of disciplined agents with their registration numbers.\n\n\
         **0 titles are not the current text.** Placeholder.\n",
    );
    let mut rates = [
        json!({"register_id": PRIVATE_ID, "act": PRIVATE_NAME, "collection": "NotifiableInstrument", "section": "1",
               "heading": "Private rate", "topic": "income tax rates", "kind": "rate", "amounts": ["20%"],
               "content": "Private rate content must not be distributed.", "rate_id": "R00001"}),
        json!({"register_id": PUBLIC_ID, "act": PUBLIC_NAME, "collection": "Act", "section": "1", "heading": "Public rate",
               "topic": "income tax rates", "kind": "rate", "amounts": ["10%"], "content": "The public rate is 10%.",
               "rate_id": "R00002"}),
    ];
    rates[0]["rate_id"] = json!("R00001");
    write(
        &root.join("rates").join("rates.jsonl"),
        &rates.iter().map(|r| format!("{r}\n")).collect::<String>(),
    );
    write(
        &root.join("rates").join("RATES.md"),
        "2 entries across 2 titles.\nPrivate Disciplinary Register\n",
    );
    Fixture {
        _dir: dir,
        root,
        build,
    }
}

fn stage(f: &Fixture, name: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg(name)
        .env("FADDEN_MODULE_DIR", &f.build)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

fn publish(f: &Fixture) -> PathBuf {
    let (code, _, err) = stage(f, "dist");
    assert_eq!(code, 0, "{err}");
    f.dist()
}

fn verify(f: &Fixture) -> (i32, String) {
    let (code, out, _) = stage(f, "dist_verify");
    (code, out)
}

/// The PASS or FAIL word after a check's label.
fn status(output: &str, label: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(label))
        .collect();
    assert_eq!(lines.len(), 1, "{output}");
    lines[0][label.len()..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn distribution_rebuilds_counts_and_rates_markdown_from_filtered_rows() {
    let f = fixture();
    let output = publish(&f);
    let mut sources = read_json(&output.join("sources.json"));
    assert_eq!(sources["counts"]["titles"], 1);
    assert_eq!(sources["counts"]["jsonl_rows"], 1);
    assert_eq!(sources["counts"]["epub_bytes"], 0);
    assert_eq!(sources["titles_count"], 1);
    assert_eq!(sources["titles"][0]["epub"], Value::Null);
    assert_eq!(sources["titles"][0]["epub_included"], false);
    let index = std::fs::read_to_string(output.join("INDEX.md")).unwrap();
    assert!(index.contains(PUBLIC_NAME) && !index.contains(PRIVATE_NAME));
    let rates_md = std::fs::read_to_string(output.join("rates").join("RATES.md")).unwrap();
    let rates: Vec<Value> = std::fs::read_to_string(output.join("rates").join("rates.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rates
            .iter()
            .map(|r| r["rate_id"].clone())
            .collect::<Vec<_>>(),
        [json!("R00001")]
    );
    assert!(rates_md.contains("1 entries across 1 titles."));
    assert!(rates_md.contains(PUBLIC_NAME) && !rates_md.contains(PRIVATE_NAME));
    let readme = std::fs::read_to_string(output.join("README.md")).unwrap();
    for absent in [
        "epub/<register_id>.epub",
        "titles name people",
        "disciplined agents",
    ] {
        assert!(!readme.contains(absent), "{absent}");
    }
    assert!(
        readme.contains("markdown/<register_id>/<register_id>.md") && readme.contains("REMOVED.md")
    );
    let policy = read_json(&f.build.join("pii_contact_allowlist.json"));
    let mut unique: Vec<(String, String)> = policy["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().into(),
                e["sha256"].as_str().unwrap().into(),
            )
        })
        .collect();
    unique.sort();
    unique.dedup();
    let removed = std::fs::read_to_string(output.join("REMOVED.md")).unwrap();
    assert!(removed.contains(&format!(
        "{} unique organisational identifiers",
        unique.len()
    )));

    assert_eq!(verify(&f).0, 0);
    // Nested machine-readable counts that drift from the shipped files fail.
    sources["counts"]["jsonl_rows"] = json!(2);
    std::fs::write(output.join("sources.json"), sources.to_string()).unwrap();
    assert_eq!(verify(&f).0, 1);
    // A full-corpus RATES.md carrying a removed title fails too.
    sources["counts"]["jsonl_rows"] = json!(1);
    std::fs::write(output.join("sources.json"), sources.to_string()).unwrap();
    let md = output.join("rates").join("RATES.md");
    let text = std::fs::read_to_string(&md).unwrap();
    std::fs::write(&md, format!("{text}{PRIVATE_NAME}\n")).unwrap();
    assert_eq!(verify(&f).0, 1);
}

#[test]
fn the_verifier_fails_on_each_claim_it_reports_as_passing() {
    let f = fixture();
    let output = publish(&f);
    let (code, out) = verify(&f);
    assert_eq!(code, 0);
    for label in [
        "no row names private individuals",
        "no unapproved contact identifiers",
        "title files stay inside real link-free directories",
        "all distributed title files are validated UTF-8 text",
        "title file inventory matches declared outputs",
        "no title file names private individuals",
    ] {
        assert_eq!(status(&out, label), "PASS", "{label}");
    }
    let rows = output
        .join("markdown")
        .join(PUBLIC_ID)
        .join("sections.jsonl");
    let clean_rows = std::fs::read_to_string(&rows).unwrap();
    let row: Value = serde_json::from_str(clean_rows.trim()).unwrap();
    let markdown = output
        .join("markdown")
        .join(PUBLIC_ID)
        .join(format!("{PUBLIC_ID}.md"));
    let clean_markdown = std::fs::read_to_string(&markdown).unwrap();
    let sources_path = output.join("sources.json");
    let clean_sources = std::fs::read_to_string(&sources_path).unwrap();
    let with_text = |text: &str| {
        let mut r = row.clone();
        r["text"] = json!(text);
        format!("{r}\n")
    };
    let contact = "new-contact@example.test";
    let expect = |label: &str| {
        let (code, out) = verify(&f);
        assert_eq!(
            (code, status(&out, label)),
            (1, "FAIL".to_string()),
            "{label}\n{out}"
        );
        out
    };

    std::fs::write(&rows, with_text(ALL_CAPS_ROW)).unwrap();
    expect("no row names private individuals");
    std::fs::write(&rows, with_text(contact)).unwrap();
    assert!(!expect("no unapproved contact identifiers").contains(contact));
    std::fs::write(&rows, "[]\n").unwrap();
    expect("every JSONL row parses");
    std::fs::write(&rows, b"\xff\xfe").unwrap();
    expect("all distributed title files are validated UTF-8 text");
    std::fs::write(&rows, &clean_rows).unwrap();

    std::fs::write(&markdown, format!("{clean_markdown}\n{contact}\n")).unwrap();
    assert!(!expect("no unapproved contact identifiers").contains(contact));
    std::fs::write(
        &markdown,
        format!("{clean_markdown}\n| Smith, John | 12345678 | s 30-15 |\n"),
    )
    .unwrap();
    let out = expect("no title file names private individuals");
    assert_eq!(status(&out, "no row names private individuals"), "PASS");
    assert!(!out.contains("Smith, John"));
    std::fs::write(&markdown, &clean_markdown).unwrap();
    std::fs::write(&rows, with_text("| Smith, John | 12345678 | s 30-15 |")).unwrap();
    expect("no row names private individuals");
    std::fs::write(&rows, &clean_rows).unwrap();

    let stray = output.join("markdown").join(PRIVATE_ID);
    std::fs::create_dir(&stray).unwrap();
    expect("no removed title present");
    std::fs::remove_dir(&stray).unwrap();

    let planted = output.join("markdown").join(PUBLIC_ID).join("figure.png");
    std::fs::write(&planted, b"\x89PNG\r\n\x1a\n").unwrap();
    let out = expect("no image files");
    assert_eq!(
        status(&out, "all distributed title files are validated UTF-8 text"),
        "FAIL"
    );
    assert_eq!(
        status(&out, "title file inventory matches declared outputs"),
        "FAIL"
    );
    std::fs::remove_file(&planted).unwrap();

    std::fs::write(&markdown, b"GIF89a\x00\x01").unwrap();
    expect("all distributed title files are validated UTF-8 text");
    std::fs::write(&markdown, &clean_markdown).unwrap();
    let hidden = output
        .join("markdown")
        .join(PUBLIC_ID)
        .join(".operator-notes");
    std::fs::write(&hidden, "scratch\n").unwrap();
    expect("title file inventory matches declared outputs");
    std::fs::remove_file(&hidden).unwrap();

    let mut document: Value = serde_json::from_str(&clean_sources).unwrap();
    document["titles"][0]["markdown"] = json!("../../outside.md");
    std::fs::write(&sources_path, document.to_string()).unwrap();
    expect("title file inventory matches declared outputs");
    let mut document: Value = serde_json::from_str(&clean_sources).unwrap();
    document["counts"]["acts"] = json!(9);
    document["counts"]["instruments"] = json!(9);
    std::fs::write(&sources_path, document.to_string()).unwrap();
    expect("sources count: acts and instruments");
    std::fs::write(&sources_path, &clean_sources).unwrap();

    let readme = output.join("README.md");
    let clean_readme = std::fs::read_to_string(&readme).unwrap();
    std::fs::write(
        &readme,
        format!("{clean_readme}\n- `epub/<register_id>.epub` - as served\n"),
    )
    .unwrap();
    expect("README does not promise EPUB files");
    let drifted = clean_readme.replace(
        "1 Acts and 0 legislative and notifiable",
        "9 Acts and 9 legislative and notifiable",
    );
    assert_ne!(drifted, clean_readme);
    std::fs::write(&readme, drifted).unwrap();
    expect("README headline matches generated counts");
    std::fs::write(&readme, &clean_readme).unwrap();
    assert_eq!(verify(&f).0, 0);
}

#[test]
fn a_stray_entry_under_markdown_is_reported_not_raised() {
    let f = fixture();
    let output = publish(&f);
    let (code, out) = verify(&f);
    assert_eq!(
        (code, status(&out, "no directory beyond what is listed")),
        (0, "PASS".into())
    );
    let markdown = output.join("markdown");
    std::fs::write(markdown.join(".DS_Store"), "metadata\n").unwrap();
    std::fs::write(markdown.join("F2026N00001"), "not a title directory\n").unwrap();
    std::fs::write(markdown.join("notes.md"), "scratch\n").unwrap();
    std::fs::create_dir(markdown.join("draft-backup")).unwrap();
    std::fs::write(
        markdown.join("draft-backup").join("sections.jsonl"),
        "{\"register_id\": \"draft-backup\", \"text\": \"x\"}\n",
    )
    .unwrap();
    let (code, out) = verify(&f);
    assert_eq!(
        (code, status(&out, "no directory beyond what is listed")),
        (1, "FAIL".into())
    );
    for entry in [
        ".DS_Store [file]",
        "F2026N00001 [file]",
        "notes.md [file]",
        "draft-backup [directory]",
    ] {
        assert!(out.contains(entry), "{entry}\n{out}");
    }
    assert_eq!(status(&out, "no row names private individuals"), "PASS");
}

#[cfg(windows)]
#[test]
fn a_top_level_junction_is_a_named_failure() {
    let f = fixture();
    let output = publish(&f);
    let outside = f.root.join("outside-title");
    std::fs::create_dir(&outside).unwrap();
    let junction = output.join("markdown").join("F2026N00002");
    let made = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(made.status.success());
    let (code, out) = verify(&f);
    assert_eq!(
        (code, status(&out, "no directory beyond what is listed")),
        (1, "FAIL".into())
    );
    assert!(out.contains("F2026N00002 [junction]"), "{out}");
}

/// Make a symbolic link to a directory (`dir`) or file, or return false where
/// this Windows account lacks the privilege, so the test is skipped.
fn symlink(target: &Path, link: &Path, dir: bool) -> bool {
    #[cfg(unix)]
    let made = {
        let _ = dir;
        std::os::unix::fs::symlink(target, link)
    };
    #[cfg(windows)]
    let made = if dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match made {
        Ok(()) => true,
        // ERROR_PRIVILEGE_NOT_HELD: any other failure is a broken test.
        Err(error) if cfg!(windows) && error.raw_os_error() == Some(1314) => {
            eprintln!("skipped: symbolic links are unavailable to this account ({error})");
            false
        }
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn a_top_level_symlink_is_a_named_failure() {
    let f = fixture();
    let output = publish(&f);
    let outside = f.root.join("outside-title");
    std::fs::create_dir(&outside).unwrap();
    if !symlink(&outside, &output.join("markdown").join("F2026N00001"), true) {
        return;
    }
    let (code, out) = verify(&f);
    assert_eq!(
        (code, status(&out, "no directory beyond what is listed")),
        (1, "FAIL".into())
    );
    assert!(out.contains("F2026N00001 [symlink]"), "{out}");
}

#[test]
fn the_exit_status_has_three_bands() {
    let f = fixture();
    let output = publish(&f);
    let (code, out) = verify(&f);
    assert_eq!(code, 0);
    assert!(out.contains("all checks passed"));
    std::fs::remove_dir_all(output.join("markdown").join(PUBLIC_ID)).unwrap();
    let (code, out) = verify(&f);
    assert_eq!(code, 1);
    assert!(out.contains("FAILED") && !out.contains("VERIFIER ERROR"));
    for broken in ["missing", "malformed"] {
        let f = fixture();
        let output = publish(&f);
        if broken == "missing" {
            std::fs::remove_file(output.join("sources.json")).unwrap();
        } else {
            std::fs::write(output.join("sources.json"), "{not json").unwrap();
        }
        let (code, out, err) = stage(&f, "dist_verify");
        assert_eq!(code, 2, "{broken}");
        assert!(out.contains("VERIFIER ERROR") && out.contains("unverified, not clean"));
        assert!(!err.is_empty());
    }
}

#[test]
fn published_rows_blank_lines_and_malformed_rows_are_counted() {
    let f = fixture();
    let sections = f
        .root
        .join("markdown")
        .join(PUBLIC_ID)
        .join("sections.jsonl");
    let row: Value =
        serde_json::from_str(std::fs::read_to_string(&sections).unwrap().trim()).unwrap();
    let mut unnumbered = row.clone();
    unnumbered["section"] = Value::Null;
    unnumbered["kind"] = Value::Null;
    unnumbered["text"] = json!("Unnumbered words.");
    std::fs::write(&sections, format!("\n{row}\n\n{unnumbered}\n")).unwrap();
    let rates = f.root.join("rates").join("rates.jsonl");
    let text = std::fs::read_to_string(&rates).unwrap();
    std::fs::write(&rates, format!("\n{text}\n")).unwrap();
    let sources_path = f.root.join("sources.json");
    let mut sources = read_json(&sources_path);
    sources["titles"][0]["jsonl_rows"] = json!(2);
    std::fs::write(&sources_path, sources.to_string()).unwrap();
    let output = publish(&f);
    assert_eq!(
        read_json(&output.join("sources.json"))["counts"]["jsonl_rows"],
        2
    );
    assert_eq!(verify(&f).0, 0);

    let published = output
        .join("markdown")
        .join(PUBLIC_ID)
        .join("sections.jsonl");
    let cases = [
        (
            "every JSONL row parses",
            vec![
                {
                    let mut r = row.clone();
                    r["text"] = json!(["not", "text"]);
                    r
                },
                {
                    let mut r = row.clone();
                    r["kind"] = json!(7);
                    r
                },
            ],
        ),
        (
            "each JSONL row matches its title directory",
            vec![{
                let mut r = row.clone();
                r["register_id"] = json!(PRIVATE_ID);
                r
            }],
        ),
    ];
    for (label, rows) in cases {
        std::fs::write(
            &published,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let (code, out) = verify(&f);
        assert_eq!((code, status(&out, label)), (1, "FAIL".into()), "{label}");
    }
    std::fs::write(&published, format!("{row}\n")).unwrap();
    let published_rates = output.join("rates").join("rates.jsonl");
    let text = std::fs::read_to_string(&published_rates).unwrap();
    std::fs::write(&published_rates, format!("\n{text}[]\n")).unwrap();
    let (code, out) = verify(&f);
    assert_eq!(
        (code, status(&out, "every rates JSONL row parses")),
        (1, "FAIL".into())
    );
}

#[test]
fn the_contact_gate_names_eight_and_refuses_missing_files() {
    let f = fixture();
    let titles = [pyjson_value(&json!({"register_id": PUBLIC_ID}))];
    let root = f.root.to_string_lossy().to_string();
    let contacts: Vec<String> = (0..9).map(|n| format!("person{n}@example.test")).collect();
    std::fs::write(
        f.root
            .join("markdown")
            .join(PUBLIC_ID)
            .join(format!("{PUBLIC_ID}.md")),
        contacts.join(" "),
    )
    .unwrap();
    let title = pyjson_value(
        &json!({"register_id": PUBLIC_ID, "markdown": format!("markdown/{PUBLIC_ID}/{PUBLIC_ID}.md"),
                                     "sections_jsonl": format!("markdown/{PUBLIC_ID}/sections.jsonl"), "endnotes": false}),
    );
    let error = dist::reject_unapproved_contacts(&root, std::slice::from_ref(&title), &[])
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("unapproved contact identifiers: 9 ("),
        "{error}"
    );
    assert_eq!(error.matches("C2004A00001:email:").count(), 8);
    std::fs::remove_file(
        f.root
            .join("markdown")
            .join(PUBLIC_ID)
            .join("sections.jsonl"),
    )
    .unwrap();
    let error = dist::reject_unapproved_contacts(&root, &titles, &[])
        .unwrap_err()
        .to_string();
    assert_eq!(error, "listed title C2004A00001 has no sections.jsonl");
    std::fs::remove_dir_all(f.root.join("markdown").join(PUBLIC_ID)).unwrap();
    let error = dist::reject_unapproved_contacts(&root, &titles, &[])
        .unwrap_err()
        .to_string();
    assert_eq!(error, "listed title C2004A00001 has no markdown directory");
}

// ----------------------------------------------------------- paths

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn unusable_targets_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let a_file = dir.path().join("file");
    std::fs::write(&a_file, "").unwrap();
    let root = if cfg!(windows) {
        "C:\\".to_string()
    } else {
        "/".to_string()
    };
    for (target, message) in [
        (root, "must be a named child directory"),
        (
            s(&dir.path().join("missing").join("dist")),
            "parent does not exist",
        ),
        (s(&a_file), "must be a directory"),
    ] {
        let error = dist::validated_distribution_target(&target)
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
    let target = s(&dir.path().join("dist"));
    let error =
        dist::validated_managed_sibling(&target, &s(&dir.path().join(".dist.cache-x")), "cache")
            .unwrap_err();
    assert_eq!(error.to_string(), "unknown managed distribution path kind");
}

#[test]
fn a_linked_target_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("real")).unwrap();
    if !symlink(&dir.path().join("real"), &dir.path().join("dist"), true) {
        return;
    }
    let error = dist::validated_distribution_target(&s(&dir.path().join("dist")))
        .unwrap_err()
        .to_string();
    assert!(error.contains("must not be a link or reparse point"));
}

#[test]
fn sibling_allocation_gives_up_after_one_hundred_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let fixed = format!("{:032x}", 7);
    std::fs::create_dir(dir.path().join(format!(".dist.stage-{fixed}"))).unwrap();
    let mut calls = 0;
    let error = dist::new_managed_sibling(&s(&dir.path().join("dist")), "stage", || {
        calls += 1;
        fixed.clone()
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "could not allocate a unique distribution stage path"
    );
    assert_eq!(calls, 100);
}

#[test]
fn a_managed_path_that_is_a_file_is_not_removed() {
    let dir = tempfile::tempdir().unwrap();
    let stage = dir.path().join(format!(".dist.stage-{}", "0".repeat(32)));
    std::fs::write(&stage, "not a directory").unwrap();
    let error =
        dist::remove_managed_tree(&s(&dir.path().join("dist")), &s(&stage), "stage").unwrap_err();
    assert_eq!(
        error.to_string(),
        "refusing to remove a non-directory managed path"
    );
    assert!(stage.is_file());
}

struct Promotion {
    dir: tempfile::TempDir,
}

impl Promotion {
    fn target(&self) -> PathBuf {
        self.dir.path().join("dist")
    }
    fn stage(&self) -> PathBuf {
        self.dir
            .path()
            .join(format!(".dist.stage-{}", "1".repeat(32)))
    }
    fn promote(&self) -> Result<(), String> {
        dist::promote_distribution(&s(&self.stage()), &s(&self.target())).map_err(|e| e.to_string())
    }
}

fn promotion() -> Promotion {
    Promotion {
        dir: tempfile::tempdir().unwrap(),
    }
}

fn denied(path: &str) -> OsError {
    OsError::new(
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        Some(path),
        tax_radar_au::pyos::Style::Native,
    )
}

#[test]
fn promotion_refuses_a_stage_that_is_not_a_directory() {
    let p = promotion();
    std::fs::write(p.stage(), "").unwrap();
    assert_eq!(
        p.promote().unwrap_err(),
        "distribution stage must be a real directory"
    );
}

#[test]
fn a_first_publication_failure_leaves_nothing_to_restore() {
    let p = promotion();
    std::fs::create_dir(p.stage()).unwrap();
    dist::set_rename_hook(|src, _| Some(denied(src)));
    assert!(p.promote().is_err());
    dist::set_rename_hook(|_, _| None);
    assert!(!p.target().exists());
    assert!(p.stage().is_dir());
}

fn fail_second_rename(p: &Promotion, before_raising: impl Fn(&Path) + 'static) -> String {
    std::fs::create_dir(p.stage()).unwrap();
    std::fs::create_dir(p.target()).unwrap();
    std::fs::write(p.target().join("old.txt"), "old").unwrap();
    let mut first: Option<PathBuf> = None;
    dist::set_rename_hook(move |src, dst| match &first {
        None => {
            first = Some(PathBuf::from(dst));
            None
        }
        Some(backup) => {
            before_raising(backup);
            Some(denied(src))
        }
    });
    let message = p.promote().unwrap_err();
    dist::set_rename_hook(|_, _| None);
    message
}

#[test]
fn a_target_that_reappears_is_not_rolled_back_over() {
    let p = promotion();
    let target = p.target();
    let message = fail_second_rename(&p, move |_| std::fs::create_dir(&target).unwrap());
    assert!(
        message.contains("automatic rollback is unsafe"),
        "{message}"
    );
}

#[test]
fn a_backup_replaced_by_a_file_is_not_restored() {
    let p = promotion();
    let message = fail_second_rename(&p, |backup| {
        std::fs::remove_dir_all(backup).unwrap();
        std::fs::write(backup, "swapped").unwrap();
    });
    assert_eq!(message, "distribution backup is unsafe to restore");
    assert!(!p.target().exists());
}

#[test]
fn a_successful_promotion_replaces_the_old_tree_and_removes_the_backup() {
    let p = promotion();
    std::fs::create_dir(p.stage()).unwrap();
    std::fs::write(p.stage().join("new.txt"), "new").unwrap();
    std::fs::create_dir(p.target()).unwrap();
    std::fs::write(p.target().join("old.txt"), "old").unwrap();
    p.promote().unwrap();
    assert!(p.target().join("new.txt").is_file() && !p.target().join("old.txt").exists());
    let names: Vec<String> = std::fs::read_dir(p.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["dist"]);
}

#[test]
fn nested_directories_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("inner")).unwrap();
    std::fs::write(dir.path().join("inner").join("file.md"), "x").unwrap();
    let (files, directories) = dist_verify::title_tree(&s(dir.path())).unwrap();
    assert_eq!(directories, ["inner"]);
    assert_eq!(
        files
            .iter()
            .map(|f| Path::new(f)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned())
            .collect::<Vec<_>>(),
        ["file.md"]
    );
    assert_eq!(
        dist_verify::entry_kind(&s(&dir.path().join("absent"))),
        "unreadable"
    );
}

#[cfg(unix)]
#[test]
fn both_tree_walkers_refuse_a_named_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let title = dir.path().join("title");
    std::fs::create_dir(&title).unwrap();
    let pipe = std::ffi::CString::new(s(&title.join("pipe"))).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
    let error = dist_verify::title_tree(&s(&title)).unwrap_err();
    assert_eq!(
        (error.class(), error.to_string().as_str()),
        ("ValueError", "title tree contains a non-regular file")
    );
    let error = dist::title_tree(&s(&title)).unwrap_err();
    assert_eq!(
        (error.class(), error.to_string().as_str()),
        ("RuntimeError", "title tree contains a non-regular file")
    );
    assert_eq!(dist_verify::entry_kind(&s(&title.join("pipe"))), "other");
}

// --------------------------------------------------------- rendering

fn points(text: &[u32]) -> String {
    text.iter().map(|&cp| char::from_u32(cp).unwrap()).collect()
}

#[test]
fn long_rate_sections_are_truncated_with_a_count() {
    let mut records: Vec<Value> = (0..41)
        .map(|n| json!({"register_id": PUBLIC_ID, "act": "Act", "collection": "LegislativeInstrument", "section": n.to_string(),
                        "heading": "Table", "topic": "income tax rates", "kind": "table", "content": "| a |"}))
        .collect();
    records.extend((0..121).map(|_| json!({"register_id": PUBLIC_ID, "act": "Act", "collection": "Act", "section": null,
                                            "topic": "income tax rates", "kind": "rate", "amounts": ["1%"], "content": "One per cent."})));
    let records: Vec<_> = records.iter().map(pyjson_value).collect();
    let md = points(&dist::render_rates_markdown(&records).unwrap());
    for needle in [
        "### Rate tables",
        "**Act** _(instrument)_ s 0 \u{2014} Table",
        "_1 further tables in rates.jsonl._",
        "_1 further provisions in rates.jsonl._",
    ] {
        assert!(md.contains(needle), "{needle}");
    }
    let one = [pyjson_value(
        &json!({"register_id": PUBLIC_ID, "act": "Act", "collection": "Act", "section": "1",
                                     "heading": "Table", "topic": "medicare levy", "kind": "table", "content": "| a |"}),
    )];
    let md = points(&dist::render_rates_markdown(&one).unwrap());
    assert!(md.contains("**Act** s 1 \u{2014} Table"));
    assert!(!md.contains("further") && !md.contains("### Provisions carrying a number"));
}

#[test]
fn readme_count_phrases_are_rewritten_and_malformed_ones_kept() {
    let text = "x Acts and 2 legislative and notifiable. 1 Acts and 2 legislative and notifiable";
    assert_eq!(
        dist::replace_readme_collection_counts(text, 5, 6),
        "x Acts and 2 legislative and notifiable. 5 Acts and 6 legislative and notifiable"
    );
}

const SAMPLE: &str = "Intro paragraph.\n\n\
    **Table-shaped instruments are split on their tables.** 14 titles carry\n\
    `granularity: table_block`: no headings anywhere, just a run of tables. The Tax\n\
    Practitioners Board publishes its terminations this way, and Excise By-law No.\n\
    127 prescribes petroleum fields one table per basin.\n\n\
    Following paragraph.\n";

#[test]
fn the_table_block_paragraph_is_replaced_with_the_subset_figure() {
    let result = dist::replace_readme_table_block_paragraph(SAMPLE, 2);
    assert!(result.contains("2 titles carry") && !result.contains("14 titles carry"));
    assert!(
        !result.contains("Excise By-law No.")
            && !result.contains("publishes its terminations this way")
    );
    assert!(
        result.starts_with("Intro paragraph.\n\n") && result.ends_with("Following paragraph.\n")
    );
    assert_eq!(result.matches(dist::TABLE_BLOCK_LEAD).count(), 1);
    assert_eq!(
        dist::replace_readme_table_block_paragraph("No such paragraph here.\n", 2),
        "No such paragraph here.\n"
    );
    let hostile = dist::TABLE_BLOCK_LEAD.repeat(20_000);
    assert!(dist::replace_readme_table_block_paragraph(&hostile, 2).contains("2 titles"));
    // finalize writes the paragraph dist rewrites; a reworded lead would stop the rewrite.
    let finalize = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("fadden")
            .join("finalize.rs"),
    )
    .unwrap();
    assert!(finalize.contains(dist::TABLE_BLOCK_LEAD));
}

// ------------------------------------------------- publication safety

/// The stage and backup siblings `dist` may leave beside the target.
fn managed_paths(f: &Fixture) -> Vec<String> {
    std::fs::read_dir(&f.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".dist.stage-") || n.starts_with(".dist.backup-"))
        .collect()
}

fn with_existing_dist(f: &Fixture) -> PathBuf {
    std::fs::create_dir(f.dist()).unwrap();
    let marker = f.dist().join("last-publishable-build.txt");
    std::fs::write(&marker, "keep me").unwrap();
    marker
}

fn refused(f: &Fixture, needle: &str) -> String {
    let (code, _, err) = stage(f, "dist");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains(needle), "{err:?} lacks {needle:?}");
    err
}

fn empty_policy(f: &Fixture) {
    std::fs::write(
        f.build.join("pii_contact_allowlist.json"),
        r#"{"schema_version": 1, "entries": []}"#,
    )
    .unwrap();
}

fn declare_endnotes(f: &Fixture) {
    let path = f.root.join("sources.json");
    let mut sources = read_json(&path);
    for title in sources["titles"].as_array_mut().unwrap() {
        if title["register_id"] == PUBLIC_ID {
            title["endnotes"] = json!(format!("markdown/{PUBLIC_ID}/endnotes.md"));
        }
    }
    std::fs::write(&path, sources.to_string()).unwrap();
}

#[test]
fn preflight_findings_preserve_the_existing_distribution() {
    let contact = "markdown-only-contact@example.test";
    let pair = "| Smith, John | 12345678 | s 30-15 |";
    for file in [
        format!("{PUBLIC_ID}.md"),
        "endnotes.md".into(),
        "sections.jsonl".into(),
    ] {
        for (planted, needle, secret) in [
            (contact, "unapproved contact identifiers", contact),
            (pair, "private-person registration pairs", "Smith, John"),
        ] {
            let f = fixture();
            empty_policy(&f);
            let marker = with_existing_dist(&f);
            let path = f.root.join("markdown").join(PUBLIC_ID).join(&file);
            if file == "sections.jsonl" {
                let row = json!({"register_id": PUBLIC_ID, "text": planted});
                std::fs::write(&path, format!("{row}\n")).unwrap();
            } else {
                std::fs::write(&path, format!("{planted}\n")).unwrap();
            }
            if file == "endnotes.md" {
                declare_endnotes(&f);
            }
            let err = refused(&f, needle);
            assert!(!err.contains(secret), "{err}");
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
            assert!(managed_paths(&f).is_empty());
        }
    }
}

#[test]
fn binary_files_and_title_dotfiles_preserve_the_existing_distribution() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let markdown = f
        .root
        .join("markdown")
        .join(PUBLIC_ID)
        .join(format!("{PUBLIC_ID}.md"));
    std::fs::write(markdown, b"GIF89a\x00\x01").unwrap();
    refused(
        &f,
        "RuntimeError: listed title C2004A00001 contains an unreadable or non-text file",
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");

    let f = fixture();
    let marker = with_existing_dist(&f);
    std::fs::write(
        f.root
            .join("markdown")
            .join(PUBLIC_ID)
            .join(".operator-notes"),
        "scratch\n",
    )
    .unwrap();
    refused(&f, "unexpected file inventory");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_mid_build_failure_preserves_the_prior_distribution() {
    // The Python test made copytree raise; a corpus missing its licence notice
    // fails the build after the titles are copied into the stage.
    let f = fixture();
    let marker = with_existing_dist(&f);
    std::fs::remove_file(f.root.join("LICENCE-NOTICE.md")).unwrap();
    refused(&f, "FileNotFoundError: [Errno 2]");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_failed_staging_validation_preserves_the_prior_distribution() {
    // A README without the headline phrases builds but fails verification.
    let f = fixture();
    let marker = with_existing_dist(&f);
    std::fs::write(f.root.join("README.md"), "No headline here.\n").unwrap();
    refused(
        &f,
        "RuntimeError: staged distribution failed validation: README states the real title count",
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

fn publish_in_process(f: &Fixture) -> Result<(), String> {
    let mut state = tax_radar_au::fadden::Run::default();
    dist::publish(&f.build.to_string_lossy(), &mut state).map_err(|e| format!("{}: {e}", e.class()))
}

#[test]
fn a_first_promotion_rename_failure_leaves_the_old_dist_untouched() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = calls.clone();
    dist::set_rename_hook(move |src, _| {
        counter.set(counter.get() + 1);
        Some(denied(src))
    });
    let error = publish_in_process(&f).unwrap_err();
    dist::set_rename_hook(|_, _| None);
    assert!(error.to_lowercase().contains("permission"), "{error}");
    assert_eq!(calls.get(), 1);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_second_promotion_rename_failure_rolls_back_the_old_dist() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = calls.clone();
    dist::set_rename_hook(move |src, _| {
        counter.set(counter.get() + 1);
        (counter.get() == 2).then(|| denied(src))
    });
    let error = publish_in_process(&f).unwrap_err();
    dist::set_rename_hook(|_, _| None);
    assert!(error.to_lowercase().contains("permission"), "{error}");
    assert_eq!(calls.get(), 3);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_successful_promotion_removes_stage_and_backup() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    publish(&f);
    assert!(!marker.exists());
    assert!(
        f.dist()
            .join("markdown")
            .join(PUBLIC_ID)
            .join("sections.jsonl")
            .is_file()
    );
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn managed_cleanup_refuses_a_path_outside_the_dist_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("corpus").join("dist");
    std::fs::create_dir(target.parent().unwrap()).unwrap();
    let unrelated = dir.path().join(".dist.stage-unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    let error = dist::remove_managed_tree(&s(&target), &s(&unrelated), "stage").unwrap_err();
    assert!(error.to_string().contains("validated sibling"));
    assert!(unrelated.is_dir());
}

#[test]
fn accounting_standards_under_restrictive_notices_are_dropped_and_listed() {
    let f = fixture();
    let (standard, name) = ("F2015L01601", "AASB 112 - Income Taxes - August 2015");
    let folder = f.root.join("markdown").join(standard);
    let row = json!({"register_id": standard, "act": name, "collection": "LegislativeInstrument",
                     "section": "1", "heading": "Tax rate", "kind": "section", "text": "The tax rate is 30%."});
    write(&folder.join("sections.jsonl"), &format!("{row}\n"));
    write(&folder.join(format!("{standard}.md")), "# AASB 112\n");
    let path = f.root.join("sources.json");
    let mut sources = read_json(&path);
    let mut title = sources["titles"][0].clone();
    title["register_id"] = json!(standard);
    title["name"] = json!(name);
    title["collection"] = json!("LegislativeInstrument");
    title["markdown"] = json!(format!("markdown/{standard}/{standard}.md"));
    title["sections_jsonl"] = json!(format!("markdown/{standard}/sections.jsonl"));
    sources["titles"].as_array_mut().unwrap().push(title);
    std::fs::write(&path, sources.to_string()).unwrap();
    let readme = f.root.join("README.md");
    let text = std::fs::read_to_string(&readme)
        .unwrap()
        .replace("2 in-force principal titles", "3 in-force principal titles");
    std::fs::write(&readme, text).unwrap();
    let output = publish(&f);
    assert!(!output.join("markdown").join(standard).exists());
    let shipped = read_json(&output.join("sources.json"));
    let expected = json!({"register_id": standard, "name": name, "reason": "restrictive copyright notice; see REMOVED.md"});
    assert!(
        shipped["excluded_titles"]
            .as_array()
            .unwrap()
            .contains(&expected)
    );
    let removed = std::fs::read_to_string(output.join("REMOVED.md")).unwrap();
    assert!(removed.contains("## 1 accounting standards under restrictive copyright notices"));
    assert!(removed.contains(&format!(
        "[{standard}](https://www.legislation.gov.au/{standard}/latest/text)"
    )));
    // The verifier keys on the standard itself, whatever sources.json says.
    let copy = output.join("markdown").join(standard);
    std::fs::create_dir(&copy).unwrap();
    for entry in std::fs::read_dir(&folder).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
    }
    let (code, out) = verify(&f);
    assert_eq!(
        (
            code,
            status(&out, "no title under a restrictive copyright notice")
        ),
        (1, "FAIL".into())
    );
}

#[test]
fn readme_count_replacement_is_precise_and_linear() {
    let text = "1 Acts and 2 legislative and notifiable instruments. 3 Acts and 4 legislative and notifiable instruments.";
    assert_eq!(
        dist::replace_readme_collection_counts(text, 5, 6),
        "5 Acts and 6 legislative and notifiable instruments. 5 Acts and 6 legislative and notifiable instruments."
    );
    let digits = "9".repeat(200_000);
    let long = format!("{digits} Acts and {digits} legislative and notifiable");
    assert_eq!(
        dist::replace_readme_collection_counts(&long, 5, 6),
        "5 Acts and 6 legislative and notifiable"
    );
}

#[test]
fn links_inside_titles_are_refused() {
    // Before copying: a nested link in the source stops the build.
    let f = fixture();
    let outside = f.root.join("outside.txt");
    std::fs::write(&outside, "must not be copied").unwrap();
    let link = f
        .root
        .join("markdown")
        .join(PUBLIC_ID)
        .join("unexpected-link");
    if !symlink(&outside, &link, false) {
        return;
    }
    refused(
        &f,
        "ValueError: corpus tree contains a symbolic link, junction or reparse point",
    );

    // After publication: the verifier names the title as unsafe.
    let f = fixture();
    let output = publish(&f);
    let rows = output
        .join("markdown")
        .join(PUBLIC_ID)
        .join("sections.jsonl");
    let real = f.root.join("real-rows.jsonl");
    std::fs::rename(&rows, &real).unwrap();
    assert!(symlink(&real, &rows, false));
    let (code, out) = verify(&f);
    assert_eq!(
        (
            code,
            status(&out, "title files stay inside real link-free directories")
        ),
        (1, "FAIL".into())
    );
    assert!(out.contains(PUBLIC_ID));
}

// ------------------------------------------- final-review regressions

/// Access denied as the operating system reports it (EACCES, ERROR_ACCESS_DENIED).
fn io_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 })
}

fn access_denied(path: &str) -> OsError {
    OsError::new(&io_error(), Some(path), tax_radar_au::pyos::Style::Native)
}

fn policy(f: &Fixture) -> String {
    s(&f.build.join("pii_contact_allowlist.json"))
}

#[test]
fn an_error_part_way_through_the_markdown_listing_is_a_verifier_error() {
    let f = fixture();
    let output = publish(&f);
    // dist/markdown holds one title: fail before it, and after it is listed.
    for at in [0, 1] {
        tax_radar_au::pyos::set_listing_hook(move |path, index| {
            (path.ends_with("markdown") && index == at).then(io_error)
        });
        let mut run = tax_radar_au::fadden::Run::default();
        let result = dist_verify::verify_distribution(&s(&output), &policy(&f), &mut run);
        tax_radar_au::pyos::set_listing_hook(|_, _| None);
        let error = result
            .err()
            .unwrap_or_else(|| panic!("index {at} verified"));
        assert_eq!(error.class(), "PermissionError", "{error}");
        assert!(!String::from_utf8_lossy(&run.stdout).contains("all checks passed"));
    }
}

#[test]
fn a_staged_listing_error_stops_publication() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    tax_radar_au::pyos::set_listing_hook(|path, index| {
        (path.contains(".dist.stage-") && path.ends_with("markdown") && index == 1).then(io_error)
    });
    let error = publish_in_process(&f).unwrap_err();
    tax_radar_au::pyos::set_listing_hook(|_, _| None);
    assert!(error.starts_with("PermissionError"), "{error}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn walk_drops_a_directory_whose_listing_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("b.txt"), "b").unwrap();
    tax_radar_au::pyos::set_listing_hook(|path, index| {
        (path.ends_with("sub") && index == 1).then(io_error)
    });
    let steps: Vec<_> = tax_radar_au::pyos::walk(&s(dir.path()), |_| {})
        .collect::<Result<_, _>>()
        .unwrap();
    tax_radar_au::pyos::set_listing_hook(|_, _| None);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].dirs, ["sub"]);
    assert_eq!(steps[0].files, ["a.txt"]);
}

#[test]
fn a_title_listed_twice_is_refused_as_python_copytree_refuses_it() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let path = f.root.join("sources.json");
    let mut sources = read_json(&path);
    let first = sources["titles"][0].clone();
    sources["titles"] = json!([first, first]);
    std::fs::write(&path, sources.to_string()).unwrap();
    let err = refused(&f, "FileExistsError: ");
    let expected = if cfg!(windows) {
        "[WinError 183] Cannot create a file when that file already exists: "
    } else {
        "[Errno 17] File exists: "
    };
    assert!(err.contains(expected), "{err}");
    assert!(err.trim_end().ends_with(&format!("{PUBLIC_ID}'")), "{err}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

/// Whether `path` is a managed stage or backup directory itself, not a path
/// inside one: every link gate probes, but these guards probe only the root.
fn is_managed(path: &str, kind: &str) -> bool {
    Path::new(path)
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with(&format!(".dist.{kind}-")))
}

#[test]
fn a_failed_stage_probe_stops_promotion() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let renames = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = renames.clone();
    dist::set_rename_hook(move |_, _| {
        counter.set(counter.get() + 1);
        None
    });
    let mut failed = false;
    dist::set_probe_hook(move |path| {
        if is_managed(path, "stage") && !failed {
            failed = true;
            return Some(access_denied(path));
        }
        None
    });
    let error = publish_in_process(&f).unwrap_err();
    dist::set_probe_hook(|_| None);
    dist::set_rename_hook(|_, _| None);
    assert!(error.starts_with("PermissionError"), "{error}");
    assert_eq!(renames.get(), 0);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_failed_backup_probe_stops_restoration() {
    let f = fixture();
    with_existing_dist(&f);
    let renames = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = renames.clone();
    dist::set_rename_hook(move |src, _| {
        counter.set(counter.get() + 1);
        (counter.get() == 2).then(|| access_denied(src))
    });
    dist::set_probe_hook(|path| is_managed(path, "backup").then(|| access_denied(path)));
    let error = publish_in_process(&f).unwrap_err();
    dist::set_probe_hook(|_| None);
    dist::set_rename_hook(|_, _| None);
    assert!(error.starts_with("PermissionError"), "{error}");
    assert!(error.contains(".dist.backup-"), "{error}");
    // Not restored: the old tree stays in its backup, and dist is absent.
    assert_eq!(renames.get(), 2);
    assert!(!f.dist().exists());
    let left = managed_paths(&f);
    assert_eq!(left.len(), 1);
    assert!(left[0].starts_with(".dist.backup-"));
    let kept = f.root.join(&left[0]).join("last-publishable-build.txt");
    assert_eq!(std::fs::read_to_string(kept).unwrap(), "keep me");
}

#[test]
fn a_parent_resolving_elsewhere_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dist");
    let candidate = dir.path().join(format!(".dist.stage-{}", "0".repeat(32)));
    let elsewhere = s(&dir.path().join("elsewhere"));
    let mut moved = false;
    // The first lookup sees the parent moved; the second sees the original.
    dist::set_realpath_hook(move |_| {
        if moved {
            return None;
        }
        moved = true;
        Some(elsewhere.clone())
    });
    let error = dist::validated_managed_sibling(&s(&target), &s(&candidate), "stage").unwrap_err();
    dist::set_realpath_hook(|_| None);
    assert_eq!(
        (error.class(), error.to_string().as_str()),
        (
            "ValueError",
            "managed distribution path resolves outside its parent"
        )
    );
}

/// A change to the public title's source directory.
type Change = fn(&Path);

/// Each copied-file gate, reached by changing the source after the source
/// gate passed.
#[test]
fn the_copy_step_rechecks_what_the_source_gate_saw() {
    let cases: [(&str, Change); 4] = [
        (
            "RuntimeError: listed title C2004A00001 has no sections.jsonl",
            |t| {
                std::fs::remove_file(t.join("sections.jsonl")).unwrap();
            },
        ),
        (
            "RuntimeError: listed title C2004A00001 has no markdown directory",
            |t| {
                std::fs::remove_dir_all(t).unwrap();
            },
        ),
        (
            "RuntimeError: copied title contains private-person registration details: C2004A00001",
            |t| {
                let md = t.join(format!("{PUBLIC_ID}.md"));
                let text = std::fs::read_to_string(&md).unwrap();
                std::fs::write(&md, format!("{text}{ALL_CAPS_ROW}\n")).unwrap();
            },
        ),
        (
            "RuntimeError: copied title contains an unapproved contact identifier: C2004A00001:email:",
            |t| {
                let md = t.join(format!("{PUBLIC_ID}.md"));
                let text = std::fs::read_to_string(&md).unwrap();
                std::fs::write(&md, format!("{text}Write to fresh-contact@example.test.\n"))
                    .unwrap();
            },
        ),
    ];
    for (needle, change) in cases {
        let f = fixture();
        let marker = with_existing_dist(&f);
        let directory = f.root.join("markdown").join(PUBLIC_ID);
        dist::set_preflight_hook(move || change(&directory));
        let error = publish_in_process(&f).unwrap_err();
        dist::set_preflight_hook(|| {});
        assert!(error.starts_with(needle), "{error:?} lacks {needle:?}");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
        assert!(managed_paths(&f).is_empty());
    }
}

#[test]
fn stale_sources_that_do_not_order_raise_the_pair_python_meets_first() {
    let f = fixture();
    with_existing_dist(&f);
    let rate = |rid: Value| {
        json!({"register_id": rid, "act": PUBLIC_NAME, "collection": "Act", "section": "1",
               "heading": "Public rate", "topic": "income tax rates", "kind": "rate", "amounts": ["10%"],
               "content": "The public rate is 10%.", "version_is_current": false})
    };
    let rows = [rate(json!(42)), rate(json!(true)), rate(json!(PUBLIC_ID))];
    let records: Vec<Json> = rows.iter().map(pyjson_value).collect();
    let error = dist::render_rates_markdown(&records).unwrap_err();
    let message = "'<' not supported between instances of 'str' and 'bool'";
    assert_eq!(
        (error.class(), error.to_string().as_str()),
        ("TypeError", message)
    );
    write(
        &f.root.join("rates").join("rates.jsonl"),
        &rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    );
    refused(&f, &format!("TypeError: {message}"));
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn deeply_nested_rows_are_a_recursion_error_not_a_malformed_row() {
    let depth = tax_radar_au::pyjson::MAX_DEPTH + 1;
    let deep = format!("{}{}\n", "[".repeat(depth), "]".repeat(depth));
    for rates in [true, false] {
        let f = fixture();
        let output = publish(&f);
        let path = if rates {
            output.join("rates").join("rates.jsonl")
        } else {
            output
                .join("markdown")
                .join(PUBLIC_ID)
                .join("sections.jsonl")
        };
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{deep}")).unwrap();
        let (code, out, err) = stage(&f, "dist_verify");
        assert_eq!(code, 2, "rates {rates}: {out}");
        assert!(out.contains("VERIFIER ERROR"));
        assert!(
            err.contains(
                "RecursionError: maximum recursion depth exceeded while decoding a JSON array"
            ),
            "{err}"
        );
    }
}

#[cfg(unix)]
fn run_with_timeout(f: &Fixture, name: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg(name)
        .env("FADDEN_MODULE_DIR", &f.build)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(60) {
            child.kill().unwrap();
            panic!("{name} blocked");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[cfg(unix)]
#[test]
fn a_named_pipe_licence_notice_is_refused_before_it_is_opened() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let notice = f.root.join("LICENCE-NOTICE.md");
    std::fs::remove_file(&notice).unwrap();
    let pipe = std::ffi::CString::new(s(&notice)).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
    let (code, _, err) = run_with_timeout(&f, "dist");
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("shutil.SpecialFileError: `")
            && err.contains("LICENCE-NOTICE.md` is a named pipe"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[cfg(unix)]
#[test]
fn copied_files_keep_their_times_and_mode() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let source = f
        .root
        .join("markdown")
        .join(PUBLIC_ID)
        .join(format!("{PUBLIC_ID}.md"));
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let output = publish(&f);
    let copied = output
        .join("markdown")
        .join(PUBLIC_ID)
        .join(format!("{PUBLIC_ID}.md"));
    let copied = std::fs::metadata(copied).unwrap();
    assert_eq!(copied.permissions().mode() & 0o7777, 0o640);
    assert_eq!(copied.modified().unwrap(), old);
}

#[cfg(unix)]
#[test]
fn a_name_that_is_not_unicode_stops_the_verifier() {
    use std::os::unix::ffi::OsStrExt;
    let odd = std::ffi::OsStr::from_bytes(b"notes-\xff");
    for in_markdown in [true, false] {
        let f = fixture();
        let output = publish(&f);
        if in_markdown {
            std::fs::write(output.join("markdown").join(odd), "x").unwrap();
        } else {
            // Python passed this one: only the size walk sees it.
            std::fs::create_dir(output.join(odd)).unwrap();
            std::fs::write(output.join(odd).join("notes.txt"), "x").unwrap();
        }
        let (code, out, err) = stage(&f, "dist_verify");
        assert_eq!(code, 2, "in markdown {in_markdown}: {out}");
        assert!(out.contains("VERIFIER ERROR"));
        assert!(
            err.contains("ValueError: file name is not valid Unicode: "),
            "{err}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_permission_error_classifies_an_entry_as_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("markdown");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(parent.join("F2026N00004")).unwrap();
    // Listable but not searchable: the entry's lstat is refused.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o600)).unwrap();
    let kind = dist_verify::entry_kind(&s(&parent.join("F2026N00004")));
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(kind, "unreadable");
    }
}

/// A junction back to its own title is refused before the walk enters it, as
/// Python's generator walk was.
#[cfg(windows)]
#[test]
fn a_junction_cycle_is_refused_without_being_entered() {
    let dir = tempfile::tempdir().unwrap();
    let title = dir.path().join(PUBLIC_ID);
    std::fs::create_dir(&title).unwrap();
    std::fs::write(title.join("sections.jsonl"), "{}\n").unwrap();
    let made = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(title.join("loop"))
        .arg(&title)
        .output()
        .unwrap();
    assert!(made.status.success());
    let listed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = listed.clone();
    tax_radar_au::pyos::set_listing_hook(move |path, index| {
        if index == 0 {
            seen.borrow_mut().push(path.to_string());
        }
        None
    });
    let error = tax_radar_au::fadden::corpus_paths::reject_symlinks(&s(&title)).unwrap_err();
    tax_radar_au::pyos::set_listing_hook(|_, _| None);
    assert_eq!(
        error.to_string(),
        "corpus tree contains a symbolic link, junction or reparse point"
    );
    assert_eq!(listed.borrow().as_slice(), [s(&title)]);
}

/// A directory carrying a third-party reparse tag: neither a symbolic link
/// nor a junction.
#[cfg(windows)]
fn make_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    std::fs::create_dir(path).unwrap();
    let handle = std::fs::OpenOptions::new()
        .access_mode(0x4000_0000) // GENERIC_WRITE
        .custom_flags(0x0020_0000 | 0x0200_0000) // OPEN_REPARSE_POINT | BACKUP_SEMANTICS
        .open(path)
        .unwrap();
    // REPARSE_GUID_DATA_BUFFER: tag, data length, reserved, GUID, then data.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0x0000_1234u32.to_le_bytes());
    buffer.extend_from_slice(&4u16.to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&[0x5a; 16]);
    buffer.extend_from_slice(b"test");
    let mut returned = 0u32;
    // SAFETY: the handle is open, the buffer outlives the call and its length is exact.
    let ok = unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            handle.as_raw_handle(),
            windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        // ERROR_PRIVILEGE_NOT_HELD: any other failure is a broken test.
        assert_eq!(error.raw_os_error(), Some(1314), "{error}");
        eprintln!("skipped: cannot set a reparse point ({error})");
    }
    ok != 0
}

#[cfg(windows)]
#[test]
fn a_top_level_reparse_point_is_a_named_failure() {
    let f = fixture();
    let output = publish(&f);
    let point = output.join("markdown").join("F2026N00003");
    if !make_reparse_point(&point) {
        return;
    }
    assert_eq!(dist_verify::entry_kind(&s(&point)), "reparse point");
    let (code, out) = verify(&f);
    let _ = std::fs::remove_dir(&point);
    assert_eq!(
        (code, status(&out, "no directory beyond what is listed")),
        (1, "FAIL".into())
    );
    assert!(out.contains("F2026N00003 [reparse point]"), "{out}");
}

// --------------------------------------------- repair-review regressions

/// The managed stage beside `dist`, which exists by the time the source gate runs.
fn stage_dir(root: &Path) -> PathBuf {
    std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".dist.stage-")
        })
        .unwrap()
}

#[test]
fn a_staged_licence_linked_to_its_source_is_refused_before_truncation() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let source = f.root.join("LICENCE-NOTICE.md");
    let (root, notice) = (f.root.clone(), source.clone());
    dist::set_preflight_hook(move || {
        std::fs::hard_link(&notice, stage_dir(&root).join("LICENCE-NOTICE.md")).unwrap();
    });
    let error = publish_in_process(&f).unwrap_err();
    dist::set_preflight_hook(|| {});
    assert!(
        error.starts_with("shutil.SameFileError: '") && error.ends_with("' are the same file"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "licence\n");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_directory_licence_notice_is_refused_naming_it() {
    let f = fixture();
    let marker = with_existing_dist(&f);
    let notice = f.root.join("LICENCE-NOTICE.md");
    std::fs::remove_file(&notice).unwrap();
    std::fs::create_dir(&notice).unwrap();
    let expected = if cfg!(windows) {
        "PermissionError: [Errno 13] Permission denied: "
    } else {
        "IsADirectoryError: [Errno 21] Is a directory: "
    };
    let err = refused(&f, expected);
    assert!(err.trim_end().ends_with("LICENCE-NOTICE.md'"), "{err}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

/// `sources.json` with an unused `padding` member nested `depth` arrays deep.
fn pad_sources(f: &Fixture, depth: usize) {
    let path = f.root.join("sources.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let body = text.trim_end().strip_suffix('}').unwrap();
    let padding = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    std::fs::write(&path, format!("{body}, \"padding\": {padding}}}")).unwrap();
}

#[test]
fn metadata_nested_past_the_encoder_depth_is_refused_as_python_refuses_it() {
    // Under `python -m fadden dist`, padding nested 990 deep is written and
    // 991 is refused, with a note naming the member.
    let f = fixture();
    pad_sources(&f, 990);
    publish(&f);
    let f = fixture();
    let marker = with_existing_dist(&f);
    pad_sources(&f, 991);
    let err = refused(&f, "RecursionError: maximum recursion depth exceeded");
    assert!(
        err.trim_end()
            .ends_with("when serializing dict item 'padding'"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me");
    assert!(managed_paths(&f).is_empty());
}

#[test]
fn a_row_file_that_changes_after_its_privacy_scan_is_unreadable() {
    let f = fixture();
    let output = publish(&f);
    std::fs::write(
        output
            .join("markdown")
            .join(PUBLIC_ID)
            .join("sections.jsonl"),
        b"\xff\n",
    )
    .unwrap();
    // The scan sees the file as it was; the row read then meets invalid UTF-8.
    dist_verify::set_privacy_hook(|_| Some((false, Vec::new())));
    let mut run = tax_radar_au::fadden::Run::default();
    let result = dist_verify::verify_distribution(&s(&output), &policy(&f), &mut run);
    dist_verify::set_privacy_hook(|_| None);
    let fails = result.unwrap();
    let label = "all distributed title files are validated UTF-8 text";
    assert!(fails.iter().any(|l| l == label), "{fails:?}");
    assert_eq!(status(&String::from_utf8_lossy(&run.stdout), label), "FAIL");
}

#[cfg(target_os = "linux")]
#[test]
fn copied_files_keep_their_extended_attributes() {
    let f = fixture();
    let source = f
        .root
        .join("markdown")
        .join(PUBLIC_ID)
        .join(format!("{PUBLIC_ID}.md"));
    let path = std::ffi::CString::new(s(&source)).unwrap();
    let name = std::ffi::CString::new("user.review").unwrap();
    // SAFETY: NUL-terminated path and name, and a value of the length passed.
    let set =
        unsafe { libc::setxattr(path.as_ptr(), name.as_ptr(), b"kept".as_ptr().cast(), 4, 0) };
    if set != 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSUP), "{error}");
        eprintln!("skipped: this filesystem has no user attributes");
        return;
    }
    let output = publish(&f);
    let copied = std::ffi::CString::new(s(&output
        .join("markdown")
        .join(PUBLIC_ID)
        .join(format!("{PUBLIC_ID}.md"))))
    .unwrap();
    let mut value = [0u8; 16];
    // SAFETY: NUL-terminated path and name, and a buffer of the length passed.
    let got = unsafe {
        libc::getxattr(
            copied.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    assert_eq!(&value[..usize::try_from(got).unwrap()], b"kept");
}
