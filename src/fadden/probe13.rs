//! `fadden/probe13.py`: probe the Register's version history for every title
//! download recorded as having no EPUB, and write `probe13.json` beside the
//! module so retry13 can recover the titles with an older published
//! compilation. An API failure stops the stage rather than drop a title.
use super::corpus_paths::{
    compilation_id, module_dir, register_id, require_builder_layout, script_dir_in, version_date,
    version_rows, write_json_atomic,
};
use super::http::{self, Http};
use super::{Exc, Run, get, getitem, iterate, json_error};
use crate::argparse::Exit;
use crate::pyjson::{self, Json, PyStr};
use crate::pypath;
use crate::pyvalue::py_str;

const API: &str = "https://api.prod.legislation.gov.au/v1";
const DELAY: f64 = 1.5;
/// The container level at which `json.dump` stops writing probe13.json under
/// `python -m fadden` (CPython 3.15, base revision): a version member nested
/// 988 deep is written, 989 is refused (`tools/stage_depth_probe.py`).
const OUTPUT_DEPTH: usize = 991;

fn truthy(value: Option<&Json>) -> bool {
    value.is_some_and(Json::truthy)
}

/// Probe every title without an EPUB, with the module in `scratch`.
pub fn run(http: &mut dyn Http, scratch: &str, run: &mut Run) -> Result<(), Exc> {
    let directory = script_dir_in(scratch, "probe13.py")?;
    require_builder_layout(&directory)?;
    let text = crate::pyio::read_text(&pypath::join(scratch, "manifest_raw.json"))?;
    let manifest = pyjson::loads_standard(&text).map_err(json_error)?;
    let mut missing = Vec::new();
    for item in iterate(&manifest)? {
        if !truthy(get(&item, "epub")?) {
            missing.push(item);
        }
    }
    let mut output = Vec::new();
    for item in &missing {
        let rid = register_id(getitem(item, "id")?)?;
        // Newest first: $top caps the page, so ascending order would return the
        // 60 oldest versions of a long-history title.
        let url = format!(
            "{API}/versions?$top=60&$orderby=start%20desc&$filter=titleId%20eq%20%27{rid}%27\
             &$select=titleId,start,end,isCurrent,compilationNumber,registerId"
        );
        let Some(response) = http::fetch_json(http, &url)? else {
            return Err(Exc::Runtime(format!(
                "version lookup failed for {rid} after retries; refusing to write probe13.json with that title missing"
            )));
        };
        let versions = version_rows(&response, &rid)?;
        for version in &versions {
            version_date(version.get("start"))?;
            compilation_id(version)?;
        }
        let with_document: Vec<&Json> = versions
            .iter()
            .filter(|v| truthy(v.get("registerId")))
            .collect();
        let current: Vec<&Json> = versions
            .iter()
            .filter(|v| truthy(v.get("isCurrent")))
            .collect();
        // `max(..., key=start)`: the first of the latest starts.
        let mut latest: Option<&Json> = None;
        for version in with_document {
            let start = version
                .get("start")
                .and_then(Json::as_str)
                .unwrap_or_default();
            if latest
                .is_none_or(|l| start > l.get("start").and_then(Json::as_str).unwrap_or_default())
            {
                latest = Some(version);
            }
        }
        let current_has_doc = current.first().is_some_and(|c| truthy(c.get("registerId")));
        let name = getitem(item, "name")?.clone();
        output.push(Json::Object(vec![
            (PyStr::Text("id".into()), Json::str(&rid)),
            (PyStr::Text("name".into()), name),
            (
                PyStr::Text("n_versions".into()),
                Json::Int(versions.len().into()),
            ),
            (
                PyStr::Text("current_has_doc".into()),
                Json::Bool(current_has_doc),
            ),
            (
                PyStr::Text("latest_doc".into()),
                latest.cloned().unwrap_or(Json::Null),
            ),
        ]));
        let latest_id = latest.map_or_else(
            || "None".to_string(),
            |l| py_str(l.get("registerId").unwrap_or(&Json::Null)),
        );
        let latest_start: String = latest
            .and_then(|l| l.get("start"))
            .and_then(Json::as_str)
            .unwrap_or_default()
            .chars()
            .take(10)
            .collect();
        let shown = if current_has_doc { "True" } else { "False" };
        run.print(&format!(
            "{rid:<12} vers={:<3} cur_doc={shown:<5} latest={latest_id} {latest_start}",
            versions.len()
        ));
        http.sleep(DELAY);
    }
    write_json_atomic(
        &pypath::join(scratch, "probe13.json"),
        &Json::Array(output),
        Some(1),
        OUTPUT_DEPTH,
    )
}

/// `python -m fadden probe13`: the stage takes no arguments.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let mut http = http::connect();
    Ok(match self::run(http.as_mut(), &module_dir(), run) {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}
