//! `fadden/discover.py`: page every in-force title whose name holds a tax
//! keyword, across the Act, legislative instrument and notifiable instrument
//! collections, and write `titles_all.json` and `titles_principal.json` beside
//! the module. A failed or unordered page stops the stage rather than leave a
//! silently incomplete title list.
use super::corpus_paths::{module_dir, require_builder_layout, script_dir_in, write_json_atomic};
use super::http::{self, Http};
use super::{Exc, Run, get, getitem, head};
use crate::argparse::Exit;
use crate::pyjson::Json;
use crate::pypath;
use crate::pyvalue;

const API: &str = "https://api.prod.legislation.gov.au/v1";
const KEYWORDS: [&str; 5] = [
    "Tax",
    "Excise",
    "Superannuation",
    "Customs Tariff",
    "Medicare Levy",
];
// Acts alone leave out the operative detail: the ITAA, GST and TAA Regulations
// and the FBT rate determinations are all legislative instruments.
const COLLECTIONS: [&str; 3] = ["Act", "LegislativeInstrument", "NotifiableInstrument"];

/// Why the stage stopped: an uncaught exception, or `SystemExit(message)`,
/// which prints the message alone and exits 1.
pub enum Stop {
    Raised(Exc),
    Exit(String),
}

impl From<Exc> for Stop {
    fn from(error: Exc) -> Self {
        Stop::Raised(error)
    }
}

/// `urllib.parse.quote(text)`: UTF-8, keeping letters, digits, `_.-~` and `/`.
pub fn quote(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A set element's hash class, or the `TypeError` a set raises for a list or dict.
fn set_key(value: &Json) -> Result<pyvalue::Key, Exc> {
    pyvalue::key(value).map_err(|_| {
        let name = crate::pyvalue::type_name(value);
        Exc::Type(format!(
            "cannot use '{name}' as a set element (unhashable type: '{name}')"
        ))
    })
}

/// `{r["id"] for r in rows}`'s size, with the exceptions building it raises.
fn distinct_ids(rows: &[Json]) -> Result<usize, Exc> {
    let mut keys = std::collections::HashSet::new();
    for row in rows {
        keys.insert(set_key(getitem(row, "id")?)?);
    }
    Ok(keys.len())
}

/// `page_titles(keyword, collection)`.
fn page_titles(http: &mut dyn Http, keyword: &str, collection: &str) -> Result<Vec<Json>, Stop> {
    let filter = format!(
        "collection eq '{collection}' and isInForce eq true and contains(name,'{keyword}')"
    );
    let select = "id,name,isPrincipal,isInForce,collection,year,number,status";
    let (mut rows, mut skip) = (Vec::new(), 0);
    loop {
        let url = format!(
            "{API}/titles?$top=100&$skip={skip}&$orderby=id&$filter={}&$select={select}",
            quote(&filter)
        );
        let page = http::fetch_json(http, &url)?;
        // A partial title list is worse than none: later stages treat it as
        // authoritative, so a failed page is never a clean end of results.
        let Some(Json::Array(values)) = page.as_ref().and_then(|d| d.get("value")) else {
            return Err(Stop::Raised(Exc::Runtime(format!(
                "title discovery failed at skip={skip} for {}; refusing to write an incomplete title list",
                crate::py::repr_str(keyword)
            ))));
        };
        let count = values.len();
        rows.extend(values.iter().cloned());
        if distinct_ids(&rows)? != rows.len() {
            return Err(Stop::Exit(format!(
                "duplicate ids while paging {}: unordered paging",
                crate::py::repr_str(keyword)
            )));
        }
        if count < 100 {
            break;
        }
        skip += 100;
        http.sleep(2.0);
    }
    Ok(rows)
}

/// Discover every title, with the module in `scratch`.
pub fn run(http: &mut dyn Http, scratch: &str, run: &mut Run) -> Result<(), Stop> {
    require_builder_layout(&script_dir_in(scratch, "discover.py")?)?;
    let mut seen = std::collections::HashSet::new();
    let mut all_rows: Vec<Json> = Vec::new();
    for collection in COLLECTIONS {
        for keyword in KEYWORDS {
            let rows = page_titles(http, keyword, collection)?;
            let mut fresh = 0;
            for row in &rows {
                let id = set_key(getitem(row, "id")?)?;
                if seen.insert(id) {
                    all_rows.push(row.clone());
                    fresh += 1;
                }
            }
            run.print(&format!(
                "{collection:<22} {keyword:<16} matched {:>4}, new {fresh:>4}",
                rows.len()
            ));
            http.sleep(3.0);
        }
    }

    let mut principal = Vec::new();
    for row in &all_rows {
        if get(row, "isPrincipal")?.is_some_and(Json::truthy) {
            principal.push(row.clone());
        }
    }
    // The totals span all three collections, so they count titles, not Acts.
    run.print(&format!(
        "\ntotal distinct in-force titles: {}",
        all_rows.len()
    ));
    run.print(&format!(
        "of which principal (client-side filter): {}",
        principal.len()
    ));

    // Stage then replace, so a crash part way through never truncates the
    // list versions reads as its authoritative input.
    let principal_json = Json::Array(principal.clone());
    write_json_atomic(
        &pypath::join(scratch, "titles_all.json"),
        &Json::Array(all_rows),
        Some(1),
    )?;
    write_json_atomic(
        &pypath::join(scratch, "titles_principal.json"),
        &principal_json,
        Some(1),
    )?;

    run.print("\nsample principal titles:");
    let mut names = Vec::with_capacity(principal.len());
    for row in &principal {
        names.push(getitem(row, "name")?.clone());
    }
    let mut order: Vec<usize> = (0..principal.len()).collect();
    crate::pysort::sort(&mut order, |&a, &b| {
        super::rates::py_lt(&names[a], &names[b])
    })?;
    for &index in order.iter().take(15) {
        let row = &principal[index];
        // `"   %-12s %s" % (id, name[:80])`, printed as Python prints a str.
        let id = pyvalue::str_points(getitem(row, "id")?);
        let name = pyvalue::str_points(&head(getitem(row, "name")?, 80)?);
        let mut line: Vec<u32> = "   ".chars().map(u32::from).collect();
        line.extend(&id);
        line.extend(std::iter::repeat_n(
            u32::from(' '),
            12usize.saturating_sub(id.len()),
        ));
        line.push(u32::from(' '));
        line.extend(name);
        run.print_points(&line)?;
    }
    Ok(())
}

/// `python -m fadden discover`: the stage takes no arguments.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let mut http = http::connect();
    Ok(match self::run(http.as_mut(), &module_dir(), run) {
        Ok(()) => 0,
        Err(Stop::Raised(error)) => run.uncaught(&error),
        Err(Stop::Exit(message)) => {
            run.eprint(&message);
            1
        }
    })
}
