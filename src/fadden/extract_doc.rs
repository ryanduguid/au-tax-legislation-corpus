//! `fadden/extract.py`'s `Doc` parser, `md_table` and `epub_blocks`: an
//! EPUB's XHTML documents as paragraph, image and table blocks.
//!
//! `Doc` subclasses CPython's `HTMLParser`; here [`htmlparser::parse`] gives
//! the events its handlers receive, in order, and a handler's `ValueError`
//! stops the document as the exception did.
use std::collections::{BTreeMap, HashMap};

use num_bigint::BigInt;

use super::Exc;
use super::epub;
use crate::htmlparser::{self, Event};
use crate::py;
use crate::pypath;
use crate::pyre::re;

const MAX_COLSPAN: i128 = 1024;
pub const MAX_TABLE_COLUMNS: usize = 4096;
pub const MAX_TABLE_CELLS: i128 = 1_000_000;
const MAX_TABLE_DEPTH: usize = 64;
const COAT_MAX_PX: u32 = 200;
const COAT_MIN_BYTES: u64 = 5000;
const COAT_MIN_RATIO: f64 = 1.15;
const COAT_MAX_RATIO: f64 = 1.60;
const HTAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// One block `epub_blocks` returns.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    P {
        cls: String,
        text: String,
        sectno: bool,
    },
    Img {
        alt: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    /// The volume boundary before each document.
    File,
}

fn value_error(message: &str) -> Exc {
    Exc::value(message)
}

/// The table state `Doc` saves when a nested table opens.
#[derive(Default)]
struct Frame {
    table: Vec<Vec<String>>,
    row: Vec<String>,
    cell: Vec<String>,
    in_td: bool,
    buf: Vec<String>,
    colspan: i128,
    rowspan: i128,
    /// Column to remaining rowspan; None for `rowspan="0"`.
    spans: BTreeMap<usize, Option<i128>>,
    group: Option<String>,
    in_row: bool,
}

/// The table counters carried from one document to the next.
#[derive(Clone, Copy, Default)]
pub struct Counts {
    expanded: i128,
    rendered: i128,
    buffered: i128,
}

struct Doc<'a> {
    img_sizes: &'a HashMap<String, u64>,
    f: Frame,
    blocks: Vec<Block>,
    cls: String,
    in_p: bool,
    sectno: bool,
    tables: Vec<Frame>,
    counts: Counts,
}

/// `dict(attrs).get(name)`: the last value given, None for a bare attribute.
fn attr<'a>(attrs: &'a [(String, Option<String>)], name: &str) -> Option<Option<&'a str>> {
    attrs
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_deref())
}

/// `a.get(name) or default`.
fn attr_or<'a>(attrs: &'a [(String, Option<String>)], name: &str, default: &'a str) -> &'a str {
    match attr(attrs, name) {
        Some(Some(v)) if !v.is_empty() => v,
        _ => default,
    }
}

/// `os.path.basename(p)`.
pub fn basename(p: &str) -> String {
    pypath::split(p).1
}

/// `int(span)` for a span of at most 32 characters, or None for its ValueError.
fn span_int(span: &str) -> Option<i128> {
    py::parse_int(span, 10)
        .ok()
        .and_then(|v| i128::try_from(v).ok())
}

/// `px(name)`: the attribute's decimal digits as an int, 0 when there are
/// none or too many for `int()`.
fn px(attrs: &[(String, Option<String>)], name: &str) -> BigInt {
    let digits: String = attr_or(attrs, name, "0")
        .chars()
        .filter(|&c| py::is_decimal(c))
        .collect();
    if digits.is_empty() {
        return BigInt::from(0);
    }
    py::parse_int(&digits, 10).unwrap_or_default()
}

impl<'a> Doc<'a> {
    fn new(img_sizes: &'a HashMap<String, u64>, counts: Counts) -> Self {
        Doc {
            img_sizes,
            f: Frame {
                colspan: 1,
                rowspan: 1,
                ..Frame::default()
            },
            blocks: Vec::new(),
            cls: String::new(),
            in_p: false,
            sectno: false,
            tables: Vec::new(),
            counts,
        }
    }

    fn start(&mut self, tag: &str, attrs: &[(String, Option<String>)]) -> Result<(), Exc> {
        if tag == "p" || tag == "li" || HTAGS.contains(&tag) {
            self.flush();
            self.in_p = true;
            self.cls = match attr(attrs, "class") {
                Some(Some(v)) => v.to_string(),
                _ => String::new(),
            };
            self.sectno = false;
            self.f.buf.clear();
        } else if tag == "span" && attr_or(attrs, "class", "").contains("CharSectno") {
            self.sectno = true;
        } else if tag == "td" || tag == "th" {
            if !self.f.in_row || self.f.in_td {
                return Err(value_error(
                    "table cell outside a row or inside another cell",
                ));
            }
            self.f.in_td = true;
            self.f.cell.clear();
            self.f.buf.clear();
            let span = attr_or(attrs, "colspan", "1");
            if py::char_len(span) > 32 {
                return Err(value_error("table colspan exceeds the supported budget"));
            }
            self.f.colspan = span_int(span).map_or(1, |v| v.max(1));
            if self.f.colspan > MAX_COLSPAN {
                return Err(value_error("table colspan exceeds the supported budget"));
            }
            let span = attr_or(attrs, "rowspan", "1");
            if py::char_len(span) > 32 {
                return Err(value_error("table rowspan exceeds the supported budget"));
            }
            self.f.rowspan = span_int(span).map_or(1, |v| if v < 0 { 1 } else { v });
        } else if tag == "tr" {
            if self.tables.is_empty() || self.f.in_row {
                return Err(value_error(
                    "table row outside a table or inside another row",
                ));
            }
            if self.f.group.is_none() {
                self.f.group = Some("implicit".into());
            }
            let zeros = self.f.spans.values().filter(|n| n.is_none()).count();
            self.charge_cells(zeros as i128)?;
            self.f.in_row = true;
        } else if matches!(tag, "thead" | "tbody" | "tfoot") {
            if self.tables.is_empty() || self.f.in_row || !self.group_open() {
                return Err(value_error("table row group is unbalanced"));
            }
            self.finish_group()?;
            self.f.group = Some(tag.into());
        } else if matches!(tag, "caption" | "colgroup") && !self.tables.is_empty() {
            if self.f.in_row || !self.group_open() {
                return Err(value_error("table row group is unbalanced"));
            }
            self.finish_group()?;
        } else if tag == "table" {
            if !self.tables.is_empty() && !self.f.in_td {
                return Err(value_error("nested table outside a cell"));
            }
            if self.tables.len() >= MAX_TABLE_DEPTH {
                return Err(value_error(
                    "table nesting exceeds the supported depth budget",
                ));
            }
            self.flush();
            let fresh = Frame {
                colspan: 1,
                rowspan: 1,
                ..Frame::default()
            };
            self.tables.push(std::mem::replace(&mut self.f, fresh));
        } else if tag == "br" {
            self.f.buf.push(" ".into());
        } else if tag == "img" {
            self.image(attrs)?;
        }
        Ok(())
    }

    fn group_open(&self) -> bool {
        matches!(self.f.group.as_deref(), None | Some("implicit"))
    }

    fn image(&mut self, attrs: &[(String, Option<String>)]) -> Result<(), Exc> {
        let alt = py::strip(attr_or(attrs, "alt", "")).to_string();
        let src = basename(attr_or(attrs, "src", ""));
        let (w, h) = (px(attrs, "width"), px(attrs, "height"));
        let b = self.img_sizes.get(&src).copied().unwrap_or(0);
        // `w / h` is a float division: CPython raises once the rounded
        // quotient reaches 2**1024, at w / h >= 2**1024 - 2**970.
        let zero = BigInt::from(0);
        let limit = (BigInt::from(1) << 1024) - (BigInt::from(1) << 970);
        if h != zero && w >= &h * &limit {
            return Err(Exc::Overflow(
                "integer division result too large for a float".into(),
            ));
        }
        let small = |v: &BigInt| u32::try_from(v).ok().filter(|&n| 0 < n && n < COAT_MAX_PX);
        let ratio = match (small(&w), small(&h)) {
            (Some(w), Some(h)) => Some(f64::from(w) / f64::from(h)),
            _ => None,
        };
        let is_arms = alt.to_ascii_lowercase().contains("coat of arms")
            || (alt.is_empty()
                && b >= COAT_MIN_BYTES
                && ratio.is_some_and(|r| COAT_MIN_RATIO < r && r < COAT_MAX_RATIO));
        let text = if is_arms {
            "[Commonwealth Coat of Arms omitted, not licensed under CC BY]".to_string()
        } else if !alt.is_empty() {
            alt
        } else {
            format!("[image not described in source: {src}]")
        };
        if self.f.in_td {
            self.f.cell.push(format!("> Figure: {text}"));
        } else {
            self.blocks.push(Block::Img { alt: text });
        }
        Ok(())
    }

    fn startend(&mut self, tag: &str, attrs: &[(String, Option<String>)]) -> Result<(), Exc> {
        self.start(tag, attrs)?;
        if matches!(
            tag,
            "td" | "th" | "tr" | "table" | "thead" | "tbody" | "tfoot"
        ) {
            self.end(tag)?;
        }
        Ok(())
    }

    fn end(&mut self, tag: &str) -> Result<(), Exc> {
        if (tag == "p" || tag == "li" || HTAGS.contains(&tag)) && self.in_p {
            self.flush();
        } else if tag == "td" || tag == "th" {
            self.end_cell()?;
        } else if tag == "tr" {
            self.end_row()?;
        } else if matches!(tag, "thead" | "tbody" | "tfoot") {
            if self.f.group.as_deref() != Some(tag) || self.f.in_row {
                return Err(value_error("table row group is unbalanced"));
            }
            self.finish_group()?;
        } else if tag == "table" && !self.tables.is_empty() {
            self.end_table()?;
        }
        Ok(())
    }

    fn end_cell(&mut self) -> Result<(), Exc> {
        if !self.f.in_td {
            return Err(value_error("table cell is unbalanced"));
        }
        self.flush();
        let mut column = self.f.row.len();
        while self.f.spans.contains_key(&column) {
            column += 1;
        }
        let end = column + self.f.colspan as usize;
        if end > MAX_TABLE_COLUMNS {
            return Err(value_error("table row exceeds the supported column budget"));
        }
        if self.f.spans.range(column..end).next().is_some() {
            return Err(value_error("table cell spans overlap"));
        }
        let growth = (end - self.f.row.len()) as i128;
        self.check_buffer_growth(growth)?;
        self.charge_cells(self.f.colspan * self.f.rowspan.max(1))?;
        self.counts.buffered += growth;
        let parts: Vec<&str> = self
            .f
            .cell
            .iter()
            .map(String::as_str)
            .filter(|x| !x.is_empty())
            .collect();
        let cell = py::strip(&parts.join(" ")).to_string();
        self.f.row.resize(column, String::new());
        self.f.row.push(cell);
        self.f.row.extend(std::iter::repeat_n(
            String::new(),
            self.f.colspan as usize - 1,
        ));
        if self.f.rowspan != 1 {
            let n = (self.f.rowspan != 0).then_some(self.f.rowspan);
            for c in column..end {
                self.f.spans.insert(c, n);
            }
        }
        self.f.colspan = 1;
        self.f.rowspan = 1;
        self.f.cell.clear();
        self.f.in_td = false;
        Ok(())
    }

    fn end_row(&mut self) -> Result<(), Exc> {
        if !self.f.in_row || self.f.in_td {
            return Err(value_error("table row is unbalanced"));
        }
        let span_width = self.f.spans.keys().next_back().map_or(0, |c| c + 1);
        let width = self.f.row.len().max(span_width);
        // Even an empty source row retains two lists until table closure.
        let allocation = (width.max(1) - self.f.row.len()) as i128;
        self.check_buffer_growth(allocation)?;
        self.counts.buffered += allocation;
        self.f.row.resize(width, String::new());
        let row = std::mem::take(&mut self.f.row);
        self.f.table.push(row);
        self.f.spans = std::mem::take(&mut self.f.spans)
            .into_iter()
            .filter(|(_, n)| n.is_none_or(|n| n > 1))
            .map(|(c, n)| (c, n.map(|n| n - 1)))
            .collect();
        self.f.in_row = false;
        Ok(())
    }

    fn end_table(&mut self) -> Result<(), Exc> {
        if self.f.in_row || !self.group_open() {
            return Err(value_error("table is unbalanced"));
        }
        self.finish_group()?;
        let rows: Vec<Vec<String>> = std::mem::take(&mut self.f.table)
            .into_iter()
            .filter(|r| r.iter().any(|c| !py::strip(c).is_empty()))
            .collect();
        if !rows.is_empty() {
            let widest = rows.iter().map(Vec::len).max().unwrap_or(0);
            let rendered = rows.len() as i128 * widest as i128;
            if rendered > MAX_TABLE_CELLS - self.counts.rendered {
                return Err(value_error(
                    "document exceeds the supported padded table budget",
                ));
            }
            self.counts.rendered += rendered;
        }
        let nested = self.tables.last().is_some_and(|outer| outer.in_td);
        if !rows.is_empty() && !nested {
            self.blocks.push(Block::Table { rows: rows.clone() });
        }
        self.f = self.tables.pop().unwrap_or_default();
        if !rows.is_empty() && nested {
            let lines = md_table(&rows)?;
            self.f.cell.push(lines.join("<br>"));
        }
        Ok(())
    }

    fn charge_cells(&mut self, count: i128) -> Result<(), Exc> {
        if count > MAX_TABLE_CELLS - self.counts.expanded {
            return Err(value_error(
                "document exceeds the supported table cell budget",
            ));
        }
        self.counts.expanded += count;
        Ok(())
    }

    fn check_buffer_growth(&self, count: i128) -> Result<(), Exc> {
        if count > MAX_TABLE_CELLS - self.counts.buffered {
            return Err(value_error(
                "document exceeds the supported buffered table cell budget",
            ));
        }
        Ok(())
    }

    fn finish_group(&mut self) -> Result<(), Exc> {
        let implied = self.f.spans.values().flatten().copied().max().unwrap_or(0);
        let zeros = self.f.spans.values().filter(|n| n.is_none()).count() as i128;
        self.charge_cells(implied * zeros)?;
        self.f.spans.clear();
        self.f.group = None;
        Ok(())
    }

    /// `_text()`: the buffered text, spaces collapsed and stripped.
    fn text(&self) -> String {
        let joined = self.f.buf.concat().replace('\u{a0}', " ");
        let mut out = String::with_capacity(joined.len());
        let mut in_run = false;
        for c in joined.chars() {
            if c == ' ' || c == '\t' {
                if !in_run {
                    out.push(' ');
                }
                in_run = true;
            } else {
                out.push(c);
                in_run = false;
            }
        }
        py::strip(&out).to_string()
    }

    fn flush(&mut self) {
        if self.in_p || self.f.in_td {
            let t = self.text();
            if !t.is_empty() {
                if self.f.in_td {
                    self.f.cell.push(t);
                } else {
                    self.blocks.push(Block::P {
                        cls: self.cls.clone(),
                        text: t,
                        sectno: self.sectno,
                    });
                }
            }
        }
        self.in_p = false;
        self.sectno = false;
        self.f.buf.clear();
    }

    /// `d.feed(text); d.close(); d._flush()`.
    fn run(&mut self, text: &str) -> Result<(), Exc> {
        for positioned in htmlparser::parse(text, true) {
            match positioned.event {
                Event::StartTag(tag, attrs) => self.start(&tag, &attrs)?,
                Event::StartEndTag(tag, attrs) => self.startend(&tag, &attrs)?,
                Event::EndTag(tag) => self.end(&tag)?,
                Event::Data(data) => self.f.buf.push(data),
                _ => {}
            }
        }
        if !self.tables.is_empty() {
            return Err(value_error("table is unbalanced at end of document"));
        }
        self.flush();
        Ok(())
    }
}

/// `md_table(rows)`.
pub fn md_table(rows: &[Vec<String>]) -> Result<Vec<String>, Exc> {
    let w = rows.iter().map(Vec::len).max().unwrap_or(0);
    if w > MAX_TABLE_COLUMNS || (w * rows.len()) as i128 > MAX_TABLE_CELLS {
        return Err(value_error(
            "table exceeds the supported rectangular cell budget",
        ));
    }
    let line = |r: &Vec<String>| {
        let mut cells: Vec<String> = r.iter().map(|c| c.replace('|', "\\|")).collect();
        cells.resize(w, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut out = vec![line(&rows[0]), format!("|{}|", vec!["---"; w].join("|"))];
    out.extend(rows[1..].iter().map(line));
    Ok(out)
}

/// `n.lower().endswith((".html", ".xhtml"))`. No character outside ASCII
/// lowercases to one of these letters, so an ASCII comparison is exact.
fn is_document(name: &str) -> bool {
    let tail = |suffix: &str| {
        let n = suffix.len();
        let last: Vec<char> = name.chars().rev().take(n).collect();
        last.len() == n
            && last
                .iter()
                .rev()
                .zip(suffix.chars())
                .all(|(c, s)| c.is_ascii() && c.to_ascii_lowercase() == s)
    };
    tail(".html") || tail(".xhtml")
}

/// `epub_blocks(path)`.
pub fn epub_blocks(path: &str) -> Result<Vec<Block>, Exc> {
    let archive = epub::open(path)?;
    let mut keyed = Vec::new();
    for name in archive.namelist().filter(|n| is_document(n)) {
        let number = match re(r"document_(\d+)", 0).search(name) {
            Some(m) => py::parse_int(&m[1], 10).map_err(Exc::value)?,
            None => BigInt::from(0),
        };
        keyed.push((number, name.to_string()));
    }
    keyed.sort();
    let mut sizes = HashMap::new();
    for member in &archive.members {
        sizes.insert(basename(&member.filename), member.file_size);
    }
    let mut out = Vec::new();
    let mut counts = Counts::default();
    for (_, name) in keyed {
        let bytes = archive.read(&name)?;
        let mut doc = Doc::new(&sizes, counts);
        doc.run(&String::from_utf8_lossy(&bytes))?;
        counts = doc.counts;
        out.push(Block::File);
        out.append(&mut doc.blocks);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(html: &str) -> Result<Vec<Block>, String> {
        let sizes = HashMap::new();
        let mut doc = Doc::new(&sizes, Counts::default());
        doc.run(html).map_err(|e| e.to_string())?;
        Ok(doc.blocks)
    }

    #[test]
    fn spans_fill_their_columns() {
        let html = "<table><tr><td rowspan=2>a</td><td>b</td></tr><tr><td>c</td></tr></table>";
        let rows = vec![
            vec!["a".to_string(), "b".to_string()],
            vec![String::new(), "c".to_string()],
        ];
        assert_eq!(blocks(html).unwrap(), vec![Block::Table { rows }]);
    }

    #[test]
    fn unbalanced_tables_and_huge_images_fail_as_python_does() {
        assert_eq!(
            blocks("<td>x</td>").unwrap_err(),
            "table cell outside a row or inside another cell"
        );
        assert_eq!(
            blocks("<table><tr><td>x").unwrap_err(),
            "table is unbalanced at end of document"
        );
        let wide = format!("<img width=\"{}\" height=\"1\">", "9".repeat(400));
        assert_eq!(
            blocks(&wide).unwrap_err(),
            "integer division result too large for a float"
        );
    }

    #[test]
    fn coat_of_arms_needs_every_signal() {
        let mut sizes = HashMap::new();
        sizes.insert("arms.png".to_string(), 6000);
        let mut doc = Doc::new(&sizes, Counts::default());
        doc.run("<img src=\"x/arms.png\" width=\"130\" height=\"100\">")
            .unwrap();
        assert_eq!(
            doc.blocks,
            vec![Block::Img {
                alt: "[Commonwealth Coat of Arms omitted, not licensed under CC BY]".into()
            }]
        );
        assert!(is_document("OEBPS/Document_1.XHTML"));
        assert!(!is_document("a.htm"));
    }
}
