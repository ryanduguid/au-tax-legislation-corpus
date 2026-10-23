//! `fadden/corpus_paths.py`: the filesystem boundaries the ported stages use.
use std::collections::BTreeSet;
use std::io::Write as _;

use super::Exc;
use crate::pyjson::Json;
use crate::pyos::{OsError, Style};
use crate::pypath::{self, PyPath};

/// `register_id(value)`: `[A-Z][0-9]{4}[A-Z][0-9]{5}`.
pub fn register_id(value: &Json) -> Result<String, Exc> {
    let valid = value.as_str().filter(|s| {
        let b = s.as_bytes();
        b.len() == 11
            && b[0].is_ascii_uppercase()
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[5].is_ascii_uppercase()
            && b[6..].iter().all(u8::is_ascii_digit)
    });
    valid
        .map(str::to_string)
        .ok_or_else(|| Exc::value("invalid Federal Register identifier"))
}

pub fn register_id_str(value: &str) -> Result<String, Exc> {
    register_id(&Json::str(value))
}

/// `child(root, *parts)`: a realpath-contained descendant of `root`.
pub fn child(root: &str, parts: &[&str]) -> Result<String, Exc> {
    let root_path = pypath::realpath(root)?;
    let mut joined = root_path.clone();
    for part in parts {
        joined = pypath::join(&joined, part);
    }
    let candidate = pypath::realpath(&joined)?;
    let sep = pypath::SEP;
    let prefix = if root_path.ends_with(sep) {
        root_path.clone()
    } else {
        format!("{root_path}{sep}")
    };
    if candidate == root_path || !candidate.starts_with(&prefix) {
        return Err(Exc::value("path escapes the corpus root"));
    }
    Ok(candidate)
}

/// `refuse_unlisted_titles(root, listed)`: a title entry under markdown/ the
/// build does not list stops the stage; dot-entries are tooling.
pub fn refuse_unlisted_titles(root: &str, listed: &BTreeSet<String>) -> Result<(), Exc> {
    let base = child(root, &["markdown"])?;
    if !crate::pyos::isdir(&base) {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(&base).map_err(|e| OsError::new(&e, Some(&base), Style::Native))?;
    let mut stale = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| OsError::new(&e, Some(&base), Style::Native))?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let name = crate::pyos::entry_name(&entry)?;
        if !listed.contains(&name) {
            stale.push(name);
        }
    }
    stale.sort();
    if stale.is_empty() {
        return Ok(());
    }
    Err(Exc::Runtime(format!(
        "markdown/ holds entries this build does not list, which would stay searchable as in force: {}. \
         Move them out of the corpus and re-run; nothing was written or deleted.",
        stale.join(", ")
    )))
}

/// The directory of the stage's Python module: `FADDEN_MODULE_DIR`, which
/// `python -m fadden` sets, else `fadden` under the working directory.
pub fn module_dir() -> String {
    std::env::var("FADDEN_MODULE_DIR").unwrap_or_else(|_| {
        let cwd = std::env::current_dir().unwrap_or_default();
        pypath::join(&cwd.to_string_lossy(), "fadden")
    })
}

/// `Path(<module>/<file>).resolve().parent`.
pub fn script_dir(file: &str) -> Result<PyPath, Exc> {
    script_dir_in(&module_dir(), file)
}

/// `Path(<module>/<file>).resolve().parent` for a given module directory.
pub fn script_dir_in(module: &str, file: &str) -> Result<PyPath, Exc> {
    let resolved = pypath::realpath(&pypath::join(module, file))?;
    Ok(PyPath::new(&resolved).parent())
}

fn is_file(path: &PyPath) -> bool {
    std::fs::metadata(path.fs()).is_ok_and(|m| m.is_file())
}

/// `corpus_root(script_file)`: the deterministic output root.
pub fn corpus_root(script_dir: &PyPath) -> String {
    if script_dir.name() == "fadden" {
        let parent = script_dir.parent();
        if parent.name() == "build" {
            return parent.parent().display();
        }
        return parent.join("corpus").display();
    }
    if script_dir.name() == "build" {
        return script_dir.parent().display();
    }
    script_dir.join("corpus").display()
}

/// `require_builder_layout(script_file)`: refuse an installed package directory.
pub fn require_builder_layout(directory: &PyPath) -> Result<(), Exc> {
    if directory.name() == "build" || is_file(&directory.join("sources.json")) {
        return Ok(());
    }
    if directory.name() == "fadden" {
        let project = directory.parent();
        if project.name() == "build"
            || (is_file(&project.join("pyproject.toml")) && is_file(&project.join("VERSION")))
        {
            return Ok(());
        }
    }
    Err(Exc::Runtime(
        "corpus stages require a source checkout or deployed build directory; \
         installed package directories are read-only inputs"
            .into(),
    ))
}

/// `str.splitlines()` over code points.
pub fn splitlines(points: &[u32]) -> Vec<&[u32]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < points.len() {
        let cp = points[i];
        if matches!(
            cp,
            0x0a | 0x0b | 0x0c | 0x0d | 0x1c | 0x1d | 0x1e | 0x85 | 0x2028 | 0x2029
        ) {
            lines.push(&points[start..i]);
            i += if cp == 0x0d && points.get(i + 1) == Some(&0x0a) {
                2
            } else {
                1
            };
            start = i;
        } else {
            i += 1;
        }
    }
    if start < points.len() {
        lines.push(&points[start..]);
    }
    lines
}

/// `markdown_text(value)` for `str(value)`: one literal Markdown line.
pub fn markdown_text(text: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(text.len());
    for (i, line) in splitlines(text).into_iter().enumerate() {
        if i > 0 {
            out.push(u32::from(' '));
        }
        for &cp in line {
            if char::from_u32(cp).is_some_and(|c| c.is_ascii_punctuation()) {
                out.push(u32::from('\\'));
            }
            out.push(cp);
        }
    }
    out
}

/// `with atomic_text_writer(path) as f: f.write(chunk)` for each chunk: text
/// mode (`\n` is the platform's line separator) and strict UTF-8, staged in a
/// unique sibling that replaces `path` only when every chunk was written.
pub fn write_text_atomic(path: &str, chunks: &[Vec<u32>]) -> Result<(), Exc> {
    write_text_atomic_then(path, chunks, None)
}

/// `write_text_atomic`, where `failure`, if any, is the exception the writing
/// block raises after its last chunk: the staged file is removed instead.
pub fn write_text_atomic_then(
    path: &str,
    chunks: &[Vec<u32>],
    failure: Option<Exc>,
) -> Result<(), Exc> {
    let destination = PyPath::new(path);
    let (temporary, mut file) = loop {
        let candidate = destination
            .parent()
            .join(&format!(
                ".{}.{}.tmp",
                destination.name(),
                crate::pyos::random_hex(32)
            ))
            .display();
        match crate::pyos::create_new(&candidate, false) {
            Ok(file) => break (candidate, file),
            Err(error) if error.kind == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let written = (|| -> Result<(), Exc> {
        for chunk in chunks {
            let mut text = Vec::with_capacity(chunk.len());
            for &cp in chunk {
                if cp == 0x0a && cfg!(windows) {
                    text.push(0x0d);
                }
                text.push(cp);
            }
            if let Some(message) = crate::pyvalue::utf8_strict_error(&text) {
                return Err(Exc::UnicodeEncode(message));
            }
            let bytes: String = text.iter().filter_map(|&cp| char::from_u32(cp)).collect();
            file.write_all(bytes.as_bytes())
                .map_err(|e| OsError::new(&e, None, Style::Errno))?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        file.sync_all()
            .map_err(|e| OsError::new(&e, None, Style::Errno))?;
        drop(file);
        if let Ok(meta) = std::fs::metadata(destination.fs()) {
            std::fs::set_permissions(&temporary, meta.permissions())
                .map_err(|e| OsError::new(&e, Some(&temporary), Style::Errno))?;
        }
        crate::pyos::replace(&temporary, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// `sorted(glob(markdown_root/*/sections.jsonl))` as entry names: entries not
/// starting with a dot whose sections.jsonl exists, even as a dangling link,
/// in the order of their full paths. A matching name that is not valid
/// Unicode is refused rather than skipped.
pub fn sections_files(markdown_root: &str) -> Result<Vec<String>, crate::pyos::NotUnicode> {
    let Ok(entries) = std::fs::read_dir(markdown_root) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name().to_string_lossy().starts_with('.')
            || std::fs::symlink_metadata(entry.path().join("sections.jsonl")).is_err()
        {
            continue;
        }
        names.push(crate::pyos::entry_name(&entry)?);
    }
    names.sort_by_key(|name| format!("{name}{}", pypath::SEP));
    Ok(names)
}

/// `open(path, "w", encoding="utf-8").write(text)`: text mode, so `\n` is the
/// platform's line separator.
pub fn write_text(path: &str, text: &[u32]) -> Result<(), Exc> {
    let mut bytes = Vec::with_capacity(text.len());
    for &cp in text {
        if cp == 0x0a && cfg!(windows) {
            bytes.push(b'\r');
        }
        let c = char::from_u32(cp).ok_or_else(|| {
            Exc::UnicodeEncode(crate::pyvalue::utf8_strict_error(text).unwrap_or_default())
        })?;
        let mut buffer = [0; 4];
        bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
    }
    let mut file =
        std::fs::File::create(path).map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
    file.write_all(&bytes)
        .map_err(|e| OsError::new(&e, None, Style::Errno))?;
    Ok(())
}

/// `iso_date(value)`: a `YYYY-MM-DD` calendar date before it becomes a URL part.
pub fn iso_date(value: &str) -> Result<String, Exc> {
    let shaped = value.len() == 10
        && value.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !shaped {
        return Err(Exc::value("invalid ISO calendar date"));
    }
    let number = |range: std::ops::Range<usize>| value[range].parse::<i64>().unwrap_or_default();
    crate::py::Date::new(number(0..4), number(5..7), number(8..10)).map_err(Exc::value)?;
    Ok(value.to_string())
}

/// `version_date(value)`: the Register's date or timestamp, validated, as its
/// calendar day.
pub fn version_date(value: Option<&Json>) -> Result<String, Exc> {
    const START: &str = "[0-9]{4}-[0-9]{2}-[0-9]{2}(?:T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:[.][0-9]{1,7})?(?:Z|[+-][0-9]{2}:[0-9]{2})?)?";
    let text = value
        .and_then(Json::as_str)
        .filter(|t| crate::pyre::fullmatch(START, 0, t))
        .ok_or_else(|| Exc::value("invalid Register version start"))?;
    if text.len() > 10 {
        let tail = &text[text.len().saturating_sub(6)..];
        let offset = tail.starts_with(['+', '-']) && tail.as_bytes()[3] == b':';
        if offset
            && (tail[1..3].parse::<u32>().unwrap_or(0) > 23
                || tail[4..6].parse::<u32>().unwrap_or(0) > 59)
        {
            return Err(Exc::value("invalid Register timestamp offset"));
        }
        // `datetime.time(hour, minute, second)`.
        for (range, name, top) in [
            (11..13, "hour", 23),
            (14..16, "minute", 59),
            (17..19, "second", 59),
        ] {
            let n: u32 = text[range].parse().unwrap_or_default();
            if n > top {
                return Err(Exc::value(format!("{name} must be in 0..{top}, not {n}")));
            }
        }
    }
    iso_date(&text[..10])
}

/// `version_rows(payload, title_id)`: the rows of a versions response, each
/// for `title_id`.
pub fn version_rows(payload: &Json, title_id: &str) -> Result<Vec<Json>, Exc> {
    let Some(Json::Array(rows)) = payload.get("value") else {
        return Err(Exc::value("invalid Register versions response"));
    };
    for row in rows {
        let ours = matches!(row, Json::Object(_))
            && crate::pyvalue::py_eq(
                row.get("titleId").unwrap_or(&Json::Null),
                &Json::str(title_id),
            );
        if !ours {
            return Err(Exc::value(
                "Register version belongs to another or unknown title",
            ));
        }
    }
    Ok(rows.clone())
}

/// `compilation_id(row)`: None for an explicit null; a missing field is an API failure.
pub fn compilation_id(row: &Json) -> Result<Option<String>, Exc> {
    match row.get("registerId") {
        None => Err(Exc::value(
            "Register version has no document identity field",
        )),
        Some(Json::Null) => Ok(None),
        Some(value) => register_id(value).map(Some),
    }
}

/// `write_json_atomic(path, value, indent=indent)`: `json.dump` with its
/// defaults through the atomic writer.
pub fn write_json_atomic(path: &str, value: &Json, indent: Option<usize>) -> Result<(), Exc> {
    // A failed dump leaves no file: the writer removes its temporary.
    super::refuse_deep(value)?;
    write_text_atomic(
        path,
        &[crate::pyvalue::dumps_with(value, indent, true, false)],
    )
}

const LINKED_TREE: &str = "corpus tree contains a symbolic link, junction or reparse point";

type ProbeHook = Box<dyn FnMut(&str) -> Option<OsError>>;

thread_local! {
    static PROBE_HOOK: std::cell::RefCell<Option<ProbeHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook(path)` before every link and reparse-point probe on this thread;
/// an error it returns is the probe's. For tests.
#[doc(hidden)]
pub fn set_probe_hook(hook: impl FnMut(&str) -> Option<OsError> + 'static) {
    PROBE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// `islink(path) or is_junction(path) or is_reparse_point(path)`: the last
/// `lstat`s the path and raises if it cannot. Every gate that refuses links
/// asks this one question.
pub fn linked(path: &str) -> Result<bool, Exc> {
    let injected = PROBE_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(path)));
    if let Some(error) = injected {
        return Err(Exc::Os(error));
    }
    Ok(crate::pyos::islink(path)
        || crate::pyos::isjunction(path)
        || crate::pyos::is_reparse_point_at(path)?)
}

/// `reject_symlinks(directory)`: refuse a tree holding a link, junction or
/// reparse point before anything walks or copies it.
pub fn reject_symlinks(directory: &str) -> Result<(), Exc> {
    if linked(directory)? {
        return Err(Exc::value(LINKED_TREE));
    }
    for step in crate::pyos::walk(directory, |_| {}) {
        let step = step?;
        for name in step.dirs.iter().chain(&step.files) {
            if linked(&pypath::join(&step.top, name))? {
                return Err(Exc::value(LINKED_TREE));
            }
        }
    }
    Ok(())
}

/// `os.path.relpath(path, start)` for a path inside `start`.
pub fn relative(path: &str, start: &str) -> String {
    let prefix = format!("{}{}", start.trim_end_matches(pypath::SEP), pypath::SEP);
    path.strip_prefix(&prefix).unwrap_or(path).to_string()
}

/// `_absolute(path)`: `Path(os.path.abspath(os.fspath(Path(path))))`.
pub fn abspath(path: &str) -> String {
    pypath::abspath(&PyPath::new(path).display())
}

/// Whether an `lstat` result is an ordinary entry of the wanted kind: a
/// directory or a regular file, and no link, junction or reparse point.
pub fn ordinary(path: &str, details: &std::fs::Metadata, directory: bool) -> bool {
    let kind = details.file_type();
    (if directory {
        kind.is_dir()
    } else {
        kind.is_file()
    }) && !crate::pyos::islink(path)
        && !crate::pyos::isjunction(path)
        && !crate::pyos::is_reparse_point(details)
}

/// `_same_location(left, right)`: the same path once links are resolved.
pub fn same_location(left: &str, right: &str) -> Result<bool, Exc> {
    Ok(pypath::normcase(&pypath::realpath(left)?) == pypath::normcase(&pypath::realpath(right)?))
}
