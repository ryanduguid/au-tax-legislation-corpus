"""Failure paths of the live-evidence publication transaction.

The Windows classes drive the kernel32, ntdll and msvcrt calls that the Ubuntu
coverage leg cannot reach. Failures that real files cannot produce are forced by
replacing one kernel32 function, or one os call, at the point the exporter makes it.
The platform-neutral classes run everywhere.
"""

from __future__ import annotations

import contextlib
import ctypes
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable
from unittest import mock

import fadden.export_live_evidence_bundles as export_module
from fadden.export_live_evidence_bundles import LiveEvidenceBundleError

WINDOWS = unittest.skipUnless(
    os.name == "nt", "the publication transaction calls kernel32, ntdll and msvcrt"
)
_WINDLL = getattr(ctypes, "WinDLL", None)
_PREFIX = ".output.live-evidence-"


class _Library:
    """A loaded Windows library with chosen functions replaced."""

    def __init__(self, name: str, overrides: dict[str, Callable[..., int]]) -> None:
        self._library = _WINDLL(name, use_last_error=True)
        self._overrides = overrides

    def __getattr__(self, name: str) -> Any:
        return self._overrides.get(name) or getattr(self._library, name)


def _kernel32(**overrides: Callable[..., int]) -> Any:
    def load(name: str, **_options: object) -> _Library:
        return _Library(name, overrides if name == "kernel32" else {})

    return mock.patch.object(export_module.ctypes, "WinDLL", side_effect=load)


def _close_handle_then_fail(handle: object) -> int:
    _WINDLL("kernel32", use_last_error=True).CloseHandle(handle)
    return 0


def _close_fails() -> Any:
    real_close = os.close

    def close_then_fail(descriptor: int) -> None:
        real_close(descriptor)
        raise OSError("close")

    return mock.patch.object(export_module.os, "close", side_effect=close_then_fail)


class _TemporaryRoot(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def directory(self, name: str) -> Path:
        path = self.root / name
        path.mkdir()
        return path

    def file(self, name: str, content: bytes = b"{}\n") -> Path:
        path = self.root / name
        path.write_bytes(content)
        return path


@WINDOWS
class WindowsPinningTests(_TemporaryRoot):
    def test_missing_paths_and_files_are_not_pinned(self) -> None:
        with export_module._PinnedDirectoryChain() as chain:
            with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be pinned"):
                chain.pin(self.root / "missing", "capture input")
            with self.assertRaisesRegex(LiveEvidenceBundleError, "ordinary directory"):
                chain.pin(self.file("file.json"), "capture input")

    def test_unreadable_handle_details_are_not_pinned(self) -> None:
        with export_module._PinnedDirectoryChain() as chain, _kernel32(
            GetFileInformationByHandle=lambda *_: 0
        ):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be pinned"):
                chain.pin(self.root, "capture input")

    def test_a_failed_handle_close_is_reported(self) -> None:
        chain = export_module._PinnedDirectoryChain()
        chain.pin(self.root, "capture input")
        with _kernel32(CloseHandle=_close_handle_then_fail):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "snapshot cleanup failed"):
                chain.__exit__(None, None, None)

    def test_a_missing_child_cannot_be_opened(self) -> None:
        with export_module._PinnedDirectoryChain() as chain:
            handle = chain.pin(self.root, "capture input")
            with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be opened"):
                export_module._open_pinned_child(
                    handle, "missing.json", export_module._FILE_NON_DIRECTORY_FILE
                )

    def test_an_unpinned_copy_opens_the_source_path(self) -> None:
        source = self.file("source.json")

        expected = export_module._copy_regular_file(source, self.root / "copy.json")

        self.assertEqual((self.root / "copy.json").read_bytes(), source.read_bytes())
        self.assertTrue(os.path.samestat(expected, os.lstat(source)))


@WINDOWS
class WindowsLockedDirectoryTests(_TemporaryRoot):
    def lock(self, path: Path) -> int:
        return export_module._open_windows_locked_directory(
            path, os.lstat(self.root), "live evidence output parent"
        )

    def test_missing_and_replaced_directories_are_refused(self) -> None:
        with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be pinned"):
            self.lock(self.root / "missing")
        with self.assertRaisesRegex(LiveEvidenceBundleError, "changed while being pinned"):
            self.lock(self.directory("other"))

    def test_a_handle_that_cannot_be_duplicated_is_refused(self) -> None:
        with _kernel32(DuplicateHandle=lambda *_: 0):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be pinned"):
                self.lock(self.root)

    def test_failed_closes_and_deletions_are_reported(self) -> None:
        with _kernel32(CloseHandle=lambda *_: 0, SetFileInformationByHandle=lambda *_: 0):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "parent cleanup failed"):
                export_module._close_windows_handle(1, "live evidence output parent")
            with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be removed"):
                export_module._mark_exact_windows_handle_for_deletion(1, "staging")


@WINDOWS
class WindowsOwnedStagingTests(_TemporaryRoot):
    def owned(self) -> Any:
        staging = self.directory(f"{_PREFIX}test.tmp")
        (staging / "bundle.json").write_bytes(b"{}\n")
        owned = export_module._OwnedStaging.pin(
            staging,
            parent=self.root,
            prefix=_PREFIX,
            parent_details=os.lstat(self.root),
            staging_details=os.lstat(staging),
        )
        self.addCleanup(owned.close)
        owned.remember_file("bundle.json")
        return owned

    def test_a_moved_staging_directory_releases_the_parent(self) -> None:
        staging = self.directory(f"{_PREFIX}moved.tmp")
        with self.assertRaisesRegex(LiveEvidenceBundleError, "staging changed while being pinned"):
            export_module._OwnedStaging.pin(
                staging,
                parent=self.root,
                prefix=_PREFIX,
                parent_details=os.lstat(self.root),
                staging_details=os.lstat(self.root),
            )

    def test_a_child_that_cannot_be_opened_or_inspected_stops_cleanup(self) -> None:
        owned = self.owned()
        expected = owned.files["bundle.json"]
        with self.assertRaisesRegex(LiveEvidenceBundleError, "changed before cleanup"):
            export_module._open_owned_staging_child(owned, "missing.json", expected)
        for patch in (
            mock.patch("msvcrt.open_osfhandle", side_effect=OSError("descriptor")),
            mock.patch.object(export_module.os, "fstat", side_effect=OSError("stat")),
        ):
            with self.subTest(patch=patch.attribute), patch:
                with self.assertRaisesRegex(LiveEvidenceBundleError, "changed before cleanup"):
                    export_module._open_owned_staging_child(owned, "bundle.json", expected)

    def test_a_child_descriptor_that_will_not_close_is_reported(self) -> None:
        owned = self.owned()
        expected = owned.files["bundle.json"]
        with mock.patch.object(export_module.os, "fstat", side_effect=OSError("stat")), _close_fails():
            with self.assertRaisesRegex(LiveEvidenceBundleError, "child cleanup failed"):
                export_module._open_owned_staging_child(owned, "bundle.json", expected)

    def test_a_replaced_child_is_refused_and_released(self) -> None:
        owned = self.owned()
        other = os.lstat(self.file("other.json"))
        with self.assertRaisesRegex(LiveEvidenceBundleError, "changed before cleanup"):
            export_module._open_owned_staging_child(owned, "bundle.json", other)
        with _close_fails():
            with self.assertRaisesRegex(LiveEvidenceBundleError, "child cleanup failed"):
                export_module._open_owned_staging_child(owned, "bundle.json", other)

    def test_cleanup_reports_a_child_descriptor_that_will_not_close(self) -> None:
        owned = self.owned()
        with mock.patch.object(export_module, "_mark_exact_windows_handle_for_deletion"), (
            _close_fails()
        ):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "child cleanup failed"):
                export_module._cleanup_owned_staging(owned)

    def test_the_handle_bound_rename_never_replaces_a_destination(self) -> None:
        owned = self.owned()
        taken = self.directory("output")

        with self.assertRaisesRegex(OSError, "handle-bound rename failed"):
            export_module._promote_no_replace(owned.path, taken, owned)

        self.assertEqual(list(taken.iterdir()), [])
        self.assertTrue((owned.path / "bundle.json").is_file())


@WINDOWS
class WindowsPublicationTests(_TemporaryRoot):
    def publish(self, output: Path) -> None:
        export_module._publish_candidates(output, [])

    def test_directories_that_cannot_be_created_are_refused(self) -> None:
        for output, message in (
            (self.root / "new" / "output", "parent could not be created"),
            (self.root / "output", "could not be written"),
        ):
            with self.subTest(message=message), mock.patch.object(
                export_module.os, "mkdir", side_effect=OSError("denied")
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, message):
                    self.publish(output)
            self.assertEqual(list(self.root.iterdir()), [])

    def test_an_output_parent_replaced_after_preflight_is_refused(self) -> None:
        other = os.lstat(self.directory("other"))
        with mock.patch.object(export_module, "_preflight_output", return_value=(False, other)):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "changed before publication"):
                self.publish(self.root / "output")
        self.assertEqual([path.name for path in self.root.iterdir()], ["other"])

    def test_an_output_parent_replaced_before_promotion_is_refused(self) -> None:
        other = os.lstat(self.directory("other"))
        original = export_module._ordinary_directory_details
        parents = iter([None, other])

        def parent_moves(path: Path, label: str) -> os.stat_result:
            details = original(path, label)
            if label != "live evidence output parent":
                return details
            return next(parents) or details

        with mock.patch.object(
            export_module, "_ordinary_directory_details", side_effect=parent_moves
        ):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "changed before promotion"):
                self.publish(self.root / "output")
        self.assertEqual([path.name for path in self.root.iterdir()], ["other"])


class OwnedStagingBoundaryTests(_TemporaryRoot):
    def staging(self, path: Path, details: os.stat_result | None = None) -> Any:
        return SimpleNamespace(
            path=path,
            parent=self.root,
            prefix=_PREFIX,
            staging_details=details or os.lstat(path),
            files={},
        )

    def test_pinning_is_refused_off_windows(self) -> None:
        details = os.lstat(self.root)
        with mock.patch.object(export_module.os, "name", "posix"):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "unavailable on this platform"):
                export_module._OwnedStaging.pin(
                    self.root / "staging",
                    parent=self.root,
                    prefix=_PREFIX,
                    parent_details=details,
                    staging_details=details,
                )

    def close_with_failures(self, failing: tuple[str, ...], message: str) -> list[str]:
        attempts: list[str] = []

        def close_handle(_handle: int, label: str) -> None:
            attempts.append(label)
            if label in failing:
                raise LiveEvidenceBundleError(f"{label} cleanup failed.")

        details = os.lstat(self.root)
        owned = export_module._OwnedStaging(self.root, self.root, _PREFIX, details, details, 1, 2, {})
        with mock.patch.object(export_module, "_close_windows_handle", side_effect=close_handle):
            with self.assertRaisesRegex(LiveEvidenceBundleError, message):
                owned.close()
        return attempts

    def test_close_tries_both_handles_and_reports_staging_first(self) -> None:
        both = ("live evidence staging", "live evidence output parent")
        for failing, message in (
            (both[:1], "^live evidence staging cleanup"),
            (both[1:], "^live evidence output parent cleanup"),
            (both, "^live evidence staging cleanup"),
        ):
            with self.subTest(failing=failing):
                self.assertEqual(self.close_with_failures(failing, message), list(both))

    def test_cleanup_refuses_staging_outside_the_boundary(self) -> None:
        outside = self.directory("elsewhere") / f"{_PREFIX}x.tmp"
        outside.mkdir()
        with self.assertRaisesRegex(LiveEvidenceBundleError, "refusing to remove"):
            export_module._cleanup_owned_staging(self.staging(outside))

    def test_cleanup_refuses_uninspectable_replaced_or_unlistable_staging(self) -> None:
        path = self.directory(f"{_PREFIX}x.tmp")
        replaced = os.lstat(self.directory("other"))
        for staging, patch, message in (
            (self.staging(path), mock.patch.object(
                export_module.os, "lstat", side_effect=PermissionError("denied")
            ), "could not be inspected"),
            (self.staging(path, replaced), contextlib.nullcontext(), "changed before cleanup"),
            (self.staging(path), mock.patch.object(
                Path, "iterdir", side_effect=OSError("listing")
            ), "could not be listed"),
        ):
            with self.subTest(message=message), patch:
                with self.assertRaisesRegex(LiveEvidenceBundleError, message):
                    export_module._cleanup_owned_staging(staging)

    def test_promotion_refuses_other_paths_and_other_platforms(self) -> None:
        owned = SimpleNamespace(path=self.root / "staging", parent=self.root)
        with self.assertRaisesRegex(LiveEvidenceBundleError, "boundary is invalid"):
            export_module._promote_no_replace(self.root / "other", self.root / "output", owned)
        with mock.patch.object(export_module.os, "name", "posix"):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "before staging cleanup"):
                export_module._promote_no_replace(owned.path, self.root / "output", owned)


class StagedOutputTests(_TemporaryRoot):
    def test_staged_files_must_be_ordinary_readable_and_unchanged(self) -> None:
        for path, message in (
            (self.directory("folder"), "non-ordinary file"),
            (self.root / "missing.json", "could not be read"),
            (self.file("bundle.json"), "changed during validation"),
        ):
            with self.subTest(message=message):
                with self.assertRaisesRegex(LiveEvidenceBundleError, message):
                    export_module._read_staged_file(path, b"[]\n")

    def test_staged_json_must_parse_canonically(self) -> None:
        for content, message in ((b"{", "JSON is invalid"), (b'{"a":1}', "non-canonical")):
            with self.subTest(message=message):
                staging = self.directory(f"staging-{len(content)}")
                (staging / "bundle.json").write_bytes(content)
                with self.assertRaisesRegex(LiveEvidenceBundleError, message):
                    export_module._validate_staged_export(staging, {"bundle.json": content})

    def test_an_unlistable_staging_directory_is_refused(self) -> None:
        with mock.patch.object(Path, "iterdir", side_effect=OSError("listing")):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be listed"):
                export_module._validate_staged_export(self.root, {})

    def test_a_failed_new_file_write_closes_its_descriptor(self) -> None:
        target = self.root / "bundle.json"
        with mock.patch.object(export_module.os, "chmod", side_effect=OSError("mode")):
            with self.assertRaisesRegex(OSError, "mode"):
                export_module._write_new(target, b"{}\n")
        # Windows refuses to remove a file that still has an open descriptor.
        target.unlink()
