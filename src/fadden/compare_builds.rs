//! `fadden/compare_builds.py`: list the sections that changed between two builds.
//!
//! Reads two corpus roots, each holding `markdown/<register_id>/sections.jsonl`,
//! and compares every title present in both. Rows match on their container and
//! section label (a repeated label in order of appearance, a row without one by
//! its heading). The comparison covers the derived reading view, not the
//! authorised text, and is no finding about an amendment's legal effect.
use std::collections::{BTreeSet, HashMap};

use super::corpus_paths::{child, register_id, register_id_str};
use super::{Exc, Run, json_error};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyio::Lines;
use crate::pyjson::{self, Json};
use crate::pyos::{OsError, Style};
use crate::pyvalue::{self, Key, dumps_line, py_eq, str_points, type_name};

/// `titles(root)`: the register ids under `root` that hold a sections.jsonl.
pub fn titles(root: &str) -> Result<BTreeSet<String>, Exc> {
    let markdown = child(root, &["markdown"])?;
    let entries = std::fs::read_dir(&markdown)
        .map_err(|e| OsError::new(&e, Some(&markdown), Style::Native))?;
    let mut found = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|e| OsError::new(&e, Some(&markdown), Style::Native))?;
        let Ok(rid) = register_id_str(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let file = child(&markdown, &[&rid, "sections.jsonl"])?;
        if std::fs::metadata(&file).is_ok_and(|m| m.is_file()) {
            found.insert(rid);
        }
    }
    Ok(found)
}

/// `load_rows(root, rid)`: the rows of one title's sections.jsonl.
pub fn load_rows(root: &str, rid: &str) -> Result<Vec<Json>, Exc> {
    let path = child(
        root,
        &["markdown", &register_id_str(rid)?, "sections.jsonl"],
    )?;
    let mut lines = Lines::open(&path)?;
    let mut rows = Vec::new();
    let mut number = 0;
    while let Some(line) = lines.next_line() {
        let line = line?;
        number += 1;
        if crate::py::strip(&line).is_empty() {
            continue;
        }
        let row = pyjson::loads_standard(&line).map_err(json_error)?;
        if !matches!(row.get("text"), Some(Json::Str(_))) {
            return Err(Exc::value(format!(
                "{path}:{number} is not a sections.jsonl row"
            )));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// `row.get(field)`, None when absent.
fn field<'a>(row: &'a Json, name: &str) -> &'a Json {
    row.get(name).unwrap_or(&Json::Null)
}

/// `f"{value}"` as code points.
fn text(value: &Json) -> Vec<u32> {
    str_points(value)
}

fn prefixed(prefix: &str, value: &Json) -> Vec<u32> {
    let mut out: Vec<u32> = prefix.chars().map(u32::from).collect();
    out.extend(text(value));
    out
}

/// `keyed(rows)`: rows by (container, label, occurrence), in insertion order.
fn keyed(rows: &[Json]) -> Result<Vec<(Key, usize)>, Exc> {
    let mut seen: HashMap<(Key, Key), usize> = HashMap::new();
    let mut result = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let label = if field(row, "section").truthy() {
            prefixed("s ", field(row, "section"))
        } else {
            let heading = field(row, "heading");
            let shown = if heading.truthy() {
                text(heading)
            } else {
                Vec::new()
            };
            let mut out: Vec<u32> = "heading ".chars().map(u32::from).collect();
            out.extend(shown);
            out
        };
        let container = field(row, "container");
        let container = if container.truthy() {
            container.clone()
        } else {
            Json::str("")
        };
        let base = (
            pyvalue::key(&container)
                .map_err(|m| Exc::Type(format!("cannot use 'tuple' as a dict key ({m})")))?,
            Key::Str(label),
        );
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        result.push((
            Key::Tuple(vec![
                base.0,
                base.1,
                Key::Str(count.to_string().chars().map(u32::from).collect()),
            ]),
            index,
        ));
    }
    Ok(result)
}

fn place(row: &Json) -> Vec<(&'static str, Json)> {
    vec![
        ("container", field(row, "container").clone()),
        ("section", field(row, "section").clone()),
        ("heading", field(row, "heading").clone()),
    ]
}

fn object(pairs: Vec<(&str, Json)>) -> Json {
    Json::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (crate::pyjson::PyStr::Text(k.to_string()), v))
            .collect(),
    )
}

fn compilation(rows: &[Json]) -> Json {
    match rows.first() {
        None => Json::Null,
        Some(row) => object(vec![
            ("number", field(row, "compilation_number").clone()),
            ("date", field(row, "compilation_date").clone()),
        ]),
    }
}

/// `compare_title(old_rows, new_rows)`.
pub fn compare_title(old_rows: &[Json], new_rows: &[Json]) -> Result<Json, Exc> {
    let old = keyed(old_rows)?;
    let new = keyed(new_rows)?;
    let old_index: HashMap<&Key, usize> = old.iter().map(|(k, i)| (k, *i)).collect();
    let new_keys: std::collections::HashSet<&Key> = new.iter().map(|(k, _)| k).collect();
    let mut changed = Vec::new();
    let mut added = Vec::new();
    for (key, index) in &new {
        let row = &new_rows[*index];
        let Some(&at) = old_index.get(key) else {
            added.push(object(place(row)));
            continue;
        };
        let before = &old_rows[at];
        let text_changed = !py_eq(field(before, "text"), field(row, "text"));
        let heading_changed = !py_eq(field(before, "heading"), field(row, "heading"));
        if text_changed || heading_changed {
            let change = match (text_changed, heading_changed) {
                (true, true) => "text and heading",
                (true, false) => "text",
                _ => "heading",
            };
            let mut pairs = place(row);
            pairs.push(("change", Json::str(change)));
            changed.push(object(pairs));
        }
    }
    let removed = old
        .iter()
        .filter(|(key, _)| !new_keys.contains(key))
        .map(|(_, index)| object(place(&old_rows[*index])))
        .collect();
    Ok(object(vec![
        ("old_compilation", compilation(old_rows)),
        ("new_compilation", compilation(new_rows)),
        ("added", Json::Array(added)),
        ("removed", Json::Array(removed)),
        ("changed", Json::Array(changed)),
    ]))
}

/// `_describe(place)`: "<container>, s <section>" or the heading.
fn describe(place: &Json) -> Result<Vec<u32>, Exc> {
    let mut parts: Vec<Vec<u32>> = Vec::new();
    let container = field(place, "container");
    if container.truthy() {
        match container {
            Json::Str(s) => parts.push(s.code_points()),
            other => {
                return Err(Exc::Type(format!(
                    "sequence item 0: expected str instance, {} found",
                    type_name(other)
                )));
            }
        }
    }
    let section = field(place, "section");
    parts.push(if section.truthy() {
        prefixed("s ", section)
    } else {
        text(field(place, "heading"))
    });
    Ok(parts.join(&[u32::from(','), u32::from(' ')][..]))
}

fn parser() -> Parser {
    Parser::new("python -m fadden compare_builds")
        .description("List the sections added, removed and changed between two corpus builds.")
        .arg(Action::positional("old_root").help("corpus root of the earlier build"))
        .arg(Action::positional("new_root").help("corpus root of the later build"))
        .arg(Action::option("--title").help("compare only this register id"))
        .arg(Action::flag("--json").help("print one JSON object per title"))
}

fn points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

type Reports = (BTreeSet<String>, BTreeSet<String>, Vec<(String, Json)>);

fn collect(args: &crate::argparse::Namespace) -> Result<Reports, Exc> {
    let (old_root, new_root) = (
        args.str("old_root").unwrap_or_default(),
        args.str("new_root").unwrap_or_default(),
    );
    let mut old_titles = titles(&old_root)?;
    let mut new_titles = titles(&new_root)?;
    if let Some(title) = args.str("title").filter(|t| !t.is_empty()) {
        let rid = register_id(&Json::str(&title))?;
        if !old_titles.contains(&rid) || !new_titles.contains(&rid) {
            return Err(Exc::value(format!("{rid} is not in both builds")));
        }
        old_titles = BTreeSet::from([rid.clone()]);
        new_titles = BTreeSet::from([rid]);
    }
    let mut reports = Vec::new();
    for rid in old_titles.intersection(&new_titles) {
        let old_rows = load_rows(&old_root, rid)?;
        let new_rows = load_rows(&new_root, rid)?;
        reports.push((rid.clone(), compare_title(&old_rows, &new_rows)?));
    }
    Ok((old_titles, new_titles, reports))
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    let (old_titles, new_titles, reports) = match collect(&args) {
        Ok(found) => found,
        Err(error @ (Exc::Os(_) | Exc::Value { .. })) => {
            run.eprint(&format!("error: {error}"));
            return Ok(1);
        }
        Err(error) => return Ok(run.uncaught(&error)),
    };
    match report(run, &args, &old_titles, &new_titles, &reports) {
        Ok(()) => Ok(0),
        Err(error) => Ok(run.uncaught(&error)),
    }
}

fn list_len(value: &Json) -> usize {
    match value {
        Json::Array(items) => items.len(),
        _ => 0,
    }
}

fn report(
    run: &mut Run,
    args: &crate::argparse::Namespace,
    old_titles: &BTreeSet<String>,
    new_titles: &BTreeSet<String>,
    reports: &[(String, Json)],
) -> Result<(), Exc> {
    let only_old: Vec<&String> = old_titles.difference(new_titles).collect();
    let only_new: Vec<&String> = new_titles.difference(old_titles).collect();
    if args.flag("json") {
        for (rid, report) in reports {
            let Json::Object(pairs) = report else {
                unreachable!()
            };
            let mut all = vec![(
                crate::pyjson::PyStr::Text("register_id".into()),
                Json::str(rid),
            )];
            all.extend(pairs.iter().cloned());
            run.print_points(&dumps_line(&Json::Object(all), false))?;
        }
        for (rids, side) in [(&only_old, "old"), (&only_new, "new")] {
            for rid in rids {
                let line = Json::obj([
                    ("register_id", Json::str(rid)),
                    ("only_in", Json::str(side)),
                ]);
                run.print_points(&dumps_line(&line, true))?;
            }
        }
        return Ok(());
    }
    let mut differing = 0;
    for (rid, report) in reports {
        let entries =
            ["added", "removed", "changed"].map(|k| report.get(k).cloned().unwrap_or(Json::Null));
        if entries.iter().all(|e| list_len(e) == 0) {
            continue;
        }
        differing += 1;
        let compilation = |name: &str, part: &str| -> Vec<u32> {
            match report.get(name) {
                Some(value @ Json::Object(_)) => text(field(value, part)),
                _ => points("None"),
            }
        };
        let mut line = points(&format!("{rid}: compilation "));
        line.extend(compilation("old_compilation", "number"));
        line.extend(points(" ("));
        line.extend(compilation("old_compilation", "date"));
        line.extend(points(") to "));
        line.extend(compilation("new_compilation", "number"));
        line.extend(points(" ("));
        line.extend(compilation("new_compilation", "date"));
        line.extend(points(")"));
        run.print_points(&line)?;
        let [added, removed, changed] = entries;
        if let Json::Array(items) = &changed {
            for place in items {
                let mut line = points("  changed (");
                line.extend(text(field(place, "change")));
                line.extend(points("): "));
                line.extend(describe(place)?);
                run.print_points(&line)?;
            }
        }
        for (items, label) in [(&added, "  added: "), (&removed, "  removed: ")] {
            if let Json::Array(items) = items {
                for place in items {
                    let mut line = points(label);
                    line.extend(describe(place)?);
                    run.print_points(&line)?;
                }
            }
        }
    }
    run.print(&format!(
        "{} titles compared, {differing} with section changes; {} only in the old build, {} only in the new build.",
        reports.len(),
        only_old.len(),
        only_new.len()
    ));
    run.print(
        "This compares the derived reading view; read both compilations on the Register \
         before relying on a change.",
    );
    Ok(())
}
