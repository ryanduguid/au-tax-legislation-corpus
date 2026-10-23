//! `fadden/finalize.py`: the last corpus stage. It writes sources.json,
//! INDEX.md, LICENCE-NOTICE.md and README.md under the corpus root, copies
//! the support scripts there and into build/, and prints the totals.
use std::collections::{BTreeSet, HashMap};

use num_bigint::BigInt;

use super::corpus_paths::{
    child, corpus_root, markdown_text, module_dir, refuse_unlisted_titles, register_id,
    require_builder_layout, script_dir_in, write_text_atomic, write_text_atomic_then,
};
use super::pii_scan::{dict_key, for_each_row, load};
use super::rates::{py_lt, thousands};
use super::{ENCODE_DEPTH, Exc, Run, get, getitem, iterate, too_deep};
use crate::argparse::Exit;
use crate::py;
use crate::pyjson::{Json, PyStr};
use crate::pyos::{self, OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyvalue::{self, Key, str_points, type_name};

const KEYWORDS: [&str; 5] = [
    "Tax",
    "Excise",
    "Superannuation",
    "Customs Tariff",
    "Medicare Levy",
];
const LICENCE: &str = "CC BY 4.0";
const LICENCE_URL: &str = "https://creativecommons.org/licenses/by/4.0/";
const REGISTER: &str = "https://www.legislation.gov.au";

const ATTR_UNCHANGED: &str = r#"Sourced from the Federal Register of Legislation at %s. For the latest information on Australian Government law please go to https://www.legislation.gov.au"#;
const ATTR_CHANGED: &str = r#"Based on content from the Federal Register of Legislation at %s. For the latest information on Australian Government legislation please go to https://www.legislation.gov.au. Changes: converted from EPUB to markdown, contents pages removed, endnotes separated, images replaced by their descriptive alt text, compilation cover pages and running headers omitted."#;
const CORPUS: &str = r#"Commonwealth tax statutes and legislative instruments"#;
const LICENCE_NOTE: &str = r#"Excludes the Commonwealth Coat of Arms and any third-party material. The EPUBs under epub/ embed the Coat of Arms as an image; it is NOT licensed under CC BY and must not be reused. The markdown omits it. Authorised versions are PDF only, stamped under sections 15ZA and 15ZB of the Legislation Act 2003; the EPUB reading view used here is NOT the authorised text."#;
const SELECTION: &str = r#"In-force principal titles in the Act, LegislativeInstrument and NotifiableInstrument collections whose name contains Tax, Excise, Superannuation, Customs Tariff or Medicare Levy. isPrincipal could not be applied server-side (the API returns 400 for any filter containing it), so it was applied client-side. Selection is by title keyword, so a tax-relevant title without one of those words in its name is absent, and some collected titles (notably Commonwealth employee superannuation schemes) are not tax law. Check the collection field before relying on a provision: an Act and a regulation made under it are not interchangeable."#;
const LICENCE_NOTICE: &str = r#"# Licence notice

Commonwealth legislation from the Federal Register of Legislation, licensed
[Creative Commons Attribution 4.0 International](%s).

## Attribution for the raw EPUBs under `epub/`

> %s

## Attribution for the markdown, JSONL and index (changed content)

> %s

## Not covered by CC BY

The Commonwealth Coat of Arms and any third-party copyright material are
excluded from the licence. The EPUBs under `epub/` embed the Coat of Arms as an
image. Do not reuse it. The markdown omits all images and keeps only their
descriptive alt text.

## Not the authorised text

Authorised versions are PDF only, stamped under sections 15ZA and 15ZB of the
Legislation Act 2003. Everything here derives from the EPUB reading view.
"#;
const README: &str = r#"# Commonwealth tax legislation knowledge base

Built {retrieved} from the Federal Register of Legislation.

## What this is

{n} in-force principal titles covering tax, excise, superannuation, customs
tariff and Medicare levy: {n_act} Acts and {n_inst} legislative and notifiable
instruments. {rows} retrieval rows, {words} words.

Each title is stored as:

- `epub/<register_id>.epub`: the file exactly as the Register served it
- `markdown/<register_id>/<register_id>.md`: full text with YAML frontmatter
- `markdown/<register_id>/sections.jsonl`: one row per section, ready for RAG
- `markdown/<register_id>/endnotes.md`: amendment history, kept out of the sections

Plus a derived index:

- `rates/rates.jsonl`: the provisions its extraction rules select as carrying a
  rate, threshold, factor or indexation rule, bucketed by topic and cited back
  to its section. The rules are a set of numeric and phrase patterns over
  sentences and tables, so this is a finding aid rather than a complete list:
  read the title itself before concluding a provision carries no number. Each
  `rate_id` is an ordinal within one generated snapshot, not a persistent
  identifier, so cite the register id, section and snapshot alongside it.
- `rates/RATES.md`: the same, readable

`sources.json` records the register id, collection, compilation number and
compilation date for every title.

**Check the `collection` field before relying on a provision.** An Act and a
regulation made under it are not interchangeable, and an instrument can be
disallowed or sunset while its enabling Act stands.

## Checking staleness

    python check_current.py
    python check_current.py Act                    # one collection only

It asks the Register API for each title's current compilation and reports four
things separately: superseded compilations, titles whose in-force version has no
published compilation, titles no longer in force, and lookups that failed.
Read-only, about {mins} minutes for everything.

No run's counts are repeated here: it is a separate command, run after this
README is written, and its report goes to stdout. Its four buckets are not
interchangeable. The {notcurrent} titles listed in `sources.json` under
`titles_not_current_version` belong to the second bucket, not the first: their
in-force version has no published compilation, so there is no newer document to
re-download and the path built from that date answers 404.

## Licence and attribution

Licensed [{lic}]({lic_url}). Full statement in LICENCE-NOTICE.md. Every JSONL row
carries its own attribution, because rows travel independently of this file.

The markdown is CHANGED content (converted from EPUB, contents pages removed,
endnotes separated), so it uses the Register's "Based on content from" wording.
The EPUBs are unchanged and use "Sourced from".

The Commonwealth Coat of Arms is not covered by CC BY. It is embedded in the
EPUBs; the markdown omits all images and keeps only their alt text.

## Limits

**Not the authorised text.** Authorised versions are PDF only, stamped under
sections 15ZA and 15ZB of the Legislation Act 2003. Fine for research and
retrieval, not for citation in formal advice.

**Legislation only.** No ATO rulings, determinations or practice statements, no
case law, no explanatory memoranda. Figures the ATO computes rather than
Parliament enacts are therefore absent: the FBT gross-up factors, the Division
7A benchmark rate and the indexed superannuation caps among them. The Acts and
instruments give you the formula; the ATO publishes the result.

**Selection is by title keyword.** A tax-relevant title without Tax, Excise,
Superannuation, Customs Tariff or Medicare Levy in its name is absent: the
Customs Act 1901 and Charities Act 2013 among them.

**Some collected titles are not tax law.** Three ways they got in:

- The Commonwealth employee superannuation schemes match "Superannuation".
- The Register matches more than the current name. That is mostly a benefit:
  the Passenger Movement Charge Act 1978 came in on its former title, the
  Departure Tax Act 1978. `keywords_in_name` in `sources.json` is empty for
  these, so they can be reviewed rather than guessed at.
- A substring match pulled in roughly 35 documents that have nothing to do with
  tax: about 24 aviation airworthiness directives, because "Rotax" and "Britax"
  contain "tax", and around 10 social security instruments about taxi licence
  buybacks, because "taxi" starts with it. Every one is named `AD/...` or
  `Social Security (Exempt Lump Sum ...)`. Filter on `keywords_in_name` plus the
  name to drop them; nothing was deleted, because the same rule strict enough to
  catch Rotax also throws away the Departure Tax Act.

**Sunsetting.** Legislative instruments expire under Part 4 of the Legislation
Act 2003 unless remade. An instrument in force on the build date may have sunset
since. `check_current.py` reports these under "no longer in force": it reads the
Register's `status` before the compilation fields, so a repealed or ceased title
is not reported as a compilation still to be published.

**Tables** are converted to markdown tables. Cells spanning several columns are
padded so later cells keep their position, except where a table's spans vary row
to row, in which case the unpadded grid is kept because padding scatters those
values. Images inside a cell stay in that cell.

**Some instruments have no structure to find.** {whole} of them are stored as a
single `whole_act` row rather than split by section, because the document
genuinely has no sections and is short enough that one row is the right chunk.
No text is lost: retrieval returns the whole instrument.

**Table-shaped instruments are split on their tables.** {tabled} titles carry
`granularity: table_block`: no headings anywhere, just a run of tables. The Tax
Practitioners Board publishes its terminations this way, and Excise By-law No.
127 prescribes petroleum fields one table per basin. Each table becomes a row,
carrying the prose that preceded it: the sentence naming the enabling
provision, or the basin name. The largest of these was a single 14,928-word row
before the split.

**{pii_titles} titles name people.** The Tax Practitioners Board registers its
terminations and suspensions as notifiable instruments, so the corpus carries
about {pii_names} name mentions of disciplined agents with their registration
numbers and the provision they fell foul of. That text is public on the
Register, but splitting it into rows
makes it searchable by name in a way the source document is not. Exclude
`granularity: table_block` rows whose title starts with "Termination and
Suspension" or "TPB Termination" if that is not wanted.

**{notcurrent} titles are not the current text.** The Register can record that a
version commenced without publishing a compilation for it, and every document
path built from that date answers 404. Those titles carry the last compilation
that exists, marked `version_is_current: false` in the front matter and on every
JSONL row, with `superseded_from` giving the commencement the text does not
reflect. `sources.json` lists them under `titles_not_current_version`.

**Read `kind`, not `section`, to tell rows apart.** A row with a `section` id is
a `section`. A row with `section: null` is one of several kinds: `container` for
text sitting directly under a Chapter, Part, Division or Schedule heading,
`introductory` for the material before the first heading, `unnumbered` for a
headed passage with no section number, `whole_act` where a document has no
structure to split on, and `table_block` where it was split on its tables. Its
`heading` names the container or passage. Schedules in particular hold large
bodies of text that belong to no section number.

**Per-Act retrieval dates vary.** Each Act's `retrieved` field and attribution
carry the date that Act's EPUB was actually downloaded, not the build date.

## Parser notes

Section numbers render as `40 <U+2011> 1`: digits joined by a non-breaking hyphen
padded with non-breaking spaces. A regex expecting a plain adjacent hyphen
matches nothing.

Table cells wrap content in `<p>`, so cell text must accumulate in a buffer that
survives a nested paragraph.
"#;

type P = Vec<u32>;

fn points(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

/// `template % (values...)` for a template of bare `%s` fields.
fn percent_s(template: &str, values: &[&str]) -> String {
    let mut parts = template.split("%s");
    let mut out = parts.next().unwrap_or_default().to_string();
    for (part, value) in parts.zip(values) {
        out.push_str(value);
        out.push_str(part);
    }
    out
}

/// `fmt % value` for a template with one `%s`: `str(value)` in its place.
fn with_str(fmt: &str, value: &Json) -> Json {
    let (head, tail) = fmt.split_once("%s").unwrap_or((fmt, ""));
    let mut out = points(head);
    out.extend(str_points(value));
    out.extend(tail.chars().map(u32::from));
    Json::Str(PyStr::from_code_points(out))
}

/// `keywords_in_name(name)`: the keywords visible in the current name.
fn keywords_in_name(name: &Json) -> Result<Vec<&'static str>, Exc> {
    let Json::Str(name) = name else {
        return Err(Exc::Type(format!(
            "expected string or bytes-like object, got '{}'",
            type_name(name)
        )));
    };
    // A lone surrogate is no letter and no keyword character, as U+FFFD is not.
    let text = pyvalue::points_lossy(&name.code_points());
    Ok(KEYWORDS
        .into_iter()
        .filter(|k| {
            let pattern = format!("(?<![A-Za-z]){}", crate::pyre::escape(k));
            crate::pyre::re(&pattern, crate::pyre::I)
                .search(&text)
                .is_some()
        })
        .collect())
}

/// A Python `int` or `float`; `bool` counts as `int`.
#[derive(Clone, Debug)]
enum Num {
    Int(BigInt),
    Float(f64),
}

impl Num {
    fn of(value: &Json) -> Option<Num> {
        match value {
            Json::Bool(b) => Some(Num::Int(BigInt::from(u8::from(*b)))),
            Json::Int(i) => Some(Num::Int(i.clone())),
            Json::Float(f) => Some(Num::Float(*f)),
            _ => None,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Num::Int(_) => "int",
            Num::Float(_) => "float",
        }
    }

    fn json(&self) -> Json {
        match self {
            Num::Int(i) => Json::Int(i.clone()),
            Num::Float(f) => Json::Float(*f),
        }
    }
}

/// `float(i)`, correctly rounded (ties to even), or Python's OverflowError.
fn int_to_float(i: &BigInt) -> Result<f64, Exc> {
    let magnitude = i.magnitude();
    let bits = magnitude.bits();
    let value = if bits <= 53 {
        u64::try_from(magnitude).expect("53 bits") as f64
    } else {
        let shift = bits - 54;
        let top = u64::try_from(&(magnitude >> shift)).expect("54 bits");
        let sticky = magnitude.trailing_zeros().unwrap_or(0) < shift;
        let mut mantissa = top >> 1;
        if top & 1 == 1 && (sticky || mantissa & 1 == 1) {
            mantissa += 1;
        }
        // At most 2**53, exact; a power of two past 2**1023 is infinite.
        mantissa as f64 * 2f64.powi(i32::try_from(shift + 1).unwrap_or(i32::MAX))
    };
    if !value.is_finite() {
        return Err(Exc::Overflow("int too large to convert to float".into()));
    }
    Ok(if i.sign() == num_bigint::Sign::Minus {
        -value
    } else {
        value
    })
}

fn unsupported(op: &str, left: &str, right: &Json) -> Exc {
    Exc::Type(format!(
        "unsupported operand type(s) for {op}: '{left}' and '{}'",
        type_name(right)
    ))
}

/// `a + b` (`op` names the operator in the TypeError).
fn add(a: &Num, b: &Json, op: &str) -> Result<Num, Exc> {
    let Some(b) = Num::of(b) else {
        return Err(unsupported(op, a.type_name(), b));
    };
    Ok(match (a, b) {
        (Num::Int(x), Num::Int(y)) => Num::Int(x + y),
        (Num::Int(x), Num::Float(y)) => Num::Float(int_to_float(x)? + y),
        (Num::Float(x), Num::Int(y)) => Num::Float(x + int_to_float(&y)?),
        (Num::Float(x), Num::Float(y)) => Num::Float(x + y),
    })
}

/// `sum(items)` as CPython 3.15 adds: ints in a C `long` (32 bits on
/// Windows) until one does not fit; from the first float, floats and ints
/// with Neumaier compensation; after an int that left the `long`, plain `+`.
fn py_sum(items: impl IntoIterator<Item = Json>) -> Result<Num, Exc> {
    enum State {
        Long(i64),
        Float(f64, f64),
        Any(Num),
    }
    let range = if cfg!(windows) {
        i64::from(i32::MIN)..=i64::from(i32::MAX)
    } else {
        i64::MIN..=i64::MAX
    };
    let mut state = State::Long(0);
    for item in items {
        state = match state {
            State::Long(total) => {
                let small = match &item {
                    Json::Bool(b) => Some(i64::from(*b)),
                    Json::Int(i) => i64::try_from(i).ok(),
                    _ => None,
                };
                match small
                    .filter(|b| range.contains(b))
                    .and_then(|b| total.checked_add(b))
                    .filter(|t| range.contains(t))
                {
                    Some(total) => State::Long(total),
                    None => match add(&Num::Int(total.into()), &item, "+")? {
                        Num::Float(f) => State::Float(f, 0.0),
                        other => State::Any(other),
                    },
                }
            }
            State::Float(f, c) => {
                let x = match &item {
                    Json::Float(x) => *x,
                    Json::Bool(b) => f64::from(u8::from(*b)),
                    Json::Int(i) => int_to_float(i)?,
                    other => return Err(unsupported("+", "float", other)),
                };
                let t = f + x;
                let c = if f.abs() >= x.abs() {
                    c + ((f - t) + x)
                } else {
                    c + ((x - t) + f)
                };
                State::Float(t, c)
            }
            State::Any(total) => State::Any(add(&total, &item, "+")?),
        };
    }
    Ok(match state {
        State::Long(total) => Num::Int(total.into()),
        State::Float(f, c) if c != 0.0 && c.is_finite() => Num::Float(f + c),
        State::Float(f, _) => Num::Float(f),
        State::Any(total) => total,
    })
}

/// Commas between thousands in the leading run of digits.
fn grouped(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let digits = &rest[..end];
    let mut out = sign.to_string();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out.push_str(&rest[end..]);
    out
}

/// `f"{value:,}"`.
fn comma(value: &Json) -> Result<String, Exc> {
    Ok(match Num::of(value) {
        Some(Num::Int(i)) => grouped(&py::int_repr(&i).map_err(Exc::value)?),
        Some(Num::Float(f)) => grouped(&py::float_repr(f)),
        None if matches!(value, Json::Str(_)) => {
            return Err(Exc::value("Cannot specify ',' with 's'."));
        }
        None => {
            return Err(Exc::Type(format!(
                "unsupported format string passed to {}.__format__",
                type_name(value)
            )));
        }
    })
}

/// `"%d" % value`.
fn percent_d(value: &Num) -> Result<String, Exc> {
    let int = match value {
        Num::Int(i) => i.clone(),
        Num::Float(f) if f.is_nan() => {
            return Err(Exc::value("cannot convert float NaN to integer"));
        }
        Num::Float(f) if f.is_infinite() => {
            return Err(Exc::Overflow(
                "cannot convert float infinity to integer".into(),
            ));
        }
        Num::Float(f) => pyvalue::float_to_int(f.trunc()),
    };
    py::int_repr(&int).map_err(Exc::value)
}

/// `"%.1f" % value`.
fn percent_1f(value: f64) -> String {
    if value.is_nan() {
        "nan".into()
    } else {
        format!("{value:.1}")
    }
}

/// `int(value)` for a truthy decoded JSON value.
fn py_int(value: &Json) -> Result<BigInt, Exc> {
    match value {
        Json::Float(f) if f.is_nan() => Err(Exc::value("cannot convert float NaN to integer")),
        Json::Float(f) if f.is_infinite() => Err(Exc::Overflow(
            "cannot convert float infinity to integer".into(),
        )),
        Json::Float(f) => Ok(pyvalue::float_to_int(f.trunc())),
        Json::Str(PyStr::Text(s)) => py::parse_int(s, 10).map_err(Exc::value),
        Json::Str(PyStr::Lone(cps)) => Err(Exc::value(format!(
            "invalid literal for int() with base 10: {}",
            pyvalue::repr_points(&cps[..cps.len().min(200)])
        ))),
        Json::Array(_) | Json::Object(_) | Json::Null => Err(Exc::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(value)
        ))),
        other => Ok(match Num::of(other) {
            Some(Num::Int(i)) => i,
            _ => BigInt::from(0),
        }),
    }
}

/// `len(value)`.
fn py_len(value: &Json) -> Result<usize, Exc> {
    match value {
        Json::Array(items) => Ok(items.len()),
        Json::Object(pairs) => Ok(pairs.len()),
        Json::Str(s) => Ok(s.code_points().len()),
        other => Err(Exc::Type(format!(
            "object of type '{}' has no len()",
            type_name(other)
        ))),
    }
}

/// `pii_summary()`: the titles pii_flagged.json lists and its name
/// mentions, rounded to the nearest 50.
fn pii_summary(scratch: &str) -> Result<(usize, BigInt), Exc> {
    let path = pypath::join(scratch, "pii_flagged.json");
    let counted = (|| -> Result<(usize, BigInt), Exc> {
        let flagged = load(&path)?;
        let titles = py_len(&flagged)?;
        let mut names = BigInt::from(0);
        for t in iterate(&flagged)? {
            match get(&t, "names_est")? {
                Some(value) if value.truthy() => names += py_int(value)?,
                _ => {}
            }
        }
        Ok((titles, names))
    })();
    let (titles, names) = match counted {
        Err(error @ (Exc::Os(_) | Exc::Value { .. } | Exc::Type(_))) => {
            return Err(Exc::Runtime(format!(
                "pii_flagged.json is missing or unreadable ({error}); run pii_scan.py and \
                 pii_scan2.py before finalize.py so the README's PII counts come from a real scan"
            )));
        }
        other => other?,
    };
    let rounded = (int_to_float(&names)? / 50.0).round_ties_even();
    Ok((titles, pyvalue::float_to_int(rounded) * 50))
}

/// A title the stage publishes, with the rows its sections.jsonl holds.
struct Title {
    entry: Json,
    rows: usize,
    section_rows: usize,
}

impl Title {
    fn get(&self, field: &str) -> Option<&Json> {
        self.entry.get(field)
    }

    fn truthy(&self, field: &str) -> bool {
        self.get(field).is_some_and(Json::truthy)
    }

    fn or_null(&self, field: &str) -> Json {
        self.get(field).cloned().unwrap_or(Json::Null)
    }

    /// `a.get(field, 0)`.
    fn or_zero(&self, field: &str) -> Json {
        self.get(field).cloned().unwrap_or(Json::Int(0.into()))
    }

    /// `a.get("collection") or "unknown"`.
    fn collection(&self) -> Json {
        match self.get("collection") {
            Some(c) if c.truthy() => c.clone(),
            _ => Json::str("unknown"),
        }
    }

    fn granularity_is(&self, kind: &str) -> bool {
        self.get("granularity").and_then(Json::as_str) == Some(kind)
    }

    fn not_current(&self) -> bool {
        matches!(self.get("version_is_current"), Some(Json::Bool(false)))
    }
}

struct Collection {
    key: Json,
    titles: usize,
    rows: usize,
    words: Num,
}

/// `CorpusInventory`.
struct Inventory {
    missing: Vec<Json>,
    titles: Vec<Title>,
    kinds: HashMap<Key, usize>,
    by_collection: Vec<(Key, Collection)>,
    total_rows: usize,
    section_rows: usize,
    total_words: Num,
    epub_bytes: Num,
    acts: usize,
    instruments: usize,
}

fn str_key(text: &str) -> Key {
    Key::Str(points(text))
}

impl Inventory {
    fn collection(&self, name: &str) -> Option<&Collection> {
        let key = str_key(name);
        self.by_collection
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, c)| c)
    }

    fn kind(&self, name: &str) -> usize {
        self.kinds.get(&str_key(name)).copied().unwrap_or(0)
    }

    fn count(&self, test: impl Fn(&Title) -> bool) -> usize {
        self.titles.iter().filter(|t| test(t)).count()
    }
}

/// `assemble_corpus_inventory(raw, markdown, root)`.
fn assemble(raw: &Json, markdown: &Json, root: &str) -> Result<Inventory, Exc> {
    let mut missing = Vec::new();
    for a in iterate(raw)? {
        if !get(&a, "epub")?.is_some_and(Json::truthy) {
            missing.push(a);
        }
    }
    let mut listed = Vec::new();
    for a in iterate(markdown)? {
        if get(&a, "markdown")?.is_some_and(Json::truthy) {
            listed.push(a);
        }
    }
    let names = listed
        .iter()
        .map(|a| getitem(a, "name").cloned())
        .collect::<Result<Vec<_>, _>>()?;
    let mut order: Vec<usize> = (0..listed.len()).collect();
    crate::pysort::sort(&mut order, |&i, &j| py_lt(&names[i], &names[j]))?;

    let mut kinds: HashMap<Key, usize> = HashMap::new();
    let mut titles = Vec::with_capacity(order.len());
    for i in order {
        let entry = listed[i].clone();
        let rid = register_id(getitem(&entry, "id")?)?;
        let path = child(root, &["markdown", &rid, "sections.jsonl"])?;
        let (mut rows, mut section_rows) = (0, 0);
        for_each_row(&path, |row| {
            rows += 1;
            let section = get(&row, "section")?.is_some_and(Json::truthy);
            if section {
                section_rows += 1;
            }
            let kind = match get(&row, "kind")? {
                Some(kind) if kind.truthy() => kind.clone(),
                _ if get(&row, "section")?.is_some_and(Json::truthy) => Json::str("section"),
                _ => Json::str("unnumbered"),
            };
            *kinds.entry(dict_key(&kind, "dict key")?).or_insert(0) += 1;
            Ok(())
        })?;
        titles.push(Title {
            entry,
            rows,
            section_rows,
        });
    }

    let total_rows = titles.iter().map(|t| t.rows).sum();
    let section_rows = titles.iter().map(|t| t.section_rows).sum();
    let total_words = py_sum(titles.iter().map(|t| t.or_zero("words")))?;
    let epub_bytes = py_sum(titles.iter().map(|t| t.or_zero("bytes")))?;

    let mut by_collection: Vec<(Key, Collection)> = Vec::new();
    for t in &titles {
        let c = t.collection();
        let key = dict_key(&c, "dict key")?;
        let at = match by_collection.iter().position(|(k, _)| *k == key) {
            Some(at) => at,
            None => {
                by_collection.push((
                    key,
                    Collection {
                        key: c,
                        titles: 0,
                        rows: 0,
                        words: Num::Int(0.into()),
                    },
                ));
                by_collection.len() - 1
            }
        };
        let d = &mut by_collection[at].1;
        d.titles += 1;
        d.rows += t.rows;
        d.words = add(&d.words, &t.or_zero("words"), "+=")?;
    }
    let mut inventory = Inventory {
        missing,
        titles,
        kinds,
        by_collection,
        total_rows,
        section_rows,
        total_words,
        epub_bytes,
        acts: 0,
        instruments: 0,
    };
    inventory.acts = inventory.collection("Act").map_or(0, |c| c.titles);
    inventory.instruments = inventory.titles.len() - inventory.acts;
    Ok(inventory)
}

/// A dict key as `json.dump` writes it: a float as `NaN`, `Infinity` or its
/// repr, a bool as `true` or `false`, an int (at most 4,300 digits once
/// decoded) in decimal. Unhashable values never become keys.
fn json_key(key: &Json) -> PyStr {
    match key {
        Json::Str(s) => s.clone(),
        other => PyStr::from_code_points(pyvalue::dumps_line(other, false)),
    }
}

fn obj(pairs: Vec<(&str, Json)>) -> Json {
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

/// `build_sources_document(inventory, retrieved)`.
fn sources_document(inv: &Inventory, retrieved: &str) -> Result<Json, Exc> {
    let ok = &inv.titles;
    let by_collection = Json::Object(
        inv.by_collection
            .iter()
            .map(|(_, c)| {
                let counts = obj(vec![
                    ("titles", int(c.titles)),
                    ("rows", int(c.rows)),
                    ("words", c.words.json()),
                ]);
                (json_key(&c.key), counts)
            })
            .collect(),
    );
    let mut no_keyword = 0;
    for a in ok {
        if keywords_in_name(getitem(&a.entry, "name")?)?.is_empty() {
            no_keyword += 1;
        }
    }
    let counts = obj(vec![
        ("titles", int(ok.len())),
        ("acts", int(inv.acts)),
        ("instruments", int(inv.instruments)),
        ("by_collection", by_collection),
        ("titles_without_epub", int(inv.missing.len())),
        ("jsonl_rows", int(inv.total_rows)),
        ("words_body_only", inv.total_words.json()),
        ("epub_bytes", inv.epub_bytes.json()),
        ("rows_with_section_id", int(inv.section_rows)),
        ("rows_container", int(inv.kind("container"))),
        ("rows_unnumbered", int(inv.kind("unnumbered"))),
        ("rows_introductory", int(inv.kind("introductory"))),
        ("titles_no_keyword_in_current_name", int(no_keyword)),
        (
            "titles_whole_act_chunk",
            int(inv.count(|t| t.granularity_is("whole_act"))),
        ),
        (
            "titles_table_block_chunk",
            int(inv.count(|t| t.granularity_is("table_block"))),
        ),
        (
            "titles_with_endnotes",
            int(inv.count(|t| t.truthy("endnotes"))),
        ),
        (
            "titles_not_current_version",
            int(inv.count(Title::not_current)),
        ),
    ]);
    let mut not_current = Vec::new();
    for a in ok.iter().filter(|t| t.not_current()) {
        not_current.push(obj(vec![
            ("register_id", getitem(&a.entry, "id")?.clone()),
            ("name", getitem(&a.entry, "name")?.clone()),
            ("collection", a.or_null("collection")),
            ("text_is_version_from", a.or_null("versionStart")),
            ("compilation_number", a.or_null("compilationNumber")),
            ("superseded_from", a.or_null("current_version_start")),
            (
                "reason",
                Json::str("the in-force version has no published compilation on the Register"),
            ),
        ]));
    }
    let mut titles = Vec::new();
    for a in ok {
        let id = getitem(&a.entry, "id")?;
        let name = getitem(&a.entry, "name")?;
        let keywords = keywords_in_name(name)?;
        titles.push(obj(vec![
            ("register_id", id.clone()),
            ("name", name.clone()),
            ("collection", a.or_null("collection")),
            (
                "keywords_in_name",
                Json::Array(keywords.into_iter().map(Json::str).collect()),
            ),
            ("long_title", a.or_null("long_title")),
            ("compilation_number", a.or_null("compilationNumber")),
            ("compilation_date", a.or_null("versionStart")),
            (
                "version_is_current",
                a.get("version_is_current")
                    .cloned()
                    .unwrap_or(Json::Bool(true)),
            ),
            ("current_version_start", a.or_null("current_version_start")),
            ("retrieved", a.or_null("retrieved")),
            ("jsonl_rows", int(a.rows)),
            ("granularity", a.or_null("granularity")),
            ("words", a.or_null("words")),
            ("epub_bytes", a.or_null("bytes")),
            ("epub", with_str("epub/%s", getitem(&a.entry, "epub")?)),
            (
                "markdown",
                with_str("markdown/%s", getitem(&a.entry, "markdown")?),
            ),
            ("sections_jsonl", with_str("markdown/%s/sections.jsonl", id)),
            (
                "endnotes",
                if a.truthy("endnotes") {
                    with_str("markdown/%s/endnotes.md", id)
                } else {
                    Json::Null
                },
            ),
            ("source_url", a.or_null("sourceUrl")),
            (
                "register_page",
                with_str(&format!("{REGISTER}/%s/latest/text"), id),
            ),
        ]));
    }
    let mut without_epub = Vec::new();
    for a in &inv.missing {
        without_epub.push(obj(vec![
            ("register_id", getitem(a, "id")?.clone()),
            ("name", getitem(a, "name")?.clone()),
            (
                "collection",
                a.get("collection").cloned().unwrap_or(Json::Null),
            ),
            ("reason", a.get("reason").cloned().unwrap_or(Json::Null)),
        ]));
    }
    Ok(obj(vec![
        ("corpus", Json::str(CORPUS)),
        ("retrieved", Json::str(retrieved)),
        ("source", Json::str("Federal Register of Legislation")),
        (
            "source_api",
            Json::str("https://api.prod.legislation.gov.au/v1/"),
        ),
        ("licence", Json::str(LICENCE)),
        ("licence_url", Json::str(LICENCE_URL)),
        (
            "attribution_epub",
            Json::str(&percent_s(ATTR_UNCHANGED, &[retrieved])),
        ),
        (
            "attribution_markdown",
            Json::str(&percent_s(ATTR_CHANGED, &[retrieved])),
        ),
        ("licence_note", Json::str(LICENCE_NOTE)),
        ("selection", Json::str(SELECTION)),
        ("counts", counts),
        ("titles_not_current_version", Json::Array(not_current)),
        ("titles", Json::Array(titles)),
        ("titles_without_epub", Json::Array(without_epub)),
    ]))
}

fn newline(level: usize) -> P {
    let mut out = vec![u32::from('\n')];
    out.extend(std::iter::repeat_n(u32::from(' '), level));
    out
}

/// A value `json.dump` writes in one piece, or None for a list or dict.
fn scalar(value: &Json) -> Result<Option<P>, Exc> {
    Ok(match value {
        Json::Array(_) | Json::Object(_) => None,
        Json::Int(i) => Some(points(&py::int_repr(i).map_err(Exc::value)?)),
        other => Some(pyvalue::dumps_line(other, false)),
    })
}

/// The pieces `json.dump(value, f, indent=1, ensure_ascii=False)` hands to
/// `f.write`, in order, until the exception that stops it, if any. As in
/// CPython 3.15, an exception raised encoding an item carries a note per
/// enclosing list or dict naming the item.
fn encode(value: &Json, level: usize, chunks: &mut Vec<P>) -> Result<(), Exc> {
    if matches!(value, Json::Array(_) | Json::Object(_)) && level >= ENCODE_DEPTH {
        return Err(too_deep());
    }
    match value {
        Json::Array(items) if items.is_empty() => chunks.push(points("[]")),
        Json::Object(pairs) if pairs.is_empty() => chunks.push(points("{}")),
        Json::Array(items) => {
            let mut buf = points("[");
            buf.extend(newline(level + 1));
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf = points(",");
                    buf.extend(newline(level + 1));
                }
                let note = |e: Exc| e.noted(format!("when serializing list item {i}"));
                match scalar(item).map_err(note)? {
                    Some(text) => {
                        buf.extend(text);
                        chunks.push(std::mem::take(&mut buf));
                    }
                    None => {
                        chunks.push(std::mem::take(&mut buf));
                        encode(item, level + 1, chunks).map_err(note)?;
                    }
                }
            }
            chunks.push(newline(level));
            chunks.push(points("]"));
        }
        Json::Object(pairs) => {
            chunks.push(points("{"));
            chunks.push(newline(level + 1));
            for (i, (key, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    let mut separator = points(",");
                    separator.extend(newline(level + 1));
                    chunks.push(separator);
                }
                let mut text = Vec::new();
                pyvalue::dump_str(&key.code_points(), false, &mut text);
                chunks.push(text);
                chunks.push(points(": "));
                let note = |e: Exc| {
                    let key = pyvalue::repr_points(&key.code_points());
                    e.noted(format!("when serializing dict item {key}"))
                };
                match scalar(item).map_err(note)? {
                    Some(text) => chunks.push(text),
                    None => encode(item, level + 1, chunks).map_err(note)?,
                }
            }
            chunks.push(newline(level));
            chunks.push(points("}"));
        }
        other => chunks.extend(scalar(other)?),
    }
    Ok(())
}

/// `markdown_text(str(value))`.
fn md(value: &Json) -> P {
    markdown_text(&str_points(value))
}

/// `markdown_text(a.get(field) or "-")`.
fn md_or_dash(value: Option<&Json>) -> P {
    match value {
        Some(v) if v.truthy() => md(v),
        _ => md(&Json::str("-")),
    }
}

/// `"|".join`-style table row: `| a | b |`.
fn row(cells: Vec<P>) -> P {
    let mut out = points("|");
    for cell in cells {
        out.extend(points(" "));
        out.extend(cell);
        out.extend(points(" |"));
    }
    out
}

/// `[%s](markdown/%s)` for a title link.
fn link(a: &Json) -> Result<P, Exc> {
    let mut out = points("[");
    out.extend(md(getitem(a, "name")?));
    out.extend(points("](markdown/"));
    out.extend(str_points(getitem(a, "markdown")?));
    out.extend(points(")"));
    Ok(out)
}

const COLLECTIONS: [(&str, &str); 3] = [
    ("Act", "Acts"),
    ("LegislativeInstrument", "Legislative instruments"),
    ("NotifiableInstrument", "Notifiable instruments"),
];

/// `build_index_document(inventory, retrieved)`.
fn index_document(inv: &Inventory, retrieved: &str) -> Result<P, Exc> {
    let ok = &inv.titles;
    let words = comma(&inv.total_words.json())?;
    let mut idx: Vec<P> = [
        "# Commonwealth tax legislation: index".to_string(),
        String::new(),
        format!("Retrieved {retrieved} from the Federal Register of Legislation."),
        format!("Licence [{LICENCE}]({LICENCE_URL}). Attribution and limits: see README.md."),
        String::new(),
        format!(
            "{} titles ({} Acts, {} instruments), {} retrieval rows, {words} words.",
            ok.len(),
            inv.acts,
            inv.instruments,
            thousands(inv.total_rows)
        ),
        String::new(),
        "| Collection | Titles | Rows | Words |".into(),
        "|---|---|---|---|".into(),
    ]
    .iter()
    .map(|line| points(line))
    .collect();
    for (key, label) in COLLECTIONS {
        if let Some(d) = inv.collection(key) {
            idx.push(row(vec![
                points(label),
                points(&thousands(d.titles)),
                points(&thousands(d.rows)),
                points(&comma(&d.words.json())?),
            ]));
        }
    }
    let in_collection = |t: &Title, key: &str| t.collection().as_str() == Some(key);
    for (key, label) in COLLECTIONS {
        let group: Vec<&Title> = ok.iter().filter(|t| in_collection(t, key)).collect();
        if group.is_empty() {
            continue;
        }
        for line in [
            String::new(),
            format!("## {label} ({})", group.len()),
            String::new(),
            "| Title | Register ID | Comp. | Comp. date | Rows | Words |".into(),
            "|---|---|---|---|---|---|".into(),
        ] {
            idx.push(points(&line));
        }
        for a in group {
            idx.push(row(vec![
                link(&a.entry)?,
                md(getitem(&a.entry, "id")?),
                md_or_dash(a.get("compilationNumber")),
                md_or_dash(a.get("versionStart")),
                points(&thousands(a.rows)),
                points(&comma(&a.or_zero("words"))?),
            ]));
        }
    }
    let leftover: Vec<&Title> = ok
        .iter()
        .filter(|t| !COLLECTIONS.iter().any(|(key, _)| in_collection(t, key)))
        .collect();
    if !leftover.is_empty() {
        for line in [
            String::new(),
            format!("## Other collections ({})", leftover.len()),
            String::new(),
            "| Title | Collection | Register ID | Rows |".into(),
            "|---|---|---|---|".into(),
        ] {
            idx.push(points(&line));
        }
        for a in leftover {
            idx.push(row(vec![
                link(&a.entry)?,
                md_or_dash(a.get("collection")),
                md(getitem(&a.entry, "id")?),
                points(&thousands(a.rows)),
            ]));
        }
    }
    if !inv.missing.is_empty() {
        for line in [
            "",
            "## Titles with no EPUB available",
            "",
            "| Title | Collection | Register ID | Reason |",
            "|---|---|---|---|",
        ] {
            idx.push(points(line));
        }
        for a in &inv.missing {
            idx.push(row(vec![
                md(getitem(a, "name")?),
                md_or_dash(a.get("collection")),
                md(getitem(a, "id")?),
                md_or_dash(a.get("reason")),
            ]));
        }
    }
    let mut out = idx.join(&u32::from('\n'));
    out.push(u32::from('\n'));
    Ok(out)
}

/// `build_readme_document(inventory, retrieved, pii_titles, pii_names)`.
fn readme_document(inv: &Inventory, retrieved: &str, pii: &(usize, BigInt)) -> Result<P, Exc> {
    let ok = &inv.titles;
    let mins = ((ok.len() as f64 * 1.5 / 60.0).round_ties_even() as u64).max(1);
    let fields = [
        ("{retrieved}", retrieved.to_string()),
        ("{n}", ok.len().to_string()),
        ("{n_act}", inv.acts.to_string()),
        ("{n_inst}", inv.instruments.to_string()),
        ("{rows}", thousands(inv.total_rows)),
        ("{words}", comma(&inv.total_words.json())?),
        ("{lic}", LICENCE.into()),
        ("{lic_url}", LICENCE_URL.into()),
        ("{mins}", mins.to_string()),
        ("{pii_titles}", pii.0.to_string()),
        ("{pii_names}", comma(&Json::Int(pii.1.clone()))?),
        (
            "{whole}",
            inv.count(|t| t.granularity_is("whole_act")).to_string(),
        ),
        (
            "{tabled}",
            inv.count(|t| t.granularity_is("table_block")).to_string(),
        ),
        ("{notcurrent}", inv.count(Title::not_current).to_string()),
    ];
    let mut text = README.to_string();
    for (field, value) in fields {
        text = text.replace(field, &value);
    }
    Ok(points(&text))
}

/// `with atomic_text_writer(path) as f: f.write(text)`, where building the
/// text can fail after the staging file is open.
fn write_document(path: &str, text: Result<P, Exc>) -> Result<(), Exc> {
    match text {
        Ok(text) => write_text_atomic(path, &[text]),
        Err(error) => write_text_atomic_then(path, &[], Some(error)),
    }
}

/// `shutil.copy(src, dst)`: the contents, then the permission bits.
fn copy(src: &str, dst: &str) -> Result<(), Exc> {
    let dst = if pyos::isdir(dst) {
        pypath::join(dst, &pypath::split(src).1)
    } else {
        dst.to_string()
    };
    super::dist::copyfile(src, &dst, &py::repr_str(src))?;
    let meta = std::fs::metadata(src).map_err(|e| OsError::new(&e, Some(src), Style::Native))?;
    #[cfg(unix)]
    let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
    // Only the owner-write bit reaches Windows, as the read-only attribute.
    #[cfg(windows)]
    let mode = if meta.permissions().readonly() {
        0o444
    } else {
        0o666
    };
    pyos::chmod(&dst, mode).map_err(|e| OsError::new(&e, Some(&dst), Style::Native))?;
    Ok(())
}

/// `copy_if_different(src, dst)`: a re-run from build/ would copy a file onto itself.
fn copy_if_different(src: &str, dst: &str) -> Result<(), Exc> {
    if pypath::abspath(src) != pypath::abspath(dst) {
        copy(src, dst)?;
    }
    Ok(())
}

/// `finish_corpus_publication(inventory, retrieved, pii_counts)`.
fn finish(
    inv: &Inventory,
    root: &str,
    scratch: &str,
    retrieved: &str,
    pii: &(usize, BigInt),
    run: &mut Run,
) -> Result<(), Exc> {
    let path = child(root, &["README.md"])?;
    write_document(&path, readme_document(inv, retrieved, pii))?;

    let build = child(root, &["build"])?;
    pyos::makedirs(&PyPath::new(&build))?;
    for name in ["check_current.py", "corpus_paths.py", "http_fetch.py"] {
        copy_if_different(&pypath::join(scratch, name), &child(root, &[name])?)?;
    }
    for name in [
        "discover.py",
        "versions.py",
        "download.py",
        "extract.py",
        "finalize.py",
        "check_current.py",
        "corpus_paths.py",
        "http_fetch.py",
    ] {
        copy_if_different(&pypath::join(scratch, name), &child(&build, &[name])?)?;
    }

    let epub_mb = match &inv.epub_bytes {
        Num::Int(i) => int_to_float(i)?,
        Num::Float(f) => *f,
    } / 1e6;
    let words = percent_d(&inv.total_words)?;
    run.print(&format!(
        "titles={} (acts={} instruments={}) rows={} words={words} epub={} MB missing={} whole_act={}",
        inv.titles.len(),
        inv.acts,
        inv.instruments,
        inv.total_rows,
        percent_1f(epub_mb),
        inv.missing.len(),
        inv.count(|t| t.granularity_is("whole_act")),
    ));
    let by = &inv.by_collection;
    let mut order: Vec<usize> = (0..by.len()).collect();
    crate::pysort::sort(&mut order, |&i, &j| py_lt(&by[i].1.key, &by[j].1.key))?;
    let body: Vec<String> = order
        .into_iter()
        .map(|i| format!("{}: {}", pyvalue::repr(&by[i].1.key), by[i].1.titles))
        .collect();
    run.print(&format!("by collection: {{{}}}", body.join(", ")));
    Ok(())
}

/// `main(retrieved)` with the module in `scratch`.
pub fn run(scratch: &str, retrieved: &str, run: &mut Run) -> Result<(), Exc> {
    let directory = script_dir_in(scratch, "finalize.py")?;
    let root = corpus_root(&directory);
    require_builder_layout(&directory)?;
    let markdown = load(&pypath::join(scratch, "manifest_md.json"))?;
    let raw = load(&pypath::join(scratch, "manifest_raw.json"))?;
    let mut listed = BTreeSet::new();
    for a in iterate(&markdown)? {
        if get(&a, "markdown")?.is_some_and(Json::truthy) {
            listed.insert(register_id(getitem(&a, "id")?)?);
        }
    }
    refuse_unlisted_titles(&root, &listed)?;
    let pii = pii_summary(scratch)?;
    let inv = assemble(&raw, &markdown, &root)?;

    let sources = sources_document(&inv, retrieved)?;
    let path = child(&root, &["sources.json"])?;
    let mut chunks = Vec::new();
    let failure = encode(&sources, 0, &mut chunks).err();
    write_text_atomic_then(&path, &chunks, failure)?;

    let path = child(&root, &["INDEX.md"])?;
    write_document(&path, index_document(&inv, retrieved))?;
    let path = child(&root, &["LICENCE-NOTICE.md"])?;
    let notice = LICENCE_NOTICE
        .replacen("%s", LICENCE_URL, 1)
        .replacen("%s", &percent_s(ATTR_UNCHANGED, &[retrieved]), 1)
        .replacen("%s", &percent_s(ATTR_CHANGED, &[retrieved]), 1);
    write_document(&path, Ok(points(&notice)))?;

    finish(&inv, &root, scratch, retrieved, &pii, run)
}

/// `datetime.date.today().isoformat()`.
fn today() -> Result<String, Exc> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    Ok(super::extract::local_date(now)?.isoformat())
}

/// `python -m fadden finalize`: the dispatcher passes today's date and no arguments.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let outcome = today().and_then(|day| self::run(&module_dir(), &day, run));
    Ok(match outcome {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int_json(text: &str) -> Json {
        Json::Int(text.parse().unwrap())
    }

    #[test]
    fn ints_convert_to_the_nearest_float() {
        let two53 = BigInt::from(1u64 << 53);
        assert_eq!(int_to_float(&(&two53 + 1)).unwrap(), 9007199254740992.0);
        assert_eq!(int_to_float(&(&two53 + 3)).unwrap(), 9007199254740996.0);
        assert_eq!(
            int_to_float(&-(&two53 + BigInt::from(3))).unwrap(),
            -9007199254740996.0
        );
        let big = BigInt::from(1u8) << 1024u32;
        assert!(int_to_float(&big).is_err());
        let largest = (BigInt::from(1u8) << 1024u32) - (BigInt::from(1u8) << 970u32);
        assert!(int_to_float(&largest).is_err());
        let below = &largest - 1;
        assert_eq!(int_to_float(&below).unwrap(), f64::MAX);
    }

    #[test]
    fn sum_compensates_floats_as_cpython_does() {
        let items = [0.1, 0.2, 0.3].map(Json::Float);
        assert!(matches!(py_sum(items).unwrap(), Num::Float(f) if f == 0.6));
        let mixed = vec![
            Json::Int(1.into()),
            Json::Float(1e100),
            Json::Int(1.into()),
            Json::Float(-1e100),
        ];
        assert!(matches!(py_sum(mixed).unwrap(), Num::Float(f) if f == 1.0));
        let error = py_sum(vec![Json::Float(0.5), Json::str("x")]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported operand type(s) for +: 'float' and 'str'"
        );
        let huge = vec![int_json(&"9".repeat(400)), Json::Float(1.0)];
        assert_eq!(
            py_sum(huge).unwrap_err().to_string(),
            "int too large to convert to float"
        );
    }

    #[test]
    fn grouping_matches_format_spec() {
        assert_eq!(comma(&Json::Float(1e16)).unwrap(), "1e+16");
        assert_eq!(comma(&Json::Float(-1234567.25)).unwrap(), "-1,234,567.25");
        assert_eq!(comma(&Json::Bool(true)).unwrap(), "1");
        assert_eq!(comma(&int_json("-1234")).unwrap(), "-1,234");
        assert_eq!(comma(&Json::Float(f64::NAN)).unwrap(), "nan");
        assert_eq!(percent_1f(0.25), "0.2");
        assert_eq!(percent_d(&Num::Float(-2.7)).unwrap(), "-2");
    }

    #[test]
    fn keywords_respect_the_letter_boundary() {
        assert_eq!(
            keywords_in_name(&Json::str("Income TAX and Medicare levy")).unwrap(),
            ["Tax", "Medicare Levy"]
        );
        assert!(
            keywords_in_name(&Json::str("Rotax engines"))
                .unwrap()
                .is_empty()
        );
        assert!(keywords_in_name(&Json::Int(1.into())).is_err());
    }

    #[test]
    fn dump_pieces_follow_the_python_encoder() {
        let value = obj(vec![
            (
                "a",
                Json::Array(vec![Json::Int(1.into()), Json::Array(vec![])]),
            ),
            ("b", Json::Object(vec![])),
        ]);
        let mut chunks = Vec::new();
        encode(&value, 0, &mut chunks).unwrap();
        let text: Vec<String> = chunks.iter().map(|c| pyvalue::points_lossy(c)).collect();
        assert_eq!(
            text,
            [
                "{", "\n ", "\"a\"", ": ", "[\n  1", ",\n  ", "[]", "\n ", "]", ",\n ", "\"b\"",
                ": ", "{}", "\n", "}"
            ]
        );
    }
}
