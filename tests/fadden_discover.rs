//! `fadden discover`: ported from DiscoverTests in tests/corpus/test_pipeline_paths.py and the
//! discovery cases of tests/corpus/test_regressions.py (failure handling, paging, labels), with
//! scripted responses and no network traffic.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tax_radar_au::fadden::Run;
use tax_radar_au::fadden::discover::{self, Stop};
use tax_radar_au::fadden::http::{Failure, Http, Redirects, Response};

/// Answers each request with the next body; requests and waits are recorded in order.
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

struct Checkout {
    _dir: tempfile::TempDir,
    module: PathBuf,
}

fn checkout() -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    let module = project.join("fadden");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(project.join("pyproject.toml"), "x\n").unwrap();
    std::fs::write(project.join("VERSION"), "x\n").unwrap();
    Checkout { _dir: dir, module }
}

fn title(id: &str, name: &str, principal: bool, collection: &str) -> Value {
    json!({"id": id, "name": name, "isPrincipal": principal, "isInForce": true,
           "collection": collection, "year": 2099, "number": 1, "status": "InForce"})
}

/// 15 searches (3 collections by 5 keywords); `pages(search)` gives each one's pages.
fn script(pages: impl Fn(usize) -> Vec<Vec<Value>>) -> Script {
    let mut bodies = Vec::new();
    for search in 0..15 {
        for page in pages(search) {
            bodies.push(Ok(json!({"value": page}).to_string()));
        }
    }
    Script {
        bodies,
        events: Vec::new(),
    }
}

fn run(script: &mut Script, module: &Path) -> (Result<(), Stop>, Run) {
    let mut out = Run::default();
    let result = discover::run(script, module.to_str().unwrap(), &mut out);
    (result, out)
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn a_title_matched_by_two_keywords_is_listed_once() {
    let co = checkout();
    let shared = title("C2099A00001", "Tax and Excise Act", true, "Act");
    let mut s = script(|search| match search {
        0 | 1 => vec![vec![shared.clone()]],
        _ => vec![vec![]],
    });
    let (result, out) = run(&mut s, &co.module);
    assert!(result.is_ok());
    assert_eq!(read(&co.module.join("titles_all.json")), json!([shared]));
    assert_eq!(
        read(&co.module.join("titles_principal.json")),
        json!([shared])
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("Act                    Tax              matched    1, new    1\n"));
    assert!(stdout.contains("Act                    Excise           matched    1, new    0\n"));
}

#[test]
fn a_failed_page_writes_neither_output() {
    let co = checkout();
    std::fs::write(co.module.join("titles_all.json"), "[\"old\"]").unwrap();
    // The third search's page fails all 3 attempts after 2 good searches.
    let mut bodies: Vec<Result<String, Failure>> = vec![
        Ok(json!({"value": []}).to_string()),
        Ok(json!({"value": []}).to_string()),
    ];
    bodies.extend((0..3).map(|_| Err(Failure::Status(503))));
    let mut s = Script {
        bodies,
        events: Vec::new(),
    };
    let (result, _) = run(&mut s, &co.module);
    let Err(Stop::Raised(error)) = result else {
        panic!("expected a RuntimeError")
    };
    assert_eq!(error.class(), "RuntimeError");
    assert_eq!(
        error.to_string(),
        "title discovery failed at skip=0 for 'Superannuation'; refusing to write an incomplete title list"
    );
    assert_eq!(
        std::fs::read_to_string(co.module.join("titles_all.json")).unwrap(),
        "[\"old\"]"
    );
    assert!(!co.module.join("titles_principal.json").exists());
}

#[test]
fn paging_orders_by_id_and_refuses_a_page_that_repeats_one() {
    let co = checkout();
    let rows: Vec<Value> = (0..100)
        .map(|n| title(&format!("C2099A{n:05}"), "Tax Act", false, "Act"))
        .collect();
    let repeat = vec![rows[5].clone()];
    let mut s = script(|search| {
        if search == 0 {
            vec![rows.clone(), repeat.clone()]
        } else {
            vec![vec![]]
        }
    });
    let (result, _) = run(&mut s, &co.module);
    let Err(Stop::Exit(message)) = result else {
        panic!("expected SystemExit")
    };
    assert_eq!(
        message,
        "duplicate ids while paging 'Tax': unordered paging"
    );
    let first = &s.events[0];
    assert!(
        first.contains("$top=100&$skip=0&$orderby=id&$filter="),
        "{first}"
    );
    assert!(s.events[2].contains("$skip=100"), "{}", s.events[2]);
    assert_eq!(s.events[1], "sleep 2");
}

#[test]
fn ids_that_python_equates_are_one_id_while_paging() {
    // 1 and 1.0 hash alike, so a page holding both repeats an id.
    let co = checkout();
    let page = vec![
        json!({"id": 1, "name": "A"}),
        json!({"id": 1.0, "name": "B"}),
    ];
    let mut s = script(|search| {
        if search == 0 {
            vec![page.clone()]
        } else {
            vec![vec![]]
        }
    });
    let (result, _) = run(&mut s, &co.module);
    let Err(Stop::Exit(message)) = result else {
        panic!("expected SystemExit")
    };
    assert_eq!(
        message,
        "duplicate ids while paging 'Tax': unordered paging"
    );
    assert!(!co.module.join("titles_all.json").exists());
}

#[test]
fn an_id_python_equates_with_one_on_an_earlier_page_repeats_it() {
    // A full first page of int ids, then the float equal to one of them.
    let co = checkout();
    let rows: Vec<Value> = (1..=100).map(|n| json!({"id": n, "name": "A"})).collect();
    let mut s = script(|search| {
        if search == 0 {
            vec![rows.clone(), vec![json!({"id": 7.0, "name": "B"})]]
        } else {
            vec![vec![]]
        }
    });
    let (result, _) = run(&mut s, &co.module);
    let Err(Stop::Exit(message)) = result else {
        panic!("expected SystemExit")
    };
    assert_eq!(
        message,
        "duplicate ids while paging 'Tax': unordered paging"
    );
    assert!(s.events[2].contains("$skip=100"), "{}", s.events[2]);
    assert!(!co.module.join("titles_all.json").exists());
    assert!(!co.module.join("titles_principal.json").exists());
}

#[test]
fn an_unhashable_id_raises_the_set_type_error() {
    let co = checkout();
    let page = vec![json!({"id": [1], "name": "A"})];
    let mut s = script(|search| {
        if search == 0 {
            vec![page.clone()]
        } else {
            vec![vec![]]
        }
    });
    let (result, _) = run(&mut s, &co.module);
    let Err(Stop::Raised(error)) = result else {
        panic!("expected a TypeError")
    };
    assert_eq!(
        error.to_string(),
        "cannot use 'list' as a set element (unhashable type: 'list')"
    );
}

#[test]
fn totals_count_titles_and_keep_the_collection_breakdown() {
    let co = checkout();
    let act = title("C2099A00001", "Income Tax Act", true, "Act");
    let instrument = title(
        "F2099L00001",
        "Tax Regulations",
        false,
        "LegislativeInstrument",
    );
    let mut s = script(|search| match search {
        0 => vec![vec![act.clone()]],
        5 => vec![vec![instrument.clone()]],
        _ => vec![vec![]],
    });
    let (result, out) = run(&mut s, &co.module);
    assert!(result.is_ok());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("LegislativeInstrument  Tax              matched    1, new    1\n"));
    assert!(stdout.contains(
        "\ntotal distinct in-force titles: 2\nof which principal (client-side filter): 1\n"
    ));
    assert!(stdout.ends_with("\nsample principal titles:\n   C2099A00001  Income Tax Act\n"));
    // 15 searches, each followed by a 3-second pause.
    assert_eq!(s.events.iter().filter(|e| *e == "sleep 3").count(), 15);
}
