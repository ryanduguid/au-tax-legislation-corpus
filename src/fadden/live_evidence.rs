//! `fadden/export_live_evidence_bundles.py`: deterministic live-only
//! `evidence-bundle.v2` candidates from a verified Stage 3A capture.
//!
//! The export copies the capture graph once into a private snapshot through a
//! pinned directory chain (handles on Windows, no-follow descriptors on
//! POSIX), proves the snapshot with the capture's own graph validator, builds
//! the candidates, and publishes them through identity-bound Windows handle
//! operations: a no-replace rename of the held staging directory and deletion
//! of exactly the held children. Publication is Windows only; elsewhere it
//! fails before creating an output parent or a staging directory.
//!
//! The publication workflow still runs the Python stage; this port is verified
//! against it before any workflow change.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Read, Write};

use super::capture_register::{
    self as capture, ROW_FIELDS, capture_message, odata_context, sha256_id,
};
use super::corpus_paths::{self, abspath};
use super::iso::{self, Zone};
use super::{Exc, Run};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, DecodeError, Json, PyStr};
use crate::pyos::{self, OsError};
use crate::pypath::{self, PyPath};
use crate::pyre;
use crate::pyvalue::{self, py_eq};

const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_BUNDLE_BYTES: usize = 1024 * 1024;
const BASELINE_TITLE_FIELDS: [&str; 10] = [
    "register_id",
    "name",
    "collection",
    "compilation_number",
    "compilation_date",
    "version_is_current",
    "current_version_start",
    "retrieved",
    "source_url",
    "register_page",
];
const CAPTURE_RESULT_FIELDS: [&str; 6] = [
    "register_id",
    "collection",
    "checked_at",
    "state",
    "error_category",
    "requests",
];
const REQUEST_FIELDS: [&str; 10] = [
    "role",
    "url",
    "checked_at",
    "http_status",
    "transport_error_category",
    "attempt_count",
    "response_headers",
    "response_length",
    "response_sha256",
    "evidence_path",
];
const OBSERVATION_FIELDS: [&str; 14] = [
    "register_id",
    "collection",
    "state",
    "evidence_id",
    "observed_compilation_number",
    "observed_compilation_date",
    "observed_register_document_id",
    "current_version_start",
    "evidence_url",
    "checked_at",
    "error_category",
    "capture_result_sha256",
    "primary_response_sha256",
    "primary_response_media_type",
];

const CLASS: &str = "fadden.export_live_evidence_bundles.LiveEvidenceBundleError";

/// `LiveEvidenceBundleError(message)`.
pub fn live_error(message: impl Into<String>) -> Exc {
    Exc::Value {
        class: CLASS,
        message: message.into(),
    }
}

/// The message of a `LiveEvidenceBundleError`, if `error` is one.
pub fn live_message(error: &Exc) -> Option<&str> {
    match error {
        Exc::Value { class, message } if *class == CLASS => Some(message),
        _ => None,
    }
}

type P = Vec<u32>;

fn pts(text: &str) -> P {
    text.chars().map(u32::from).collect()
}

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or(&Json::Null)
}

fn is(value: &Json, text: &str) -> bool {
    py_eq(value, &Json::str(text))
}

// ----------------------------------------------------------- test hooks

/// A step of the export a test can fail or observe, as the Python tests
/// patch `os`, `ctypes` and the module's helpers.
#[doc(hidden)]
#[derive(Debug)]
pub enum Step<'a> {
    /// Pinning a directory of the capture chain.
    Pin(&'a str),
    /// Opening a pinned child relative to its directory.
    OpenChild(&'a str),
    /// Closing a pinned or held handle.
    Close(&'a str),
    /// Listing a directory's inventory.
    List(&'a str),
    /// Copying one snapshot file.
    Copy(&'a str),
    /// Creating the snapshot directories.
    Snapshot(&'a str),
    /// Every snapshot file copied; the hook may race the final inventory.
    Copied(&'a str),
    /// `os.mkdir` of the output parent or the staging directory.
    Mkdir(&'a str),
    /// `os.chmod` of a new directory or file.
    Chmod(&'a str),
    /// `_write_new(path)`.
    Write(&'a str),
    /// Holding a directory locked against rename and deletion.
    Lock(&'a str),
    /// The staged export validated; the hook may race the promotion.
    Validated(&'a str),
    /// The handle-bound rename.
    Promote(&'a str),
    /// Marking one held object for deletion.
    Delete(&'a str),
    /// Cleanup is about to open a staging child.
    Cleanup(&'a str),
    /// Cleanup of the held staging directory begins.
    CleanupStaging(&'a str),
    /// Re-reading the staged export: an error is its refusal.
    Reread(&'a str),
    /// The existing output parent, or a staged file, inspected: the hook may
    /// race its replacement.
    Inspected(&'a str),
    /// Removing the temporary snapshot directory.
    TempCleanup(&'a str),
    /// About to take a snapshot source file's expected identity; the hook
    /// may race it.
    Identify(&'a str),
}

type FaultHook = Box<dyn FnMut(Step) -> Option<OsError>>;

thread_local! {
    static FAULT_HOOK: RefCell<Option<FaultHook>> = const { RefCell::new(None) };
    static VERSION_DIR: RefCell<Option<String>> = const { RefCell::new(None) };
    static NOT_WINDOWS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `hook(step)` at each [`Step`] on this thread; an error it returns
/// fails that step. For tests.
#[doc(hidden)]
pub fn set_fault_hook(hook: impl FnMut(Step) -> Option<OsError> + 'static) {
    FAULT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// Read `VERSION` and `pyproject.toml` from `directory` on this thread. For tests.
#[doc(hidden)]
pub fn set_version_dir(directory: Option<&str>) {
    VERSION_DIR.with(|d| *d.borrow_mut() = directory.map(str::to_string));
}

/// Behave as a platform other than Windows on this thread, as patching
/// `os.name` did. For tests.
#[doc(hidden)]
pub fn set_not_windows(not_windows: bool) {
    NOT_WINDOWS.with(|n| n.set(not_windows));
}

/// Remove every hook on this thread. For tests.
#[doc(hidden)]
pub fn clear_hooks() {
    FAULT_HOOK.with(|h| *h.borrow_mut() = None);
    VERSION_DIR.with(|d| *d.borrow_mut() = None);
    NOT_WINDOWS.with(|n| n.set(false));
}

fn injected(step: Step) -> Option<OsError> {
    FAULT_HOOK.with(|h| h.borrow_mut().as_mut().and_then(|hook| hook(step)))
}

fn io_fault(step: Step) -> std::io::Result<()> {
    injected(step).map_or(Ok(()), |error| Err(std::io::Error::other(error.text)))
}

fn is_windows() -> bool {
    cfg!(windows) && !NOT_WINDOWS.with(std::cell::Cell::get)
}

// ------------------------------------------------------------ identities

/// What `os.stat_result` holds that the export compares: the type and
/// permission bits, the device and file number, the link count and the size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stat {
    pub mode: u32,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub size: u64,
    pub reparse: bool,
}

const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
#[cfg(windows)]
const S_IFLNK: u32 = 0o120_000;
const S_IFMT: u32 = 0o170_000;

impl Stat {
    fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    fn is_reg(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
}

/// `_same_regular_file_identity(expected, observed)`: one regular file with
/// a single link.
fn same_regular(expected: &Stat, observed: &Stat) -> bool {
    expected.is_reg()
        && observed.is_reg()
        && (expected.dev, expected.ino) == (observed.dev, observed.ino)
        && expected.nlink == 1
        && observed.nlink == 1
}

/// `_same_directory_identity(expected, observed)`.
fn same_directory(expected: &Stat, observed: &Stat) -> bool {
    expected.is_dir()
        && observed.is_dir()
        && (expected.dev, expected.ino) == (observed.dev, observed.ino)
}

#[cfg(unix)]
fn stat_of(meta: &std::fs::Metadata) -> Stat {
    use std::os::unix::fs::MetadataExt;
    Stat {
        mode: meta.mode(),
        dev: meta.dev(),
        ino: meta.ino(),
        nlink: meta.nlink(),
        size: meta.size(),
        reparse: false,
    }
}

#[cfg(windows)]
mod win {
    //! The Windows handle calls the export makes.
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FileRenameInformation, NtOpenFile, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, GENERIC_READ, HANDLE,
        INVALID_HANDLE_VALUE, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ADD_FILE, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileAttributeTagInfo, FileDispositionInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE, SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    use super::{S_IFDIR, S_IFLNK, S_IFREG, Stat};

    pub const FILE_DIRECTORY_FILE: u32 = 0x1;
    pub const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    pub const READ: u32 = GENERIC_READ | SYNCHRONIZE;
    pub const DELETE_ACCESS: u32 = FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE;
    pub const ADD_FILE: u32 = FILE_ADD_FILE;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `os.fstat(handle)` as the export compares it.
    pub fn handle_stat(handle: RawHandle) -> std::io::Result<Stat> {
        // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value to be overwritten.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the call and info is writable.
        if unsafe { GetFileInformationByHandle(handle as HANDLE, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let attributes = info.dwFileAttributes;
        let mut mode = if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            S_IFDIR | 0o111
        } else {
            S_IFREG
        };
        mode |= if attributes & FILE_ATTRIBUTE_READONLY != 0 {
            0o444
        } else {
            0o666
        };
        Ok(Stat {
            mode,
            dev: u64::from(info.dwVolumeSerialNumber),
            ino: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            nlink: u64::from(info.nNumberOfLinks),
            size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
            reparse: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        })
    }

    /// The reparse tag of an open reparse point.
    fn reparse_tag(handle: RawHandle) -> std::io::Result<u32> {
        // SAFETY: an all-zero FILE_ATTRIBUTE_TAG_INFO is a valid value to be overwritten.
        let mut info: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the call and info is writable for its size.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle as HANDLE,
                FileAttributeTagInfo,
                (&raw mut info).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(info.ReparseTag)
    }

    /// `os.lstat(path)` as the export compares it, from one handle opened
    /// without following a final reparse point: a symbolic link's tag makes
    /// it `S_IFLNK`, as CPython reports it. Where that open is refused for
    /// access or sharing, CPython reads the directory entry instead, with no
    /// device or file number and one link; so does this.
    pub fn path_stat(path: &str) -> std::io::Result<Stat> {
        const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
        let handle = match create(
            path,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        ) {
            Ok(handle) => handle,
            Err(error)
                if matches!(
                    error.raw_os_error().map(|code| code as u32),
                    Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                ) =>
            {
                return entry_stat(path);
            }
            Err(error) => return Err(error),
        };
        let mut stat = handle_stat(handle.as_raw_handle())?;
        if stat.reparse && reparse_tag(handle.as_raw_handle())? == IO_REPARSE_TAG_SYMLINK {
            stat.mode = S_IFLNK | (stat.mode & 0o777);
        }
        Ok(stat)
    }

    /// CPython's directory-entry `lstat` fallback.
    fn entry_stat(path: &str) -> std::io::Result<Stat> {
        use std::os::windows::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path)?;
        let attributes = meta.file_attributes();
        let mut mode = if meta.file_type().is_symlink() {
            S_IFLNK
        } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            S_IFDIR | 0o111
        } else {
            S_IFREG
        };
        mode |= if attributes & FILE_ATTRIBUTE_READONLY != 0 {
            0o444
        } else {
            0o666
        };
        Ok(Stat {
            mode,
            dev: 0,
            ino: 0,
            nlink: 1,
            size: meta.file_size(),
            reparse: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        })
    }

    /// `CreateFileW(path, access, share, OPEN_EXISTING, backup semantics,
    /// no reparse following)`.
    pub fn create(path: &str, access: u32, share: u32) -> std::io::Result<OwnedHandle> {
        let name = wide(path);
        // SAFETY: the name is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                share,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateFileW returned a new handle this function now owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    /// `NtOpenFile` of `name` relative to the open directory `parent`,
    /// never following a reparse point.
    pub fn open_child(
        parent: RawHandle,
        name: &str,
        options: u32,
        access: u32,
    ) -> Option<OwnedHandle> {
        let buffer: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let length = u16::try_from((buffer.len() - 1) * 2).ok()?;
        let unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: length + 2,
            Buffer: buffer.as_ptr() as *mut u16,
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent as HANDLE,
            ObjectName: &unicode,
            Attributes: 0,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        // SAFETY: an all-zero IO_STATUS_BLOCK is a valid value to be overwritten.
        let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: every pointer refers to a live local for the call.
        let result = unsafe {
            NtOpenFile(
                &mut handle,
                access,
                &attributes,
                &mut status,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            )
        };
        if result != 0 || handle.is_null() {
            return None;
        }
        // SAFETY: NtOpenFile returned a new handle this function now owns.
        Some(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    /// `CloseHandle(handle)`, its failure reported.
    pub fn close(handle: OwnedHandle) -> bool {
        let raw = std::os::windows::io::IntoRawHandle::into_raw_handle(handle);
        // SAFETY: the handle was owned and is closed exactly once here.
        unsafe { CloseHandle(raw as HANDLE) != 0 }
    }

    /// `SetFileInformationByHandle(handle, FileDispositionInfo, delete)`:
    /// the held object is removed when its last handle closes.
    pub fn mark_for_deletion(handle: RawHandle) -> bool {
        let information = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: the handle is open and the structure outlives the call.
        unsafe {
            SetFileInformationByHandle(
                handle as HANDLE,
                FileDispositionInfo,
                (&raw const information).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            ) != 0
        }
    }

    /// `NtSetInformationFile(staging, FileRenameInformation)` with the held
    /// parent as the root directory and `ReplaceIfExists` false.
    pub fn rename_no_replace(staging: RawHandle, parent: RawHandle, name: &str) -> bool {
        use std::mem::{offset_of, size_of};
        use windows_sys::Wdk::Storage::FileSystem::FILE_RENAME_INFORMATION;
        let encoded: Vec<u16> = name.encode_utf16().collect();
        // A zeroed FILE_RENAME_INFORMATION (ReplaceIfExists false) followed by
        // the name, in a buffer the size of the structure plus the name, as
        // the Python ctypes code builds it; offsets come from the structure.
        let root = offset_of!(FILE_RENAME_INFORMATION, RootDirectory);
        let length = offset_of!(FILE_RENAME_INFORMATION, FileNameLength);
        let start = offset_of!(FILE_RENAME_INFORMATION, FileName);
        let size = size_of::<FILE_RENAME_INFORMATION>() + encoded.len() * 2;
        let mut buffer = vec![0u64; size.div_ceil(8)];
        // SAFETY: the buffer holds `size` bytes, 8-byte aligned.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), size) };
        bytes[root..root + size_of::<HANDLE>()]
            .copy_from_slice(&(parent as usize).to_le_bytes()[..size_of::<HANDLE>()]);
        bytes[length..length + 4].copy_from_slice(&((encoded.len() * 2) as u32).to_le_bytes());
        for (i, unit) in encoded.iter().enumerate() {
            bytes[start + 2 * i..start + 2 * i + 2].copy_from_slice(&unit.to_le_bytes());
        }
        // SAFETY: an all-zero IO_STATUS_BLOCK is a valid value to be overwritten.
        let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open and the buffer outlives the call.
        let result = unsafe {
            NtSetInformationFile(
                staging as HANDLE,
                &mut status,
                buffer.as_ptr().cast(),
                size as u32,
                FileRenameInformation,
            )
        };
        result == 0
    }
}

/// `os.lstat(path)`.
fn path_stat(path: &str) -> std::io::Result<Stat> {
    #[cfg(windows)]
    {
        win::path_stat(path)
    }
    #[cfg(unix)]
    {
        Ok(stat_of(&std::fs::symlink_metadata(path)?))
    }
}

/// `os.fstat(file)`.
fn file_stat(file: &std::fs::File) -> std::io::Result<Stat> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        win::handle_stat(file.as_raw_handle())
    }
    #[cfg(unix)]
    {
        Ok(stat_of(&file.metadata()?))
    }
}

/// Not a link, junction or other reparse point, by path.
fn plain(path: &str, details: &Stat) -> bool {
    !pyos::islink(path) && !pyos::isjunction(path) && !details.reparse
}

// --------------------------------------------------------- the snapshot

/// A directory of the pinned chain: a Windows handle or a POSIX descriptor.
enum Pinned {
    #[cfg(windows)]
    Handle(std::os::windows::io::OwnedHandle),
    #[cfg(unix)]
    Fd(std::os::fd::OwnedFd),
}

impl Pinned {
    #[cfg(windows)]
    fn raw(&self) -> std::os::windows::io::RawHandle {
        use std::os::windows::io::AsRawHandle;
        let Pinned::Handle(handle) = self;
        handle.as_raw_handle()
    }

    #[cfg(unix)]
    fn fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        let Pinned::Fd(fd) = self;
        fd.as_raw_fd()
    }
}

/// `_PinnedDirectoryChain`: source directories held while their recognised
/// children are copied once.
struct PinnedChain {
    held: Vec<(String, Pinned)>,
}

#[cfg(unix)]
fn cstring(text: &str) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(std::ffi::OsStr::new(text).as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

/// `os.open(name, flags, dir_fd=parent)` or, without a parent, of the path.
#[cfg(unix)]
fn open_at(
    parent: Option<std::os::fd::RawFd>,
    name: &str,
    flags: libc::c_int,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let name = cstring(name)?;
    let flags = flags | libc::O_CLOEXEC;
    // SAFETY: the name is NUL-terminated; the result is a new descriptor or -1.
    let fd = unsafe {
        match parent {
            Some(dir) => libc::openat(dir, name.as_ptr(), flags),
            None => libc::open(name.as_ptr(), flags),
        }
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor is new and owned here.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// `os.stat(name, dir_fd=parent, follow_symlinks=False)`.
#[cfg(unix)]
fn stat_at(parent: std::os::fd::RawFd, name: &str) -> std::io::Result<Stat> {
    let name = cstring(name)?;
    // SAFETY: an all-zero stat is a valid value to be overwritten.
    let mut details: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the name is NUL-terminated and details is writable.
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            &mut details,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(raw_stat(&details))
}

/// `os.fstat(fd)` on a held descriptor, without duplicating it.
#[cfg(unix)]
fn fstat_fd(fd: std::os::fd::RawFd) -> std::io::Result<Stat> {
    // SAFETY: an all-zero stat is a valid value to be overwritten.
    let mut details: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is open and details is writable.
    if unsafe { libc::fstat(fd, &mut details) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(raw_stat(&details))
}

#[cfg(unix)]
fn raw_stat(details: &libc::stat) -> Stat {
    Stat {
        mode: details.st_mode,
        dev: details.st_dev,
        ino: details.st_ino,
        nlink: details.st_nlink,
        size: details.st_size as u64,
        reparse: false,
    }
}

/// This thread's `errno`.
#[cfg(target_os = "linux")]
fn errno() -> *mut libc::c_int {
    // SAFETY: always valid for the calling thread.
    unsafe { libc::__errno_location() }
}

/// This thread's `errno`.
#[cfg(all(unix, not(target_os = "linux")))]
fn errno() -> *mut libc::c_int {
    // SAFETY: always valid for the calling thread.
    unsafe { libc::__error() }
}

/// `os.listdir(fd)`.
#[cfg(unix)]
fn list_fd(fd: std::os::fd::RawFd) -> std::io::Result<Vec<String>> {
    // SAFETY: dup returns a new descriptor that fdopendir takes over.
    let copy = unsafe { libc::dup(fd) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `copy` is an open directory descriptor owned by the stream.
    let stream = unsafe { libc::fdopendir(copy) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `copy` is still ours to close.
        unsafe { libc::close(copy) };
        return Err(error);
    }
    // SAFETY: the stream is open.
    unsafe { libc::rewinddir(stream) };
    let mut names = Vec::new();
    loop {
        // readdir returns null at the end and on an error; only errno tells
        // them apart, so it is cleared first, as CPython's listdir does.
        // SAFETY: errno is this thread's; the stream is open.
        let entry = unsafe {
            *errno() = 0;
            libc::readdir(stream)
        };
        if entry.is_null() {
            // SAFETY: errno is this thread's.
            let code = unsafe { *errno() };
            if code != 0 {
                // SAFETY: the stream is closed once.
                unsafe { libc::closedir(stream) };
                return Err(std::io::Error::from_raw_os_error(code));
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated within the entry.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = String::from_utf8_lossy(name.to_bytes()).into_owned();
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    // SAFETY: the stream is closed once.
    unsafe { libc::closedir(stream) };
    Ok(names)
}

impl PinnedChain {
    fn new() -> PinnedChain {
        PinnedChain { held: Vec::new() }
    }

    /// `pin(path, label, parent_handle=...)`.
    fn pin(&mut self, path: &PyPath, label: &str, parent: Option<usize>) -> Result<usize, Exc> {
        let text = path.display();
        if injected(Step::Pin(&text)).is_some() {
            return Err(live_error(format!("{label} could not be pinned.")));
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            let handle = match parent {
                None => win::create(
                    &text,
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                )
                .map_err(|_| live_error(format!("{label} could not be pinned.")))?,
                Some(index) => open_child(
                    self.held[index].1.raw(),
                    path.name(),
                    win::FILE_DIRECTORY_FILE,
                    win::READ,
                )?,
            };
            let details = win::handle_stat(handle.as_raw_handle())
                .map_err(|_| live_error(format!("{label} could not be pinned.")))?;
            if !details.is_dir() || details.reparse {
                return Err(live_error(format!(
                    "{label} must be an ordinary directory."
                )));
            }
            self.held.push((text, Pinned::Handle(handle)));
            Ok(self.held.len() - 1)
        }
        #[cfg(unix)]
        {
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
            let fd = match parent {
                None => open_at(None, &text, flags),
                Some(index) => open_at(Some(self.held[index].1.fd()), path.name(), flags),
            }
            .map_err(|_| live_error(format!("{label} could not be pinned.")))?;
            self.held.push((text, Pinned::Fd(fd)));
            Ok(self.held.len() - 1)
        }
    }

    /// `__exit__`: close every held directory, last first.
    fn close(self) -> Result<(), Exc> {
        let mut failed = false;
        for (path, pinned) in self.held.into_iter().rev() {
            let forced = injected(Step::Close(&path)).is_some();
            #[cfg(windows)]
            {
                let Pinned::Handle(handle) = pinned;
                failed |= !win::close(handle) || forced;
            }
            #[cfg(unix)]
            {
                drop(pinned);
                failed |= forced;
            }
            if failed {
                return Err(live_error("capture snapshot cleanup failed."));
            }
        }
        Ok(())
    }
}

/// `_open_pinned_child(directory_handle, name, options)`.
#[cfg(windows)]
fn open_child(
    parent: std::os::windows::io::RawHandle,
    name: &str,
    options: u32,
    access: u32,
) -> Result<std::os::windows::io::OwnedHandle, Exc> {
    if injected(Step::OpenChild(name)).is_some() {
        return Err(live_error(
            "capture snapshot source file could not be opened.",
        ));
    }
    win::open_child(parent, name, options, access)
        .ok_or_else(|| live_error("capture snapshot source file could not be opened."))
}

/// `_pinned_directory_details(path, handle)`.
fn pinned_directory_details(chain: &PinnedChain, path: &str, index: usize) -> Result<Stat, Exc> {
    let changed = || live_error("capture input inventory changed.");
    #[cfg(unix)]
    let details = {
        let _ = path;
        fstat_fd(chain.held[index].1.fd()).map_err(|_| changed())?
    };
    #[cfg(windows)]
    let details = {
        let _ = (chain, index);
        path_stat(path).map_err(|_| changed())?
    };
    if !details.is_dir() {
        return Err(changed());
    }
    Ok(details)
}

/// `_pinned_path_identity_matches(path, expected)`.
fn identity_matches(path: &str, expected: &Stat) -> bool {
    path_stat(path)
        .is_ok_and(|observed| same_directory(expected, &observed) && plain(path, &observed))
}

/// `_child_details(source_path, directory_handle=...)`.
fn child_details(chain: &PinnedChain, path: &PyPath, index: usize) -> Result<Stat, Exc> {
    let unavailable = || live_error("capture snapshot source file is unavailable.");
    #[cfg(unix)]
    {
        stat_at(chain.held[index].1.fd(), path.name()).map_err(|_| unavailable())
    }
    #[cfg(windows)]
    {
        let _ = (chain, index);
        path_stat(&path.display()).map_err(|_| unavailable())
    }
}

/// `_directory_inventory(path, directory_handle=...)`: each entry's mode,
/// device and file number.
fn directory_inventory(
    chain: &PinnedChain,
    path: &str,
    index: usize,
) -> Result<BTreeMap<String, (u32, u64, u64)>, Exc> {
    let changed = || live_error("capture input inventory changed.");
    io_fault(Step::List(path)).map_err(|_| changed())?;
    let mut inventory = BTreeMap::new();
    #[cfg(unix)]
    {
        let fd = chain.held[index].1.fd();
        for name in list_fd(fd).map_err(|_| changed())? {
            let details = stat_at(fd, &name).map_err(|_| changed())?;
            inventory.insert(name, (details.mode, details.dev, details.ino));
        }
    }
    #[cfg(windows)]
    {
        let _ = (chain, index);
        for entry in std::fs::read_dir(path).map_err(|_| changed())? {
            let entry = entry.map_err(|_| changed())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let details = path_stat(&pypath::join(path, &name)).map_err(|_| changed())?;
            inventory.insert(name, (details.mode, details.dev, details.ino));
        }
    }
    Ok(inventory)
}

/// `_ordinary_file_details(path, directory_handle=...)`: a regular file with
/// one link, and on Windows no link, junction or reparse point.
fn ordinary_file_details(chain: &PinnedChain, path: &PyPath, index: usize) -> Result<Stat, Exc> {
    injected(Step::Identify(&path.display()));
    let expected = child_details(chain, path, index)?;
    let windows_refusal = cfg!(windows) && !plain(&path.display(), &expected);
    if !expected.is_reg() || windows_refusal || expected.nlink != 1 {
        return Err(live_error("capture snapshot source file is not ordinary."));
    }
    Ok(expected)
}

/// `_copy_regular_file(source, target, directory_handle=..., expected=...)`.
fn copy_regular_file(
    chain: &PinnedChain,
    source: &PyPath,
    target: &str,
    index: usize,
    expected: Stat,
) -> Result<Stat, Exc> {
    let changed = || live_error("capture snapshot source file changed.");
    let failed = || live_error("capture snapshot source file could not be copied.");
    let name = source.name();
    #[cfg(windows)]
    let file = std::fs::File::from(open_child(
        chain.held[index].1.raw(),
        name,
        win::FILE_NON_DIRECTORY_FILE,
        win::READ,
    )?);
    #[cfg(unix)]
    let file = std::fs::File::from(
        open_at(
            Some(chain.held[index].1.fd()),
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW,
        )
        .map_err(|_| failed())?,
    );
    let copied = (|| -> std::io::Result<Result<Stat, Exc>> {
        let mut file = file;
        let mut out = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(target)?;
        io_fault(Step::Copy(&source.display()))?;
        let opened = file_stat(&file)?;
        if !same_regular(&expected, &opened) {
            return Ok(Err(changed()));
        }
        let mut block = vec![0u8; 64 * 1024];
        loop {
            let read = file.read(&mut block)?;
            if read == 0 {
                break;
            }
            out.write_all(&block[..read])?;
        }
        Ok(Ok(file_stat(&file)?))
    })()
    .map_err(|_| failed())??;
    let read_details = copied;
    let finished = child_details(chain, source, index)?;
    if !(same_regular(&expected, &read_details) && same_regular(&read_details, &finished)) {
        return Err(changed());
    }
    Ok(expected)
}

fn evidence_name(name: &str) -> bool {
    name.len() == 76
        && name.starts_with("sha256-")
        && name.ends_with(".json")
        && name[7..71]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `_snapshot_capture_graph(capture_dir, snapshot)`: copy the recognised
/// files once through the held directory chain into `snapshot`.
fn snapshot_capture_graph(capture_dir: &str, snapshot: &str) -> Result<(), Exc> {
    let source = PyPath::new(&pypath::abspath(capture_dir));
    let mut chain_paths = vec![source.clone()];
    while !chain_paths
        .last()
        .expect("the source")
        .parent()
        .same(chain_paths.last().expect("the source"))
    {
        chain_paths.push(chain_paths.last().expect("the source").parent());
    }
    chain_paths.reverse();
    let mut pinned = PinnedChain::new();
    let body = snapshot_into(&mut pinned, &source, &chain_paths, snapshot);
    // __exit__ runs either way; its failure replaces the body's outcome.
    pinned.close()?;
    body
}

fn snapshot_into(
    pinned: &mut PinnedChain,
    source: &PyPath,
    chain_paths: &[PyPath],
    snapshot: &str,
) -> Result<(), Exc> {
    let mut parent = None;
    let mut indexes = Vec::new();
    for directory in chain_paths {
        let index = pinned.pin(directory, "capture input", parent)?;
        indexes.push(index);
        parent = Some(index);
    }
    let source_index = *indexes.last().expect("the source");
    let mut pinned_directories: Vec<(String, Stat)> = Vec::new();
    for (directory, &index) in chain_paths.iter().zip(&indexes) {
        let text = directory.display();
        let details = pinned_directory_details(pinned, &text, index)?;
        pinned_directories.push((text, details));
    }
    let source_text = source.display();
    let root_inventory = directory_inventory(pinned, &source_text, source_index)?;
    let layout: Vec<&str> = root_inventory.keys().map(String::as_str).collect();
    if layout
        != [
            "evidence",
            "monitor-baseline.json",
            "register-capture.json",
            "register-observation.json",
        ]
    {
        return Err(live_error("capture input layout is invalid."));
    }
    let source_evidence = source.join("evidence");
    let evidence_index = pinned.pin(&source_evidence, "capture evidence", Some(source_index))?;
    let evidence_text = source_evidence.display();
    let details = pinned_directory_details(pinned, &evidence_text, evidence_index)?;
    pinned_directories.push((evidence_text.clone(), details));
    if !pinned_directories
        .iter()
        .all(|(path, expected)| identity_matches(path, expected))
    {
        return Err(live_error("capture input inventory changed."));
    }
    let evidence_inventory = directory_inventory(pinned, &evidence_text, evidence_index)?;
    if !evidence_inventory.keys().all(|name| evidence_name(name)) {
        return Err(live_error("capture evidence layout is invalid."));
    }
    let created = io_fault(Step::Snapshot(snapshot))
        .and_then(|()| pyos::mkdir_private(snapshot))
        .and_then(|()| pyos::mkdir_private(&pypath::join(snapshot, "evidence")));
    created.map_err(|_| live_error("capture snapshot could not be created."))?;
    let mut copied: Vec<(PyPath, usize, Stat)> = Vec::new();
    for name in [
        "monitor-baseline.json",
        "register-capture.json",
        "register-observation.json",
    ] {
        let path = source.join(name);
        let expected = ordinary_file_details(pinned, &path, source_index)?;
        let stat = copy_regular_file(
            pinned,
            &path,
            &pypath::join(snapshot, name),
            source_index,
            expected,
        )?;
        copied.push((path, source_index, stat));
    }
    let evidence_snapshot = pypath::join(snapshot, "evidence");
    for name in evidence_inventory.keys() {
        let path = source_evidence.join(name);
        let expected = ordinary_file_details(pinned, &path, evidence_index)?;
        let stat = copy_regular_file(
            pinned,
            &path,
            &pypath::join(&evidence_snapshot, name),
            evidence_index,
            expected,
        )?;
        copied.push((path, evidence_index, stat));
    }
    injected(Step::Copied(&source_text));
    if directory_inventory(pinned, &source_text, source_index)? != root_inventory
        || directory_inventory(pinned, &evidence_text, evidence_index)? != evidence_inventory
        || !pinned_directories
            .iter()
            .all(|(path, expected)| identity_matches(path, expected))
    {
        return Err(live_error("capture input inventory changed."));
    }
    for (path, index, expected) in &copied {
        // The helper's own refusal stands: Python's `except OSError` here
        // never sees it.
        let finished = child_details(pinned, path, *index)?;
        if !same_regular(expected, &finished) {
            return Err(live_error("capture snapshot source file changed."));
        }
    }
    Ok(())
}

// ----------------------------------------------------------- candidates

/// `_json_bytes(value)`: `json.dumps(ensure_ascii=False, indent=2,
/// allow_nan=False)` and a newline, as UTF-8.
fn json_bytes(value: &Json) -> Result<Vec<u8>, Exc> {
    fn non_finite(value: &Json) -> Option<f64> {
        match value {
            Json::Float(f) if !f.is_finite() => Some(*f),
            Json::Array(items) => items.iter().find_map(non_finite),
            Json::Object(pairs) => pairs.iter().find_map(|(_, v)| non_finite(v)),
            _ => None,
        }
    }
    if let Some(value) = non_finite(value) {
        return Err(Exc::value(format!(
            "Out of range float values are not JSON compliant: {}",
            crate::py::float_repr(value)
        )));
    }
    super::publication_bundles::bundle_bytes(value)
}

/// `_read_json(path, label)`: the bytes and the object.
fn read_json(path: &str, label: &str) -> Result<(Vec<u8>, Json), Exc> {
    let unreadable = || live_error(format!("{label} could not be read as UTF-8 JSON."));
    let content = std::fs::read(path).map_err(|_| unreadable())?;
    let text = std::str::from_utf8(&content).map_err(|_| unreadable())?;
    let value = pyjson::loads(text).map_err(|error| match error {
        DecodeError::Duplicate => unreadable(),
        DecodeError::Invalid(message) if !message.starts_with("Exceeds the limit") => unreadable(),
        other => super::json_error(other),
    })?;
    if !matches!(value, Json::Object(_)) {
        return Err(live_error(format!("{label} must be an object.")));
    }
    Ok((content, value))
}

fn required_object<'a>(value: &'a Json, label: &str) -> Result<&'a Json, Exc> {
    if !matches!(value, Json::Object(_)) {
        return Err(live_error(format!("{label} must be an object.")));
    }
    Ok(value)
}

/// `_required_text(value, label)`: non-empty text equal to its own strip.
fn required_text(value: &Json, label: &str) -> Result<P, Exc> {
    let refused = || live_error(format!("{label} must be non-empty text."));
    let Json::Str(s) = value else {
        return Err(refused());
    };
    let text = s.code_points();
    let space = |cp: &u32| char::from_u32(*cp).is_some_and(crate::py::is_space);
    if text.is_empty() || text.first().is_some_and(space) || text.last().is_some_and(space) {
        return Err(refused());
    }
    Ok(text)
}

fn required_sha256(value: &Json, label: &str) -> Result<String, Exc> {
    let text = required_text(value, label)?;
    if !super::monitor_contract::sha256_id(&text) {
        return Err(live_error(format!(
            "{label} must be a lowercase SHA-256 identifier."
        )));
    }
    Ok(pyvalue::points_lossy(&text))
}

/// `_register_id(value, label)`.
fn register_id(value: &Json, label: &str) -> Result<String, Exc> {
    let invalid = || live_error(format!("{label} is invalid."));
    required_text(value, label).map_err(|_| invalid())?;
    corpus_paths::register_id(value).map_err(|_| invalid())
}

/// `_SEMVER.fullmatch(version)`, with `re`'s Unicode `\d`.
fn semver(text: &[u32]) -> bool {
    let digit = |cp: u32| iso::decimal(cp);
    let ascii_nonzero = |cp: u32| (u32::from('1')..=u32::from('9')).contains(&cp);
    let number = |part: &[u32]| {
        part == [u32::from('0')]
            || (!part.is_empty() && ascii_nonzero(part[0]) && part[1..].iter().all(|&cp| digit(cp)))
    };
    let identifier = |part: &[u32]| {
        !part.is_empty()
            && part.iter().all(|&cp| {
                char::from_u32(cp).is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            })
    };
    let split = |text: &[u32], at: char| -> (Vec<u32>, Option<Vec<u32>>) {
        match text.iter().position(|&cp| cp == u32::from(at)) {
            Some(i) => (text[..i].to_vec(), Some(text[i + 1..].to_vec())),
            None => (text.to_vec(), None),
        }
    };
    let (rest, build) = split(text, '+');
    let (core, pre) = split(&rest, '-');
    let dotted_ok = |part: &[u32], check: &dyn Fn(&[u32]) -> bool| {
        part.split(|&cp| cp == u32::from('.')).all(check)
    };
    let parts: Vec<&[u32]> = core.split(|&cp| cp == u32::from('.')).collect();
    parts.len() == 3
        && parts.iter().all(|p| number(p))
        && pre.as_deref().is_none_or(|p| dotted_ok(p, &identifier))
        && build.as_deref().is_none_or(|b| dotted_ok(b, &identifier))
}

/// `_producer_version()`: `VERSION`, equal to `pyproject.toml`'s version and
/// strict Semantic Version 2.0.0.
fn producer_version() -> Result<String, Exc> {
    let root = match VERSION_DIR.with(|d| d.borrow().clone()) {
        Some(directory) => PyPath::new(&directory),
        None => corpus_paths::script_dir("export_live_evidence_bundles.py")?.parent(),
    };
    // Read in turn; only OSError becomes the export's refusal, and a decoding
    // error escapes as Python's UnicodeDecodeError does.
    let read = |name: &str| match crate::pyio::read_text(&root.join(name).display()) {
        Ok(text) => Ok(text),
        Err(crate::pyio::ReadError::Os(_)) => {
            Err(live_error("producer version could not be read."))
        }
        Err(error) => Err(Exc::from(error)),
    };
    let version = read("VERSION")?;
    let pyproject = read("pyproject.toml")?;
    let version = version.strip_suffix('\n').unwrap_or(&version);
    let version = version.strip_suffix('\r').unwrap_or(version);
    let package = pyre::re(r#"^version\s*=\s*"([^"]+)"\s*$"#, pyre::M)
        .search(&pyproject)
        .map(|caps| caps[1].to_string());
    if !semver(&pts(version)) || package.as_deref() != Some(version) {
        return Err(live_error(
            "producer version must be strict Semantic Version 2.0.0.",
        ));
    }
    Ok(version.to_string())
}

/// `_candidate_identity_pair(identity)`.
fn candidate_pair(identity: &str) -> Result<(String, String), Exc> {
    let invalid = || live_error("candidate identity is invalid.");
    let inner = identity
        .strip_prefix("bundle-frl-")
        .and_then(|s| s.strip_suffix("-r1"))
        .ok_or_else(invalid)?;
    let lower_id = |s: &str| {
        let b = s.as_bytes();
        b.len() == 11
            && b[0].is_ascii_lowercase()
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[5].is_ascii_lowercase()
            && b[6..].iter().all(u8::is_ascii_digit)
    };
    let (first, second) = inner.split_once('-').ok_or_else(invalid)?;
    if !lower_id(first) || !lower_id(second) {
        return Err(invalid());
    }
    Ok((
        register_id(
            &Json::str(&first.to_uppercase()),
            "candidate baseline identifier",
        )?,
        register_id(
            &Json::str(&second.to_uppercase()),
            "candidate document identifier",
        )?,
    ))
}

/// `_rights(checked_at)`.
fn rights(checked_at: &Json) -> Result<Json, Exc> {
    let text = required_text(checked_at, "observation checked_at")?;
    let date = pyvalue::points_lossy(&text[..text.len().min(10)]);
    Ok(Json::Object(vec![
        (PyStr::Text("mode".into()), Json::str("metadata-only")),
        (
            PyStr::Text("attribution".into()),
            Json::str(&format!(
                "Based on content from the Federal Register of Legislation at {date}. For the latest information on Australian Government legislation please go to https://www.legislation.gov.au. Changes: selected and reformatted Federal Register metadata into a bounded evidence bundle and factual source update; no legislation text is reproduced."
            )),
        ),
        (
            PyStr::Text("licence_url".into()),
            Json::str("https://creativecommons.org/licenses/by/4.0/"),
        ),
    ]))
}

/// `_project_members(value, fields, label)`.
fn project_members(value: &Json, fields: &[&str], label: &str) -> Result<Json, Exc> {
    let mut out = Vec::new();
    for field in fields {
        let member = value
            .get(field)
            .ok_or_else(|| live_error(format!("{label} is missing {field}.")))?;
        out.push((PyStr::Text((*field).into()), member.clone()));
    }
    Ok(Json::Object(out))
}

fn set(value: &mut Json, key: &str, member: Json) {
    if let Json::Object(pairs) = value
        && let Some(slot) = pairs.iter_mut().find(|(k, _)| k.as_str() == Some(key))
    {
        slot.1 = member;
    }
}

/// `_project_capture_result(value)`.
fn project_capture_result(value: &Json) -> Result<Json, Exc> {
    let mut result = project_members(value, &CAPTURE_RESULT_FIELDS, "capture result")?;
    let Json::Array(requests) = get(&result, "requests") else {
        return Err(live_error("capture result requests must be an array."));
    };
    let projected = requests
        .iter()
        .map(|request| {
            project_members(
                required_object(request, "capture request")?,
                &REQUEST_FIELDS,
                "capture request",
            )
        })
        .collect::<Result<Vec<_>, Exc>>()?;
    set(&mut result, "requests", Json::Array(projected));
    Ok(result)
}

/// `_read_response(path)`.
fn read_response(path: &str) -> Result<Vec<u8>, Exc> {
    let mut response = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| {
            f.take(MAX_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut response)
        })
        .map_err(|_| live_error("retained response could not be read."))?;
    if response.len() > MAX_RESPONSE_BYTES {
        return Err(live_error("retained response exceeds the 256 KiB limit."));
    }
    Ok(response)
}

/// `_version_timestamp(value, label)`.
fn version_timestamp(value: &Json, label: &str) -> Result<(), Exc> {
    let text = required_text(value, label)?;
    iso::instant(&text, 7, Zone::Any).ok_or_else(|| live_error(format!("{label} is invalid.")))?;
    Ok(())
}

/// `_raw_current_row(response, register_id)`.
fn raw_current_row(response: &[u8], register: &str) -> Result<Json, Exc> {
    let ambiguous = || live_error("retained response is not unambiguous OData JSON.");
    let text = std::str::from_utf8(response).map_err(|_| ambiguous())?;
    let document = pyjson::loads(text).map_err(|error| match error {
        DecodeError::Duplicate => ambiguous(),
        DecodeError::Invalid(message) if !message.starts_with("Exceeds the limit") => ambiguous(),
        other => super::json_error(other),
    })?;
    let one_row = matches!(get(&document, "value"), Json::Array(rows) if rows.len() == 1);
    if !document.has_exactly(&["@odata.context", "value"])
        || !is(get(&document, "@odata.context"), &odata_context())
        || !one_row
    {
        return Err(live_error("retained response OData shape is invalid."));
    }
    let Json::Array(rows) = get(&document, "value") else {
        unreachable!("checked above")
    };
    let row = rows[0].clone();
    if !row.has_exactly(&ROW_FIELDS) {
        return Err(live_error("retained response OData row is invalid."));
    }
    if !is(get(&row, "titleId"), register)
        || !matches!(get(&row, "isCurrent"), Json::Bool(true))
        || !is(get(&row, "status"), "InForce")
    {
        return Err(live_error(
            "retained response is not the declared current row.",
        ));
    }
    if matches!(get(&row, "registerId"), Json::Null) {
        return Err(live_error(
            "retained response current document identifier is missing.",
        ));
    }
    register_id(get(&row, "registerId"), "raw current document identifier")?;
    required_text(
        get(&row, "compilationNumber"),
        "raw current compilation number",
    )?;
    version_timestamp(get(&row, "start"), "raw current compilation start")?;
    version_timestamp(
        get(&row, "registeredAt"),
        "raw current registration timestamp",
    )?;
    Ok(row)
}

/// The run facts every candidate shares.
struct Run4 {
    version: String,
    baseline_sha256: String,
    observation_sha256: String,
    observed_at: Json,
}

/// `_candidate_bundle(title, result, observation, ...)`.
fn candidate_bundle(
    title: &Json,
    result: &Json,
    observation: &Json,
    run: &Run4,
    response: &[u8],
) -> Result<(String, Json), Exc> {
    let baseline_title = project_members(title, &BASELINE_TITLE_FIELDS, "baseline title")?;
    let capture_result = project_capture_result(result)?;
    let bundle_observation = project_members(observation, &OBSERVATION_FIELDS, "observation")?;
    let register = register_id(
        get(&baseline_title, "register_id"),
        "baseline title register_id",
    )?;
    let document = register_id(
        get(&bundle_observation, "observed_register_document_id"),
        "observed register document id",
    )?;
    let requests = match get(&capture_result, "requests") {
        Json::Array(items) if items.len() == 1 => items,
        _ => {
            return Err(live_error(
                "SUPERSEDED capture result must have one request.",
            ));
        }
    };
    let request = required_object(&requests[0], "capture request")?;
    let response_sha256 = required_sha256(get(request, "response_sha256"), "response SHA-256")?;
    if sha256_id(response) != response_sha256 {
        return Err(live_error(
            "retained response digest does not match the capture request.",
        ));
    }
    if !py_eq(
        get(request, "response_length"),
        &Json::Int(response.len().into()),
    ) {
        return Err(live_error(
            "retained response length does not match the capture request.",
        ));
    }
    if !is(
        get(&bundle_observation, "primary_response_sha256"),
        &response_sha256,
    ) {
        return Err(live_error(
            "observation response digest does not match the capture request.",
        ));
    }
    if !is(get(&capture_result, "state"), "SUPERSEDED")
        || !is(get(&bundle_observation, "state"), "SUPERSEDED")
    {
        return Err(live_error("candidate state is not SUPERSEDED."));
    }
    if !is(get(request, "role"), "current")
        || !py_eq(get(request, "http_status"), &Json::Int(200.into()))
    {
        return Err(live_error("candidate current request is invalid."));
    }
    if !is(
        get(&bundle_observation, "primary_response_media_type"),
        "application/json",
    ) {
        return Err(live_error(
            "candidate primary response media type is invalid.",
        ));
    }
    let raw_row = raw_current_row(response, &register)?;
    let start = match get(&raw_row, "start") {
        Json::Str(s) => s.code_points(),
        _ => Vec::new(),
    };
    let start_date = Json::Str(PyStr::from_code_points(
        start[..start.len().min(10)].to_vec(),
    ));
    if !py_eq(
        get(&raw_row, "compilationNumber"),
        get(&bundle_observation, "observed_compilation_number"),
    ) || !py_eq(
        &start_date,
        get(&bundle_observation, "observed_compilation_date"),
    ) || !py_eq(
        get(&raw_row, "registerId"),
        get(&bundle_observation, "observed_register_document_id"),
    ) {
        return Err(live_error(
            "retained response does not prove the observed compilation.",
        ));
    }
    if !is(
        get(&bundle_observation, "capture_result_sha256"),
        &sha256_id(&json_bytes(&capture_result)?),
    ) {
        return Err(live_error("observation capture result digest is invalid."));
    }
    let bundle_id = format!(
        "bundle-frl-{}-{}-r1",
        register.to_lowercase(),
        document.to_lowercase()
    );
    candidate_pair(&bundle_id)?;
    let rights = rights(get(&bundle_observation, "checked_at"))?;
    let object = |pairs: Vec<(&str, Json)>| {
        Json::Object(
            pairs
                .into_iter()
                .map(|(k, v)| (PyStr::Text(k.into()), v))
                .collect(),
        )
    };
    let bundle = object(vec![
        ("schema_version", Json::str("evidence-bundle.v2")),
        (
            "producer",
            object(vec![
                ("name", Json::str("tax-radar-au")),
                ("version", Json::str(&run.version)),
            ]),
        ),
        (
            "run",
            object(vec![
                ("observed_at", run.observed_at.clone()),
                ("scope_id", Json::str("au-primary-tax-legislation.v4")),
                ("complete", Json::Bool(true)),
                ("run_status", Json::str("VERIFIED")),
                ("baseline_sha256", Json::str(&run.baseline_sha256)),
                ("observation_sha256", Json::str(&run.observation_sha256)),
            ]),
        ),
        ("baseline_title", baseline_title),
        ("capture_result", capture_result),
        ("observation", bundle_observation),
        ("rights", rights),
        ("primary_response_base64", Json::str(&base64(response))),
    ]);
    Ok((bundle_id, bundle))
}

/// `base64.b64encode(data).decode("ascii")`.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `_build_validated_snapshot(capture)`: the release tag and the candidates.
fn build_validated_snapshot(capture: &str) -> Result<(String, Vec<(String, Json)>), Exc> {
    let (baseline_bytes, baseline) =
        read_json(&pypath::join(capture, "monitor-baseline.json"), "baseline")?;
    let (capture_bytes, capture_document) =
        read_json(&pypath::join(capture, "register-capture.json"), "capture")?;
    let (observation_bytes, observation_document) = read_json(
        &pypath::join(capture, "register-observation.json"),
        "observation",
    )?;
    if !is(
        get(&capture_document, "schema_version"),
        "au-tax-register-capture.v1",
    ) || !is(get(&capture_document, "mode"), "live")
        || !is(
            get(&observation_document, "schema_version"),
            "au-tax-register-observation.v4",
        )
        || !is(get(&observation_document, "mode"), "live")
        || !is(
            get(&observation_document, "scope_id"),
            "au-primary-tax-legislation.v4",
        )
        || !matches!(get(&observation_document, "complete"), Json::Bool(true))
        || !is(get(&observation_document, "run_status"), "VERIFIED")
    {
        return Err(live_error(
            "capture must be a verified live Stage 3A graph.",
        ));
    }
    let baseline_sha256 = sha256_id(&baseline_bytes);
    let observation_sha256 = sha256_id(&observation_bytes);
    if !is(get(&capture_document, "baseline_sha256"), &baseline_sha256)
        || !is(
            get(&observation_document, "baseline_sha256"),
            &baseline_sha256,
        )
        || !is(
            get(&observation_document, "capture_sha256"),
            &sha256_id(&capture_bytes),
        )
    {
        return Err(live_error("capture graph digests are inconsistent."));
    }
    let (Json::Array(titles), Json::Array(results), Json::Array(observations)) = (
        get(&baseline, "titles"),
        get(&capture_document, "results"),
        get(&observation_document, "observations"),
    ) else {
        return Err(live_error("capture graph collections must be arrays."));
    };
    let mut titles_by_id: Vec<(String, &Json)> = Vec::new();
    for title in titles {
        let id = register_id(
            get(required_object(title, "baseline title")?, "register_id"),
            "baseline title register_id",
        )?;
        match titles_by_id.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = title,
            None => titles_by_id.push((id, title)),
        }
    }
    let mut results_by_id: Vec<(String, &Json)> = Vec::new();
    for result in results {
        let id = register_id(
            get(required_object(result, "capture result")?, "register_id"),
            "capture result register_id",
        )?;
        match results_by_id.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = result,
            None => results_by_id.push((id, result)),
        }
    }
    let mut candidates: Vec<(String, Json)> = Vec::new();
    for raw in observations {
        let observation = required_object(raw, "observation item")?;
        if !is(get(observation, "state"), "SUPERSEDED") {
            continue;
        }
        let id = register_id(get(observation, "register_id"), "observation register_id")?;
        let missing = || live_error("SUPERSEDED observation is missing its capture data.");
        let title = titles_by_id
            .iter()
            .find(|(k, _)| *k == id)
            .ok_or_else(missing)?
            .1;
        let result = results_by_id
            .iter()
            .find(|(k, _)| *k == id)
            .ok_or_else(missing)?
            .1;
        let title = required_object(title, "baseline title")?;
        let result = required_object(result, "capture result")?;
        let request = match get(result, "requests") {
            Json::Array(items) if !items.is_empty() => {
                required_object(&items[0], "capture request")?
            }
            _ => {
                return Err(live_error("SUPERSEDED capture result must have a request."));
            }
        };
        let digest = required_sha256(get(request, "response_sha256"), "capture response SHA-256")?;
        let expected_path = format!("evidence/sha256-{}.json", &digest[7..]);
        if !is(get(request, "evidence_path"), &expected_path) {
            return Err(live_error("capture evidence path is invalid."));
        }
        let response = read_response(&pypath::join(capture, &expected_path))?;
        let run = Run4 {
            version: producer_version()?,
            baseline_sha256: baseline_sha256.clone(),
            observation_sha256: observation_sha256.clone(),
            observed_at: Json::Str(PyStr::from_code_points(required_text(
                get(&observation_document, "observed_at"),
                "observed_at",
            )?)),
        };
        let candidate = candidate_bundle(title, result, observation, &run, &response)?;
        candidate_pair(&candidate.0)?;
        if candidates.iter().any(|(k, _)| *k == candidate.0) {
            return Err(live_error("candidate identity is duplicated."));
        }
        candidates.push(candidate);
    }
    Ok((
        format!("live-evidence-v2-{}", &observation_sha256[7..]),
        candidates,
    ))
}

// ----------------------------------------------------------- publication

/// `_require_absent_destination(path)`.
fn require_absent_destination(path: &str) -> Result<(), Exc> {
    match pyos::lstat(path) {
        Err(error) if error.not_found() => Ok(()),
        Err(_) => Err(live_error("live evidence output could not be inspected.")),
        Ok(_) => Err(live_error(
            "live evidence output directory must not already exist.",
        )),
    }
}

/// `_ordinary_directory_details(path, label)`.
fn ordinary_directory_details(path: &str, label: &str) -> Result<Stat, Exc> {
    let details =
        path_stat(path).map_err(|_| live_error(format!("{label} could not be inspected.")))?;
    if !details.is_dir() || !plain(path, &details) {
        return Err(live_error(format!(
            "{label} must be an ordinary directory."
        )));
    }
    Ok(details)
}

/// `_require_ordinary_ancestors(path, label)`: every directory up to, not
/// including, the root.
fn require_ordinary_ancestors(path: &PyPath, label: &str) -> Result<(), Exc> {
    let mut current = path.clone();
    while !current.same(&current.parent()) {
        ordinary_directory_details(&current.display(), label)?;
        current = current.parent();
    }
    Ok(())
}

/// `_output_collides_capture(output, capture)`: the output is the capture or
/// inside it, once links are resolved.
fn output_collides_capture(output: &str, capture: &str) -> Result<bool, Exc> {
    let output = pypath::normcase(&pypath::realpath(output)?);
    let capture = pypath::normcase(&pypath::realpath(capture)?);
    let parts = |p: &str| -> Vec<String> {
        let (drive, root, tail) = pypath::splitroot(p);
        let mut parts = vec![format!("{drive}{root}")];
        parts.extend(
            // posixpath splits on `/` alone; ntpath on both separators.
            tail.split(|c| c == '/' || (cfg!(windows) && c == '\\'))
                .filter(|s| !s.is_empty() && *s != ".")
                .map(str::to_string),
        );
        parts
    };
    let (o, c) = (parts(&output), parts(&capture));
    // commonpath raises ValueError for different drives or root kinds.
    if o[0] != c[0] {
        return Ok(false);
    }
    Ok(o.len() >= c.len() && o[..c.len()] == c[..])
}

fn safe_output_leaf(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 100
        && name.is_ascii()
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `_preflight_output(output)`: whether the one missing parent must be
/// created, and the existing parent's identity.
fn preflight_output(output: &PyPath) -> Result<(bool, Option<Stat>), Exc> {
    if !safe_output_leaf(output.name()) {
        return Err(live_error("live evidence output name is unsafe."));
    }
    require_absent_destination(&output.display())?;
    let parent = output.parent();
    let parent_text = parent.display();
    // One inspection: its identity is what publication later compares with.
    match path_stat(&parent_text) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !safe_output_leaf(parent.name()) {
                return Err(live_error("live evidence output parent name is unsafe."));
            }
            require_ordinary_ancestors(&parent.parent(), "live evidence output")
                .map_err(|_| live_error("live evidence permits only one missing output parent."))?;
            Ok((true, None))
        }
        Err(_) => Err(live_error(
            "live evidence output parent could not be inspected.",
        )),
        Ok(details) => {
            injected(Step::Inspected(&parent_text));
            if !details.is_dir() || !plain(&parent_text, &details) {
                return Err(live_error(
                    "live evidence output parent must be an ordinary directory.",
                ));
            }
            require_ordinary_ancestors(&parent.parent(), "live evidence output")?;
            Ok((false, Some(details)))
        }
    }
}

/// `_write_new(path, content)`: a new file holding `content`.
#[cfg(windows)]
fn write_new(path: &str, content: &[u8]) -> std::io::Result<()> {
    io_fault(Step::Write(path))?;
    let mut file = pyos::create_new(path, true).map_err(|e| std::io::Error::other(e.text))?;
    chmod(path, 0o600)?;
    file.write_all(content)
}

/// `os.chmod(path, mode)`.
#[cfg(windows)]
fn chmod(path: &str, mode: u32) -> std::io::Result<()> {
    io_fault(Step::Chmod(path))?;
    pyos::chmod(path, mode)
}

/// `_read_staged_file(path, expected)`.
#[cfg(windows)]
fn read_staged_file(path: &str, expected: &[u8]) -> Result<(), Exc> {
    let unreadable = || live_error("staged live evidence output could not be read.");
    let details = path_stat(path).map_err(|_| unreadable())?;
    if !details.is_reg() || pyos::islink(path) || details.reparse {
        return Err(live_error(
            "staged live evidence output contains a non-ordinary file.",
        ));
    }
    injected(Step::Inspected(path));
    /// One staged file read twice through one handle.
    struct Reread {
        opened: Stat,
        first: Vec<u8>,
        second: Vec<u8>,
        read_details: Stat,
        finished: Stat,
    }
    let read = (|| -> std::io::Result<Reread> {
        let mut file = std::fs::File::open(path)?;
        let opened = file_stat(&file)?;
        let limit = expected.len() as u64 + 1;
        let (mut first, mut second) = (Vec::new(), Vec::new());
        (&mut file).take(limit).read_to_end(&mut first)?;
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0))?;
        (&mut file).take(limit).read_to_end(&mut second)?;
        let read_details = file_stat(&file)?;
        drop(file);
        Ok(Reread {
            opened,
            first,
            second,
            read_details,
            finished: path_stat(path)?,
        })
    })()
    .map_err(|_| unreadable())?;
    let Reread {
        opened,
        first,
        second,
        read_details,
        finished,
    } = read;
    if !(same_regular(&details, &opened)
        && same_regular(&opened, &read_details)
        && same_regular(&read_details, &finished)
        && first == second
        && first == expected
        && first.len() as u64 == read_details.size
        && read_details.size == details.size)
    {
        return Err(live_error(
            "staged live evidence output changed during validation.",
        ));
    }
    Ok(())
}

/// `_validate_staged_export(staging, expected)`.
#[cfg(windows)]
fn validate_staged_export(staging: &str, expected: &[(String, Vec<u8>)]) -> Result<(), Exc> {
    if let Some(error) = injected(Step::Reread(staging)) {
        return Err(live_error(error.text));
    }
    ordinary_directory_details(staging, "staged live evidence output")?;
    let mut names: Vec<String> = std::fs::read_dir(staging)
        .and_then(|entries| {
            entries
                .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect()
        })
        .map_err(|_| live_error("staged live evidence output could not be listed."))?;
    names.sort();
    let mut wanted: Vec<String> = expected.iter().map(|(n, _)| n.clone()).collect();
    wanted.sort();
    if names != wanted {
        return Err(live_error("staged live evidence output layout is invalid."));
    }
    for (name, content) in expected {
        read_staged_file(&pypath::join(staging, name), content)?;
        let parsed = std::str::from_utf8(content)
            .ok()
            .and_then(|text| pyjson::loads(text).ok())
            .ok_or_else(|| live_error("staged live evidence output JSON is invalid."))?;
        if !matches!(parsed, Json::Object(_)) || json_bytes(&parsed)? != *content {
            return Err(live_error(
                "staged live evidence output JSON is non-canonical.",
            ));
        }
    }
    Ok(())
}

/// `_OwnedStaging`: the held output parent and staging directory, and the
/// identities of the files this exporter wrote into it.
#[cfg(windows)]
struct OwnedStaging {
    path: PyPath,
    parent: PyPath,
    prefix: String,
    staging_details: Stat,
    parent_handle: std::os::windows::io::OwnedHandle,
    staging_handle: std::os::windows::io::OwnedHandle,
    files: Vec<(String, Stat)>,
}

/// `_open_windows_locked_directory(path, expected, label, desired_access=...)`:
/// the exact directory, opened so no one else may rename or delete it.
#[cfg(windows)]
fn open_locked_directory(
    path: &str,
    expected: &Stat,
    label: &str,
    access: u32,
) -> Result<std::os::windows::io::OwnedHandle, Exc> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    if injected(Step::Lock(path)).is_some() {
        return Err(live_error(format!("{label} could not be pinned.")));
    }
    let handle = win::create(
        path,
        win::DELETE_ACCESS | access,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
    .map_err(|_| live_error(format!("{label} could not be pinned.")))?;
    let observed = match win::handle_stat(handle.as_raw_handle()) {
        Ok(observed) => observed,
        Err(_) => {
            win::close(handle);
            return Err(live_error(format!("{label} could not be pinned.")));
        }
    };
    if !same_directory(expected, &observed) {
        win::close(handle);
        return Err(live_error(format!("{label} changed while being pinned.")));
    }
    Ok(handle)
}

#[cfg(windows)]
fn close_handle(handle: std::os::windows::io::OwnedHandle, label: &str) -> Result<(), Exc> {
    let forced = injected(Step::Close(label)).is_some();
    if !win::close(handle) || forced {
        return Err(live_error(format!("{label} cleanup failed.")));
    }
    Ok(())
}

#[cfg(windows)]
fn mark_for_deletion(handle: std::os::windows::io::RawHandle, label: &str) -> Result<(), Exc> {
    if injected(Step::Delete(label)).is_some() || !win::mark_for_deletion(handle) {
        return Err(live_error(format!("{label} could not be removed.")));
    }
    Ok(())
}

#[cfg(windows)]
impl OwnedStaging {
    fn pin(
        path: PyPath,
        parent: PyPath,
        prefix: String,
        parent_details: &Stat,
        staging_details: Stat,
    ) -> Result<OwnedStaging, Exc> {
        let parent_handle = open_locked_directory(
            &parent.display(),
            parent_details,
            "live evidence output parent",
            win::ADD_FILE,
        )?;
        let staging_handle = match open_locked_directory(
            &path.display(),
            &staging_details,
            "live evidence staging",
            0,
        ) {
            Ok(handle) => handle,
            Err(error) => {
                close_handle(parent_handle, "live evidence output parent")?;
                return Err(error);
            }
        };
        Ok(OwnedStaging {
            path,
            parent,
            prefix,
            staging_details,
            parent_handle,
            staging_handle,
            files: Vec::new(),
        })
    }

    /// `remember_file(name)`: `_ordinary_file_details`, whose refusals are
    /// the export's own errors rather than an `OSError`.
    fn remember_file(&mut self, name: &str) -> Result<(), Exc> {
        let path = self.path.join(name).display();
        let details = path_stat(&path)
            .map_err(|_| live_error("capture snapshot source file is unavailable."))?;
        if !details.is_reg() || !plain(&path, &details) || details.nlink != 1 {
            return Err(live_error("capture snapshot source file is not ordinary."));
        }
        self.files.push((name.to_string(), details));
        Ok(())
    }

    /// `close()`: both handles, the staging directory's failure first.
    fn close(self) -> Result<(), Exc> {
        let staging = close_handle(self.staging_handle, "live evidence staging");
        let parent = close_handle(self.parent_handle, "live evidence output parent");
        staging.and(parent)
    }

    /// `_open_owned_staging_child(staging, name, expected)`.
    fn open_child(&self, name: &str, expected: &Stat) -> Result<std::fs::File, Exc> {
        use std::os::windows::io::AsRawHandle;
        let changed = || live_error("live evidence staging changed before cleanup.");
        let handle = open_child(
            self.staging_handle.as_raw_handle(),
            name,
            win::FILE_NON_DIRECTORY_FILE,
            win::DELETE_ACCESS,
        )
        .map_err(|_| changed())?;
        let file = std::fs::File::from(handle);
        match file_stat(&file) {
            Ok(observed) if same_regular(expected, &observed) => Ok(file),
            _ => {
                close_handle(file.into(), "live evidence staging child")?;
                Err(changed())
            }
        }
    }

    /// `_cleanup_owned_staging(staging)`: delete only the recorded children
    /// of the exact held staging directory, then the directory itself.
    fn cleanup(&self) -> Result<(), Exc> {
        use std::os::windows::io::AsRawHandle;
        if !self.path.parent().same(&self.parent) || !self.path.name().starts_with(&self.prefix) {
            return Err(live_error(
                "refusing to remove staging outside the live evidence boundary.",
            ));
        }
        let text = self.path.display();
        injected(Step::CleanupStaging(&text));
        let current = match path_stat(&text) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(live_error("live evidence staging could not be inspected.")),
        };
        if !same_directory(&self.staging_details, &current) {
            return Err(live_error("live evidence staging changed before cleanup."));
        }
        let mut names: Vec<String> = std::fs::read_dir(&text)
            .and_then(|entries| {
                entries
                    .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect()
            })
            .map_err(|_| live_error("live evidence staging could not be listed."))?;
        names.sort();
        let mut recorded: Vec<String> = self.files.iter().map(|(n, _)| n.clone()).collect();
        recorded.sort();
        if names != recorded {
            return Err(live_error("live evidence staging changed before cleanup."));
        }
        for (name, expected) in &self.files {
            injected(Step::Cleanup(name));
            let file = self.open_child(name, expected)?;
            let marked = mark_for_deletion(file.as_raw_handle(), "live evidence staging child");
            // finally: a close failure replaces the marking's outcome.
            close_handle(file.into(), "live evidence staging child")?;
            marked?;
        }
        mark_for_deletion(self.staging_handle.as_raw_handle(), "live evidence staging")
    }

    /// `_promote_no_replace(staging, destination, owned)`.
    fn promote(&self, staging: &PyPath, destination: &PyPath) -> Result<(), Exc> {
        use std::os::windows::io::AsRawHandle;
        if !staging.same(&self.path) || !destination.parent().same(&self.parent) {
            return Err(live_error("live evidence promotion boundary is invalid."));
        }
        let failed = || live_error("live evidence output could not be promoted.");
        if injected(Step::Promote(&destination.display())).is_some() {
            return Err(failed());
        }
        if !win::rename_no_replace(
            self.staging_handle.as_raw_handle(),
            self.parent_handle.as_raw_handle(),
            destination.name(),
        ) {
            return Err(failed());
        }
        Ok(())
    }
}

/// `_publish_candidates(output, candidates)`: the official paths written.
fn publish_candidates(
    output: &PyPath,
    mut candidates: Vec<(String, Json)>,
) -> Result<Vec<String>, Exc> {
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let mut expected: Vec<(String, Vec<u8>)> = Vec::new();
    for (bundle_id, bundle) in &candidates {
        let content = json_bytes(bundle)?;
        if content.len() > MAX_BUNDLE_BYTES {
            return Err(live_error("serialised bundle exceeds the 1 MiB limit."));
        }
        expected.push((format!("{bundle_id}.json"), content));
    }
    let (create_parent, known_parent) = preflight_output(output)?;
    if !is_windows() {
        return Err(live_error(
            "identity-bound live evidence publication is supported only on Windows.",
        ));
    }
    #[cfg(windows)]
    {
        publish_on_windows(output, &expected, create_parent, known_parent)?;
        Ok(expected
            .iter()
            .map(|(name, _)| output.join(name).display())
            .collect())
    }
    #[cfg(not(windows))]
    {
        let _ = (create_parent, known_parent);
        unreachable!("is_windows() is false off Windows")
    }
}

#[cfg(windows)]
fn publish_on_windows(
    output: &PyPath,
    expected: &[(String, Vec<u8>)],
    create_parent: bool,
    known_parent: Option<Stat>,
) -> Result<(), Exc> {
    let parent = output.parent();
    let parent_text = parent.display();
    if create_parent {
        io_fault(Step::Mkdir(&parent_text))
            .and_then(|()| pyos::mkdir_private(&parent_text))
            .and_then(|()| chmod(&parent_text, 0o700))
            .map_err(|_| live_error("live evidence output parent could not be created."))?;
    }
    let parent_details = ordinary_directory_details(&parent_text, "live evidence output parent")?;
    if known_parent.is_some_and(|known| !same_directory(&known, &parent_details)) {
        return Err(live_error(
            "live evidence output parent changed before publication.",
        ));
    }
    require_ordinary_ancestors(&parent.parent(), "live evidence output")?;
    let output_text = output.display();
    require_absent_destination(&output_text)?;

    let prefix = format!(".{}.live-evidence-", output.name());
    let staging = parent.join(&format!("{prefix}{}.tmp", pyos::try_random_hex(32)?));
    let staging_text = staging.display();
    let mut owned: Option<OwnedStaging> = None;
    let outcome = (|| -> Result<(), Exc> {
        let not_written = || live_error("live evidence output could not be written.");
        io_fault(Step::Mkdir(&staging_text))
            .and_then(|()| pyos::mkdir_private(&staging_text))
            .map_err(|_| not_written())?;
        let staging_details = ordinary_directory_details(&staging_text, "live evidence staging")?;
        owned = Some(OwnedStaging::pin(
            staging.clone(),
            parent.clone(),
            prefix.clone(),
            &parent_details,
            staging_details,
        )?);
        let held = owned.as_mut().expect("just pinned");
        chmod(&staging_text, 0o700).map_err(|_| not_written())?;
        for (name, content) in expected {
            write_new(&staging.join(name).display(), content).map_err(|_| not_written())?;
            held.remember_file(name)?;
        }
        validate_staged_export(&staging_text, expected)?;
        injected(Step::Validated(&staging_text));
        let current = ordinary_directory_details(&parent_text, "live evidence output parent")?;
        if !same_directory(&parent_details, &current) {
            return Err(live_error(
                "live evidence output parent changed before promotion.",
            ));
        }
        require_absent_destination(&output_text)?;
        held.promote(&staging, output)
    })();
    // finally: cleanup, then close; each failure replaces what came before.
    if let Some(held) = owned {
        let cleanup = held.cleanup();
        let closed = held.close();
        closed?;
        cleanup?;
    }
    outcome
}

/// The exporter's private snapshot directory, as
/// `tempfile.TemporaryDirectory(prefix="tax-radar-live-evidence-")` makes one.
fn temporary_directory() -> std::io::Result<String> {
    let base = std::env::temp_dir();
    let base = base.to_string_lossy();
    for _ in 0..100 {
        let name = format!(
            "tax-radar-live-evidence-{}",
            pyos::try_random_hex(8).map_err(|e| std::io::Error::other(e.text))?
        );
        let path = pypath::join(&base, &name);
        match pyos::mkdir_private(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}

/// What a successful export reports.
#[derive(Debug)]
pub struct LiveEvidenceExport {
    pub release_tag: String,
    pub candidates: Vec<String>,
}

/// `export_live_evidence_bundles(capture_dir, output_dir)`: v2 candidates
/// only from a private, verified Stage 3A snapshot.
pub fn export_live_evidence_bundles(
    capture_dir: &str,
    output_dir: &str,
) -> Result<LiveEvidenceExport, Exc> {
    let source = abspath(capture_dir);
    let output = PyPath::new(&abspath(output_dir));
    if output_collides_capture(&output.display(), &source)? {
        return Err(live_error(
            "live evidence output would replace a capture input.",
        ));
    }
    let cleanup_failed = || live_error("live evidence snapshot cleanup failed.");
    let temporary = temporary_directory().map_err(|_| cleanup_failed())?;
    let snapshot = pypath::join(&temporary, "capture");
    let built = (|| -> Result<(String, Vec<(String, Json)>), Exc> {
        snapshot_capture_graph(&source, &snapshot)?;
        match capture::validate_capture_graph(&snapshot) {
            Ok(()) => {}
            Err(error) if capture_message(&error).is_some() => {
                return Err(live_error(
                    "capture graph is not a verified Stage 3A graph.",
                ));
            }
            Err(error) => return Err(error),
        }
        build_validated_snapshot(&snapshot)
    })();
    let removed =
        io_fault(Step::TempCleanup(&temporary)).and_then(|()| std::fs::remove_dir_all(&temporary));
    let (release_tag, candidates) = match (built, removed) {
        (_, Err(_)) => return Err(cleanup_failed()),
        (Err(Exc::Os(_)), Ok(())) => return Err(cleanup_failed()),
        (outcome, Ok(())) => outcome?,
    };
    let paths = match publish_candidates(&output, candidates) {
        Ok(paths) => paths,
        Err(Exc::Os(_)) => return Err(live_error("live evidence export failed.")),
        Err(error) => return Err(error),
    };
    Ok(LiveEvidenceExport {
        release_tag,
        candidates: paths,
    })
}

fn parser() -> Parser {
    Parser::new("export_live_evidence_bundles")
        .description("Export live evidence-bundle.v2 candidates.")
        .arg(Action::positional("capture_dir"))
        .arg(Action::option("--out").required().metavar("OUTPUT_DIR"))
}

/// `python -m fadden export_live_evidence_bundles capture --out DIR`.
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let parser = parser();
    let args = parser.parse(argv)?;
    let arg = |name: &str| args.str(name).unwrap_or_default();
    match export_live_evidence_bundles(&arg("capture_dir"), &arg("out")) {
        Ok(export) => {
            run.print(&format!("candidate_count={}", export.candidates.len()));
            run.print(&format!("release_tag={}", export.release_tag));
            Ok(0)
        }
        Err(error) => {
            if let Some(message) = live_message(&error) {
                run.eprint(&format!("error: {message}"));
                return Ok(1);
            }
            if let Exc::Os(_) = error {
                run.eprint("error: live evidence export failed");
                return Ok(1);
            }
            Ok(run.uncaught(&error))
        }
    }
}
