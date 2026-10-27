//! The port of `fadden/discover.py` (removed after the port): page every in-force title whose name holds a tax
//! keyword, across the Act, legislative instrument and notifiable instrument
//! collections, and write `titles_all.json` and `titles_principal.json` beside
//! the module. A failed or unordered page stops the stage rather than leave a
//! silently incomplete title list, and so does a list that has lost titles the
//! earlier `titles_all.json` held, unless `--accept-missing` records the loss.
use super::corpus_paths::{module_dir, require_builder_layout, script_dir_in, write_json_atomic};
use super::http::{self, Http};
use super::{Exc, Run, get, getitem, head};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::Json;

/// The share of earlier titles that may drop out without `--accept-missing`, as
/// a divisor: more than 1 in 20 (5%) refuses. Instruments sunset in batches, so
/// some loss is routine; the paging defect lost 142 of 813 (17%).
const MISSING_SHARE_DIVISOR: usize = 20;

/// The container level at which `json.dump` stops writing the title lists under
/// `python -m fadden` (CPython 3.15, base revision): a row member nested
/// 988 deep is written, 989 is refused (`tools/stage_depth_probe.py`).
const TITLES_DEPTH: usize = 990;
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

/// The ids and principal flags of the earlier `titles_all.json`, in its order:
/// `None` only when no directory entry exists (the first run); `Err` when an
/// entry exists but cannot be read as one, a dangling link included.
fn earlier_titles(path: &str) -> Result<Option<Vec<(Json, bool)>>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {}
    }
    let text = crate::pyio::read_text(path).map_err(|e| Exc::from(e).to_string())?;
    let Json::Array(rows) = crate::pyjson::loads_standard(&text).map_err(|e| match e {
        crate::pyjson::DecodeError::Invalid(message) => message,
        other => format!("{other:?}"),
    })?
    else {
        return Err("it is not a JSON list".into());
    };
    let mut titles = Vec::with_capacity(rows.len());
    for row in &rows {
        let id = getitem(row, "id").map_err(|e| e.to_string())?;
        set_key(id).map_err(|e| e.to_string())?;
        let principal = get(row, "isPrincipal")
            .map_err(|e| e.to_string())?
            .is_some_and(Json::truthy);
        titles.push((id.clone(), principal));
    }
    Ok(Some(titles))
}

/// Refuse to replace the title lists when titles the earlier list held have
/// dropped out: any principal title, or more than 1 in `MISSING_SHARE_DIVISOR`
/// of them. `accept` records the loss on stdout and lets the run continue.
fn check_missing(path: &str, rows: &[Json], accept: bool, run: &mut Run) -> Result<(), Stop> {
    const OVERRIDE: &str = "rerun with --accept-missing if the Register really dropped them";
    let earlier = match earlier_titles(path) {
        Ok(Some(earlier)) => earlier,
        Ok(None) => return Ok(()),
        Err(error) if accept => {
            run.print(&format!(
                "\n--accept-missing: replacing an unreadable earlier title list ({error})"
            ));
            return Ok(());
        }
        Err(error) => {
            return Err(Stop::Exit(format!(
                "cannot compare with the earlier title list ({error}); refusing to replace it; \
                 rerun with --accept-missing to replace it unchecked"
            )));
        }
    };
    let mut current = std::collections::HashSet::new();
    for row in rows {
        current.insert(set_key(getitem(row, "id")?)?);
    }
    let mut missing = Vec::new();
    let mut principal = 0;
    for (id, was_principal) in &earlier {
        if !current.contains(&set_key(id)?) {
            missing.push(crate::pyvalue::repr(id));
            principal += usize::from(*was_principal);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let summary = format!(
        "{} of {} earlier titles are missing ({principal} principal): {}",
        missing.len(),
        earlier.len(),
        missing.join(", ")
    );
    if accept {
        run.print(&format!("\n--accept-missing: {summary}"));
        return Ok(());
    }
    if principal > 0 || missing.len() * MISSING_SHARE_DIVISOR > earlier.len() {
        return Err(Stop::Exit(format!(
            "refusing to replace the title lists: {summary}; {OVERRIDE}"
        )));
    }
    run.print(&format!("\n{summary}"));
    Ok(())
}

/// Discover every title, with the module in `scratch`; `accept_missing` lets a
/// run replace a list it would otherwise refuse (see `check_missing`).
pub fn run(
    http: &mut dyn Http,
    scratch: &str,
    accept_missing: bool,
    run: &mut Run,
) -> Result<(), Stop> {
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

    // Compare before writing either file, so a refusal leaves both unchanged.
    check_missing(
        &pypath::join(scratch, "titles_all.json"),
        &all_rows,
        accept_missing,
        run,
    )?;

    // Stage then replace, so a crash part way through never truncates the
    // list versions reads as its authoritative input.
    let principal_json = Json::Array(principal.clone());
    write_json_atomic(
        &pypath::join(scratch, "titles_all.json"),
        &Json::Array(all_rows),
        Some(1),
        TITLES_DEPTH,
    )?;
    write_json_atomic(
        &pypath::join(scratch, "titles_principal.json"),
        &principal_json,
        Some(1),
        TITLES_DEPTH,
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

/// `python -m fadden discover [--accept-missing]`.
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let args = Parser::new("python -m fadden discover")
        .description("List every in-force title whose name holds a tax keyword.")
        .arg(Action::flag("--accept-missing").help(
            "replace the title lists even though titles the earlier titles_all.json held are missing",
        ))
        .parse(argv)?;
    let mut http = http::connect();
    Ok(
        match self::run(
            http.as_mut(),
            &module_dir(),
            args.flag("accept_missing"),
            run,
        ) {
            Ok(()) => 0,
            Err(Stop::Raised(error)) => run.uncaught(&error),
            Err(Stop::Exit(message)) => {
                run.eprint(&message);
                1
            }
        },
    )
}
