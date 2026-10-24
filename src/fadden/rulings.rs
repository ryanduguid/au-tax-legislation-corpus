//! `fadden/rulings.py`: paragraph JSONL from named ATO Legal Database documents.
//!
//! Reads ATO-authored documents (rulings, determinations, practical compliance
//! guidelines, decision impact statements, edited versions of private advice)
//! from https://www.ato.gov.au/law and writes one JSONL row per numbered
//! paragraph. A finding aid: not the authorised text, not advice, and not a
//! statement of the Commissioner's current view.
//!
//! Reuse rests on the ATO's copyright notice, which each page must carry or
//! point to through a dc.Rights link; a page with neither is refused. Only
//! ATO-authored families are accepted, from an explicit target list capped at
//! MAX_TARGETS. The output directory must not exist: every run writes a new one
//! with the exact bytes each page returned beside their SHA-256. A document
//! whose text trips the corpus PII patterns is excluded and the run exits 1.
use sha2::{Digest, Sha256};

use super::http::{self, Failure, Http};
use super::{Exc, Run, json_error, pii};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::htmlparser::{self, Event};
use crate::py;
use crate::pyjson::{self, Json, PyStr};
use crate::pypath::PyPath;
use crate::pyre;
use crate::pyurl;
use crate::pyvalue::{dumps_sorted, repr_points};

pub const BASE_URL: &str = "https://www.ato.gov.au/law/view/document?docid=";
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TARGETS: usize = 100;
pub const REQUEST_SPACING: f64 = 1.0;
pub const NOTICE: &str = "You are free to copy, adapt, modify, transmit and distribute this material as you wish \
(but not in any way that suggests the ATO or the Commonwealth endorses you or any of your \
services or products).";
pub const COPYRIGHT_NOTICE_URL: &str =
    "https://www.ato.gov.au/about-ato/using-our-website/copyright-notice";
const RIGHTS_HOSTS: [&str; 2] = ["www.ato.gov.au", "ato.gov.au"];
const RIGHTS_PATH: &str = "/content/corporate/about_this_site.htm";
const RIGHTS_FRAGMENT: &str = "copyright";

/// docid prefix and family: ATO-authored families only.
const FAMILIES: [(&str, &str); 9] = [
    ("TXR", "Taxation Ruling"),
    ("TXD", "Taxation Determination"),
    ("GST", "GST ruling or determination"),
    ("CLR", "Class Ruling"),
    ("PRR", "Product Ruling"),
    ("COG", "Practical Compliance Guideline"),
    ("PSR", "Law Administration Practice Statement"),
    ("LIT/ICD", "Decision Impact Statement"),
    ("EV", "Edited version of private advice"),
];

const DOCID: &str = r"^[A-Za-z]{2,4}(?:/[A-Za-z0-9.-]+){1,5}$";
const PARA: &str = r"^P(\d+[A-Z]?|[A-Z])$";
const HEAD: &str = r"^H\d+[A-Z]?$";
/// Some families (decision impact statements) print "1. " without a P anchor.
const PRINTED: &str = r"^(\d+[A-Z]?)\.\s";
const BLOCKS: [&str; 13] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "td",
    "th",
    "dd",
    "dt",
    "blockquote",
];

/// A target that cannot be fetched, verified or extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct RulingsError(pub String);

impl std::fmt::Display for RulingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn refused(message: impl Into<String>) -> RulingsError {
    RulingsError(message.into())
}

/// `family_of(docid)`: the ATO document family, or None when not ATO-authored.
pub fn family_of(docid: &str) -> Option<&'static str> {
    let upper = docid.to_uppercase();
    FAMILIES
        .iter()
        .find(|(prefix, _)| upper == *prefix || upper.starts_with(&format!("{prefix}/")))
        .map(|(_, family)| *family)
}

/// `_norm(text)`: whitespace runs as one space, stripped.
fn norm(text: &str) -> String {
    py::strip(&pyre::re(r"\s+", 0).sub(" ", text)).to_string()
}

/// `_canonical(docid)`: case-insensitive and without a trailing slash.
fn canonical(points: &[u32]) -> Vec<u32> {
    let stripped = strip_points(points);
    let end = stripped
        .iter()
        .rposition(|&cp| cp != u32::from('/'))
        .map_or(0, |i| i + 1);
    let mut out = Vec::new();
    for &cp in &stripped[..end] {
        match char::from_u32(cp) {
            Some(c) => out.extend(c.to_uppercase().map(u32::from)),
            None => out.push(cp),
        }
    }
    out
}

fn is_space(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(py::is_space)
}

fn strip_points(points: &[u32]) -> &[u32] {
    let start = points
        .iter()
        .position(|&cp| !is_space(cp))
        .unwrap_or(points.len());
    let end = points
        .iter()
        .rposition(|&cp| !is_space(cp))
        .map_or(start, |i| i + 1);
    &points[start..end.max(start)]
}

fn points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

/// The meta tags and paragraph blocks of a Legal Database page, collected as
/// `_LawParser` collects them from `html.parser.HTMLParser` events.
#[derive(Default)]
struct LawParser {
    meta: Vec<(String, String)>,
    blocks: Vec<(Vec<String>, String, bool)>,
    page_text: Vec<String>,
    document_text: Vec<String>,
    edited_version: bool,
    law: i64,
    div_stack: Vec<&'static str>,
    in_body: i64,
    skip: i64,
    sup: i64,
    bold: i64,
    stop: bool,
    ev: u8,
    block: Option<Vec<String>>,
    plain: bool,
    anchors: Vec<String>,
    depth: i64,
}

impl LawParser {
    fn collecting(&self) -> bool {
        self.ev == 2 || (self.in_body != 0 && self.skip == 0 && !self.stop)
    }

    fn start(&mut self, tag: &str, attrs: &[(String, Option<String>)]) {
        // `{k: (v or "") for k, v in attrs}`: a repeated attribute's last value wins.
        let get = |name: &str| -> Option<String> {
            attrs
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone().unwrap_or_default())
        };
        if tag == "meta"
            && let Some(name) = get("name").filter(|n| !n.is_empty())
            && !self.meta.iter().any(|(k, _)| *k == name)
        {
            self.meta.push((name, get("content").unwrap_or_default()));
        }
        if tag == "div" {
            let id = get("id").unwrap_or_default();
            let class = get("class").unwrap_or_default();
            let classes: Vec<&str> = py::split_whitespace(&class);
            let role = if id == "LawBody" {
                self.in_body += 1;
                self.law += 1;
                "body"
            } else if id.starts_with("Law") {
                self.law += 1;
                "law"
            } else if id.to_lowercase().starts_with("ev_disclaimer") && self.ev == 0 {
                self.ev = 1;
                self.edited_version = true;
                "evdisc"
            } else if self.ev == 2 && classes.contains(&"container-fluid") {
                self.ev = 3;
                ""
            } else if self.in_body != 0 && (id == "footnotes" || classes.contains(&"panel")) {
                self.skip += 1;
                "skip"
            } else {
                ""
            };
            self.div_stack.push(role);
        }
        if tag == "sup" {
            self.sup += 1;
        }
        if tag == "strong" || tag == "b" {
            self.bold += 1;
        }
        if tag == "a"
            && let Some(name) = get("name").filter(|n| !n.is_empty())
        {
            let name = name.trim_start_matches('#').to_string();
            if name.to_uppercase() == "COPYRIGHT" && self.in_body != 0 {
                self.stop = true;
            }
            if self.block.is_some() && !self.anchors.contains(&name) {
                self.anchors.push(name);
            }
        }
        if BLOCKS.contains(&tag) && self.collecting() {
            if self.block.is_none() {
                self.block = Some(Vec::new());
                self.anchors = Vec::new();
                self.depth = 0;
                self.plain = false;
            }
            self.depth += 1;
        }
    }

    fn end(&mut self, tag: &str) {
        if tag == "div"
            && let Some(role) = self.div_stack.pop()
        {
            match role {
                "body" => {
                    self.in_body -= 1;
                    self.law -= 1;
                }
                "law" => self.law -= 1,
                "skip" => self.skip -= 1,
                "evdisc" if self.ev == 1 => self.ev = 2,
                _ => {}
            }
        }
        if tag == "article" && self.ev == 2 {
            self.ev = 3;
        }
        if tag == "sup" && self.sup != 0 {
            self.sup -= 1;
        }
        if (tag == "strong" || tag == "b") && self.bold != 0 {
            self.bold -= 1;
        }
        if BLOCKS.contains(&tag) && self.block.is_some() {
            self.depth -= 1;
            if self.depth <= 0 {
                let text = norm(&self.block.take().unwrap_or_default().concat());
                let anchors = std::mem::take(&mut self.anchors);
                if !text.is_empty() || !anchors.is_empty() {
                    let bold = !text.is_empty() && !self.plain;
                    self.blocks.push((anchors, text, bold));
                }
            }
        }
    }

    fn data(&mut self, data: &str) {
        self.page_text.push(data.to_string());
        if self.law != 0 || matches!(self.ev, 1 | 2) {
            self.document_text.push(data.to_string());
        }
        if self.block.is_some() && self.sup == 0 && self.collecting() {
            if let Some(block) = &mut self.block {
                block.push(data.to_string());
            }
            if !py::strip(data).is_empty() && self.bold == 0 {
                self.plain = true;
            }
        }
    }

    fn feed(page: &str) -> LawParser {
        let mut parser = LawParser::default();
        for positioned in htmlparser::parse(page, true) {
            match positioned.event {
                Event::StartTag(tag, attrs) => parser.start(&tag, &attrs),
                Event::StartEndTag(tag, attrs) => {
                    parser.start(&tag, &attrs);
                    parser.end(&tag);
                }
                Event::EndTag(tag) => parser.end(&tag),
                Event::Data(text) => parser.data(&text),
                _ => {}
            }
        }
        parser
    }
}

/// `_is_ato_rights_link(value)`: only the ATO copyright notice address.
fn is_ato_rights_link(value: &str) -> bool {
    let Ok(parts) = pyurl::urlsplit(py::strip(value)) else {
        return false;
    };
    let Ok(port) = pyurl::port(&parts.netloc) else {
        return false;
    };
    matches!(parts.scheme.as_str(), "http" | "https")
        && RIGHTS_HOSTS.contains(&pyurl::hostname(&parts.netloc).unwrap_or_default().as_str())
        && port.is_none()
        && parts.path == RIGHTS_PATH
        && parts.fragment == RIGHTS_FRAGMENT
        && parts.query.is_empty()
}

/// One numbered paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    pub paragraph: String,
    pub heading: String,
    pub text: String,
    pub numbering: &'static str,
}

/// `parse_document(raw, docid)`'s result.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub reference: String,
    pub title: String,
    pub document_type: String,
    pub issued: String,
    pub licence_basis: &'static str,
    pub withdrawal_status: &'static str,
    pub withdrawal_notice: String,
    pub paragraphs: Vec<Paragraph>,
    pub document_text: String,
}

/// `parse_document(raw, docid)`: verify one fetched page and read its paragraphs.
pub fn parse_document(raw: &[u8], docid: &str) -> Result<Parsed, RulingsError> {
    let page =
        std::str::from_utf8(raw).map_err(|_| refused(format!("{docid}: response is not UTF-8")))?;
    let parser = LawParser::feed(page);
    // `{k.lower(): v for k, v in parser.meta.items()}`: a later key wins.
    let mut meta: Vec<(String, String)> = Vec::new();
    for (k, v) in &parser.meta {
        let key = k.to_lowercase();
        match meta.iter_mut().find(|(m, _)| *m == key) {
            Some(slot) => slot.1 = v.clone(),
            None => meta.push((key, v.clone())),
        }
    }
    let field = |name: &str| -> String {
        meta.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let either = |a: &str, b: &str| -> String {
        let first = field(a);
        if first.is_empty() { field(b) } else { first }
    };

    let reference = field("ato.reference.id");
    if reference.is_empty() || canonical(&points(&reference)) != canonical(&points(docid)) {
        return Err(refused(format!(
            "{docid}: page reference id {} does not match the target",
            py::repr_str(&reference)
        )));
    }
    let text = norm(&crate::html::unescape(&parser.page_text.concat()));
    let rights = either("dc.rights", "dc_rights");
    let basis = if text.contains(NOTICE) {
        "document-notice"
    } else if is_ato_rights_link(&rights) {
        "site-notice-via-dc-rights"
    } else {
        return Err(refused(format!(
            "{docid}: no ATO reuse notice and no dc.Rights link to it"
        )));
    };

    let para = pyre::re(PARA, 0);
    let head = pyre::re(HEAD, 0);
    let printed = pyre::re(PRINTED, 0);
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut heading = String::new();
    if parser.edited_version {
        // No paragraph numbers in the source: number the non-heading blocks in
        // order and say so in the numbering field.
        for (_, block_text, bold) in &parser.blocks {
            if *bold {
                heading = block_text.clone();
            } else if !block_text.is_empty() {
                paragraphs.push(Paragraph {
                    paragraph: (paragraphs.len() + 1).to_string(),
                    heading: heading.clone(),
                    text: block_text.clone(),
                    numbering: "sequential",
                });
            }
        }
        if paragraphs.is_empty() {
            return Err(refused(format!(
                "{docid}: no paragraphs found after the edited-version disclaimer"
            )));
        }
    }
    let anchored = parser.blocks.iter().any(|(anchors, _, _)| {
        anchors.iter().any(|name| {
            para.search(name).is_some() && name.chars().nth(1).is_some_and(py::is_decimal)
        })
    });
    let blocks = if parser.edited_version {
        &[][..]
    } else {
        &parser.blocks[..]
    };
    let mut current: Option<usize> = None;
    for (anchors, block_text, _) in blocks {
        let mut number = anchors.iter().find_map(|name| {
            para.search(name)
                .and_then(|m| m.get(1))
                .map(|g| g.as_str().to_string())
        });
        let has_head = anchors.iter().any(|name| head.search(name).is_some());
        if number.is_none() && !anchored && !has_head {
            number = printed
                .search(block_text)
                .and_then(|m| m.get(1))
                .map(|g| g.as_str().to_string());
        }
        if number.is_none() && (has_head || py::casefold(block_text) == "notice of withdrawal") {
            heading = block_text.clone();
            current = None;
            continue;
        }
        if let Some(number) = number {
            paragraphs.push(Paragraph {
                paragraph: number,
                heading: heading.clone(),
                text: block_text.clone(),
                numbering: "document",
            });
            current = Some(paragraphs.len() - 1);
        } else if let Some(i) = current
            && !block_text.is_empty()
        {
            paragraphs[i].text = format!("{} {block_text}", paragraphs[i].text);
        }
    }
    if paragraphs.is_empty() {
        return Err(refused(format!(
            "{docid}: no numbered paragraphs found in LawBody"
        )));
    }

    let notice: Vec<&str> = paragraphs
        .iter()
        .filter(|p| py::casefold(&p.heading) == "notice of withdrawal")
        .map(|p| p.text.as_str())
        .collect();
    let withdrawal_notice = py::strip(&notice.join(" ")).to_string();

    Ok(Parsed {
        reference,
        title: either("dc.title", "dc_title"),
        document_type: either("dc.type.documenttype", "dc.description"),
        issued: either("dc.date.issued", "dc_date_issued"),
        licence_basis: basis,
        withdrawal_status: if withdrawal_notice.is_empty() {
            "UNKNOWN"
        } else {
            "NOTICE_PRESENT"
        },
        withdrawal_notice,
        paragraphs,
        document_text: norm(&parser.document_text.join(" ")),
    })
}

/// `pii_findings(texts)`: the kinds of personal data found, never the values.
pub fn pii_findings(texts: &[&str]) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for text in texts {
        if pii::has_private_person_registration_pair(text) {
            kinds.push("name-with-registration-number".into());
        }
        kinds.extend(
            pii::contact_fingerprints(text)
                .into_iter()
                .map(|(kind, _)| kind.to_string()),
        );
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

/// `scanned_texts(parsed)`: every text that reaches the output or sits in the
/// retained document region.
pub fn scanned_texts(parsed: &Parsed) -> Vec<&str> {
    let mut texts = vec![
        parsed.title.as_str(),
        parsed.document_type.as_str(),
        parsed.issued.as_str(),
        parsed.document_text.as_str(),
    ];
    for row in &parsed.paragraphs {
        texts.push(&row.heading);
        texts.push(&row.text);
    }
    texts
}

/// `fetch(docid)`: the page bytes and final URL, refusing oversize or
/// off-site responses. A rate limit or server fault is retried; a
/// `ValueError` urllib raises stops the stage.
pub fn fetch(
    http: &mut dyn Http,
    docid: &str,
) -> Result<Result<(Vec<u8>, String), RulingsError>, Exc> {
    // Validated docids hold only characters `quote(docid, safe="/")` keeps.
    let url = format!("{BASE_URL}{docid}");
    let mut failure = "no attempt made".to_string();
    let mut response = None;
    let mut request = http::Request::default();
    for number in 1..=http::TRIES {
        http::attempt(http, number);
        match http.get(&url, Some(MAX_BYTES + 1), &mut request) {
            Ok(r) => {
                response = Some(r);
                break;
            }
            Err(Failure::Status(code)) => {
                failure = format!("HTTP {code}");
                if code != 429 && code < 500 {
                    return Ok(Err(refused(format!("{docid}: fetch failed ({failure})"))));
                }
            }
            Err(Failure::Transport(name)) => failure = name,
            Err(Failure::Uncaught(message)) => return Err(Exc::value(message)),
        }
    }
    let Some(response) = response else {
        return Ok(Err(refused(format!("{docid}: fetch failed ({failure})"))));
    };
    Ok(verify(docid, response))
}

/// The fetched page for `docid`, or why it is refused.
fn verify(docid: &str, response: http::Response) -> Result<(Vec<u8>, String), RulingsError> {
    let parts = pyurl::urlsplit(&response.url)
        .map_err(|_| refused(format!("{docid}: malformed final URL")))?;
    let port =
        pyurl::port(&parts.netloc).map_err(|_| refused(format!("{docid}: malformed final URL")))?;
    let host = pyurl::hostname(&parts.netloc).unwrap_or_default();
    if parts.scheme != "https" || host != "www.ato.gov.au" || !matches!(port, None | Some(443)) {
        return Err(refused(format!(
            "{docid}: redirected off https://www.ato.gov.au to {}://{}",
            parts.scheme, parts.netloc
        )));
    }
    if !response.content_type.starts_with("text/html") {
        return Err(refused(format!(
            "{docid}: unexpected content type {}",
            py::repr_str(&response.content_type)
        )));
    }
    if response.body.len() > MAX_BYTES {
        return Err(refused(format!(
            "{docid}: response larger than {MAX_BYTES} bytes"
        )));
    }
    Ok((response.body, response.url))
}

/// Why the target list was refused: the stage's own error, or an exception
/// reading it, which the Python stage did not catch.
pub enum TargetsError {
    Refused(RulingsError),
    Uncaught(Exc),
}

/// `load_targets(path)`: a JSON array of distinct ATO-authored docids.
pub fn load_targets(path: &str) -> Result<Vec<String>, TargetsError> {
    let text = crate::pyio::read_text(path).map_err(|e| TargetsError::Uncaught(e.into()))?;
    let data = pyjson::loads_standard(&text).map_err(|e| TargetsError::Uncaught(json_error(e)))?;
    let refuse = |m: String| TargetsError::Refused(refused(m));
    let items: Vec<Vec<u32>> = match &data {
        Json::Array(items) if items.iter().all(|i| matches!(i, Json::Str(_))) => items
            .iter()
            .map(|i| match i {
                Json::Str(s) => strip_points(&s.code_points()).to_vec(),
                _ => unreachable!("every item is a str"),
            })
            .collect(),
        _ => {
            return Err(refuse(
                "targets must be a JSON array of docid strings".into(),
            ));
        }
    };
    let mut distinct: Vec<Vec<u32>> = items.iter().map(|t| canonical(t)).collect();
    distinct.sort();
    distinct.dedup();
    if distinct.len() != items.len() {
        return Err(refuse("targets contain a duplicate docid".into()));
    }
    if items.len() > MAX_TARGETS {
        return Err(refuse(format!(
            "{} targets exceed the cap of {MAX_TARGETS}",
            items.len()
        )));
    }
    let docid_re = pyre::re(DOCID, 0);
    let mut targets = Vec::new();
    for item in &items {
        let docid = PyStr::from_code_points(item.clone());
        let valid = docid.as_str().filter(|d| docid_re.search(d).is_some());
        let Some(docid) = valid else {
            return Err(refuse(format!(
                "{} is not a Legal Database docid",
                repr_points(item)
            )));
        };
        if family_of(docid).is_none() {
            return Err(refuse(format!(
                "{docid}: not an ATO-authored document family"
            )));
        }
        targets.push(docid.to_string());
    }
    Ok(targets)
}

/// `_safe_name(docid)`: a readable stem plus a digest of the canonical docid.
pub fn safe_name(docid: &str) -> String {
    let readable = pyre::re("[^A-Za-z0-9]+", 0).sub("_", docid);
    let digest = Sha256::digest(
        canonical(&points(docid))
            .into_iter()
            .filter_map(char::from_u32)
            .collect::<String>()
            .as_bytes(),
    );
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}", readable.trim_matches('_'), &hex[..16])
}

fn s(text: &str) -> Json {
    Json::str(text)
}

fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (PyStr::Text(k.into()), v))
            .collect(),
    )
}

fn int(n: usize) -> Json {
    Json::Int(n.into())
}

fn write_new(path: &PyPath, bytes: &[u8]) -> Result<(), Exc> {
    use std::io::Write;
    let mut file = crate::pyos::create_new(&path.display(), false)?;
    file.write_all(bytes)
        .map_err(|e| crate::pyos::OsError::new(&e, None, crate::pyos::Style::Errno))?;
    Ok(())
}

fn text_bytes(points: &[u32]) -> Vec<u8> {
    points
        .iter()
        .filter_map(|&cp| char::from_u32(cp))
        .collect::<String>()
        .into_bytes()
}

/// `run(targets, out)`: fetch, verify and extract each target into the new
/// directory `out`; 1 when any target was excluded.
pub fn run(
    http: &mut dyn Http,
    targets: &[String],
    out: &PyPath,
    run: &mut Run,
) -> Result<Result<i32, RulingsError>, Exc> {
    if std::fs::metadata(out.fs()).is_ok() {
        return Ok(Err(refused(format!(
            "{out} already exists; every run writes a new directory"
        ))));
    }
    let fetched_on = http.today();
    let mut documents = Vec::new();
    let mut excluded: Vec<(String, String)> = Vec::new();
    let mut rows: Vec<Vec<u32>> = Vec::new();
    let mut raw_files: Vec<(String, Vec<u8>)> = Vec::new();
    for (index, docid) in targets.iter().enumerate() {
        if index > 0 && REQUEST_SPACING != 0.0 {
            http.sleep(REQUEST_SPACING);
        }
        let fetched = fetch(http, docid)?
            .and_then(|(raw, final_url)| parse_document(&raw, docid).map(|p| (raw, final_url, p)));
        let (raw, final_url, parsed) = match fetched {
            Ok(found) => found,
            Err(error) => {
                excluded.push((docid.clone(), error.0));
                continue;
            }
        };
        let kinds = pii_findings(&scanned_texts(&parsed));
        if !kinds.is_empty() {
            excluded.push((
                docid.clone(),
                format!("personal data patterns: {}", kinds.join(", ")),
            ));
            continue;
        }
        let digest: String = Sha256::digest(&raw)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let raw_name = format!("raw/{}.html", safe_name(docid));
        if raw_files.iter().any(|(name, _)| *name == raw_name) {
            return Ok(Err(refused(format!(
                "{docid}: raw file name {raw_name} collides with an earlier target"
            ))));
        }
        let family = family_of(docid).map_or(Json::Null, s);
        documents.push(object(vec![
            ("docid", s(docid)),
            ("reference", s(&parsed.reference)),
            ("family", family.clone()),
            ("title", s(&parsed.title)),
            ("document_type", s(&parsed.document_type)),
            ("issued", s(&parsed.issued)),
            ("source_url", s(&final_url)),
            ("fetched_on", s(&fetched_on)),
            ("sha256", s(&digest)),
            ("bytes", int(raw.len())),
            ("raw_file", s(&raw_name)),
            ("licence_basis", s(parsed.licence_basis)),
            ("withdrawal_status", s(parsed.withdrawal_status)),
            ("withdrawal_notice", s(&parsed.withdrawal_notice)),
            ("paragraphs", int(parsed.paragraphs.len())),
        ]));
        raw_files.push((raw_name, raw));
        for row in &parsed.paragraphs {
            rows.push(dumps_sorted(
                &object(vec![
                    ("docid", s(docid)),
                    ("family", family.clone()),
                    ("title", s(&parsed.title)),
                    ("paragraph", s(&row.paragraph)),
                    ("numbering", s(row.numbering)),
                    ("heading", s(&row.heading)),
                    ("text", s(&row.text)),
                    ("source_url", s(&final_url)),
                    ("source_sha256", s(&digest)),
                    ("fetched_on", s(&fetched_on)),
                    ("licence_basis", s(parsed.licence_basis)),
                    ("withdrawal_status", s(parsed.withdrawal_status)),
                    ("withdrawal_notice", s(&parsed.withdrawal_notice)),
                ]),
                None,
            ));
        }
    }

    // `out.mkdir(parents=True)`: only a missing parent is created; any other
    // failure, such as a parent that is a file on POSIX, is the output's own.
    let mkdir_out = || {
        std::fs::create_dir(out.fs()).map_err(|e| {
            crate::pyos::OsError::new(&e, Some(&out.display()), crate::pyos::Style::Native)
        })
    };
    if let Err(error) = mkdir_out() {
        if error.kind != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
        crate::pyos::makedirs(&out.parent())?;
        mkdir_out()?;
    }
    let raw_dir = out.join("raw");
    std::fs::create_dir(raw_dir.fs()).map_err(|e| {
        crate::pyos::OsError::new(&e, Some(&raw_dir.display()), crate::pyos::Style::Native)
    })?;
    for (name, raw) in &raw_files {
        write_new(&out.join(name), raw)?;
    }
    let mut jsonl = Vec::new();
    for row in &rows {
        jsonl.extend(text_bytes(row));
        jsonl.push(b'\n');
    }
    write_new(&out.join("rulings.jsonl"), &jsonl)?;
    let manifest = object(vec![
        ("stage", s("rulings")),
        ("fetched_on", s(&fetched_on)),
        (
            "targets",
            Json::Array(targets.iter().map(|t| s(t)).collect()),
        ),
        ("documents", Json::Array(documents.clone())),
        (
            "excluded",
            Json::Array(
                excluded
                    .iter()
                    .map(|(d, r)| object(vec![("docid", s(d)), ("reason", s(r))]))
                    .collect(),
            ),
        ),
        ("rows", int(rows.len())),
        ("reuse_notice", s(NOTICE)),
        ("reuse_notice_url", s(COPYRIGHT_NOTICE_URL)),
    ]);
    let mut manifest_bytes = text_bytes(&dumps_sorted(&manifest, Some(2)));
    manifest_bytes.push(b'\n');
    write_new(&out.join("manifest.json"), &manifest_bytes)?;
    let licence = format!(
        "# ATO Legal Database material\n\n\
         The rows in rulings.jsonl are derived from documents published by the Australian \
         Taxation Office on its Legal Database. They are reproduced under the ATO copyright \
         notice ({COPYRIGHT_NOTICE_URL}):\n\n\
         > {NOTICE}\n\n\
         Copyright in the source documents remains with the Australian Taxation Office for \
         the Commonwealth of Australia. Neither the ATO nor the Commonwealth endorses this \
         corpus. The rows are not the authorised text; read the source document at its \
         source_url before relying on it. An edited version of private advice cannot be \
         relied on by anyone.\n"
    );
    write_new(&out.join("LICENCE-NOTICE.md"), licence.as_bytes())?;
    run.print(&format!(
        "rulings: {} documents, {} rows, {} excluded -> {out}",
        documents.len(),
        rows.len(),
        excluded.len()
    ));
    for (docid, reason) in &excluded {
        run.eprint(&format!("  excluded {docid}: {reason}"));
    }
    Ok(Ok(i32::from(!excluded.is_empty())))
}

fn parser() -> Parser {
    Parser::new("python -m fadden rulings")
        .description("Derive paragraph JSONL from named ATO Legal Database documents.")
        .arg(
            Action::positional("targets")
                .help(&format!("JSON array of docids (at most {MAX_TARGETS})")),
        )
        .arg(
            Action::option("--out")
                .required()
                .help("new output directory; must not exist"),
        )
}

pub fn main(argv: &[String], run_state: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    // argparse's `type=Path`: each path as pathlib spells it.
    let targets_path = PyPath::new(&args.str("targets").unwrap_or_default()).display();
    let out = PyPath::new(&args.str("out").unwrap_or_default());
    let outcome = match load_targets(&targets_path) {
        Err(TargetsError::Uncaught(error)) => Err(error),
        Err(TargetsError::Refused(error)) => Ok(Err(error)),
        Ok(targets) => {
            let mut http = http::connect();
            run(http.as_mut(), &targets, &out, run_state)
        }
    };
    Ok(match outcome {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => {
            run_state.eprint(&format!("rulings: {error}"));
            1
        }
        Err(error) => run_state.uncaught(&error),
    })
}
