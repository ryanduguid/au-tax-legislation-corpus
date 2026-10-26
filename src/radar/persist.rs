//! Staged queue writes with rollback; readers detect interrupted generations.
use std::cell::RefCell;
use std::io::Write;

use super::{Error, Result};
use crate::pyos::{self, OsError, Style};
use crate::pypath::PyPath;

/// A filesystem step a test hook can observe or make fail.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Writing, flushing and syncing a staged file.
    Write,
    Link,
    Replace,
    /// Publishing a receipt on Windows (`os.rename`).
    Rename,
    Unlink,
    /// Resolving a path to compare outputs with inputs.
    Resolve,
}

/// What a test hook asks the step to do.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Proceed,
    /// Fail without performing the step.
    Fail(std::io::ErrorKind),
    /// Perform the step, then fail as an interruption would.
    FailAfter(std::io::ErrorKind),
}

type Hook = Box<dyn FnMut(Step, &str, &str) -> Fault>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Observe every filesystem step on this thread: `(step, source, destination)`.
#[doc(hidden)]
pub fn set_hook(hook: impl FnMut(Step, &str, &str) -> Fault + 'static) {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

#[doc(hidden)]
pub fn clear_hook() {
    HOOK.with(|h| *h.borrow_mut() = None);
}

fn simulated(kind: std::io::ErrorKind, step: Step, path: &str) -> OsError {
    let error = std::io::Error::new(kind, format!("simulated {step:?} failure"));
    OsError::new(&error, Some(path), Style::Native)
}

/// Run one filesystem step through the test hook, if any.
fn guarded<T>(
    step: Step,
    src: &str,
    dst: &str,
    op: impl FnOnce() -> std::result::Result<T, OsError>,
) -> std::result::Result<T, OsError> {
    let fault = HOOK.with(|h| {
        h.borrow_mut()
            .as_mut()
            .map_or(Fault::Proceed, |f| f(step, src, dst))
    });
    match fault {
        Fault::Proceed => op(),
        Fault::Fail(kind) => Err(simulated(kind, step, src)),
        Fault::FailAfter(kind) => op().and(Err(simulated(kind, step, src))),
    }
}

fn unlink(path: &PyPath) -> std::result::Result<(), OsError> {
    let text = path.display();
    guarded(Step::Unlink, &text, "", || pyos::unlink(&text))
}

fn remove_quietly(path: &PyPath) {
    let _ = unlink(path);
}

fn replace(src: &PyPath, dst: &PyPath) -> std::result::Result<(), OsError> {
    let (from, to) = (src.display(), dst.display());
    guarded(Step::Replace, &from, &to, || pyos::replace(&from, &to))
}

fn link(src: &PyPath, dst: &PyPath) -> std::result::Result<(), OsError> {
    let (from, to) = (src.display(), dst.display());
    guarded(Step::Link, &from, &to, || pyos::link(&from, &to))
}

fn resolve(path: &PyPath) -> std::result::Result<PyPath, OsError> {
    guarded(Step::Resolve, &path.display(), "", || path.resolve())
}

#[doc(hidden)]
pub fn restore_quietly(parked: &PyPath, destination: &PyPath) {
    if replace(parked, destination).is_ok() {
        // Renaming two links to the same inode can leave the backup name.
        remove_quietly(parked);
    }
}

/// Create a unique staging file beside `destination`, owner-only when private.
fn sibling_partial(
    destination: &PyPath,
    private: bool,
) -> std::result::Result<(PyPath, std::fs::File), OsError> {
    loop {
        let name = format!("{}.{}.partial", destination.name(), pyos::random_hex12());
        let candidate = destination.parent().join(&name);
        match pyos::create_new(&candidate.display(), private) {
            Ok(file) => return Ok((candidate, file)),
            // A 48-bit name collision.
            Err(error) if error.kind == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Text-mode UTF-8 bytes: newlines become the platform's line separator.
fn text_bytes(text: &str) -> Vec<u8> {
    if cfg!(windows) {
        text.replace('\n', "\r\n").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

/// Write, flush and fsync the whole text, as the Python stream did.
fn write_all(
    mut file: std::fs::File,
    path: &PyPath,
    text: &str,
) -> std::result::Result<(), OsError> {
    let os = |e: std::io::Error| OsError::new(&e, None, Style::Errno);
    guarded(Step::Write, &path.display(), "", || {
        file.write_all(&text_bytes(text)).map_err(os)?;
        file.flush().map_err(os)?;
        file.sync_all().map_err(os)
    })
}

/// Link the old inode without changing its name, permissions or ownership.
#[doc(hidden)]
pub fn backup_existing(destination: &PyPath) -> Result<Option<PyPath>> {
    let details = match pyos::lstat(&destination.display()) {
        Ok(details) => details,
        Err(error) if error.not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !details.is_file() || details.file_type().is_symlink() || pyos::is_reparse_point(&details) {
        return Err(Error::monitor("queue destinations must be ordinary files."));
    }
    let (parked, file) = sibling_partial(destination, true)?;
    drop(file);
    pyos::unlink(&parked.display())?;
    // Link creation never replaces a collision; failure leaves the original intact.
    link(destination, &parked)?;
    Ok(Some(parked))
}

/// The JSON and Markdown destinations a write to `output_dir` will use.
pub fn output_paths(output_dir: &PyPath, stem: &str) -> Result<(PyPath, PyPath)> {
    let valid = !stem.is_empty()
        && stem
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase()));
    if !valid {
        return Err(Error::monitor(
            "output file stem must be a lower-case hyphenated name.",
        ));
    }
    let output_dir = contained_output(output_dir)?;
    Ok((
        output_dir.join(&format!("{stem}.json")),
        output_dir.join(&format!("{stem}.md")),
    ))
}

/// Absolute outputs are explicit; relative outputs must stay within the working directory.
pub fn contained_output(path: &PyPath) -> Result<PyPath> {
    if path.is_absolute() {
        return Ok(path.clone());
    }
    let root = PyPath::cwd()?.resolve()?;
    let resolved = root.join(&path.display()).resolve()?;
    if !resolved.is_relative_to(&root) {
        return Err(Error::monitor(format!(
            "output path must stay within {root}."
        )));
    }
    let absolute = root.join(&path.display());
    Ok(absolute.parent().resolve()?.join(absolute.name()))
}

/// Publish a new receipt atomically without replacing any existing path.
pub fn write_receipt(text: &str, path: &PyPath, inputs: &[PyPath]) -> Result<PyPath> {
    let path = contained_output(path)?;
    refuse_input_overwrite(std::slice::from_ref(&path), inputs)?;
    match pyos::lstat(&path.display()) {
        Ok(_) => {
            return Err(Error::monitor(
                "The validation output must not exist; choose another --out.",
            ));
        }
        Err(error) if error.not_found() => {}
        Err(error) => return Err(error.into()),
    }
    pyos::makedirs(&path.parent())?;
    let (staged, file) = sibling_partial(&path, false)?;
    let published = write_all(file, &staged, text).and_then(|()| {
        if cfg!(windows) {
            // Windows rename fails if a destination appears after validation.
            let (from, to) = (staged.display(), path.display());
            guarded(Step::Rename, &from, &to, || rename_new(&staged, &path))
        } else {
            // A hard link publishes the fully written inode with no clobber.
            link(&staged, &path)
        }
    });
    if let Err(error) = published {
        // A cleanup failure is reported as the original error.
        let _ = unlink(&staged);
        return Err(error.into());
    }
    match unlink(&staged) {
        Ok(()) => {}
        Err(error) if error.not_found() => {}
        Err(_) => {
            return Err(Error::monitor(
                "Validation receipt was published, but its staging file could not be removed.",
            ));
        }
    }
    Ok(path)
}

#[cfg(windows)]
fn rename_new(src: &PyPath, dst: &PyPath) -> std::result::Result<(), OsError> {
    pyos::rename_new(&src.display(), &dst.display())
}

#[cfg(not(windows))]
fn rename_new(_src: &PyPath, _dst: &PyPath) -> std::result::Result<(), OsError> {
    unreachable!("POSIX publishes receipts with a hard link")
}

/// Reject output paths that name an input, including symbolic and hard links.
pub fn refuse_input_overwrite(destinations: &[PyPath], inputs: &[PyPath]) -> Result<()> {
    let unchecked = |_| Error::monitor("Output paths could not be checked against input files.");
    let protected = inputs
        .iter()
        .map(resolve)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(unchecked)?;
    for destination in destinations {
        let resolved = resolve(destination).map_err(unchecked)?;
        let mut clash = protected.iter().any(|p| p.same(&resolved));
        if !clash && pyos::exists(&resolved.display()) {
            for path in &protected {
                if pyos::exists(&path.display()) {
                    let ours = pyos::file_id(&resolved.display()).map_err(unchecked)?;
                    let theirs = pyos::file_id(&path.display()).map_err(unchecked)?;
                    if ours == theirs {
                        clash = true;
                        break;
                    }
                }
            }
        }
        if clash {
            return Err(Error::monitor(
                "The output path would replace an input file; choose another --out.",
            ));
        }
    }
    Ok(())
}

/// Stage and commit the JSON/Markdown pair, restoring the old pair on failure.
///
/// The two replacements are sequential and rollback is best effort, so this
/// assumes a single writer per output directory; readers detect a mixed pair
/// by the companion check rather than by any lock.
pub fn write_queue_files(
    json_text: &str,
    markdown_text: &str,
    output_dir: &PyPath,
    stem: &str,
    private: bool,
) -> Result<(PyPath, PyPath)> {
    let (json_path, markdown_path) = output_paths(output_dir, stem)?;
    pyos::makedirs(&json_path.parent())?;
    let rendered = [(&json_path, json_text), (&markdown_path, markdown_text)];

    let mut staged: Vec<(PyPath, PyPath)> = Vec::new();
    for (destination, text) in rendered {
        let outcome = sibling_partial(destination, private).and_then(|(path, file)| {
            staged.push((path.clone(), destination.clone()));
            write_all(file, &path, text)
        });
        if let Err(error) = outcome {
            for (path, _) in &staged {
                remove_quietly(path);
            }
            return Err(error.into());
        }
    }

    let mut backups: Vec<Option<PyPath>> = Vec::new();
    let mut replaced = 0;
    let mut outcome: Result<()> = Ok(());
    for (_, destination) in &staged {
        match backup_existing(destination) {
            Ok(parked) => backups.push(parked),
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    if outcome.is_ok() {
        for ((path, destination), _) in staged.iter().zip(&backups) {
            replaced += 1;
            if let Err(error) = replace(path, destination) {
                outcome = Err(error.into());
                break;
            }
        }
    }
    if outcome.is_err() {
        for index in (0..replaced).rev() {
            let destination = &staged[index].1;
            match &backups[index] {
                None => remove_quietly(destination),
                Some(parked) => restore_quietly(parked, destination),
            }
        }
    } else {
        for parked in backups.iter().flatten() {
            remove_quietly(parked);
        }
    }
    // Unattempted destinations still hold their originals; these links are expendable.
    for parked in backups.iter().skip(replaced).flatten() {
        remove_quietly(parked);
    }
    for (path, _) in &staged {
        remove_quietly(path);
    }
    outcome.map(|()| (json_path, markdown_path))
}
