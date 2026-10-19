//! The `os` calls the radar makes, failing with the text of CPython's `OSError`.
//!
//! On Windows, `open` and `os.open` go through the C runtime and report an
//! errno; the directory, link, rename and stat calls report the Win32 error.
//! A message names the path as Python passed it: the `str` of a `Path`.
use std::fs::{File, Metadata, OpenOptions};
use std::io;

use crate::py;

/// Which layer reported the error, which decides how Python renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// The C runtime's errno (`open`, `os.open`, file reads and writes).
    Errno,
    /// The platform call itself (Win32 on Windows).
    Native,
}

/// An `OSError`: its `str()` and the condition callers branch on.
#[derive(Debug)]
pub struct OsError {
    pub kind: io::ErrorKind,
    pub text: String,
    /// `error.strerror`.
    pub strerror: String,
}

impl OsError {
    pub fn new(error: &io::Error, filename: Option<&str>, style: Style) -> Self {
        let text = match filename {
            Some(name) => py::oserror_str(error, name, style.into()),
            None => without_filename(error, style),
        };
        OsError {
            kind: error.kind(),
            text,
            strerror: strerror(error, style),
        }
    }

    /// An error from a call that names two paths, such as `os.replace(a, b)`.
    pub fn two(error: &io::Error, src: &str, dst: &str, style: Style) -> Self {
        let one = py::oserror_str(error, src, style.into());
        OsError {
            kind: error.kind(),
            text: format!("{one} -> {}", py::repr_str(dst)),
            strerror: strerror(error, style),
        }
    }

    pub fn not_found(&self) -> bool {
        self.kind == io::ErrorKind::NotFound
    }

    /// The `OSError` subclass Python raises for this error.
    pub fn class(&self) -> &'static str {
        let number = |prefix: &str| {
            self.text
                .strip_prefix(prefix)
                .and_then(|rest| rest.split(']').next())
                .and_then(|n| n.parse::<i32>().ok())
        };
        // CPython's winerror_to_errno for the Win32 errors these calls report.
        let errno = number("[Errno ").or_else(|| {
            number("[WinError ").map(|winerror| match winerror {
                2 | 3 | 15 | 53 | 67 | 161 | 206 => 2,
                5 | 32 | 33 => 13,
                80 | 183 => 17,
                267 => 20,
                _ => 0,
            })
        });
        match errno {
            Some(2) => "FileNotFoundError",
            Some(1 | 13) => "PermissionError",
            Some(17) => "FileExistsError",
            Some(20) => "NotADirectoryError",
            Some(21) => "IsADirectoryError",
            Some(32) => "BrokenPipeError",
            _ => "OSError",
        }
    }
}

impl From<Style> for py::OsStyle {
    fn from(style: Style) -> Self {
        match style {
            Style::Errno => py::OsStyle::Errno,
            Style::Native => py::OsStyle::Native,
        }
    }
}

impl std::fmt::Display for OsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// `OSError.strerror`: the message without its number or filename.
fn strerror(error: &io::Error, style: Style) -> String {
    let text = without_filename(error, style);
    match text
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
    {
        Some((_, message)) => message.to_string(),
        None => text,
    }
}

/// `str(OSError)` with no filename, as a failed write or flush reports it.
fn without_filename(error: &io::Error, style: Style) -> String {
    let with = py::oserror_str(error, "", style.into());
    with.strip_suffix(": ''").unwrap_or(&with).to_string()
}

/// `open(path, "rb")`, non-blocking where the platform has `O_NONBLOCK`.
pub fn open_read(path: &str) -> Result<File, OsError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
    // FileIO refuses a directory that POSIX opened for reading.
    #[cfg(unix)]
    if file.metadata().is_ok_and(|m| m.is_dir()) {
        let error = io::Error::from_raw_os_error(libc::EISDIR);
        return Err(OsError::new(&error, Some(path), Style::Errno));
    }
    Ok(file)
}

/// `os.open(path, O_WRONLY | O_CREAT | O_EXCL, mode)`.
pub fn create_new(path: &str, private: bool) -> Result<File, OsError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o666 });
    }
    #[cfg(not(unix))]
    let _ = private;
    options
        .open(path)
        .map_err(|e| OsError::new(&e, Some(path), Style::Errno))
}

/// `os.lstat(path)`.
pub fn lstat(path: &str) -> Result<Metadata, OsError> {
    std::fs::symlink_metadata(path).map_err(|e| OsError::new(&e, Some(path), Style::Native))
}

/// `os.path.isdir(path)`.
pub fn isdir(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_dir())
}

/// `os.path.exists(path)`.
pub fn exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

/// `Path(path).mkdir(parents=True, exist_ok=True)`.
pub fn makedirs(path: &crate::pypath::PyPath) -> Result<(), OsError> {
    let text = path.display();
    match std::fs::create_dir(&text) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent();
            if parent.display() == text {
                return Err(OsError::new(&error, Some(&text), Style::Native));
            }
            makedirs(&parent)?;
            match std::fs::create_dir(&text) {
                Ok(()) => Ok(()),
                Err(_) if isdir(&text) => Ok(()),
                Err(error) => Err(OsError::new(&error, Some(&text), Style::Native)),
            }
        }
        Err(_) if isdir(&text) => Ok(()),
        Err(error) => Err(OsError::new(&error, Some(&text), Style::Native)),
    }
}

/// `os.link(src, dst, follow_symlinks=False)`.
pub fn link(src: &str, dst: &str) -> Result<(), OsError> {
    std::fs::hard_link(src, dst).map_err(|e| OsError::two(&e, src, dst, Style::Native))
}

/// `os.replace(src, dst)`.
pub fn replace(src: &str, dst: &str) -> Result<(), OsError> {
    std::fs::rename(src, dst).map_err(|e| OsError::two(&e, src, dst, Style::Native))
}

/// `os.rename(src, dst)` on Windows, which refuses an existing destination.
#[cfg(windows)]
pub fn rename_new(src: &str, dst: &str) -> Result<(), OsError> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (from, to) = (wide(src), wide(dst));
    // SAFETY: both buffers are NUL-terminated UTF-16 strings that outlive the call.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        return Err(OsError::two(&error, src, dst, Style::Native));
    }
    Ok(())
}

/// `os.unlink(path)`.
pub fn unlink(path: &str) -> Result<(), OsError> {
    std::fs::remove_file(path).map_err(|e| OsError::new(&e, Some(path), Style::Native))
}

/// The identity `os.path.samestat` compares: device and file number.
#[cfg(unix)]
pub fn file_id(path: &str) -> Result<(u64, u64), OsError> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| (m.dev(), m.ino()))
        .map_err(|e| OsError::new(&e, Some(path), Style::Native))
}

/// The identity `os.path.samestat` compares: volume serial number and file index.
#[cfg(windows)]
pub fn file_id(path: &str) -> Result<(u64, u64), OsError> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
    };
    let file = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|e| OsError::new(&e, Some(path), Style::Native))?;
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value to be overwritten.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the duration of the call and info is writable.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        let error = io::Error::last_os_error();
        return Err(OsError::new(&error, Some(path), Style::Native));
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((u64::from(info.dwVolumeSerialNumber), index))
}

/// Whether an `lstat` result is a Windows reparse point (`FILE_ATTRIBUTE_REPARSE_POINT`).
pub fn is_reparse_point(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

/// Twelve lower-case hex digits, as `uuid.uuid4().hex[:12]` gives.
pub fn random_hex12() -> String {
    random_hex(12)
}

/// `count` random lower-case hex digits.
pub fn random_hex(count: usize) -> String {
    let mut bytes = vec![0u8; count.div_ceil(2)];
    getrandom::fill(&mut bytes).expect("operating system random source");
    let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    text[..count].to_string()
}

/// The reparse tag of a Windows reparse point, without following it.
#[cfg(windows)]
fn reparse_tag(path: &str) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    let wide: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: WIN32_FIND_DATAW is plain data; FindFirstFileW fills it on success.
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let handle = unsafe { FindFirstFileW(wide.as_ptr(), &mut data) };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: the handle came from a successful FindFirstFileW.
    unsafe { FindClose(handle) };
    (data.dwFileAttributes & 0x400 != 0).then_some(data.dwReserved0)
}

const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// `os.path.islink(path)`: on Windows only a symbolic-link reparse point.
pub fn islink(path: &str) -> bool {
    #[cfg(windows)]
    {
        std::fs::symlink_metadata(path).is_ok_and(|m| is_reparse_point(&m))
            && reparse_tag(path) == Some(IO_REPARSE_TAG_SYMLINK)
    }
    #[cfg(not(windows))]
    {
        let _ = (IO_REPARSE_TAG_SYMLINK, IO_REPARSE_TAG_MOUNT_POINT);
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
    }
}

/// `os.path.isjunction(path)`: a Windows mount-point reparse point.
pub fn isjunction(path: &str) -> bool {
    #[cfg(windows)]
    {
        std::fs::symlink_metadata(path).is_ok_and(|m| is_reparse_point(&m))
            && reparse_tag(path) == Some(IO_REPARSE_TAG_MOUNT_POINT)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// `corpus_paths.is_reparse_point(path)`: `os.lstat`, which raises for a
/// missing path, then the reparse attribute (never set on POSIX).
pub fn is_reparse_point_at(path: &str) -> Result<bool, OsError> {
    Ok(is_reparse_point(&lstat(path)?))
}

/// One `os.walk(top, followlinks=False)` step: a directory, its subdirectory
/// names and its other names, in the order the directory lists them.
pub struct WalkStep {
    pub top: String,
    pub dirs: Vec<String>,
    pub files: Vec<String>,
}

/// `os.walk(top, followlinks=False)` top-down, errors ignored. A directory
/// symlink is listed among `dirs` but not entered; a junction is entered, as
/// CPython does. `order` may reorder each step's `dirs` before descent, as a
/// caller sorting `directories` in place does.
pub fn walk(top: &str, mut order: impl FnMut(&mut WalkStep)) -> Vec<WalkStep> {
    let mut steps = Vec::new();
    let mut stack = vec![top.to_string()];
    while let Some(top) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&top) else {
            continue;
        };
        let mut step = WalkStep {
            top: top.clone(),
            dirs: Vec::new(),
            files: Vec::new(),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
            if is_dir {
                step.dirs.push(name);
            } else {
                step.files.push(name);
            }
        }
        order(&mut step);
        for name in step.dirs.iter().rev() {
            let path = crate::pypath::join(&top, name);
            if !islink(&path) {
                stack.push(path);
            }
        }
        steps.push(step);
    }
    steps
}
