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

/// `os.makedirs(path)`: missing parents are created, and `path` itself must
/// not exist.
pub fn makedirs_new(path: &str) -> Result<(), OsError> {
    let (head, tail) = crate::pypath::split(path);
    if !head.is_empty() && !tail.is_empty() && !exists(&head) {
        match makedirs_new(&head) {
            Err(error) if error.kind == io::ErrorKind::AlreadyExists => {}
            other => other?,
        }
    }
    std::fs::create_dir(path).map_err(|e| OsError::new(&e, Some(path), Style::Native))
}

/// `os.mkdir(path, 0o700)`. On Windows, CPython 3.13 and later give such a
/// directory a protected DACL granting full access to SYSTEM, the
/// Administrators group and the owner only; it is created with that
/// descriptor rather than the parent's inherited permissions.
#[cfg(unix)]
pub fn mkdir_private(path: &str) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// `os.mkdir(path, 0o700)`. On Windows, CPython 3.13 and later give such a
/// directory a protected DACL granting full access to SYSTEM, the
/// Administrators group and the owner only; it is created with that
/// descriptor rather than the parent's inherited permissions.
#[cfg(windows)]
pub fn mkdir_private(path: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let sddl = wide("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)");
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the SDDL string is NUL-terminated and outlives the call, which
    // writes a LocalAlloc'd descriptor into `descriptor`.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let target = wide(path);
    // SAFETY: the path is NUL-terminated and the attributes and descriptor
    // stay valid for the call.
    let created = unsafe { CreateDirectoryW(target.as_ptr(), &attributes) };
    let error = io::Error::last_os_error();
    // SAFETY: `descriptor` came from LocalAlloc and is freed once.
    unsafe { LocalFree(descriptor) };
    if created == 0 {
        return Err(error);
    }
    Ok(())
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

/// `(st_dev, st_ino)` of an open file, as `os.fstat` reports them.
#[cfg(unix)]
pub fn handle_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok((meta.dev(), meta.ino()))
}

/// `(st_dev, st_ino)` of an open file, as `os.fstat` reports them: the volume
/// serial number and the file index.
#[cfg(windows)]
pub fn handle_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value to be overwritten.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the duration of the call and info is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((u64::from(info.dwVolumeSerialNumber), index))
}

/// `(st_dev, st_ino)` of `path` without following a final link, as `os.lstat`
/// reports them.
#[cfg(unix)]
pub fn path_identity(path: &str) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

/// `(st_dev, st_ino)` of `path` without following a final link, as `os.lstat`
/// reports them.
#[cfg(windows)]
pub fn path_identity(path: &str) -> io::Result<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let file = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    handle_identity(&file)
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

thread_local! {
    static FIXED_HEX: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static RANDOM_FAILURE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Make the `n`th following [`try_random_hex`] call on this thread fail, as
/// an `os.urandom` failure inside `uuid.uuid4()` would; `None` stops it.
/// For tests.
#[doc(hidden)]
pub fn set_random_failure(n: Option<usize>) {
    RANDOM_FAILURE.with(|f| f.set(n));
}

/// Make [`random_hex`] return `value` on this thread, as patching
/// `uuid.uuid4` to a fixed `hex` did; `None` restores randomness. For tests.
#[doc(hidden)]
pub fn set_fixed_hex(value: Option<&str>) {
    FIXED_HEX.with(|f| *f.borrow_mut() = value.map(str::to_string));
}

/// `count` random lower-case hex digits, or the operating system's refusal
/// to supply randomness, which `uuid.uuid4()` raises as an `OSError`.
pub fn try_random_hex(count: usize) -> Result<String, OsError> {
    if let Some(n) = RANDOM_FAILURE.with(std::cell::Cell::get) {
        RANDOM_FAILURE.with(|f| f.set(n.checked_sub(1).filter(|&left| left > 0)));
        if n == 1 {
            let error = io::Error::other("simulated randomness failure");
            return Err(OsError::new(&error, None, Style::Errno));
        }
    }
    if let Some(fixed) = FIXED_HEX.with(|f| f.borrow().clone()) {
        return Ok(fixed);
    }
    let mut bytes = vec![0u8; count.div_ceil(2)];
    getrandom::fill(&mut bytes).map_err(|e| {
        let error = io::Error::other(e.to_string());
        OsError::new(&error, None, Style::Errno)
    })?;
    let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(text[..count].to_string())
}

/// `count` random lower-case hex digits.
pub fn random_hex(count: usize) -> String {
    if let Some(fixed) = FIXED_HEX.with(|f| f.borrow().clone()) {
        return fixed;
    }
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

type ListingHook = Box<dyn FnMut(&str, usize) -> Option<io::Error>>;

thread_local! {
    static LISTING_HOOK: std::cell::RefCell<Option<ListingHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook(directory, index)` before each entry `scandir` yields on this
/// thread, including once past the last; an error it returns ends the
/// listing with that error. For tests.
#[doc(hidden)]
pub fn set_listing_hook(hook: impl FnMut(&str, usize) -> Option<io::Error> + 'static) {
    LISTING_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// `os.scandir(path)`: its entries in listing order, each of which can fail.
pub fn scandir(path: &str) -> io::Result<impl Iterator<Item = io::Result<std::fs::DirEntry>>> {
    let mut entries = std::fs::read_dir(path)?;
    let path = path.to_string();
    let (mut index, mut done) = (0, false);
    Ok(std::iter::from_fn(move || {
        if done {
            return None;
        }
        let injected =
            LISTING_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(&path, index)));
        index += 1;
        if let Some(error) = injected {
            done = true;
            return Some(Err(error));
        }
        let next = entries.next();
        done = next.is_none();
        next
    }))
}

/// A directory entry whose name is not valid Unicode (bytes that are not
/// UTF-8 on POSIX, a lone surrogate on Windows), by its lossy path. Python
/// carried such a name as a surrogate-escaped str; the stages refuse it.
#[derive(Debug)]
pub struct NotUnicode(pub String);

/// A directory entry's name as a str.
pub fn entry_name(entry: &std::fs::DirEntry) -> Result<String, NotUnicode> {
    entry
        .file_name()
        .into_string()
        .map_err(|_| NotUnicode(entry.path().to_string_lossy().into_owned()))
}

/// One `os.walk(top, followlinks=False)` step: a directory, its subdirectory
/// names and its other names, in the order the directory lists them.
pub struct WalkStep {
    pub top: String,
    pub dirs: Vec<String>,
    pub files: Vec<String>,
}

/// `os.walk(top, followlinks=False)` top-down, errors ignored: a directory
/// that cannot be opened or fails while listing yields no step, as CPython's
/// walk drops it. A directory symlink is listed among `dirs` but not entered;
/// a junction is entered, as CPython does. `order` may reorder each step's
/// `dirs` before descent, as a caller sorting `directories` in place does.
///
/// Like CPython's generator, a step's subdirectories are listed only when the
/// caller asks for the next step, so a caller that refuses an entry stops
/// before the walk enters it.
pub fn walk<F: FnMut(&mut WalkStep)>(top: &str, order: F) -> Walk<F> {
    Walk {
        stack: vec![top.to_string()],
        descend: None,
        order,
    }
}

/// The steps of `walk`, in order.
pub struct Walk<F> {
    stack: Vec<String>,
    /// The last step's directory and subdirectories, entered on the next call.
    descend: Option<(String, Vec<String>)>,
    order: F,
}

impl<F: FnMut(&mut WalkStep)> Iterator for Walk<F> {
    type Item = Result<WalkStep, NotUnicode>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((top, dirs)) = self.descend.take() {
            for name in dirs.iter().rev() {
                let path = crate::pypath::join(&top, name);
                if !islink(&path) {
                    self.stack.push(path);
                }
            }
        }
        'directories: while let Some(top) = self.stack.pop() {
            let Ok(entries) = scandir(&top) else {
                continue;
            };
            let mut step = WalkStep {
                top: top.clone(),
                dirs: Vec::new(),
                files: Vec::new(),
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    continue 'directories;
                };
                let name = match entry_name(&entry) {
                    Ok(name) => name,
                    Err(error) => return Some(Err(error)),
                };
                let is_dir = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
                if is_dir {
                    step.dirs.push(name);
                } else {
                    step.files.push(name);
                }
            }
            (self.order)(&mut step);
            self.descend = Some((top, step.dirs.clone()));
            return Some(Ok(step));
        }
        None
    }
}
