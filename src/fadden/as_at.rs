//! `fadden/as_at.py`: classify the build's titles against a date, and check a
//! quotation against a row.
//!
//! A compilation date is the start date of that compiled text version, not
//! necessarily the commencement or application date of every provision it
//! contains. For each title this says whether the compilation the build
//! captured corresponds to the date by the build's own metadata: `captured`,
//! `before_compilation`, `superseded` or `after_retrieval`. `captured` is not a
//! finding that the text was the law on the date.
use unicode_normalization::UnicodeNormalization;

use super::{Exc, Run, json_error};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::py;
use crate::pyjson::{self, Json};
use crate::pypath;
use crate::pyvalue::{self, dumps_line, repr, type_name};

pub const CAPTURED: &str = "captured";
pub const BEFORE_COMPILATION: &str = "before_compilation";
pub const SUPERSEDED: &str = "superseded";
pub const AFTER_RETRIEVAL: &str = "after_retrieval";
const STATUSES: [&str; 4] = [CAPTURED, BEFORE_COMPILATION, SUPERSEDED, AFTER_RETRIEVAL];

fn action(status: &str) -> Option<&'static str> {
    match status {
        BEFORE_COMPILATION => Some("read the Register's earlier compilations"),
        SUPERSEDED => Some("a later version applies; read the Register"),
        AFTER_RETRIEVAL => Some("run check_current and read the Register"),
        _ => None,
    }
}

/// `date.fromisoformat(value).isoformat() == value`: a canonical `YYYY-MM-DD`.
pub fn is_date(value: &str) -> bool {
    py::Date::fromisoformat(value).is_some_and(|d| d.isoformat() == value)
}

/// `title.get(field)` on a dict, or the AttributeError `.get` raises otherwise.
fn get<'a>(title: &'a Json, field: &str) -> Result<Option<&'a Json>, Exc> {
    match title {
        Json::Object(_) => Ok(title.get(field)),
        other => Err(Exc::Attribute(format!(
            "'{}' object has no attribute 'get'",
            type_name(other)
        ))),
    }
}

/// `title.get('id', '?')` as an f-string shows it.
fn id(title: &Json) -> String {
    title
        .get("id")
        .map_or_else(|| "?".into(), pyvalue::str_for_stderr)
}

fn date<'a>(title: &'a Json, field: &str) -> Result<&'a str, Exc> {
    let value = get(title, field)?;
    match value.and_then(Json::as_str) {
        Some(text) if is_date(text) => Ok(text),
        _ => Err(Exc::value(format!(
            "{}: {field} is not a YYYY-MM-DD date: {}",
            id(title),
            value.map_or_else(|| "None".into(), repr)
        ))),
    }
}

/// A title's (compilation start, superseded from, retrieved), as canonical dates.
pub struct Window<'a> {
    pub start: &'a str,
    pub superseded: Option<&'a str>,
    pub retrieved: &'a str,
}

/// Refuses a manifest entry whose dates cannot all be true at once.
pub fn window(title: &Json) -> Result<Window<'_>, Exc> {
    let start = date(title, "versionStart")?;
    let retrieved = date(title, "retrieved")?;
    if start > retrieved {
        return Err(Exc::value(format!(
            "{}: versionStart is after retrieved",
            id(title)
        )));
    }
    let current = get(title, "version_is_current")?.unwrap_or(&Json::Bool(true));
    let Json::Bool(current) = current else {
        return Err(Exc::value(format!(
            "{}: version_is_current must be true or false, not {}",
            id(title),
            repr(current)
        )));
    };
    if *current {
        return Ok(Window {
            start,
            superseded: None,
            retrieved,
        });
    }
    let superseded = date(title, "current_version_start")?;
    if !(start < superseded && superseded <= retrieved) {
        return Err(Exc::value(format!(
            "{}: current_version_start must fall after versionStart and on or before retrieved",
            id(title)
        )));
    }
    Ok(Window {
        start,
        superseded: Some(superseded),
        retrieved,
    })
}

/// How the compilation captured for `title` relates to the date `on`.
pub fn classify(title: &Json, on: &str) -> Result<&'static str, Exc> {
    let w = window(title)?;
    Ok(if on < w.start {
        BEFORE_COMPILATION
    } else if let Some(superseded) = w.superseded {
        if on >= superseded {
            SUPERSEDED
        } else {
            CAPTURED
        }
    } else if on > w.retrieved {
        AFTER_RETRIEVAL
    } else {
        CAPTURED
    })
}

/// NFKC, with the Register's non-breaking hyphen (U+2011, which NFKC folds to
/// U+2010) read as `-`. Case is not folded.
fn literal(text: &str) -> String {
    text.nfkc()
        .map(|c| if c == '\u{2010}' { '-' } else { c })
        .collect()
}

/// Whether `quote` appears in `text` once both are NFKC-normalised.
///
/// A match shows the quotation is faithful to the row up to the forms NFKC
/// folds; it does not show that the row states the law for the question or
/// the date. A blank quotation is refused rather than matching every text.
pub fn quote_in_text(quote: &str, text: &str) -> Result<bool, Exc> {
    if py::strip(quote).is_empty() {
        return Err(Exc::value("a blank quotation matches every text"));
    }
    Ok(literal(text).contains(&literal(quote)))
}

/// The titles a manifest iterates over, as `for title in titles` sees them.
fn iterate(titles: &Json) -> Result<Vec<Json>, Exc> {
    match titles {
        Json::Array(items) => Ok(items.clone()),
        // A dict iterates its keys and a str its characters, all strs.
        Json::Object(pairs) => Ok(pairs.iter().map(|(k, _)| Json::Str(k.clone())).collect()),
        Json::Str(s) => Ok(s
            .code_points()
            .into_iter()
            .map(|cp| Json::Str(crate::pyjson::PyStr::from_code_points(vec![cp])))
            .collect()),
        other => Err(Exc::Type(format!(
            "'{}' object is not iterable",
            type_name(other)
        ))),
    }
}

/// The manifest path the module's own directory supplies.
fn default_manifest() -> String {
    let directory = std::env::var("FADDEN_MODULE_DIR").unwrap_or_else(|_| {
        let cwd = std::env::current_dir().unwrap_or_default();
        pypath::join(&cwd.to_string_lossy(), "fadden")
    });
    pypath::join(&directory, "manifest_md.json")
}

fn parser(default: &str) -> Parser {
    Parser::new("python -m fadden as_at")
        .description("Classify each title's captured compilation against one date.")
        .arg(Action::positional("date").help("the date to test, as YYYY-MM-DD"))
        .arg(
            Action::option("--manifest")
                .default(crate::argparse::Value::Str(default.into()))
                .help("manifest_md.json to read"),
        )
        .arg(Action::flag("--json").help("print one JSON object per title"))
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let parser = parser(&default_manifest());
    let args = parser.parse(argv)?;
    let on = args.str("date").unwrap_or_default();
    if !is_date(&on) {
        return Err(parser.error(&format!(
            "date must be YYYY-MM-DD, not {}",
            py::repr_str(&on)
        )));
    }
    let manifest = args.str("manifest").unwrap_or_default();
    let loaded = crate::pyio::read_text(&manifest)
        .map_err(Exc::from)
        .and_then(|text| pyjson::loads_standard(&text).map_err(json_error))
        .and_then(|titles| {
            let titles = iterate(&titles)?;
            let statuses = titles
                .iter()
                .map(|title| classify(title, &on))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((titles, statuses))
        });
    let (titles, statuses) = match loaded {
        Ok(rows) => rows,
        Err(error @ (Exc::Os(_) | Exc::Value { .. } | Exc::Type(_) | Exc::Attribute(_))) => {
            run.eprint(&format!("error: {error}"));
            return Ok(1);
        }
        Err(error) => return Ok(run.uncaught(&error)),
    };
    if args.flag("json") {
        for (title, status) in titles.iter().zip(&statuses) {
            let w = window(title).expect("every title was windowed while classifying");
            let s = Json::str;
            let row = Json::obj([
                (
                    "register_id",
                    title.get("id").cloned().unwrap_or(Json::Null),
                ),
                ("name", title.get("name").cloned().unwrap_or(Json::Null)),
                ("status", s(status)),
                ("compilation_date", s(w.start)),
                ("superseded_from", w.superseded.map_or(Json::Null, s)),
                ("retrieved", s(w.retrieved)),
            ]);
            if let Err(error) = run.print_points(&dumps_line(&row, false)) {
                return Ok(run.uncaught(&error));
            }
        }
        return Ok(0);
    }
    run.print(&format!("{} titles against {on}:", titles.len()));
    for status in STATUSES {
        let count = statuses.iter().filter(|s| **s == status).count();
        let note = match action(status) {
            Some(text) if count > 0 => format!(" ({text})"),
            _ => String::new(),
        };
        run.print(&format!("  {status}: {count}{note}"));
    }
    run.print(
        "captured means the build holds the compilation for that date; it is not a \
         finding that the text was the law then.",
    );
    Ok(0)
}
