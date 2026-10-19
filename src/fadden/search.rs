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
use std::collections::BTreeSet;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags, params_from_iter};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::corpus_paths::{child, register_id, register_id_str};
use super::{Exc, Run};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, Json, PyStr};
use crate::pyos::{OsError, Style};
use crate::pypath::PyPath;
use crate::pyre::{self, S as DOTALL};
use crate::pyvalue::{self, dumps_line, py_eq, repr, str_for_stderr, type_name};

const SCHEMA: &str = "2";
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

fn cited() -> String {
    format!(
        r"(?:the\s+)?\*?([A-Z][A-Za-z]*(?:\s+[A-Z][A-Za-z]*){{0,3}}\s+{YEAR}|(?=[A-Z])[^.;:,\n]{{0,150}}?\b(?:Act|Regulations?|Rules)\b(?:\s+{YEAR})?)"
    )
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

/// (kind, key, cited act or None, citation text) for each reference in `text`.
pub fn citations(text: &str) -> Vec<(String, String, Option<String>, String)> {
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
        let citation =
            crate::py::split_whitespace(found.get(0).expect("group 0").as_str()).join(" ");
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
                out.push((kind.clone(), key, act.clone(), citation.clone()));
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
fn percent_s(value: &Json) -> String {
    pyvalue::py_str(value)
}

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
            Err(_) => return Err(refused(format!("{place} is not JSON"))),
        };
        if !matches!(row.get("text"), Some(Json::Str(_))) {
            return Err(refused(format!("{place} is not a sections.jsonl row")));
        }
        let ordinal = rows.len() + 1;
        let section = get(&row, "section");
        let shown = if section.truthy() {
            percent_s(section)
        } else {
            "-".into()
        };
        let expected = format!("{rid}:{ordinal:04}:{shown}");
        if !py_eq(get(&row, "register_id"), &Json::str(&rid)) {
            return Err(refused(format!("{place} names another title")));
        }
        if !py_eq(get(&row, "row_id"), &Json::str(&expected)) {
            return Err(refused(format!(
                "{place} has row_id {}, expected {}",
                repr(get(&row, "row_id")),
                crate::py::repr_str(&expected)
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

/// `json.loads(bytes)`: the encoding `json.detect_encoding` picks, decoded with
/// `surrogatepass`; None where Python raises.
fn json_bytes(data: &[u8]) -> Option<Json> {
    let points: Vec<u32> =
        if data.starts_with(&[0, 0, 0xfe, 0xff]) || data.starts_with(&[0xff, 0xfe, 0, 0]) {
            utf32(data, None)?
        } else if data.starts_with(&[0xfe, 0xff]) || data.starts_with(&[0xff, 0xfe]) {
            utf16(data, None)?
        } else if let Some(rest) = data.strip_prefix(b"\xef\xbb\xbf") {
            utf8_surrogatepass(rest)?
        } else if data.len() >= 4 && data[0] == 0 {
            if data[1] != 0 {
                utf16(data, Some(true))?
            } else {
                utf32(data, Some(true))?
            }
        } else if data.len() >= 4 && data[1] == 0 {
            if data[2] != 0 || data[3] != 0 {
                utf16(data, Some(false))?
            } else {
                utf32(data, Some(false))?
            }
        } else if data.len() == 2 && data[0] == 0 {
            utf16(data, Some(true))?
        } else if data.len() == 2 && data[1] == 0 {
            utf16(data, Some(false))?
        } else {
            utf8_surrogatepass(data)?
        };
    // A raw lone surrogate becomes an escape, which the scanner turns back into one.
    let mut text = String::with_capacity(points.len());
    for cp in points {
        match char::from_u32(cp) {
            Some(c) => text.push(c),
            None => text.push_str(&format!("\\u{cp:04x}")),
        }
    }
    pyjson::loads_standard(&text).ok()
}

fn utf8_surrogatepass(data: &[u8]) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        // An encoded surrogate (ED A0..BF xx) is accepted as a lone code point.
        if data[i] == 0xed
            && i + 2 < data.len()
            && (0xa0..=0xbf).contains(&data[i + 1])
            && (0x80..=0xbf).contains(&data[i + 2])
        {
            out.push(0xd000 | (u32::from(data[i + 1] & 0x3f) << 6) | u32::from(data[i + 2] & 0x3f));
            i += 3;
            continue;
        }
        let end = (i + 4).min(data.len());
        let chunk = match std::str::from_utf8(&data[i..end]) {
            Ok(s) => s,
            Err(e) if e.valid_up_to() > 0 => {
                std::str::from_utf8(&data[i..i + e.valid_up_to()]).ok()?
            }
            Err(_) => return None,
        };
        let c = chunk.chars().next()?;
        out.push(u32::from(c));
        i += c.len_utf8();
    }
    Some(out)
}

fn utf16(data: &[u8], big: Option<bool>) -> Option<Vec<u32>> {
    let (big, body) = match big {
        Some(b) => (b, data),
        None => (data[0] == 0xfe, &data[2..]),
    };
    if body.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = body
        .chunks(2)
        .map(|c| {
            if big {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = u32::from(units[i]);
        if (0xd800..0xdc00).contains(&u)
            && i + 1 < units.len()
            && (0xdc00..0xe000).contains(&u32::from(units[i + 1]))
        {
            out.push(0x10000 + ((u - 0xd800) << 10) + (u32::from(units[i + 1]) - 0xdc00));
            i += 2;
        } else {
            out.push(u);
            i += 1;
        }
    }
    Some(out)
}

fn utf32(data: &[u8], big: Option<bool>) -> Option<Vec<u32>> {
    let (big, body) = match big {
        Some(b) => (b, data),
        None => (data[0] == 0, &data[4..]),
    };
    if body.len() % 4 != 0 {
        return None;
    }
    body.chunks(4)
        .map(|c| {
            let v = if big {
                u32::from_be_bytes([c[0], c[1], c[2], c[3]])
            } else {
                u32::from_le_bytes([c[0], c[1], c[2], c[3]])
            };
            (v <= 0x10ffff).then_some(v)
        })
        .collect()
}

fn read_corpus(root: &str) -> Result<(Vec<u8>, Json, Vec<Title>), Exc> {
    let path = child(root, &["sources.json"])?;
    let sources_bytes = crate::pyio::open(&path)
        .and_then(|file| read_all(file, &path))
        .map_err(|e| refused(format!("cannot read sources.json ({})", strerror(&e))))?;
    let not_finished = || refused("sources.json is not a finished corpus inventory");
    let sources = json_bytes(&sources_bytes).ok_or_else(not_finished)?;
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
    refuse_unlisted_titles(root, &ids)?;
    let read = listed
        .iter()
        .map(|title| read_title(root, title))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sources_bytes, sources, read))
}

/// `refuse_unlisted_titles(root, listed)`.
fn refuse_unlisted_titles(root: &str, listed: &BTreeSet<String>) -> Result<(), Exc> {
    let base = child(root, &["markdown"])?;
    if !crate::pyos::isdir(&base) {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(&base).map_err(|e| OsError::new(&e, Some(&base), Style::Native))?;
    let mut stale = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| OsError::new(&e, Some(&base), Style::Native))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && !listed.contains(&name) {
            stale.push(name);
        }
    }
    stale.sort();
    if stale.is_empty() {
        return Ok(());
    }
    Err(refused(format!(
        "markdown/ holds entries this build does not list, which would stay searchable as in force: {}. \
         Move them out of the corpus and re-run; nothing was written or deleted.",
        stale.join(", ")
    )))
}

const SCHEMA_SQL: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE titles (
  register_id TEXT PRIMARY KEY, name TEXT NOT NULL, collection TEXT,
  compilation_number TEXT, compilation_date TEXT, version_is_current INTEGER NOT NULL,
  retrieved TEXT, register_page TEXT, source_url TEXT, rows INTEGER NOT NULL,
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

/// A strict UTF-8 encoding error for code points, as `str.encode()` words it.
fn utf8_strict_error(points: &[u32]) -> Option<String> {
    let start = points.iter().position(|&cp| char::from_u32(cp).is_none())?;
    let end = (start..points.len())
        .find(|&j| char::from_u32(points[j]).is_some())
        .unwrap_or(points.len());
    Some(if end - start == 1 {
        format!(
            "'utf-8' codec can't encode character '\\u{:04x}' in position {start}: surrogates not allowed",
            points[start]
        )
    } else {
        format!(
            "'utf-8' codec can't encode characters in position {start}-{}: surrogates not allowed",
            end - 1
        )
    })
}

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
                utf8_strict_error(points).unwrap_or_default(),
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
        Json::Str(PyStr::Lone(points)) => Err(Exc::value(format!(
            "invalid literal for int() with base 10: {}",
            pyvalue::repr_points(points)
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
    let mut nan_id = 0;
    for entry in &read {
        let name = entry
            .title
            .get("name")
            .ok_or_else(|| Exc::Key("'name'".into()))?;
        let key = pyvalue::key(name, &mut nan_id).map_err(|m| {
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
                get(title, "register_page").clone(),
                get(title, "source_url").clone(),
                Json::Int(entry.rows.len().into()),
                Json::str(&entry.sections_sha256),
                Json::str(&entry.markdown_sha256),
            ];
            connection
                .execute(
                    "INSERT INTO titles VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
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
            connection
                .execute(
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
    connection
        .execute(
            "INSERT INTO rows (row_id, register_id, section, section_key, division_key, subdivision_key, heading, container, text) VALUES (?,?,?,?,?,?,?,?,?)",
            params_from_iter(bind_all(&values)?),
        )
        .map_err(|e| sqlite_error(&e))?;
    let id = connection.last_insert_rowid();
    let heading = normalise_value(&or_empty(get(row, "heading")))?;
    connection
        .execute(
            "INSERT INTO rows_fts (rowid, heading, text) VALUES (?,?,?)",
            rusqlite::params![id, heading, normalise(&text)],
        )
        .map_err(|e| sqlite_error(&e))?;
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
            connection
                .execute(
                    "INSERT INTO definitions VALUES (?,?,?,?,?)",
                    rusqlite::params![term, term_key(&term), row_id, position as i64 + 1, body],
                )
                .map_err(|e| sqlite_error(&e))?;
            counts.definitions += 1;
        }
    }
    for (kind, cited, act, citation) in citations(&text) {
        let target = match &act {
            None => Some(rid.to_string()),
            Some(name) => names
                .iter()
                .find(|(k, _)| *k == pyvalue::Key::Str(name.chars().map(u32::from).collect()))
                .and_then(|(_, v)| v.clone()),
        };
        connection
            .execute(
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
    let columns: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
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
ORDER BY r.section_key IN (SELECT value FROM json_each(?)) DESC, \
bm25(rows_fts, 4.0, 1.0), r.id LIMIT ?";
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
        Sql::Text(sections_json),
        Sql::Integer(limit),
    ];
    Ok(rows_of(connection, QUERY_SQL, &params)?
        .into_iter()
        .map(hit)
        .collect())
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

fn parser() -> Parser {
    Parser::new("python -m fadden search")
        .description("Build and query a local full-text, definitions and reference index.")
        .subcommands("command")
        .sub("build", Some("index a finished corpus root"), |p| {
            p.arg(Action::positional("corpus_root")).arg(
                Action::option("--out")
                    .required()
                    .help("new index file; must not exist"),
            )
        })
        .sub("query", Some("rank rows that contain every word"), |p| {
            p.arg(Action::positional("index"))
                .arg(Action::positional("words").one_or_more())
                .arg(Action::option("--title").help("search only this register id"))
                .arg(
                    Action::option("--limit")
                        .int()
                        .default(crate::argparse::Value::Int(10))
                        .choices(
                            &(1..=100)
                                .map(|n| n.to_string())
                                .collect::<Vec<_>>()
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>(),
                        )
                        .metavar("1-100"),
                )
                .arg(Action::flag("--json").help("print one object per hit"))
        })
        .sub("define", Some("look up a defined term"), |p| {
            p.arg(Action::positional("index"))
                .arg(Action::positional("term").one_or_more())
                .arg(Action::option("--title").help("look only in this register id"))
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

fn list(args: &crate::argparse::Namespace, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(crate::argparse::Value::List(items)) => items.clone(),
        _ => Vec::new(),
    }
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    let outcome = execute(&args, run);
    Ok(match outcome {
        Ok(()) => 0,
        // `except (SearchIndexError, ValueError, OSError, sqlite3.Error)`.
        Err(error @ (Exc::Value { .. } | Exc::UnicodeEncode(_) | Exc::Os(_) | Exc::Sqlite(_))) => {
            run.eprint(&format!("error: {error}"));
            1
        }
        Err(error) => run.uncaught(&error),
    })
}

fn execute(args: &crate::argparse::Namespace, run: &mut Run) -> Result<(), Exc> {
    let command = args.str("command").unwrap_or_default();
    if command == "build" {
        let [titles, rows, definitions, refs] = build(
            &args.str("corpus_root").unwrap_or_default(),
            &args.str("out").unwrap_or_default(),
        )?;
        run.print(&format!(
            "indexed {titles} titles, {rows} rows, {definitions} definitions and {refs} section references"
        ));
        return Ok(());
    }
    let connection = open_index(&args.str("index").unwrap_or_default())?;
    let title = args.str("title");
    match command.as_str() {
        "query" => {
            let limit = match args.get("limit") {
                Some(crate::argparse::Value::Int(n)) => *n,
                _ => 10,
            };
            let hits = query(&connection, &list(args, "words"), title.as_deref(), limit)?;
            if args.flag("json") {
                for hit in &hits {
                    run.print_points(&dumps_line(hit, false))?;
                }
                return Ok(());
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
            run.print(&format!(
                "{} rows. {}",
                hits.len(),
                source_line(&connection)?
            ));
        }
        "define" => {
            let found = define(&connection, &list(args, "term").join(" "), title.as_deref())?;
            if args.flag("json") {
                for item in &found {
                    run.print_points(&dumps_line(item, false))?;
                }
                return Ok(());
            }
            for item in &found {
                let s = |k: &str| pyvalue::py_str(item.get(k).unwrap_or(&Json::Null));
                run.print(&format!("{} ({}, {})", s("term"), s("act"), s("heading")));
                run.print(&format!("   {}", describe(item)));
                run.print(&format!("   {}", s("definition").replace("\n\n", "\n   ")));
            }
            if found.is_empty() {
                run.print(
                    "No definition paragraph found. The term may be defined another way or in a row this \
                     index does not read as definitions; this is not a finding that it has its ordinary meaning.",
                );
            }
            run.print(&source_line(&connection)?);
        }
        _ => {
            let row_id = args.str("row_id").unwrap_or_default();
            let report = refs(&connection, &row_id)?;
            if args.flag("json") {
                run.print_points(&dumps_line(&report, false))?;
                return Ok(());
            }
            let s = |v: &Json| pyvalue::py_str(v);
            run.print(&format!(
                "{}: {}",
                s(report.at("row_id")),
                s(report.at("heading"))
            ));
            if let Json::Array(cites) = report.at("cites") {
                for reference in cites {
                    let rows = match reference.at("rows") {
                        Json::Array(rows) => rows.clone(),
                        _ => Vec::new(),
                    };
                    let mut shown: Vec<String> =
                        rows.iter().take(5).map(|r| s(r.at("row_id"))).collect();
                    if rows.len() > 5 {
                        shown.push(format!("and {} more", rows.len() - 5));
                    }
                    let joined = shown.join(", ");
                    let place = if !joined.is_empty() {
                        joined
                    } else if matches!(reference.at("register_id"), Json::Null) {
                        "not in this index".into()
                    } else {
                        format!("no row for that {}", s(reference.at("kind")))
                    };
                    run.print(&format!(
                        "  cites {} {} ({}): {place}",
                        s(reference.at("kind")),
                        s(reference.at("key")),
                        s(reference.at("citation"))
                    ));
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
            run.print(&source_line(&connection)?);
        }
    }
    Ok(())
}
