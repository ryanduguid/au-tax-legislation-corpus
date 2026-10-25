//! Python parity, frozen. `tests/golden/<stage>.cases.json` holds cases from the
//! port's differential harness, each run through the Python stages this binary
//! replaced (the base revision, 43b5e0c) on Windows and on Linux. Every input
//! is fabricated. For each case the binary must exit as Python did, print what
//! Python printed (the last line of a traceback where Python printed one), and
//! leave every file with the recorded SHA-256 of its normalised text.
//!
//! Outputs are normalised as the harness normalised them: the case directory
//! reads `ROOT`, the module directory `MODULE`, generated stage, backup and
//! temporary names `HEX`, whole-second sleeps in the discover and versions
//! fake-http logs `sleep N`, finalize's date `TODAY` (today's, taken from the
//! operating system before and after the cases, so a run across midnight
//! accepts either day), and bytes that are not UTF-8 `\xNN`. finalize's
//! `build/build-record.json` holds a timestamp and the binary's digest, so it
//! is checked whole by `check_build_record` rather than by a recorded hash.
//! `rulings`, `probe13`, `discover` and `versions` fetch over HTTP, so their
//! cases need the scripted responses of the `fake-http` feature:
//! `cargo test --features fake-http --test fadden_golden`.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tax_radar_au::pyjson;

const PLATFORM: &str = if cfg!(windows) { "windows" } else { "linux" };

static MANAGED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(stage|backup|publication-bundles|monitor-contract|live-evidence|register-capture)-[0-9a-f]{32}").unwrap()
});
static TEMP_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[0-9a-f]{32}\.tmp").unwrap());
// The fake-http log: Python logs time.sleep(3) as 3, the binary's f64 as 3.0.
static WHOLE_SLEEP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^sleep (\d+)\.0$").unwrap());
static CONTACT_DETAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(no unapproved contact identifiers +FAIL  ).*").unwrap());
// dist_verify tallies file extensions in directory-listing order, which the
// filesystem decides; Python's order differs between NTFS, ext4, btrfs and tmpfs.
static EXTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(no image files +(?:PASS|FAIL)  )\{([^\n]*)\}").unwrap());
static ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"('(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"): \d+"#).unwrap());

fn sorted_exts(text: &str) -> String {
    EXTS.replace_all(text, |c: &regex::Captures| {
        let mut items: Vec<&str> = ITEM.find_iter(&c[2]).map(|m| m.as_str()).collect();
        items.sort_unstable();
        format!("{}{{{}}}", &c[1], items.join(", "))
    })
    .into_owned()
}

static UNORDERABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"TypeError: '<' not supported between instances of '(\w+)' and '(\w+)'").unwrap()
});

/// The repository's module data, which matches the base package's.
fn module_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fadden")
}

/// The module's JSON data and the tools finalize ships at the corpus root.
fn module_data() -> Vec<(String, Vec<u8>)> {
    const SHIPPED: [&str; 3] = ["check_current.py", "corpus_paths.py", "http_fetch.py"];
    std::fs::read_dir(module_source())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                || p.file_name()
                    .is_some_and(|n| SHIPPED.iter().any(|s| n == *s))
        })
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&p).unwrap())
        })
        .collect()
}

fn copy_module(to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for (name, bytes) in module_data() {
        std::fs::write(to.join(name), bytes).unwrap();
    }
}

/// `str(path)` as stage output may spell it: native, with forward slashes,
/// and with escaped backslashes, longest first.
fn forms(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy().into_owned();
    let mut forms = vec![text.replace('\\', "/"), text.replace('\\', "\\\\"), text];
    forms.sort();
    forms.dedup();
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms
}

/// `bytes.decode("utf-8", "backslashreplace")`.
fn decode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    for chunk in data.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn normalise(data: &[u8], work: &Path, module: &Path, dates: &[String]) -> String {
    let mut data = data.to_vec();
    if data.starts_with(b"SQLite format 3\x00") && data.len() >= 100 {
        // The bundled SQLite records its own version at offset 96.
        data[96..100].copy_from_slice(b"VVVV");
    }
    let mut out = decode(&data);
    for form in forms(work) {
        out = out.replace(&form, "ROOT");
    }
    out = MANAGED_NAME.replace_all(&out, ".$1-HEX").into_owned();
    out = TEMP_NAME.replace_all(&out, ".HEX.tmp").into_owned();
    for form in forms(module) {
        out = out.replace(&form, "MODULE");
    }
    for date in dates {
        out = out.replace(date.as_str(), "TODAY");
    }
    out
}

fn files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        found.push(path.clone());
        if path.is_dir() && !path.is_symlink() {
            files_under(&path, found);
        }
    }
}

/// Every file and directory the case left, the copied module's unchanged data
/// left out, as the SHA-256 of its normalised text (None for a directory).
fn snapshot(
    family: &str,
    work: &Path,
    module: &Path,
    dates: &[String],
) -> BTreeMap<String, Option<String>> {
    let base: BTreeMap<String, Vec<u8>> = module_data().into_iter().collect();
    let mut paths = Vec::new();
    files_under(work, &mut paths);
    let mut found = BTreeMap::new();
    for path in paths {
        let rel = path
            .strip_prefix(work)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "proj/fadden" || rel.starts_with("proj/fadden/") {
            let name = rel
                .trim_start_matches("proj/fadden")
                .trim_start_matches('/');
            if path.is_dir()
                || rel.contains("__pycache__")
                || base
                    .get(name)
                    .is_some_and(|b| std::fs::read(&path).ok().as_ref() == Some(b))
            {
                continue;
            }
        }
        let value = (!path.is_dir()).then(|| {
            let text = normalise(&std::fs::read(&path).unwrap(), work, module, dates);
            // Stage 5 fixtures were recorded after the harness began this rewrite.
            if rel == "http.log" && matches!(family, "discover" | "versions") {
                sha256(&WHOLE_SLEEP.replace_all(&text, "sleep $1"))
            } else {
                sha256(&text)
            }
        });
        found.insert(
            MANAGED_NAME.replace_all(&rel, ".$1-HEX").into_owned(),
            value,
        );
    }
    found
}

fn content(value: &Value) -> Option<Vec<u8>> {
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.get("t").and_then(Value::as_str) {
        return Some(text.as_bytes().to_vec());
    }
    let hex = value["x"].as_str().unwrap();
    Some(
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect(),
    )
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Output against Python's: the text itself, or for long output its SHA-256.
fn same_text(actual: &str, expected: &Value) -> bool {
    match expected {
        Value::String(text) => actual == text,
        other => other["sha256"].as_str() == Some(&sha256(actual)),
    }
}

fn unordered(family: &str, line: &str) -> String {
    if family != "dist_verify" {
        return line.to_string();
    }
    // CPython sorted a set there, so the two types can come in either order.
    UNORDERABLE
        .replace_all(line, |c: &regex::Captures| {
            let mut pair = [c[1].to_string(), c[2].to_string()];
            pair.sort();
            format!(
                "TypeError: '<' not supported between instances of '{}' and '{}'",
                pair[0], pair[1]
            )
        })
        .into_owned()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `item`'s `bytes` and `sha256` against the file at `file`.
fn digest_matches(item: &Value, file: &Path) -> Result<(), String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if item["bytes"] != bytes.len() || item["sha256"] != sha256_hex(&bytes).as_str() {
        return Err(format!("{}: size or digest", file.display()));
    }
    Ok(())
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    keys
}

fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}

/// Checks finalize's build record whole, failing closed: every field, the
/// binary's name, version and digest, each input's and shipped tool's size and
/// digest against the bytes on disk, and the inputs being the 3 module files
/// and each title sources.json publishes (the corpus root is the record's
/// grandparent).
fn check_build_record(path: &Path, module: &Path, binary: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("unreadable: {e}"))?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|e| format!("not JSON: {e}"))?;
    let expect = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
    expect(
        keys(&record)
            == [
                "environment",
                "finalized_at",
                "implementation",
                "inputs",
                "lineage",
                "schema",
                "scope",
                "shipped_tools",
                "source",
            ],
        "top-level fields",
    )?;
    expect(
        record["schema"] == "au-tax-corpus-build-record.v1" && record["scope"] == "finalize",
        "schema or scope",
    )?;
    let at = record["finalized_at"].as_str().unwrap_or("");
    expect(
        at.len() == 20
            && at.char_indices().all(|(i, c)| match i {
                4 | 7 => c == '-',
                10 => c == 'T',
                13 | 16 => c == ':',
                19 => c == 'Z',
                _ => c.is_ascii_digit(),
            }),
        "finalized_at",
    )?;
    let source = &record["source"];
    expect(
        keys(source) == ["dirty", "reason", "repository", "revision"]
            && source["repository"] == env!("CARGO_PKG_REPOSITORY")
            && source["revision"].is_null()
            && source["dirty"].is_null()
            && text(&source["reason"]),
        "source",
    )?;
    let program = std::fs::read(binary).map_err(|e| format!("binary: {e}"))?;
    let implementation = &record["implementation"];
    expect(
        keys(implementation)
            == [
                "cargo_lock_reason",
                "cargo_lock_sha256",
                "language",
                "program",
                "sha256",
                "version",
            ]
            && implementation["language"] == "rust"
            && implementation["program"] == binary.file_name().unwrap().to_string_lossy().as_ref()
            && implementation["version"] == env!("CARGO_PKG_VERSION")
            && implementation["sha256"] == sha256_hex(&program).as_str()
            && implementation["cargo_lock_sha256"].is_null()
            && text(&implementation["cargo_lock_reason"]),
        "implementation",
    )?;
    let corpus = path
        .parent()
        .and_then(Path::parent)
        .ok_or("no corpus root")?;
    let inputs = record["inputs"].as_array().ok_or("inputs")?;
    let mut places = Vec::new();
    for input in inputs {
        let relative = input["path"].as_str().filter(|p| {
            !p.is_empty()
                && !p.starts_with('/')
                && !p.contains(['\\', ':'])
                && !p.split('/').any(|part| part == "..")
        });
        let (Some(root), Some(relative)) = (input["root"].as_str(), relative) else {
            return Err(format!("input fields {input}"));
        };
        expect(
            keys(input) == ["bytes", "path", "root", "sha256"],
            "input fields",
        )?;
        let base = match root {
            "corpus" => corpus,
            "module" => module,
            other => return Err(format!("input root {other}")),
        };
        digest_matches(input, &base.join(relative))?;
        places.push((root, relative));
    }
    let mut sorted = places.clone();
    sorted.sort_unstable();
    sorted.dedup();
    expect(places == sorted, "inputs not sorted and unique")?;
    let module_inputs: Vec<&str> = places
        .iter()
        .filter(|(r, _)| *r == "module")
        .map(|(_, p)| *p)
        .collect();
    expect(
        module_inputs == ["manifest_md.json", "manifest_raw.json", "pii_flagged.json"],
        "module inputs",
    )?;
    // Python's json.loads, which reads the NaN and deep nesting json.dump wrote.
    let sources = std::fs::read(corpus.join("sources.json"))
        .ok()
        .and_then(|b| pyjson::loads_standard(&String::from_utf8_lossy(&b)).ok())
        .ok_or("no sources.json beside the record")?;
    let Some(pyjson::Json::Array(published)) = sources.get("titles") else {
        return Err("sources.json titles".into());
    };
    let mut titles: Vec<String> = published
        .iter()
        .map(|t| {
            let id = t.get("register_id").and_then(|r| r.as_str()).unwrap_or("");
            format!("markdown/{id}/sections.jsonl")
        })
        .collect();
    titles.sort_unstable();
    let corpus_inputs: Vec<&str> = places
        .iter()
        .filter(|(r, _)| *r == "corpus")
        .map(|(_, p)| *p)
        .collect();
    expect(
        corpus_inputs == titles,
        "corpus inputs are not the published titles",
    )?;
    let lineage = &record["lineage"];
    expect(
        keys(lineage) == ["known", "unknown"]
            && lineage["known"] == serde_json::json!([])
            && text(&lineage["unknown"]),
        "lineage",
    )?;
    let shipped = &record["shipped_tools"];
    let files = shipped["files"].as_array().ok_or("shipped tools")?;
    expect(
        keys(shipped) == ["files", "requires"] && text(&shipped["requires"]) && files.len() == 3,
        "shipped tools",
    )?;
    for (tool, name) in files
        .iter()
        .zip(["check_current.py", "corpus_paths.py", "http_fetch.py"])
    {
        let nested = format!("{name}/{name}");
        let at = tool["name"]
            .as_str()
            .filter(|n| *n == name || *n == nested)
            .ok_or_else(|| format!("shipped tool {tool}"))?;
        expect(
            keys(tool) == ["bytes", "name", "sha256"],
            "shipped tool fields",
        )?;
        digest_matches(tool, &corpus.join(at))?;
    }
    expect(
        record["environment"]
            == serde_json::json!({
                "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
            }),
        "environment",
    )
}

/// Today's date as the operating system gives it, independently of the
/// binary's own `local_today`, so that a wrong date in its output still fails.
fn os_today() -> String {
    let out = if cfg!(windows) {
        Command::new("powershell")
            // PowerShell 7's module path (inherited from a pwsh parent, as on CI)
            // stops Windows PowerShell loading its own modules.
            .env_remove("PSModulePath")
            .args(["-NoProfile", "-Command", "Get-Date -Format yyyy-MM-dd"])
            .output()
    } else {
        Command::new("date").arg("+%F").output()
    };
    let today = String::from_utf8(out.unwrap().stdout).unwrap();
    let today = today.trim().to_string();
    assert!(
        today.len() == 10 && today.as_bytes()[4] == b'-',
        "date: {today}"
    );
    today
}

/// Run one case and return what differs from Python, if anything.
fn run_case(family: &str, case: &Value, shared_module: &Path, dates: &[String]) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let input = &case["input"];
    let mut module = shared_module.to_path_buf();
    if let Some(project) = input["project"].as_array() {
        module = work.join("proj").join("fadden");
        copy_module(&module);
        // The harness wrote these with Python's text mode.
        let marker = if cfg!(windows) { "x\r\n" } else { "x\n" };
        for name in project {
            std::fs::write(work.join("proj").join(name.as_str().unwrap()), marker).unwrap();
        }
    }
    // In the harness's order: a directory listing on some filesystems shows it.
    let files = input["files"].as_array().unwrap();
    let scripted = files.iter().any(|f| f[0] == "script.json");
    for file in files {
        let (name, value) = (file[0].as_str().unwrap(), &file[1]);
        let target = work.join(name);
        match content(value) {
            None => std::fs::create_dir_all(&target).unwrap(),
            Some(bytes) => {
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(&target, bytes).unwrap();
            }
        }
    }
    let steps: Vec<Vec<String>> = match input["steps"].as_array() {
        Some(steps) => steps
            .iter()
            .map(|s| serde_json::from_value(s.clone()).unwrap())
            .collect(),
        None => vec![serde_json::from_value(input["argv"].clone()).unwrap()],
    };
    let expected = &case["expected"][PLATFORM];
    let mut problems = Vec::new();
    for (n, argv) in steps.iter().enumerate() {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fadden"));
        command
            .arg(family)
            .args(argv)
            .current_dir(&work)
            .env("FADDEN_MODULE_DIR", &module);
        for name in ["COLUMNS", "LINES", "FORCE_COLOR", "NO_COLOR"] {
            command.env_remove(name);
        }
        if scripted {
            command
                .env("FADDEN_FAKE_HTTP", work.join("script.json"))
                .env("FADDEN_FAKE_HTTP_LOG", work.join("http.log"));
        }
        let out = command.output().unwrap();
        let stdout = normalise(&out.stdout, &work, &module, dates);
        let stdout = sorted_exts(&CONTACT_DETAIL.replace_all(&stdout, "$1<findings>"));
        let stderr = normalise(&out.stderr, &work, &module, dates).replace("\r\n", "\n");
        let want = &expected["steps"][n];
        let code = out.status.code().unwrap_or(-1);
        if i64::from(code) != want[0].as_i64().unwrap() {
            problems.push(format!("step {n}: exit {code}, Python {}", want[0]));
        }
        if !same_text(&stdout, &want[1]) {
            problems.push(format!("step {n}: stdout\n{stdout}\nPython\n{}", want[1]));
        }
        let stderr = unordered(family, &stderr);
        let want_err = match want[2].as_str() {
            Some(text) => Value::String(unordered(family, text)),
            None => want[2].clone(),
        };
        if !same_text(&stderr, &want_err) {
            problems.push(format!("step {n}: stderr\n{stderr}\nPython\n{want_err}"));
        }
    }
    let mut left = snapshot(family, &work, &module, dates);
    let want: BTreeMap<String, Option<String>> =
        serde_json::from_value(expected["files"].clone()).unwrap();
    for (key, value) in &want {
        if value.as_deref() == Some("build-record") {
            let binary = Path::new(env!("CARGO_BIN_EXE_fadden"));
            match check_build_record(&work.join(key), &module, binary) {
                Ok(()) => {
                    left.insert(key.clone(), value.clone());
                }
                Err(problem) => problems.push(format!("{key}: {problem}")),
            }
        }
    }
    if left != want {
        let differ: Vec<&String> = left
            .keys()
            .chain(want.keys())
            .filter(|k| left.get(*k) != want.get(*k))
            .collect();
        problems.push(format!("files differ: {differ:?}"));
    }
    (!problems.is_empty())
        .then(|| format!("{family} case {}:\n{}", case["index"], problems.join("\n")))
}

fn family(name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(format!("{name}.cases.json"));
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(!cases.is_empty(), "{name} has no cases");
    let shared = tempfile::tempdir().unwrap();
    let module = shared.path().join("fadden");
    copy_module(&module);
    let run = |dates: &[String]| -> Vec<String> {
        cases
            .iter()
            .filter_map(|case| run_case(name, case, &module, dates))
            .collect()
    };
    // finalize is given today's date; a run across midnight is run again
    // accepting either day.
    let mut dates = Vec::new();
    if name == "finalize" {
        dates.push(os_today());
    }
    let mut failures = run(&dates);
    if name == "finalize" {
        let after = os_today();
        if !dates.contains(&after) {
            dates.push(after);
            failures = run(&dates);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// `check_build_record` accepts a whole, true record and refuses each
/// mutation of it, including fields it could otherwise skip.
#[test]
fn the_build_record_check_fails_closed() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let corpus = dir.path().join("corpus");
    let module = dir.path().join("fadden");
    let binary = Path::new(env!("CARGO_BIN_EXE_fadden"));
    for (base, name, body) in [
        (
            &corpus,
            "sources.json",
            r#"{"titles": [{"register_id": "C1"}]}"#,
        ),
        (&corpus, "markdown/C1/sections.jsonl", "{}\n"),
        (&corpus, "check_current.py", "a\n"),
        (&corpus, "corpus_paths.py", "b\n"),
        (&corpus, "http_fetch.py", "c\n"),
        (&module, "manifest_md.json", "[]"),
        (&module, "manifest_raw.json", "[1]"),
        (&module, "pii_flagged.json", "[2]"),
    ] {
        let path = base.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let digest = |path: PathBuf| {
        let bytes = std::fs::read(path).unwrap();
        (bytes.len(), sha256_hex(&bytes))
    };
    let input = |root: &str, path: &str| {
        let base = if root == "corpus" { &corpus } else { &module };
        let (bytes, sha256) = digest(base.join(path));
        json!({"root": root, "path": path, "bytes": bytes, "sha256": sha256})
    };
    let tool = |name: &str| {
        let (bytes, sha256) = digest(corpus.join(name));
        json!({"name": name, "bytes": bytes, "sha256": sha256})
    };
    let good = json!({
        "schema": "au-tax-corpus-build-record.v1",
        "scope": "finalize",
        "finalized_at": "2026-10-25T01:02:03Z",
        "source": {"repository": env!("CARGO_PKG_REPOSITORY"), "revision": null,
                   "dirty": null, "reason": "r"},
        "implementation": {"language": "rust",
                           "program": binary.file_name().unwrap().to_string_lossy(),
                           "version": env!("CARGO_PKG_VERSION"),
                           "sha256": digest(binary.to_path_buf()).1,
                           "cargo_lock_sha256": null, "cargo_lock_reason": "r"},
        "inputs": [input("corpus", "markdown/C1/sections.jsonl"),
                   input("module", "manifest_md.json"), input("module", "manifest_raw.json"),
                   input("module", "pii_flagged.json")],
        "lineage": {"known": [], "unknown": "u"},
        "shipped_tools": {"requires": "q", "files": [tool("check_current.py"),
                          tool("corpus_paths.py"), tool("http_fetch.py")]},
        "environment": {"platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)},
    });
    let path = corpus.join("build").join("build-record.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let check = |record: &Value| {
        std::fs::write(&path, record.to_string()).unwrap();
        check_build_record(&path, &module, binary)
    };
    assert_eq!(check(&good), Ok(()));
    type Mutation = (&'static str, fn(&mut Value));
    let mutations: [Mutation; 14] = [
        ("inputs null", |r| r["inputs"] = Value::Null),
        ("inputs empty", |r| r["inputs"] = json!([])),
        ("input dropped", |r| {
            r["inputs"].as_array_mut().unwrap().remove(0);
        }),
        ("path missing", |r| {
            r["inputs"][0].as_object_mut().unwrap().remove("path");
        }),
        ("traversal", |r| {
            r["inputs"][1]["path"] = json!("../fadden/manifest_md.json")
        }),
        ("input bytes", |r| r["inputs"][0]["bytes"] = json!(0)),
        ("root", |r| r["inputs"][1]["root"] = json!("elsewhere")),
        ("binary digest", |r| {
            r["implementation"]["sha256"] = json!("0".repeat(64))
        }),
        ("lockfile", |r| {
            r["implementation"]["cargo_lock_sha256"] = json!("0".repeat(64))
        }),
        ("dirty", |r| r["source"]["dirty"] = json!(false)),
        ("tool malformed", |r| {
            r["shipped_tools"]["files"][0]
                .as_object_mut()
                .unwrap()
                .remove("sha256");
        }),
        ("tool bytes", |r| {
            r["shipped_tools"]["files"][2]["bytes"] = json!(0)
        }),
        ("timestamp", |r| r["finalized_at"] = json!("yesterday")),
        ("extra field", |r| r["extra"] = json!(1)),
    ];
    for (what, mutate) in mutations {
        let mut record = good.clone();
        mutate(&mut record);
        assert!(check(&record).is_err(), "{what} was accepted");
    }
}

#[test]
fn as_at_matches_python() {
    family("as_at");
}

#[test]
fn compare_builds_matches_python() {
    family("compare_builds");
}

#[test]
fn search_matches_python() {
    family("search");
}

#[test]
fn rates_matches_python() {
    family("rates");
}

#[test]
fn dist_verify_matches_python() {
    family("dist_verify");
}

#[test]
fn dist_matches_python() {
    family("dist");
}

#[test]
fn extract_matches_python() {
    family("extract");
}

#[test]
fn finalize_matches_python() {
    family("finalize");
}

#[cfg(feature = "fake-http")]
#[test]
fn capture_register_matches_python() {
    family("capture_register");
}

#[test]
fn export_live_evidence_bundles_matches_python() {
    family("export_live_evidence_bundles");
}

#[test]
fn export_monitor_contract_matches_python() {
    family("export_monitor_contract");
}

#[test]
fn export_publication_bundles_matches_python() {
    family("export_publication_bundles");
}

#[test]
fn pii_scan_matches_python() {
    family("pii_scan");
}

#[test]
fn pii_scan2_matches_python() {
    family("pii_scan2");
}

#[cfg(feature = "fake-http")]
#[test]
fn rulings_matches_python() {
    family("rulings");
}

#[cfg(feature = "fake-http")]
#[test]
fn probe13_matches_python() {
    family("probe13");
}

#[cfg(feature = "fake-http")]
#[test]
fn discover_matches_python() {
    family("discover");
}

#[cfg(feature = "fake-http")]
#[test]
fn versions_matches_python() {
    family("versions");
}
