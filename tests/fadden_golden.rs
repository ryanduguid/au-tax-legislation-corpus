//! Python parity, frozen. `tests/golden/<stage>.cases.json` holds cases from the
//! port's differential harness, each run through the Python stages this binary
//! replaced (the base revision, 43b5e0c) on Windows and on Linux. Every input
//! is fabricated. For each case the binary must exit as Python did, print what
//! Python printed (the last line of a traceback where Python printed one), and
//! leave every file with the recorded SHA-256 of its normalised text.
//!
//! Outputs are normalised as the harness normalised them: the case directory
//! reads `ROOT`, the module directory `MODULE`, generated stage, backup and
//! temporary names `HEX`, and bytes that are not UTF-8 `\xNN`. `rulings` and
//! `probe13` fetch over HTTP, so their cases need the scripted responses of
//! the `fake-http` feature: `cargo test --features fake-http --test fadden_golden`.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

const PLATFORM: &str = if cfg!(windows) { "windows" } else { "linux" };

static MANAGED_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(stage|backup)-[0-9a-f]{32}").unwrap());
static TEMP_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[0-9a-f]{32}\.tmp").unwrap());
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

fn module_data() -> Vec<(String, Vec<u8>)> {
    std::fs::read_dir(module_source())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
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

fn normalise(data: &[u8], work: &Path, module: &Path) -> String {
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
fn snapshot(work: &Path, module: &Path) -> BTreeMap<String, Option<String>> {
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
        let value = (!path.is_dir())
            .then(|| sha256(&normalise(&std::fs::read(&path).unwrap(), work, module)));
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

/// Run one case and return what differs from Python, if anything.
fn run_case(family: &str, case: &Value, shared_module: &Path) -> Option<String> {
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
        let stdout = normalise(&out.stdout, &work, &module);
        let stdout = sorted_exts(&CONTACT_DETAIL.replace_all(&stdout, "$1<findings>"));
        let stderr = normalise(&out.stderr, &work, &module).replace("\r\n", "\n");
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
    let left = snapshot(&work, &module);
    let want: BTreeMap<String, Option<String>> =
        serde_json::from_value(expected["files"].clone()).unwrap();
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
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|case| run_case(name, case, &module))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
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
