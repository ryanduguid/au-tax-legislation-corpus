//! `fadden/dist.py`: build, verify and publish the redistributable subset of
//! the corpus.
//!
//! Three things in the full corpus do not travel: the EPUBs (they embed the
//! Commonwealth Coat of Arms, outside the Register's CC BY 4.0 grant), the
//! titles the first PII scan flagged (registers naming private individuals)
//! and the accounting standards under their own copyright notices. Every
//! title kept is re-scanned for private-person pairs and unapproved contact
//! details, before copying and again on the copy; REMOVED.md lists what was
//! dropped. The subset is built in a private sibling of `dist/`, verified with
//! dist_verify, and only then promoted, the old tree moved aside first and
//! restored if the promotion fails.
use std::cell::RefCell;

use super::corpus_paths::{
    child, corpus_root, linked, markdown_text, module_dir, register_id, reject_symlinks, relative,
    require_builder_layout, script_dir_in, write_text,
};
use super::dist_verify::{self, LICENCE_EXCLUDED, expected_title_files, verify_distribution};
use super::pii::{self, Approved, Unapproved};
use super::{Exc, Run, get, getitem, iterate, json_error};
use crate::argparse::Exit;
use crate::py;
use crate::pyio::Lines;
use crate::pyjson::{self, Json, PyStr};
use crate::pyos::{self, OsError, Style};
use crate::pypath;
use crate::pyvalue::{self, Key, Stop, dumps_line, str_points, type_name};

type P = Vec<u32>;

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

fn thousands(n: usize) -> String {
    super::rates::thousands(n)
}

fn exists(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// `os.path.realpath(path)`, as the managed-sibling check resolves parents.
fn realpath(path: &str) -> Result<String, Exc> {
    let injected = REALPATH_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(path)));
    match injected {
        Some(resolved) => Ok(resolved),
        None => Ok(pypath::realpath(path)?),
    }
}

/// `os.path.abspath(path)`.
fn abspath(path: &str) -> String {
    pypath::abspath(path)
}

// ----------------------------------------------------------- test hooks

type RenameHook = Box<dyn FnMut(&str, &str) -> Option<OsError>>;
type RealpathHook = Box<dyn FnMut(&str) -> Option<String>>;
type PreflightHook = Box<dyn FnMut()>;

thread_local! {
    static RENAME_HOOK: RefCell<Option<RenameHook>> = const { RefCell::new(None) };
    static REALPATH_HOOK: RefCell<Option<RealpathHook>> = const { RefCell::new(None) };
    static PREFLIGHT_HOOK: RefCell<Option<PreflightHook>> = const { RefCell::new(None) };
}

/// Run `hook(source, destination)` before every rename on this thread; an
/// error it returns fails that rename. For tests.
#[doc(hidden)]
pub fn set_rename_hook(hook: impl FnMut(&str, &str) -> Option<OsError> + 'static) {
    RENAME_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

#[doc(hidden)]
pub use super::corpus_paths::set_probe_hook;

/// Run `hook(path)` before every managed-sibling parent lookup on this
/// thread; a path it returns replaces the resolved one. For tests.
#[doc(hidden)]
pub fn set_realpath_hook(hook: impl FnMut(&str) -> Option<String> + 'static) {
    REALPATH_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// Run `hook()` after the source privacy gate passes and before any title is
/// copied, on this thread. For tests.
#[doc(hidden)]
pub fn set_preflight_hook(hook: impl FnMut() + 'static) {
    PREFLIGHT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// `os.rename(src, dst)`, which never replaces an existing directory here.
fn rename(src: &str, dst: &str) -> Result<(), Exc> {
    let injected = RENAME_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(src, dst)));
    if let Some(error) = injected {
        return Err(Exc::Os(error));
    }
    #[cfg(windows)]
    {
        pyos::rename_new(src, dst)?;
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(src, dst).map_err(|e| OsError::two(&e, src, dst, Style::Native))?;
    }
    Ok(())
}

// ------------------------------------------------- managed sibling paths

/// `_validated_distribution_target(path)`: an absolute, non-link directory
/// target with a real parent.
pub fn validated_distribution_target(path: &str) -> Result<String, Exc> {
    let target = abspath(path);
    let (parent, name) = pypath::split(&target);
    if name.is_empty() || target == parent {
        return Err(Exc::value(
            "distribution target must be a named child directory",
        ));
    }
    if !pyos::isdir(&parent) {
        return Err(Exc::value("distribution target parent does not exist"));
    }
    if exists(&target) {
        if linked(&target)? {
            return Err(Exc::value(
                "distribution target must not be a link or reparse point",
            ));
        }
        if !pyos::isdir(&target) {
            return Err(Exc::value("distribution target must be a directory"));
        }
    }
    Ok(target)
}

/// `_validated_managed_sibling(target, candidate, kind)`: one private stage or
/// backup path beside `target`, the only computed paths the publisher removes.
pub fn validated_managed_sibling(target: &str, candidate: &str, kind: &str) -> Result<String, Exc> {
    let target = validated_distribution_target(target)?;
    if kind != "stage" && kind != "backup" {
        return Err(Exc::value("unknown managed distribution path kind"));
    }
    let candidate = abspath(candidate);
    let (parent, target_name) = pypath::split(&target);
    let (candidate_parent, candidate_name) = pypath::split(&candidate);
    let prefix = format!(".{target_name}.{kind}-");
    let suffix = candidate_name.strip_prefix(&prefix).unwrap_or("");
    let hex = suffix.len() == 32
        && suffix
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if pypath::normcase(&candidate_parent) != pypath::normcase(&parent) || !hex {
        return Err(Exc::value(
            "managed distribution path is not a validated sibling",
        ));
    }
    let real = |p: &str| -> Result<String, Exc> { Ok(pypath::normcase(&realpath(p)?)) };
    if real(&candidate_parent)? != real(&parent)? {
        return Err(Exc::value(
            "managed distribution path resolves outside its parent",
        ));
    }
    Ok(candidate)
}

/// `_new_managed_sibling(target, kind)`: a unique, absent stage or backup path.
pub fn new_managed_sibling(
    target: &str,
    kind: &str,
    mut hex: impl FnMut() -> String,
) -> Result<String, Exc> {
    let target = validated_distribution_target(target)?;
    let (parent, target_name) = pypath::split(&target);
    for _ in 0..100 {
        let candidate = pypath::join(&parent, &format!(".{target_name}.{kind}-{}", hex()));
        let candidate = validated_managed_sibling(&target, &candidate, kind)?;
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Exc::Runtime(format!(
        "could not allocate a unique distribution {kind} path"
    )))
}

/// `_remove_managed_tree(target, candidate, kind)`: remove only a validated
/// stage or backup directory, never a link or file.
pub fn remove_managed_tree(target: &str, candidate: &str, kind: &str) -> Result<(), Exc> {
    let candidate = validated_managed_sibling(target, candidate, kind)?;
    if !exists(&candidate) {
        return Ok(());
    }
    if linked(&candidate)? || !pyos::isdir(&candidate) {
        return Err(Exc::value(
            "refusing to remove a non-directory managed path",
        ));
    }
    reject_symlinks(&candidate)?;
    std::fs::remove_dir_all(&candidate)
        .map_err(|e| OsError::new(&e, Some(&candidate), Style::Native))?;
    Ok(())
}

/// `_promote_distribution(staging, target)`: move the old tree aside, move the
/// stage into place, and restore the old tree if that second rename fails.
pub fn promote_distribution(staging: &str, target: &str) -> Result<(), Exc> {
    let target = validated_distribution_target(target)?;
    let staging = validated_managed_sibling(&target, staging, "stage")?;
    if !pyos::isdir(&staging) || linked(&staging)? {
        return Err(Exc::value("distribution stage must be a real directory"));
    }
    let mut backup = None;
    if exists(&target) {
        let path = new_managed_sibling(&target, "backup", || pyos::random_hex(32))?;
        rename(&target, &path)?;
        backup = Some(path);
    }
    if let Err(error) = rename(&staging, &target) {
        if let Some(path) = backup {
            if exists(&target) {
                return Err(Exc::Runtime(
                    "distribution promotion failed after creating its target; automatic rollback is unsafe".into(),
                ));
            }
            let path = validated_managed_sibling(&target, &path, "backup")?;
            if !pyos::isdir(&path) || linked(&path)? {
                return Err(Exc::Runtime(
                    "distribution backup is unsafe to restore".into(),
                ));
            }
            rename(&path, &target)?;
        }
        return Err(error);
    }
    if let Some(path) = backup {
        remove_managed_tree(&target, &path, "backup")?;
    }
    Ok(())
}

// ------------------------------------------------------ privacy gates

/// `_title_tree(directory)`: the verifier's walk, with a non-regular file
/// raised as the RuntimeError the other publication gates raise.
pub fn title_tree(directory: &str) -> Result<(Vec<String>, Vec<String>), Exc> {
    reject_symlinks(directory)?;
    dist_verify::title_tree(directory).map_err(|error| match error {
        Exc::Value { message, .. } => Exc::Runtime(message),
        other => other,
    })
}

/// `_title_privacy_findings(directory, rid, title, approved)`: the number of
/// files pairing a name with a registration number, and the unapproved
/// contacts, from exactly the declared title outputs.
pub fn title_privacy_findings(
    directory: &str,
    rid: &str,
    title: &Json,
    approved: &[Approved],
) -> Result<(usize, Vec<Unapproved>), Exc> {
    let unreadable = || {
        Exc::Runtime(format!(
            "listed title {rid} contains an unreadable or non-text file"
        ))
    };
    let (files, nested) = match title_tree(directory) {
        Ok(tree) => tree,
        Err(error) if pii::unreadable(&error) => return Err(unreadable()),
        Err(error) => return Err(error),
    };
    let relative_files: std::collections::BTreeSet<String> =
        files.iter().map(|f| relative(f, directory)).collect();
    let (expected, metadata_ok) = expected_title_files(rid, title)?;
    if !nested.is_empty() || relative_files != expected || !metadata_ok {
        return Err(Exc::Runtime(format!(
            "listed title {rid} has an unexpected file inventory"
        )));
    }
    let (mut private_files, mut matches) = (0, Vec::new());
    for path in &files {
        match pii::privacy_findings_in_file(path, rid, approved) {
            Ok((pair, found)) => {
                private_files += usize::from(pair);
                matches.extend(found);
            }
            Err(error) if pii::unreadable(&error) => return Err(unreadable()),
            Err(error) => return Err(error),
        }
    }
    matches.sort();
    matches.dedup();
    Ok((private_files, matches))
}

/// `reject_unapproved_contacts(titles, approved)`: fail before anything is
/// copied on any private-person pair or unapproved contact. Diagnostics carry
/// only the Register id and a truncated digest.
pub fn reject_unapproved_contacts(
    root: &str,
    titles: &[Json],
    approved: &[Approved],
) -> Result<(), Exc> {
    let (mut total, mut examples, mut private_titles) = (0usize, Vec::new(), Vec::new());
    for title in titles {
        let rid = register_id(getitem(title, "register_id")?)?;
        let directory = child(root, &["markdown", &rid])?;
        if !pyos::isdir(&directory) {
            return Err(Exc::Runtime(format!(
                "listed title {rid} has no markdown directory"
            )));
        }
        let sections = child(&directory, &["sections.jsonl"])?;
        if !std::fs::metadata(&sections).is_ok_and(|m| m.is_file()) {
            return Err(Exc::Runtime(format!(
                "listed title {rid} has no sections.jsonl"
            )));
        }
        let (private_files, unexpected) =
            title_privacy_findings(&directory, &rid, title, approved)?;
        if private_files > 0 {
            private_titles.push(rid.clone());
        }
        for (kind, digest, _) in unexpected {
            total += 1;
            if examples.len() < 8 {
                examples.push(format!("{rid}:{kind}:{}", &digest[..16]));
            }
        }
    }
    if !private_titles.is_empty() {
        return Err(Exc::Runtime(format!(
            "private-person registration pairs in redistributed title files: {} ({})",
            private_titles.len(),
            private_titles
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if total > 0 {
        return Err(Exc::Runtime(format!(
            "unapproved contact identifiers: {total} ({})",
            examples.join(", ")
        )));
    }
    Ok(())
}

// ------------------------------------------------------------ rendering

/// `str(value)` for `%s`.
fn s(value: &Json) -> P {
    str_points(value)
}

/// `value or default`.
fn or<'a>(value: &'a Json, default: &'a Json) -> &'a Json {
    if value.truthy() { value } else { default }
}

fn field<'a>(record: &'a Json, name: &str) -> &'a Json {
    record.get(name).unwrap_or(&Json::Null)
}

/// `collections.Counter` keyed by hashable JSON values, first-insertion order.
#[derive(Default)]
struct Counter(Vec<(Key, Json, usize)>);

impl Counter {
    fn add(&mut self, value: &Json, n: usize) -> Result<(), Exc> {
        let key = pyvalue::key(value).map_err(|m| {
            Exc::Type(format!(
                "cannot use '{}' as a dict key ({m})",
                type_name(value)
            ))
        })?;
        match self.0.iter_mut().find(|(k, _, _)| *k == key) {
            Some(slot) => slot.2 += n,
            None => self.0.push((key, value.clone(), n)),
        }
        Ok(())
    }

    /// One element of `Counter(iterable)`: its C counter words an unhashable
    /// value without the "cannot use ... as a dict key" prefix.
    fn tally(&mut self, value: &Json) -> Result<(), Exc> {
        let key = pyvalue::key(value).map_err(Exc::Type)?;
        match self.0.iter_mut().find(|(k, _, _)| *k == key) {
            Some(slot) => slot.2 += 1,
            None => self.0.push((key, value.clone(), 1)),
        }
        Ok(())
    }

    fn get(&self, value: &Json) -> usize {
        pyvalue::key(value)
            .ok()
            .and_then(|key| self.0.iter().find(|(k, _, _)| *k == key))
            .map_or(0, |(_, _, n)| *n)
    }

    fn most_common(&self) -> Vec<(&Json, usize)> {
        let mut items: Vec<(&Json, usize)> = self.0.iter().map(|(_, v, n)| (v, *n)).collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.1));
        items
    }
}

/// `sorted(keys)` for the distinct keys of a dict, in insertion order, or the
/// TypeError CPython's sort meets first.
fn sorted_keys(values: Vec<Json>) -> Result<Vec<Json>, Exc> {
    let mut values = values;
    crate::pysort::sort(&mut values, super::rates::py_lt)?;
    Ok(values)
}

/// `stale_source_lines(records)`: the warning for rows from a stale compilation.
fn stale_source_lines(records: &[Json]) -> Result<Vec<P>, Exc> {
    let mut stale: Vec<(Key, Json, &Json)> = Vec::new();
    let unknown = Json::str("unknown");
    for r in records {
        if matches!(r.get("version_is_current"), Some(Json::Bool(false))) {
            let id = or(field(r, "register_id"), &unknown).clone();
            let key = pyvalue::key(&id).map_err(|m| {
                Exc::Type(format!(
                    "cannot use '{}' as a dict key ({m})",
                    type_name(&id)
                ))
            })?;
            if !stale.iter().any(|(k, _, _)| *k == key) {
                stale.push((key, id, r));
            }
        }
    }
    if stale.is_empty() {
        return Ok(Vec::new());
    }
    let ids = sorted_keys(stale.iter().map(|(_, id, _)| id.clone()).collect())?;
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
    for id in ids {
        let key = pyvalue::key(&id).map_err(Exc::Type)?;
        let (_, _, r) = stale
            .iter()
            .find(|(k, _, _)| *k == key)
            .expect("listed above");
        let mut line = pts("- ");
        line.extend(markdown_text(&s(or(field(r, "act"), &dash))));
        line.extend(pts(" ("));
        line.extend(markdown_text(&s(&id)));
        line.extend(pts("): compilation "));
        line.extend(markdown_text(&s(or(field(r, "compilation_number"), &dash))));
        line.extend(pts(" of "));
        line.extend(markdown_text(&s(or(field(r, "compilation_date"), &dash))));
        line.extend(pts(", `version_is_current: false`"));
        lines.push(line);
    }
    lines.push(Vec::new());
    Ok(lines)
}

/// `str.replace(old, new)` on code points.
fn replace(text: &[u32], old: char, new: &str) -> P {
    let mut out = Vec::with_capacity(text.len());
    for &cp in text {
        if cp == u32::from(old) {
            out.extend(pts(new));
        } else {
            out.push(cp);
        }
    }
    out
}

/// `value.replace(...)` on a field that must be a `str`.
fn str_field(value: &Json, method: &str) -> Result<P, Exc> {
    match value {
        Json::Str(text) => Ok(text.code_points()),
        other => Err(Exc::Attribute(format!(
            "'{}' object has no attribute '{method}'",
            type_name(other)
        ))),
    }
}

/// `", ".join(value)`.
fn join_strs(value: &Json) -> Result<P, Exc> {
    let items: Vec<Json> = match value {
        Json::Array(items) => items.clone(),
        Json::Object(pairs) => pairs.iter().map(|(k, _)| Json::Str(k.clone())).collect(),
        Json::Str(text) => text
            .code_points()
            .into_iter()
            .map(|cp| Json::Str(PyStr::from_code_points(vec![cp])))
            .collect(),
        _ => return Err(Exc::Type("can only join an iterable".into())),
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Json::Str(text) = item else {
            return Err(Exc::Type(format!(
                "sequence item {i}: expected str instance, {} found",
                type_name(item)
            )));
        };
        if i > 0 {
            out.extend(pts(", "));
        }
        out.extend(text.code_points());
    }
    Ok(out)
}

fn take(points: P, n: usize) -> P {
    points.into_iter().take(n).collect()
}

/// `render_rates_markdown(records)`: the readable rate index, from exactly the
/// records being distributed.
pub fn render_rates_markdown(records: &[Json]) -> Result<P, Exc> {
    let (other, unknown, act) = (Json::str("other"), Json::str("unknown"), Json::str("Act"));
    let (mut by_topic, mut by_kind, mut by_coll) =
        (Counter::default(), Counter::default(), Counter::default());
    for r in records {
        by_topic.tally(or(&r.get("topic").cloned().unwrap_or(Json::Null), &other))?;
    }
    for r in records {
        by_kind.tally(or(field(r, "kind"), &unknown))?;
    }
    for r in records {
        by_coll.tally(or(field(r, "collection"), &unknown))?;
    }
    let mut ids: Vec<Key> = Vec::new();
    for r in records {
        let id = field(r, "register_id");
        if id.truthy() {
            let key = pyvalue::key(id).map_err(|m| {
                Exc::Type(format!(
                    "cannot use '{}' as a set element ({m})",
                    type_name(id)
                ))
            })?;
            if !ids.contains(&key) {
                ids.push(key);
            }
        }
    }
    let mut md: Vec<P> = vec![
        pts("# Rates, thresholds and indexation"),
        Vec::new(),
        pts(
            "> **Redistributable subset.** Entries for titles excluded from this distribution are not included.",
        ),
        Vec::new(),
        pts(
            "Derived from the Acts and instruments in this distribution. Every entry cites its title, \
             compilation and section. Check the provision before relying on a number: this is a finding \
             aid, not advice, and not the authorised text.",
        ),
        Vec::new(),
        pts(&format!(
            "{} entries across {} titles.",
            thousands(records.len()),
            thousands(ids.len())
        )),
        Vec::new(),
    ];
    md.extend(stale_source_lines(records)?);
    for line in [
        "Each `rate_id` is an ordinal within this snapshot, not a persistent",
        "identifier. Adding or removing a title renumbers the entries after it, so",
        "cite the register id, section and snapshot rather than the ordinal alone.",
        "",
        "| Kind | Count |",
        "|---|---|",
    ] {
        md.push(pts(line));
    }
    let table_row = |key: &Json, n: usize| {
        let mut row = pts("| ");
        row.extend(s(key));
        row.extend(pts(&format!(" | {} |", thousands(n))));
        row
    };
    for (k, n) in by_kind.most_common() {
        md.push(table_row(k, n));
    }
    md.extend([Vec::new(), pts("| Collection | Count |"), pts("|---|---|")]);
    for (c, n) in by_coll.most_common() {
        md.push(table_row(c, n));
    }
    md.extend([Vec::new(), pts("| Topic | Count |"), pts("|---|---|")]);
    for (t, n) in by_topic.most_common() {
        md.push(table_row(t, n));
    }
    let table = Json::str("table");
    let is_act = |r: &Json| pyvalue::py_eq(or(field(r, "collection"), &act), &act);
    // `"\n".join(md)` refuses a non-str item only once everything is built.
    let mut join_error: Option<Exc> = None;
    for (topic, _) in by_topic.most_common() {
        let Json::Str(name) = topic else {
            return Err(Exc::Type(format!(
                "can only concatenate str (not \"{}\") to str",
                type_name(topic)
            )));
        };
        let mut heading = pts("## ");
        heading.extend(name.code_points());
        md.extend([Vec::new(), heading, Vec::new()]);
        let topic_records: Vec<&Json> = records
            .iter()
            .filter(|r| pyvalue::py_eq(or(field(r, "topic"), &other), topic))
            .collect();
        let tables: Vec<&&Json> = topic_records
            .iter()
            .filter(|r| pyvalue::py_eq(field(r, "kind"), &table))
            .collect();
        if !tables.is_empty() {
            md.extend([pts("### Rate tables"), Vec::new()]);
            for r in tables.iter().take(40) {
                let mut line = pts("**");
                line.extend(s(r.get("act").unwrap_or(&Json::str(""))));
                line.extend(pts("**"));
                if !is_act(r) {
                    line.extend(pts(" _(instrument)_"));
                }
                line.push(u32::from(' '));
                if field(r, "section").truthy() {
                    line.extend(pts("s "));
                    line.extend(s(getitem(r, "section")?));
                }
                line.extend(pts(" \u{2014} "));
                line.extend(s(or(field(r, "heading"), &Json::str(""))));
                md.push(line);
                md.push(Vec::new());
                let content = or(field(r, "content"), &Json::str("")).clone();
                match &content {
                    Json::Str(text) => md.push(text.code_points()),
                    other => {
                        if join_error.is_none() {
                            join_error = Some(Exc::Type(format!(
                                "sequence item {}: expected str instance, {} found",
                                md.len(),
                                type_name(other)
                            )));
                        }
                        md.push(Vec::new());
                    }
                }
                md.push(Vec::new());
            }
            if tables.len() > 40 {
                md.extend([
                    pts(&format!(
                        "_{} further tables in rates.jsonl._",
                        tables.len() - 40
                    )),
                    Vec::new(),
                ]);
            }
        }
        let others: Vec<&&Json> = topic_records
            .iter()
            .filter(|r| !pyvalue::py_eq(field(r, "kind"), &table))
            .collect();
        if !others.is_empty() {
            for line in [
                "### Provisions carrying a number",
                "",
                "| Title | Made by | Section | Kind | Amounts | Provision |",
                "|---|---|---|---|---|---|",
            ] {
                md.push(pts(line));
            }
            for r in others.iter().take(120) {
                let amounts = take(
                    join_strs(r.get("amounts").unwrap_or(&Json::Array(Vec::new())))?,
                    40,
                );
                let act_cell = take(
                    replace(
                        &str_field(or(field(r, "act"), &Json::str("")), "replace")?,
                        '|',
                        "\\|",
                    ),
                    48,
                );
                let made_by = if is_act(r) { "Act" } else { "instrument" };
                let section = s(or(field(r, "section"), &Json::str("-")));
                let kind = s(or(field(r, "kind"), &unknown));
                let content = str_field(or(field(r, "content"), &Json::str("")), "replace")?;
                let content = take(replace(&replace(&content, '|', "\\|"), '\n', " "), 150);
                let mut line = pts("| ");
                for (i, cell) in [act_cell, pts(made_by), section, kind, amounts, content]
                    .into_iter()
                    .enumerate()
                {
                    if i > 0 {
                        line.extend(pts(" | "));
                    }
                    line.extend(cell);
                }
                line.extend(pts(" |"));
                md.push(line);
            }
            if others.len() > 120 {
                md.extend([
                    Vec::new(),
                    pts(&format!(
                        "_{} further provisions in rates.jsonl._",
                        others.len() - 120
                    )),
                ]);
            }
        }
    }
    if let Some(error) = join_error {
        return Err(error);
    }
    let mut out = Vec::new();
    for (i, line) in md.into_iter().enumerate() {
        if i > 0 {
            out.push(0x0a);
        }
        out.extend(line);
    }
    out.push(0x0a);
    Ok(out)
}

pub const TABLE_BLOCK_LEAD: &str = "**Table-shaped instruments are split on their tables.**";

/// `replace_readme_table_block_paragraph(text, count)`: rewrite the
/// table_block paragraph finalize writes, for the subset, by a linear scan.
pub fn replace_readme_table_block_paragraph(text: &str, count: usize) -> String {
    let Some(start) = text.find(TABLE_BLOCK_LEAD) else {
        return text.to_string();
    };
    let end = text[start..]
        .find("\n\n")
        .map_or(text.len(), |i| start + i + 2);
    format!(
        "{}{TABLE_BLOCK_LEAD} {count} titles carry `granularity: table_block`: no headings anywhere, just a run \
         of tables. Each table becomes a row, carrying the prose that preceded it, such as the sentence naming \
         the enabling provision or the basin name. The Tax Practitioners Board instruments chunked this way are \
         not in this distribution; see REMOVED.md.\n\n{}",
        &text[..start],
        &text[end..]
    )
}

/// `replace_readme_collection_counts(text, acts, instruments)`: rewrite every
/// `N Acts and N legislative and notifiable` summary by a linear scan.
pub fn replace_readme_collection_counts(text: &str, acts: usize, instruments: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let marker: Vec<char> = " Acts and ".chars().collect();
    let suffix: Vec<char> = " legislative and notifiable".chars().collect();
    let replacement = format!("{acts} Acts and {instruments} legislative and notifiable");
    let find = |from: usize| (from..chars.len()).find(|&i| chars[i..].starts_with(&marker));
    let mut out = String::new();
    let mut cursor = 0;
    loop {
        let Some(marker_at) = find(cursor) else {
            out.extend(&chars[cursor..]);
            return out;
        };
        let mut number_start = marker_at;
        while number_start > cursor && py::is_decimal(chars[number_start - 1]) {
            number_start -= 1;
        }
        let second_start = marker_at + marker.len();
        let mut second_end = second_start;
        while second_end < chars.len() && py::is_decimal(chars[second_end]) {
            second_end += 1;
        }
        if number_start < marker_at
            && second_start < second_end
            && chars[second_end..].starts_with(&suffix)
        {
            out.extend(&chars[cursor..number_start]);
            out.push_str(&replacement);
            cursor = second_end + suffix.len();
        } else {
            out.extend(&chars[cursor..=marker_at]);
            cursor = marker_at + 1;
        }
    }
}

// ------------------------------------------------------------ building

fn load(path: &str) -> Result<Json, Exc> {
    let text = crate::pyio::read_text(path)?;
    pyjson::loads_standard(&text).map_err(json_error)
}

fn set(object: &mut Json, key: &str, value: Json) {
    if let Json::Object(pairs) = object {
        match pairs.iter_mut().find(|(k, _)| k.as_str() == Some(key)) {
            Some(slot) => slot.1 = value,
            None => pairs.push((PyStr::Text(key.into()), value)),
        }
    }
}

/// The string `json.dumps` writes for a dict key: str as it is, numbers,
/// booleans and None in their JSON spelling.
fn json_key(value: &Json) -> PyStr {
    match value {
        Json::Str(s) => s.clone(),
        Json::Null => PyStr::Text("null".into()),
        Json::Bool(b) => PyStr::Text(if *b { "true" } else { "false" }.into()),
        other => PyStr::from_code_points(dumps_line(other, false)),
    }
}

fn int(n: usize) -> Json {
    Json::Int(n.into())
}

/// The container level at which `json.dump` stops writing sources.json under
/// `python -m fadden dist` (CPython 3.15, base revision): a sources.json member
/// nested 990 deep is written, 991 is refused (`tools/stage_depth_probe.py`).
const SOURCES_DEPTH: usize = 991;

/// `json.dump(value, f, indent=1, ensure_ascii=False)` through a text-mode
/// file, which it opens first. It stops at the first string the file cannot
/// encode (a lone surrogate) or the first container past `SOURCES_DEPTH`,
/// whichever the encoder reaches first, leaving the text written before it.
/// The depth stop carries a note per enclosing item. The encoding error,
/// raised by the file's write, carries none; its position is inside the chunk
/// the encoder yielded, after the file's newline translation.
fn dump_json(path: &str, value: &Json) -> Result<(), Exc> {
    fn depth_error(value: &Json, level: usize) -> Option<Exc> {
        match value {
            Json::Array(_) | Json::Object(_) if level >= SOURCES_DEPTH => Some(super::too_deep()),
            Json::Array(items) => items.iter().enumerate().find_map(|(i, v)| {
                depth_error(v, level + 1)
                    .map(|e| e.noted(format!("when serializing list item {i}")))
            }),
            Json::Object(pairs) => pairs.iter().find_map(|(k, v)| {
                depth_error(v, level + 1).map(|e| {
                    let key = pyvalue::repr_points(&k.code_points());
                    e.noted(format!("when serializing dict item {key}"))
                })
            }),
            _ => None,
        }
    }
    let (written, stop) = pyvalue::dump_until_stop(value, Some(1), false, SOURCES_DEPTH);
    write_text(path, &written)?;
    match stop {
        None => Ok(()),
        Some(Stop::Depth) => Err(depth_error(value, 0).unwrap_or_else(super::too_deep)),
        Some(Stop::Unencodable(chunk)) => {
            let mut translated = Vec::with_capacity(chunk.len());
            for cp in chunk {
                if cp == 0x0a && cfg!(windows) {
                    translated.push(0x0d);
                }
                translated.push(cp);
            }
            Err(Exc::UnicodeEncode(
                pyvalue::utf8_strict_error(&translated).unwrap_or_default(),
            ))
        }
    }
}

/// One item of the list `shutil.copytree` collects, as its `repr`: usually a
/// `(source, destination, str(error))` tuple.
type CopyError = String;

fn copy_error(from: &str, to: &str, text: &str) -> CopyError {
    format!(
        "({}, {}, {})",
        py::repr_str(from),
        py::repr_str(to),
        py::repr_str(text)
    )
}

/// Why copying one entry failed: an `OSError` (its text), the items of a
/// `shutil.Error`, or an exception copytree does not catch.
enum CopyFailure {
    Os(String),
    Nested(Vec<CopyError>),
    Other(Exc),
}

impl From<OsError> for CopyFailure {
    fn from(error: OsError) -> Self {
        CopyFailure::Os(error.text)
    }
}

impl From<Exc> for CopyFailure {
    fn from(error: Exc) -> Self {
        match error {
            Exc::Os(error) => CopyFailure::Os(error.text),
            // SpecialFileError is an OSError.
            Exc::Shutil {
                class: "shutil.SpecialFileError",
                message,
            } => CopyFailure::Os(message),
            // SameFileError is a shutil.Error: copytree extends its list with
            // `err.args[0]`, here the message, one character at a time.
            Exc::Shutil {
                class: "shutil.SameFileError",
                message,
            } => CopyFailure::Nested(
                message
                    .chars()
                    .map(|c| py::repr_str(&c.to_string()))
                    .collect(),
            ),
            other => CopyFailure::Other(other),
        }
    }
}

/// `shutil.copytree(src, dst, symlinks=True)`: list `src`, create `dst`,
/// which must not exist, and copy each entry, collecting the failures into one
/// `shutil.Error` after the directory's own metadata is copied.
fn copytree(src: &str, dst: &str) -> Result<(), Exc> {
    let errors = copytree_errors(src, dst)?;
    if errors.is_empty() {
        return Ok(());
    }
    Err(Exc::Shutil {
        class: "shutil.Error",
        message: format!("[{}]", errors.join(", ")),
    })
}

fn copytree_errors(src: &str, dst: &str) -> Result<Vec<CopyError>, Exc> {
    let listing = |e: std::io::Error| OsError::new(&e, Some(src), Style::Native);
    let mut names = Vec::new();
    for entry in pyos::scandir(src).map_err(listing)? {
        names.push(pyos::entry_name(&entry.map_err(listing)?)?);
    }
    pyos::makedirs_new(dst)?;
    let mut errors = Vec::new();
    for name in names {
        let (from, to) = (pypath::join(src, &name), pypath::join(dst, &name));
        match copy_entry(&from, &to) {
            Ok(()) => {}
            Err(CopyFailure::Os(text)) => errors.push(copy_error(&from, &to, &text)),
            Err(CopyFailure::Nested(nested)) => errors.extend(nested),
            Err(CopyFailure::Other(error)) => return Err(error),
        }
    }
    // Copying a directory's times can fail on Windows; copytree ignores that.
    if let Err(error) = copystat(src, dst)
        && cfg!(not(windows))
    {
        errors.push(copy_error(src, dst, &error.text));
    }
    Ok(errors)
}

/// One `copytree` entry: a link recreated as a link, a directory copied
/// recursively, anything else with `copy2`.
fn copy_entry(from: &str, to: &str) -> Result<(), CopyFailure> {
    let meta =
        std::fs::symlink_metadata(from).map_err(|e| OsError::new(&e, Some(from), Style::Native))?;
    // copytree enters a junction rather than recreating it.
    if meta.file_type().is_symlink() && !pyos::isjunction(from) {
        // shortcut: the link's own times and mode are not copied. The source
        // gate refused links just before the copy, so only a concurrent change
        // reaches here, and the copied-tree gate then refuses the link.
        let target =
            std::fs::read_link(from).map_err(|e| OsError::new(&e, Some(from), Style::Native))?;
        let target_text = target.to_string_lossy();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, to);
        // `os.symlink` without target_is_directory makes a file link.
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, to);
        made.map_err(|e| OsError::two(&e, &target_text, to, Style::Native))?;
        return Ok(());
    }
    if std::fs::metadata(from).is_ok_and(|m| m.is_dir()) {
        return match copytree_errors(from, to) {
            Ok(nested) if nested.is_empty() => Ok(()),
            Ok(nested) => Err(CopyFailure::Nested(nested)),
            Err(error) => Err(error.into()),
        };
    }
    copy2(from, to)
}

/// `shutil.copy2(src, dst)`: the data, times and permission bits. On Windows
/// CopyFile2 copies them together, as `std::fs::copy`'s CopyFileExW does.
fn copy2(from: &str, to: &str) -> Result<(), CopyFailure> {
    #[cfg(windows)]
    {
        std::fs::copy(from, to).map_err(|e| OsError::two(&e, from, to, Style::Native))?;
    }
    #[cfg(not(windows))]
    {
        // copytree hands copy2 the DirEntry, which the same-file message shows.
        let name = pypath::split(from).1;
        copyfile(from, to, &format!("<DirEntry {}>", py::repr_str(&name)))?;
        copystat(from, to)?;
    }
    Ok(())
}

/// `shutil.copyfile(src, dst)`, `src` shown as `shown`: the same file or a
/// named pipe on either side is refused before anything is opened, so a
/// destination linked to its source is never truncated.
pub(crate) fn copyfile(src: &str, dst: &str, shown: &str) -> Result<(), Exc> {
    if let (Ok(a), Ok(b)) = (pyos::file_id(src), pyos::file_id(dst))
        && a == b
    {
        return Err(Exc::Shutil {
            class: "shutil.SameFileError",
            message: format!("{shown} and {} are the same file", py::repr_str(dst)),
        });
    }
    #[cfg(unix)]
    for path in [src, dst] {
        use std::os::unix::fs::FileTypeExt;
        if std::fs::metadata(path).is_ok_and(|m| m.file_type().is_fifo()) {
            return Err(Exc::Shutil {
                class: "shutil.SpecialFileError",
                message: format!("`{path}` is a named pipe"),
            });
        }
    }
    let mut input = crate::pyio::open(src)?;
    let mut output =
        std::fs::File::create(dst).map_err(|e| OsError::new(&e, Some(dst), Style::Errno))?;
    std::io::copy(&mut input, &mut output).map_err(|e| OsError::two(&e, src, dst, Style::Errno))?;
    Ok(())
}

/// `shutil.copystat(src, dst)`: access and modification times, set by path,
/// then on Linux the extended attributes, then the permission bits.
fn copystat(src: &str, dst: &str) -> Result<(), OsError> {
    let source = |e: std::io::Error| OsError::new(&e, Some(src), Style::Native);
    let target = |e: std::io::Error| OsError::new(&e, Some(dst), Style::Native);
    let meta = std::fs::metadata(src).map_err(source)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let path = std::ffi::CString::new(dst)
            .map_err(|_| target(std::io::ErrorKind::InvalidInput.into()))?;
        let times = [
            libc::timespec {
                tv_sec: meta.atime(),
                tv_nsec: meta.atime_nsec(),
            },
            libc::timespec {
                tv_sec: meta.mtime(),
                tv_nsec: meta.mtime_nsec(),
            },
        ];
        // SAFETY: a NUL-terminated path and two timespecs, as utimensat takes.
        if unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) } != 0 {
            return Err(target(std::io::Error::last_os_error()));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let times = std::fs::FileTimes::new()
            .set_accessed(meta.accessed().map_err(source)?)
            .set_modified(meta.modified().map_err(source)?);
        // FILE_WRITE_ATTRIBUTES, with FILE_FLAG_BACKUP_SEMANTICS so a directory opens.
        std::fs::OpenOptions::new()
            .access_mode(0x100)
            .custom_flags(0x0200_0000)
            .open(dst)
            .and_then(|f| f.set_times(times))
            .map_err(target)?;
    }
    #[cfg(target_os = "linux")]
    copyxattr(src, dst)?;
    std::fs::set_permissions(dst, meta.permissions()).map_err(target)
}

/// `shutil._copyxattr(src, dst)`: every extended attribute, ignoring the
/// errors Python ignores (unsupported or absent attributes, and attributes
/// this account may not read or write).
#[cfg(target_os = "linux")]
fn copyxattr(src: &str, dst: &str) -> Result<(), OsError> {
    use std::ffi::CString;
    use std::io::Error;
    let c = |p: &str| {
        CString::new(p).map_err(|_| {
            OsError::new(
                &Error::from_raw_os_error(libc::EINVAL),
                Some(p),
                Style::Native,
            )
        })
    };
    let (s, d) = (c(src)?, c(dst)?);
    // Read a size-queried buffer, retrying if it grew in between.
    let read = |fill: &dyn Fn(*mut libc::c_char, usize) -> isize| -> Result<Vec<u8>, Error> {
        loop {
            let size = fill(std::ptr::null_mut(), 0);
            if size < 0 {
                return Err(Error::last_os_error());
            }
            let mut buffer = vec![0u8; size as usize];
            let got = fill(buffer.as_mut_ptr().cast(), buffer.len());
            if got >= 0 {
                buffer.truncate(got as usize);
                return Ok(buffer);
            }
            let error = Error::last_os_error();
            if error.raw_os_error() != Some(libc::ERANGE) {
                return Err(error);
            }
        }
    };
    // SAFETY (each call below): NUL-terminated paths and names, and a buffer
    // of the length passed, or null with length 0 to query the size.
    let names = match read(&|buf, len| unsafe { libc::listxattr(s.as_ptr(), buf, len) }) {
        Ok(names) => names,
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOTSUP | libc::ENODATA | libc::EINVAL)
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(OsError::new(&e, Some(src), Style::Native)),
    };
    for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let Ok(name) = CString::new(name) else {
            continue;
        };
        let copied =
            read(&|buf, len| unsafe { libc::getxattr(s.as_ptr(), name.as_ptr(), buf.cast(), len) })
                .map_err(|e| (e, src))
                .and_then(|value| {
                    let set = unsafe {
                        libc::setxattr(
                            d.as_ptr(),
                            name.as_ptr(),
                            value.as_ptr().cast(),
                            value.len(),
                            0,
                        )
                    };
                    if set == 0 {
                        Ok(())
                    } else {
                        Err((Error::last_os_error(), dst))
                    }
                });
        if let Err((e, path)) = copied
            && !matches!(
                e.raw_os_error(),
                Some(libc::EPERM | libc::ENOTSUP | libc::ENODATA | libc::EINVAL | libc::EACCES)
            )
        {
            return Err(OsError::new(&e, Some(path), Style::Native));
        }
    }
    Ok(())
}

/// `len(text.split())` on a row's text, or the AttributeError a non-str raises.
fn word_count(text: &Json) -> Result<usize, Exc> {
    let points = str_field(text, "split")?;
    Ok(points
        .split(|&cp| char::from_u32(cp).is_some_and(py::is_space))
        .filter(|w| !w.is_empty())
        .count())
}

/// Python `re.sub(pattern, replacement, text, count=1, flags=re.M)` with a
/// literal replacement.
fn sub_once(pattern: &str, flags: u8, replacement: &str, text: &str) -> String {
    let re = crate::pyre::re(pattern, flags);
    match re
        .search(text)
        .and_then(|c| c.get(0).map(|m| (m.start(), m.end())))
    {
        Some((a, b)) => format!("{}{replacement}{}", &text[..a], &text[b..]),
        None => text.to_string(),
    }
}

/// `re.sub(pattern, replacement, text)`: every match, replacement literal.
fn sub_all(pattern: &str, replacement: &str, text: &str) -> String {
    crate::pyre::re(pattern, 0).sub_with(text, |_| replacement.to_string())
}

struct Paths {
    root: String,
    here: String,
    policy: String,
}

/// `_build_distribution(staging)`.
fn build_distribution(paths: &Paths, staging: &str, run: &mut Run) -> Result<(), Exc> {
    let approved = pii::load_contact_allowlist(&paths.policy)?;
    let flagged = load(&pypath::join(&paths.here, "pii_flagged.json"))?;
    let mut drop: Vec<(String, Json)> = Vec::new();
    for f in iterate(&flagged)? {
        let rid = register_id(getitem(&f, "register_id")?)?;
        match drop.iter_mut().find(|(k, _)| *k == rid) {
            Some(slot) => slot.1 = f,
            None => drop.push((rid, f)),
        }
    }
    let src = load(&child(&paths.root, &["sources.json"])?)?;
    let source_titles = iterate(getitem(&src, "titles")?)?;
    let mut licensed: Vec<(String, Json)> = Vec::new();
    for t in &source_titles {
        let value = getitem(t, "register_id")?;
        if matches!(value, Json::Array(_) | Json::Object(_)) {
            return Err(Exc::Type(format!(
                "cannot use '{0}' as a set element (unhashable type: '{0}')",
                type_name(value)
            )));
        }
        if value
            .as_str()
            .is_some_and(|v| LICENCE_EXCLUDED.contains(&v))
        {
            let rid = register_id(value)?;
            let name = getitem(t, "name")?.clone();
            match licensed.iter_mut().find(|(k, _)| *k == rid) {
                Some(slot) => slot.1 = name,
                None => licensed.push((rid, name)),
            }
        }
    }
    let removed: std::collections::BTreeSet<String> = drop
        .iter()
        .map(|(k, _)| k.clone())
        .chain(licensed.iter().map(|(k, _)| k.clone()))
        .collect();
    let in_removed = |value: &Json| -> Result<bool, Exc> {
        if matches!(value, Json::Array(_) | Json::Object(_)) {
            return Err(Exc::Type(format!(
                "cannot use '{0}' as a set element (unhashable type: '{0}')",
                type_name(value)
            )));
        }
        Ok(value.as_str().is_some_and(|v| removed.contains(v)))
    };

    let mut titles: Vec<Json> = Vec::new();
    for title in &source_titles {
        let rid = register_id(getitem(title, "register_id")?)?;
        if removed.contains(&rid) {
            continue;
        }
        let mut copy = title.clone();
        set(&mut copy, "register_id", Json::str(&rid));
        set(&mut copy, "epub", Json::Null);
        set(&mut copy, "epub_included", Json::Bool(false));
        titles.push(copy);
    }

    // The gate runs against the source before anything is copied.
    reject_unapproved_contacts(&paths.root, &titles, &approved)?;
    PREFLIGHT_HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook();
        }
    });

    for part in ["markdown", "rates"] {
        let path = child(staging, &[part])?;
        pyos::makedirs_new(&path)?;
    }

    let unknown = Json::str("unknown");
    let (mut kept_rows, mut kept_words, mut section_rows) = (0usize, 0usize, 0usize);
    let mut kind_counts = Counter::default();
    let mut stats = Counter::default();
    for t in &titles {
        stats.tally(or(field(t, "collection"), &unknown))?;
    }
    let (mut rows_by_coll, mut words_by_coll) = (Counter::default(), Counter::default());
    for title in &titles {
        let rid = register_id(getitem(title, "register_id")?)?;
        let d = child(&paths.root, &["markdown", &rid])?;
        if !pyos::isdir(&d) {
            return Err(Exc::Runtime(format!(
                "listed title {rid} has no markdown directory"
            )));
        }
        let sections = child(&d, &["sections.jsonl"])?;
        if !std::fs::metadata(&sections).is_ok_and(|m| m.is_file()) {
            return Err(Exc::Runtime(format!(
                "listed title {rid} has no sections.jsonl"
            )));
        }
        reject_symlinks(&d)?;
        let copied = child(staging, &["markdown", &rid])?;
        // Links are copied as links so the copied-tree gate refuses them.
        copytree(&d, &copied)?;
        let (private_files, unexpected) = title_privacy_findings(&copied, &rid, title, &approved)?;
        if private_files > 0 {
            return Err(Exc::Runtime(format!(
                "copied title contains private-person registration details: {rid}"
            )));
        }
        if let Some((kind, digest, _)) = unexpected.first() {
            return Err(Exc::Runtime(format!(
                "copied title contains an unapproved contact identifier: {rid}:{kind}:{}",
                &digest[..16]
            )));
        }
        let mut lines = Lines::open(&child(staging, &["markdown", &rid, "sections.jsonl"])?)?;
        while let Some(line) = lines.next_line() {
            let line = line?;
            if py::strip(&line).is_empty() {
                continue;
            }
            let row = pyjson::loads_standard(&line).map_err(json_error)?;
            let text = or(get(&row, "text")?.unwrap_or(&Json::Null), &Json::str("")).clone();
            let coll = or(
                or(field(&row, "collection"), field(title, "collection")),
                &unknown,
            )
            .clone();
            kept_rows += 1;
            let words = word_count(&text)?;
            kept_words += words;
            rows_by_coll.add(&coll, 1)?;
            words_by_coll.add(&coll, words)?;
            let kind = if field(&row, "kind").truthy() {
                field(&row, "kind").clone()
            } else if field(&row, "section").truthy() {
                Json::str("section")
            } else {
                Json::str("unnumbered")
            };
            kind_counts.add(&kind, 1)?;
            if field(&row, "section").truthy() {
                section_rows += 1;
            }
        }
    }

    // The JSONL and its readable companion both come from this filtered set.
    let mut rate_records: Vec<Json> = Vec::new();
    let mut rates_dropped = 0usize;
    let mut lines = Lines::open(&child(&paths.root, &["rates", "rates.jsonl"])?)?;
    while let Some(line) = lines.next_line() {
        let line = line?;
        if py::strip(&line).is_empty() {
            continue;
        }
        let mut record = pyjson::loads_standard(&line).map_err(json_error)?;
        if in_removed(getitem(&record, "register_id")?)? {
            rates_dropped += 1;
            continue;
        }
        set(
            &mut record,
            "rate_id",
            Json::str(&format!("R{:05}", rate_records.len() + 1)),
        );
        rate_records.push(record);
    }
    let rates_path = child(staging, &["rates", "rates.jsonl"])?;
    std::fs::File::create(&rates_path)
        .map_err(|e| OsError::new(&e, Some(&rates_path), Style::Errno))?;
    let mut rates_text = Vec::new();
    for record in &rate_records {
        let mut line = dumps_line(record, false);
        if let Some(message) = pyvalue::utf8_strict_error(&line) {
            write_text(&rates_path, &rates_text)?;
            return Err(Exc::UnicodeEncode(message));
        }
        line.push(0x0a);
        rates_text.extend(line);
    }
    write_text(&rates_path, &rates_text)?;
    let rates_md_path = child(staging, &["rates", "RATES.md"])?;
    std::fs::File::create(&rates_md_path)
        .map_err(|e| OsError::new(&e, Some(&rates_md_path), Style::Errno))?;
    write_text(&rates_md_path, &render_rates_markdown(&rate_records)?)?;

    // sources.json minus the dropped titles, every count rebuilt from what ships.
    let mut unavailable = Vec::new();
    if let Some(items) = get(&src, "titles_without_epub")? {
        for a in iterate(items)? {
            if !in_removed(get(&a, "register_id")?.unwrap_or(&Json::Null))? {
                unavailable.push(a);
            }
        }
    }
    let act = Json::str("Act");
    let mut by_collection = Vec::new();
    for coll in sorted_keys(stats.0.iter().map(|(_, v, _)| v.clone()).collect())? {
        by_collection.push((
            json_key(&coll),
            Json::obj([
                ("titles", int(stats.get(&coll))),
                ("rows", int(rows_by_coll.get(&coll))),
                ("words", int(words_by_coll.get(&coll))),
            ]),
        ));
    }
    let count_where = |f: &dyn Fn(&Json) -> bool| titles.iter().filter(|t| f(t)).count();
    let acts = stats.get(&act);
    let counts = Json::Object(
        [
            ("titles", int(titles.len())),
            ("acts", int(acts)),
            ("instruments", int(titles.len() - acts)),
            ("by_collection", Json::Object(by_collection)),
            ("titles_without_epub", int(unavailable.len())),
            ("jsonl_rows", int(kept_rows)),
            ("words_body_only", int(kept_words)),
            ("epub_bytes", int(0)),
            ("rows_with_section_id", int(section_rows)),
            (
                "rows_container",
                int(kind_counts.get(&Json::str("container"))),
            ),
            (
                "rows_unnumbered",
                int(kind_counts.get(&Json::str("unnumbered"))),
            ),
            (
                "rows_introductory",
                int(kind_counts.get(&Json::str("introductory"))),
            ),
            (
                "titles_no_keyword_in_current_name",
                int(count_where(&|t| !field(t, "keywords_in_name").truthy())),
            ),
            (
                "titles_whole_act_chunk",
                int(count_where(&|t| {
                    pyvalue::py_eq(field(t, "granularity"), &Json::str("whole_act"))
                })),
            ),
            (
                "titles_table_block_chunk",
                int(count_where(&|t| {
                    pyvalue::py_eq(field(t, "granularity"), &Json::str("table_block"))
                })),
            ),
            (
                "titles_with_endnotes",
                int(count_where(&|t| field(t, "endnotes").truthy())),
            ),
            (
                "titles_not_current_version",
                int(count_where(&|t| {
                    matches!(t.get("version_is_current"), Some(Json::Bool(false)))
                })),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (PyStr::Text(k.into()), v))
        .collect(),
    );
    let table_block =
        count_where(&|t| pyvalue::py_eq(field(t, "granularity"), &Json::str("table_block")));
    let mut not_current = Vec::new();
    if let Some(items) = get(&src, "titles_not_current_version")? {
        for t in iterate(items)? {
            if !in_removed(get(&t, "register_id")?.unwrap_or(&Json::Null))? {
                not_current.push(t);
            }
        }
    }
    let mut excluded: Vec<(String, Json)> = Vec::new();
    for (rid, f) in &drop {
        excluded.push((
            rid.clone(),
            Json::obj([
                ("register_id", Json::str(rid)),
                ("name", getitem(f, "name")?.clone()),
                (
                    "reason",
                    Json::str("names private individuals; see REMOVED.md"),
                ),
            ]),
        ));
    }
    for (rid, name) in &licensed {
        excluded.push((
            rid.clone(),
            Json::obj([
                ("register_id", Json::str(rid)),
                ("name", name.clone()),
                (
                    "reason",
                    Json::str("restrictive copyright notice; see REMOVED.md"),
                ),
            ]),
        ));
    }
    excluded.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = src.clone();
    set(&mut out, "titles", Json::Array(titles.clone()));
    set(&mut out, "counts", counts);
    set(&mut out, "titles_count", int(titles.len()));
    set(&mut out, "titles_without_epub", Json::Array(unavailable));
    set(
        &mut out,
        "titles_not_current_version",
        Json::Array(not_current),
    );
    set(
        &mut out,
        "excluded_titles",
        Json::Array(excluded.into_iter().map(|(_, e)| e).collect()),
    );
    dump_json(&child(staging, &["sources.json"])?, &out)?;

    let licence_src = child(&paths.root, &["LICENCE-NOTICE.md"])?;
    let licence_dst = child(staging, &["LICENCE-NOTICE.md"])?;
    // `shutil.copy`: the data, then the permission bits.
    copyfile(&licence_src, &licence_dst, &py::repr_str(&licence_src))?;
    let mode = std::fs::metadata(&licence_src)
        .map_err(|e| OsError::new(&e, Some(&licence_src), Style::Native))?
        .permissions();
    std::fs::set_permissions(&licence_dst, mode)
        .map_err(|e| OsError::new(&e, Some(&licence_dst), Style::Native))?;

    // INDEX.md and README.md describe the full corpus: rewrite both for dist.
    let index_text = crate::pyio::read_text(&child(&paths.root, &["INDEX.md"])?)?;
    let mut dropped_lines = 0;
    let kept: Vec<&str> = index_text
        .split('\n')
        .filter(|line| {
            let hit = removed.iter().any(|r| line.contains(r.as_str()));
            dropped_lines += usize::from(hit);
            !hit
        })
        .collect();
    let mut txt = kept.join("\n");
    let instruments = titles.len() - acts;
    txt = sub_once(
        r"^[\d,]+ titles \([\d,]+ Acts, [\d,]+ instruments\), [\d,]+ retrieval rows, [\d,]+ words\.$",
        crate::pyre::M,
        &format!(
            "{} titles ({acts} Acts, {instruments} instruments), {} retrieval rows, {} words.\n\n\
             {} further titles were removed before publication; see REMOVED.md.",
            thousands(titles.len()),
            thousands(kept_rows),
            thousands(kept_words),
            removed.len()
        ),
        &txt,
    );
    for (key, label) in [
        ("Act", "Acts"),
        ("LegislativeInstrument", "Legislative instruments"),
        ("NotifiableInstrument", "Notifiable instruments"),
    ] {
        let k = Json::str(key);
        txt = sub_once(
            &format!(
                r"^\| {} \| [\d,]+ \| [\d,]+ \| [\d,]+ \|$",
                crate::pyre::escape(label)
            ),
            crate::pyre::M,
            &format!(
                "| {label} | {} | {} | {} |",
                thousands(stats.get(&k)),
                thousands(rows_by_coll.get(&k)),
                thousands(words_by_coll.get(&k))
            ),
            &txt,
        );
        txt = sub_once(
            &format!(r"^## {} \(\d+\)$", crate::pyre::escape(label)),
            crate::pyre::M,
            &format!("## {label} ({})", stats.get(&k)),
            &txt,
        );
    }
    write_text(&child(staging, &["INDEX.md"])?, &pts(&txt))?;

    let mut rd = crate::pyio::read_text(&child(&paths.root, &["README.md"])?)?;
    rd = sub_all(
        &format!("{} in-force principal titles", source_titles.len()),
        &format!("{} in-force principal titles", titles.len()),
        &rd,
    );
    rd = replace_readme_collection_counts(&rd, acts, instruments);
    rd = sub_all(
        r"instruments\.\s+[\d,]+ retrieval rows, [\d,]+ words\.",
        &format!(
            "instruments. {} retrieval rows, {} words.",
            thousands(kept_rows),
            thousands(kept_words)
        ),
        &rd,
    );
    rd = sub_once(
        r"^- `epub/<register_id>\.epub`[^\n]*\n",
        crate::pyre::M,
        "",
        &rd,
    );
    rd = sub_once(
        r"\*\*[^*\n]{1,24} titles name people\.\*\*.*?\n\n",
        crate::pyre::S,
        &format!(
            "**Titles naming people are not in this distribution.** The Tax Practitioners Board registers its \
             terminations and suspensions as notifiable instruments; the {} titles carrying those name tables \
             were removed before publication. See REMOVED.md for the list and their Register links.\n\n",
            drop.len()
        ),
        &rd,
    );
    rd = replace_readme_table_block_paragraph(&rd, table_block);
    rd = format!(
        "> **This is the redistributable subset.** The EPUBs, {} titles that name private individuals and {} \
         accounting standards under restrictive copyright notices are not included. See REMOVED.md for what \
         was dropped and why, and run the pipeline yourself for the full corpus.\n\n{rd}",
        drop.len(),
        licensed.len()
    );
    write_text(&child(staging, &["README.md"])?, &pts(&rd))?;
    run.print(&format!(
        "INDEX.md: dropped {dropped_lines} lines referencing removed titles"
    ));

    let mut lines: Vec<P> = vec![
        pts("# What was removed from this distribution"),
        Vec::new(),
        pts(&format!(
            "The full corpus this was built from covers {} titles. This distribution carries {}.",
            thousands(source_titles.len()),
            thousands(titles.len())
        )),
        Vec::new(),
        pts("## The EPUBs"),
        Vec::new(),
        pts(
            "Not included. The Register serves them with the Commonwealth Coat of Arms embedded, and the Coat of \
             Arms is excluded from the CC BY 4.0 grant covering everything else. Stripping it would also destroy \
             the only reason to ship them, which is that they are byte-identical to what legislation.gov.au \
             served. Run `download.py` to fetch your own.",
        ),
        Vec::new(),
        pts(
            "The markdown and JSONL contain no image data of any kind. Where the source had a figure, the text \
             carries a marker in its place: `[Commonwealth Coat of Arms omitted, not licensed under CC BY]` for \
             the Coat of Arms, `[image not described in source: ...]` for everything else.",
        ),
        Vec::new(),
        pts(&format!(
            "## {} titles naming private individuals",
            drop.len()
        )),
        Vec::new(),
        pts(
            "The Tax Practitioners Board registers terminations and suspensions of tax and BAS agents as \
             notifiable instruments. Each is a table of named people with their registration number and the \
             provision they breached. That is public on the Register, where it is a PDF you read one at a time. \
             Shipping it as rows in a dataset makes it name-searchable at scale, which is a different act, so \
             these are omitted.",
        ),
        Vec::new(),
        pts("Each remains available from the Register at \
             `https://www.legislation.gov.au/<register_id>/latest/text`."),
        Vec::new(),
        pts("| Register ID | Rows | Title |"),
        pts("|---|---|---|"),
    ];
    // `sorted(drop, key=lambda r: -drop[r]["names_est"])`: every key first.
    let mut ordered: Vec<(&String, &Json, Json)> = Vec::new();
    for (rid, f) in &drop {
        let names = getitem(f, "names_est")?;
        let key = match names {
            Json::Bool(b) => Json::Int(-num_bigint::BigInt::from(u8::from(*b))),
            Json::Int(i) => Json::Int(-i),
            Json::Float(x) => Json::Float(-x),
            other => {
                return Err(Exc::Type(format!(
                    "bad operand type for unary -: '{}'",
                    type_name(other)
                )));
            }
        };
        ordered.push((rid, f, key));
    }
    crate::pysort::sort(&mut ordered, |a, b| super::rates::py_lt(&a.2, &b.2))?;
    for (rid, f, _) in ordered {
        // The tuple's items are evaluated before `%` formats them.
        let rows_total = getitem(f, "rows_total")?;
        let name = replace(&str_field(getitem(f, "name")?, "replace")?, '|', "\\|");
        let rows = match rows_total {
            Json::Bool(b) => i64::from(*b).to_string(),
            Json::Int(i) => i.to_string(),
            Json::Float(x) if x.is_finite() => pyvalue::float_to_int(x.trunc()).to_string(),
            Json::Float(x) if x.is_nan() => {
                return Err(Exc::value("cannot convert float NaN to integer"));
            }
            Json::Float(_) => {
                return Err(Exc::Overflow(
                    "cannot convert float infinity to integer".into(),
                ));
            }
            other => {
                return Err(Exc::Type(format!(
                    "format argument 3: %d requires a real number, not {}",
                    type_name(other)
                )));
            }
        };
        let mut line = pts(&format!(
            "| [{rid}](https://www.legislation.gov.au/{rid}/latest/text) | {rows} | "
        ));
        line.extend(name);
        line.extend(pts(" |"));
        lines.push(line);
    }
    let mut unique: Vec<(&str, &str)> = approved
        .iter()
        .map(|(k, d, _)| (k.as_str(), d.as_str()))
        .collect();
    unique.sort_unstable();
    unique.dedup();
    lines.push(Vec::new());
    lines.push(pts(&format!(
        "Detection did not rely on the titles being recognisable. Every row in all {} titles was tested for \
         personal names appearing alongside agent registration numbers; these {} were what came back, and a \
         second pass at a lower threshold found nothing outside them. The only contact details the second pass \
         approved are {} unique organisational identifiers. Each is bound to a reviewed Register title in the \
         hashed allowlist; a new or moved contact fails publication.",
        thousands(source_titles.len()),
        drop.len(),
        unique.len()
    )));
    if !licensed.is_empty() {
        lines.extend([
            Vec::new(),
            pts(&format!("## {} accounting standards under restrictive copyright notices", licensed.len())),
            Vec::new(),
            pts(
                "Each carries its own notice, which the Register's CC BY 4.0 grant does not cover. AASB 112, \
                 2021-5 and 2023-2 reproduce IFRS Foundation material for personal, non-commercial use only; AASB \
                 1056 and 2023-4 need the AASB's written permission for any reproduction. Each remains available \
                 from the Register.",
            ),
            Vec::new(),
            pts("| Register ID | Title |"),
            pts("|---|---|"),
        ]);
        let mut sorted = licensed.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (rid, name) in &sorted {
            let mut line = pts(&format!(
                "| [{rid}](https://www.legislation.gov.au/{rid}/latest/text) | "
            ));
            line.extend(replace(&str_field(name, "replace")?, '|', "\\|"));
            line.extend(pts(" |"));
            lines.push(line);
        }
    }
    let mut removed_md = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            removed_md.push(0x0a);
        }
        removed_md.extend(line);
    }
    removed_md.push(0x0a);
    write_text(&child(staging, &["REMOVED.md"])?, &removed_md)?;

    let mut size = 0u64;
    for step in pyos::walk(staging, |_| {}) {
        let step = step?;
        for name in &step.files {
            let path = pypath::join(&step.top, name);
            size += std::fs::metadata(&path)
                .map_err(|e| OsError::new(&e, Some(&path), Style::Native))?
                .len();
        }
    }
    run.print(&format!(
        "titles {} (dropped {}) | rows {} | words {} | rates {} (dropped {rates_dropped}) | {:.1} MB",
        titles.len(),
        removed.len(),
        thousands(kept_rows),
        thousands(kept_words),
        rate_records.len(),
        size as f64 / 1e6
    ));
    Ok(())
}

/// Build, validate and publish `dist/` without exposing partial output, with
/// the module in `here`.
pub fn publish(here: &str, run: &mut Run) -> Result<(), Exc> {
    let directory = script_dir_in(here, "dist.py")?;
    let root = corpus_root(&directory);
    let dist = child(&root, &["dist"])?;
    require_builder_layout(&directory)?;
    let here = here.to_string();
    let paths = Paths {
        policy: pypath::join(&here, "pii_contact_allowlist.json"),
        root,
        here,
    };
    let target = validated_distribution_target(&dist)?;
    let staging = new_managed_sibling(&target, "stage", || pyos::random_hex(32))?;
    std::fs::create_dir(&staging).map_err(|e| OsError::new(&e, Some(&staging), Style::Native))?;
    let outcome = (|| -> Result<(), Exc> {
        build_distribution(&paths, &staging, run)?;
        let failures = verify_distribution(&staging, &paths.policy, run)?;
        if !failures.is_empty() {
            return Err(Exc::Runtime(format!(
                "staged distribution failed validation: {}",
                failures.join(", ")
            )));
        }
        promote_distribution(&staging, &target)
    })();
    // A successful promotion consumes the staging path; any earlier failure
    // leaves it beside dist, where this exact-prefix guard removes it.
    let cleanup = remove_managed_tree(&target, &staging, "stage");
    match cleanup {
        Err(error) => Err(error),
        Ok(()) => outcome,
    }
}

/// `python -m fadden dist`: the stage takes no arguments.
pub fn main(_argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    Ok(match publish(&module_dir(), run) {
        Ok(()) => 0,
        Err(error) => run.uncaught(&error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `json.dump` leaves in the file and raises for a lone surrogate:
    /// the text before the failing chunk, and a position inside that chunk
    /// counting the `\r` the file's newline translation adds on Windows.
    #[test]
    fn dump_json_stops_at_the_unencodable_chunk() {
        let crlf = usize::from(cfg!(windows));
        let cases = [
            (
                r#"{"good": 1, "bad": "\ud800"}"#,
                "{\n \"good\": 1,\n \"bad\": ",
                1,
            ),
            (r#"{"good": 1, "\ud800": 2}"#, "{\n \"good\": 1,\n ", 1),
            (
                r#"{"a": [1, "x", "\ud800"]}"#,
                "{\n \"a\": [\n  1,\n  \"x\"",
                5 + crlf,
            ),
            (r#"["\ud800"]"#, "", 4 + crlf),
            (
                r#"{"outer": {"bad": "pre\ud800post"}}"#,
                "{\n \"outer\": {\n  \"bad\": ",
                4,
            ),
            (r#""\ud800""#, "", 1),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sources.json");
        let path = path.to_str().unwrap();
        for (source, prefix, position) in cases {
            let value = pyjson::loads(source).unwrap();
            let error = dump_json(path, &value).unwrap_err();
            let message = format!(
                r"'utf-8' codec can't encode character '\ud800' in position {position}: surrogates not allowed"
            );
            assert_eq!(
                (error.class(), error.to_string()),
                ("UnicodeEncodeError", message),
                "{source}"
            );
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                prefix.replace('\n', if cfg!(windows) { "\r\n" } else { "\n" }),
                "{source}"
            );
        }
    }
}
