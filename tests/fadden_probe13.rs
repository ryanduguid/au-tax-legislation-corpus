//! `fadden probe13`: ported from Probe13Tests in tests/corpus/test_pipeline_paths.py
//! and the probe13 cases of tests/corpus/test_output_security.py, with scripted
//! responses and no network traffic.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tax_radar_au::fadden::http::{Failure, Http, Redirects, Response};
use tax_radar_au::fadden::{Run, probe13};

const RID: &str = "C2099A00001";

/// Answers every request with the next scripted body; records requests and waits.
#[derive(Default)]
struct Script {
    bodies: Vec<Result<String, Failure>>,
    events: Vec<String>,
}

impl Http for Script {
    fn get(
        &mut self,
        url: &str,
        _limit: Option<usize>,
        _redirects: &mut Redirects,
    ) -> Result<Response, Failure> {
        self.events.push(format!("GET {url}"));
        let next = if self.bodies.is_empty() {
            Err(Failure::Transport("URLError".into()))
        } else {
            self.bodies.remove(0)
        };
        next.map(|body| Response {
            url: url.into(),
            content_type: "application/json".into(),
            body: body.into_bytes(),
        })
    }
    fn sleep(&mut self, seconds: f64) {
        self.events.push(format!("sleep {seconds}"));
    }
    fn today(&mut self) -> String {
        "2099-02-01".into()
    }
}

/// A source checkout's module directory holding `manifest_raw.json`.
struct Checkout {
    _dir: tempfile::TempDir,
    module: PathBuf,
}

fn checkout(manifest: Value) -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    let module = project.join("fadden");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(project.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(project.join("VERSION"), "x\n").unwrap();
    std::fs::write(module.join("manifest_raw.json"), manifest.to_string()).unwrap();
    Checkout { _dir: dir, module }
}

fn standard() -> Checkout {
    checkout(json!([
        {"id": RID, "name": "Synthetic Tax Act", "epub": null},
        {"id": "C2099A00002", "name": "Downloaded Act", "epub": "C2099A00002.epub"},
    ]))
}

fn probe(
    c: &Checkout,
    bodies: Vec<Result<String, Failure>>,
) -> (Result<(), String>, String, Script) {
    let mut http = Script {
        bodies,
        ..Script::default()
    };
    let mut state = Run::default();
    let outcome = probe13::run(&mut http, &c.module.to_string_lossy(), &mut state)
        .map_err(|e| format!("{}: {e}", e.class()));
    (outcome, String::from_utf8(state.stdout).unwrap(), http)
}

fn written(module: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(module.join("probe13.json")).unwrap()).unwrap()
}

#[test]
fn the_latest_published_compilation_is_recorded() {
    let c = standard();
    let versions = json!([
        {"titleId": RID, "start": "2099-07-01T00:00:00", "isCurrent": true, "compilationNumber": "3", "registerId": null},
        {"titleId": RID, "start": "2099-01-01T00:00:00", "isCurrent": false, "compilationNumber": "2", "registerId": "C2099C00002"},
        {"titleId": RID, "start": "2098-01-01T00:00:00", "isCurrent": false, "compilationNumber": "1", "registerId": "C2098C00001"},
    ]);
    let (outcome, stdout, http) = probe(&c, vec![Ok(json!({"value": versions}).to_string())]);
    outcome.unwrap();
    assert_eq!(
        written(&c.module),
        json!([{"id": RID, "name": "Synthetic Tax Act", "n_versions": 3, "current_has_doc": false, "latest_doc": versions[1]}])
    );
    assert!(
        regex::Regex::new(r"vers=3 +cur_doc=False latest=C2099C00002 2099-01-01")
            .unwrap()
            .is_match(&stdout),
        "{stdout}"
    );
    // Only the title without an EPUB is looked up, newest versions first, then the pause.
    assert_eq!(http.events.len(), 2);
    assert!(
        http.events[0].contains("$orderby=start%20desc&$filter=titleId%20eq%20%27C2099A00001%27")
    );
    assert_eq!(http.events[1], "sleep 1.5");
}

#[test]
fn a_title_with_no_published_document_has_no_latest() {
    let c = standard();
    let (outcome, stdout, _) = probe(&c, vec![Ok(r#"{"value": []}"#.into())]);
    outcome.unwrap();
    assert_eq!(written(&c.module)[0]["latest_doc"], Value::Null);
    assert!(stdout.contains("latest=None "));
}

#[test]
fn a_failed_lookup_retries_then_refuses_to_write() {
    let c = standard();
    let (outcome, _, http) = probe(
        &c,
        vec![
            Ok(r#"{"error": "unavailable"}"#.into()),
            Ok("not json".into()),
            Err(Failure::Status(503)),
        ],
    );
    assert_eq!(
        outcome.unwrap_err(),
        "RuntimeError: version lookup failed for C2099A00001 after retries; refusing to write probe13.json with that title missing"
    );
    assert_eq!(
        http.events.iter().filter(|e| e.starts_with("GET")).count(),
        3
    );
    assert_eq!(http.events.iter().filter(|e| *e == "sleep 6").count(), 2);
    assert!(!c.module.join("probe13.json").exists());
}

#[test]
fn filter_injection_is_refused_before_any_request() {
    let c = checkout(
        json!([{"id": "C2099A00001' or isCurrent eq true", "name": "Synthetic", "epub": null}]),
    );
    let (outcome, _, http) = probe(&c, vec![]);
    assert_eq!(
        outcome.unwrap_err(),
        "ValueError: invalid Federal Register identifier"
    );
    assert!(http.events.is_empty());
}

#[test]
fn malformed_version_rows_cannot_replace_the_probe_manifest() {
    for response in [
        json!({"value": 1}),
        json!({"value": ["wrong shape"]}),
        json!({"value": [{"titleId": "C2099A00002"}]}),
        json!({"value": [{"titleId": RID, "start": "2099-02-30", "registerId": null}]}),
        json!({"value": [{"titleId": RID, "start": "2099-01-01"}]}),
    ] {
        let c = checkout(json!([{"id": RID, "name": "Synthetic", "epub": null}]));
        std::fs::write(c.module.join("probe13.json"), "previous manifest").unwrap();
        let (outcome, _, _) = probe(&c, vec![Ok(response.to_string())]);
        assert!(
            outcome.unwrap_err().starts_with("ValueError: "),
            "{response}"
        );
        assert_eq!(
            std::fs::read_to_string(c.module.join("probe13.json")).unwrap(),
            "previous manifest"
        );
    }
}
