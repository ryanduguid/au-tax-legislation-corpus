//! `fadden/dist_verify.py`: verify the distribution before it goes anywhere.
//!
//! Checks the claims the distribution makes about itself rather than trusting
//! the build that made it: no personal names or unapproved contact details,
//! no image bytes, every row parses, every title listed in sources.json is
//! present, and nothing links to a title that was removed.
//!
//! The exit status is a three-state contract: 0 every check passed, 1 a check
//! failed and the distribution is not fit to publish, 2 the verifier could not
//! run, so nothing was checked. Anything unexpected lands in the 2 band and
//! says so; only the checks themselves produce a 1.
use std::collections::BTreeSet;

use super::corpus_paths::{
    child, corpus_root, module_dir, register_id, register_id_str, reject_symlinks, relative,
    require_builder_layout, script_dir,
};
use super::pii::{self, Approved};
use super::{Exc, Run, get, getitem, iterate, json_error};
use crate::argparse::Exit;
use crate::py;
use crate::pyio::Lines;
use crate::pyjson::{self, Json};
use crate::pyos;
use crate::pypath;
use crate::pyvalue::{self, py_eq, repr, str_points};

/// Accounting standards made as legislative instruments carry their own
/// copyright notices, which the Register's CC BY 4.0 grant does not cover
/// (each read at source on 27 September 2026).
pub const LICENCE_EXCLUDED: [&str; 5] = [
    "F2015L01601", // AASB 112 Income Taxes
    "F2021L00963", // AASB 2021-5
    "F2023L00949", // AASB 2023-2
    "F2023L01301", // AASB 2023-4
    "F2024L00075", // AASB 1056 Superannuation Entities
];

type P = Vec<u32>;

type PrivacyHook = Box<dyn FnMut(&str) -> Option<(bool, Vec<pii::Unapproved>)>>;

thread_local! {
    static PRIVACY_HOOK: std::cell::RefCell<Option<PrivacyHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook(title_file)` before each title file's privacy scan on this
/// thread; findings it returns replace the scan. For tests.
#[doc(hidden)]
pub fn set_privacy_hook(hook: impl FnMut(&str) -> Option<(bool, Vec<pii::Unapproved>)> + 'static) {
    PRIVACY_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

fn thousands(n: usize) -> String {
    super::rates::thousands(n)
}

/// `_entry_kind(entry, is_junction)`: what a top-level entry is, links not followed.
pub fn entry_kind(path: &str) -> &'static str {
    if pyos::islink(path) {
        return "symlink";
    }
    if pyos::isjunction(path) {
        return "junction";
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return "unreadable";
    };
    if pyos::is_reparse_point(&meta) {
        "reparse point"
    } else if meta.is_dir() {
        "directory"
    } else if meta.is_file() {
        "file"
    } else {
        "other"
    }
}

/// `_title_tree(directory)`: the contained regular files (as resolved paths)
/// and nested directories of a link-free title tree.
pub fn title_tree(directory: &str) -> Result<(Vec<String>, Vec<String>), Exc> {
    reject_symlinks(directory)?;
    let mut files = Vec::new();
    let mut nested = Vec::new();
    for step in pyos::walk(directory, |s| s.dirs.sort()) {
        let step = step?;
        for name in &step.dirs {
            nested.push(relative(&pypath::join(&step.top, name), directory));
        }
        let mut names = step.files.clone();
        names.sort();
        for name in names {
            let rel = relative(&pypath::join(&step.top, &name), directory);
            let contained = child(directory, &[&rel])?;
            if !std::fs::metadata(&contained).is_ok_and(|m| m.is_file()) {
                return Err(Exc::value("title tree contains a non-regular file"));
            }
            files.push(contained);
        }
    }
    Ok((files, nested))
}

/// `_expected_title_files(rid, title)`: the declared outputs, and whether the
/// manifest's paths name exactly those.
pub fn expected_title_files(rid: &str, title: &Json) -> Result<(BTreeSet<String>, bool), Exc> {
    let field = |name: &str| title.get(name).unwrap_or(&Json::Null);
    let mut expected: BTreeSet<String> =
        [String::from("sections.jsonl"), format!("{rid}.md")].into();
    let mut ok = py_eq(
        field("markdown"),
        &Json::str(&format!("markdown/{rid}/{rid}.md")),
    ) && py_eq(
        field("sections_jsonl"),
        &Json::str(&format!("markdown/{rid}/sections.jsonl")),
    );
    let endnotes = field("endnotes");
    if endnotes.truthy() {
        expected.insert("endnotes.md".into());
        ok = ok && py_eq(endnotes, &Json::str(&format!("markdown/{rid}/endnotes.md")));
    } else if ok {
        // `endnotes in {None, False}`: membership hashes the value first.
        if matches!(endnotes, Json::Array(_) | Json::Object(_)) {
            return Err(Exc::Type(format!(
                "cannot use '{0}' as a set element (unhashable type: '{0}')",
                pyvalue::type_name(endnotes)
            )));
        }
        ok = py_eq(endnotes, &Json::Null) || py_eq(endnotes, &Json::Bool(false));
    }
    Ok((expected, ok))
}

/// `collections.Counter` over string keys, in first-insertion order.
#[derive(Default)]
struct Tally(Vec<(P, usize)>);

impl Tally {
    fn add(&mut self, key: P, n: usize) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 += n,
            None => self.0.push((key, n)),
        }
    }

    fn get(&self, key: &[u32]) -> usize {
        self.0.iter().find(|(k, _)| k == key).map_or(0, |(_, n)| *n)
    }

    fn total(&self) -> usize {
        self.0.iter().map(|(_, n)| n).sum()
    }

    /// `str(dict(counter))`.
    fn repr(&self) -> String {
        let body: Vec<String> = self
            .0
            .iter()
            .map(|(k, n)| format!("{}: {n}", pyvalue::repr_points(k)))
            .collect();
        format!("{{{}}}", body.join(", "))
    }
}

fn head_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// `os.path.splitext(name)[1].lower()`.
fn extension(name: &str) -> String {
    let Some(dot) = name.rfind('.') else {
        return String::new();
    };
    if name[..dot].chars().all(|c| c == '.') {
        return String::new();
    }
    name[dot..].to_lowercase()
}

fn read_text(path: &str) -> Result<String, Exc> {
    Ok(crate::pyio::read_text(path)?)
}

/// `t.split()` word count over the text's code points.
fn words(points: &[u32]) -> usize {
    points
        .split(|&cp| char::from_u32(cp).is_some_and(py::is_space))
        .filter(|w| !w.is_empty())
        .count()
}

/// `str` or None: the value when it is a `str`.
fn as_points(value: &Json) -> Option<P> {
    match value {
        Json::Str(s) => Some(s.code_points()),
        _ => None,
    }
}

/// `a or b`.
fn or<'a>(a: &'a Json, b: &'a Json) -> &'a Json {
    if a.truthy() { a } else { b }
}

/// `verify_distribution(distribution, contact_allowlist)`: run every check on
/// one tree, printing each, and return the labels that failed.
pub fn verify_distribution(
    dist_root: &str,
    policy: &str,
    run: &mut Run,
) -> Result<Vec<String>, Exc> {
    let mut fails: Vec<String> = Vec::new();
    let mut check = |run: &mut Run, label: &str, ok: bool, detail: &str| {
        let mut line = format!("  {label:<52} {}", if ok { "PASS" } else { "FAIL" });
        if !detail.is_empty() {
            line.push_str("  ");
            line.push_str(detail);
        }
        run.print(&line);
        if !ok {
            fails.push(label.to_string());
        }
    };

    let approved: Vec<Approved> = pii::load_contact_allowlist(policy)?;
    let src = pyjson::loads_standard(&read_text(&child(dist_root, &["sources.json"])?)?)
        .map_err(json_error)?;
    let titles = iterate(getitem(&src, "titles")?)?;
    let mut by_id: Vec<(String, Json)> = Vec::new();
    for t in &titles {
        let rid = register_id(getitem(t, "register_id")?)?;
        match by_id.iter_mut().find(|(k, _)| *k == rid) {
            Some(slot) => slot.1 = t.clone(),
            None => by_id.push((rid, t.clone())),
        }
    }
    let empty = Json::Object(Vec::new());
    let title_of = |rid: &str| {
        by_id
            .iter()
            .find(|(k, _)| k == rid)
            .map_or(&empty, |(_, t)| t)
    };
    let listed: BTreeSet<String> = by_id.iter().map(|(k, _)| k.clone()).collect();
    let markdown_root = child(dist_root, &["markdown"])?;
    let listing_error =
        |e: std::io::Error| pyos::OsError::new(&e, Some(&markdown_root), pyos::Style::Native);
    let mut names = Vec::new();
    // An error part-way through the listing stops the verifier, as Python's
    // scandir iteration raises; the entries already seen are not a complete tree.
    for entry in pyos::scandir(&markdown_root).map_err(listing_error)? {
        names.push(pyos::entry_name(&entry.map_err(listing_error)?)?);
    }
    names.sort();
    let (mut present, mut stray) = (BTreeSet::new(), Vec::new());
    for name in names {
        let kind = entry_kind(&pypath::join(&markdown_root, &name));
        match register_id_str(&name) {
            Ok(rid) if kind == "directory" => {
                present.insert(rid);
            }
            _ => stray.push(format!("{name} [{kind}]")),
        }
    }
    let excluded = match get(&src, "excluded_titles")? {
        Some(value) => iterate(value)?,
        None => Vec::new(),
    };
    let mut removed = BTreeSet::new();
    for e in &excluded {
        removed.insert(register_id(getitem(e, "register_id")?)?);
    }

    check(
        run,
        "every listed title has a directory",
        listed.is_subset(&present),
        &format!("missing {}", listed.difference(&present).count()),
    );
    let mut unexpected: Vec<String> = present
        .difference(&listed)
        .map(|r| format!("{r} [directory]"))
        .collect();
    unexpected.extend(stray);
    let shown = if unexpected.is_empty() {
        String::new()
    } else {
        format!(
            ": {}",
            unexpected
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    check(
        run,
        "no directory beyond what is listed",
        unexpected.is_empty(),
        &format!("unexpected {}{shown}", unexpected.len()),
    );
    check(
        run,
        "no removed title present",
        removed.is_disjoint(&present),
        "",
    );
    let licensed: Vec<&str> = LICENCE_EXCLUDED
        .iter()
        .copied()
        .filter(|r| present.contains(*r))
        .collect();
    let mut licensed_sorted = licensed.clone();
    licensed_sorted.sort_unstable();
    check(
        run,
        "no title under a restrictive copyright notice",
        licensed.is_empty(),
        &licensed_sorted.join(", "),
    );

    let (mut rows, mut bad, mut section_rows, mut rid_mismatch) = (0usize, 0usize, 0usize, 0usize);
    let mut hot = Tally::default();
    let mut contact_hot: Vec<((String, &'static str, String), usize)> = Vec::new();
    let mut private_file_hot = Tally::default();
    let mut kind_counts = Tally::default();
    let (mut rows_by_coll, mut words_by_coll) = (Tally::default(), Tally::default());
    let mut unsafe_row_paths = BTreeSet::new();
    let mut unreadable_title_files = BTreeSet::new();
    let mut invalid_inventories = BTreeSet::new();
    let unknown = Json::str("unknown");
    for rid in &present {
        let title_directory = pypath::join(&markdown_root, rid);
        let tree = title_tree(&title_directory)
            .and_then(|tree| child(&markdown_root, &[rid, "sections.jsonl"]).map(|p| (tree, p)));
        let ((title_files, nested), p) = match tree {
            Ok(found) => found,
            Err(Exc::Os(_) | Exc::Value { .. }) => {
                unsafe_row_paths.insert(rid.clone());
                continue;
            }
            Err(other) => return Err(other),
        };
        let relative_files: BTreeSet<String> = title_files
            .iter()
            .map(|f| relative(f, &title_directory))
            .collect();
        let (expected_files, metadata_ok) = expected_title_files(rid, title_of(rid))?;
        if !nested.is_empty() || relative_files != expected_files || !metadata_ok {
            invalid_inventories.insert(rid.clone());
        }
        let mut unreadable_paths = BTreeSet::new();
        for title_file in &title_files {
            let injected =
                PRIVACY_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(title_file)));
            let scanned = match injected {
                Some(found) => Ok(found),
                None => pii::privacy_findings_in_file(title_file, rid, &approved),
            };
            match scanned {
                Err(error) if pii::unreadable(&error) => {
                    unreadable_title_files.insert(rid.clone());
                    unreadable_paths.insert(pypath::normcase(title_file));
                }
                Err(error) => return Err(error),
                Ok((pair, contacts)) => {
                    if pair {
                        private_file_hot.add(pts(rid), 1);
                    }
                    for (kind, digest, _) in contacts {
                        let key = (rid.clone(), kind, digest[..16].to_string());
                        match contact_hot.iter_mut().find(|(k, _)| *k == key) {
                            Some(slot) => slot.1 += 1,
                            None => contact_hot.push((key, 1)),
                        }
                    }
                }
            }
        }
        if !std::fs::metadata(&p).is_ok_and(|m| m.is_file())
            || unreadable_paths.contains(&pypath::normcase(&p))
        {
            continue;
        }
        let read = (|| -> Result<(), Exc> {
            let mut lines = Lines::open(&p)?;
            while let Some(line) = lines.next_line() {
                let line = line?;
                if py::strip(&line).is_empty() {
                    continue;
                }
                rows += 1;
                // Only TypeError and ValueError count as a malformed row.
                let loaded = pyjson::loads_standard(&line);
                if let Err(error @ pyjson::DecodeError::Recursion(_)) = loaded {
                    return Err(json_error(error));
                }
                let parsed = (|| -> Option<(Json, P, P, P)> {
                    let r = loaded.ok()?;
                    if !matches!(r, Json::Object(_)) {
                        return None;
                    }
                    let field = |name: &str| r.get(name).unwrap_or(&Json::Null);
                    let text = if field("text").truthy() {
                        as_points(field("text"))?
                    } else {
                        Vec::new()
                    };
                    let title_collection = title_of(rid).get("collection").unwrap_or(&Json::Null);
                    let coll = as_points(or(or(field("collection"), title_collection), &unknown))?;
                    let kind = if field("kind").truthy() {
                        as_points(field("kind"))?
                    } else if field("section").truthy() {
                        pts("section")
                    } else {
                        pts("unnumbered")
                    };
                    Some((r.clone(), text, coll, kind))
                })();
                let Some((r, text, coll, kind)) = parsed else {
                    bad += 1;
                    continue;
                };
                if !py_eq(r.get("register_id").unwrap_or(&Json::Null), &Json::str(rid)) {
                    rid_mismatch += 1;
                }
                rows_by_coll.add(coll.clone(), 1);
                words_by_coll.add(coll, words(&text));
                kind_counts.add(kind, 1);
                if r.get("section").is_some_and(Json::truthy) {
                    section_rows += 1;
                }
                if pii::has_private_person_registration_pair(&pyvalue::points_lossy(&text)) {
                    hot.add(pts(rid), 1);
                }
            }
            Ok(())
        })();
        match read {
            Ok(()) => {}
            Err(error) if pii::unreadable(&error) => {
                unreadable_title_files.insert(rid.clone());
            }
            Err(error) => return Err(error),
        }
    }
    let first_five =
        |set: &BTreeSet<String>| set.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    check(
        run,
        "every JSONL row parses",
        bad == 0,
        &format!("{} rows, {bad} malformed", thousands(rows)),
    );
    check(
        run,
        "each JSONL row matches its title directory",
        rid_mismatch == 0,
        &format!("{rid_mismatch} mismatched register ids"),
    );
    check(
        run,
        "title files stay inside real link-free directories",
        unsafe_row_paths.is_empty(),
        &first_five(&unsafe_row_paths),
    );
    check(
        run,
        "title file inventory matches declared outputs",
        invalid_inventories.is_empty(),
        &first_five(&invalid_inventories),
    );
    check(
        run,
        "all distributed title files are validated UTF-8 text",
        unreadable_title_files.is_empty(),
        &first_five(&unreadable_title_files),
    );
    check(
        run,
        "no title file names private individuals",
        private_file_hot.0.is_empty(),
        &head_chars(&private_file_hot.repr(), 60),
    );
    check(
        run,
        "no row names private individuals",
        hot.0.is_empty(),
        &head_chars(&hot.repr(), 60),
    );
    // The order of these findings is the binary's (sorted per title); CPython
    // iterated a set, whose order follows its string hashes.
    let contacts_shown: Vec<String> = contact_hot
        .iter()
        .map(|((rid, kind, digest), n)| {
            format!(
                "({}, {}, {}): {n}",
                py::repr_str(rid),
                py::repr_str(kind),
                py::repr_str(digest)
            )
        })
        .collect();
    check(
        run,
        "no unapproved contact identifiers",
        contact_hot.is_empty(),
        &head_chars(&format!("{{{}}}", contacts_shown.join(", ")), 120),
    );

    let counts_value = get(&src, "counts")?.cloned().unwrap_or(Json::Null);
    let counts = if counts_value.truthy() {
        counts_value
    } else {
        Json::Object(Vec::new())
    };
    let count = |name: &str| -> Result<Json, Exc> {
        Ok(get(&counts, name)?.cloned().unwrap_or(Json::Null))
    };
    let collection_of =
        |t: &Json| -> Json { or(t.get("collection").unwrap_or(&Json::Null), &unknown).clone() };
    let mut collections: Vec<Json> = Vec::new();
    for t in &titles {
        let c = collection_of(t);
        if matches!(c, Json::Array(_) | Json::Object(_)) {
            return Err(Exc::Type(format!(
                "cannot use '{0}' as a set element (unhashable type: '{0}')",
                pyvalue::type_name(&c)
            )));
        }
        if !collections.iter().any(|x| pyvalue::item_eq(x, &c)) {
            collections.push(c);
        }
    }
    sort_values(&mut collections)?;
    let mut expected_by_coll: Vec<(Json, Json)> = Vec::new();
    for coll in &collections {
        let n_titles = titles
            .iter()
            .filter(|t| py_eq(&collection_of(t), coll))
            .count();
        let key = as_points(coll);
        let (n_rows, n_words) = key
            .as_ref()
            .map_or((0, 0), |k| (rows_by_coll.get(k), words_by_coll.get(k)));
        expected_by_coll.push((
            coll.clone(),
            Json::obj([
                ("titles", Json::Int(n_titles.into())),
                ("rows", Json::Int(n_rows.into())),
                ("words", Json::Int(n_words.into())),
            ]),
        ));
    }
    let int = |n: usize| Json::Int(n.into());
    let titles_count = titles.len();
    check(
        run,
        "sources count: titles",
        py_eq(&count("titles")?, &int(titles_count)),
        &format!("{} vs {titles_count}", repr(&count("titles")?)),
    );
    let act = Json::str("Act");
    let acts = titles
        .iter()
        .filter(|t| py_eq(t.get("collection").unwrap_or(&Json::Null), &act))
        .count();
    let instruments = titles_count - acts;
    check(
        run,
        "sources count: acts and instruments",
        py_eq(&count("acts")?, &int(acts)) && py_eq(&count("instruments")?, &int(instruments)),
        &format!(
            "{}/{} vs {acts}/{instruments}",
            repr(&count("acts")?),
            repr(&count("instruments")?)
        ),
    );
    check(
        run,
        "sources count: rows",
        py_eq(&count("jsonl_rows")?, &int(rows)),
        &format!("{} vs {rows}", repr(&count("jsonl_rows")?)),
    );
    let total_words = words_by_coll.total();
    check(
        run,
        "sources count: words",
        py_eq(&count("words_body_only")?, &int(total_words)),
        &format!("{} vs {total_words}", repr(&count("words_body_only")?)),
    );
    check(
        run,
        "sources count: section rows",
        py_eq(&count("rows_with_section_id")?, &int(section_rows)),
        &format!(
            "{} vs {section_rows}",
            repr(&count("rows_with_section_id")?)
        ),
    );
    check(
        run,
        "sources count: tracked row kinds",
        py_eq(
            &count("rows_container")?,
            &int(kind_counts.get(&pts("container"))),
        ) && py_eq(
            &count("rows_unnumbered")?,
            &int(kind_counts.get(&pts("unnumbered"))),
        ) && py_eq(
            &count("rows_introductory")?,
            &int(kind_counts.get(&pts("introductory"))),
        ),
        &kind_counts.repr(),
    );
    let by_collection = count("by_collection")?;
    check(
        run,
        "sources collection counts",
        same_dict(&by_collection, &expected_by_coll),
        &pyvalue::points_lossy(
            &str_points(&by_collection)
                .into_iter()
                .take(80)
                .collect::<Vec<_>>(),
        ),
    );
    let epub_bytes = count("epub_bytes")?;
    check(
        run,
        "sources declares no distributed EPUB bytes",
        py_eq(&epub_bytes, &int(0)),
        &pyvalue::points_lossy(&str_points(&epub_bytes)),
    );
    check(
        run,
        "title metadata does not point to excluded EPUBs",
        titles.iter().all(|t| {
            t.get("epub").is_none_or(|v| matches!(v, Json::Null))
                && matches!(t.get("epub_included"), Some(Json::Bool(false)))
        }),
        "",
    );

    let walked = pyos::walk(dist_root, |_| {}).collect::<Result<Vec<_>, _>>()?;
    let mut exts = Tally::default();
    let mut size: u64 = 0;
    for step in &walked {
        for name in &step.files {
            exts.add(pts(&extension(name)), 1);
        }
    }
    let images = [".png", ".jpg", ".jpeg", ".gif", ".svg", ".epub"];
    check(
        run,
        "no image files",
        !images.iter().any(|e| exts.get(&pts(e)) > 0),
        &exts.repr(),
    );

    let idx = read_text(&child(dist_root, &["INDEX.md"])?)?;
    check(
        run,
        "INDEX links no removed title",
        !removed.iter().any(|r| idx.contains(r.as_str())),
        "",
    );
    let headline = format!(
        "{} titles ({acts} Acts, {instruments} instruments), {} retrieval rows, {} words.",
        thousands(titles_count),
        thousands(rows),
        thousands(total_words)
    );
    check(
        run,
        "INDEX headline matches generated counts",
        idx.contains(&headline),
        "",
    );
    let rd = read_text(&child(dist_root, &["README.md"])?)?;
    check(
        run,
        "README states the real title count",
        rd.contains(&format!("{titles_count} in-force principal")),
        "",
    );
    check(
        run,
        "README headline matches generated counts",
        rd.contains(&format!(
            "{acts} Acts and {instruments} legislative and notifiable"
        )) && rd.contains(&format!(
            "{} retrieval rows, {} words.",
            thousands(rows),
            thousands(total_words)
        )),
        "",
    );
    check(
        run,
        "README does not promise EPUB files",
        !rd.contains("epub/<register_id>.epub"),
        "",
    );
    check(
        run,
        "README does not claim rows naming people",
        !rd.contains("titles name people") && !rd.contains("disciplined agents"),
        "",
    );
    let removed_md = read_text(&child(dist_root, &["REMOVED.md"])?)?;
    check(
        run,
        "REMOVED.md lists every exclusion",
        removed.iter().all(|r| removed_md.contains(r.as_str())),
        "",
    );

    let mut rate_ids: Vec<P> = Vec::new();
    let mut rates_bad = 0usize;
    let mut lines = Lines::open(&child(dist_root, &["rates", "rates.jsonl"])?)?;
    while let Some(line) = lines.next_line() {
        let line = line?;
        if py::strip(&line).is_empty() {
            continue;
        }
        match pyjson::loads_standard(&line) {
            Ok(record @ Json::Object(_))
                if matches!(record.get("register_id"), Some(Json::Str(_))) =>
            {
                rate_ids.push(as_points(record.at("register_id")).unwrap_or_default());
            }
            Err(error @ pyjson::DecodeError::Recursion(_)) => return Err(json_error(error)),
            _ => rates_bad += 1,
        }
    }
    check(
        run,
        "every rates JSONL row parses",
        rates_bad == 0,
        &format!("{rates_bad} malformed"),
    );
    let in_set = |set: &BTreeSet<String>, id: &[u32]| set.iter().any(|r| pts(r) == id);
    check(
        run,
        "no rates entry cites a removed title",
        !rate_ids.iter().any(|id| in_set(&removed, id)),
        "",
    );
    check(
        run,
        "every rates entry cites a present title",
        rate_ids.iter().all(|id| in_set(&present, id)),
        "",
    );
    let rates_md = read_text(&child(dist_root, &["rates", "RATES.md"])?)?;
    let distinct: BTreeSet<&P> = rate_ids.iter().filter(|id| !id.is_empty()).collect();
    let rate_headline = format!(
        "{} entries across {} titles.",
        thousands(rate_ids.len()),
        thousands(distinct.len())
    );
    check(
        run,
        "RATES headline matches filtered JSONL",
        rates_md.contains(&rate_headline),
        "",
    );
    let mut named = false;
    for e in &excluded {
        let name = e.get("name").unwrap_or(&Json::Null);
        if !name.truthy() {
            continue;
        }
        let Json::Str(text) = name else {
            return Err(Exc::Type(format!(
                "'in <string>' requires string as left operand, not {}",
                pyvalue::type_name(name)
            )));
        };
        if text.as_str().is_some_and(|t| rates_md.contains(t)) {
            named = true;
            break;
        }
    }
    check(run, "RATES contains no removed-title name", !named, "");

    for step in &walked {
        for name in &step.files {
            let path = pypath::join(&step.top, name);
            let meta = std::fs::metadata(&path)
                .map_err(|e| pyos::OsError::new(&e, Some(&path), pyos::Style::Native))?;
            size += meta.len();
        }
    }
    run.print(&format!(
        "\n{} titles | {} rows | {:.1} MB | {} removed",
        thousands(titles_count),
        thousands(rows),
        size as f64 / 1e6,
        removed.len()
    ));
    if fails.is_empty() {
        run.print("RESULT: all checks passed");
    } else {
        let shown: Vec<String> = fails.iter().map(|f| py::repr_str(f)).collect();
        run.print(&format!("RESULT: FAILED: [{}]", shown.join(", ")));
    }
    Ok(fails)
}

/// `sorted(values)` for hashable JSON scalars, or the TypeError Python raises
/// for values that do not order.
fn sort_values(values: &mut [Json]) -> Result<(), Exc> {
    let all_str = values.iter().all(|v| matches!(v, Json::Str(_)));
    if all_str {
        values.sort_by_key(|v| as_points(v).unwrap_or_default());
        return Ok(());
    }
    let numeric = |v: &Json| matches!(v, Json::Bool(_) | Json::Int(_) | Json::Float(_));
    if values.iter().all(numeric) {
        values.sort_by(|a, b| {
            let f = |v: &Json| match v {
                Json::Bool(b) => f64::from(u8::from(*b)),
                Json::Int(i) => i.to_string().parse().unwrap_or(f64::NAN),
                Json::Float(f) => *f,
                _ => f64::NAN,
            };
            f(a).partial_cmp(&f(b)).unwrap_or(std::cmp::Ordering::Equal)
        });
        return Ok(());
    }
    if values.len() < 2 {
        return Ok(());
    }
    // Mixed types: which pair Python's sort meets first follows the set's
    // hash order, so the names in this message can come in the other order.
    let a = values
        .iter()
        .find(|v| !matches!(v, Json::Str(_)))
        .unwrap_or(&values[0]);
    Err(Exc::Type(format!(
        "'<' not supported between instances of 'str' and '{}'",
        pyvalue::type_name(a)
    )))
}

/// `value == expected` for a dict built with JSON-value keys.
fn same_dict(value: &Json, expected: &[(Json, Json)]) -> bool {
    let Json::Object(pairs) = value else {
        return false;
    };
    pairs.len() == expected.len()
        && expected.iter().all(|(k, v)| {
            let Json::Str(key) = k else {
                return false;
            };
            pairs
                .iter()
                .any(|(pk, pv)| pk.code_points() == key.code_points() && pyvalue::item_eq(pv, v))
        })
}

/// `python -m fadden dist_verify`: 0 clean, 1 a check failed, 2 not checked.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let setup = (|| -> Result<(String, String), Exc> {
        let directory = script_dir("dist_verify.py")?;
        let dist = child(&corpus_root(&directory), &["dist"])?;
        require_builder_layout(&directory)?;
        Ok((
            dist,
            pypath::join(&module_dir(), "pii_contact_allowlist.json"),
        ))
    })();
    let (dist, policy) = match setup {
        Ok(found) => found,
        Err(error) => return Ok(run.uncaught(&error)),
    };
    Ok(match verify_distribution(&dist, &policy, run) {
        Ok(fails) => i32::from(!fails.is_empty()),
        Err(error) => {
            // CPython printed the whole traceback here; the binary prints its last line.
            run.eprint(&format!("{}: {error}", error.class()));
            run.print("RESULT: VERIFIER ERROR: the checks did not run, so this distribution is unverified, not clean");
            2
        }
    })
}
