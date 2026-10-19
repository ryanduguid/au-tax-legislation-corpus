//! `fadden/pii_scan.py` and `fadden/pii_scan2.py`: find corpus rows that name
//! private individuals or carry contact details.
//!
//! The first scan flags the shape of a disciplinary register: a row with at
//! least 3 personal names and 3 agent registration numbers. It writes
//! `pii_flagged.json` beside the module for finalize to report. The second scan
//! catches what that shape misses: any row pairing a name with an 8-digit
//! number outside the titles the first scan flagged, and contact details
//! anywhere, reported only as digests unless the hashed allowlist approves
//! them. Both read the corpus beside the module and take no arguments.
use super::corpus_paths::{
    child, corpus_root, module_dir, register_id_str, require_builder_layout, script_dir,
    sections_files, write_text,
};
use super::pii::{self, contact_fingerprints, private_person_registration_details};
use super::{Exc, Run, get, getitem, head, iterate, json_error};
use crate::argparse::Exit;
use crate::py;
use crate::pyio::Lines;
use crate::pyjson::{self, Json, PyStr};
use crate::pypath;
use crate::pyvalue::{self, Key, dumps_with, repr, str_points, type_name};

type P = Vec<u32>;
/// A contact match shown without its identifier: title, row id, digest prefix.
type Example = (String, Json, String);

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

/// `"%-*s" % value` for code points.
fn pad(mut points: P, width: usize) -> P {
    while points.len() < width {
        points.push(u32::from(' '));
    }
    points
}

/// `r.get("text") or ""`, as the regex scanners accept it.
fn row_text(row: &Json) -> Result<String, Exc> {
    match get(row, "text")? {
        Some(Json::Str(s)) => Ok(pyvalue::points_lossy(&s.code_points())),
        Some(v) if v.truthy() => Err(Exc::Type(format!(
            "expected string or bytes-like object, got '{}'",
            type_name(v)
        ))),
        _ => Ok(String::new()),
    }
}

/// `json.load(open(path))`.
fn load(path: &str) -> Result<Json, Exc> {
    let text = crate::pyio::read_text(path)?;
    pyjson::loads_standard(&text).map_err(json_error)
}

/// `for line in open(sections.jsonl)`, one decoded row per non-blank line.
fn for_each_row(path: &str, mut f: impl FnMut(Json) -> Result<(), Exc>) -> Result<(), Exc> {
    let mut lines = Lines::open(path)?;
    while let Some(line) = lines.next_line() {
        let line = line?;
        if py::strip(&line).is_empty() {
            continue;
        }
        f(pyjson::loads_standard(&line).map_err(json_error)?)?;
    }
    Ok(())
}

fn dict_key(value: &Json, container: &str) -> Result<Key, Exc> {
    pyvalue::key(value).map_err(|m| {
        Exc::Type(format!(
            "cannot use '{}' as a {container} ({m})",
            type_name(value)
        ))
    })
}

struct Flagged {
    register_id: String,
    name: Json,
    collection: Json,
    rows_total: usize,
    rows_flagged: usize,
    names_est: usize,
    words: usize,
    row_ids: Vec<Json>,
}

fn int(n: usize) -> Json {
    Json::Int(n.into())
}

fn first_scan(run: &mut Run) -> Result<(), Exc> {
    let directory = script_dir("pii_scan.py")?;
    let root = corpus_root(&directory);
    require_builder_layout(&directory)?;
    let scratch = module_dir();
    let titles = load(&pypath::join(&scratch, "manifest_md.json"))?;
    let mut byid: Vec<(Key, Json)> = Vec::new();
    for t in iterate(&titles)? {
        let key = dict_key(getitem(&t, "id")?, "dict key")?;
        match byid.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = t,
            None => byid.push((key, t)),
        }
    }

    let mut flagged: Vec<Flagged> = Vec::new();
    let markdown_root = child(&root, &["markdown"])?;
    for name in sections_files(&markdown_root)? {
        let rid = register_id_str(&name)?;
        let path = child(&markdown_root, &[&rid, "sections.jsonl"])?;
        let mut rows = Vec::new();
        for_each_row(&path, |row| {
            rows.push(row);
            Ok(())
        })?;
        let mut hot: Vec<(Json, usize, usize)> = Vec::new();
        for row in &rows {
            let text = row_text(row)?;
            let (names, numbers) = private_person_registration_details(&text);
            // A disciplinary table has many of both; a signature block has one
            // name and no registration number.
            if numbers.len() >= 3 && names.len() >= 3 {
                let words = text
                    .split(|c: char| py::is_space(c))
                    .filter(|w| !w.is_empty())
                    .count();
                hot.push((
                    row.get("row_id").cloned().unwrap_or(Json::Null),
                    names.len(),
                    words,
                ));
            }
        }
        if !hot.is_empty() {
            let title = byid
                .iter()
                .find(|(k, _)| *k == Key::Str(pts(&rid)))
                .map(|(_, t)| t);
            let field = |name: &str| title.and_then(|t| t.get(name).cloned());
            flagged.push(Flagged {
                register_id: rid,
                name: field("name").unwrap_or_else(|| Json::str("?")),
                collection: field("collection").unwrap_or(Json::Null),
                rows_total: rows.len(),
                rows_flagged: hot.len(),
                names_est: hot.iter().map(|h| h.1).sum(),
                words: hot.iter().map(|h| h.2).sum(),
                row_ids: hot.into_iter().map(|h| h.0).collect(),
            });
        }
    }

    flagged.sort_by_key(|f| std::cmp::Reverse(f.names_est));
    run.print(&format!(
        "titles carrying person-name rows: {}",
        flagged.len()
    ));
    run.print(&format!(
        "{:<13} {:<6} {:<7} {:<8} TITLE",
        "REGISTER_ID", "ROWS", "FLAG", "NAMES"
    ));
    for f in &flagged {
        let mut line = pts(&format!(
            "  {:<12} {:>5} {:>6} {:>7}  ",
            f.register_id, f.rows_total, f.rows_flagged, f.names_est
        ));
        line.extend(str_points(&head(&f.name, 52)?));
        run.print_points(&line)?;
    }
    let words: usize = flagged.iter().map(|f| f.words).sum();
    run.print(&format!(
        "\ntotal flagged rows: {} | est. distinct names: {} | words: {}",
        flagged.iter().map(|f| f.rows_flagged).sum::<usize>(),
        flagged.iter().map(|f| f.names_est).sum::<usize>(),
        super::rates::thousands(words)
    ));
    let document = Json::Array(
        flagged
            .into_iter()
            .map(|f| {
                Json::Object(vec![
                    (PyStr::Text("register_id".into()), Json::str(&f.register_id)),
                    (PyStr::Text("name".into()), f.name),
                    (PyStr::Text("collection".into()), f.collection),
                    (PyStr::Text("rows_total".into()), int(f.rows_total)),
                    (PyStr::Text("rows_flagged".into()), int(f.rows_flagged)),
                    (PyStr::Text("names_est".into()), int(f.names_est)),
                    (PyStr::Text("words".into()), int(f.words)),
                    (PyStr::Text("row_ids".into()), Json::Array(f.row_ids)),
                ])
            })
            .collect(),
    );
    let path = pypath::join(&scratch, "pii_flagged.json");
    if let Err(error) = super::refuse_deep(&document) {
        // Python opened the file before its encoder stopped.
        std::fs::File::create(&path)
            .map_err(|e| crate::pyos::OsError::new(&e, Some(&path), crate::pyos::Style::Errno))?;
        return Err(error);
    }
    write_text(&path, &dumps_with(&document, Some(1), true, false))
}

/// `python -m fadden pii_scan`: the stage takes no arguments.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    Ok(match first_scan(run) {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}

/// `dict(counter)` as `print` shows it, or `"none"` when empty.
fn counts(items: &[(&'static str, usize)]) -> String {
    if items.is_empty() {
        return "none".into();
    }
    let body: Vec<String> = items.iter().map(|(k, n)| format!("'{k}': {n}")).collect();
    format!("{{{}}}", body.join(", "))
}

fn bump(items: &mut Vec<(&'static str, usize)>, label: &'static str) {
    match items.iter_mut().find(|(k, _)| *k == label) {
        Some(item) => item.1 += 1,
        None => items.push((label, 1)),
    }
}

fn second_scan(run: &mut Run) -> Result<i32, Exc> {
    let directory = script_dir("pii_scan2.py")?;
    let root = corpus_root(&directory);
    require_builder_layout(&directory)?;
    let scratch = module_dir();
    let flagged = load(&pypath::join(&scratch, "pii_flagged.json"))?;
    let mut known: Vec<Key> = Vec::new();
    for item in iterate(&flagged)? {
        let key = dict_key(getitem(&item, "register_id")?, "set element")?;
        if !known.contains(&key) {
            known.push(key);
        }
    }
    let approved =
        pii::load_contact_allowlist(&pypath::join(&scratch, "pii_contact_allowlist.json"))?;
    let mut weak: Vec<(String, Json, usize, usize)> = Vec::new();
    let mut contacts: Vec<(&'static str, usize)> = Vec::new();
    let mut unapproved: Vec<(&'static str, usize)> = Vec::new();
    let mut examples: Vec<(&'static str, Vec<Example>)> = Vec::new();
    let markdown_root = child(&root, &["markdown"])?;
    for name in sections_files(&markdown_root)? {
        let rid = register_id_str(&name)?;
        let path = child(&markdown_root, &[&rid, "sections.jsonl"])?;
        let is_known = known.contains(&Key::Str(pts(&rid)));
        for_each_row(&path, |row| {
            let text = row_text(&row)?;
            let row_id = || row.get("row_id").cloned().unwrap_or(Json::Null);
            for (label, digest) in contact_fingerprints(&text) {
                bump(&mut contacts, label);
                let key = (label.to_string(), digest.clone(), rid.clone());
                if !approved.contains(&key) {
                    bump(&mut unapproved, label);
                    // Keep enough to find and compare a match without copying
                    // the contact identifier into logs.
                    let slot = match examples.iter().position(|(k, _)| *k == label) {
                        Some(i) => i,
                        None => {
                            examples.push((label, Vec::new()));
                            examples.len() - 1
                        }
                    };
                    if examples[slot].1.len() < 5 {
                        examples[slot]
                            .1
                            .push((rid.clone(), row_id(), digest[..16].to_string()));
                    }
                }
            }
            if is_known {
                return Ok(());
            }
            let (names, numbers) = private_person_registration_details(&text);
            if !names.is_empty() && !numbers.is_empty() {
                weak.push((rid.clone(), row_id(), names.len(), numbers.len()));
            }
            Ok(())
        })?;
    }

    run.print(&format!(
        "rows outside the 12 known titles pairing a name with an 8-digit number: {}",
        weak.len()
    ));
    for (rid, row_id, names, numbers) in weak.iter().take(12) {
        let mut line = pts(&format!("   {rid:<12} "));
        line.extend(pad(str_points(row_id), 26));
        line.extend(pts(&format!(" names={names} nums={numbers}")));
        run.print_points(&line)?;
    }
    run.print("");
    run.print(&format!(
        "contact details across the whole corpus: {}",
        counts(&contacts)
    ));
    run.print(&format!(
        "unapproved contact details: {}",
        counts(&unapproved)
    ));
    for (label, found) in &examples {
        let shown: Vec<String> = found
            .iter()
            .take(3)
            .map(|(rid, row_id, digest)| {
                format!(
                    "({}, {}, {})",
                    py::repr_str(rid),
                    repr(row_id),
                    py::repr_str(digest)
                )
            })
            .collect();
        run.print(&format!(
            "   {label:<6} register_id, row_id, sha256[:16]: [{}]",
            shown.join(", ")
        ));
    }
    Ok(i32::from(!weak.is_empty() || !unapproved.is_empty()))
}

/// `python -m fadden pii_scan2`: 1 when it found anything to review.
pub fn main2(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    Ok(match second_scan(run) {
        Ok(code) => code,
        Err(error) => run.uncaught(&error),
    })
}
