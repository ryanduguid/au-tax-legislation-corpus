// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden/search.py`: build and query a local search index over a finished corpus.
//!
//! `build` checks every row of a finished corpus against `sources.json` before
//! anything is written, then writes one SQLite file: a full-text table (FTS5),
//! the SHA-256 of every `sections.jsonl` read, the defined terms found in
//! definitions rows and the section references found in row text. `query`
//! ranks rows by BM25, `define` looks a term up among definitions paragraphs
//! and `refs` lists a row's citations both ways. All of it is a finding aid
//! over the EPUB reading view, not the authorised text.
//!
//! The index is built with the transaction pattern Python's `sqlite3` used:
//! the schema script in autocommit mode, then every insert in one transaction.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags, params_from_iter};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::as_at;
use super::corpus_paths::{child, register_id, register_id_str};
use super::quotes::{self, Quotation};
use super::{Exc, Run};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, Json, PyStr};
use crate::pyos::{OsError, Style};
use crate::pypath::PyPath;
use crate::pyre::{self, S as DOTALL};
use crate::pyvalue::{self, dumps_line, py_eq, repr, str_for_stderr, type_name};

const SCHEMA: &str = "4";
pub const STEM_NOTICE: &str = "Word-form matches are not quotation matches.";

#[derive(Clone, Copy)]
enum SearchMode {
    Exact,
    Stemmed,
}

impl SearchMode {
    /// Both identifiers are fixed here; query words remain bound parameters.
    fn sql(self, exact: &'static str) -> Cow<'static, str> {
        match self {
            Self::Exact => Cow::Borrowed(exact),
            Self::Stemmed => Cow::Owned(exact.replace("rows_fts", "rows_stem")),
        }
    }

    fn mark(self, hit: Json) -> Json {
        match self {
            Self::Exact => hit,
            Self::Stemmed => with_field(
                with_field(hit, "stemmed", Json::Bool(true)),
                "stem_boundary",
                Json::str(STEM_NOTICE),
            ),
        }
    }
}
pub const NOTICE: &str = "Finding aid derived from the Register's EPUB reading view, not authorised \
legislation or tax advice. Read the authorised compilation on the Register before relying on a row.";

const DEFINITIONS_HEADING: &str = r"(?i)\b(?:definitions?|dictionary|interpretation)\b";
const LEAD_IN: &str =
    r"In (?:this|these) [^:,\n]{1,60}(?:, (?:unless|except) [^:,\n]{1,60})?[,:]\s+";
const VERB: &str = r"(?P<term>[^\n]{1,150}?)\s+(?:means|has the meaning given by|has the same meaning as|has the meaning|includes|does not include)\b";
const COLON: &str = r"(?P<term>[^:\n(]{1,80}):\s+\S";
const QUALIFIER: &str = r",\s+(?:in relation to|in respect of|for the purposes of)\b.*\Z";
const NOT_A_TERM: &str =
    r"(?i)(?:note|example|however|for example|in this|but|and|or|other than)\b";
const MAX_TERM_WORDS: usize = 15;

const SECTION_RE: &str = r"\d+[A-Z]{0,4}(?:[-‐‑]\d+[A-Z]{0,4})?";
const YEAR: &str = r"(?:18|19|20)\d{2}\b";
const LABEL: &str = r"\d+[A-Z]{0,4}(?:[-‐‑](?:\d+[A-Z]{0,4}|[A-Z]{1,3}\b))?(?![\w‐‑-]|\.\d)(?:\([0-9A-Za-z]{1,6}\))*";
const LIST_SEPARATOR: &str = r"(?:,\s+(?:(?:and|or)\s+)?|\s+(?:and|or)\s+)";
const SUBDIVISION: &str = r"\d+[A-Z]{0,4}[-‐‑][A-Z]{1,3}\b";
const MARKDOWN_HEADING: &str = r"^#{1,6} (.+)$";

/// Cited names that stand for a title's exact name, each checked against the
/// Register on 4 November 2026. A cited name resolves only as a title's name or
/// through this list, so any other abbreviation stays unresolved.
const ABBREVIATIONS: [(&str, &str); 5] = [
    ("ITAA 1997", "Income Tax Assessment Act 1997"),
    ("ITAA 1936", "Income Tax Assessment Act 1936"),
    ("TAA 1953", "Taxation Administration Act 1953"),
    (
        "GST Act",
        "A New Tax System (Goods and Services Tax) Act 1999",
    ),
    ("FBTAA 1986", "Fringe Benefits Tax Assessment Act 1986"),
];

fn cited() -> String {
    format!(
        r"(?:the\s+)?\*?([A-Z][A-Za-z]*(?:\s+[A-Z][A-Za-z]*){{0,3}}\s+{YEAR}|(?=[A-Z])[^.;:,\n]{{0,150}}?\b(?:Act|Regulations?|Rules)\b(?:\s+{YEAR})?)"
    )
}

/// Lower-case words that name a source, in a sentence or straight after a title's name.
const SOURCE_WORDS: &str = "bills?|regs?|regulations?|rules?|ordinances?|determinations?|instruments?|orders?|codes?|schedules?|acts?|laws?|legislation|statutes?|by-?laws?|rulings?|enactments?|treaty|treaties";

/// A lower-case word other than `and`, `or` or a source word, or a provision word: either
/// may follow a name without carrying it on.
fn name_ending_word() -> String {
    format!(
        r"(?!(?:and|or|{SOURCE_WORDS})\b)[a-z]|(?:Division|Subdivision|Part|Schedule|Chapter)\b"
    )
}

/// Whether an abbreviation ending at `end` ends its name there, rather than begin a
/// longer one such as `ITAA 1997 Amendment Act 2025`. After any whitespace, line breaks
/// included, only these may follow, past any closing `*`: the end of the text; `,` `.` `;`
/// `:` `)` or `]`; a
/// `name_ending_word`; a parenthesised part, then any of these; or `and` or `or`
/// (optionally `the`) before a `name_ending_word` or before a listed abbreviation or
/// held title that itself ends its name. Anything else, such as a capitalised word, a
/// digit or the `-2` of `ITAA 1997-2`, does not.
fn ends_name(text: &str, mut end: usize, lookup: impl Fn(&str) -> Option<String>) -> bool {
    let word_pattern = name_ending_word();
    let ending = pyre::re(&format!(r"\s*(?:\Z|[,.;:)\]])|\s+(?:{word_pattern})"), 0);
    let part = pyre::re(r"\s*\((?:[^()\n]|\([^()\n]*\))*\)", 0);
    let join = pyre::re(r"\s+(?:and|or)(?:\s+the)?\s+", 0);
    let word = pyre::re(&word_pattern, 0);
    let name = pyre::re(&cited(), 0);
    loop {
        end += text[end..].len() - text[end..].trim_start_matches('*').len();
        if ending.match_at(text, end).is_some() {
            return true;
        }
        if let Some(found) = part.match_at(text, end) {
            end = found.get(0).expect("group 0").end();
            continue;
        }
        let Some(found) = join.match_at(text, end) else {
            return false;
        };
        let after = found.get(0).expect("group 0").end();
        if word.match_at(text, after).is_some() {
            return true;
        }
        let Some(next) = name.match_at(text, after).and_then(|found| found.get(1)) else {
            return false;
        };
        let written = crate::py::split_whitespace(next.as_str()).join(" ");
        if !ABBREVIATIONS
            .iter()
            .any(|(abbreviation, _)| *abbreviation == written)
            && lookup(&written).is_none()
        {
            return false;
        }
        end = next.end();
    }
}

/// The title a citation names: `own` for a bare citation or one `of this Act`,
/// otherwise by exact name or through a listed abbreviation that ends its name.
fn cited_title(
    text: &str,
    end: usize,
    act: Option<&str>,
    own: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let Some(written) = act else {
        return own.map(str::to_string);
    };
    match ABBREVIATIONS
        .iter()
        .find(|(abbreviation, _)| *abbreviation == written)
    {
        // A blocked abbreviation stays unresolved rather than reach a wrong title.
        Some(_) if !ends_name(text, end, &lookup) => None,
        Some((_, name)) => lookup(name),
        None => lookup(written),
    }
}

fn citation_pattern() -> String {
    let c = cited();
    format!(
        r"\b(?P<keyword>(?i:(?:sub)?(?:section|paragraph|subparagraph)|subdivision|division))s?\s+(?P<labels>{LABEL}(?:{LIST_SEPARATOR}{LABEL})*)(?:\s+(?:of|in)\s+(?:Schedule\s+\d+[A-Z]?\s+to\s+{c}|this\s+\w+|Schedule\s+\d+[A-Z]?\b|{c}))?"
    )
}

fn level_pattern() -> String {
    format!(
        r"(Chapter|Part|Schedule|Division|Subdivision)\b(?:\s+({SUBDIVISION}|{SECTION_RE})(?![.\w]))?"
    )
}

/// `unicodedata.normalize("NFKC", text)`.
pub fn normalise(text: &str) -> String {
    text.nfkc().collect()
}

/// `normalise(value)` for a JSON value, or the TypeError a non-str raises.
fn normalise_value(value: &Json) -> Result<String, Exc> {
    match value {
        Json::Str(PyStr::Text(s)) => Ok(normalise(s)),
        // A lone surrogate passes through NFKC; the value fails later, at binding.
        Json::Str(PyStr::Lone(points)) => Ok(pyvalue::points_lossy(points)),
        other => Err(Exc::Type(format!(
            "normalize() argument 2 must be str, not {}",
            type_name(other)
        ))),
    }
}

fn hyphens(text: &str) -> String {
    text.replace(['\u{2010}', '\u{2011}'], "-")
}

/// Lookup key for a defined term: folded, unstarred, single-spaced, plain hyphens.
pub fn term_key(term: &str) -> String {
    let folded = crate::py::casefold(&hyphens(&normalise(term)).replace('*', ""));
    crate::py::split_whitespace(&folded).join(" ")
}

pub fn section_key(section: &str) -> String {
    hyphens(&normalise(section)).replace(' ', "")
}

/// `value or ""` for a value later normalised.
fn or_empty(value: &Json) -> Json {
    if value.truthy() {
        value.clone()
    } else {
        Json::str("")
    }
}

fn definition_start(paragraph: &str) -> Option<String> {
    let lead_in = pyre::re(LEAD_IN, 0);
    let paragraph = match lead_in.search(paragraph) {
        Some(m) => {
            let m = m.get(0).expect("group 0");
            format!("{}{}", &paragraph[..m.start()], &paragraph[m.end()..])
        }
        None => paragraph.to_string(),
    };
    let found = pyre::re(VERB, 0)
        .match_at(&paragraph, 0)
        .or_else(|| pyre::re(COLON, 0).match_at(&paragraph, 0))?;
    let term = found.name("term").expect("term group").as_str();
    let term = pyre::re(QUALIFIER, DOTALL).sub("", term).replace('*', "");
    let term = term
        .trim_matches(|c| " \t\"'\u{2018}\u{2019}\u{201c}\u{201d}".contains(c))
        .to_string();
    if term.is_empty()
        || pyre::re(NOT_A_TERM, 0).match_at(&term, 0).is_some()
        || crate::py::split_whitespace(&term).len() > MAX_TERM_WORDS
    {
        return None;
    }
    Some(term)
}

/// Split a definitions row into (term, definition text) pairs.
pub fn definitions(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    for paragraph in text.split("\n\n") {
        let paragraph = crate::py::strip(paragraph);
        if paragraph.is_empty() {
            continue;
        }
        if paragraph.starts_with("- ") || paragraph.starts_with('>') {
            if let Some(index) = current {
                found[index].1.push(paragraph.to_string());
            }
            continue;
        }
        if paragraph.starts_with('(') {
            current = None;
            continue;
        }
        match definition_start(paragraph) {
            Some(term) => {
                found.push((term, vec![paragraph.to_string()]));
                current = Some(found.len() - 1);
            }
            None => {
                if let Some(index) = current {
                    found[index].1.push(paragraph.to_string());
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(term, body)| (term, body.join("\n\n")))
        .collect()
}

/// (kind, key, cited act or None, citation text, where the citation ends in `text`)
/// for each reference in `text`.
pub fn citations(text: &str) -> Vec<(String, String, Option<String>, String, usize)> {
    let pattern = pyre::re(&citation_pattern(), 0);
    let separator = pyre::re(LIST_SEPARATOR, 0);
    let mut out = Vec::new();
    for found in pattern.captures_iter(text) {
        let keyword = crate::py::casefold(found.name("keyword").expect("keyword").as_str());
        let kind = if keyword == "division" || keyword == "subdivision" {
            keyword.clone()
        } else {
            "section".to_string()
        };
        let act = [3, 4]
            .into_iter()
            .find_map(|g| found.get(g).map(|m| m.as_str()).filter(|s| !s.is_empty()))
            .map(|a| crate::py::split_whitespace(a).join(" "));
        let whole = found.get(0).expect("group 0");
        let citation = crate::py::split_whitespace(whole.as_str()).join(" ");
        let mut hyphenated: Option<bool> = None;
        for label in separator.split(found.name("labels").expect("labels").as_str()) {
            let key = section_key(label.split('(').next().unwrap_or(""));
            let has_hyphen = key.contains('-');
            match hyphenated {
                None => hyphenated = Some(has_hyphen),
                Some(first) if first != has_hyphen => break,
                Some(_) => {}
            }
            let fits = if pyre::fullmatch(SUBDIVISION, 0, &key) {
                kind == "subdivision" && !label.contains('(')
            } else {
                kind == "section"
                    || (kind == "division" && !format!("{key}{label}").contains(['-', '(']))
            };
            if fits {
                out.push((
                    kind.clone(),
                    key,
                    act.clone(),
                    citation.clone(),
                    whole.end(),
                ));
            }
        }
    }
    out
}

pub type Place = (Option<String>, Option<String>);

/// The (Division, Subdivision) each row of one title sits in, as far as the
/// Python generator got: its places, and the error that stopped it at the
/// next row, if any.
pub fn structure(rows: &[Json], markdown: &str) -> (Vec<Place>, Option<Exc>) {
    let level = pyre::re(&level_pattern(), 0);
    let mut places: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    let (mut division, mut subdivision): (Option<String>, Option<String>) = (None, None);
    for text in pyre::re(MARKDOWN_HEADING, pyre::M).findall(markdown) {
        let heading = crate::py::split_whitespace(&normalise(&text)).join(" ");
        if let Some(found) = level.match_at(&heading, 0) {
            let label = found.get(2).map(|m| section_key(m.as_str()));
            match found.get(1).map(|m| m.as_str()) {
                Some("Division") => {
                    division = label;
                    subdivision = None;
                }
                Some("Subdivision") => {
                    subdivision = label.filter(|l| pyre::fullmatch(SUBDIVISION, 0, l));
                }
                _ => {
                    division = None;
                    subdivision = None;
                }
            }
        }
        places.push((heading, division.clone(), subdivision.clone()));
    }
    let mut position = 0;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let heading = match normalise_value(&or_empty(row.get("heading").unwrap_or(&Json::Null))) {
            Ok(heading) => heading,
            Err(error) => return (out, Some(error)),
        };
        let heading = match row.get("heading") {
            Some(Json::Str(PyStr::Lone(_))) => None,
            _ => Some(crate::py::split_whitespace(&heading).join(" ")),
        };
        let found = heading.and_then(|h| (position..places.len()).find(|&i| places[i].0 == h));
        match found {
            Some(index) => {
                position = index + 1;
                out.push((places[index].1.clone(), places[index].2.clone()));
            }
            None => out.push((None, None)),
        }
    }
    (out, None)
}

/// A SearchIndexError: the corpus or the index failed a check.
fn refused(message: impl Into<String>) -> Exc {
    Exc::Value {
        class: "fadden.search.SearchIndexError",
        message: message.into(),
    }
}

fn sqlite_error(error: &rusqlite::Error) -> Exc {
    let message = match error {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        rusqlite::Error::SqliteFailure(code, None) => code.to_string(),
        other => other.to_string(),
    };
    Exc::Sqlite(message)
}

fn strerror(error: &OsError) -> String {
    error.strerror.clone()
}

fn read_bytes(root: &str, rid: &str, name: &str) -> Result<Vec<u8>, Exc> {
    let path = child(root, &["markdown", rid, name])?;
    let file = crate::pyio::open(&path)
        .map_err(|e| refused(format!("{rid}: cannot read {name} ({})", strerror(&e))))?;
    read_all(file, &path)
        .map_err(|e| refused(format!("{rid}: cannot read {name} ({})", strerror(&e))))
}

fn read_all(mut file: std::fs::File, path: &str) -> Result<Vec<u8>, OsError> {
    use std::io::Read;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
    Ok(data)
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub struct Title {
    title: Json,
    rid: String,
    rows: Vec<Json>,
    markdown: String,
    sections_sha256: String,
    markdown_sha256: String,
}

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or(&Json::Null)
}

/// `"%s" % value`: `str()`, with a lone surrogate kept for binding to refuse.
fn read_title(root: &str, title: &Json) -> Result<Title, Exc> {
    let rid = register_id(get(title, "register_id"))?;
    let data = read_bytes(root, &rid, "sections.jsonl")?;
    let markdown = read_bytes(root, &rid, &format!("{rid}.md"))?;
    let (text, markdown_text) = match (std::str::from_utf8(&data), std::str::from_utf8(&markdown)) {
        (Ok(a), Ok(b)) => (a.to_string(), b.to_string()),
        _ => {
            return Err(refused(format!(
                "{rid}: sections.jsonl or {rid}.md is not UTF-8"
            )));
        }
    };
    let mut rows: Vec<Json> = Vec::new();
    for (number, line) in crate::py::splitlines(&text)
        .into_iter()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
    {
        if crate::py::strip(line).is_empty() {
            continue;
        }
        let place = format!("{rid}/sections.jsonl:{number}");
        let row = match pyjson::loads_standard(line) {
            Ok(row) => row,
            Err(pyjson::DecodeError::Invalid(message))
                if message.starts_with("Exceeds the limit") =>
            {
                return Err(Exc::value(message));
            }
            Err(error @ pyjson::DecodeError::Recursion(_)) => return Err(super::json_error(error)),
            Err(_) => return Err(refused(format!("{place} is not JSON"))),
        };
        if !matches!(row.get("text"), Some(Json::Str(_))) {
            return Err(refused(format!("{place} is not a sections.jsonl row")));
        }
        let ordinal = rows.len() + 1;
        let section = get(&row, "section");
        let mut expected: Vec<u32> = format!("{rid}:{ordinal:04}:")
            .chars()
            .map(u32::from)
            .collect();
        if section.truthy() {
            expected.extend(pyvalue::str_points(section));
        } else {
            expected.push(u32::from('-'));
        }
        let expected = Json::Str(PyStr::from_code_points(expected));
        if !py_eq(get(&row, "register_id"), &Json::str(&rid)) {
            return Err(refused(format!("{place} names another title")));
        }
        if !py_eq(get(&row, "row_id"), &expected) {
            return Err(refused(format!(
                "{place} has row_id {}, expected {}",
                repr(get(&row, "row_id")),
                repr(&expected)
            )));
        }
        for field in ["compilation_number", "compilation_date"] {
            if !py_eq(get(&row, field), get(title, field)) {
                return Err(refused(format!(
                    "{place} {field} differs from sources.json"
                )));
            }
        }
        if !matches!(row.get("authorised"), Some(Json::Bool(false))) {
            return Err(refused(format!("{place} is not marked authorised: false")));
        }
        rows.push(row);
    }
    if !py_eq(&Json::Int(rows.len().into()), get(title, "jsonl_rows")) {
        return Err(refused(format!(
            "{rid}: sections.jsonl holds {} rows; sources.json records {}",
            rows.len(),
            str_for_stderr(get(title, "jsonl_rows"))
        )));
    }
    Ok(Title {
        title: title.clone(),
        rid,
        rows,
        markdown: markdown_text,
        sections_sha256: hex(&data),
        markdown_sha256: hex(&markdown),
    })
}

fn read_corpus(root: &str) -> Result<(Vec<u8>, Json, Vec<Title>), Exc> {
    let path = child(root, &["sources.json"])?;
    let sources_bytes = crate::pyio::open(&path)
        .and_then(|file| read_all(file, &path))
        .map_err(|e| refused(format!("cannot read sources.json ({})", strerror(&e))))?;
    let not_finished = || refused("sources.json is not a finished corpus inventory");
    let sources = match pyjson::loads_bytes(&sources_bytes) {
        Ok(sources) => sources,
        Err(Some(error @ pyjson::DecodeError::Recursion(_))) => {
            return Err(super::json_error(error));
        }
        Err(_) => return Err(not_finished()),
    };
    let listed = match sources.get("titles") {
        Some(Json::Array(items)) if items.iter().all(|t| matches!(t, Json::Object(_))) => {
            items.clone()
        }
        _ => return Err(not_finished()),
    };
    let ids = listed
        .iter()
        .map(|t| register_id(get(t, "register_id")))
        .collect::<Result<BTreeSet<String>, Exc>>()?;
    super::corpus_paths::refuse_unlisted_titles(root, &ids).map_err(|e| match e {
        Exc::Runtime(message) => refused(message),
        other => other,
    })?;
    let read = listed
        .iter()
        .map(|title| read_title(root, title))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sources_bytes, sources, read))
}

const SCHEMA_SQL: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE titles (
  register_id TEXT PRIMARY KEY, name TEXT NOT NULL, collection TEXT,
  compilation_number TEXT, compilation_date TEXT, version_is_current INTEGER NOT NULL,
  retrieved TEXT, superseded_from TEXT, register_page TEXT, source_url TEXT, rows INTEGER NOT NULL,
  sections_sha256 TEXT NOT NULL, markdown_sha256 TEXT NOT NULL);
CREATE TABLE rows (
  id INTEGER PRIMARY KEY, row_id TEXT NOT NULL UNIQUE,
  register_id TEXT NOT NULL REFERENCES titles, section TEXT, section_key TEXT,
  division_key TEXT, subdivision_key TEXT, heading TEXT, container TEXT, text TEXT NOT NULL);
CREATE INDEX rows_section ON rows (register_id, section_key);
CREATE INDEX rows_division ON rows (register_id, division_key);
CREATE INDEX rows_subdivision ON rows (register_id, subdivision_key);
CREATE VIRTUAL TABLE rows_fts USING fts5 (
  heading, text, content='rows', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE rows_stem USING fts5 (
  heading, text, content='rows', content_rowid='id',
  tokenize='porter unicode61 remove_diacritics 2');
CREATE TABLE definitions (
  term TEXT NOT NULL, term_key TEXT NOT NULL, row_id TEXT NOT NULL REFERENCES rows (row_id),
  position INTEGER NOT NULL, text TEXT NOT NULL);
CREATE INDEX definitions_key ON definitions (term_key);
CREATE TABLE refs (
  from_row_id TEXT NOT NULL REFERENCES rows (row_id), kind TEXT NOT NULL,
  to_register_id TEXT, to_key TEXT NOT NULL, cited_act TEXT, citation TEXT NOT NULL);
CREATE INDEX refs_to ON refs (to_register_id, kind, to_key);
CREATE INDEX refs_from ON refs (from_row_id);
";

/// How `sqlite3` binds one parameter (1-based `index`).
fn bind(value: &Json, index: usize) -> Result<Sql, Exc> {
    Ok(match value {
        Json::Null => Sql::Null,
        Json::Bool(b) => Sql::Integer(i64::from(*b)),
        Json::Int(i) => Sql::Integer(i64::try_from(i).map_err(|_| {
            Exc::Overflow("Python int too large to convert to SQLite INTEGER".into())
        })?),
        Json::Float(f) => Sql::Real(*f),
        Json::Str(PyStr::Text(s)) => Sql::Text(s.clone()),
        Json::Str(PyStr::Lone(points)) => {
            return Err(Exc::UnicodeEncode(
                crate::pyvalue::utf8_strict_error(points).unwrap_or_default(),
            ));
        }
        other => {
            return Err(Exc::Sqlite(format!(
                "Error binding parameter {index}: type '{}' is not supported",
                type_name(other)
            )));
        }
    })
}

fn bind_all(values: &[Json]) -> Result<Vec<Sql>, Exc> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| bind(v, i + 1))
        .collect()
}

fn text_or_null(value: Option<String>) -> Json {
    value.map_or(Json::Null, |s| Json::str(&s))
}

/// `int(value)` for `title.get("version_is_current", True)`.
fn py_int(value: &Json) -> Result<Json, Exc> {
    match value {
        Json::Bool(b) => Ok(Json::Int(u8::from(*b).into())),
        Json::Int(i) => Ok(Json::Int(i.clone())),
        Json::Float(f) if f.is_nan() => Err(Exc::value("cannot convert float NaN to integer")),
        Json::Float(f) if f.is_infinite() => Err(Exc::Overflow(
            "cannot convert float infinity to integer".into(),
        )),
        Json::Float(f) => Ok(Json::Int(pyvalue::float_to_int(f.trunc()))),
        Json::Str(PyStr::Text(s)) => crate::py::parse_int(s, 10)
            .map(Json::Int)
            .map_err(Exc::value),
        Json::Str(PyStr::Lone(points)) => Err(Exc::value(crate::py::invalid_int_literal(
            10,
            &pyvalue::repr_points(points),
        ))),
        other => Err(Exc::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

#[derive(Default)]
struct Counts {
    rows: usize,
    definitions: usize,
    refs: usize,
}

/// Check the corpus at `root` and write a new index file at `out`.
pub fn build(root: &str, out: &str) -> Result<[usize; 4], Exc> {
    let destination = PyPath::new(out);
    if std::fs::symlink_metadata(destination.fs()).is_ok() {
        return Err(refused("the index destination must not already exist"));
    }
    if !crate::pyos::isdir(&destination.parent().display()) {
        return Err(refused(
            "the index destination's parent directory does not exist",
        ));
    }
    let (sources_bytes, sources, read) = read_corpus(root)?;
    // Two titles sharing a name leave a citation of that name unresolved.
    let mut names: Vec<(pyvalue::Key, Option<String>)> = Vec::new();
    for entry in &read {
        let name = entry
            .title
            .get("name")
            .ok_or_else(|| Exc::Key("'name'".into()))?;
        let key = pyvalue::key(name).map_err(|m| {
            Exc::Type(format!(
                "cannot use '{}' as a dict key ({m})",
                type_name(name)
            ))
        })?;
        match names.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = None,
            None => names.push((key, Some(entry.rid.clone()))),
        }
    }
    let temporary = destination
        .parent()
        .join(&format!(
            ".{}.{}.tmp",
            destination.name(),
            crate::pyos::random_hex(32)
        ))
        .display();
    let result = write_index(&temporary, &read, &sources, &sources_bytes, &names);
    let cleanup = |path: &str| {
        let _ = std::fs::remove_file(path);
    };
    let counts = match result {
        Ok(counts) => counts,
        Err(error) => {
            cleanup(&temporary);
            return Err(error);
        }
    };
    if let Err(error) = crate::pyos::link(&temporary, &destination.display()) {
        cleanup(&temporary);
        return Err(Exc::Os(error));
    }
    crate::pyos::unlink(&temporary)?;
    Ok([read.len(), counts.rows, counts.definitions, counts.refs])
}

fn write_index(
    temporary: &str,
    read: &[Title],
    sources: &Json,
    sources_bytes: &[u8],
    names: &[(pyvalue::Key, Option<String>)],
) -> Result<Counts, Exc> {
    let connection = Connection::open(temporary).map_err(|e| sqlite_error(&e))?;
    connection
        .execute_batch(SCHEMA_SQL)
        .map_err(|e| sqlite_error(&e))?;
    connection
        .execute_batch("BEGIN")
        .map_err(|e| sqlite_error(&e))?;
    let mut counts = Counts::default();
    let outcome = (|| -> Result<(), Exc> {
        for entry in read {
            let title = &entry.title;
            let current = py_int(title.get("version_is_current").unwrap_or(&Json::Bool(true)))?;
            let values = [
                Json::str(&entry.rid),
                get(title, "name").clone(),
                get(title, "collection").clone(),
                get(title, "compilation_number").clone(),
                get(title, "compilation_date").clone(),
                current,
                get(title, "retrieved").clone(),
                get(title, "current_version_start").clone(),
                get(title, "register_page").clone(),
                get(title, "source_url").clone(),
                Json::Int(entry.rows.len().into()),
                Json::str(&entry.sections_sha256),
                Json::str(&entry.markdown_sha256),
            ];
            run(
                &connection,
                "INSERT INTO titles VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params_from_iter(bind_all(&values)?),
            )
            .map_err(|e| sqlite_error(&e))?;
            let (places, mut failure) = structure(&entry.rows, &entry.markdown);
            for (index, row) in entry.rows.iter().enumerate() {
                // zip draws row i, then its place: a failure stops before row i.
                let Some(place) = places.get(index).cloned() else {
                    return Err(failure.take().expect("structure stopped early"));
                };
                insert_row(&connection, &entry.rid, row, place, names, &mut counts)?;
            }
        }
        let mut meta = vec![
            ("schema".to_string(), Json::str(SCHEMA)),
            (
                "corpus_retrieved".to_string(),
                Json::Str(PyStr::from_code_points(pyvalue::str_points(get(
                    sources,
                    "retrieved",
                )))),
            ),
            ("sources_sha256".to_string(), Json::str(&hex(sources_bytes))),
        ];
        for (name, value) in [
            ("titles", read.len()),
            ("rows", counts.rows),
            ("definitions", counts.definitions),
            ("refs", counts.refs),
        ] {
            meta.push((format!("{name}_count"), Json::str(&value.to_string())));
        }
        for (key, value) in meta {
            run(
                &connection,
                "INSERT INTO meta VALUES (?, ?)",
                params_from_iter(bind_all(&[Json::str(&key), value])?),
            )
            .map_err(|e| sqlite_error(&e))?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            connection
                .execute_batch("COMMIT")
                .map_err(|e| sqlite_error(&e))?;
            connection.close().map_err(|(_, e)| sqlite_error(&e))?;
            Ok(counts)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            drop(connection);
            Err(error)
        }
    }
}

/// `connection.execute(sql, params)` through the statement cache: Python's
/// `sqlite3` reuses a statement it has compiled, and compiling each insert
/// again was most of the build's time.
fn run(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<usize> {
    connection.prepare_cached(sql)?.execute(params)
}

fn insert_row(
    connection: &Connection,
    rid: &str,
    row: &Json,
    place: (Option<String>, Option<String>),
    names: &[(pyvalue::Key, Option<String>)],
    counts: &mut Counts,
) -> Result<(), Exc> {
    let section = get(row, "section");
    let key = if section.truthy() {
        let normal = normalise_value(section)?;
        Json::str(&hyphens(&normal).replace(' ', ""))
    } else {
        Json::Null
    };
    let text = get(row, "text").as_str().unwrap_or_default().to_string();
    let values = [
        get(row, "row_id").clone(),
        Json::str(rid),
        section.clone(),
        key,
        text_or_null(place.0),
        text_or_null(place.1),
        get(row, "heading").clone(),
        get(row, "container").clone(),
        get(row, "text").clone(),
    ];
    run(connection,
            "INSERT INTO rows (row_id, register_id, section, section_key, division_key, subdivision_key, heading, container, text) VALUES (?,?,?,?,?,?,?,?,?)",
            params_from_iter(bind_all(&values)?),
        )
        .map_err(|e| sqlite_error(&e))?;
    let id = connection.last_insert_rowid();
    let heading = normalise_value(&or_empty(get(row, "heading")))?;
    let normal_text = normalise(&text);
    for sql in [
        "INSERT INTO rows_fts (rowid, heading, text) VALUES (?,?,?)",
        "INSERT INTO rows_stem (rowid, heading, text) VALUES (?,?,?)",
    ] {
        run(connection, sql, rusqlite::params![id, heading, normal_text])
            .map_err(|e| sqlite_error(&e))?;
    }
    counts.rows += 1;
    let row_id = get(row, "row_id").as_str().unwrap_or_default();
    let heading_text = match or_empty(get(row, "heading")) {
        Json::Str(s) => pyvalue::points_lossy(&s.code_points()),
        _ => String::new(),
    };
    if pyre::re(DEFINITIONS_HEADING, 0)
        .search(&heading_text)
        .is_some()
    {
        for (position, (term, body)) in definitions(&text).into_iter().enumerate() {
            run(
                connection,
                "INSERT INTO definitions VALUES (?,?,?,?,?)",
                rusqlite::params![term, term_key(&term), row_id, position as i64 + 1, body],
            )
            .map_err(|e| sqlite_error(&e))?;
            counts.definitions += 1;
        }
    }
    let lookup = |name: &str| {
        names
            .iter()
            .find(|(k, _)| *k == pyvalue::Key::Str(name.chars().map(u32::from).collect()))
            .and_then(|(_, v)| v.clone())
    };
    for (kind, cited, act, citation, end) in citations(&text) {
        let target = cited_title(&text, end, act.as_deref(), Some(rid), lookup);
        run(
            connection,
            "INSERT INTO refs VALUES (?,?,?,?,?,?)",
            rusqlite::params![row_id, kind, target, cited, act, citation],
        )
        .map_err(|e| sqlite_error(&e))?;
        counts.refs += 1;
    }
    Ok(())
}

/// Open an index read-only and check its schema.
pub fn open_index(path: &str) -> Result<Connection, Exc> {
    let py = PyPath::new(path);
    if !std::fs::metadata(py.fs()).is_ok_and(|m| m.is_file()) {
        return Err(refused("the index file does not exist"));
    }
    let resolved = py.resolve().map_err(Exc::Os)?;
    let connection = Connection::open_with_flags(
        resolved.fs(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| sqlite_error(&e))?;
    let found: Result<Option<Sql>, rusqlite::Error> = connection
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| {
            r.get::<_, Sql>(0)
        })
        .map(Some)
        .or_else(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(e)
            }
        });
    match found {
        Err(_) => Err(refused("the file is not a search index")),
        Ok(Some(Sql::Text(value))) if value == SCHEMA => Ok(connection),
        Ok(_) => Err(refused("the index was built by a different schema version")),
    }
}

fn to_json(value: Sql) -> Json {
    match value {
        Sql::Null => Json::Null,
        Sql::Integer(i) => Json::Int(i.into()),
        Sql::Real(f) => Json::Float(f),
        Sql::Text(s) => Json::str(&s),
        Sql::Blob(b) => Json::str(&crate::py::repr_bytes(&b)),
    }
}

/// Every row of a statement as (column, value) pairs.
fn rows_of(
    connection: &Connection,
    sql: &str,
    params: &[Sql],
) -> Result<Vec<Vec<(String, Json)>>, Exc> {
    let mut statement = connection.prepare(sql).map_err(|e| sqlite_error(&e))?;
    let columns: Vec<String> = statement.column_names().map(str::to_string).collect();
    let mut rows = statement
        .query(params_from_iter(params.iter()))
        .map_err(|e| sqlite_error(&e))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| sqlite_error(&e))? {
        let mut pairs = Vec::with_capacity(columns.len());
        for (i, name) in columns.iter().enumerate() {
            pairs.push((
                name.clone(),
                to_json(row.get::<_, Sql>(i).map_err(|e| sqlite_error(&e))?),
            ));
        }
        out.push(pairs);
    }
    Ok(out)
}

fn object(pairs: Vec<(String, Json)>) -> Json {
    Json::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (PyStr::Text(k), v))
            .collect(),
    )
}

/// `_hit(row)`: the row as a dict, `version_is_current` as a bool, and `authorised: False`.
fn hit(mut pairs: Vec<(String, Json)>) -> Json {
    for (name, value) in pairs.iter_mut() {
        if name == "version_is_current" {
            *value = Json::Bool(value.truthy());
        }
    }
    pairs.push(("authorised".into(), Json::Bool(false)));
    object(pairs)
}

/// Quote each query word as an FTS5 phrase so no input is read as syntax.
pub fn match_expression(words: &[String]) -> Result<String, Exc> {
    let word = pyre::re(r"\w+", 0);
    let phrases: Vec<String> = words
        .iter()
        .map(|w| word.findall(&normalise(w)))
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| format!("\"{}\"", tokens.join(" ")))
        .collect();
    if phrases.is_empty() {
        return Err(refused("the query has no searchable words"));
    }
    Ok(phrases.join(" "))
}

const QUERY_SQL: &str = "SELECT r.row_id, r.register_id, r.section, r.heading, r.container, r.text, \
t.name AS act, t.collection, t.compilation_number, t.compilation_date, \
t.version_is_current, t.retrieved, t.register_page, t.source_url, t.sections_sha256, \
snippet(rows_fts, 1, '[', ']', '...', 16) AS snippet \
FROM rows_fts JOIN rows r ON r.id = rows_fts.rowid \
JOIN titles t ON t.register_id = r.register_id \
WHERE rows_fts MATCH ? AND (? IS NULL OR r.register_id = ?) \
AND (? IS NULL OR r.register_id IN (SELECT value FROM json_each(?))) \
ORDER BY r.section_key IN (SELECT value FROM json_each(?)) DESC, \
t.version_is_current DESC, bm25(rows_fts, 4.0, 1.0), r.id LIMIT ?";
const DEFINE_SQL: &str = "SELECT d.term, d.text AS definition, r.row_id, r.register_id, r.section, r.heading, \
t.name AS act, t.collection, t.compilation_number, t.compilation_date, \
t.version_is_current, t.retrieved, t.register_page, t.source_url, t.sections_sha256 \
FROM definitions d JOIN rows r ON r.row_id = d.row_id \
JOIN titles t ON t.register_id = r.register_id \
WHERE ((? = 0 AND d.term_key = ?) OR (? = 1 AND d.term_key LIKE ? ESCAPE '\\')) \
AND (? IS NULL OR r.register_id = ?) \
ORDER BY t.version_is_current DESC, t.name, r.id, d.position";

fn target_sql(kind: &str) -> String {
    let column = match kind {
        "division" => "division_key",
        "subdivision" => "subdivision_key",
        _ => "section_key",
    };
    format!(
        "SELECT row_id, heading, container FROM rows WHERE register_id = ? AND {column} = ? ORDER BY id"
    )
}

fn optional_rid(title: Option<&str>) -> Result<Sql, Exc> {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => Ok(Sql::Text(register_id_str(t)?)),
        None => Ok(Sql::Null),
    }
}

pub fn query(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
) -> Result<Vec<Json>, Exc> {
    ranked(
        connection,
        words,
        title,
        limit,
        Sql::Null,
        SearchMode::Exact,
    )
}

/// Match English word forms, retaining original text and marking every hit.
pub fn query_stemmed(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
) -> Result<Vec<Json>, Exc> {
    ranked(
        connection,
        words,
        title,
        limit,
        Sql::Null,
        SearchMode::Stemmed,
    )
}

/// `query`, among only the register ids `only` lists as JSON, if it is not null.
fn ranked(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    only: Sql,
    mode: SearchMode,
) -> Result<Vec<Json>, Exc> {
    let rid = optional_rid(title)?;
    let mut sections: Vec<String> = words
        .iter()
        .filter(|w| pyre::fullmatch(SECTION_RE, 0, &normalise(w)))
        .map(|w| section_key(w))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    sections.sort();
    let encoded: Vec<Json> = sections.iter().map(|s| Json::str(s)).collect();
    let sections_json: String = pyvalue::points_lossy(&dumps_line(&Json::Array(encoded), true));
    let params = [
        Sql::Text(match_expression(words)?),
        rid.clone(),
        rid,
        only.clone(),
        only,
        Sql::Text(sections_json),
        Sql::Integer(limit),
    ];
    Ok(rows_of(connection, &mode.sql(QUERY_SQL), &params)?
        .into_iter()
        .map(|row| mode.mark(hit(row)))
        .collect())
}

/// Each title's standing against one date, classified as the `as_at` stage does.
pub struct Dated {
    date: String,
    titles: Vec<Json>,
}

/// What `query` and `define` return for a date: rows from `captured` titles, one
/// refusal for each other title that matched or that `--title` named, and a summary.
pub struct DatedReport {
    pub hits: Vec<Json>,
    pub refusals: Vec<Json>,
    pub summary: Json,
}

impl Dated {
    pub fn read(connection: &Connection, date: &str) -> Result<Dated, Exc> {
        if !as_at::is_date(date) {
            return Err(refused(format!("--as-at must be YYYY-MM-DD, not {date}")));
        }
        let sql = "SELECT register_id, name, compilation_date, superseded_from, retrieved, \
                   version_is_current FROM titles ORDER BY register_id";
        let mut titles = Vec::new();
        for row in rows_of(connection, sql, &[])? {
            let start = field(&row, "compilation_date");
            let (superseded, retrieved) =
                (field(&row, "superseded_from"), field(&row, "retrieved"));
            // The manifest's names for these fields, so `as_at` applies its own checks.
            let entry = Json::obj([
                ("id", field(&row, "register_id").clone()),
                ("versionStart", start.clone()),
                ("retrieved", retrieved.clone()),
                (
                    "version_is_current",
                    Json::Bool(field(&row, "version_is_current").truthy()),
                ),
                ("current_version_start", superseded.clone()),
            ]);
            titles.push(Json::obj([
                ("register_id", field(&row, "register_id").clone()),
                ("name", field(&row, "name").clone()),
                ("status", Json::str(as_at::classify(&entry, date)?)),
                ("compilation_date", start.clone()),
                ("superseded_from", superseded.clone()),
                ("retrieved", retrieved.clone()),
            ]));
        }
        Ok(Dated {
            date: date.to_string(),
            titles,
        })
    }

    fn title(&self, rid: &str) -> Option<&Json> {
        self.titles
            .iter()
            .find(|title| title.at("register_id").as_str() == Some(rid))
    }

    /// The title's status, window and what the status covers, or the next step.
    fn standing(&self, rid: &str) -> Json {
        let Some(title) = self.title(rid) else {
            return Json::Null;
        };
        let status = title.at("status").as_str().unwrap_or_default();
        Json::obj([
            ("date", Json::str(&self.date)),
            ("status", title.at("status").clone()),
            ("compilation_date", title.at("compilation_date").clone()),
            ("superseded_from", title.at("superseded_from").clone()),
            ("retrieved", title.at("retrieved").clone()),
            ("covers", Json::str(as_at::CAPTURED_MEANS)),
            (
                "next_step",
                text_or_null(as_at::next_step(status).map(str::to_string)),
            ),
        ])
    }

    fn captured(&self, rid: &str) -> bool {
        self.title(rid)
            .is_some_and(|title| title.at("status").as_str() == Some(as_at::CAPTURED))
    }

    /// `hits` from captured titles, the refusals and the summary.
    fn report(
        &self,
        hits: Vec<Json>,
        matched: &BTreeMap<String, usize>,
        named: Option<&str>,
    ) -> DatedReport {
        let hits: Vec<Json> = hits
            .into_iter()
            .filter(|hit| self.captured(hit.at("register_id").as_str().unwrap_or_default()))
            .map(|hit| {
                let standing = self.standing(hit.at("register_id").as_str().unwrap_or_default());
                with_field(hit, "as_at", standing)
            })
            .collect();
        let refusals = self.refusals(hits.is_empty(), matched, named);
        let summary = self.summary(hits.len(), &refusals);
        DatedReport {
            hits,
            refusals,
            summary,
        }
    }

    /// A refusal for each title not captured that `matched` rows or that `named`. With
    /// no row to show, a search of all titles refuses every title not captured, so an
    /// empty answer with exit 0 means each title in scope was held for the date.
    fn refusals(
        &self,
        no_rows: bool,
        matched: &BTreeMap<String, usize>,
        named: Option<&str>,
    ) -> Vec<Json> {
        let mut refusals = Vec::new();
        for title in &self.titles {
            let rid = title.at("register_id").as_str().unwrap_or_default();
            let rows = matched.get(rid).copied().unwrap_or_default();
            let held = title.at("status").as_str() == Some(as_at::CAPTURED);
            if !held && (rows > 0 || named == Some(rid) || (no_rows && named.is_none())) {
                refusals.push(Json::obj([
                    ("refused", Json::Bool(true)),
                    ("register_id", title.at("register_id").clone()),
                    ("act", title.at("name").clone()),
                    ("matching_rows", Json::Int(rows.into())),
                    ("as_at", self.standing(rid)),
                ]));
            }
        }
        refusals
    }

    fn summary(&self, rows: usize, refusals: &[Json]) -> Json {
        let count = |status: &'static str| {
            let of = |title: &&Json| title.at("status").as_str() == Some(status);
            (
                status,
                Json::Int(self.titles.iter().filter(of).count().into()),
            )
        };
        let withheld: usize = refusals
            .iter()
            .map(|refusal| match refusal.at("matching_rows") {
                Json::Int(rows) => usize::try_from(rows).unwrap_or_default(),
                _ => 0,
            })
            .sum();
        Json::obj([(
            "as_at_summary",
            Json::obj([
                ("date", Json::str(&self.date)),
                ("rows", Json::Int(rows.into())),
                ("withheld_rows", Json::Int(withheld.into())),
                ("titles", Json::obj(as_at::STATUSES.map(count))),
                ("covers", Json::str(as_at::CAPTURED_MEANS)),
            ]),
        )])
    }
}

/// `value` with one more member, if it is an object.
fn with_field(mut value: Json, key: &str, member: Json) -> Json {
    if let Json::Object(pairs) = &mut value {
        pairs.push((PyStr::Text(key.into()), member));
    }
    value
}

/// `query` for a date: rows only from titles the build holds for it.
pub fn query_as_at(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
) -> Result<DatedReport, Exc> {
    dated_query(connection, words, title, limit, date, SearchMode::Exact)
}

/// Word-form search with the same captured-window restriction as exact search.
pub fn query_stemmed_as_at(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
) -> Result<DatedReport, Exc> {
    dated_query(connection, words, title, limit, date, SearchMode::Stemmed)
}

fn dated_query(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
    mode: SearchMode,
) -> Result<DatedReport, Exc> {
    let dated = Dated::read(connection, date)?;
    let named = given_title(connection, title)?;
    let rid = named.clone().map_or(Sql::Null, Sql::Text);
    let sql = "SELECT r.register_id, count(*) AS matched FROM rows_fts \
               JOIN rows r ON r.id = rows_fts.rowid \
               WHERE rows_fts MATCH ? AND (? IS NULL OR r.register_id = ?) GROUP BY r.register_id";
    let params = [Sql::Text(match_expression(words)?), rid.clone(), rid];
    let mut matched = BTreeMap::new();
    for row in rows_of(connection, &mode.sql(sql), &params)? {
        let rows = match field(&row, "matched") {
            Json::Int(rows) => usize::try_from(rows).unwrap_or_default(),
            _ => 0,
        };
        let title = field(&row, "register_id").as_str().unwrap_or_default();
        matched.insert(title.to_string(), rows);
    }
    let captured = dated
        .titles
        .iter()
        .filter(|title| dated.captured(title.at("register_id").as_str().unwrap_or_default()));
    let ids = Json::Array(
        captured
            .map(|title| title.at("register_id").clone())
            .collect(),
    );
    let only = Sql::Text(pyvalue::points_lossy(&dumps_line(&ids, true)));
    let hits = ranked(connection, words, title, limit, only, mode)?;
    Ok(dated.report(hits, &matched, named.as_deref()))
}

/// `define` for a date: definitions only from titles the build holds for it.
pub fn define_as_at(
    connection: &Connection,
    term: &str,
    title: Option<&str>,
    date: &str,
) -> Result<DatedReport, Exc> {
    let dated = Dated::read(connection, date)?;
    let named = given_title(connection, title)?;
    let hits = define(connection, term, title)?;
    let mut matched = BTreeMap::new();
    for hit in &hits {
        let rid = hit.at("register_id").as_str().unwrap_or_default();
        *matched.entry(rid.to_string()).or_default() += 1;
    }
    Ok(dated.report(hits, &matched, named.as_deref()))
}

/// `cite_check`, with each citation reporting its title's standing on a date, and a
/// refusal if `--title` names one not captured, whatever the draft cites. A citation
/// that reached no title has no standing.
pub fn cite_check_as_at(
    connection: &Connection,
    draft: &str,
    title: Option<&str>,
    date: &str,
) -> Result<(Vec<Json>, Vec<Json>), Exc> {
    let dated = Dated::read(connection, date)?;
    let report = cite_check(connection, draft, title)?;
    let named = given_title(connection, title)?;
    let refusals = match named.as_deref() {
        Some(rid) => dated.refusals(true, &BTreeMap::new(), Some(rid)),
        None => Vec::new(),
    };
    let dated_item = |item: Json| {
        let standing = item
            .at("register_id")
            .as_str()
            .map(|rid| dated.standing(rid));
        with_field(item, "as_at", standing.unwrap_or(Json::Null))
    };
    Ok((report.into_iter().map(dated_item).collect(), refusals))
}

pub fn define(connection: &Connection, term: &str, title: Option<&str>) -> Result<Vec<Json>, Exc> {
    let key = term_key(term);
    if key.is_empty() {
        return Err(refused("the term is empty"));
    }
    let rid = optional_rid(title)?;
    let exact = rows_of(
        connection,
        DEFINE_SQL,
        &[
            Sql::Integer(0),
            Sql::Text(key.clone()),
            Sql::Integer(0),
            Sql::Null,
            rid.clone(),
            rid.clone(),
        ],
    )?;
    if !exact.is_empty() {
        return Ok(exact.into_iter().map(hit).collect());
    }
    let prefix = format!(
        "{} %",
        key.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    Ok(rows_of(
        connection,
        DEFINE_SQL,
        &[
            Sql::Integer(1),
            Sql::Null,
            Sql::Integer(1),
            Sql::Text(prefix),
            rid.clone(),
            rid,
        ],
    )?
    .into_iter()
    .map(hit)
    .collect())
}

fn field<'a>(pairs: &'a [(String, Json)], name: &str) -> &'a Json {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map_or(&Json::Null, |(_, v)| v)
}

/// What `row_id` cites, and rows that cite its section, Division or Subdivision.
pub fn refs(connection: &Connection, row_id: &str) -> Result<Json, Exc> {
    let found = rows_of(
        connection,
        "SELECT * FROM rows WHERE row_id = ?",
        &[Sql::Text(row_id.into())],
    )?;
    let Some(row) = found.into_iter().next() else {
        return Err(refused(format!("{row_id} is not in the index")));
    };
    let mut outbound = Vec::new();
    for reference in rows_of(
        connection,
        "SELECT * FROM refs WHERE from_row_id = ? ORDER BY rowid",
        &[Sql::Text(row_id.into())],
    )? {
        let to = field(&reference, "to_register_id").clone();
        let kind = field(&reference, "kind")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let targets = match &to {
            Json::Null => Vec::new(),
            Json::Str(PyStr::Text(target)) => rows_of(
                connection,
                &target_sql(&kind),
                &[
                    Sql::Text(target.clone()),
                    Sql::Text(
                        field(&reference, "to_key")
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    ),
                ],
            )?
            .into_iter()
            .map(object)
            .collect(),
            _ => Vec::new(),
        };
        outbound.push(object(vec![
            ("kind".into(), field(&reference, "kind").clone()),
            ("key".into(), field(&reference, "to_key").clone()),
            ("citation".into(), field(&reference, "citation").clone()),
            ("cited_act".into(), field(&reference, "cited_act").clone()),
            ("register_id".into(), to),
            ("rows".into(), Json::Array(targets)),
        ]));
    }
    let sql_value = |value: &Json| match value {
        Json::Str(PyStr::Text(s)) => Sql::Text(s.clone()),
        Json::Int(i) => Sql::Integer(i64::try_from(i).unwrap_or_default()),
        _ => Sql::Null,
    };
    let inbound: Vec<Json> = rows_of(
        connection,
        "SELECT DISTINCT r.row_id, r.heading, f.kind, f.citation FROM refs f \
JOIN rows r ON r.row_id = f.from_row_id WHERE f.to_register_id = ? AND r.row_id != ? \
AND ((f.kind = 'section' AND f.to_key = ?) OR (f.kind = 'division' AND f.to_key = ?) \
OR (f.kind = 'subdivision' AND f.to_key = ?)) ORDER BY r.id",
        &[
            sql_value(field(&row, "register_id")),
            Sql::Text(row_id.into()),
            sql_value(field(&row, "section_key")),
            sql_value(field(&row, "division_key")),
            sql_value(field(&row, "subdivision_key")),
        ],
    )?
    .into_iter()
    .map(object)
    .collect();
    Ok(object(vec![
        ("row_id".into(), Json::str(row_id)),
        ("register_id".into(), field(&row, "register_id").clone()),
        ("section".into(), field(&row, "section").clone()),
        ("division".into(), field(&row, "division_key").clone()),
        ("subdivision".into(), field(&row, "subdivision_key").clone()),
        ("heading".into(), field(&row, "heading").clone()),
        ("cites".into(), Json::Array(outbound)),
        ("cited_by".into(), Json::Array(inbound)),
    ]))
}

/// What `cite-check` calls a citation, in the order its summary lists them.
const OUTCOMES: [&str; 7] = [
    "resolved",
    "assumed",
    "container_level",
    "ambiguous",
    "multi_section",
    "not_held",
    "superseded",
];
/// The 3 that let `cite-check` exit 0.
const CLEAN_OUTCOMES: [&str; 3] = ["resolved", "assumed", "container_level"];
/// What a quotation is graded, in the order the summary lists them. A quotation not
/// compared has no grade.
const GRADES: [&str; 4] = ["exact", "normalised", "partial", "none"];
/// The 2 that let `cite-check` exit 0.
const CLEAN_GRADES: [&str; 2] = ["exact", "normalised"];
/// The shortest quotation called `distinct`, in words. The share of word runs of each
/// length that only one row holds is in docs/build-evidence.md; this is the first length
/// at which 4 in 5 did on that build. It is a measurement of that build, not a guarantee.
const DISTINCT_FLOOR_WORDS: usize = 10;
/// Stands in for the register ID of a name two titles share, so the name still counts
/// as held.
const SHARED_NAME: &str = "";

/// A full stop, or the like, that ends a sentence, or a blank line. A stop after a listed
/// lower-case abbreviation does not, and anything unsure runs on: the wider scan is safer.
const SENTENCE_END: &str = r"(?<!\bcf)(?<!\bviz)(?<!\betc)(?<!\be\.g)(?<!\bi\.e)(?<!\breg)(?<!\bregs)[.!?](?=\s+[A-Z(]|\s*\Z)|\n\s*\n";

/// What may follow `this` and stay in the title.
const THIS_PROVISION: &str = "Act|Division|Subdivision|Part|Schedule|Chapter|division|subdivision|part|schedule|chapter|section|subsection";

/// Anything in a sentence that may name a source. `this` provisions and numbered
/// provisions are skipped first; then any capitalised word counts, as do `of the` and
/// `of this` phrases and the listed instrument words.
/// shortcut: the instrument words are a fixed list. What it misses stays `assumed`.
fn source_pattern() -> String {
    format!(
        r"\bthis\s+(?:{THIS_PROVISION})\b|\b(?:Sections?|Subsections?|Paragraphs?|Subparagraphs?|Divisions?|Subdivisions?|Parts?|Schedules?|Chapters?)\s+[0-9IVX][0-9A-Z-]*\b|(?P<source>(?<![A-Za-z0-9])[A-Z]|\bof\s+(?:the\s+[a-z]|that\b)|\b(?:of|in|under)\s+this\s+(?!(?:{THIS_PROVISION})\b)|(?<![A-Za-z0-9])(?:{SOURCE_WORDS})(?![A-Za-z0-9]))"
    )
}

/// The byte range of the sentence around a citation ending at `end`.
fn sentence_around(draft: &str, end: usize) -> (usize, usize) {
    let (mut start, mut stop) = (0, draft.len());
    for found in pyre::re(SENTENCE_END, 0).captures_iter(draft) {
        let boundary = found.get(0).expect("group 0");
        if boundary.end() <= end {
            start = boundary.end();
        } else {
            stop = boundary.start();
            break;
        }
    }
    (start, stop)
}

/// Whether the sentence around a citation ending at `end` may name a source, so that
/// `--title` cannot stand in for the Act. A sentence's own capitalised first word does
/// not count, but `ITAA 1936 section 8-1` opens with a name and does.
fn sentence_names_a_source(draft: &str, end: usize) -> bool {
    let (start, stop) = sentence_around(draft, end);
    let sentence = &draft[start..stop];
    let opening = pyre::re(r"\W*[A-Z]?[a-z]*\b", 0)
        .match_at(sentence, 0)
        .map_or(0, |found| found.get(0).expect("group 0").end());
    pyre::re(&source_pattern(), 0)
        .captures_iter(&sentence[opening..])
        .any(|found| found.name("source").is_some())
}

const CITE_CHECK_NOTICE: &str = "A resolved citation names its Act and a provision this build holds. An \
assumed citation names no Act in its sentence, so it shows only that --title holds the \
provision, not that the author meant that title. Neither shows \
that the provision says what the draft claims or applied at any date. A quotation is read \
only in quotation marks in the citation's sentence, and only against a resolved or assumed \
section. Exact and normalised show those words are in that row of this build, not that they \
are fairly quoted, authorised text or the law at any date. Distinct means no other row of this build holds at least 10 such words, and shows no more about any other source. Current and \
superseded are this build's record from the Register, not a check today; a later amendment can \
outdate them. Forms refs does \
not read, such as s 8-1, are not counted.";

/// The register ID `title` gives, refused unless the index holds it.
fn given_title(connection: &Connection, title: Option<&str>) -> Result<Option<String>, Exc> {
    let Some(t) = title.filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let rid = register_id_str(t)?;
    let sql = "SELECT 1 FROM titles WHERE register_id = ?";
    if rows_of(connection, sql, &[Sql::Text(rid.clone())])?.is_empty() {
        return Err(refused(format!("{rid} is not in the index")));
    }
    Ok(Some(rid))
}

/// Each title name and its register ID. Two titles sharing a name leave a citation of
/// that name ambiguous, not absent, so the name maps to `SHARED_NAME`.
fn title_names(connection: &Connection) -> Result<BTreeMap<String, String>, Exc> {
    let mut names = BTreeMap::new();
    for row in rows_of(connection, "SELECT name, register_id FROM titles", &[])? {
        let rid = field(&row, "register_id").as_str().unwrap_or_default();
        names
            .entry(field(&row, "name").as_str().unwrap_or_default().to_string())
            .and_modify(|slot: &mut String| *slot = SHARED_NAME.to_string())
            .or_insert_with(|| rid.to_string());
    }
    Ok(names)
}

/// The title a citation reaches and, when it cannot reach one, why that is `ambiguous`.
/// A bare citation reaches `own` only if nothing in its sentence may name a source. A
/// named title must end where the name does. A name that is not held has no reason.
fn citation_target(
    draft: &str,
    end: usize,
    act: Option<&str>,
    own: Option<&str>,
    names: &BTreeMap<String, String>,
) -> (Option<String>, Option<&'static str>) {
    let lookup = |name: &str| names.get(name).cloned();
    if act.is_some() {
        if !ends_name(draft, end, lookup) {
            return (None, None);
        }
    } else if own.is_none() {
        return (None, Some("no title is named"));
    } else if sentence_names_a_source(draft, end) {
        return (None, Some("its sentence may name another source"));
    }
    match cited_title(draft, end, act, own, lookup) {
        Some(rid) if rid == SHARED_NAME => (None, Some("2 titles share this name")),
        target => (target, None),
    }
}

/// A row of `rows_of`.
type Row = Vec<(String, Json)>;

/// The rows a citation reaches in a title, and that title's row.
fn cited_rows(
    connection: &Connection,
    rid: &str,
    kind: &str,
    key: &str,
) -> Result<(Vec<Row>, Option<Row>), Exc> {
    let title = Sql::Text(rid.to_string());
    let rows = rows_of(
        connection,
        &target_sql(kind),
        &[title.clone(), Sql::Text(key.to_string())],
    )?;
    let sql = "SELECT * FROM titles WHERE register_id = ?";
    let held = rows_of(connection, sql, &[title])?.into_iter().next();
    Ok((rows, held))
}

fn citation_outcome(
    in_range: bool,
    no_one_title: bool,
    rows: usize,
    current: bool,
    section: bool,
    bare: bool,
) -> &'static str {
    if in_range {
        // `citations` reads only the first item of a range.
        "multi_section"
    } else if no_one_title {
        "ambiguous"
    } else if rows == 0 {
        "not_held"
    } else if !current {
        "superseded"
    } else if !section && !bare {
        "container_level"
    } else if section && rows > 1 {
        "ambiguous"
    } else if bare {
        "assumed"
    } else {
        "resolved"
    }
}

/// The columns of the cited title that pin a result to the build.
const TITLE_FIELDS: [&str; 4] = [
    "compilation_number",
    "compilation_date",
    "retrieved",
    "sections_sha256",
];

/// One citation found in a draft: kind, key, the Act it names, its text and where it ends.
type FoundCitation = (String, String, Option<String>, String, usize);

/// How many rows other than `row_id` hold all of `quote`. FTS finds the rows with its
/// words in order, reading the NFKC quotation with the tokeniser that read each row, so a
/// row that holds it is never missed. `quotes::grade` then applies the same test as the
/// row reached.
fn other_rows(connection: &Connection, quote: &str, row_id: &str) -> Result<usize, Exc> {
    let sql = "SELECT r.row_id, r.text FROM rows_fts JOIN rows r ON r.id = rows_fts.rowid \
               WHERE rows_fts MATCH ?";
    let phrase = format!("text : \"{}\"", normalise(quote).replace('"', "\"\""));
    let found = rows_of(connection, sql, &[Sql::Text(phrase)])?;
    let other = |row: &&Row| {
        field(row, "row_id").as_str() != Some(row_id)
            && quotes::grade(quote, field(row, "text").as_str().unwrap_or_default()).held()
    };
    Ok(found.iter().filter(other).count())
}

/// Why a quotation was not compared, or `None` if it can be.
fn not_compared(quotation: &Quotation, row: Option<&Json>, end: usize) -> Option<&'static str> {
    if !quotation.closed {
        Some("it has no closing mark")
    } else if quotation.surrounds(end) {
        Some("the citation is inside this quotation")
    } else if row.is_none() {
        Some("the citation reached no one section")
    } else {
        None
    }
}

/// The report object for one quotation beside a citation ending at `end`. `row` is the
/// row the citation reached. A quotation not compared has no grade and a reason.
fn quotation_report(
    connection: &Connection,
    quotation: &Quotation,
    row: Option<&Json>,
    end: usize,
) -> Result<Json, Exc> {
    let mut report = vec![
        ("quote".into(), Json::str(&quotation.text)),
        ("start".into(), Json::Int(quotation.start.into())),
    ];
    let count = |n: Option<usize>| n.map_or(Json::Null, |n| Json::Int(n.into()));
    let reason = not_compared(quotation, row, end);
    let (mut grade, mut matched, mut words, mut others, mut distinct) =
        (Json::Null, None, None, None, Json::Null);
    if let (None, Some(row)) = (reason, row) {
        let row_id = row.as_str().unwrap_or_default();
        let sql = "SELECT text FROM rows WHERE row_id = ?";
        let held = rows_of(connection, sql, &[Sql::Text(row_id.to_string())])?;
        let text = held.first().and_then(|r| field(r, "text").as_str());
        let found = quotes::grade(&quotation.text, text.unwrap_or_default());
        grade = Json::str(found.grade);
        (matched, words) = (found.matched_words, Some(found.quote_words));
        if found.held() {
            let others_found = other_rows(connection, &quotation.text, row_id)?;
            others = Some(others_found);
            let long_enough = found.quote_words >= DISTINCT_FLOOR_WORDS;
            distinct = Json::Bool(others_found == 0 && long_enough);
        }
    }
    report.extend([
        ("grade".into(), grade),
        ("reason".into(), text_or_null(reason.map(str::to_string))),
        ("matched_words".into(), count(matched)),
        ("quote_words".into(), count(words)),
        ("other_rows".into(), count(others)),
        ("distinct".into(), distinct),
    ]);
    Ok(object(report))
}

/// A draft's quotations, and each citation's end with the sentence around it.
struct DraftQuotes {
    all: Vec<Quotation>,
    citations: Vec<(usize, (usize, usize))>,
}

/// Each quotation in the sentence around a citation ending at `end`, compared with `row`
/// if there is one. A citation inside a quotation is what is quoted, not its source, so
/// it does not answer for that quotation if another citation beside it can; if none can,
/// the quotation is listed uncompared rather than lost.
fn sentence_quotations(
    connection: &Connection,
    draft: &str,
    end: usize,
    quotes: &DraftQuotes,
    row: Option<&Json>,
) -> Result<Json, Exc> {
    if quotes.all.is_empty() {
        return Ok(Json::Array(Vec::new()));
    }
    let sentence = sentence_around(draft, end);
    let answered = |q: &Quotation| {
        let outside = |c: &&(usize, (usize, usize))| !q.surrounds(c.0) && q.in_sentence(c.1);
        quotes.citations.iter().find(outside).is_some()
    };
    let near = quotes.all.iter().filter(|q| q.in_sentence(sentence));
    let reports = near
        .filter(|q| !(q.surrounds(end) && answered(q)))
        .map(|q| quotation_report(connection, q, row, end));
    Ok(Json::Array(reports.collect::<Result<_, _>>()?))
}

/// The report object for one citation: the outcome, then the rows and build behind it.
fn citation_report(
    connection: &Connection,
    draft: &str,
    item: FoundCitation,
    own: Option<&str>,
    names: &BTreeMap<String, String>,
    digest: &Json,
    quotes: &DraftQuotes,
) -> Result<Json, Exc> {
    let (kind, key, act, citation, end) = item;
    let (target, reason) = citation_target(draft, end, act.as_deref(), own, names);
    let (rows, held) = match &target {
        Some(rid) => cited_rows(connection, rid, &kind, &key)?,
        None => (Vec::new(), None),
    };
    let from_title = |name: &str| held.as_ref().map_or(Json::Null, |t| field(t, name).clone());
    let range = pyre::re(&format!(r"\s+to\s+(?:{LABEL})"), 0);
    let outcome = citation_outcome(
        range.match_at(draft, end).is_some(),
        reason.is_some(),
        rows.len(),
        from_title("version_is_current").truthy(),
        kind == "section",
        act.is_none(),
    );
    let newlines = draft.as_bytes()[..end].iter().filter(|b| **b == b'\n');
    let reason = reason.or((outcome == "ambiguous").then_some("several rows share this number"));
    let row_ids: Vec<Json> = rows.iter().map(|r| field(r, "row_id").clone()).collect();
    let graded = matches!(outcome, "resolved" | "assumed") && kind == "section";
    let row = row_ids.first().filter(|_| graded);
    let quotations = sentence_quotations(connection, draft, end, quotes, row)?;
    let mut fields = vec![
        ("line".into(), Json::Int((newlines.count() + 1).into())),
        ("citation".into(), Json::str(&citation)),
        ("kind".into(), Json::str(&kind)),
        ("key".into(), Json::str(&key)),
        ("cited_act".into(), text_or_null(act)),
        ("outcome".into(), Json::str(outcome)),
        ("reason".into(), text_or_null(reason.map(str::to_string))),
        ("register_id".into(), text_or_null(target)),
        ("rows".into(), Json::Array(row_ids)),
        ("quotations".into(), quotations),
    ];
    for name in TITLE_FIELDS {
        fields.push((name.into(), from_title(name)));
    }
    fields.push(("sources_sha256".into(), digest.clone()));
    Ok(object(fields))
}

/// One object per citation in `draft`, in its order, with the outcome and the rows and
/// build it resolved against. `title` is what a citation naming no title means; without
/// it that citation is `ambiguous`.
pub fn cite_check(
    connection: &Connection,
    draft: &str,
    title: Option<&str>,
) -> Result<Vec<Json>, Exc> {
    let own = given_title(connection, title)?;
    let names = title_names(connection)?;
    let sql = "SELECT value FROM meta WHERE key = 'sources_sha256'";
    let digest = rows_of(connection, sql, &[])?
        .first()
        .map_or(Json::Null, |row| field(row, "value").clone());
    let found = citations(draft);
    let all = quotes::quotations(draft);
    let spans = |item: &FoundCitation| (item.4, sentence_around(draft, item.4));
    let spans = found.iter().filter(|_| !all.is_empty()).map(spans);
    let draft_quotes = DraftQuotes {
        citations: spans.collect(),
        all,
    };
    found
        .into_iter()
        .map(|item| {
            let own = own.as_deref();
            citation_report(connection, draft, item, own, &names, &digest, &draft_quotes)
        })
        .collect()
}

fn source_line(connection: &Connection) -> Result<String, Exc> {
    let meta = rows_of(connection, "SELECT key, value FROM meta", &[])?;
    let lookup = |key: &str| -> Option<Json> {
        meta.iter()
            .rev()
            .find(|row| field(row, "key").as_str() == Some(key))
            .map(|row| field(row, "value").clone())
    };
    let retrieved =
        lookup("corpus_retrieved").map_or_else(|| "None".to_string(), |v| pyvalue::py_str(&v));
    let digest = lookup("sources_sha256").map_or_else(String::new, |v| pyvalue::py_str(&v));
    let short: String = digest.chars().take(16).collect();
    Ok(format!(
        "Index of the corpus retrieved {retrieved}, sources.json sha256 {short}. {NOTICE}"
    ))
}

fn describe(hit: &Json) -> String {
    let s = |k: &str| pyvalue::py_str(hit.get(k).unwrap_or(&Json::Null));
    let state = if hit.get("version_is_current").is_some_and(Json::truthy) {
        "current"
    } else {
        "not current"
    };
    format!(
        "{}, compilation {} ({}), {state}, retrieved {}\n   {}",
        s("row_id"),
        s("compilation_number"),
        s("compilation_date"),
        s("retrieved"),
        s("register_page")
    )
}

const AS_AT_HELP: &str =
    "answer only from titles the build holds for this YYYY-MM-DD; refuse the others";

fn parser() -> Parser {
    Parser::new("python -m fadden search")
        .description(
            "Build and query a local full-text, definitions and reference index. \
             search cite-check INDEX DRAFT checks a draft's citations against one.",
        )
        .subcommands("command")
        .sub("build", Some("index a finished corpus root"), |p| {
            p.arg(Action::positional("corpus_root")).arg(
                Action::option("--out")
                    .required()
                    .help("new index file; must not exist"),
            )
        })
        .sub(
            "query",
            Some("rank rows that contain every word"),
            query_arguments,
        )
        .sub("define", Some("look up a defined term"), |p| {
            p.arg(Action::positional("index"))
                .arg(Action::positional("term").one_or_more())
                .arg(Action::option("--title").help("look only in this register id"))
                .arg(Action::option("--as-at").metavar("DATE").help(AS_AT_HELP))
                .arg(Action::flag("--json"))
        })
        .sub(
            "refs",
            Some("sections a row cites and rows citing it"),
            |p| {
                p.arg(Action::positional("index"))
                    .arg(Action::positional("row_id"))
                    .arg(Action::flag("--json"))
            },
        )
}

fn query_arguments(parser: Parser) -> Parser {
    let limits: Vec<String> = (1..=100).map(|n| n.to_string()).collect();
    let limits: Vec<&str> = limits.iter().map(String::as_str).collect();
    parser
        .arg(Action::positional("index"))
        .arg(Action::positional("words").one_or_more())
        .arg(Action::option("--title").help("search only this register id"))
        .arg(Action::option("--as-at").metavar("DATE").help(AS_AT_HELP))
        .arg(Action::flag("--stem").help("match English word forms; not quotation matching"))
        .arg(
            Action::option("--limit")
                .int()
                .default(crate::argparse::Value::Int(10))
                .choices(&limits)
                .metavar("1-100"),
        )
        .arg(Action::flag("--json").help("print one object per hit"))
}

/// Apart from `parser`, whose usage line is frozen by the Python parity fixtures.
fn cite_check_parser() -> Parser {
    Parser::new("python -m fadden search cite-check")
        .description("Report how each citation in a UTF-8 draft resolves against an index, and how any quotation in its sentence compares with the row reached.")
        .arg(Action::positional("index"))
        .arg(Action::positional("draft"))
        .arg(Action::option("--title").help("register id to check a citation naming no Act against; it is reported as assumed, never resolved"))
        .arg(
            Action::option("--as-at")
                .metavar("DATE")
                .help("report each cited title's status on this YYYY-MM-DD; any status but captured exits 1"),
        )
        .arg(Action::flag("--json").help("print one object per citation"))
}

fn list(args: &crate::argparse::Namespace, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(crate::argparse::Value::List(items)) => items.clone(),
        _ => Vec::new(),
    }
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let outcome = if argv.first().is_some_and(|word| word == "cite-check") {
        execute_cite_check(&cite_check_parser().parse(&argv[1..])?, run)
    } else {
        execute(&parser().parse(argv)?, run)
    };
    Ok(match outcome {
        Ok(code) => code,
        // `except (SearchIndexError, ValueError, OSError, sqlite3.Error)`.
        Err(error @ (Exc::Value { .. } | Exc::UnicodeEncode(_) | Exc::Os(_) | Exc::Sqlite(_))) => {
            run.eprint(&format!("error: {error}"));
            1
        }
        Err(error) => run.uncaught(&error),
    })
}

/// Up to 5 row ids, then how many more.
fn shown_rows(rows: &[Json]) -> String {
    let mut shown: Vec<String> = rows
        .iter()
        .take(5)
        .map(|r| pyvalue::py_str(r.at("row_id")))
        .collect();
    if rows.len() > 5 {
        shown.push(format!("and {} more", rows.len() - 5));
    }
    shown.join(", ")
}

fn outcome_of(item: &Json) -> String {
    pyvalue::py_str(item.at("outcome"))
}

/// A citation's quotations.
fn quotations_of(item: &Json) -> &[Json] {
    match item.at("quotations") {
        Json::Array(found) => found,
        _ => &[],
    }
}

fn print_quotation(quotation: &Json, run: &mut Run) {
    let s = |key: &str| pyvalue::py_str(quotation.at(key));
    let quote = crate::py::split_whitespace(&s("quote")).join(" ");
    let line = match quotation.at("grade").as_str() {
        Some(grade @ ("exact" | "normalised")) => {
            let distinct = if quotation.at("distinct").truthy() {
                "distinct"
            } else {
                "not distinct"
            };
            format!(
                "  quotation {grade}, {distinct}, {} other rows hold it: {quote}",
                s("other_rows")
            )
        }
        Some("partial") => format!(
            "  quotation partial, longest run {} of {} words: {quote}",
            s("matched_words"),
            s("quote_words")
        ),
        Some(grade) => format!("  quotation {grade}: {quote}"),
        None => format!("  quotation not compared, {}: {quote}", s("reason")),
    };
    run.print(&line);
}

fn read_draft(path: &str) -> Result<String, Exc> {
    let unreadable = |e: OsError| refused(format!("cannot read the draft ({})", strerror(&e)));
    let bytes = read_all(crate::pyio::open(path).map_err(unreadable)?, path).map_err(unreadable)?;
    String::from_utf8(bytes).map_err(|_| refused("the draft is not UTF-8 text"))
}

/// A citation's standing on the date, when it has one.
fn standing_line(standing: &Json) -> Option<String> {
    let s = |key: &str| pyvalue::py_str(standing.at(key));
    if matches!(standing, Json::Null) {
        return None;
    }
    let next = match standing.at("next_step") {
        Json::Null => String::new(),
        step => format!(": {}", pyvalue::py_str(step)),
    };
    Some(format!("  as at {}: {}{next}", s("date"), s("status")))
}

/// The quotation totals: each quotation once, and each comparison with a citation.
fn print_quotation_totals(report: &[Json], run: &mut Run) {
    let pairs: Vec<&Json> = report.iter().flat_map(quotations_of).collect();
    if pairs.is_empty() {
        return;
    }
    let count = |grade: Option<&str>| {
        pairs
            .iter()
            .filter(|q| q.at("grade").as_str() == grade)
            .count()
    };
    let mut parts: Vec<String> = GRADES
        .iter()
        .map(|grade| format!("{} {grade}", count(Some(grade))))
        .collect();
    parts.push(format!("{} not compared", count(None)));
    parts.retain(|part| !part.starts_with("0 "));
    let quotes: BTreeSet<String> = pairs
        .iter()
        .map(|q| pyvalue::py_str(q.at("start")))
        .collect();
    run.print(&format!(
        "{} quotations, {} comparisons with citations: {}.",
        quotes.len(),
        pairs.len(),
        parts.join(", ")
    ));
}

fn print_citations(report: &[Json], connection: &Connection, run: &mut Run) -> Result<(), Exc> {
    for item in report {
        let s = |k: &str| pyvalue::py_str(item.at(k));
        let why = item.at("reason").as_str().unwrap_or_default();
        let rows = match item.at("rows") {
            Json::Array(ids) => ids
                .iter()
                .map(|id| object(vec![("row_id".into(), id.clone())]))
                .collect(),
            _ => Vec::new(),
        };
        run.print(&format!(
            "line {}: {}: {}: {}",
            s("line"),
            s("outcome"),
            s("citation"),
            format!("{} {why}", shown_rows(&rows)).trim()
        ));
        if let Some(line) = standing_line(item.at("as_at")) {
            run.print(&line);
        }
        for quotation in quotations_of(item) {
            print_quotation(quotation, run);
        }
    }
    print_quotation_totals(report, run);
    let counted: Vec<String> = OUTCOMES
        .iter()
        .map(|name| {
            let count = report.iter().filter(|i| outcome_of(i) == *name).count();
            format!("{count} {name}")
        })
        .filter(|part| !part.starts_with("0 "))
        .collect();
    run.print(&format!(
        "{} citations: {}. {CITE_CHECK_NOTICE}",
        report.len(),
        counted.join(", ")
    ));
    if report.iter().any(|item| item.get("as_at").is_some()) {
        run.print(as_at::CAPTURED_MEANS);
    }
    run.print(&source_line(connection)?);
    Ok(())
}

fn execute_cite_check(args: &crate::argparse::Namespace, run: &mut Run) -> Result<i32, Exc> {
    let connection = open_index(&args.str("index").unwrap_or_default())?;
    let draft = read_draft(&args.str("draft").unwrap_or_default())?;
    let title = args.str("title");
    let (report, refusals) = match args.str("as_at") {
        Some(date) => cite_check_as_at(&connection, &draft, title.as_deref(), &date)?,
        None => (
            cite_check(&connection, &draft, title.as_deref())?,
            Vec::new(),
        ),
    };
    if args.flag("json") {
        for item in &report {
            run.print_points(&dumps_line(item, false))?;
        }
    } else {
        print_citations(&report, &connection, run)?;
    }
    print_refusals(&refusals, args.flag("json"), run)?;
    let clean = report.iter().all(|item| {
        let standing = item.at("as_at");
        let unheld = !matches!(standing, Json::Null)
            && standing.at("status").as_str() != Some(as_at::CAPTURED);
        !unheld
            && CLEAN_OUTCOMES.contains(&outcome_of(item).as_str())
            && quotations_of(item).iter().all(|quotation| {
                let grade = quotation.at("grade").as_str();
                grade.is_some_and(|grade| CLEAN_GRADES.contains(&grade))
            })
    });
    Ok(i32::from(!clean || !refusals.is_empty()))
}

fn print_refusals(refusals: &[Json], json: bool, run: &mut Run) -> Result<(), Exc> {
    for refusal in refusals {
        if json {
            run.print_points(&dumps_line(refusal, false))?;
            continue;
        }
        let s = |key: &str| pyvalue::py_str(refusal.at(key));
        let standing = refusal.at("as_at");
        run.print(&format!(
            "refused {} ({}): {} on {}, {} matching rows withheld: {}",
            s("register_id"),
            s("act"),
            pyvalue::py_str(standing.at("status")),
            pyvalue::py_str(standing.at("date")),
            s("matching_rows"),
            pyvalue::py_str(standing.at("next_step"))
        ));
    }
    Ok(())
}

/// Prints a dated report's refusals and summary.
fn print_dated(report: &DatedReport, json: bool, run: &mut Run) -> Result<(), Exc> {
    print_refusals(&report.refusals, json, run)?;
    if json {
        return run.print_points(&dumps_line(&report.summary, false));
    }
    let summary = report.summary.at("as_at_summary");
    let titles = as_at::STATUSES.map(|status| {
        format!(
            "{} {status}",
            pyvalue::py_str(summary.at("titles").at(status))
        )
    });
    run.print(&format!(
        "Titles against {}: {}. {}",
        pyvalue::py_str(summary.at("date")),
        titles.join(", "),
        as_at::CAPTURED_MEANS
    ));
    Ok(())
}

/// Exit 1 if a dated report refused a title, so no caller reads a refusal as no rows.
fn refused_code(dated: Option<&DatedReport>) -> i32 {
    i32::from(dated.is_some_and(|report| !report.refusals.is_empty()))
}

fn query_results(
    args: &crate::argparse::Namespace,
    connection: &Connection,
) -> Result<(Vec<Json>, Option<DatedReport>), Exc> {
    let title = args.str("title");
    let limit = match args.get("limit") {
        Some(crate::argparse::Value::Int(n)) => *n,
        _ => 10,
    };
    let words = list(args, "words");
    let stemmed = args.flag("stem");
    let dated = args
        .str("as_at")
        .map(|date| {
            let query = if stemmed {
                query_stemmed_as_at
            } else {
                query_as_at
            };
            query(connection, &words, title.as_deref(), limit, &date)
        })
        .transpose()?;
    let hits = match &dated {
        Some(report) => report.hits.clone(),
        None => {
            let query = if stemmed { query_stemmed } else { query };
            query(connection, &words, title.as_deref(), limit)?
        }
    };
    Ok((hits, dated))
}

fn execute_query(
    args: &crate::argparse::Namespace,
    connection: &Connection,
    run: &mut Run,
) -> Result<i32, Exc> {
    let (hits, dated) = query_results(args, connection)?;
    if args.flag("json") {
        for hit in &hits {
            run.print_points(&dumps_line(hit, false))?;
        }
        if let Some(report) = &dated {
            print_dated(report, true, run)?;
        }
        return Ok(refused_code(dated.as_ref()));
    }
    if args.flag("stem") {
        run.print(&format!("Stemmed search. {STEM_NOTICE}"));
    }
    for (number, hit) in hits.iter().enumerate() {
        let s = |k: &str| pyvalue::py_str(hit.get(k).unwrap_or(&Json::Null));
        run.print(&format!("{}. {}: {}", number + 1, s("act"), s("heading")));
        run.print(&format!("   {}", describe(hit)));
        run.print(&format!(
            "   {}",
            crate::py::split_whitespace(&s("snippet")).join(" ")
        ));
    }
    if let Some(report) = &dated {
        print_dated(report, false, run)?;
    }
    run.print(&format!(
        "{} rows. {}",
        hits.len(),
        source_line(connection)?
    ));
    Ok(refused_code(dated.as_ref()))
}

fn execute_define(
    args: &crate::argparse::Namespace,
    connection: &Connection,
    run: &mut Run,
) -> Result<i32, Exc> {
    let title = args.str("title");
    let term = list(args, "term").join(" ");
    let dated = args
        .str("as_at")
        .map(|date| define_as_at(connection, &term, title.as_deref(), &date))
        .transpose()?;
    let found = match &dated {
        Some(report) => report.hits.clone(),
        None => define(connection, &term, title.as_deref())?,
    };
    if args.flag("json") {
        for item in &found {
            run.print_points(&dumps_line(item, false))?;
        }
        if let Some(report) = &dated {
            print_dated(report, true, run)?;
        }
        return Ok(refused_code(dated.as_ref()));
    }
    for item in &found {
        let s = |k: &str| pyvalue::py_str(item.get(k).unwrap_or(&Json::Null));
        run.print(&format!("{} ({}, {})", s("term"), s("act"), s("heading")));
        run.print(&format!("   {}", describe(item)));
        run.print(&format!("   {}", s("definition").replace("\n\n", "\n   ")));
    }
    if found.is_empty() && refused_code(dated.as_ref()) == 0 {
        run.print(
            "No definition paragraph found. The term may be defined another way or in a row this \
             index does not read as definitions; this is not a finding that it has its ordinary meaning.",
        );
    }
    if let Some(report) = &dated {
        print_dated(report, false, run)?;
    }
    run.print(&source_line(connection)?);
    Ok(refused_code(dated.as_ref()))
}

fn describe_reference(reference: &Json) -> String {
    let s = |v: &Json| pyvalue::py_str(v);
    let rows = match reference.at("rows") {
        Json::Array(rows) => rows.clone(),
        _ => Vec::new(),
    };
    let joined = shown_rows(&rows);
    let place = if !joined.is_empty() {
        joined
    } else if matches!(reference.at("register_id"), Json::Null) {
        "not in this index".into()
    } else {
        format!("no row for that {}", s(reference.at("kind")))
    };
    format!(
        "  cites {} {} ({}): {place}",
        s(reference.at("kind")),
        s(reference.at("key")),
        s(reference.at("citation"))
    )
}

fn execute_refs(
    args: &crate::argparse::Namespace,
    connection: &Connection,
    run: &mut Run,
) -> Result<i32, Exc> {
    let row_id = args.str("row_id").unwrap_or_default();
    let report = refs(connection, &row_id)?;
    if args.flag("json") {
        run.print_points(&dumps_line(&report, false))?;
        return Ok(0);
    }
    let s = |v: &Json| pyvalue::py_str(v);
    run.print(&format!(
        "{}: {}",
        s(report.at("row_id")),
        s(report.at("heading"))
    ));
    if let Json::Array(cites) = report.at("cites") {
        for reference in cites {
            run.print(&describe_reference(reference));
        }
    }
    if let Json::Array(sources) = report.at("cited_by") {
        for source in sources {
            run.print(&format!(
                "  cited by {} ({}): {}",
                s(source.at("row_id")),
                s(source.at("heading")),
                s(source.at("citation"))
            ));
        }
    }
    run.print(&source_line(connection)?);
    Ok(0)
}

fn execute(args: &crate::argparse::Namespace, run: &mut Run) -> Result<i32, Exc> {
    let command = args.str("command").unwrap_or_default();
    if command == "build" {
        let [titles, rows, definitions, refs] = build(
            &args.str("corpus_root").unwrap_or_default(),
            &args.str("out").unwrap_or_default(),
        )?;
        run.print(&format!(
            "indexed {titles} titles, {rows} rows, {definitions} definitions and {refs} section references"
        ));
        return Ok(0);
    }
    let connection = open_index(&args.str("index").unwrap_or_default())?;
    match command.as_str() {
        "query" => execute_query(args, &connection, run),
        "define" => execute_define(args, &connection, run),
        _ => execute_refs(args, &connection, run),
    }
}
