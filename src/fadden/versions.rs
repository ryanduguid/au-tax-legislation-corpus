//! `fadden/versions.py`: deduplicate the discovered titles by Register id and
//! resolve each principal title's current version, 8 titles to a request,
//! writing `acts_resolved.json` beside the module. A batch the API cannot
//! answer switches the rest of the run to single-title lookups; an ambiguous
//! batch is retried title by title; a malformed one, or any title left
//! unresolved, stops the stage before anything is written.
use super::corpus_paths::{
    compilation_id, module_dir, register_id, require_builder_layout, script_dir_in, version_date,
    version_rows, write_json_atomic,
};
use super::discover::quote;
use super::http::{self, Http};
use super::{Exc, Run, get, getitem, head, iterate, json_error};
use crate::argparse::Exit;
use crate::pyjson::{self, Json, PyStr};
use crate::pypath;
use crate::pyvalue::{self, py_str};

const API: &str = "https://api.prod.legislation.gov.au/v1";
const VERSION_BATCH_SIZE: usize = 8;
const RESOLUTION_DELAY: f64 = 1.5;
const INVALID_BATCH: &str = "invalid version batch; refusing to write acts_resolved.json";

/// `_versions_url(title_ids)`.
fn versions_url(title_ids: &[String]) -> String {
    let mut condition = title_ids
        .iter()
        .map(|rid| format!("titleId eq '{rid}'"))
        .collect::<Vec<_>>()
        .join(" or ");
    if title_ids.len() > 1 {
        condition = format!("({condition})");
    }
    condition.push_str(" and isCurrent eq true");
    let top = if title_ids.len() > 1 {
        title_ids.len() + 1
    } else {
        1
    };
    format!(
        "{API}/versions?$top={top}&$filter={}&$select=titleId,start,compilationNumber,registerId",
        quote(&condition)
    )
}

/// A `ValueError` from the row checks becomes the batch's `RuntimeError`;
/// anything else escapes as Python's would.
fn as_invalid_batch(error: Exc) -> Exc {
    match error {
        Exc::Value { .. } => Exc::Runtime(INVALID_BATCH.into()),
        other => other,
    }
}

/// `_batch_rows(payload, title_ids)`: every row checked; `None` when a valid
/// group needs singleton recovery.
fn batch_rows(payload: &Json, title_ids: &[String]) -> Result<Option<Vec<(String, Json)>>, Exc> {
    let invalid = || Exc::Runtime(INVALID_BATCH.into());
    let Some(Json::Array(rows)) = payload.get("value") else {
        return Err(invalid());
    };
    if payload.get("@odata.nextLink").is_some()
        || payload.get("@nextLink").is_some()
        || rows.len() > title_ids.len() + 1
    {
        return Err(invalid());
    }
    let mut by_id: Vec<(String, Json)> = Vec::new();
    let mut duplicate = false;
    for row in rows {
        if !matches!(row, Json::Object(_)) {
            return Err(invalid());
        }
        let Some(Json::Str(rid)) = row.get("titleId") else {
            return Err(invalid());
        };
        // A lone surrogate can never equal a validated identifier.
        let Some(rid) = rid.as_str().map(str::to_string) else {
            return Err(invalid());
        };
        if !title_ids.contains(&rid) {
            return Err(invalid());
        }
        let single = Json::Object(vec![(
            PyStr::Text("value".into()),
            Json::Array(vec![row.clone()]),
        )]);
        version_rows(&single, &rid).map_err(as_invalid_batch)?;
        version_date(row.get("start")).map_err(as_invalid_batch)?;
        compilation_id(row).map_err(as_invalid_batch)?;
        match by_id.iter_mut().find(|(id, _)| *id == rid) {
            Some(entry) => {
                duplicate = true;
                entry.1 = row.clone();
            }
            None => by_id.push((rid, row.clone())),
        }
    }
    Ok((!duplicate && by_id.len() == title_ids.len()).then_some(by_id))
}

/// `dict(t)` with `field` set: an existing key keeps its place.
fn set_field(record: &mut Json, field: &str, value: Json) {
    if let Json::Object(pairs) = record {
        match pairs.iter_mut().find(|(k, _)| k.as_str() == Some(field)) {
            Some(pair) => pair.1 = value,
            None => pairs.push((PyStr::Text(field.into()), value)),
        }
    }
}

/// Resolve every principal title's current version, with the module in `scratch`.
pub fn run(http: &mut dyn Http, scratch: &str, run: &mut Run) -> Result<(), Exc> {
    require_builder_layout(&script_dir_in(scratch, "versions.py")?)?;
    let text = crate::pyio::read_text(&pypath::join(scratch, "titles_all.json"))?;
    let rows = pyjson::loads_standard(&text).map_err(json_error)?;

    // Proper dedup by register id: the last row wins, the first keeps its place.
    let mut by_id: Vec<(String, Json)> = Vec::new();
    for row in iterate(&rows)? {
        let rid = register_id(getitem(&row, "id")?)?;
        match by_id.iter_mut().find(|(id, _)| *id == rid) {
            Some(entry) => entry.1 = row,
            None => by_id.push((rid, row)),
        }
    }
    let mut principal = Vec::new();
    for (_, row) in &by_id {
        if get(row, "isPrincipal")?.is_some_and(Json::truthy) {
            principal.push(row.clone());
        }
    }
    let mut names = Vec::with_capacity(principal.len());
    for row in &principal {
        names.push(getitem(row, "name")?.clone());
    }
    let mut order: Vec<usize> = (0..principal.len()).collect();
    crate::pysort::sort(&mut order, |&a, &b| {
        super::rates::py_lt(&names[a], &names[b])
    })?;
    let principal: Vec<Json> = order.into_iter().map(|i| principal[i].clone()).collect();

    // titles_all.json holds Acts and both kinds of instrument, so both totals
    // count titles; each row's own `collection` says which kind it is.
    let mut by_collection: Vec<(pyvalue::Key, Json, usize)> = Vec::new();
    for (_, row) in &by_id {
        let name = match get(row, "collection")? {
            Some(value) if value.truthy() => value.clone(),
            _ => Json::str("unknown"),
        };
        let key = pyvalue::key(&name).map_err(|_| {
            let kind = pyvalue::type_name(&name);
            Exc::Type(format!(
                "cannot use '{kind}' as a dict key (unhashable type: '{kind}')"
            ))
        })?;
        match by_collection.iter_mut().find(|(k, _, _)| *k == key) {
            Some(entry) => entry.2 += 1,
            None => by_collection.push((key, name, 1)),
        }
    }
    run.print(&format!("distinct in-force titles: {}", by_id.len()));
    run.print(&format!("distinct principal titles:   {}", principal.len()));
    crate::pysort::sort(&mut by_collection, |a, b| super::rates::py_lt(&a.1, &b.1))?;
    // `print("by collection:", ", ".join("%s %d" % ...))`, printed as Python prints a str.
    let mut line: Vec<u32> = "by collection: ".chars().map(u32::from).collect();
    for (position, (_, name, count)) in by_collection.iter().enumerate() {
        if position > 0 {
            line.extend(", ".chars().map(u32::from));
        }
        line.extend(pyvalue::str_points(name));
        line.extend(format!(" {count}").chars().map(u32::from));
    }
    run.print_points(&line)?;

    // Probe the versions filter shape once before looping.
    let probe = http::fetch_json(
        http,
        &format!(
            "{API}/versions?$top=2&$filter={}&$select=titleId,start,compilationNumber,isCurrent",
            quote("titleId eq 'C2004A05138' and isCurrent eq true")
        ),
    )?;
    let shown = match &probe {
        Some(document @ Json::Object(pairs)) if !pairs.is_empty() => {
            document.get("value").cloned().unwrap_or(Json::Null)
        }
        _ => Json::Null,
    };
    super::refuse_deep(&shown)?;
    let dumped: String = pyvalue::points_lossy(&pyvalue::dumps_line(&shown, true))
        .chars()
        .take(200)
        .collect();
    run.print(&format!("\nprobe isCurrent filter: {dumped}"));
    let has_rows = match &probe {
        Some(Json::Object(pairs)) if !pairs.is_empty() => probe
            .as_ref()
            .and_then(|d| d.get("value"))
            .is_some_and(Json::truthy),
        _ => false,
    };
    if !has_rows {
        // The probe is diagnostic; resolution failures below prevent a partial manifest.
        run.print(
            "!! isCurrent filter probe returned no rows; current-version resolution will still be attempted",
        );
    }

    let (mut resolved, mut failed): (Vec<Json>, Vec<Json>) = (Vec::new(), Vec::new());
    let (mut index, mut batching) = (0, true);
    while index < principal.len() {
        let size = if batching { VERSION_BATCH_SIZE } else { 1 };
        let group = &principal[index..(index + size).min(principal.len())];
        let mut title_ids = Vec::with_capacity(group.len());
        for title in group {
            title_ids.push(register_id(getitem(title, "id")?)?);
        }
        let batch = http::fetch_json(http, &versions_url(&title_ids))?;
        http.sleep(RESOLUTION_DELAY);
        if group.len() > 1 && batch.is_none() {
            // No decoded rows to admit; use the singleton path for this run.
            run.print("!! version batch unavailable; switching to single-title lookups");
            batching = false;
            continue;
        }
        let rows_by_id = if group.len() > 1 {
            batch_rows(batch.as_ref().unwrap_or(&Json::Null), &title_ids)?
        } else {
            Some(Vec::new())
        };
        if rows_by_id.is_none() {
            run.print(&format!(
                "!! ambiguous version batch; retrying {} title(s) individually",
                group.len()
            ));
        }
        for (offset, title) in group.iter().enumerate() {
            let position = index + offset + 1;
            let rid = getitem(title, "id")?;
            let rid_text = py_str(rid);
            let payload = match &rows_by_id {
                None => {
                    let payload =
                        http::fetch_json(http, &versions_url(std::slice::from_ref(&rid_text)))?;
                    http.sleep(RESOLUTION_DELAY);
                    payload.unwrap_or(Json::Null)
                }
                Some(rows) if group.len() > 1 => {
                    let row = rows
                        .iter()
                        .find(|(id, _)| *id == rid_text)
                        .map(|(_, row)| row.clone())
                        .ok_or_else(|| Exc::Key(pyvalue::repr(rid)))?;
                    Json::Object(vec![(PyStr::Text("value".into()), Json::Array(vec![row]))])
                }
                Some(_) => batch.clone().unwrap_or(Json::Null),
            };
            // version_rows and the field checks: a ValueError is a failed title.
            let outcome = (|| -> Result<Option<(Json, String, Option<String>)>, Exc> {
                let versions = version_rows(&payload, &rid_text)?;
                let Some(first) = versions.first() else {
                    return Ok(None);
                };
                let start = version_date(first.get("start"))?;
                let document = compilation_id(first)?;
                Ok(Some((first.clone(), start, document)))
            })();
            let found = match outcome {
                Ok(found) => found,
                Err(Exc::Value { .. }) => None,
                Err(other) => return Err(other),
            };
            match found {
                Some((first, start, document)) => {
                    let mut record = title.clone();
                    set_field(&mut record, "versionStart", Json::str(&start));
                    set_field(
                        &mut record,
                        "compilationNumber",
                        first
                            .get("compilationNumber")
                            .cloned()
                            .unwrap_or(Json::Null),
                    );
                    set_field(
                        &mut record,
                        "compilationRegisterId",
                        document.map_or(Json::Null, |d| Json::str(&d)),
                    );
                    resolved.push(record);
                }
                None => failed.push(title.clone()),
            }
            if position % 25 == 0 {
                run.print(&format!(
                    "  resolved {position}/{} (failed {})",
                    principal.len(),
                    failed.len()
                ));
            }
        }
        index += group.len();
    }

    run.print(&format!(
        "\nresolved: {}   failed: {}",
        resolved.len(),
        failed.len()
    ));
    for title in failed.iter().take(10) {
        let mut line: Vec<u32> = "   FAILED ".chars().map(u32::from).collect();
        line.extend(pyvalue::str_points(getitem(title, "id")?));
        line.push(u32::from(' '));
        line.extend(pyvalue::str_points(&head(getitem(title, "name")?, 70)?));
        run.print_points(&line)?;
    }
    // The next stage treats acts_resolved.json as a complete work list, so a
    // network or API miss fails the stage instead of shrinking the corpus.
    if !failed.is_empty() {
        return Err(Exc::Runtime(format!(
            "version resolution incomplete for {} title(s); refusing to write acts_resolved.json",
            failed.len()
        )));
    }
    write_json_atomic(
        &pypath::join(scratch, "acts_resolved.json"),
        &Json::Array(resolved),
        Some(1),
    )
}

/// `python -m fadden versions`: the stage takes no arguments.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let mut http = http::connect();
    Ok(match self::run(http.as_mut(), &module_dir(), run) {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}
