//! `fadden/rates.py`: derive a rates-and-thresholds index from the corpus on disk.
//!
//! No network. Reads every listed title's sections.jsonl and keeps the
//! provisions that carry an operative number: rate tables, percentages,
//! dollar thresholds and the indexation machinery that moves them. Every
//! record cites its title, compilation and section so the number can be
//! checked against the provision. This is a finding aid: nothing here
//! computes a rate or an amount, and it is no substitute for reading the law.
//!
//! Text is handled as code points, as Python's `str` holds it, so a lone
//! surrogate a JSON escape left in a row reaches the output writer, which
//! refuses it as strict UTF-8 does.
use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::corpus_paths::{
    child, corpus_root, markdown_text, refuse_unlisted_titles, register_id, register_id_str,
    require_builder_layout, script_dir, write_text_atomic,
};
use super::{Exc, Run, get, getitem, iterate, json_error};
use crate::argparse::Exit;
use crate::py;
use crate::pyio::Lines;
use crate::pyjson::{self, Json, PyStr};
use crate::pyre::{self, Re};
use crate::pyvalue::{self, Key, dumps_line, repr, str_points, type_name};

type P = Vec<u32>;

const FACTOR: &str = "(?<![\\d.])\\d\\.\\d{1,4}(?![\\d])";
const RATE_PHRASE: &str =
    "(rate[s]? (?:is|are|of)|percentage|factor|multiplied by|gross[\u{2011}-]up|indexation factor)";
const THRESHOLD_PHRASE: &str =
    "(threshold|limit|cap|maximum|minimum|exceeds|does not exceed|tax[\u{2011}-]free)";
const INDEX_PHRASE: &str =
    "(index(ed|ation)|CPI|consumer price index|AWOTE|average weekly ordinary time earnings)";
const YEAR: &str = "\\b(19|20)\\d{2}[\u{2011}-]\\d{2}\\b|\\b(19|20)\\d{2}\\b";

const OWNERSHIP_PHRASES: [&str; 9] = [
    "continuity of ownership",
    "beneficial own",
    "beneficial entitled",
    "beneficially own",
    "beneficially entitled",
    "wholly-owned",
    "wholly\u{2011}owned",
    "majority-owned",
    "majority\u{2011}owned",
];

/// Topic buckets, matched against the heading, provision text and title name.
/// Order matters: the first match wins, so the specific come before the general.
const TOPICS: [(&str, &str); 19] = [
    (
        "customs tariff schedules",
        "customs tariff|tariff (classification|schedule|item)|chapter \\d+ .*(goods|articles)",
    ),
    (
        "excise and fuel",
        "excise|fuel tax credit|diesel|petroleum product",
    ),
    (
        "income tax rates",
        "income tax.*rate|rates act|marginal rate|tax[\u{2011}-]free threshold|resident taxpayer|non[\u{2011}-]resident.*rate",
    ),
    ("medicare levy", "medicare"),
    (
        "fringe benefits tax",
        "fringe benefit|(?<![A-Za-z])FBT(?![A-Za-z])|gross[\u{2011}-]up",
    ),
    (
        "superannuation",
        "superannuation|super(annuation)? (guarantee|contribution|fund)|concessional cap|transfer balance|(?<![A-Za-z])SMSF(?![A-Za-z])",
    ),
    (
        "gst and indirect",
        "goods and services tax|(?<![A-Za-z])GST(?![A-Za-z])|wine equalisation|luxury car tax",
    ),
    (
        "capital gains",
        "capital gain|(?<![A-Za-z])CGT(?![A-Za-z])|cost base|discount capital",
    ),
    (
        "capital allowances and depreciation",
        "depreciat|capital allowance|decline in value|effective life|car limit|low[\u{2011}-]value pool",
    ),
    (
        "companies and franking",
        "franking|imputation|dividend|company tax|corporate tax rate|base rate entity",
    ),
    (
        "trusts and partnerships",
        "(?<![A-Za-z])trust(?![A-Za-z])|trustee|partnership|beneficiary",
    ),
    (
        "deductions and offsets",
        "deduction|offset|rebate|substantiat|entertainment|gift|donation",
    ),
    (
        "withholding and PAYG",
        "withholding|(?<![A-Za-z])PAYG(?![A-Za-z])|tax file number|instal?ment",
    ),
    (
        "family assistance",
        "family assistance|family tax benefit|child care",
    ),
    (
        "petroleum and mining",
        "petroleum resource rent|minerals resource rent|mining",
    ),
    (
        "R&D and incentives",
        "research and development|(?<![A-Za-z])R&D(?![A-Za-z])|incentive|concession",
    ),
    ("small business", "small business|aggregated turnover"),
    (
        "administration and penalties",
        "penalt|shortfall|general interest charge|(?<![A-Za-z])GIC(?![A-Za-z])|(?<![A-Za-z])SIC(?![A-Za-z])|administrative|objection|assessment",
    ),
    (
        "international",
        "foreign|non[\u{2011}-]resident|transfer pricing|thin capitalisation|double tax|treaty",
    ),
];

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

fn pstr(points: P) -> Json {
    Json::Str(PyStr::from_code_points(points))
}

/// The text a pattern searches: a lone surrogate, which none of these
/// patterns names, stands in as U+FFFD so `.` still matches it.
fn searchable(points: &[u32]) -> String {
    pyvalue::points_lossy(points)
}

fn search(pattern: &str, flags: u8, points: &[u32]) -> bool {
    pyre::re(pattern, flags).is_match(&searchable(points))
}

fn whole_matches(re: &Re, points: &[u32]) -> Vec<P> {
    re.captures_iter(&searchable(points))
        .map(|c| pts(c.get(0).map_or("", |m| m.as_str())))
        .collect()
}

fn is_decimal(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(py::is_decimal)
}

fn is_space(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(py::is_space)
}

fn strip(points: &[u32]) -> &[u32] {
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

/// `_consume_digits`: the end of one forward-only decimal token.
fn consume_digits(text: &[u32], mut index: usize, allow_commas: bool) -> usize {
    let start = index;
    while index < text.len() {
        let c = text[index];
        let comma = allow_commas
            && c == u32::from(',')
            && index > start
            && index + 1 < text.len()
            && is_decimal(text[index + 1]);
        if !(is_decimal(c) || comma) {
            break;
        }
        index += 1;
    }
    index
}

/// `money_values(text)`: well-formed dollar amounts.
pub fn money_values(text: &[u32]) -> Vec<P> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < text.len() {
        if text[index] != u32::from('$') {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        if index < text.len() && is_space(text[index]) {
            index += 1;
        }
        let number_start = index;
        index = consume_digits(text, index, true);
        if index == number_start {
            continue;
        }
        if index < text.len() && text[index] == u32::from('.') {
            let fraction_start = index + 1;
            let fraction_end = consume_digits(text, fraction_start, false);
            if fraction_end > fraction_start {
                index = fraction_end;
            }
        }
        values.push(text[start..index].to_vec());
    }
    values
}

/// `percentage_matches(text)`: `(value, start, end)` for `10%` and `12.5 %`.
pub fn percentage_matches(text: &[u32]) -> Vec<(P, usize, usize)> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < text.len() {
        if !is_decimal(text[index]) || (index > 0 && is_decimal(text[index - 1])) {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = consume_digits(text, index, false);
        if end < text.len() && text[end] == u32::from('.') {
            let fraction_start = end + 1;
            let fraction_end = consume_digits(text, fraction_start, false);
            if fraction_end > fraction_start {
                end = fraction_end;
            }
        }
        if end < text.len() && is_space(text[end]) {
            end += 1;
        }
        if end < text.len() && text[end] == u32::from('%') {
            values.push((text[start..=end].to_vec(), start, end + 1));
            index = end + 1;
        } else {
            index = end;
        }
    }
    values
}

pub fn percentage_values(text: &[u32]) -> Vec<P> {
    percentage_matches(text)
        .into_iter()
        .map(|(v, _, _)| v)
        .collect()
}

fn casefold(points: &[u32]) -> P {
    let mut out = Vec::with_capacity(points.len());
    for &cp in points {
        match char::from_u32(cp) {
            Some(c) => out.extend(py::casefold(&c.to_string()).chars().map(u32::from)),
            None => out.push(cp),
        }
    }
    out
}

fn contains(haystack: &[u32], needle: &str) -> bool {
    let needle = pts(needle);
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

fn starts_with_ownership_word(text: &[u32], index: usize) -> bool {
    ["stake", "subsidiary", "interest"].iter().any(|word| {
        let word = pts(word);
        let end = index + word.len();
        text.get(index..end) == Some(word.as_slice())
            && (end == text.len() || !py::is_alpha(text[end]))
    })
}

/// `is_ownership_test(text)`: ownership and control tests carry a `%` but are
/// not rates of tax.
pub fn is_ownership_test(text: &[u32]) -> bool {
    let lower = casefold(text);
    if OWNERSHIP_PHRASES.iter().any(|p| contains(&lower, p)) {
        return true;
    }
    for left in [
        "voting",
        "dividend",
        "capital",
        "ownership",
        "control",
        "equity",
    ] {
        for right in ["interest", "right", "stake", "power"] {
            if contains(&lower, &format!("{left} {right}")) {
                return true;
            }
        }
    }
    // As Python does, the position found in the text indexes its casefolded copy.
    percentage_matches(text).into_iter().any(|(_, _, end)| {
        end < text.len()
            && text[end] == u32::from(' ')
            && starts_with_ownership_word(&lower, end + 1)
    })
}

/// `topic_for(act, heading, body)`: the first topic whose pattern matches.
pub fn topic_for(act: &Json, heading: &Json, body: &[u32]) -> &'static str {
    let mut hay = if heading.truthy() {
        str_points(heading)
    } else {
        Vec::new()
    };
    hay.push(u32::from(' '));
    hay.extend_from_slice(&body[..body.len().min(300)]);
    hay.push(u32::from(' '));
    hay.extend(str_points(act));
    TOPICS
        .iter()
        .find(|(_, pattern)| search(pattern, pyre::I, &hay))
        .map_or("other", |(name, _)| name)
}

/// `table_blocks(text)`: contiguous Markdown tables as lists of lines.
pub fn table_blocks(text: &[u32]) -> Vec<Vec<&[u32]>> {
    let mut blocks = Vec::new();
    let mut current: Vec<&[u32]> = Vec::new();
    for line in text.split(|&cp| cp == 0x0a) {
        if line.first() == Some(&u32::from('|')) {
            current.push(line);
        } else if !current.is_empty() {
            blocks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn joined(lines: &[&[u32]], separator: char) -> P {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(u32::from(separator));
        }
        out.extend_from_slice(line);
    }
    out
}

pub fn is_rate_table(lines: &[&[u32]]) -> bool {
    let body = joined(lines, ' ');
    !money_values(&body).is_empty() || !percentage_values(&body).is_empty()
}

/// `sentences(text)`: the provision split at paragraph breaks and at `.`, `;`
/// or `:` before a capital, bracket, bullet or asterisk.
pub fn sentences(text: &[u32]) -> Vec<P> {
    let kept: Vec<&[u32]> = text
        .split(|&cp| cp == 0x0a)
        .filter(|line| line.first() != Some(&u32::from('|')))
        .collect();
    let plain = joined(&kept, '\n');
    let opener = |cp: u32| {
        char::from_u32(cp).is_some_and(|c| c.is_ascii_uppercase() || "(\u{2022}*".contains(c))
    };
    let (mut pieces, mut start, mut index) = (Vec::new(), 0, 0);
    while index < plain.len() {
        let c = plain[index];
        if c == 0x0a && index + 1 < plain.len() && plain[index + 1] == 0x0a {
            pieces.push(&plain[start..index]);
            index += 2;
            while index < plain.len() && plain[index] == 0x0a {
                index += 1;
            }
            start = index;
            continue;
        }
        if char::from_u32(c).is_some_and(|c| ".;:".contains(c)) {
            let mut following = index + 1;
            while following < plain.len() && is_space(plain[following]) {
                following += 1;
            }
            if following > index + 1 && following < plain.len() && opener(plain[following]) {
                pieces.push(&plain[start..=index]);
                start = following;
                index = following;
                continue;
            }
        }
        index += 1;
    }
    pieces.push(&plain[start..]);
    pieces
        .into_iter()
        .map(strip)
        .filter(|s| 15 < s.len() && s.len() < 600)
        .map(<[u32]>::to_vec)
        .collect()
}

/// `fold(t)`: the hyphens the source writes as U+2011 and its relatives, and
/// the no-break space, as plain ASCII.
fn fold(text: &[u32]) -> P {
    text.iter()
        .map(|&cp| match cp {
            0x2010..=0x2014 => u32::from('-'),
            0xa0 => u32::from(' '),
            other => other,
        })
        .collect()
}

/// `text = row.get("text") or ""` as the scanners will read it.
fn row_text(value: Option<&Json>) -> Result<P, Exc> {
    match value {
        Some(Json::Str(s)) => Ok(s.code_points()),
        Some(v) if !v.truthy() => Ok(Vec::new()),
        None => Ok(Vec::new()),
        // `len(text)` in money_values; a dict fails at `text[0]`.
        Some(Json::Object(_)) => Err(Exc::Key("0".into())),
        Some(Json::Array(_)) => Err(Exc::Type(
            "expected string or bytes-like object, got 'list'".into(),
        )),
        Some(other) => Err(Exc::Type(format!(
            "object of type '{}' has no len()",
            type_name(other)
        ))),
    }
}

struct Record {
    fields: Vec<(&'static str, Json)>,
    topic: &'static str,
    kind: &'static str,
    content: P,
    amounts: Option<Vec<P>>,
}

impl Record {
    fn get(&self, name: &str) -> &Json {
        self.fields
            .iter()
            .find(|(k, _)| *k == name)
            .map_or(&Json::Null, |(_, v)| v)
    }

    fn json(&self) -> Json {
        Json::Object(
            self.fields
                .iter()
                .map(|(k, v)| (PyStr::Text((*k).to_string()), v.clone()))
                .collect(),
        )
    }
}

fn sorted_set(values: impl IntoIterator<Item = P>) -> Json {
    let set: BTreeSet<P> = values.into_iter().collect();
    Json::Array(set.into_iter().map(pstr).collect())
}

fn number_cmp(a: &Json, b: &Json) -> Option<Option<Ordering>> {
    use num_bigint::BigInt;
    let int = |v: &Json| match v {
        Json::Bool(x) => Some(BigInt::from(u8::from(*x))),
        Json::Int(i) => Some(i.clone()),
        _ => None,
    };
    let int_float = |i: &BigInt, f: f64| -> Option<Ordering> {
        if f.is_nan() {
            return None;
        }
        if f.is_infinite() {
            return Some(if f > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let t = f.trunc();
        Some(match i.cmp(&pyvalue::float_to_int(t)) {
            Ordering::Equal => t.partial_cmp(&f).unwrap_or(Ordering::Equal),
            other => other,
        })
    };
    Some(match (a, b) {
        (Json::Float(x), Json::Float(y)) => x.partial_cmp(y),
        (Json::Float(x), other) => int_float(&int(other)?, *x).map(Ordering::reverse),
        (other, Json::Float(y)) => int_float(&int(other)?, *y),
        _ => Some(int(a)?.cmp(&int(b)?)),
    })
}

/// `a < b` for two JSON values, or the TypeError Python raises.
fn py_lt(a: &Json, b: &Json) -> Result<bool, Exc> {
    if let Some(order) = number_cmp(a, b) {
        return Ok(order == Some(Ordering::Less));
    }
    match (a, b) {
        (Json::Str(x), Json::Str(y)) => Ok(x.code_points() < y.code_points()),
        (Json::Array(x), Json::Array(y)) => {
            match x.iter().zip(y).position(|(p, q)| !pyvalue::item_eq(p, q)) {
                Some(i) => py_lt(&x[i], &y[i]),
                None => Ok(x.len() < y.len()),
            }
        }
        _ => Err(Exc::Type(format!(
            "'<' not supported between instances of '{}' and '{}'",
            type_name(a),
            type_name(b)
        ))),
    }
}

type SortKey = (&'static str, Json, P, &'static str);

/// `a < b` for the `(topic, act, str(section or ""), kind)` sort keys, as a
/// tuple compares: the first unequal item decides.
fn key_lt(a: &SortKey, b: &SortKey) -> Result<bool, Exc> {
    if a.0 != b.0 {
        return Ok(a.0 < b.0);
    }
    if !pyvalue::item_eq(&a.1, &b.1) {
        return py_lt(&a.1, &b.1);
    }
    if a.2 != b.2 {
        return Ok(a.2 < b.2);
    }
    Ok(a.3 < b.3)
}

/// `records.sort(key=...)`. With every title name a string the keys are
/// totally ordered and any stable sort agrees with Python's. Otherwise a
/// stable binary insertion sort meets Python's TypeError for keys that cannot
/// be ordered; which pair it meets first can differ from Python's merge.
fn sort_records(records: Vec<Record>) -> Result<Vec<Record>, Exc> {
    let keys: Vec<SortKey> = records
        .iter()
        .map(|r| {
            let section = r.get("section");
            let section = if section.truthy() {
                str_points(section)
            } else {
                Vec::new()
            };
            (r.topic, r.get("act").clone(), section, r.kind)
        })
        .collect();
    let mut order: Vec<usize> = (0..records.len()).collect();
    if keys.iter().all(|k| matches!(k.1, Json::Str(_))) {
        order.sort_by(|&i, &j| {
            if key_lt(&keys[i], &keys[j]).unwrap_or(false) {
                Ordering::Less
            } else if key_lt(&keys[j], &keys[i]).unwrap_or(false) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        });
    } else {
        for n in 1..order.len() {
            let item = order[n];
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if key_lt(&keys[item], &keys[order[mid]])? {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            order.remove(n);
            order.insert(lo, item);
        }
    }
    let mut slots: Vec<Option<Record>> = records.into_iter().map(Some).collect();
    Ok(order.into_iter().filter_map(|i| slots[i].take()).collect())
}

/// `collections.Counter` keeping first-insertion order and the first key seen.
#[derive(Default)]
struct Counter {
    items: Vec<(Key, Json, usize)>,
}

impl Counter {
    fn add(&mut self, value: &Json) -> Result<(), Exc> {
        let key = pyvalue::key(value).map_err(|m| {
            Exc::Type(format!(
                "cannot use '{}' as a dict key ({m})",
                type_name(value)
            ))
        })?;
        match self.items.iter_mut().find(|(k, _, _)| *k == key) {
            Some(item) => item.2 += 1,
            None => self.items.push((key, value.clone(), 1)),
        }
        Ok(())
    }

    /// `most_common()`: by count, ties in insertion order.
    fn most_common(&self) -> Vec<(&Json, usize)> {
        let mut items: Vec<(&Json, usize)> = self.items.iter().map(|(_, v, n)| (v, *n)).collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.1));
        items
    }
}

/// `dict(pairs)` as `print` shows it.
fn dict_repr(pairs: &[(&Json, usize)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, n)| format!("{}: {n}", repr(k)))
        .collect();
    format!("{{{}}}", body.join(", "))
}

/// `f"{n:,}"`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn md_json(value: &Json) -> P {
    markdown_text(&str_points(value))
}

/// `value or default`.
fn or<'a>(value: &'a Json, default: &'a Json) -> &'a Json {
    if value.truthy() { value } else { default }
}

/// `stale_source_lines(records)`: the warning for rows from a stale compilation.
fn stale_source_lines(records: &[Record]) -> Vec<P> {
    let mut stale: Vec<(String, &Record)> = Vec::new();
    for r in records {
        if matches!(r.get("version_is_current"), Json::Bool(false)) {
            let rid = r
                .get("register_id")
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            if !stale.iter().any(|(k, _)| *k == rid) {
                stale.push((rid, r));
            }
        }
    }
    if stale.is_empty() {
        return Vec::new();
    }
    stale.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = vec![
        pts(
            "> **Some entries come from a source that is already stale.** For the titles below \
             the Register records a later commencement with no published compilation, so these \
             entries are the last published text and do not reflect that change. Check the \
             provision on the Register before relying on a number.",
        ),
        Vec::new(),
    ];
    let dash = Json::str("-");
    for (rid, r) in stale {
        let mut line = pts("- ");
        line.extend(md_json(or(r.get("act"), &dash)));
        line.extend(pts(" ("));
        line.extend(markdown_text(&pts(&rid)));
        line.extend(pts("): compilation "));
        line.extend(md_json(or(r.get("compilation_number"), &dash)));
        line.extend(pts(" of "));
        line.extend(md_json(or(r.get("compilation_date"), &dash)));
        line.extend(pts(", `version_is_current: false`"));
        lines.push(line);
    }
    lines.push(Vec::new());
    lines
}

/// The rows of one title's sections.jsonl that carry an operative number.
fn read_title(
    markdown_root: &str,
    directory_id: &str,
    records: &mut Vec<Record>,
) -> Result<(), Exc> {
    let path = child(markdown_root, &[directory_id, "sections.jsonl"])?;
    let factor = pyre::re(FACTOR, 0);
    let year = pyre::re(YEAR, 0);
    let mut lines = Lines::open(&path)?;
    while let Some(line) = lines.next_line() {
        let line = line?;
        if py::strip(&line).is_empty() {
            continue;
        }
        let row = pyjson::loads_standard(&line).map_err(json_error)?;
        let text = get(&row, "text")?;
        let rid = register_id(getitem(&row, "register_id")?)?;
        if rid != directory_id {
            return Err(Exc::value(
                "section register ID does not match its title directory",
            ));
        }
        let text = row_text(text)?;
        let has_factor =
            |t: &[u32]| factor.is_match(&searchable(t)) && search(RATE_PHRASE, pyre::I, t);
        if money_values(&text).is_empty()
            && percentage_values(&text).is_empty()
            && !has_factor(&text)
        {
            continue;
        }
        let act = getitem(&row, "act")?.clone();
        let field = |name: &str| row.get(name).cloned().unwrap_or(Json::Null);
        let topic = topic_for(&act, &field("heading"), &text);
        let base: Vec<(&'static str, Json)> = vec![
            ("register_id", Json::str(&rid)),
            ("act", act),
            ("collection", field("collection")),
            ("compilation_number", field("compilation_number")),
            ("compilation_date", field("compilation_date")),
            ("section", field("section")),
            ("heading", field("heading")),
            ("register_page", field("register_page")),
            (
                "version_is_current",
                row.get("version_is_current")
                    .cloned()
                    .unwrap_or(Json::Bool(true)),
            ),
            ("source_url", field("source_url")),
            ("licence", field("licence")),
            ("licence_url", field("licence_url")),
            ("authorised", field("authorised")),
            ("attribution", field("attribution")),
            ("topic", Json::str(topic)),
        ];

        for table in table_blocks(&text) {
            if table.len() >= 3 && is_rate_table(&table) {
                let content = joined(&table, '\n');
                let mut fields = base.clone();
                fields.push(("kind", Json::str("table")));
                fields.push((
                    "years",
                    sorted_set(whole_matches(&year, &joined(&table, ' '))),
                ));
                fields.push(("content", pstr(content.clone())));
                records.push(Record {
                    fields,
                    topic,
                    kind: "table",
                    content,
                    amounts: None,
                });
            }
        }

        for sentence in sentences(&text) {
            let money = money_values(&sentence);
            let percentages = percentage_values(&sentence);
            let factor_here = has_factor(&sentence);
            if money.is_empty() && percentages.is_empty() && !factor_here {
                continue;
            }
            let index_phrase = search(INDEX_PHRASE, pyre::I, &sentence);
            // A percentage stands on its own; a dollar amount needs a phrase,
            // because legislation is full of incidental sums.
            if percentages.is_empty()
                && !(search(RATE_PHRASE, pyre::I, &sentence)
                    || search(THRESHOLD_PHRASE, pyre::I, &sentence)
                    || index_phrase)
            {
                continue;
            }
            let has_pct = !percentages.is_empty();
            let kind = if index_phrase {
                "indexation"
            } else if has_pct && is_ownership_test(&sentence) {
                "ownership test"
            } else if has_pct {
                "rate"
            } else if factor_here && money.is_empty() {
                "factor"
            } else {
                "threshold"
            };
            let mut amounts: BTreeSet<P> = money.into_iter().chain(percentages).collect();
            if factor_here {
                amounts.extend(whole_matches(&factor, &sentence));
            }
            let amounts: Vec<P> = amounts.into_iter().collect();
            let mut fields = base.clone();
            fields.push(("kind", Json::str(kind)));
            fields.push((
                "amounts",
                Json::Array(amounts.iter().cloned().map(pstr).collect()),
            ));
            fields.push(("years", sorted_set(whole_matches(&year, &sentence))));
            fields.push(("content", pstr(sentence.clone())));
            records.push(Record {
                fields,
                topic,
                kind,
                content: sentence,
                amounts: Some(amounts),
            });
        }
    }
    Ok(())
}

fn line(text: &str) -> P {
    pts(text)
}

fn markdown(
    records: &[Record],
    by_kind: &Counter,
    by_coll: &Counter,
    by_topic: &Counter,
) -> Result<P, Exc> {
    let distinct: BTreeSet<String> = records
        .iter()
        .map(|r| {
            r.get("register_id")
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let mut md: Vec<P> = vec![
        line("# Rates, thresholds and indexation"),
        Vec::new(),
        line("Derived from the Acts and instruments in this corpus. Every entry cites its"),
        line("title, compilation and section. Check the provision before relying on a number:"),
        line("this is a finding aid, not advice, and not the authorised text."),
        Vec::new(),
        line(&format!(
            "{} entries across {} titles.",
            thousands(records.len()),
            thousands(distinct.len())
        )),
        Vec::new(),
    ];
    md.extend(stale_source_lines(records));
    for text in [
        "Each `rate_id` is an ordinal within this snapshot, not a persistent",
        "identifier. Adding or removing a title renumbers the entries after it, so",
        "cite the register id, section and snapshot rather than the ordinal alone.",
        "",
        "A rate set by an Act and a rate set by a determination made under it are not",
        "the same kind of thing: an instrument can be disallowed or sunset while its",
        "enabling Act stands. The `collection` field on every row says which you have.",
        "",
        "Figures the ATO computes rather than Parliament enacts are absent by",
        "construction, because they appear in no legislation: the FBT gross-up factors,",
        "the Division 7A benchmark rate and the indexed superannuation caps among them.",
        "The provisions here give the formula; the ATO publishes the result.",
        "",
        "| Kind | Count |",
        "|---|---|",
    ] {
        md.push(line(text));
    }
    for (k, n) in by_kind.most_common() {
        md.push(line(&format!(
            "| {} | {} |",
            k.as_str().unwrap_or_default(),
            thousands(n)
        )));
    }
    md.extend([
        Vec::new(),
        line("| Collection | Count |"),
        line("|---|---|"),
    ]);
    for (c, n) in by_coll.most_common() {
        let mut row = line("| ");
        row.extend(md_json(c));
        row.extend(line(&format!(" | {} |", thousands(n))));
        md.push(row);
    }
    md.extend([Vec::new(), line("| Topic | Count |"), line("|---|---|")]);
    for (t, n) in by_topic.most_common() {
        md.push(line(&format!(
            "| {} | {} |",
            t.as_str().unwrap_or_default(),
            thousands(n)
        )));
    }

    let act_default = Json::str("Act");
    let is_act = |r: &Record| pyvalue::py_eq(or(r.get("collection"), &act_default), &act_default);
    for (topic, _) in by_topic.most_common() {
        let topic = topic.as_str().unwrap_or_default();
        md.extend([Vec::new(), line(&format!("## {topic}")), Vec::new()]);
        let rs: Vec<&Record> = records.iter().filter(|r| r.topic == topic).collect();
        let tables: Vec<&&Record> = rs.iter().filter(|r| r.kind == "table").collect();
        if !tables.is_empty() {
            md.push(line("### Rate tables"));
            md.push(Vec::new());
            for r in tables.iter().take(40) {
                let mut row = line("**");
                row.extend(md_json(r.get("act")));
                row.extend(line("**"));
                if !is_act(r) {
                    row.extend(line(" _(instrument)_"));
                }
                row.push(u32::from(' '));
                if r.get("section").truthy() {
                    row.extend(line("s "));
                    row.extend(md_json(r.get("section")));
                }
                row.extend(line(": "));
                row.extend(md_json(or(r.get("heading"), &Json::str(""))));
                md.push(row);
                md.push(Vec::new());
                md.push(r.content.clone());
                md.push(Vec::new());
            }
            if tables.len() > 40 {
                md.push(line(&format!(
                    "_{} further tables in rates.jsonl._",
                    tables.len() - 40
                )));
                md.push(Vec::new());
            }
        }
        let others: Vec<&&Record> = rs.iter().filter(|r| r.kind != "table").collect();
        if !others.is_empty() {
            for text in [
                "### Provisions carrying a number",
                "",
                "| Title | Made by | Section | Kind | Amounts | Provision |",
                "|---|---|---|---|---|---|",
            ] {
                md.push(line(text));
            }
            for r in others.iter().take(120) {
                let amounts = r.amounts.as_deref().unwrap_or_default();
                let joined_amounts: Vec<&[u32]> = amounts.iter().map(Vec::as_slice).collect();
                let mut listed = Vec::new();
                for (i, a) in joined_amounts.iter().enumerate() {
                    if i > 0 {
                        listed.extend(line(", "));
                    }
                    listed.extend_from_slice(a);
                }
                listed.truncate(40);
                let cells: [P; 6] = [
                    md_json(&super::head(r.get("act"), 48)?),
                    line(if is_act(r) { "Act" } else { "instrument" }),
                    md_json(or(r.get("section"), &Json::str("-"))),
                    markdown_text(&line(r.kind)),
                    markdown_text(&listed),
                    markdown_text(&r.content[..r.content.len().min(150)]),
                ];
                let mut row = line("|");
                for cell in cells {
                    row.push(u32::from(' '));
                    row.extend(cell);
                    row.extend(line(" |"));
                }
                md.push(row);
            }
            if others.len() > 120 {
                md.push(Vec::new());
                md.push(line(&format!(
                    "_{} further provisions in rates.jsonl._",
                    others.len() - 120
                )));
            }
        }
    }
    let mut text = joined(&md.iter().map(Vec::as_slice).collect::<Vec<_>>(), '\n');
    text.push(0x0a);
    Ok(text)
}

fn execute(run: &mut Run) -> Result<(), Exc> {
    let directory = script_dir("rates.py")?;
    let root = corpus_root(&directory);
    let out = child(&root, &["rates"])?;
    require_builder_layout(&directory)?;
    // The title list finalize wrote, so a directory a rebuild dropped is not
    // indexed as a current rate.
    let sources_path = child(&root, &["sources.json"])?;
    let sources = crate::pyio::read_text(&sources_path)
        .map_err(Exc::from)
        .and_then(|text| pyjson::loads_standard(&text).map_err(json_error))?;
    let titles = iterate(getitem(&sources, "titles")?)?;
    let listed = titles
        .iter()
        .map(|t| register_id(getitem(t, "register_id")?))
        .collect::<Result<BTreeSet<String>, Exc>>()?;
    refuse_unlisted_titles(&root, &listed)?;
    crate::pyos::makedirs(&crate::pypath::PyPath::new(&out))?;

    let markdown_root = child(&root, &["markdown"])?;
    let mut records = Vec::new();
    for name in super::corpus_paths::sections_files(&markdown_root) {
        let directory_id = register_id_str(&name)?;
        read_title(&markdown_root, &directory_id, &mut records)?;
    }

    // Ordinals within one generated snapshot, not persistent identifiers.
    let mut records = sort_records(records)?;
    for (i, r) in records.iter_mut().enumerate() {
        r.fields
            .push(("rate_id", Json::str(&format!("R{:05}", i + 1))));
    }
    // The source writes hyphens as U+2011, so a plain search for "gross-up"
    // finds nothing; each row also carries an ASCII-folded copy.
    for r in &mut records {
        let content = pstr(fold(&r.content));
        let heading = match r.get("heading") {
            h if !h.truthy() => Json::Null,
            Json::Str(s) => pstr(fold(&s.code_points())),
            other => {
                return Err(Exc::Attribute(format!(
                    "'{}' object has no attribute 'replace'",
                    type_name(other)
                )));
            }
        };
        r.fields.push(("content_ascii", content));
        r.fields.push(("heading_ascii", heading));
    }

    let chunks: Vec<P> = records
        .iter()
        .map(|r| {
            let mut chunk = dumps_line(&r.json(), false);
            chunk.push(0x0a);
            chunk
        })
        .collect();
    write_text_atomic(&child(&out, &["rates.jsonl"])?, &chunks)?;

    let (mut by_topic, mut by_kind, mut by_coll) =
        (Counter::default(), Counter::default(), Counter::default());
    for r in &records {
        by_topic.add(&Json::str(r.topic))?;
    }
    for r in &records {
        by_kind.add(&Json::str(r.kind))?;
    }
    let unknown = Json::str("unknown");
    for r in &records {
        by_coll.add(or(r.get("collection"), &unknown))?;
    }
    let text = markdown(&records, &by_kind, &by_coll, &by_topic)?;
    write_text_atomic(&child(&out, &["RATES.md"])?, &[text])?;

    let distinct: BTreeSet<&str> = records
        .iter()
        .filter_map(|r| r.get("register_id").as_str())
        .collect();
    run.print(&format!(
        "entries: {}  titles: {}",
        records.len(),
        distinct.len()
    ));
    run.print(&format!(
        "by collection: {}",
        dict_repr(
            &by_coll
                .items
                .iter()
                .map(|(_, v, n)| (v, *n))
                .collect::<Vec<_>>()
        )
    ));
    run.print(&format!(
        "by kind: {}",
        dict_repr(
            &by_kind
                .items
                .iter()
                .map(|(_, v, n)| (v, *n))
                .collect::<Vec<_>>()
        )
    ));
    let top: Vec<(&Json, usize)> = by_topic.most_common().into_iter().take(8).collect();
    run.print(&format!("by topic: {}", dict_repr(&top)));
    Ok(())
}

/// `python -m fadden rates`: the stage takes no arguments and ignores any given.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    Ok(match execute(run) {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}
