//! The port of `fadden/extract.py` (stage 4; removed after the port): each downloaded EPUB as markdown, an
//! endnotes file and per-section JSONL rows, then `manifest_md.json`.
//!
//! A title that fails is reported (`  FAIL <id> <message>`) and the run goes
//! on; any failure refuses the manifest. Text from the manifest (a title's
//! name and metadata) stays in code points, so a lone surrogate fails where
//! Python's UTF-8 writes fail.
use std::io::{Read, Write};

use super::corpus_paths::{
    child, corpus_root, markdown_text, module_dir, register_id, require_builder_layout,
    script_dir_in, write_json_atomic,
};
use super::epub;
use super::extract_doc::{Block, epub_blocks, md_table};
use super::{Exc, Run, get, getitem, head, iterate, json_error};
use crate::argparse::Exit;
use crate::py;
use crate::pydatetime;
use crate::pyjson::{self, Json, PyStr};
use crate::pyos::{self, OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyre::{I, Re};
use crate::pyvalue::{self, py_eq, str_points};

const LICENCE: &str = "CC BY 4.0";
/// The container level at which `json.dump` stops writing manifest_md.json
/// under `python -m fadden extract` (CPython 3.15, base revision): a title's
/// `version_is_current` nested 987 deep is written, 988 is refused
/// (`tools/extract_depth_probe.py`). The pure-Python encoder takes a frame per
/// container, so the limit follows the call stack; the differential's runner
/// sits 3 frames shallower and accepts 990.
const MANIFEST_DEPTH: usize = 989;
const LICENCE_URL: &str = "https://creativecommons.org/licenses/by/4.0/";

const SKIP_CLASS: &str = r"^(TOC\d|TofSects|Contents|Header|Footer)";
const CONTENTS_CLASS: &str = r"^(TOC\d|TofSects|Contents)";
const ENDNOTE_CLASS: &str = r"^(ENote|TableOfActs|TableOfAmend|LI-Endnote)";
const ENDNOTE_TEXT: &str = r"^(endnotes|about the endnotes|notes to the .+ act \d{4})\s*$";
const IASB_HEAD: &str = r"^(?:IASB|AASB|Conv)SectionTitle([1-4])";
const SECNO: &str = "(\\d{1,3}[A-Z]{0,4})\\s*\u{2011}\\s*(\\d{1,4}[A-Z]{0,8})";
const PLAIN_SECNO: &str = "^\\s*(\\d{1,4}[A-Z]{0,8})(?=[.\\s\u{a0}\u{2011}]|$)";
const FRONT_CLASS: &str = r"^(LongT|Preamble)";
const PREBODY_CLASS: &str = r"^(subsection|paragraph|notetext|Preamble|SOText|noteToPara)";
const PARA_CLASS: &str =
    r"^(paragraph|paragraphsub|parabullet|LI-BodyTextSubpara|LI-BodyTextParaa|LI-Sectionparaa)";
const BARE_SECHEAD: &str = r"^\s*(\d{1,3}[A-Z]{0,3})\.?\s+([A-Z][^.;]{2,140}[^\s.;,:])\s*$";
const BARE_DATEISH: &str = r"^\s*\d{1,3}\.?\s+(January|February|March|April|May|June|July|August|September|October|November|December)\b";
const BARE_CONTAINER: &str =
    r"^\s*(?:Schedule|Part|Division|Chapter)\s+\d{1,3}[A-Z]{0,3}\b.{0,90}$";
// `\.{2,}+` in Python: a possessive run of dots. Nothing after it can match
// a dot, so the greedy run matches the same text.
const TOC_LEADER: &str = r"(?<!\.)\s*\.{2,}\s*\d*$";

/// `re.match(pattern, text)` for a compiled pattern.
fn matches(pattern: &Re, text: &str) -> bool {
    pattern.match_at(text, 0).is_some()
}

fn head_level(base: &str) -> Option<usize> {
    let mapped = match base {
        "ActHead1" | "ActHead2" | "LI-Heading1" => Some(2),
        "ActHead3" => Some(3),
        "ActHead4" => Some(4),
        "ActHead5" | "LI-Heading2" => Some(5),
        _ => None,
    };
    mapped.or_else(|| {
        crate::static_re!(IASB_HEAD, I)
            .match_at(base, 0)
            .map(|m| if &m[1] == "1" { 2 } else { 5 })
    })
}

/// `(cls.split() or [""])[0]`.
fn first_token(cls: &str) -> &str {
    py::split_whitespace(cls).first().copied().unwrap_or("")
}

fn points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

fn is_space_point(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(py::is_space)
}

fn strip_points(text: &[u32]) -> &[u32] {
    let start = text
        .iter()
        .position(|&c| !is_space_point(c))
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|&c| !is_space_point(c))
        .map_or(start, |i| i + 1);
    &text[start..end]
}

/// `re.sub(r"\n{3,}", "\n\n", text)`.
fn squeeze_newlines(text: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(text.len());
    let mut run = 0;
    for &cp in text {
        if cp == 0x0a {
            run += 1;
            if run <= 2 {
                out.push(cp);
            }
        } else {
            run = 0;
            out.push(cp);
        }
    }
    out
}

/// `ascii(text)`.
fn ascii(text: &str) -> String {
    let mut out = String::new();
    for c in py::repr_str(text).chars() {
        match c as u32 {
            0..=0x7f => out.push(c),
            cp @ 0x80..=0xff => out.push_str(&format!("\\x{cp:02x}")),
            cp @ 0x100..=0xffff => out.push_str(&format!("\\u{cp:04x}")),
            cp => out.push_str(&format!("\\U{cp:08x}")),
        }
    }
    out
}

/// `yaml_scalar(value)`: a JSON scalar, ASCII only.
fn yaml_scalar(value: Option<&Json>) -> Result<String, Exc> {
    let value = value.unwrap_or(&Json::Null);
    match value {
        Json::Array(_) | Json::Object(_) => Err(Exc::value("front matter requires a scalar value")),
        Json::Float(f) if !f.is_finite() => Err(Exc::value(format!(
            "Out of range float values are not JSON compliant: {}",
            py::float_repr(*f)
        ))),
        other => Ok(pyvalue::points_lossy(&pyvalue::dumps_line(other, true))),
    }
}

fn attribution(retrieved: &str) -> String {
    format!(
        "Based on content from the Federal Register of Legislation at {retrieved}. \
         For the latest information on Australian Government legislation \
         please go to https://www.legislation.gov.au. Changes: converted \
         from EPUB to markdown, contents pages removed, endnotes separated, \
         images replaced by their descriptive alt text, compilation cover pages \
         and running headers omitted."
    )
}

/// One retrieval row; `heading` and `container` can carry the title's name.
#[derive(Debug, Clone)]
struct Row {
    section: Option<String>,
    heading: Json,
    kind: &'static str,
    container: Option<Json>,
    text: Vec<String>,
    emit_fallback: bool,
}

impl Row {
    fn has_text(&self) -> bool {
        self.text.iter().any(|t| !py::strip(t).is_empty())
    }
}

/// `_VolumeState`.
struct Volume {
    gate_free: bool,
    seen_body: bool,
    endnote_armed: bool,
    in_endnotes: bool,
    container: Option<Json>,
    /// Index of the open row in `sections`.
    cur: Option<usize>,
    prebody_open: bool,
}

impl Volume {
    fn new(gate_free: bool) -> Self {
        let mut v = Volume {
            gate_free,
            seen_body: false,
            endnote_armed: false,
            in_endnotes: false,
            container: None,
            cur: None,
            prebody_open: false,
        };
        v.reset();
        v
    }

    fn reset(&mut self) {
        self.seen_body = self.gate_free;
        self.endnote_armed = self.gate_free;
        self.in_endnotes = false;
        self.container = None;
        self.cur = None;
        self.prebody_open = false;
    }
}

struct Markdown {
    md: Vec<u32>,
    sections: Vec<Row>,
    endnotes: String,
    long_title: Option<String>,
}

struct Builder {
    lines: Vec<String>,
    sections: Vec<Row>,
    endnotes: Vec<String>,
    state: Volume,
}

impl Builder {
    fn open_row(
        &mut self,
        section: Option<String>,
        heading: Json,
        kind: &'static str,
        container: Option<Json>,
        emit_fallback: bool,
    ) {
        self.sections.push(Row {
            section,
            heading,
            kind,
            container,
            text: Vec::new(),
            emit_fallback,
        });
        self.state.cur = Some(self.sections.len() - 1);
    }

    fn emit(&mut self, s: String) {
        if self.state.in_endnotes {
            self.endnotes.push(s);
            return;
        }
        if self.state.cur.is_none() && !py::strip(&s).is_empty() {
            let container = self.state.container.clone();
            self.open_row(
                None,
                Json::str("Introductory material"),
                "introductory",
                container,
                true,
            );
        }
        if let Some(cur) = self.state.cur {
            self.sections[cur].text.push(s.clone());
        }
        self.lines.push(s);
    }
}

/// `to_markdown(blocks, meta, force_bare)`.
fn to_markdown(blocks: &[Block], meta: &Json, force_bare: bool) -> Result<Markdown, Exc> {
    let p_level = |b: &Block| match b {
        Block::P { cls, .. } => head_level(first_token(cls)),
        _ => None,
    };
    let has_acthead = blocks.iter().any(|b| p_level(b).is_some());

    // `_vol_gate`: where a heading-less volume's body starts.
    let vol_gate_at = |seg: &[Block], start: usize| {
        seg.iter().enumerate().find_map(|(off, b)| match b {
            Block::P { cls, .. }
                if matches(crate::static_re!(CONTENTS_CLASS, I), cls)
                    || matches(crate::static_re!(PREBODY_CLASS, I), first_token(cls)) =>
            {
                Some(start + off)
            }
            _ => None,
        })
    };
    let mut vol_heads = Vec::new();
    let mut vol_gate = Vec::new();
    let mut close_vol = |start: usize, end: usize| -> Result<(), Exc> {
        let seg = &blocks[start..end];
        let heads = seg.iter().any(|b| p_level(b).is_some());
        let gate = vol_gate_at(seg, start);
        vol_heads.push(heads);
        vol_gate.push(gate);
        let droppable = seg.iter().any(|b| match b {
            Block::P { cls, .. } => !matches(crate::static_re!(SKIP_CLASS, I), cls),
            _ => true,
        });
        if has_acthead && !force_bare && !seg.is_empty() && !heads && gate.is_none() && droppable {
            return Err(Exc::value(format!(
                "volume {} has no identifiable body boundary; refusing to drop {} blocks",
                vol_heads.len() - 1,
                seg.len()
            )));
        }
        Ok(())
    };
    let mut start = 0;
    for (i, b) in blocks.iter().enumerate() {
        if *b == Block::File {
            close_vol(start, i)?;
            start = i + 1;
        }
    }
    close_vol(start, blocks.len())?;

    let bare_mode = force_bare;
    let mut toc_titles = std::collections::HashSet::new();
    let mut classless_heads = std::collections::HashSet::new();
    if bare_mode {
        for b in blocks {
            let Block::P { cls, text, .. } = b else {
                continue;
            };
            if !matches(crate::static_re!(r"^TOC\d", I), first_token(cls)) {
                continue;
            }
            let joined = py::split_whitespace(text).join(" ");
            let cut = crate::static_re!(TOC_LEADER, 0).sub("", &joined);
            let t = py::strip(&cut);
            let n = py::char_len(t);
            if 2 < n && n < 120 {
                // to_lowercase stands in for str.lower: both apply Unicode's full
                // lowercase mapping with Final_Sigma, and the same mapping builds
                // and probes this set, so a match could differ from Python's only
                // where the 2 Unicode versions or Final_Sigma contexts disagree.
                toc_titles.insert(t.to_lowercase());
            }
        }
        let paras: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b, Block::P { text, .. } if !py::strip(text).is_empty()))
            .map(|(i, _)| i)
            .collect();
        for (pos, &i) in paras.iter().enumerate().take(paras.len().saturating_sub(1)) {
            let Block::P { cls, text, .. } = &blocks[i] else {
                continue;
            };
            if !py::strip(cls).is_empty() {
                continue;
            }
            let t = py::strip(text);
            let n = py::char_len(t);
            if !(2 < n && n <= 80) || t.ends_with(['.', ';', ',', ':']) {
                continue;
            }
            let Block::P { cls: next, .. } = &blocks[paras[pos + 1]] else {
                continue;
            };
            let base_n = first_token(next);
            if !base_n.is_empty() && !matches(crate::static_re!(SKIP_CLASS, I), base_n) {
                classless_heads.insert(i);
            }
        }
    }

    let mut out = Builder {
        lines: Vec::new(),
        sections: Vec::new(),
        endnotes: Vec::new(),
        state: Volume::new(!has_acthead || force_bare),
    };
    let mut long_title: Option<String> = None;
    let mut long_title_is_longt = false;
    let mut vol = 0;

    for (bi, b) in blocks.iter().enumerate() {
        if *b == Block::File {
            vol += 1;
            out.state.reset();
            continue;
        }
        if !out.state.seen_body && !vol_heads[vol] && vol_gate[vol].is_some_and(|gate| bi >= gate) {
            out.state.seen_body = true;
            let mut container = str_points(getitem(meta, "name")?);
            container.extend(points(&format!(" - volume {vol}")));
            let container = json_str(&container);
            out.state.container = Some(container.clone());
            out.open_row(None, container.clone(), "container", Some(container), false);
        }

        let (cls, text, sectno) = match b {
            Block::Img { alt } => {
                if out.state.seen_body || out.state.in_endnotes || out.state.prebody_open {
                    out.emit(format!("> Figure: {alt}"));
                }
                continue;
            }
            Block::Table { rows } => {
                if out.state.seen_body || out.state.in_endnotes || out.state.prebody_open {
                    for line in md_table(rows)? {
                        out.emit(line);
                    }
                    out.emit(String::new());
                }
                continue;
            }
            Block::P { cls, text, sectno } => (cls, text, *sectno),
            Block::File => unreachable!(),
        };
        let base = if cls.is_empty() {
            ""
        } else {
            *py::split_whitespace(cls)
                .first()
                .ok_or_else(|| Exc::Other {
                    class: "IndexError",
                    message: "list index out of range".into(),
                })?
        };

        if matches(crate::static_re!(SKIP_CLASS, I), cls) {
            continue;
        }
        if !out.state.in_endnotes
            && (matches(crate::static_re!(ENDNOTE_CLASS, I), base)
                || (out.state.endnote_armed && matches(crate::static_re!(ENDNOTE_TEXT, I), text)))
        {
            out.state.in_endnotes = true;
            out.state.seen_body = true;
            out.state.endnote_armed = true;
            out.state.cur = None;
        }
        let longt = base.to_ascii_lowercase().starts_with("longt");
        if matches(crate::static_re!(FRONT_CLASS, I), base)
            && (long_title.is_none() || longt)
            && (long_title.is_none() || !long_title_is_longt)
        {
            long_title = Some(text.clone());
            long_title_is_longt = longt;
        }

        let mut lvl = head_level(base);
        if lvl.is_none() && sectno && !has_acthead && py::char_len(text) < 200 {
            lvl = Some(5);
        }
        if let Some(lvl) = lvl.filter(|_| !out.state.in_endnotes) {
            out.state.seen_body = true;
            out.state.endnote_armed = true;
            out.lines.push(String::new());
            out.lines.push(format!("{} {text}", "#".repeat(lvl)));
            if lvl == 5 {
                let sid = match crate::static_re!(SECNO, 0).match_at(py::strip(text), 0) {
                    Some(m) => Some(format!("{}-{}", &m[1], &m[2])),
                    None => crate::static_re!(PLAIN_SECNO, 0)
                        .match_at(text, 0)
                        .map(|m| m[1].to_string()),
                };
                let kind = if sid.is_some() {
                    "section"
                } else {
                    "unnumbered"
                };
                let container = out.state.container.clone();
                out.open_row(sid, Json::str(text), kind, container, false);
            } else {
                out.state.container = Some(Json::str(text));
                out.open_row(
                    None,
                    Json::str(text),
                    "container",
                    Some(Json::str(text)),
                    false,
                );
            }
            continue;
        }

        if bare_mode && !out.state.in_endnotes && !matches(crate::static_re!(SKIP_CLASS, I), cls) {
            let normal = py::split_whitespace(text).join(" ").to_lowercase();
            if toc_titles.contains(&normal) || classless_heads.contains(&bi) {
                out.lines.push(String::new());
                out.lines.push(format!("## {text}"));
                out.state.container = Some(Json::str(text));
                out.open_row(
                    None,
                    Json::str(text),
                    "container",
                    Some(Json::str(text)),
                    false,
                );
                continue;
            }
            let sechead = if matches(crate::static_re!(BARE_DATEISH, I), text) {
                None
            } else {
                crate::static_re!(BARE_SECHEAD, 0).match_at(text, 0)
            };
            if let Some(m) = sechead {
                out.lines.push(String::new());
                out.lines.push(format!("##### {text}"));
                let container = out.state.container.clone();
                out.open_row(
                    Some(m[1].to_string()),
                    Json::str(text),
                    "section",
                    container,
                    false,
                );
                continue;
            }
            if matches(crate::static_re!(BARE_CONTAINER, 0), text) {
                out.lines.push(String::new());
                out.lines.push(format!("## {text}"));
                out.state.container = Some(Json::str(text));
                out.open_row(
                    None,
                    Json::str(text),
                    "container",
                    Some(Json::str(text)),
                    false,
                );
                continue;
            }
            if out.state.cur.is_none() && !py::strip(text).is_empty() {
                out.open_row(
                    None,
                    Json::str("Introductory material"),
                    "introductory",
                    None,
                    false,
                );
            }
        }

        if !out.state.seen_body && !out.state.in_endnotes {
            if !matches(crate::static_re!(PREBODY_CLASS, I), base) {
                continue;
            }
            out.state.prebody_open = true;
            if out.state.cur.is_none() {
                out.open_row(
                    None,
                    Json::str("Introductory material"),
                    "introductory",
                    None,
                    false,
                );
            }
        }
        let prefix = if matches(crate::static_re!(PARA_CLASS, I), base) {
            "- "
        } else if base == "notetext" {
            "> "
        } else {
            ""
        };
        out.emit(String::new());
        out.emit(format!("{prefix}{text}"));
    }

    let retrieved = getitem(meta, "retrieved")?;
    let attr = attribution(&pyvalue::py_str(retrieved));
    let id = pyvalue::py_str(getitem(meta, "id")?);
    let version_is_current = if matches!(meta.get("version_is_current"), Some(Json::Bool(false))) {
        "false"
    } else {
        "true"
    };
    let source_url = meta.get("sourceUrl").cloned().unwrap_or(Json::str(""));
    let fm = [
        "---".to_string(),
        format!("register_id: {}", yaml_scalar(Some(getitem(meta, "id")?))?),
        format!("title: {}", yaml_scalar(Some(getitem(meta, "name")?))?),
        format!(
            "long_title: {}",
            yaml_scalar(Some(&Json::str(long_title.as_deref().unwrap_or(""))))?
        ),
        format!("collection: {}", yaml_scalar(meta.get("collection"))?),
        format!(
            "compilation_number: {}",
            yaml_scalar(meta.get("compilationNumber"))?
        ),
        format!(
            "compilation_date: {}",
            yaml_scalar(meta.get("versionStart"))?
        ),
        format!("version_is_current: {version_is_current}"),
        format!(
            "superseded_from: {}",
            yaml_scalar(meta.get("current_version_start"))?
        ),
        format!("source_url: {}", yaml_scalar(Some(&source_url))?),
        format!(
            "register_page: {}",
            yaml_scalar(Some(&Json::str(&format!(
                "https://www.legislation.gov.au/{id}/latest/text"
            ))))?
        ),
        format!("retrieved: {}", yaml_scalar(Some(retrieved))?),
        format!("licence: {}", yaml_scalar(Some(&Json::str(LICENCE)))?),
        format!(
            "licence_url: {}",
            yaml_scalar(Some(&Json::str(LICENCE_URL)))?
        ),
        "authorised: false".to_string(),
        format!("attribution: {}", yaml_scalar(Some(&Json::str(&attr)))?),
        "---".to_string(),
        String::new(),
    ];
    let mut body = points("# ");
    body.extend(markdown_text(&str_points(getitem(meta, "name")?)));
    body.push(0x0a);
    if let Some(title) = long_title.as_deref().filter(|t| !t.is_empty()) {
        body.extend(points(&format!("\n> {title}\n")));
    }
    body.extend(points(&out.lines.join("\n")));
    let body = squeeze_newlines(&body);
    let mut md = points(&fm.join("\n"));
    md.extend_from_slice(strip_points(&body));
    md.push(0x0a);
    Ok(Markdown {
        md,
        sections: out.sections,
        endnotes: py::strip(&out.endnotes.join("\n")).to_string(),
        long_title,
    })
}

/// `check_chunks_complete(lines, chunks)`.
fn check_chunks_complete(lines: &[String], chunks: &[Row]) -> Result<(), Exc> {
    let kept: std::collections::HashSet<&str> = chunks
        .iter()
        .flat_map(|c| c.text[0].split('\n').map(py::strip))
        .collect();
    let lost: Vec<&str> = lines
        .iter()
        .map(|l| py::strip(l))
        .filter(|l| !l.is_empty() && !kept.contains(l))
        .collect();
    if let Some(first) = lost.first() {
        return Err(Exc::Runtime(format!(
            "chunking dropped {} body line(s), first: {}",
            lost.len(),
            ascii(py::prefix_chars(first, 80))
        )));
    }
    Ok(())
}

/// `table_split(body, name)`.
fn table_split(body: &str, name: &Json) -> Result<Vec<Row>, Exc> {
    enum Seg {
        Prose(Vec<String>),
        Table(Vec<String>),
    }
    let mut segs = Vec::new();
    let (mut cur, mut pros) = (Vec::new(), Vec::new());
    for ln in body.split('\n') {
        if ln.starts_with('|') {
            if !pros.is_empty() {
                segs.push(Seg::Prose(std::mem::take(&mut pros)));
            }
            cur.push(ln.to_string());
            continue;
        }
        if !cur.is_empty() {
            segs.push(Seg::Table(std::mem::take(&mut cur)));
        }
        let s = py::strip(ln);
        if !s.is_empty() && !s.starts_with('#') {
            pros.push(s.to_string());
        }
    }
    if !cur.is_empty() {
        segs.push(Seg::Table(cur));
    }
    if !pros.is_empty() {
        segs.push(Seg::Prose(pros));
    }
    let words = py::word_count;
    let tables: Vec<&Vec<String>> = segs
        .iter()
        .filter_map(|s| match s {
            Seg::Table(t) if t.len() >= 3 => Some(t),
            _ => None,
        })
        .collect();
    if tables.len() < 3 {
        return Ok(Vec::new());
    }
    let t_words: usize = tables.iter().flat_map(|t| t.iter()).map(|l| words(l)).sum();
    let p_words: usize = segs
        .iter()
        .filter_map(|s| match s {
            Seg::Prose(p) => Some(p),
            _ => None,
        })
        .flatten()
        .map(|l| words(l))
        .sum();
    if t_words < 2 * p_words {
        return Ok(Vec::new());
    }

    let name = str_points(name);
    let titled = |suffix: &str| {
        let mut heading = name.clone();
        heading.extend(points(&format!(" \u{2014} {suffix}")));
        json_str(&heading)
    };
    let mut out = Vec::new();
    let mut emit = |heading: Json, kind: &'static str, text: String| {
        out.push(Row {
            section: None,
            heading,
            kind,
            container: None,
            text: vec![text],
            emit_fallback: false,
        });
    };
    let total = tables.len();
    let mut pending: Vec<String> = Vec::new();
    let mut n = 0;
    for seg in &segs {
        let seg = match seg {
            Seg::Prose(p) => {
                pending.extend(p.iter().cloned());
                continue;
            }
            Seg::Table(t) if t.len() < 3 => {
                pending.extend(t.iter().cloned());
                continue;
            }
            Seg::Table(t) => t,
        };
        n += 1;
        let cap = pending.last().filter(|c| py::char_len(c) <= 80).cloned();
        let rest = if cap.is_some() {
            &pending[..pending.len() - 1]
        } else {
            &pending[..]
        };
        if rest.iter().map(|s| words(s)).sum::<usize>() > 120 {
            emit(
                titled("opening provisions"),
                "text_block",
                py::strip(&rest.join("\n")).to_string(),
            );
            pending = cap.iter().cloned().collect();
        }
        let heading = match &cap {
            Some(cap) => titled(cap),
            None => titled(&format!("table {n} of {total}")),
        };
        let text: Vec<String> = pending.iter().chain(seg.iter()).cloned().collect();
        emit(
            heading,
            "table_block",
            py::strip(&text.join("\n")).to_string(),
        );
        pending.clear();
    }
    if !pending.is_empty() {
        emit(
            titled("closing provisions"),
            "text_block",
            py::strip(&pending.join("\n")).to_string(),
        );
    }
    let lines: Vec<String> = segs
        .iter()
        .flat_map(|s| match s {
            Seg::Prose(v) | Seg::Table(v) => v.iter().cloned(),
        })
        .collect();
    check_chunks_complete(&lines, &out)?;
    Ok(out)
}

/// `dict(record, field=value)`: an existing key keeps its place.
fn set_field(record: &mut Json, field: &str, value: Json) {
    if let Json::Object(pairs) = record {
        match pairs.iter_mut().find(|(k, _)| k.as_str() == Some(field)) {
            Some(pair) => pair.1 = value,
            None => pairs.push((PyStr::Text(field.into()), value)),
        }
    }
}

fn json_str(text: &[u32]) -> Json {
    Json::Str(PyStr::from_code_points(text.to_vec()))
}

/// `open(path, "w", encoding="utf-8")` and one `write` per chunk: the file
/// is created first, `\n` becomes the platform's line separator, and a chunk
/// UTF-8 cannot encode stops the writes with the chunks before it kept.
fn write_chunks(path: &str, chunks: &[Vec<u32>]) -> Result<(), Exc> {
    let mut file =
        std::fs::File::create(path).map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
    let mut bytes = Vec::new();
    let mut failure = None;
    for chunk in chunks {
        // Without a lone surrogate, encode directly; otherwise build the text
        // the error's position counts in.
        if chunk.iter().all(|&cp| char::from_u32(cp).is_some()) {
            let mut utf8 = [0u8; 4];
            for c in chunk.iter().filter_map(|&cp| char::from_u32(cp)) {
                if c == '\n' && cfg!(windows) {
                    bytes.push(b'\r');
                }
                bytes.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
            }
            continue;
        }
        let mut text = Vec::with_capacity(chunk.len());
        for &cp in chunk {
            if cp == 0x0a && cfg!(windows) {
                text.push(0x0d);
            }
            text.push(cp);
        }
        let message = pyvalue::utf8_strict_error(&text).expect("a lone surrogate");
        failure = Some(Exc::UnicodeEncode(message));
        break;
    }
    file.write_all(&bytes)
        .map_err(|e| OsError::new(&e, None, Style::Errno))?;
    failure.map_or(Ok(()), Err)
}

/// `datetime.date.fromtimestamp(os.path.getmtime(path))`: the local date.
fn mtime_date(path: &str) -> Result<py::Date, Exc> {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| OsError::new(&e, Some(path), Style::Native))?;
    let (secs, nanos) = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            match d.subsec_nanos() {
                0 => (-(d.as_secs() as i64), 0),
                n => (-(d.as_secs() as i64) - 1, 1_000_000_000 - n),
            }
        }
    };
    // `st_mtime` is `sec + nsec * 1e-9` as a float; `fromtimestamp` floors it.
    let stamp = (secs as f64 + f64::from(nanos) * 1e-9).floor() as i64;
    local_date(stamp)
}

#[cfg(windows)]
pub(crate) fn local_date(stamp: i64) -> Result<py::Date, Exc> {
    #[repr(C)]
    #[derive(Default)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
    }
    unsafe extern "C" {
        fn _localtime64_s(tm: *mut Tm, time: *const i64) -> i32;
    }
    let mut tm = Tm::default();
    // SAFETY: the CRT fills `tm` from the time value it is given, as
    // CPython's _PyTime_localtime does.
    let errno = unsafe { _localtime64_s(&mut tm, &stamp) };
    if errno != 0 {
        let error = std::io::Error::from_raw_os_error(errno);
        return Err(OsError::new(&error, None, Style::Errno).into());
    }
    py::Date::new(
        i64::from(tm.year) + 1900,
        i64::from(tm.mon) + 1,
        i64::from(tm.mday),
    )
    .map_err(Exc::value)
}

#[cfg(unix)]
pub(crate) fn local_date(stamp: i64) -> Result<py::Date, Exc> {
    // SAFETY: localtime_r fills `tm` from the time value it is given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&(stamp as libc::time_t), &mut tm).is_null() {
            let error = std::io::Error::last_os_error();
            return Err(OsError::new(&error, None, Style::Errno).into());
        }
        tm
    };
    py::Date::new(
        i64::from(tm.tm_year) + 1900,
        i64::from(tm.tm_mon) + 1,
        i64::from(tm.tm_mday),
    )
    .map_err(Exc::value)
}

/// Opens and reads each EPUB once on background threads, discarding the
/// bytes. An archive's first open after another process wrote it can wait
/// several milliseconds for the Windows real-time scanner; warming moves that
/// wait off the stage's path. The stage still opens and reads every file
/// itself, so what it reads and every error it meets are as without this.
/// Only regular files are read, through a fixed buffer and no further than the
/// compressed budget, and the threads are detached: one stalled on a file
/// cannot hold the stage, which never waits for them.
fn warm_up(paths: Vec<String>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let paths = std::sync::Arc::new(paths);
    let next = std::sync::Arc::new(AtomicUsize::new(0));
    for _ in 0..WARMERS {
        let (paths, next) = (paths.clone(), next.clone());
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 1 << 16];
            while let Some(path) = paths.get(next.fetch_add(1, Ordering::Relaxed)) {
                if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                    continue;
                }
                let Ok(file) = std::fs::File::open(path) else {
                    continue;
                };
                let mut file = file.take(epub::MAX_EPUB_BYTES + 1);
                while matches!(file.read(&mut buffer), Ok(n) if n > 0) {}
            }
        });
    }
}

/// Threads warming EPUBs.
const WARMERS: usize = 4;

/// The parse of one title: everything inside `main`'s first `except Exception`.
struct Parsed {
    entry: Json,
    fetched: String,
    md: Markdown,
}

fn parse_title(
    entry: &mut Json,
    rid: &str,
    src: &str,
    retrieved: Option<&str>,
    run: &mut Run,
) -> Result<Parsed, Exc> {
    let mut fetched_at = entry.get("fetched_at").cloned();
    let side = format!("{src}.meta.json");
    let truthy = |v: &Option<Json>| v.as_ref().is_some_and(Json::truthy);
    if retrieved.is_none()
        && !truthy(&fetched_at)
        && std::fs::metadata(&side).is_ok_and(|m| m.is_file())
    {
        let text = crate::pyio::read_text(&side)?;
        let provenance = pyjson::loads_standard(&text).map_err(json_error)?;
        // `.get` gives None for a missing key and for JSON null alike.
        let theirs = get(&provenance, "versionStart")?.unwrap_or(&Json::Null);
        let ours = entry.get("versionStart").unwrap_or(&Json::Null);
        let same = py_eq(theirs, ours);
        if same {
            fetched_at = provenance.get("fetched_at").cloned();
        }
    }
    let fetched = if let Some(day) = retrieved {
        day.to_string()
    } else if truthy(&fetched_at) {
        let stamp = match fetched_at.as_ref() {
            Some(Json::Str(s)) => s.code_points(),
            Some(other) => {
                return Err(Exc::Attribute(format!(
                    "'{}' object has no attribute 'replace'",
                    pyvalue::type_name(other)
                )));
            }
            None => unreachable!(),
        };
        let mut replaced = Vec::with_capacity(stamp.len() + 8);
        for cp in stamp {
            if cp == u32::from('Z') {
                replaced.extend(points("+00:00"));
            } else {
                replaced.push(cp);
            }
        }
        let parsed = pydatetime::datetime_fromisoformat(&replaced).map_err(Exc::value)?;
        if parsed.offset_us.is_none() {
            return Err(Exc::value("fetched_at must include a timezone"));
        }
        parsed.utc_date().map_err(Exc::Overflow)?.isoformat()
    } else {
        let day = mtime_date(src)?.isoformat();
        let warning = "retrieval date inferred from EPUB mtime; unverified";
        set_field(entry, "provenance_warning", Json::str(warning));
        run.print(&format!("  WARN {rid} {warning}"));
        day
    };
    let mut meta = entry.clone();
    set_field(&mut meta, "retrieved", Json::str(&fetched));
    let blocks = epub_blocks(src)?;
    let mut md = to_markdown(&blocks, &meta, false)?;
    // A fallback row is not evidence that the document chunked.
    if !md.sections.iter().any(|s| !s.emit_fallback && s.has_text()) {
        let bare = to_markdown(&blocks, &meta, true)?;
        if bare
            .sections
            .iter()
            .any(|s| matches!(s.kind, "section" | "container") && s.has_text())
        {
            md = bare;
        }
    }
    Ok(Parsed {
        entry: entry.clone(),
        fetched,
        md,
    })
}

/// `"%-<width>s" % text` over code points.
fn left(text: &[u32], width: usize) -> Vec<u32> {
    let mut out = text.to_vec();
    out.resize(text.len().max(width), u32::from(' '));
    out
}

/// A title's markdown and endnotes files, written before its rows are
/// chosen; an exception here stops the stage. Returns the title's directory.
fn write_markdown(
    md_root: &str,
    entry: &Json,
    rid: &str,
    fetched: &str,
    md: &Markdown,
) -> Result<String, Exc> {
    let d = child(md_root, &[rid])?;
    let attr = attribution(fetched);
    pyos::makedirs(&PyPath::new(&d))?;
    write_chunks(
        &child(&d, &[&format!("{rid}.md")])?,
        std::slice::from_ref(&md.md),
    )?;
    let name = getitem(entry, "name")?;
    if !md.endnotes.is_empty() {
        let title = match name {
            Json::Str(s) => {
                let mut t = points("Endnotes: ");
                t.extend(s.code_points());
                json_str(&t)
            }
            other => {
                return Err(Exc::Type(format!(
                    "can only concatenate str (not \"{}\") to str",
                    pyvalue::type_name(other)
                )));
            }
        };
        let lines = [
            "---".to_string(),
            format!("register_id: {}", yaml_scalar(Some(getitem(entry, "id")?))?),
            format!("title: {}", yaml_scalar(Some(&title))?),
            format!("collection: {}", yaml_scalar(entry.get("collection"))?),
            format!(
                "compilation_number: {}",
                yaml_scalar(entry.get("compilationNumber"))?
            ),
            format!(
                "compilation_date: {}",
                yaml_scalar(entry.get("versionStart"))?
            ),
            format!(
                "source_url: {}",
                yaml_scalar(Some(
                    &entry.get("sourceUrl").cloned().unwrap_or(Json::str(""))
                ))?
            ),
            format!(
                "register_page: {}",
                yaml_scalar(Some(&Json::str(&format!(
                    "https://www.legislation.gov.au/{rid}/latest/text"
                ))))?
            ),
            format!("retrieved: {}", yaml_scalar(Some(&Json::str(fetched)))?),
            format!("licence: {}", yaml_scalar(Some(&Json::str(LICENCE)))?),
            format!(
                "licence_url: {}",
                yaml_scalar(Some(&Json::str(LICENCE_URL)))?
            ),
            "authorised: false".to_string(),
            format!("attribution: {}", yaml_scalar(Some(&Json::str(&attr)))?),
            "---".to_string(),
            String::new(),
        ];
        let mut text = points(&lines.join("\n"));
        text.extend(points("# Endnotes: "));
        text.extend(markdown_text(&str_points(name)));
        text.extend(points(&format!("\n\n{}\n", md.endnotes)));
        write_chunks(&child(&d, &["endnotes.md"])?, &[text])?;
    }
    Ok(d)
}

/// A title's `sections.jsonl`; an exception here stops the stage.
fn write_sections(
    d: &str,
    entry: &Json,
    rid: &str,
    fetched: &str,
    emitted: &[Row],
    granularity: &str,
) -> Result<(), Exc> {
    let attr = attribution(fetched);
    let name = getitem(entry, "name")?;
    let page = format!("https://www.legislation.gov.au/{rid}/latest/text");
    let field = |key: &str| entry.get(key).cloned().unwrap_or(Json::Null);
    let rows: Vec<Vec<u32>> = emitted
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let ordinal = i + 1;
            let row_id = format!("{rid}:{ordinal:04}:{}", s.section.as_deref().unwrap_or("-"));
            let text = py::strip(&s.text.join("\n")).to_string();
            let value = Json::Object(
                [
                    ("register_id", Json::str(rid)),
                    ("act", name.clone()),
                    ("collection", field("collection")),
                    ("compilation_number", field("compilationNumber")),
                    ("compilation_date", field("versionStart")),
                    (
                        "version_is_current",
                        entry
                            .get("version_is_current")
                            .cloned()
                            .unwrap_or(Json::Bool(true)),
                    ),
                    ("superseded_from", field("current_version_start")),
                    ("retrieved", Json::str(fetched)),
                    ("row_id", Json::str(&row_id)),
                    (
                        "section",
                        s.section.as_deref().map_or(Json::Null, Json::str),
                    ),
                    ("heading", s.heading.clone()),
                    ("container", s.container.clone().unwrap_or(Json::Null)),
                    ("kind", Json::str(s.kind)),
                    ("granularity", Json::str(granularity)),
                    ("text", Json::str(&text)),
                    (
                        "source_url",
                        entry.get("sourceUrl").cloned().unwrap_or(Json::str("")),
                    ),
                    ("register_page", Json::str(&page)),
                    ("licence", Json::str(LICENCE)),
                    ("licence_url", Json::str(LICENCE_URL)),
                    ("authorised", Json::Bool(false)),
                    ("attribution", Json::str(&attr)),
                ]
                .into_iter()
                .map(|(k, v)| (PyStr::Text(k.into()), v))
                .collect(),
            );
            let mut line = pyvalue::dumps_line(&value, false);
            line.push(0x0a);
            line
        })
        .collect();
    // `json.dumps` without an indent is CPython's C encoder, which goes deeper
    // than any manifest pyjson reads, so these rows never meet a depth limit.
    write_chunks(&child(d, &["sections.jsonl"])?, &rows)
}

/// `main(retrieved)` with the module in `scratch`.
pub fn run(scratch: &str, retrieved: Option<&str>, run: &mut Run) -> Result<(), Exc> {
    let directory = script_dir_in(scratch, "extract.py")?;
    require_builder_layout(&directory)?;
    let retrieved = retrieved
        .map(|day| pydatetime::date_fromisoformat(day).map(|d| d.isoformat()))
        .transpose()
        .map_err(Exc::value)?;
    let text = crate::pyio::read_text(&pypath::join(scratch, "manifest_raw.json"))?;
    let manifest = pyjson::loads_standard(&text).map_err(json_error)?;
    let root = corpus_root(&directory);
    let md_root = child(&root, &["markdown"])?;
    pyos::makedirs(&PyPath::new(&md_root))?;
    let mut out_manifest = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();

    let mut titles = Vec::new();
    for m in iterate(&manifest)? {
        if get(&m, "epub")?.is_some_and(Json::truthy) {
            titles.push(m);
        }
    }
    let planned = |a: &Json| -> Result<(String, String), Exc> {
        let rid = register_id(getitem(a, "id")?)?;
        let expected = format!("{rid}.epub");
        if !a
            .get("epub")
            .is_some_and(|e| e.as_str() == Some(expected.as_str()))
        {
            return Err(Exc::value("unexpected EPUB filename"));
        }
        Ok((rid, expected))
    };
    warm_up(
        titles
            .iter()
            .filter_map(|a| child(&root, &["epub", &planned(a).ok()?.1]).ok())
            .collect(),
    );
    for (i, a) in titles.into_iter().enumerate() {
        let i = i + 1;
        let checked = planned(&a).and_then(|(rid, expected)| {
            let src = child(&root, &["epub", &expected])?;
            Ok((rid, src))
        });
        let (rid, src) = match checked {
            Ok(pair) => pair,
            Err(error @ (Exc::Key(_) | Exc::Value { .. })) => {
                let id = a.get("id").cloned().unwrap_or(Json::str("?"));
                failures.push((pyvalue::str_for_stderr(&id), error.to_string()));
                continue;
            }
            Err(other) => return Err(other),
        };
        let mut entry = a.clone();
        set_field(&mut entry, "id", Json::str(&rid));

        let parsed = parse_title(&mut entry, &rid, &src, retrieved.as_deref(), run);
        let Parsed { entry, fetched, md } = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                record_failure(run, &mut failures, &rid, &error)?;
                continue;
            }
        };

        let d = write_markdown(&md_root, &entry, &rid, &fetched, &md)?;
        let body_points = match find(&md.md, &points("\n---\n")) {
            Some(at) if md.md.starts_with(&points("---")) => md.md[at + 5..].to_vec(),
            _ => md.md.clone(),
        };
        let body = pyvalue::points_lossy(&body_points);
        let mut emitted: Vec<Row> = md
            .sections
            .iter()
            .filter(|s| s.has_text())
            .cloned()
            .collect();
        let (mut whole, mut tabled) = (false, false);
        if !emitted.iter().any(|s| !s.emit_fallback) {
            let name = getitem(&entry, "name")?;
            match table_split(&body, name) {
                Ok(chunks) => {
                    tabled = !chunks.is_empty();
                    emitted = chunks;
                }
                Err(error) => {
                    record_failure(run, &mut failures, &rid, &error)?;
                    continue;
                }
            }
        }
        if emitted.is_empty() {
            whole = true;
            emitted = vec![Row {
                section: None,
                heading: getitem(&entry, "name")?.clone(),
                kind: "whole_act",
                container: None,
                text: vec![py::strip(&body).to_string()],
                emit_fallback: false,
            }];
        }
        let granularity = if whole {
            "whole_act"
        } else if tabled {
            "table_block"
        } else {
            "section"
        };
        write_sections(&d, &entry, &rid, &fetched, &emitted, granularity)?;

        let words = py::word_count(&body);
        let mut record = entry.clone();
        set_field(
            &mut record,
            "markdown",
            Json::str(&format!("{rid}/{rid}.md")),
        );
        set_field(&mut record, "retrieved", Json::str(&fetched));
        set_field(&mut record, "sections", Json::Int(emitted.len().into()));
        set_field(&mut record, "granularity", Json::str(granularity));
        set_field(
            &mut record,
            "long_title",
            md.long_title.as_deref().map_or(Json::Null, Json::str),
        );
        set_field(&mut record, "words", Json::Int(words.into()));
        set_field(&mut record, "endnotes", Json::Bool(!md.endnotes.is_empty()));
        out_manifest.push(record);
        let mut line = points(&format!(
            "{i:3} {} sections={:<5} words={:<9} ",
            pyvalue::points_lossy(&left(&points(&rid), 12)),
            emitted.len(),
            words
        ));
        line.extend(str_points(&head(getitem(&entry, "name")?, 52)?));
        run.print_points(&line)?;
    }

    if !failures.is_empty() {
        let ids: Vec<&str> = failures
            .iter()
            .take(10)
            .map(|(id, _)| id.as_str())
            .collect();
        let more = if failures.len() <= 10 {
            String::new()
        } else {
            format!(" (and {} more)", failures.len() - 10)
        };
        return Err(Exc::Runtime(format!(
            "extraction incomplete for {} title(s): {}{more}; refusing to write manifest_md.json",
            failures.len(),
            ids.join(", ")
        )));
    }
    write_json_atomic(
        &pypath::join(scratch, "manifest_md.json"),
        &Json::Array(out_manifest.clone()),
        Some(1),
        MANIFEST_DEPTH,
    )?;
    run.print(&format!("\nEXTRACT DONE: {} acts", out_manifest.len()));
    Ok(())
}

fn find(haystack: &[u32], needle: &[u32]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `record_failure(entry, exc)`: print it and keep it for the summary.
fn record_failure(
    run: &mut Run,
    failures: &mut Vec<(String, String)>,
    rid: &str,
    error: &Exc,
) -> Result<(), Exc> {
    run.print(&format!("  FAIL {rid} {error}"));
    failures.push((rid.to_string(), error.to_string()));
    Ok(())
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    Ok(
        match self::run(&module_dir(), argv.first().map(String::as_str), run) {
            Ok(()) => 0,
            Err(error) => run.uncaught(&error),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyre::re;

    #[test]
    fn lowercase_uses_cpythons_unicode_version() {
        // Bare-mode contents titles match by to_lowercase standing in for
        // str.lower, which is exact only on the same Unicode tables: CPython
        // 3.15 has 17.0.0. Recheck this when the pinned toolchain changes.
        assert_eq!(char::UNICODE_VERSION, (17, 0, 0));
        // Unicode 17's Beria Erfe pair, which CPython 3.15 lowercases.
        assert_eq!("\u{16EA0}".to_lowercase(), "\u{16EBB}");
    }

    #[test]
    fn manifest_depth_stops_with_pythons_notes() {
        let nested = |depth: usize| {
            (0..depth).fold(Json::Array(Vec::new()), |inner, _| Json::Array(vec![inner]))
        };
        let title = |value: Json| {
            Json::Array(vec![
                Json::Object(vec![(PyStr::Text("id".into()), Json::str("x"))]),
                Json::Object(vec![(PyStr::Text("version_is_current".into()), value)]),
            ])
        };
        // 987 lists below the title list and dict: written, as Python writes them.
        assert!(super::super::refuse_deeper_than(&title(nested(986)), MANIFEST_DEPTH).is_ok());
        // 988: the innermost stops the encoder, and each container around it
        // adds its note, innermost first.
        let error = super::super::refuse_deeper_than(&title(nested(987)), MANIFEST_DEPTH)
            .unwrap_err()
            .to_string();
        let lines: Vec<&str> = error.lines().collect();
        assert!(lines[0].contains("maximum recursion depth exceeded"));
        assert_eq!(lines.len(), 1 + 987 + 2);
        assert!(
            lines[1..=987]
                .iter()
                .all(|l| *l == "when serializing list item 0")
        );
        assert_eq!(
            lines[988],
            "when serializing dict item 'version_is_current'"
        );
        assert_eq!(lines[989], "when serializing list item 1");
    }

    #[test]
    fn ascii_escapes_as_python_does() {
        assert_eq!(
            ascii("é\u{2011}\u{1f600}'\n"),
            "\"\\xe9\\u2011\\U0001f600'\\n\""
        );
    }

    #[test]
    fn toc_leaders_are_cut() {
        let cut = re(TOC_LEADER, 0).sub("", "Application .......... 12");
        assert_eq!(cut, "Application");
        assert_eq!(re(TOC_LEADER, 0).sub("", "a. .. 3"), "a. ");
    }

    #[test]
    fn table_runs_split_with_their_captions() {
        let table = "| a | b |\n|---|---|\n| 1 | 2 |";
        let body = format!("Lead.\n{table}\nCAP\n{table}\nMore.\n{table}\nEnd.");
        let rows = table_split(&body, &Json::str("Act")).unwrap();
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            ["table_block", "table_block", "table_block", "text_block"]
        );
        assert_eq!(rows[1].heading, Json::str("Act \u{2014} CAP"));
    }

    // Ported from tests/corpus/test_extract_paths.py (TableSplitTests,
    // ToMarkdownTests), test_regressions.py (VolumeGateTests, TableSplitTests,
    // PreSectionTextTests), test_audit_regressions.py and
    // test_security_remediation.py (yaml_scalar).

    fn meta(text: &str) -> Json {
        pyjson::loads_standard(text).unwrap()
    }

    fn p(text: &str, cls: &str, sectno: bool) -> Block {
        Block::P {
            cls: cls.into(),
            text: text.into(),
            sectno,
        }
    }

    fn md_text(md: &Markdown) -> String {
        pyvalue::points_lossy(&md.md)
    }

    /// `rows(sections)`: kind, heading and the non-blank text of each row.
    fn rows(sections: &[Row]) -> Vec<(&str, Json, Vec<&str>)> {
        sections
            .iter()
            .map(|s| {
                let text = s
                    .text
                    .iter()
                    .map(String::as_str)
                    .filter(|t| !py::strip(t).is_empty())
                    .collect();
                (s.kind, s.heading.clone(), text)
            })
            .collect()
    }

    /// `"\n".join("\n".join(s["text"]) for s in sections)`.
    fn retrievable(sections: &[Row]) -> String {
        sections
            .iter()
            .map(|s| s.text.join("\n"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A stored ZIP archive, as `zipfile.ZipFile(..., "w").writestr` writes one.
    fn stored_zip(members: &[(String, &str)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in members {
            let (name, data) = (name.as_bytes(), data.as_bytes());
            let crc = crate::pyzlib::crc32(data, 0);
            let offset = out.len() as u32;
            let sizes = [crc, data.len() as u32, data.len() as u32];
            out.extend(b"PK\x03\x04");
            [20u16, 0, 0, 0, 33]
                .iter()
                .for_each(|v| out.extend(v.to_le_bytes()));
            sizes.iter().for_each(|v| out.extend(v.to_le_bytes()));
            [name.len() as u16, 0]
                .iter()
                .for_each(|v| out.extend(v.to_le_bytes()));
            out.extend(name);
            out.extend(data);
            central.extend(b"PK\x01\x02");
            [20u16, 20, 0, 0, 0, 33]
                .iter()
                .for_each(|v| central.extend(v.to_le_bytes()));
            sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
            [name.len() as u16, 0, 0, 0, 0]
                .iter()
                .for_each(|v| central.extend(v.to_le_bytes()));
            [0u32, offset]
                .iter()
                .for_each(|v| central.extend(v.to_le_bytes()));
            central.extend(name);
        }
        let (start, count) = (out.len() as u32, members.len() as u16);
        let size = central.len() as u32;
        out.extend(central);
        out.extend(b"PK\x05\x06");
        [0u16, 0, count, count]
            .iter()
            .for_each(|v| out.extend(v.to_le_bytes()));
        [size, start]
            .iter()
            .for_each(|v| out.extend(v.to_le_bytes()));
        out.extend(0u16.to_le_bytes());
        out
    }

    const VOLUME_1: &str = "<html><body>\
        <p class=\"ActHead4\"><span class=\"CharSectno\">1</span> Short title</p>\
        <p class=\"subsection\">This instrument is the Demo Approval 2025.</p>\
        </body></html>";
    const VOLUME_2: &str = "<html><body>\
        <p class=\"MadeunderText\">Demo Approval 2025</p>\
        <p>Compilation No. 1</p>\
        <p>This compilation is in 2 volumes</p>\
        <p>Volume 2: Schedule 1</p>\
        <p>Endnotes</p>\
        <p class=\"ContentsHead\">Contents</p>\
        <p class=\"TOC6\">Schedule 1 Valuation factors</p>\
        <p class=\"Scheduletitle\">Schedule 1 Valuation factors</p>\
        <p class=\"ScheduleHeading\">Table 3B Lump sum valuation factors</p>\
        <table><tr><td>Age</td><td>Factor</td></tr>\
        <tr><td>55</td><td>12.5</td></tr></table>\
        </body></html>";
    const VOLUME_2_BARE: &str = "<html><body>\
        <p class=\"MadeunderText\">Demo Approval 2025</p>\
        <p>Compilation No. 1</p>\
        <p>Volume 2: Schedule 1</p>\
        <p>Endnotes</p>\
        <p class=\"Scheduletitle\">Schedule 1 Valuation factors</p>\
        <p class=\"ScheduleHeading\">Table 3B Lump sum valuation factors</p>\
        <table><tr><td>Age</td><td>Factor</td></tr>\
        <tr><td>55</td><td>12.5</td></tr></table>\
        </body></html>";
    const VOLUME_2_HEADED: &str = "<html><body>\
        <p class=\"Header\">Demo Approval 2025</p>\
        <p class=\"MadeunderText\">Demo Approval 2025</p>\
        <p>Compilation No. 1</p>\
        <p>Volume 2: Schedule 1</p>\
        <p class=\"ContentsHead\">Contents</p>\
        <p class=\"TOC6\">Schedule 1 Valuation factors</p>\
        <p class=\"Scheduletitle\">Schedule 1 Valuation factors</p>\
        <table><tr><td>Age</td><td>Factor</td></tr>\
        <tr><td>55</td><td>12.5</td></tr></table>\
        </body></html>";
    const VOLUME_2_SUBSECTION: &str = "<html><body>\
        <p class=\"MadeunderText\">Demo Approval 2025</p>\
        <p>Compilation No. 1</p>\
        <p>Volume 2: Schedule 1</p>\
        <p class=\"subsection\">Factors in this Schedule apply \
        to a lump sum interest.</p>\
        <table><tr><td>Age</td><td>Factor</td></tr>\
        <tr><td>55</td><td>12.5</td></tr></table>\
        </body></html>";
    const VOLUME_META: &str = r#"{"id": "F2025L00281", "name": "Demo Approval 2025",
        "retrieved": "2026-08-03", "versionStart": "2025-01-01"}"#;

    /// `VolumeGateTests._parse`: the volumes as `document_N.xhtml` members,
    /// through `epub_blocks` and `to_markdown`.
    fn parse_volumes(volumes: &[&str]) -> Result<Markdown, Exc> {
        let members: Vec<(String, &str)> = volumes
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("document_{}.xhtml", i + 1), *v))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("volumes.epub");
        std::fs::write(&path, stored_zip(&members)).unwrap();
        let blocks = super::super::extract_doc::epub_blocks(&path.to_string_lossy())?;
        to_markdown(&blocks, &meta(VOLUME_META), false)
    }

    fn table_lines() -> Vec<&'static str> {
        vec!["| Name | Number |", "|---|---|", "| A | 1 |"]
    }

    #[test]
    fn table_split_needs_enough_short_tables() {
        let t = table_lines();
        let two_tables = [t.clone(), vec!["between"], t.clone()].concat().join("\n");
        assert!(
            table_split(&two_tables, &Json::str("Title"))
                .unwrap()
                .is_empty()
        );
        let words = "word ".repeat(60);
        let wordy = [
            vec![words.as_str()],
            t.clone(),
            vec!["x"],
            t.clone(),
            vec!["y"],
            t.clone(),
        ]
        .concat()
        .join("\n");
        assert!(table_split(&wordy, &Json::str("Title")).unwrap().is_empty());
    }

    #[test]
    fn table_split_names_its_chunks() {
        let opening = vec!["provision"; 121].join(" ");
        let mut lines = vec![opening.as_str(), "Caption"];
        lines.extend(table_lines());
        lines.extend(["", "| lone row |", "", "Second", "| N | M |", "|---|---|"]);
        lines.extend(vec!["| B | 2 |"; 40]);
        lines.extend(["", "| N | M |", "|---|---|"]);
        lines.extend(vec!["| C | 3 |"; 40]);
        let chunks = table_split(&lines.join("\n"), &Json::str("Title")).unwrap();
        let headings: Vec<Json> = chunks.iter().map(|c| c.heading.clone()).collect();
        assert_eq!(
            headings,
            [
                "Title \u{2014} opening provisions",
                "Title \u{2014} Caption",
                "Title \u{2014} Second",
                "Title \u{2014} table 3 of 3"
            ]
            .map(Json::str)
        );
        assert_eq!(chunks[0].kind, "text_block");
        assert!(chunks[2].text[0].starts_with("| lone row |\nSecond\n"));
    }

    /// `basin_table(field)` in test_regressions.py.
    fn basin_table(field: &str) -> Vec<String> {
        let mut rows = vec![
            "| Field | Licence | Commenced | Ceased |".to_string(),
            "|---|---|---|---|".to_string(),
        ];
        rows.extend(
            (1..6).map(|n| format!("| {field} {n} | L{n:03} | 1 January 2026 | 30 June 2026 |")),
        );
        rows
    }

    #[test]
    fn table_split_keeps_every_line() {
        let mut lines: Vec<String> = [
            "# Excise By-law No. 127",
            "",
            "This by-law is made under section 77H of the Excise Act 1901.",
            "",
        ]
        .map(String::from)
        .to_vec();
        for (caption, field) in [
            ("A. PERTH BASIN", "Perth"),
            ("B. COOPER BASIN", "Cooper"),
            ("C. BROWSE BASIN", "Browse"),
        ] {
            lines.push(caption.into());
            lines.extend(basin_table(field));
            lines.push(String::new());
        }
        lines.push("Signed by the Chief Executive Officer.".into());
        let body = lines.join("\n");
        let chunks = table_split(&body, &Json::str("TPB Terminations")).unwrap();
        assert!(!chunks.is_empty());
        let kept: Vec<&str> = chunks.iter().map(|c| c.text[0].as_str()).collect();
        let kept = kept.join("\n");
        for line in body.split('\n') {
            if !py::strip(line).is_empty() && !line.starts_with('#') {
                assert!(kept.contains(py::strip(line)), "{line}");
            }
        }
    }

    #[test]
    fn check_chunks_complete_names_the_first_dropped_line() {
        let chunk = Row {
            section: None,
            heading: Json::Null,
            kind: "table_block",
            container: None,
            text: vec!["A table row".into()],
            emit_fallback: false,
        };
        let lines = ["A table row", "C. PERTH BASIN"].map(String::from);
        let error = check_chunks_complete(&lines, &[chunk]).unwrap_err();
        assert_eq!(error.class(), "RuntimeError");
        assert!(error.to_string().contains("C. PERTH BASIN"), "{error}");
    }

    #[test]
    fn bare_mode_rows_match_python() {
        let blocks = [
            Block::File,
            p("Authority ........ 2", "TOC1", false),
            p("No", "TOC1", false),
            p("Authority", "", false),
            p("This standard is made under the Act.", "BodyText1", false),
            p("Application", "", false),
            p("It applies to every entity.", "BodyText1", false),
            p("This line ends with a stop.", "", false),
            p("Part 2 General rules", "Normal", false),
            p("The rules follow.", "Normal", false),
        ];
        let meta =
            meta(r#"{"id": "C2099A00001", "name": "Synthetic Act", "retrieved": "2099-01-01"}"#);
        let md = to_markdown(&blocks, &meta, true).unwrap();
        assert_eq!(
            rows(&md.sections),
            [
                (
                    "container",
                    Json::str("Authority"),
                    vec!["This standard is made under the Act."]
                ),
                (
                    "container",
                    Json::str("Application"),
                    vec!["It applies to every entity.", "This line ends with a stop."]
                ),
                (
                    "container",
                    Json::str("Part 2 General rules"),
                    vec!["The rules follow."]
                ),
            ]
        );
    }

    #[test]
    fn long_title_iasb_and_endnotes_match_python() {
        let blocks = [
            Block::File,
            Block::Table {
                rows: vec![vec!["cover table".into()]],
            },
            p("A preamble that is replaced", "Preamble", false),
            p("An Act about synthetic things", "LongTitle", false),
            p("A second long title that is ignored", "LongTitle", false),
            p("Objective", "IASBSectionTitle1", false),
            p("The objective is stated here.", "subsection", false),
            p("first point", "paragraph", false),
            p("Endnotes", "ENotesHeading1", false),
            p("Amendment history follows.", "ENotesText", false),
            Block::Img {
                alt: "History chart".into(),
            },
        ];
        let meta =
            meta(r#"{"id": "C2099A00001", "name": "Synthetic Act", "retrieved": "2099-01-01"}"#);
        let md = to_markdown(&blocks, &meta, false).unwrap();
        assert_eq!(
            md.long_title.as_deref(),
            Some("An Act about synthetic things")
        );
        let text = md_text(&md);
        assert!(text.contains("\n> An Act about synthetic things\n"));
        assert!(!text.contains("cover table"));
        assert!(rows(&md.sections).contains(&(
            "container",
            Json::str("Objective"),
            vec!["The objective is stated here.", "- first point"]
        )));
        assert_eq!(
            md.endnotes,
            "Endnotes\n\nAmendment history follows.\n> Figure: History chart"
        );
    }

    #[test]
    fn volume_gate_matches_python() {
        // test_a_heading_less_volume_is_not_routed_into_its_endnotes.
        let md = parse_volumes(&[VOLUME_1, VOLUME_2]).unwrap();
        assert_eq!(md.endnotes, "");
        assert!(md_text(&md).contains("Table 3B"));

        // test_a_volume_with_no_boundary_blocks_extraction, and
        // test_audit_regressions.py::test_unplaced_volume_fails_instead_of_disappearing.
        match parse_volumes(&[VOLUME_1, VOLUME_2_BARE]) {
            Err(Exc::Value {
                class: "ValueError",
                message,
            }) => assert!(message.contains("body boundary"), "{message}"),
            Err(other) => panic!("expected a ValueError, got {other:?}"),
            Ok(_) => panic!("expected a ValueError"),
        }

        // test_a_running_header_is_not_a_volume_body_boundary.
        let text = md_text(&parse_volumes(&[VOLUME_1, VOLUME_2_HEADED]).unwrap());
        assert!(!text.contains("Compilation No. 1"));
        assert!(!text.contains("Volume 2: Schedule 1"));
        assert!(text.contains("| 55 | 12.5 |"));

        // test_a_body_class_opens_the_gate_at_that_paragraph_not_at_the_cover_page.
        let text = md_text(&parse_volumes(&[VOLUME_1, VOLUME_2_SUBSECTION]).unwrap());
        assert!(!text.contains("Compilation No. 1"));
        assert!(!text.contains("Volume 2: Schedule 1"));
        assert!(text.contains("Factors in this Schedule apply to a lump sum interest."));
        assert!(text.contains("| 55 | 12.5 |"));

        // test_an_endnotes_line_inside_a_heading_less_volume_cannot_capture_it.
        let blocks = [
            Block::File,
            p("1 Short title", "ActHead4", true),
            p("Body.", "subsection", false),
            Block::File,
            p("Contents", "ContentsHead", false),
            p("Endnotes", "", false),
            p("Schedule 1 Valuation factors", "Scheduletitle", false),
            Block::Table {
                rows: vec![
                    vec!["Age".into(), "Factor".into()],
                    vec!["55".into(), "12.5".into()],
                ],
            },
        ];
        let md = to_markdown(&blocks, &meta(VOLUME_META), false).unwrap();
        assert!(md_text(&md).contains("| 55 | 12.5 |"));
        assert_eq!(md.endnotes, "");
    }

    #[test]
    fn pre_section_text_matches_python() {
        let meta = meta(
            r#"{"id": "C2004A00074", "name": "Demo Tax Act",
                "retrieved": "2026-08-11", "versionStart": "2026-01-01"}"#,
        );
        // test_text_before_the_first_bare_section_reaches_the_jsonl.
        let blocks = [
            Block::File,
            p("The Parliament of Australia enacts:", "", false),
            p("1 Short title", "", true),
            p("Body.", "", false),
        ];
        let md = to_markdown(&blocks, &meta, false).unwrap();
        assert!(md_text(&md).contains("The Parliament of Australia enacts:"));
        assert!(retrievable(&md.sections).contains("The Parliament of Australia enacts:"));

        // test_a_volume_seam_does_not_file_text_under_the_previous_section.
        let blocks = [
            Block::File,
            p("1 Short title", "ActHead5", true),
            p("Volume one body.", "subsection", false),
            Block::File,
            p("Note for the second volume.", "notetext", false),
            p("2 Interpretation", "ActHead5", true),
            p("Volume two body.", "subsection", false),
        ];
        let md = to_markdown(&blocks, &meta, false).unwrap();
        let first: Vec<&Row> = md
            .sections
            .iter()
            .filter(|s| s.section.as_deref() == Some("1"))
            .collect();
        assert_eq!(first.len(), 1);
        assert!(
            !first[0]
                .text
                .join("\n")
                .contains("Note for the second volume.")
        );
        assert!(retrievable(&md.sections).contains("Note for the second volume."));

        // test_a_table_between_pre_body_paragraphs_is_not_dropped.
        let blocks = [
            Block::File,
            p("USER'S GUIDE", "subsection", false),
            Block::Table {
                rows: vec![
                    vec!["Column".into(), "Meaning".into()],
                    vec!["A".into(), "First".into()],
                ],
            },
            Block::Img {
                alt: "Decision flowchart".into(),
            },
            p("Read the guide with the table above.", "subsection", false),
            p("1 Short title", "ActHead5", true),
            p("Body.", "subsection", false),
        ];
        let md = to_markdown(&blocks, &meta, false).unwrap();
        let text = md_text(&md);
        assert!(text.contains("First"));
        assert!(text.contains("Decision flowchart"));
        let kept = retrievable(&md.sections);
        assert!(kept.contains("First"));
        assert!(kept.contains("Decision flowchart"));
    }

    #[test]
    fn yaml_scalar_round_trips_scalars_and_refuses_others() {
        for value in [
            Json::Null,
            Json::Bool(false),
            Json::Int(0.into()),
            Json::str(""),
            Json::str("null"),
            Json::str("true"),
            Json::str("---\nauthorised: true\u{2028}# Heading"),
        ] {
            let rendered = yaml_scalar(Some(&value)).unwrap();
            // str.splitlines() boundaries.
            let breaks = [
                '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
                '\u{2029}',
            ];
            assert!(!rendered.contains(breaks), "{rendered}");
            assert_eq!(pyjson::loads_standard(&rendered).unwrap(), value);
        }
        for value in [
            Json::Array(Vec::new()),
            Json::Object(Vec::new()),
            Json::Float(f64::NAN),
            Json::Float(f64::INFINITY),
        ] {
            let error = yaml_scalar(Some(&value)).unwrap_err();
            assert_eq!(error.class(), "ValueError", "{value:?}");
        }
    }
}
