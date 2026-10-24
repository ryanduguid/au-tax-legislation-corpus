//! `pyos::chmod`: `os.chmod(path, mode)` with default arguments.
//!
//! On Windows CPython 3.14 and later (`win32_lchmod`) set or clear the read-only attribute
//! of the path itself, so a symbolic link is changed and its target is left alone (checked
//! against CPython 3.14.8 and 3.15.0 on 20 October 2026).
#![cfg(windows)]

use std::os::windows::fs::MetadataExt;
use std::path::Path;

use tax_radar_au::pyos;

const READONLY: u32 = 0x1;

fn readonly(path: &Path) -> bool {
    std::fs::symlink_metadata(path).unwrap().file_attributes() & READONLY != 0
}

/// Creating a symbolic link needs Developer Mode or the privilege. The test fails without
/// it, so a runner that cannot make links cannot pass without exercising them.
fn link(target: &Path, link: &Path, directory: bool) {
    let made = if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    made.unwrap_or_else(|e| {
        panic!("cannot create a symbolic link here ({e}); this test needs one")
    });
}

fn set_readonly(path: &Path, readonly: bool) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(readonly);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn a_link_gets_the_attribute_and_its_target_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("target.txt");
    std::fs::write(&file, "x").unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let file_link = dir.path().join("file-link");
    let folder_link = dir.path().join("folder-link");
    link(&file, &file_link, false);
    link(&folder, &folder_link, true);
    for (link, target) in [(&file_link, &file), (&folder_link, &folder)] {
        pyos::chmod(link.to_str().unwrap(), 0o444).unwrap();
        assert!(readonly(link), "{link:?} should be read-only");
        assert!(!readonly(target), "{target:?} should be untouched");
        // Clearing the link's attribute must leave a read-only target read-only.
        set_readonly(target, true);
        pyos::chmod(link.to_str().unwrap(), 0o644).unwrap();
        assert!(!readonly(link));
        assert!(readonly(target), "{target:?} should still be read-only");
        set_readonly(target, false);
    }
}

#[test]
fn an_embedded_nul_is_refused_before_any_file_changes() {
    // CPython 3.15: ValueError("chmod: embedded null character in path").
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("protected.txt");
    std::fs::write(&file, "x").unwrap();
    let path = format!("{}\0suffix", file.to_str().unwrap());
    let error = pyos::chmod(&path, 0o444).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        !readonly(&file),
        "the file the prefix names must be untouched"
    );
}

#[test]
fn a_plain_file_follows_the_owner_write_bit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain.txt");
    std::fs::write(&file, "x").unwrap();
    pyos::chmod(file.to_str().unwrap(), 0o400).unwrap();
    assert!(readonly(&file));
    pyos::chmod(file.to_str().unwrap(), 0o600).unwrap();
    assert!(!readonly(&file));
    let missing = dir.path().join("missing");
    let error = pyos::chmod(missing.to_str().unwrap(), 0o600).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(2));
}
