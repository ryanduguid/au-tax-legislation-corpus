//! `fadden versions`: ported from tests/corpus/test_version_batches.py and the versions cases of
//! tests/corpus/test_regressions.py, with scripted responses and no network traffic.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tax_radar_au::fadden::http::{Failure, Http, Response};
use tax_radar_au::fadden::{Exc, Run, versions};

/// Answers each request from `answer`; requests and waits are recorded in order.
struct Script<F: FnMut(&str) -> Result<String, Failure>> {
    answer: F,
    events: Vec<String>,
}

impl<F: FnMut(&str) -> Result<String, Failure>> Http for Script<F> {
    fn get(&mut self, url: &str, _limit: Option<usize>) -> Result<Response, Failure> {
        self.events.push(format!("GET {url}"));
        (self.answer)(url).map(|body| Response {
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

struct Checkout {
    _dir: tempfile::TempDir,
    module: PathBuf,
}

fn checkout(titles: &Value) -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    let module = project.join("fadden");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(project.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(project.join("VERSION"), "x\n").unwrap();
    std::fs::write(module.join("titles_all.json"), titles.to_string()).unwrap();
    Checkout { _dir: dir, module }
}

fn rid(n: usize) -> String {
    format!("C2099A{n:05}")
}

/// `n` principal titles, named so that sorting keeps their numeric order.
fn titles(n: usize) -> Value {
    Value::Array(
        (1..=n)
            .map(|i| json!({"id": rid(i), "name": format!("Act {i:04}"), "isPrincipal": true, "collection": "Act"}))
            .collect(),
    )
}

fn row(id: &str) -> Value {
    json!({"titleId": id, "start": "2026-07-01", "compilationNumber": "3", "registerId": "C2099C00001"})
}

/// The title ids a versions URL asks for, in order.
fn asked(url: &str) -> Vec<String> {
    url.match_indices("titleId%20eq%20%27")
        .map(|(at, m)| url[at + m.len()..at + m.len() + 11].to_string())
        .collect()
}

/// A complete, valid answer for whatever titles the URL asks for; the probe gets one row.
fn complete(url: &str) -> Result<String, Failure> {
    let ids = asked(url);
    Ok(json!({"value": ids.iter().map(|id| row(id)).collect::<Vec<_>>()}).to_string())
}

fn run<F: FnMut(&str) -> Result<String, Failure>>(
    script: &mut Script<F>,
    module: &Path,
) -> (Result<(), Exc>, String) {
    let mut out = Run::default();
    let result = versions::run(script, module.to_str().unwrap(), &mut out);
    (result, String::from_utf8_lossy(&out.stdout).into_owned())
}

fn calls<F: FnMut(&str) -> Result<String, Failure>>(script: &Script<F>) -> (usize, usize) {
    let gets = script
        .events
        .iter()
        .filter(|e| e.starts_with("GET "))
        .count();
    let sleeps = script.events.iter().filter(|e| *e == "sleep 1.5").count();
    (gets, sleeps)
}

#[test]
fn a_946_title_workload_uses_120_calls_and_119_pauses() {
    let co = checkout(&titles(946));
    let mut s = Script {
        answer: complete,
        events: Vec::new(),
    };
    let (result, _) = run(&mut s, &co.module);
    assert!(result.is_ok());
    // One probe and 119 batches of up to 8; a pause follows each batch.
    assert_eq!(calls(&s), (120, 119));
    let resolved: Value = serde_json::from_str(
        &std::fs::read_to_string(co.module.join("acts_resolved.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(resolved.as_array().unwrap().len(), 946);
    assert_eq!(resolved[0]["versionStart"], "2026-07-01");
    assert_eq!(resolved[0]["compilationRegisterId"], "C2099C00001");
}

#[test]
fn group_boundaries_and_the_exact_singleton_url() {
    let co = checkout(&titles(9));
    let mut s = Script {
        answer: complete,
        events: Vec::new(),
    };
    assert!(run(&mut s, &co.module).0.is_ok());
    let gets: Vec<&String> = s.events.iter().filter(|e| e.starts_with("GET ")).collect();
    assert_eq!(asked(gets[1]).len(), 8);
    assert!(gets[1].contains("$top=9&"), "{}", gets[1]);
    assert_eq!(
        gets[2].as_str(),
        "GET https://api.prod.legislation.gov.au/v1/versions?$top=1&$filter=titleId%20eq%20%27C2099A00009%27%20and%20isCurrent%20eq%20true&$select=titleId,start,compilationNumber,registerId"
    );
}

#[test]
fn an_unavailable_batch_switches_to_singletons_for_the_rest_of_the_run() {
    let co = checkout(&titles(17));
    let mut first_batch = true;
    let mut s = Script {
        answer: |url: &str| {
            if asked(url).len() == 8 && first_batch {
                // fetch_json gives up after 3 attempts.
                if url.contains("C2099A00008") {
                    return Err(Failure::Status(503));
                }
            }
            if asked(url).len() == 1 && url.contains("C2099A00001") {
                first_batch = false;
            }
            complete(url)
        },
        events: Vec::new(),
    };
    let (result, stdout) = run(&mut s, &co.module);
    assert!(result.is_ok());
    assert!(stdout.contains("!! version batch unavailable; switching to single-title lookups\n"));
    // Probe, 3 failed attempts at the first batch, then 17 singletons.
    let gets: Vec<&String> = s.events.iter().filter(|e| e.starts_with("GET ")).collect();
    assert_eq!(gets.len(), 1 + 3 + 17);
    assert!(gets[4..].iter().all(|g| asked(g).len() == 1));
}

#[test]
fn an_ambiguous_group_is_retried_singly_and_batching_resumes() {
    let co = checkout(&titles(16));
    let mut s = Script {
        answer: |url: &str| {
            let ids = asked(url);
            if ids.len() == 8 && ids[0] == rid(1) {
                // One title missing from its batch: valid but ambiguous.
                return Ok(
                    json!({"value": ids[1..].iter().map(|id| row(id)).collect::<Vec<_>>()})
                        .to_string(),
                );
            }
            complete(url)
        },
        events: Vec::new(),
    };
    let (result, stdout) = run(&mut s, &co.module);
    assert!(result.is_ok());
    assert!(stdout.contains("!! ambiguous version batch; retrying 8 title(s) individually\n"));
    let sizes: Vec<usize> = s
        .events
        .iter()
        .filter(|e| e.starts_with("GET "))
        .map(|g| asked(g).len())
        .collect();
    assert_eq!(sizes, [1, 8, 1, 1, 1, 1, 1, 1, 1, 1, 8]);
}

#[test]
fn a_malformed_batch_stops_the_stage_without_a_manifest() {
    for bad in [
        json!({"value": [row(&rid(1))], "@odata.nextLink": null}),
        json!({"value": (1..=10).map(|_| row(&rid(1))).collect::<Vec<_>>()}),
        json!({"value": [row("C2099Z99999")]}),
        json!({"value": [{"titleId": rid(1), "start": "2026-02-30", "registerId": "C2099C00001"}]}),
        json!({"value": [1]}),
    ] {
        let co = checkout(&titles(8));
        std::fs::write(co.module.join("acts_resolved.json"), "sentinel").unwrap();
        let mut s = Script {
            answer: |url: &str| {
                if asked(url).len() == 8 {
                    Ok(bad.to_string())
                } else {
                    complete(url)
                }
            },
            events: Vec::new(),
        };
        let (result, _) = run(&mut s, &co.module);
        let Err(error) = result else {
            panic!("expected a RuntimeError for {bad}")
        };
        assert_eq!(
            error.to_string(),
            "invalid version batch; refusing to write acts_resolved.json"
        );
        // The earlier manifest survives the refusal untouched.
        assert_eq!(
            std::fs::read_to_string(co.module.join("acts_resolved.json")).unwrap(),
            "sentinel"
        );
        // A malformed batch is never recovered title by title.
        assert_eq!(calls(&s).0, 2, "{bad}");
    }
}

#[test]
fn every_input_id_is_validated_before_any_fetch() {
    // A valid title first: an incremental check would fetch it before the bad one.
    let co = checkout(&json!([
        {"id": rid(1), "name": "Act", "isPrincipal": true},
        {"id": "not an id", "name": "Act", "isPrincipal": true},
    ]));
    let mut s = Script {
        answer: complete,
        events: Vec::new(),
    };
    let (result, _) = run(&mut s, &co.module);
    assert!(matches!(result, Err(Exc::Value { .. })));
    assert!(s.events.is_empty());
}

#[test]
fn the_last_duplicate_wins_and_principal_titles_sort_by_name() {
    let co = checkout(&json!([
        {"id": rid(1), "name": "Zed Act", "isPrincipal": true, "collection": "Act"},
        {"id": rid(2), "name": "Bee Act", "isPrincipal": false, "collection": "Act"},
        {"id": rid(1), "name": "Aye Act", "isPrincipal": true, "collection": "Act"},
        {"id": rid(3), "name": "Cee Act", "isPrincipal": true, "collection": "LegislativeInstrument"}
    ]));
    let mut s = Script {
        answer: complete,
        events: Vec::new(),
    };
    let (result, stdout) = run(&mut s, &co.module);
    assert!(result.is_ok());
    assert!(stdout.starts_with(
        "distinct in-force titles: 3\ndistinct principal titles:   2\nby collection: Act 2, LegislativeInstrument 1\n"
    ));
    let gets: Vec<&String> = s.events.iter().filter(|e| e.starts_with("GET ")).collect();
    assert_eq!(asked(gets[1]), [rid(1), rid(3)]);
    let resolved: Value = serde_json::from_str(
        &std::fs::read_to_string(co.module.join("acts_resolved.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(resolved[0]["name"], "Aye Act");
}

#[test]
fn an_unresolved_title_fails_the_stage_and_keeps_the_old_manifest() {
    let co = checkout(&titles(2));
    std::fs::write(co.module.join("acts_resolved.json"), "old").unwrap();
    let mut s = Script {
        answer: |url: &str| {
            let ids = asked(url);
            if ids.len() == 2 {
                // Ambiguous, then title 2 has no current version.
                return Ok(json!({"value": [row(&ids[0])]}).to_string());
            }
            if ids == [rid(2)] {
                return Ok(json!({"value": []}).to_string());
            }
            complete(url)
        },
        events: Vec::new(),
    };
    let (result, stdout) = run(&mut s, &co.module);
    let Err(error) = result else {
        panic!("expected a RuntimeError")
    };
    assert_eq!(
        error.to_string(),
        "version resolution incomplete for 1 title(s); refusing to write acts_resolved.json"
    );
    assert!(stdout.contains("\nresolved: 1   failed: 1\n   FAILED C2099A00002 Act 0002\n"));
    assert_eq!(
        std::fs::read_to_string(co.module.join("acts_resolved.json")).unwrap(),
        "old"
    );
}

#[test]
fn an_empty_probe_does_not_control_resolution() {
    let co = checkout(&titles(1));
    let mut s = Script {
        answer: |url: &str| {
            if url.contains("C2004A05138") {
                Ok(json!({"value": []}).to_string())
            } else {
                complete(url)
            }
        },
        events: Vec::new(),
    };
    let (result, stdout) = run(&mut s, &co.module);
    assert!(result.is_ok());
    assert!(stdout.contains("\nprobe isCurrent filter: []\n!! isCurrent filter probe returned no rows; current-version resolution will still be attempted\n"));
}
