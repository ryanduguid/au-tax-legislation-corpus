//! Source snapshots, exact JSON loading, digests and Markdown escaping.
use std::io::Read;

use sha2::{Digest, Sha256};

use super::{Error, Result};
use crate::pyjson::{self, DecodeError, Json, Layout};
use crate::pyos;
use crate::pypath::PyPath;

pub const MAX_JSON_BYTES: u64 = 50_000_000;
const MAX_JSON_DEPTH: i64 = 64;

/// Test hooks: what a snapshot reports as the file's size, and a callback run
/// after a snapshot's bytes are decoded for parsing.
#[doc(hidden)]
pub type SizeHook = Box<dyn FnMut(&PyPath, u64) -> u64>;
#[doc(hidden)]
pub type ReadHook = Box<dyn FnMut(&PyPath)>;

thread_local! {
    static SIZE_HOOK: std::cell::RefCell<Option<SizeHook>> = const { std::cell::RefCell::new(None) };
    static READ_HOOK: std::cell::RefCell<Option<ReadHook>> = const { std::cell::RefCell::new(None) };
}

#[doc(hidden)]
pub fn set_size_hook(hook: impl FnMut(&PyPath, u64) -> u64 + 'static) {
    SIZE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

#[doc(hidden)]
pub fn set_read_hook(hook: impl FnMut(&PyPath) + 'static) {
    READ_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

#[doc(hidden)]
pub fn clear_hooks() {
    SIZE_HOOK.with(|h| *h.borrow_mut() = None);
    READ_HOOK.with(|h| *h.borrow_mut() = None);
}

/// One immutable read of a source and the digest of those exact bytes.
pub struct SourceSnapshot {
    pub path: PyPath,
    pub content: Vec<u8>,
    pub sha256: String,
}

/// Whether an open file is a regular disk file, as `stat.S_ISREG(os.fstat(fd).st_mode)`.
#[cfg(windows)]
fn regular(file: &std::fs::File) -> std::result::Result<bool, std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
    // SAFETY: the handle stays open for the duration of the call.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Ok(false);
    }
    Ok(file.metadata()?.is_file())
}

#[cfg(not(windows))]
fn regular(file: &std::fs::File) -> std::result::Result<bool, std::io::Error> {
    Ok(file.metadata()?.is_file())
}

impl SourceSnapshot {
    /// Read `path` once: a regular file only, and at most `limit` bytes.
    pub fn capture(path: &PyPath, label: &str, limit: u64) -> Result<SourceSnapshot> {
        let shown = path.display();
        let unreadable =
            |text: &str| Error::monitor(format!("{label} could not be read: {shown} ({text})."));
        let file = pyos::open_read(&shown).map_err(|error| {
            if error.not_found() {
                Error::monitor(format!("{label} does not exist: {shown}."))
            } else {
                unreadable(&error.text)
            }
        })?;
        let os_error =
            |e: std::io::Error| unreadable(&pyos::OsError::new(&e, None, pyos::Style::Errno).text);
        if !regular(&file).map_err(os_error)? {
            return Err(Error::monitor(format!(
                "{label} must be a regular file: {shown}."
            )));
        }
        let size = file.metadata().map_err(os_error)?.len();
        let size = SIZE_HOOK.with(|h| h.borrow_mut().as_mut().map_or(size, |f| f(path, size)));
        if size > limit {
            return Err(Error::too_large(format!("{label} exceeds {limit} bytes.")));
        }
        // The first read is sized from metadata; a file that holds more than it
        // reported is read only to one byte past the limit.
        // Capacity is reserved exactly, so memory follows the bytes read.
        let mut content = Vec::with_capacity(usize::try_from(size + 1).unwrap_or(0));
        let mut reader = file;
        (&mut reader)
            .take(size + 1)
            .read_to_end(&mut content)
            .map_err(os_error)?;
        if content.len() as u64 > size {
            let more = limit + 1 - content.len() as u64;
            content.reserve_exact(usize::try_from(more).unwrap_or(0));
            (&mut reader)
                .take(more)
                .read_to_end(&mut content)
                .map_err(os_error)?;
        }
        if content.len() as u64 > limit {
            return Err(Error::too_large(format!("{label} exceeds {limit} bytes.")));
        }
        let sha256 = hex(&Sha256::digest(&content));
        Ok(SourceSnapshot {
            path: path.clone(),
            content,
            sha256,
        })
    }

    pub fn text(&self, label: &str) -> Result<String> {
        String::from_utf8(self.content.clone()).map_err(|_| {
            Error::monitor(format!(
                "{label} could not be read as UTF-8: {}.",
                self.path
            ))
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`.
pub fn canonical_json(value: &Json) -> String {
    pyjson::dumps(value, Layout::Canonical)
}

pub fn sha256_json(value: &Json) -> String {
    hex(&Sha256::digest(canonical_json(value).as_bytes()))
}

/// Parse a snapshot as JSON, refusing deep nesting and repeated members.
pub fn load_json(snapshot: &SourceSnapshot, label: &str) -> Result<Json> {
    if snapshot.content.len() as u64 > MAX_JSON_BYTES {
        return Err(Error::too_large(format!(
            "{label} exceeds {MAX_JSON_BYTES} bytes."
        )));
    }
    let text = snapshot.text(label)?;
    READ_HOOK.with(|h| {
        if let Some(f) = h.borrow_mut().as_mut() {
            f(&snapshot.path);
        }
    });
    let mut depth = 0i64;
    let (mut quoted, mut escaped) = (false, false);
    for character in text.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == '[' || character == '{' {
            depth += 1;
            if depth > MAX_JSON_DEPTH {
                return Err(Error::monitor(format!(
                    "{label} exceeds {MAX_JSON_DEPTH} JSON nesting levels."
                )));
            }
        } else if character == ']' || character == '}' {
            depth -= 1;
        }
    }
    pyjson::loads(&text).map_err(|error| match error {
        DecodeError::Duplicate => Error::monitor(format!(
            "{label} contains duplicate JSON members: {}.",
            snapshot.path
        )),
        // The depth check above stops nesting long before the decoder's limit.
        DecodeError::Invalid(_) | DecodeError::Recursion(_) => {
            Error::monitor(format!("{label} is not valid JSON: {}.", snapshot.path))
        }
    })
}

/// Read and parse `path`, as `load_json(path, label=...)` does.
pub fn load_json_path(path: &PyPath, label: &str) -> Result<Json> {
    load_json(
        &SourceSnapshot::capture(path, label, MAX_JSON_BYTES)?,
        label,
    )
}

/// A JSON object whose member names are exactly `required`.
pub fn load_json_exact(snapshot: &SourceSnapshot, required: &[&str], label: &str) -> Result<Json> {
    let payload = load_json(snapshot, label)?;
    if !payload.has_exactly(required) {
        let mut names = required.to_vec();
        names.sort_unstable();
        return Err(Error::monitor(format!(
            "{label} must contain exactly: {}.",
            names.join(", ")
        )));
    }
    Ok(payload)
}

/// Escape metadata as ordinary Markdown text, outside code spans.
pub fn safe_markdown(value: &str) -> String {
    let text = crate::py::splitlines(value).join(" ");
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
