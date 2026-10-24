//! `fadden rulings`: ported from tests/corpus/test_rulings.py, with fabricated
//! Legal Database pages and no network traffic.
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::Run;
use tax_radar_au::fadden::http::{Failure, Http, RETRY_DELAY, Request, Response};
use tax_radar_au::fadden::rulings::{
    self, BASE_URL, MAX_BYTES, MAX_TARGETS, NOTICE, Parsed, RulingsError, TargetsError,
};
use tax_radar_au::pypath::PyPath;

const DOCID: &str = "TXR/TR20991/NAT/ATO/00001";
const EV_ID: &str = "EV/1052000000001";

#[derive(Default)]
struct Page {
    docid: Option<String>,
    notice: Option<bool>,
    rights: String,
    body_extra: String,
    reference: Option<String>,
}

/// A fabricated page shaped like a Legal Database document.
fn page_with(p: Page) -> String {
    let docid = p.docid.unwrap_or_else(|| DOCID.into());
    let reference = p.reference.unwrap_or_else(|| docid.clone());
    let rights_meta = if p.rights.is_empty() {
        String::new()
    } else {
        format!(r#"<meta content="{}" name="dc.Rights" />"#, p.rights)
    };
    let notice_html = if p.notice.unwrap_or(true) {
        format!("<p>{NOTICE}</p>")
    } else {
        String::new()
    };
    let extra = p.body_extra;
    format!(
        r##"<html><head>
<meta content="{reference}" name="ato.reference.id" />
<meta content="TR 2099/1 - Fabricated ruling for tests" name="DC.Title" />
<meta content="ATO Ruling" name="dc.Type.documentType" />
<meta content="2099/01/31" name="dc.Date.Issued" />
{rights_meta}
</head><body><div id="main-content">
<div id="LawFront"><p>Front matter that is not paragraph text</p></div>
<div id="LawBody">
  <div class="panel panel-default"><table><tr><td>Table of Contents</td><td>1</td></tr></table></div>
  <p><strong>What this Ruling is about</strong><a name="H1" id="H1"></a></p>
  <p class="indentlevel0"><a name="P1" id="P1"></a>1. A fabricated first paragraph.<sup><a href="#fp1">[1]</a></sup></p>
  <p class="indentlevel0"><a name="P2" id="P2"></a>2. A second paragraph with a list:</p>
  <ul><li>first item</li><li>second item</li></ul>
  <h3><a name="H2" id="H2"></a>Ruling</h3>
  <p class="indentlevel0"><a name="P3" id="P3"></a>3. The ruling paragraph.</p>
  {extra}
  <p class="indentlevel0"><a name="COPYRIGHT" id="COPYRIGHT"></a>Copyright line</p>
  {notice_html}
  <div id="footnotes"><p>[1] A footnote that is not a paragraph.</p></div>
</div></div></body></html>"##
    )
}

fn page() -> String {
    page_with(Page::default())
}

fn unanchored(text: &str) -> String {
    regex::Regex::new(r#"<a name="P\d+" id="P\d+"></a>"#)
        .unwrap()
        .replace_all(text, "")
        .into_owned()
}

fn parse(raw: &str, docid: &str) -> Result<Parsed, RulingsError> {
    rulings::parse_document(raw.as_bytes(), docid)
}

fn numbers(parsed: &Parsed) -> Vec<&str> {
    parsed
        .paragraphs
        .iter()
        .map(|p| p.paragraph.as_str())
        .collect()
}

fn refused(result: Result<Parsed, RulingsError>, needle: &str) {
    let error = result.unwrap_err().0;
    assert!(error.contains(needle), "{error:?} lacks {needle:?}");
}

const WITHDRAWN: &str = r#"<div id="LawBody"><h3>Notice of Withdrawal</h3><p><a name="PA"></a>This fabricated ruling is withdrawn from 1 July 2099.</p>"#;

/// Scripted responses and a record of every request and wait.
#[derive(Default)]
struct Script {
    responses: Vec<Result<Response, Failure>>,
    events: Vec<String>,
}

impl Script {
    fn ok(url: &str, kind: &str, body: &[u8]) -> Result<Response, Failure> {
        Ok(Response {
            url: url.into(),
            content_type: kind.into(),
            body: body.to_vec(),
        })
    }
}

impl Http for Script {
    fn get(
        &mut self,
        url: &str,
        limit: Option<usize>,
        _request: &mut Request,
    ) -> Result<Response, Failure> {
        self.events.push(format!("GET {url}"));
        let mut next = if self.responses.is_empty() {
            Err(Failure::Transport("URLError".into()))
        } else {
            self.responses.remove(0)
        };
        if let (Ok(r), Some(n)) = (&mut next, limit) {
            r.body.truncate(n);
        }
        next
    }
    fn sleep(&mut self, seconds: f64) {
        self.events.push(format!("sleep {seconds}"));
    }
    fn today(&mut self) -> String {
        "2099-02-01".into()
    }
}

/// Serves each docid's page, as the Python tests' `fetcher` did.
struct Pages(Vec<(String, Result<Response, Failure>)>, Vec<String>);

impl Http for Pages {
    fn get(
        &mut self,
        url: &str,
        _limit: Option<usize>,
        _request: &mut Request,
    ) -> Result<Response, Failure> {
        let docid = url.strip_prefix(BASE_URL).unwrap();
        self.0
            .iter()
            .find(|(d, _)| d == docid)
            .map(|(_, r)| r.clone())
            .unwrap()
    }
    fn sleep(&mut self, seconds: f64) {
        self.1.push(format!("sleep {seconds}"));
    }
    fn today(&mut self) -> String {
        "2099-02-01".into()
    }
}

fn served(docid: &str, raw: &str) -> (String, Result<Response, Failure>) {
    (
        docid.into(),
        Script::ok(
            &format!("{BASE_URL}{docid}"),
            "text/html; charset=utf-8",
            raw.as_bytes(),
        ),
    )
}

fn run_pages(
    pages: Vec<(String, Result<Response, Failure>)>,
    targets: &[&str],
    out: &Path,
) -> (i32, Run, Vec<String>) {
    let mut http = Pages(pages, Vec::new());
    let mut state = Run::default();
    let targets: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
    let code = rulings::run(
        &mut http,
        &targets,
        &PyPath::new(&out.to_string_lossy()),
        &mut state,
    )
    .unwrap()
    .unwrap();
    (code, state, http.1)
}

fn manifest(out: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap()
}

fn rows(out: &Path) -> Vec<Value> {
    std::fs::read_to_string(out.join("rulings.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// ---------------------------------------------------------------- parsing

#[test]
fn empty_notice_fragments_remain_unknown() {
    let raw = page().replace(
        r#"<div id="LawBody">"#,
        r#"<div id="LawBody"><h3>Notice of Withdrawal</h3><p><a name="PA"></a></p><p><a name="PB"></a></p><h3><a name="H999"></a>Ruling</h3>"#,
    );
    let parsed = parse(&raw, DOCID).unwrap();
    assert_eq!(
        (parsed.withdrawal_status, parsed.withdrawal_notice.as_str()),
        ("UNKNOWN", "")
    );
    assert_eq!(numbers(&parsed), ["A", "B", "1", "2", "3"]);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("run");
    let (code, _, _) = run_pages(vec![served(DOCID, &raw)], &[DOCID], &out);
    assert_eq!(code, 0);
    let manifest = manifest(&out);
    let rows = rows(&out);
    for record in manifest["documents"]
        .as_array()
        .unwrap()
        .iter()
        .chain(&rows)
    {
        assert_eq!(
            (&record["withdrawal_status"], &record["withdrawal_notice"]),
            (&json!("UNKNOWN"), &json!(""))
        );
    }
    assert_eq!(rows.last().unwrap()["text"], "3. The ruling paragraph.");
}

#[test]
fn withdrawal_notice_keeps_its_lettered_paragraph() {
    let parsed = parse(&page().replace(r#"<div id="LawBody">"#, WITHDRAWN), DOCID).unwrap();
    assert_eq!(parsed.withdrawal_status, "NOTICE_PRESENT");
    assert_eq!(
        parsed.withdrawal_notice,
        "This fabricated ruling is withdrawn from 1 July 2099."
    );
    assert_eq!(parsed.paragraphs[0].paragraph, "A");
    let mentions = parse(
        &page_with(Page {
            body_extra: "<p>Other rulings were withdrawn.</p>".into(),
            ..Page::default()
        }),
        DOCID,
    )
    .unwrap();
    assert_eq!(
        (
            mentions.withdrawal_status,
            mentions.withdrawal_notice.as_str()
        ),
        ("UNKNOWN", "")
    );
}

#[test]
fn lettered_notice_keeps_printed_numbered_paragraphs() {
    let parsed = parse(
        &unanchored(&page()).replace(r#"<div id="LawBody">"#, WITHDRAWN),
        DOCID,
    )
    .unwrap();
    assert_eq!(numbers(&parsed), ["A", "1", "2", "3"]);
    assert_eq!(
        parsed.paragraphs[2].text,
        "2. A second paragraph with a list: first item second item"
    );
    assert_eq!(parsed.withdrawal_status, "NOTICE_PRESENT");
}

#[test]
fn paragraphs_headings_and_continuations() {
    let parsed = parse(&page(), DOCID).unwrap();
    assert_eq!(parsed.licence_basis, "document-notice");
    assert_eq!(parsed.title, "TR 2099/1 - Fabricated ruling for tests");
    assert_eq!(parsed.issued, "2099/01/31");
    let got: Vec<(&str, &str, &str)> = parsed
        .paragraphs
        .iter()
        .map(|p| (p.paragraph.as_str(), p.heading.as_str(), p.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (
                "1",
                "What this Ruling is about",
                "1. A fabricated first paragraph."
            ),
            (
                "2",
                "What this Ruling is about",
                "2. A second paragraph with a list: first item second item"
            ),
            ("3", "Ruling", "3. The ruling paragraph."),
        ]
    );
    let text: Vec<&str> = parsed.paragraphs.iter().map(|p| p.text.as_str()).collect();
    let text = text.join(" ");
    for absent in [
        "Table of Contents",
        "[1]",
        "footnote",
        "Copyright line",
        "Front matter",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
    assert!(parsed.paragraphs.iter().all(|p| p.numbering == "document"));
}

#[test]
fn reference_and_notice_are_checked() {
    refused(
        parse(
            &page_with(Page {
                reference: Some("TXR/TR20992/NAT/ATO/00001".into()),
                ..Page::default()
            }),
            DOCID,
        ),
        "does not match",
    );
    refused(
        parse(
            &page_with(Page {
                reference: Some(format!("{DOCID}0")),
                ..Page::default()
            }),
            DOCID,
        ),
        "does not match",
    );
    let lower = parse(
        &page_with(Page {
            reference: Some(format!("{}/", DOCID.to_lowercase())),
            ..Page::default()
        }),
        DOCID,
    )
    .unwrap();
    assert_eq!(lower.reference, format!("{}/", DOCID.to_lowercase()));
    refused(
        parse(
            &page_with(Page {
                notice: Some(false),
                ..Page::default()
            }),
            DOCID,
        ),
        "no ATO reuse notice",
    );
}

#[test]
fn dc_rights_must_be_the_ato_copyright_address() {
    for value in [
        "http://www.ato.gov.au/content/corporate/about_this_site.htm#copyright",
        "https://ato.gov.au/content/corporate/about_this_site.htm#copyright",
    ] {
        let parsed = parse(
            &page_with(Page {
                notice: Some(false),
                rights: value.into(),
                ..Page::default()
            }),
            DOCID,
        )
        .unwrap();
        assert_eq!(parsed.licence_basis, "site-notice-via-dc-rights", "{value}");
    }
    for value in [
        "http://example.invalid/content/corporate/about_this_site.htm#copyright",
        "http://www.ato.gov.au.example.invalid/content/corporate/about_this_site.htm#copyright",
        "http://www.ato.gov.au/elsewhere/about_this_site.htm#copyright",
        "http://www.ato.gov.au:8080/content/corporate/about_this_site.htm#copyright",
        "about_this_site.htm#copyright",
        "http://www.ato.gov.au:notaport/content/corporate/about_this_site.htm#copyright",
        "http://www.ato.gov.au:99999/content/corporate/about_this_site.htm#copyright",
    ] {
        refused(
            parse(
                &page_with(Page {
                    notice: Some(false),
                    rights: value.into(),
                    ..Page::default()
                }),
                DOCID,
            ),
            "no ATO reuse",
        );
    }
}

#[test]
fn printed_numbers_are_used_only_when_no_paragraph_anchors_exist() {
    let parsed = parse(&unanchored(&page()), DOCID).unwrap();
    assert_eq!(numbers(&parsed), ["1", "2", "3"]);
    assert_eq!(
        parsed.paragraphs[1].text,
        "2. A second paragraph with a list: first item second item"
    );
    let mixed = parse(
        &page_with(Page {
            body_extra: "<p>4. Printed but unanchored.</p>".into(),
            ..Page::default()
        }),
        DOCID,
    )
    .unwrap();
    assert_eq!(numbers(&mixed), ["1", "2", "3"]);
    let empty = regex::Regex::new(r">\d\. ")
        .unwrap()
        .replace_all(&page().replace(r#"name="P"#, r#"name="X"#), ">")
        .into_owned();
    refused(parse(&empty, DOCID), "no numbered paragraphs");
}

#[test]
fn nested_blocks_repeated_anchors_and_empty_blocks() {
    let extra = r#"<p class="indentlevel0"><a name="P4"></a><a name="P4"></a>4. Repeated anchor.</p><ul><li><p>5. nested</p> list item</li></ul><p></p>"#;
    let parsed = parse(
        &page_with(Page {
            body_extra: extra.into(),
            ..Page::default()
        }),
        DOCID,
    )
    .unwrap();
    let tail: Vec<(&str, &str)> = parsed.paragraphs[3..]
        .iter()
        .map(|p| (p.paragraph.as_str(), p.text.as_str()))
        .collect();
    assert_eq!(tail, [("4", "4. Repeated anchor. 5. nested list item")]);
    let error = rulings::parse_document(b"\xff\xfe<html></html>", DOCID)
        .unwrap_err()
        .0;
    assert!(error.contains("response is not UTF-8"));
}

fn ev_page() -> String {
    format!(
        r##"<html><head>
<meta content="{EV_ID}" name="ato.reference.id" />
<meta content="GST - Fabricated arrangement" name="DC_TITLE" />
<meta content="2099/01/31" name="DC_DATE_ISSUED" />
<meta content="http://www.ato.gov.au/content/corporate/about_this_site.htm#copyright" name="DC_RIGHTS" />
</head><body><main><article>
<div class="container-fluid"><a href="#">Back to browse</a></div>
<DIV ID="ev_disclaimer_v0"><table><tr><td>You cannot rely on this record.</td></tr></table></DIV>
<P><STRONG>Question 1</STRONG></P>
<P>Is Entity X making a supply?</P>
<P><STRONG>Answer</STRONG></P>
<P>Yes. Entity X makes a supply under section 9-5.</P>
<div class="container-fluid"><a href="#main-content">Back to top</a></div>
</article></main><footer><p>Footer text</p></footer></body></html>"##
    )
}

#[test]
fn edited_versions_are_numbered_in_order_and_marked_sequential() {
    let parsed = parse(&ev_page(), EV_ID).unwrap();
    assert_eq!(parsed.licence_basis, "site-notice-via-dc-rights");
    assert_eq!(parsed.title, "GST - Fabricated arrangement");
    let got: Vec<(&str, &str, &str, &str)> = parsed
        .paragraphs
        .iter()
        .map(|p| {
            (
                p.paragraph.as_str(),
                p.heading.as_str(),
                p.text.as_str(),
                p.numbering,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "1",
                "Question 1",
                "Is Entity X making a supply?",
                "sequential"
            ),
            (
                "2",
                "Answer",
                "Yes. Entity X makes a supply under section 9-5.",
                "sequential"
            ),
        ]
    );
    let text: String = parsed
        .paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for absent in ["cannot rely", "Back to", "Footer"] {
        assert!(!text.contains(absent));
    }
    let trailing =
        "<div class=\"container-fluid\"><a href=\"#main-content\">Back to top</a></div>\n";
    let closed = parse(
        &ev_page().replace(trailing, r#"<P><A NAME="anchor-only"></A></P>"#),
        EV_ID,
    )
    .unwrap();
    assert_eq!(numbers(&closed), ["1", "2"]);
    let bare = regex::Regex::new(r"<P>[^<]*</P>")
        .unwrap()
        .replace_all(&ev_page(), "")
        .into_owned();
    refused(parse(&bare, EV_ID), "no paragraphs found after the edited");
}

#[test]
fn personal_data_anywhere_in_the_document_region_is_found() {
    let contact = "Call 02 6216 1111.";
    for raw in [
        page().replace(
            "<strong>What this Ruling is about</strong>",
            &format!("<strong>{contact}</strong>"),
        ),
        page().replace("TR 2099/1 - Fabricated ruling for tests", contact),
        page().replace("[1] A footnote that is not a paragraph.", contact),
        page().replace("Table of Contents", contact),
    ] {
        let parsed = parse(&raw, DOCID).unwrap();
        assert_eq!(
            rulings::pii_findings(&rulings::scanned_texts(&parsed)),
            ["phone"]
        );
    }
    let chrome = page().replace(
        r#"<div id="main-content">"#,
        r#"<header><p>Phone 13 28 61 or 02 6216 1111</p></header><div id="main-content">"#,
    );
    let parsed = parse(&chrome, DOCID).unwrap();
    assert!(rulings::pii_findings(&rulings::scanned_texts(&parsed)).is_empty());
    assert_eq!(
        rulings::pii_findings(&["Agent Jane Citizen 12345678"]),
        ["name-with-registration-number"]
    );
}

// ---------------------------------------------------------------- targets

fn targets(value: Value) -> Result<Vec<String>, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("targets.json");
    std::fs::write(&path, value.to_string()).unwrap();
    rulings::load_targets(&path.to_string_lossy()).map_err(|e| match e {
        TargetsError::Refused(error) => error.0,
        TargetsError::Uncaught(error) => format!("uncaught {error}"),
    })
}

#[test]
fn only_ato_authored_families_are_accepted() {
    assert_eq!(targets(json!([DOCID, EV_ID])).unwrap(), [DOCID, EV_ID]);
    for docid in ["PAC/19970038/6-5", "JUD/2099ATC20-001"] {
        assert!(targets(json!([docid])).unwrap_err().contains("not an ATO"));
    }
}

#[test]
fn duplicates_malformed_ids_and_oversize_lists_are_refused() {
    let many: Vec<String> = (0..=MAX_TARGETS).map(|n| format!("EV/{n:013}")).collect();
    for (value, message) in [
        (json!([DOCID, DOCID.to_lowercase()]), "duplicate"),
        (json!(["not a docid"]), "not a Legal Database docid"),
        (json!(many), "exceed the cap"),
        (json!({"docid": DOCID}), "JSON array"),
    ] {
        let error = targets(value).unwrap_err();
        assert!(error.contains(message), "{error:?}");
    }
}

// ---------------------------------------------------------------- runs

#[test]
fn run_writes_rows_manifest_notice_and_exact_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let raw = page();
    let (code, state, _) = run_pages(vec![served(DOCID, &raw)], &[DOCID], &out);
    assert_eq!(code, 0);
    let rows = rows(&out);
    assert_eq!(
        rows.iter()
            .map(|r| r["paragraph"].clone())
            .collect::<Vec<_>>(),
        [json!("1"), json!("2"), json!("3")]
    );
    let digest: String = Sha256::digest(raw.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(rows.iter().all(|r| r["source_sha256"] == digest.as_str()));
    let manifest = manifest(&out);
    let document = &manifest["documents"][0];
    assert_eq!(document["family"], "Taxation Ruling");
    assert_eq!(document["withdrawal_status"], "UNKNOWN");
    assert_eq!(
        std::fs::read(out.join(document["raw_file"].as_str().unwrap())).unwrap(),
        raw.as_bytes()
    );
    assert!(
        std::fs::read_to_string(out.join("LICENCE-NOTICE.md"))
            .unwrap()
            .contains(NOTICE)
    );
    let printed = String::from_utf8(state.stdout).unwrap();
    assert!(
        printed.starts_with("rulings: 1 documents, 3 rows, 0 excluded -> "),
        "{printed}"
    );
}

#[test]
fn run_binds_the_notice_to_every_row_and_the_capture_date() {
    let raw = page().replace(
        r#"<div id="LawBody">"#,
        r#"<div id="LawBody"><h3>Notice of Withdrawal</h3><p><a name="PA"></a>Withdrawal applies only after the stated date.</p>"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("withdrawal");
    assert_eq!(run_pages(vec![served(DOCID, &raw)], &[DOCID], &out).0, 0);
    let document = manifest(&out)["documents"][0].clone();
    for row in rows(&out) {
        assert_eq!(row["withdrawal_status"], "NOTICE_PRESENT");
        assert_eq!(row["withdrawal_notice"], document["withdrawal_notice"]);
        assert_eq!(row["fetched_on"], document["fetched_on"]);
    }
    assert_eq!(document["fetched_on"], "2099-02-01");
}

#[test]
fn an_existing_destination_is_never_reused() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut http = Pages(vec![served(DOCID, &page())], Vec::new());
    let mut state = Run::default();
    let error = rulings::run(
        &mut http,
        &[DOCID.into()],
        &PyPath::new(&out.to_string_lossy()),
        &mut state,
    )
    .unwrap()
    .unwrap_err();
    assert!(error.0.contains("already exists"));
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
fn personal_data_and_failures_are_excluded_and_fail_the_run() {
    let raw = page_with(Page {
        body_extra: r#"<p><a name="P4"></a>4. Call 02 6216 1111 for help.</p>"#.into(),
        ..Page::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let (code, state, _) = run_pages(
        vec![
            served(DOCID, &raw),
            (EV_ID.into(), Err(Failure::Status(404))),
        ],
        &[DOCID, EV_ID],
        &out,
    );
    assert_eq!(code, 1);
    let manifest = manifest(&out);
    assert_eq!(manifest["documents"], json!([]));
    let excluded: Vec<&Value> = manifest["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["docid"])
        .collect();
    assert_eq!(excluded, [&json!(DOCID), &json!(EV_ID)]);
    assert!(
        manifest["excluded"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("personal data patterns: phone")
    );
    let stderr = String::from_utf8(state.stderr).unwrap();
    assert!(!(stderr + &manifest.to_string()).contains("6216"));
    assert_eq!(
        std::fs::read_to_string(out.join("rulings.jsonl")).unwrap(),
        ""
    );
}

#[test]
fn a_dropped_response_is_excluded_without_aborting_the_run() {
    let other = "TXR/TR20992/NAT/ATO/00001";
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let raw = page_with(Page {
        docid: Some(other.into()),
        ..Page::default()
    });
    let (code, _, _) = run_pages(
        vec![
            (
                DOCID.into(),
                Err(Failure::Transport("IncompleteRead".into())),
            ),
            served(other, &raw),
        ],
        &[DOCID, other],
        &out,
    );
    assert_eq!(code, 1);
    let manifest = manifest(&out);
    assert_eq!(manifest["documents"][0]["docid"], other);
    assert!(
        manifest["excluded"][0]["reason"]
            .as_str()
            .unwrap()
            .ends_with("fetch failed (IncompleteRead)")
    );
}

#[test]
fn requests_after_the_first_are_spaced() {
    let other = "TXR/TR20991/NAT/ATO/00002";
    let dir = tempfile::tempdir().unwrap();
    let raw = page_with(Page {
        docid: Some(other.into()),
        ..Page::default()
    });
    let (code, _, sleeps) = run_pages(
        vec![served(DOCID, &page()), served(other, &raw)],
        &[DOCID, other],
        &dir.path().join("out"),
    );
    assert_eq!(code, 0);
    assert_eq!(sleeps, ["sleep 1"]);
}

#[test]
fn two_targets_naming_one_document_are_refused_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let second = format!("{DOCID}/");
    let mut http = Pages(
        vec![served(DOCID, &page()), served(&second, &page())],
        Vec::new(),
    );
    let mut state = Run::default();
    let error = rulings::run(
        &mut http,
        &[DOCID.into(), second],
        &PyPath::new(&out.to_string_lossy()),
        &mut state,
    )
    .unwrap()
    .unwrap_err();
    assert!(error.0.contains("collides with an earlier target"));
    assert!(!out.exists());
    let names: std::collections::BTreeSet<String> = ["TXR/A-B", "TXR/A.B", "TXR/A_B"]
        .iter()
        .map(|d| rulings::safe_name(d))
        .collect();
    assert_eq!(names.len(), 3);
}

// ---------------------------------------------------------------- fetching

#[test]
fn off_site_redirects_non_html_and_oversize_are_refused() {
    let url = format!("{BASE_URL}{DOCID}");
    let huge = vec![b'x'; MAX_BYTES + 1];
    for (response, message) in [
        (
            Script::ok("https://example.invalid/x", "text/html", b"<html></html>"),
            "redirected off",
        ),
        (
            Script::ok(
                &format!("http://www.ato.gov.au/law/view/document?docid={DOCID}"),
                "text/html",
                b"",
            ),
            "redirected off",
        ),
        (
            Script::ok(
                &format!("https://www.ato.gov.au:8443/law/view/document?docid={DOCID}"),
                "text/html",
                b"",
            ),
            "redirected off",
        ),
        (
            Script::ok("https://www.ato.gov.au:notaport/law", "text/html", b""),
            "malformed final URL",
        ),
        (
            Script::ok(&url, "application/pdf", b"<html></html>"),
            "content type",
        ),
        (Script::ok(&url, "text/html", &huge), "larger than"),
    ] {
        let mut http = Script {
            responses: vec![response],
            ..Script::default()
        };
        let error = rulings::fetch(&mut http, DOCID).unwrap().unwrap_err().0;
        assert!(error.contains(message), "{error:?} lacks {message:?}");
    }
}

#[test]
fn client_errors_stop_and_server_errors_retry() {
    let mut http = Script {
        responses: vec![Err(Failure::Status(404))],
        ..Script::default()
    };
    assert!(
        rulings::fetch(&mut http, DOCID)
            .unwrap()
            .unwrap_err()
            .0
            .contains("HTTP 404")
    );
    assert_eq!(http.events.len(), 1);

    let url = format!("{BASE_URL}{DOCID}");
    let mut http = Script {
        responses: vec![
            Err(Failure::Status(503)),
            Script::ok(&url, "text/html; charset=utf-8", page().as_bytes()),
        ],
        ..Script::default()
    };
    let (raw, final_url) = rulings::fetch(&mut http, DOCID).unwrap().unwrap();
    assert_eq!((raw, final_url), (page().into_bytes(), url.clone()));
    assert_eq!(
        http.events,
        [
            format!("GET {url}"),
            format!("sleep {RETRY_DELAY}"),
            format!("GET {url}")
        ]
    );

    let mut http = Script {
        responses: vec![Err(Failure::Status(503)), Err(Failure::Status(404))],
        ..Script::default()
    };
    assert!(
        rulings::fetch(&mut http, DOCID)
            .unwrap()
            .unwrap_err()
            .0
            .contains("HTTP 404")
    );

    let mut http = Script::default();
    let error = rulings::fetch(&mut http, DOCID).unwrap().unwrap_err().0;
    assert!(error.ends_with("fetch failed (URLError)"), "{error}");
    assert_eq!(
        http.events.iter().filter(|e| e.starts_with("GET")).count(),
        3
    );
}

// ---------------------------------------------------------------- command line

#[test]
fn main_reports_an_invalid_target_list() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("targets.json"),
        json!({"docid": DOCID}).to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .args(["rulings", "targets.json", "--out", "out"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        "rulings: targets must be a JSON array of docid strings\n"
    );
    assert!(!dir.path().join("out").exists());
}
