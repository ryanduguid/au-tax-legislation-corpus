"""Export deterministic live-only evidence-bundle.v2 candidates."""

from __future__ import annotations

import argparse
import base64
import ctypes
import datetime as dt
import hashlib
import json
import os
import re
import stat
import sys
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from ctypes import wintypes

from .capture_register import (
    ODATA_CONTEXT,
    ROW_FIELDS,
    CaptureRegisterError,
    validate_capture_graph,
)
from .corpus_paths import register_id as _validate_register_id


_SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
_SEMVER = re.compile(
    r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)
_VERSION_PATH = Path(__file__).resolve().parent.parent / "VERSION"
_PYPROJECT_PATH = Path(__file__).resolve().parent.parent / "pyproject.toml"
_MAX_RESPONSE_BYTES = 256 * 1024
_MAX_BUNDLE_BYTES = 1024 * 1024
_EVIDENCE_NAME = re.compile(r"sha256-[0-9a-f]{64}\.json\Z")
_CANDIDATE_ID = re.compile(
    r"bundle-frl-([a-z]\d{4}[a-z]\d{5})-([a-z]\d{4}[a-z]\d{5})-r1\Z"
)
_SAFE_OUTPUT_LEAF = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,99}\Z")
_VERSION_TIMESTAMP = re.compile(
    r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}"
    r"(?:\.\d{1,7})?(?:Z|[+-]\d{2}:\d{2})?\Z"
)
_BASELINE_TITLE_FIELDS = (
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
)
_CAPTURE_RESULT_FIELDS = (
    "register_id",
    "collection",
    "checked_at",
    "state",
    "error_category",
    "requests",
)
_REQUEST_FIELDS = (
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
)
_OBSERVATION_FIELDS = (
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
)


@dataclass(frozen=True)
class LiveEvidenceExport:
    release_tag: str
    candidates: tuple[Path, ...]


class LiveEvidenceBundleError(ValueError):
    pass


class _DuplicateJsonMemberError(ValueError):
    """Internal signal for ambiguous source JSON."""


def _reject_duplicate_json_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateJsonMemberError
        result[key] = value
    return result


def _details_are_reparse_point(details: os.stat_result) -> bool:
    return bool(
        getattr(details, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    )


def _path_is_junction(path: Path) -> bool:
    return getattr(os.path, "isjunction", lambda _path: False)(path)


def _same_regular_file_identity(expected: os.stat_result, observed: os.stat_result) -> bool:
    return (
        stat.S_ISREG(expected.st_mode)
        and stat.S_ISREG(observed.st_mode)
        and os.path.samestat(expected, observed)
        and getattr(expected, "st_nlink", 1) == 1
        and getattr(observed, "st_nlink", 1) == 1
    )


def _same_directory_identity(expected: os.stat_result, observed: os.stat_result) -> bool:
    return stat.S_ISDIR(expected.st_mode) and stat.S_ISDIR(observed.st_mode) and os.path.samestat(
        expected, observed
    )


class _ByHandleFileInformation(ctypes.Structure):
    _fields_ = [
        ("file_attributes", wintypes.DWORD),
        ("creation_time", wintypes.FILETIME),
        ("last_access_time", wintypes.FILETIME),
        ("last_write_time", wintypes.FILETIME),
        ("volume_serial_number", wintypes.DWORD),
        ("file_size_high", wintypes.DWORD),
        ("file_size_low", wintypes.DWORD),
        ("number_of_links", wintypes.DWORD),
        ("file_index_high", wintypes.DWORD),
        ("file_index_low", wintypes.DWORD),
    ]


class _UnicodeString(ctypes.Structure):
    _fields_ = [
        ("length", ctypes.c_ushort),
        ("maximum_length", ctypes.c_ushort),
        ("buffer", wintypes.LPWSTR),
    ]


class _ObjectAttributes(ctypes.Structure):
    _fields_ = [
        ("length", wintypes.ULONG),
        ("root_directory", wintypes.HANDLE),
        ("object_name", ctypes.POINTER(_UnicodeString)),
        ("attributes", wintypes.ULONG),
        ("security_descriptor", wintypes.LPVOID),
        ("security_quality_of_service", wintypes.LPVOID),
    ]


class _IoStatusBlock(ctypes.Structure):
    _fields_ = [("status", wintypes.LONG), ("information", ctypes.c_size_t)]


_FILE_ATTRIBUTE_DIRECTORY = 0x10
_FILE_ATTRIBUTE_REPARSE_POINT = 0x400
_FILE_READ_ATTRIBUTES = 0x80
_DELETE = 0x00010000
_FILE_ADD_FILE = 0x2
_GENERIC_READ = 0x80000000
_SYNCHRONIZE = 0x00100000
_FILE_SHARE_READ = 0x1
_FILE_SHARE_WRITE = 0x2
_OPEN_EXISTING = 3
_FILE_FLAG_BACKUP_SEMANTICS = 0x02000000
_FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000
_FILE_DIRECTORY_FILE = 0x1
_FILE_NON_DIRECTORY_FILE = 0x40
_FILE_SYNCHRONOUS_IO_NONALERT = 0x20
_FILE_DISPOSITION_INFO = 4
_FILE_RENAME_INFORMATION = 10


class _FileRenameInfo(ctypes.Structure):
    _fields_ = [
        ("replace_if_exists", wintypes.BOOL),
        ("root_directory", wintypes.HANDLE),
        ("file_name_length", wintypes.DWORD),
    ]


class _FileDispositionInfo(ctypes.Structure):
    _fields_ = [("delete_file", wintypes.BOOL)]


def _posix_descriptor_pinning_available() -> bool:
    """Whether this host can bind children to no-follow directory descriptors."""

    return (
        os.name == "posix"
        and hasattr(os, "O_DIRECTORY")
        and hasattr(os, "O_NOFOLLOW")
        and os.open in os.supports_dir_fd
        and os.stat in os.supports_dir_fd
        and os.stat in os.supports_follow_symlinks
        and os.listdir in os.supports_fd
    )


def _windows_handle_value(handle: wintypes.HANDLE | int | None) -> int:
    """Return a ctypes Windows handle as a stable integer value."""

    value = getattr(handle, "value", handle)
    if value is None:
        return 0
    return int(value)


class _PinnedDirectoryChain:
    """Hold source directories while recognised children are copied once."""

    def __init__(self) -> None:
        self._handles: list[int] = []

    def __enter__(self) -> _PinnedDirectoryChain:
        return self

    def __exit__(self, _type: object, _value: object, _traceback: object) -> None:
        if os.name == "nt":
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            for handle in reversed(self._handles):
                if not kernel32.CloseHandle(wintypes.HANDLE(handle)):
                    raise LiveEvidenceBundleError("capture snapshot cleanup failed.")
        elif _posix_descriptor_pinning_available():
            try:
                for handle in reversed(self._handles):
                    os.close(handle)
            except OSError as exc:
                raise LiveEvidenceBundleError("capture snapshot cleanup failed.") from exc

    def pin(self, path: Path, label: str, *, parent_handle: int | None = None) -> int | None:
        if os.name == "nt":
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel32.CreateFileW.argtypes = [
                wintypes.LPCWSTR,
                wintypes.DWORD,
                wintypes.DWORD,
                wintypes.LPVOID,
                wintypes.DWORD,
                wintypes.DWORD,
                wintypes.HANDLE,
            ]
            kernel32.CreateFileW.restype = wintypes.HANDLE
            kernel32.GetFileInformationByHandle.argtypes = [
                wintypes.HANDLE, ctypes.POINTER(_ByHandleFileInformation)
            ]
            kernel32.GetFileInformationByHandle.restype = wintypes.BOOL
            if parent_handle is None:
                handle = kernel32.CreateFileW(
                    str(path),
                    _FILE_READ_ATTRIBUTES,
                    _FILE_SHARE_READ | _FILE_SHARE_WRITE,
                    None,
                    _OPEN_EXISTING,
                    _FILE_FLAG_BACKUP_SEMANTICS | _FILE_FLAG_OPEN_REPARSE_POINT,
                    None,
                )
                if _windows_handle_value(handle) == _windows_handle_value(wintypes.HANDLE(-1)):
                    raise LiveEvidenceBundleError(f"{label} could not be pinned.")
            else:
                handle = wintypes.HANDLE(
                    _open_pinned_child(parent_handle, path.name, _FILE_DIRECTORY_FILE)
                )
            handle_value = _windows_handle_value(handle)
            details = _ByHandleFileInformation()
            if not kernel32.GetFileInformationByHandle(handle, ctypes.byref(details)):
                kernel32.CloseHandle(handle)
                raise LiveEvidenceBundleError(f"{label} could not be pinned.")
            if (
                not details.file_attributes & _FILE_ATTRIBUTE_DIRECTORY
                or details.file_attributes & _FILE_ATTRIBUTE_REPARSE_POINT
            ):
                kernel32.CloseHandle(handle)
                raise LiveEvidenceBundleError(f"{label} must be an ordinary directory.")
            self._handles.append(handle_value)
            return handle_value
        if not _posix_descriptor_pinning_available():
            raise LiveEvidenceBundleError("secure directory pinning is unavailable on this platform.")
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
        try:
            handle = (
                os.open(os.fspath(path), flags)
                if parent_handle is None
                else os.open(path.name, flags, dir_fd=parent_handle)
            )
        except OSError as exc:
            raise LiveEvidenceBundleError(f"{label} could not be pinned.") from exc
        details = os.fstat(handle)
        if not stat.S_ISDIR(details.st_mode):
            os.close(handle)
            raise LiveEvidenceBundleError(f"{label} must be an ordinary directory.")
        self._handles.append(handle)
        return handle


def _pinned_directory_details(path: Path, directory_handle: int | None) -> os.stat_result:
    try:
        details = (
            os.fstat(directory_handle)
            if _posix_descriptor_pinning_available() and directory_handle is not None
            else os.lstat(path)
        )
    except OSError as exc:
        raise LiveEvidenceBundleError("capture input inventory changed.") from exc
    if not stat.S_ISDIR(details.st_mode):
        raise LiveEvidenceBundleError("capture input inventory changed.")
    return details


def _pinned_path_identity_matches(path: Path, expected: os.stat_result) -> bool:
    try:
        observed = os.lstat(path)
    except OSError:
        return False
    return (
        _same_directory_identity(expected, observed)
        and not os.path.islink(path)
        and not _path_is_junction(path)
        and not _details_are_reparse_point(observed)
    )


def _child_details(
    source_path: Path, *, directory_handle: int | None = None
) -> os.stat_result:
    try:
        if _posix_descriptor_pinning_available():
            if directory_handle is None:
                raise LiveEvidenceBundleError("secure directory pinning is unavailable.")
            return os.stat(source_path.name, dir_fd=directory_handle, follow_symlinks=False)
        return os.lstat(source_path)
    except OSError as exc:
        raise LiveEvidenceBundleError("capture snapshot source file is unavailable.") from exc


def _directory_inventory(
    path: Path, *, directory_handle: int | None = None
) -> dict[str, tuple[int, int, int]]:
    try:
        if _posix_descriptor_pinning_available():
            if directory_handle is None:
                raise LiveEvidenceBundleError("secure directory pinning is unavailable.")
            names = os.listdir(directory_handle)
            return {
                name: (details.st_mode, details.st_dev, details.st_ino)
                for name in names
                for details in (
                    os.stat(name, dir_fd=directory_handle, follow_symlinks=False),
                )
            }
        entries = list(path.iterdir())
        return {
            entry.name: (details.st_mode, details.st_dev, details.st_ino)
            for entry in entries
            for details in (os.lstat(entry),)
        }
    except OSError as exc:
        raise LiveEvidenceBundleError("capture input inventory changed.") from exc


def _ordinary_file_details(
    source_path: Path, *, directory_handle: int | None = None
) -> os.stat_result:
    expected = _child_details(source_path, directory_handle=directory_handle)
    if (
        not stat.S_ISREG(expected.st_mode)
        or (
            not _posix_descriptor_pinning_available()
            and (
                os.path.islink(source_path)
                or _path_is_junction(source_path)
                or _details_are_reparse_point(expected)
            )
        )
        or getattr(expected, "st_nlink", 1) != 1
    ):
        raise LiveEvidenceBundleError("capture snapshot source file is not ordinary.")
    return expected


def _open_pinned_child(
    directory_handle: int,
    name: str,
    options: int,
    *,
    desired_access: int = _GENERIC_READ | _SYNCHRONIZE,
) -> int:
    name_buffer = ctypes.create_unicode_buffer(name)
    unicode_name = _UnicodeString(
        len(name) * 2,
        (len(name) + 1) * 2,
        ctypes.cast(name_buffer, wintypes.LPWSTR),
    )
    attributes = _ObjectAttributes(
        ctypes.sizeof(_ObjectAttributes),
        wintypes.HANDLE(directory_handle),
        ctypes.pointer(unicode_name),
        0,
        None,
        None,
    )
    status = _IoStatusBlock()
    handle = wintypes.HANDLE()
    ntdll = ctypes.WinDLL("ntdll", use_last_error=True)
    ntdll.NtOpenFile.argtypes = [
        ctypes.POINTER(wintypes.HANDLE),
        wintypes.ULONG,
        ctypes.POINTER(_ObjectAttributes),
        ctypes.POINTER(_IoStatusBlock),
        wintypes.ULONG,
        wintypes.ULONG,
    ]
    ntdll.NtOpenFile.restype = wintypes.LONG
    result = ntdll.NtOpenFile(
        ctypes.byref(handle),
        desired_access,
        ctypes.byref(attributes),
        ctypes.byref(status),
        _FILE_SHARE_READ | _FILE_SHARE_WRITE,
        options | _FILE_SYNCHRONOUS_IO_NONALERT | _FILE_FLAG_OPEN_REPARSE_POINT,
    )
    if result != 0:
        raise LiveEvidenceBundleError("capture snapshot source file could not be opened.")
    return int(handle.value)


def _open_pinned_regular_file(directory_handle: int, name: str) -> int:
    return _open_pinned_child(directory_handle, name, _FILE_NON_DIRECTORY_FILE)


def _copy_regular_file(
    source_path: Path,
    target_path: Path,
    *,
    directory_handle: int | None = None,
    expected: os.stat_result | None = None,
) -> os.stat_result:
    if expected is None:
        expected = _ordinary_file_details(source_path, directory_handle=directory_handle)
    try:
        if os.name == "nt" and directory_handle is None:
            source = open(source_path, "rb")
        elif os.name == "nt":
            import msvcrt

            source = os.fdopen(
                msvcrt.open_osfhandle(
                    _open_pinned_regular_file(directory_handle, source_path.name),
                    os.O_RDONLY | os.O_BINARY,
                ),
                "rb",
            )
        elif _posix_descriptor_pinning_available() and directory_handle is not None:
            file_handle = os.open(
                source_path.name,
                os.O_RDONLY | os.O_NOFOLLOW,
                dir_fd=directory_handle,
            )
            try:
                source = os.fdopen(file_handle, "rb")
            except OSError:
                os.close(file_handle)
                raise
        else:
            raise LiveEvidenceBundleError("secure directory pinning is unavailable.")
        with source, open(target_path, "xb") as target:
            opened = os.fstat(source.fileno())
            if not _same_regular_file_identity(expected, opened):
                raise LiveEvidenceBundleError("capture snapshot source file changed.")
            while block := source.read(64 * 1024):
                target.write(block)
            read_details = os.fstat(source.fileno())
        final = _child_details(source_path, directory_handle=directory_handle)
    except OSError as exc:
        raise LiveEvidenceBundleError("capture snapshot source file could not be copied.") from exc
    if not (
        _same_regular_file_identity(expected, read_details)
        and _same_regular_file_identity(read_details, final)
    ):
        raise LiveEvidenceBundleError("capture snapshot source file changed.")
    return expected


def _snapshot_capture_graph(capture_dir: str | Path, snapshot: Path) -> None:
    source = Path(os.path.abspath(os.fspath(capture_dir)))
    chain = list(reversed([source, *source.parents]))
    with _PinnedDirectoryChain() as pinned:
        handles: dict[Path, int | None] = {}
        parent_handle: int | None = None
        for directory in chain:
            handles[directory] = pinned.pin(
                directory, "capture input", parent_handle=parent_handle
            )
            parent_handle = handles[directory]
        pinned_directories = {
            directory: _pinned_directory_details(directory, handles[directory])
            for directory in chain
        }
        root_inventory = _directory_inventory(source, directory_handle=handles[source])
        if set(root_inventory) != {
            "monitor-baseline.json",
            "register-capture.json",
            "register-observation.json",
            "evidence",
        }:
            raise LiveEvidenceBundleError("capture input layout is invalid.")
        source_evidence = source / "evidence"
        evidence_handle = pinned.pin(
            source_evidence, "capture evidence", parent_handle=handles[source]
        )
        pinned_directories[source_evidence] = _pinned_directory_details(
            source_evidence, evidence_handle
        )
        if not all(
            _pinned_path_identity_matches(directory, expected)
            for directory, expected in pinned_directories.items()
        ):
            raise LiveEvidenceBundleError("capture input inventory changed.")
        evidence_inventory = _directory_inventory(
            source_evidence, directory_handle=evidence_handle
        )
        if any(_EVIDENCE_NAME.fullmatch(name) is None for name in evidence_inventory):
            raise LiveEvidenceBundleError("capture evidence layout is invalid.")
        try:
            os.mkdir(snapshot, 0o700)
            os.mkdir(snapshot / "evidence", 0o700)
        except OSError as exc:
            raise LiveEvidenceBundleError("capture snapshot could not be created.") from exc
        copied: list[tuple[Path, int | None, os.stat_result]] = []
        for name in (
            "monitor-baseline.json",
            "register-capture.json",
            "register-observation.json",
        ):
            path = source / name
            copied.append(
                (
                    path,
                    handles[source],
                    _copy_regular_file(
                        path,
                        snapshot / name,
                        directory_handle=handles[source],
                        expected=_ordinary_file_details(
                            path, directory_handle=handles[source]
                        ),
                    ),
                ),
            )
        for name in sorted(evidence_inventory):
            path = source_evidence / name
            copied.append(
                (
                    path,
                    evidence_handle,
                    _copy_regular_file(
                        path,
                        snapshot / "evidence" / name,
                        directory_handle=evidence_handle,
                        expected=_ordinary_file_details(
                            path, directory_handle=evidence_handle
                        ),
                    ),
                ),
            )
        if (
            _directory_inventory(source, directory_handle=handles[source]) != root_inventory
            or _directory_inventory(source_evidence, directory_handle=evidence_handle)
            != evidence_inventory
            or not all(
                _pinned_path_identity_matches(directory, expected)
                for directory, expected in pinned_directories.items()
            )
        ):
            raise LiveEvidenceBundleError("capture input inventory changed.")
        for path, directory_handle, expected in copied:
            try:
                final = _child_details(path, directory_handle=directory_handle)
            except OSError as exc:
                raise LiveEvidenceBundleError("capture snapshot source file changed.") from exc
            if not _same_regular_file_identity(expected, final):
                raise LiveEvidenceBundleError("capture snapshot source file changed.")


def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode(
        "utf-8"
    )


def _sha256_id(content: bytes) -> str:
    return f"sha256:{hashlib.sha256(content).hexdigest()}"


def _read_json(path: Path, label: str) -> tuple[bytes, dict[str, Any]]:
    try:
        content = path.read_bytes()
        value = json.loads(
            content.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_members
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
        raise LiveEvidenceBundleError(f"{label} could not be read as UTF-8 JSON.") from exc
    if not isinstance(value, dict):
        raise LiveEvidenceBundleError(f"{label} must be an object.")
    return content, value


def _required_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise LiveEvidenceBundleError(f"{label} must be an object.")
    return value


def _required_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or value.strip() != value:
        raise LiveEvidenceBundleError(f"{label} must be non-empty text.")
    return value


def _required_sha256(value: Any, label: str) -> str:
    text = _required_text(value, label)
    if _SHA256_ID.fullmatch(text) is None:
        raise LiveEvidenceBundleError(f"{label} must be a lowercase SHA-256 identifier.")
    return text


def _producer_version() -> str:
    try:
        version = _VERSION_PATH.read_text(encoding="utf-8")
        pyproject = _PYPROJECT_PATH.read_text(encoding="utf-8")
    except OSError as exc:
        raise LiveEvidenceBundleError("producer version could not be read.") from exc
    version = version.removesuffix("\n").removesuffix("\r")
    package_version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', pyproject, re.MULTILINE)
    if (
        _SEMVER.fullmatch(version) is None
        or package_version is None
        or package_version.group(1) != version
    ):
        raise LiveEvidenceBundleError("producer version must be strict Semantic Version 2.0.0.")
    return version


def _register_id(value: Any, label: str) -> str:
    try:
        return _validate_register_id(_required_text(value, label))
    except ValueError as exc:
        raise LiveEvidenceBundleError(f"{label} is invalid.") from exc


def _candidate_identity_pair(identity: str) -> tuple[str, str]:
    """Recover the two validated Register identifiers from a candidate identity."""

    match = _CANDIDATE_ID.fullmatch(_required_text(identity, "candidate identity"))
    if match is None:
        raise LiveEvidenceBundleError("candidate identity is invalid.")
    return (
        _register_id(match.group(1).upper(), "candidate baseline identifier"),
        _register_id(match.group(2).upper(), "candidate document identifier"),
    )


def _candidate_identity(register_id: str, document_id: str) -> str:
    """Construct an injective v2 identity from two strict Register identifiers."""

    pair = (
        _register_id(register_id, "candidate baseline identifier"),
        _register_id(document_id, "candidate document identifier"),
    )
    identity = f"bundle-frl-{pair[0].lower()}-{pair[1].lower()}-r1"
    if _candidate_identity_pair(identity) != pair:
        raise LiveEvidenceBundleError("candidate identity invariant failed.")
    return identity


def _rights(checked_at: str) -> dict[str, str]:
    capture_date = _required_text(checked_at, "observation checked_at")[:10]
    return {
        "mode": "metadata-only",
        "attribution": (
            "Based on content from the Federal Register of Legislation at "
            f"{capture_date}. For the latest information on Australian Government legislation "
            "please go to https://www.legislation.gov.au. Changes: selected and reformatted "
            "Federal Register metadata into a bounded evidence bundle and factual source update; "
            "no legislation text is reproduced."
        ),
        "licence_url": "https://creativecommons.org/licenses/by/4.0/",
    }


def _project_members(
    value: dict[str, Any], fields: tuple[str, ...], label: str
) -> dict[str, Any]:
    try:
        return {field: value[field] for field in fields}
    except KeyError as exc:
        raise LiveEvidenceBundleError(f"{label} is missing {exc.args[0]}.") from exc


def _project_capture_result(value: dict[str, Any]) -> dict[str, Any]:
    result = _project_members(value, _CAPTURE_RESULT_FIELDS, "capture result")
    requests = result["requests"]
    if not isinstance(requests, list):
        raise LiveEvidenceBundleError("capture result requests must be an array.")
    result["requests"] = [
        _project_members(
            _required_object(request, "capture request"),
            _REQUEST_FIELDS,
            "capture request",
        )
        for request in requests
    ]
    return result


def _read_response(path: Path) -> bytes:
    try:
        with open(path, "rb") as source:
            response = source.read(_MAX_RESPONSE_BYTES + 1)
    except OSError as exc:
        raise LiveEvidenceBundleError("retained response could not be read.") from exc
    if len(response) > _MAX_RESPONSE_BYTES:
        raise LiveEvidenceBundleError("retained response exceeds the 256 KiB limit.")
    return response


def _version_timestamp(value: Any, label: str) -> str:
    text = _required_text(value, label)
    if _VERSION_TIMESTAMP.fullmatch(text) is None:
        raise LiveEvidenceBundleError(f"{label} is invalid.")
    try:
        dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError as exc:
        raise LiveEvidenceBundleError(f"{label} is invalid.") from exc
    return text


def _raw_current_row(response: bytes, register_id: str) -> dict[str, Any]:
    try:
        document = json.loads(
            response.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_members
        )
    except (UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
        raise LiveEvidenceBundleError("retained response is not unambiguous OData JSON.") from exc
    if (
        not isinstance(document, dict)
        or set(document) != {"@odata.context", "value"}
        or document["@odata.context"] != ODATA_CONTEXT
        or not isinstance(document["value"], list)
        or len(document["value"]) != 1
    ):
        raise LiveEvidenceBundleError("retained response OData shape is invalid.")
    row = document["value"][0]
    if not isinstance(row, dict) or set(row) != ROW_FIELDS:
        raise LiveEvidenceBundleError("retained response OData row is invalid.")
    if row["titleId"] != register_id or row["isCurrent"] is not True or row["status"] != "InForce":
        raise LiveEvidenceBundleError("retained response is not the declared current row.")
    if row["registerId"] is None:
        raise LiveEvidenceBundleError("retained response current document identifier is missing.")
    _register_id(row["registerId"], "raw current document identifier")
    _required_text(row["compilationNumber"], "raw current compilation number")
    _version_timestamp(row["start"], "raw current compilation start")
    _version_timestamp(row["registeredAt"], "raw current registration timestamp")
    return row


def _candidate_bundle(
    title: dict[str, Any],
    result: dict[str, Any],
    observation: dict[str, Any],
    *,
    version: str,
    baseline_sha256: str,
    observation_sha256: str,
    observed_at: str,
    response: bytes,
) -> tuple[str, dict[str, Any]]:
    baseline_title = _project_members(title, _BASELINE_TITLE_FIELDS, "baseline title")
    capture_result = _project_capture_result(result)
    bundle_observation = _project_members(observation, _OBSERVATION_FIELDS, "observation")
    register_id = _register_id(
        baseline_title.get("register_id"), "baseline title register_id"
    )
    document_id = _register_id(
        bundle_observation.get("observed_register_document_id"),
        "observed register document id",
    )
    requests = capture_result["requests"]
    if not isinstance(requests, list) or len(requests) != 1:
        raise LiveEvidenceBundleError("SUPERSEDED capture result must have one request.")
    request = _required_object(requests[0], "capture request")
    response_sha256 = _required_sha256(request.get("response_sha256"), "response SHA-256")
    if _sha256_id(response) != response_sha256:
        raise LiveEvidenceBundleError(
            "retained response digest does not match the capture request."
        )
    if request.get("response_length") != len(response):
        raise LiveEvidenceBundleError(
            "retained response length does not match the capture request."
        )
    if bundle_observation.get("primary_response_sha256") != response_sha256:
        raise LiveEvidenceBundleError(
            "observation response digest does not match the capture request."
        )
    if capture_result.get("state") != "SUPERSEDED" or bundle_observation.get("state") != "SUPERSEDED":
        raise LiveEvidenceBundleError("candidate state is not SUPERSEDED.")
    if request.get("role") != "current" or request.get("http_status") != 200:
        raise LiveEvidenceBundleError("candidate current request is invalid.")
    if bundle_observation.get("primary_response_media_type") != "application/json":
        raise LiveEvidenceBundleError("candidate primary response media type is invalid.")
    raw_row = _raw_current_row(response, register_id)
    if (
        raw_row["compilationNumber"] != bundle_observation.get("observed_compilation_number")
        or raw_row["start"][:10] != bundle_observation.get("observed_compilation_date")
        or raw_row["registerId"] != bundle_observation.get("observed_register_document_id")
    ):
        raise LiveEvidenceBundleError("retained response does not prove the observed compilation.")
    if bundle_observation.get("capture_result_sha256") != _sha256_id(
        _json_bytes(capture_result)
    ):
        raise LiveEvidenceBundleError("observation capture result digest is invalid.")
    bundle_id = _candidate_identity(register_id, document_id)
    return bundle_id, {
        "schema_version": "evidence-bundle.v2",
        "producer": {"name": "tax-radar-au", "version": version},
        "run": {
            "observed_at": observed_at,
            "scope_id": "au-primary-tax-legislation.v4",
            "complete": True,
            "run_status": "VERIFIED",
            "baseline_sha256": baseline_sha256,
            "observation_sha256": observation_sha256,
        },
        "baseline_title": baseline_title,
        "capture_result": capture_result,
        "observation": bundle_observation,
        "rights": _rights(
            _required_text(bundle_observation.get("checked_at"), "observation checked_at")
        ),
        "primary_response_base64": base64.b64encode(response).decode("ascii"),
    }


def _absolute(path: str | Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _require_absent_destination(path: Path) -> None:
    try:
        os.lstat(path)
    except FileNotFoundError:
        return
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence output could not be inspected.") from exc
    raise LiveEvidenceBundleError("live evidence output directory must not already exist.")


def _ordinary_directory_details(path: Path, label: str) -> os.stat_result:
    try:
        details = os.lstat(path)
    except OSError as exc:
        raise LiveEvidenceBundleError(f"{label} could not be inspected.") from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(path)
        or _path_is_junction(path)
        or _details_are_reparse_point(details)
    ):
        raise LiveEvidenceBundleError(f"{label} must be an ordinary directory.")
    return details


def _open_windows_locked_directory(
    path: Path, expected: os.stat_result, label: str, *, desired_access: int = 0
) -> int:
    """Open the exact directory while denying competing delete or rename access."""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    kernel32.CreateFileW.restype = wintypes.HANDLE
    handle = kernel32.CreateFileW(
        str(path),
        _FILE_READ_ATTRIBUTES | _DELETE | _SYNCHRONIZE | desired_access,
        _FILE_SHARE_READ | _FILE_SHARE_WRITE,
        None,
        _OPEN_EXISTING,
        _FILE_FLAG_BACKUP_SEMANTICS | _FILE_FLAG_OPEN_REPARSE_POINT,
        None,
    )
    if _windows_handle_value(handle) == _windows_handle_value(wintypes.HANDLE(-1)):
        raise LiveEvidenceBundleError(f"{label} could not be pinned.")
    handle_value = _windows_handle_value(handle)
    try:
        import msvcrt

        kernel32.DuplicateHandle.argtypes = [
            wintypes.HANDLE,
            wintypes.HANDLE,
            wintypes.HANDLE,
            ctypes.POINTER(wintypes.HANDLE),
            wintypes.DWORD,
            wintypes.BOOL,
            wintypes.DWORD,
        ]
        kernel32.DuplicateHandle.restype = wintypes.BOOL
        current = kernel32.GetCurrentProcess()
        duplicate = wintypes.HANDLE()
        if not kernel32.DuplicateHandle(
            current,
            wintypes.HANDLE(handle_value),
            current,
            ctypes.byref(duplicate),
            0,
            False,
            2,
        ):
            raise OSError(ctypes.get_last_error(), "could not duplicate directory handle")
        descriptor = msvcrt.open_osfhandle(
            _windows_handle_value(duplicate), os.O_RDONLY | os.O_BINARY
        )
        try:
            observed = os.fstat(descriptor)
        finally:
            os.close(descriptor)
    except OSError as exc:
        kernel32.CloseHandle(wintypes.HANDLE(handle_value))
        raise LiveEvidenceBundleError(f"{label} could not be pinned.") from exc
    if not _same_directory_identity(expected, observed):
        kernel32.CloseHandle(wintypes.HANDLE(handle_value))
        raise LiveEvidenceBundleError(f"{label} changed while being pinned.")
    return handle_value


def _close_windows_handle(handle: int, label: str) -> None:
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    if not kernel32.CloseHandle(wintypes.HANDLE(handle)):
        raise LiveEvidenceBundleError(f"{label} cleanup failed.")


def _mark_exact_windows_handle_for_deletion(handle: int, label: str) -> None:
    information = _FileDispositionInfo(True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.SetFileInformationByHandle.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        wintypes.LPVOID,
        wintypes.DWORD,
    ]
    kernel32.SetFileInformationByHandle.restype = wintypes.BOOL
    if not kernel32.SetFileInformationByHandle(
        wintypes.HANDLE(handle),
        _FILE_DISPOSITION_INFO,
        ctypes.byref(information),
        ctypes.sizeof(information),
    ):
        raise LiveEvidenceBundleError(f"{label} could not be removed.")


def _open_owned_staging_child(
    staging: _OwnedStaging, name: str, expected: os.stat_result
) -> int:
    try:
        handle = _open_pinned_child(
            staging.staging_handle,
            name,
            _FILE_NON_DIRECTORY_FILE,
            desired_access=_FILE_READ_ATTRIBUTES | _DELETE | _SYNCHRONIZE,
        )
    except LiveEvidenceBundleError as exc:
        raise LiveEvidenceBundleError("live evidence staging changed before cleanup.") from exc
    descriptor = -1
    try:
        import msvcrt

        descriptor = msvcrt.open_osfhandle(
            handle, os.O_RDONLY | os.O_BINARY
        )
        observed = os.fstat(descriptor)
    except OSError as exc:
        if descriptor != -1:
            try:
                os.close(descriptor)
            except OSError as close_error:
                raise LiveEvidenceBundleError(
                    "live evidence staging child cleanup failed."
                ) from close_error
        else:
            _close_windows_handle(handle, "live evidence staging child")
        raise LiveEvidenceBundleError("live evidence staging changed before cleanup.") from exc
    if not _same_regular_file_identity(expected, observed):
        try:
            os.close(descriptor)
        except OSError as exc:
            raise LiveEvidenceBundleError("live evidence staging child cleanup failed.") from exc
        raise LiveEvidenceBundleError("live evidence staging changed before cleanup.")
    return descriptor


@dataclass
class _OwnedStaging:
    path: Path
    parent: Path
    prefix: str
    parent_details: os.stat_result
    staging_details: os.stat_result
    parent_handle: int
    staging_handle: int
    files: dict[str, os.stat_result]
    delete_on_close: bool = False

    @classmethod
    def pin(
        cls,
        path: Path,
        *,
        parent: Path,
        prefix: str,
        parent_details: os.stat_result,
        staging_details: os.stat_result,
    ) -> _OwnedStaging:
        if os.name != "nt":
            raise LiveEvidenceBundleError(
                "identity-bound live evidence directory operations are unavailable on this platform."
            )
        parent_handle = _open_windows_locked_directory(
            parent,
            parent_details,
            "live evidence output parent",
            desired_access=_FILE_ADD_FILE,
        )
        try:
            staging_handle = _open_windows_locked_directory(
                path, staging_details, "live evidence staging"
            )
        except LiveEvidenceBundleError:
            _close_windows_handle(parent_handle, "live evidence output parent")
            raise
        return cls(
            path,
            parent,
            prefix,
            parent_details,
            staging_details,
            parent_handle,
            staging_handle,
            {},
        )

    def remember_file(self, name: str) -> None:
        details = _ordinary_file_details(self.path / name)
        self.files[name] = details

    def close(self) -> None:
        staging_error: LiveEvidenceBundleError | None = None
        try:
            _close_windows_handle(self.staging_handle, "live evidence staging")
        except LiveEvidenceBundleError as exc:
            staging_error = exc
        try:
            _close_windows_handle(self.parent_handle, "live evidence output parent")
        except LiveEvidenceBundleError:
            if staging_error is None:
                raise
        if staging_error is not None:
            raise staging_error


def _require_ordinary_ancestors(path: Path, label: str) -> None:
    current = path
    while current != current.parent:
        _ordinary_directory_details(current, label)
        current = current.parent


def _output_collides_capture(output: Path, capture: Path) -> bool:
    output_text = os.path.normcase(os.path.realpath(output))
    capture_text = os.path.normcase(os.path.realpath(capture))
    try:
        return os.path.commonpath((output_text, capture_text)) == capture_text
    except ValueError:
        return False


def _preflight_output(output: Path) -> tuple[bool, os.stat_result | None]:
    if _SAFE_OUTPUT_LEAF.fullmatch(output.name) is None:
        raise LiveEvidenceBundleError("live evidence output name is unsafe.")
    _require_absent_destination(output)
    try:
        parent_details = os.lstat(output.parent)
    except FileNotFoundError:
        if _SAFE_OUTPUT_LEAF.fullmatch(output.parent.name) is None:
            raise LiveEvidenceBundleError("live evidence output parent name is unsafe.")
        try:
            _require_ordinary_ancestors(output.parent.parent, "live evidence output")
        except LiveEvidenceBundleError as exc:
            raise LiveEvidenceBundleError("live evidence permits only one missing output parent.") from exc
        return True, None
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence output parent could not be inspected.") from exc
    if (
        not stat.S_ISDIR(parent_details.st_mode)
        or os.path.islink(output.parent)
        or _path_is_junction(output.parent)
        or _details_are_reparse_point(parent_details)
    ):
        raise LiveEvidenceBundleError("live evidence output parent must be an ordinary directory.")
    _require_ordinary_ancestors(output.parent.parent, "live evidence output")
    return False, parent_details


def _write_new(path: Path, content: bytes) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        os.chmod(path, 0o600)
        with os.fdopen(descriptor, "wb") as target:
            descriptor = -1
            target.write(content)
    finally:
        if descriptor != -1:
            os.close(descriptor)


def _read_staged_file(path: Path, expected: bytes) -> None:
    try:
        details = os.lstat(path)
        if (
            not stat.S_ISREG(details.st_mode)
            or os.path.islink(path)
            or _details_are_reparse_point(details)
        ):
            raise LiveEvidenceBundleError("staged live evidence output contains a non-ordinary file.")
        with open(path, "rb") as source:
            opened = os.fstat(source.fileno())
            first = source.read(len(expected) + 1)
            source.seek(0)
            second = source.read(len(expected) + 1)
            read_details = os.fstat(source.fileno())
        final = os.lstat(path)
    except LiveEvidenceBundleError:
        raise
    except OSError as exc:
        raise LiveEvidenceBundleError("staged live evidence output could not be read.") from exc
    if not (
        _same_regular_file_identity(details, opened)
        and _same_regular_file_identity(opened, read_details)
        and _same_regular_file_identity(read_details, final)
        and first == second == expected
        and len(first) == read_details.st_size == details.st_size
    ):
        raise LiveEvidenceBundleError("staged live evidence output changed during validation.")


def _validate_staged_export(staging: Path, expected: dict[str, bytes]) -> None:
    """Re-read exactly the files that the exporter generated before promotion."""
    _ordinary_directory_details(staging, "staged live evidence output")
    try:
        entries = {entry.name for entry in staging.iterdir()}
    except OSError as exc:
        raise LiveEvidenceBundleError("staged live evidence output could not be listed.") from exc
    if entries != set(expected):
        raise LiveEvidenceBundleError("staged live evidence output layout is invalid.")
    for name, content in expected.items():
        _read_staged_file(staging / name, content)
        try:
            parsed = json.loads(content.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_members)
        except (UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
            raise LiveEvidenceBundleError("staged live evidence output JSON is invalid.") from exc
        if not isinstance(parsed, dict) or _json_bytes(parsed) != content:
            raise LiveEvidenceBundleError("staged live evidence output JSON is non-canonical.")


def _cleanup_owned_staging(staging: _OwnedStaging) -> None:
    """Delete only recorded children from the exact held staging directory."""
    if staging.path.parent != staging.parent or not staging.path.name.startswith(staging.prefix):
        raise LiveEvidenceBundleError("refusing to remove staging outside the live evidence boundary.")
    try:
        current = os.lstat(staging.path)
    except FileNotFoundError:
        return
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence staging could not be inspected.") from exc
    if not _same_directory_identity(staging.staging_details, current):
        raise LiveEvidenceBundleError("live evidence staging changed before cleanup.")
    try:
        entries = {entry.name for entry in staging.path.iterdir()}
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence staging could not be listed.") from exc
    if entries != set(staging.files):
        raise LiveEvidenceBundleError("live evidence staging changed before cleanup.")
    for name, expected in staging.files.items():
        descriptor = _open_owned_staging_child(staging, name, expected)
        try:
            import msvcrt

            _mark_exact_windows_handle_for_deletion(
                msvcrt.get_osfhandle(descriptor), "live evidence staging child"
            )
        finally:
            try:
                os.close(descriptor)
            except OSError as exc:
                raise LiveEvidenceBundleError("live evidence staging child cleanup failed.") from exc
    _mark_exact_windows_handle_for_deletion(staging.staging_handle, "live evidence staging")
    staging.delete_on_close = True


def _promote_no_replace(staging: Path, destination: Path, owned: _OwnedStaging) -> None:
    """Rename the exact pinned staging handle without replacing a destination."""
    if staging != owned.path or destination.parent != owned.parent:
        raise LiveEvidenceBundleError("live evidence promotion boundary is invalid.")
    if os.name != "nt":
        raise LiveEvidenceBundleError("live evidence output parent changed before staging cleanup.")
    encoded_name = destination.name.encode("utf-16le")
    name_offset = _FileRenameInfo.file_name_length.offset + ctypes.sizeof(wintypes.DWORD)
    size = ctypes.sizeof(_FileRenameInfo) + len(encoded_name)
    buffer = ctypes.create_string_buffer(size)
    information = ctypes.cast(buffer, ctypes.POINTER(_FileRenameInfo)).contents
    information.replace_if_exists = False
    information.root_directory = wintypes.HANDLE(owned.parent_handle)
    information.file_name_length = len(encoded_name)
    ctypes.memmove(ctypes.addressof(buffer) + name_offset, encoded_name, len(encoded_name))
    status = _IoStatusBlock()
    ntdll = ctypes.WinDLL("ntdll", use_last_error=True)
    ntdll.NtSetInformationFile.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(_IoStatusBlock),
        wintypes.LPVOID,
        wintypes.ULONG,
        wintypes.ULONG,
    ]
    ntdll.NtSetInformationFile.restype = wintypes.LONG
    result = ntdll.NtSetInformationFile(
        wintypes.HANDLE(owned.staging_handle),
        ctypes.byref(status),
        buffer,
        size,
        _FILE_RENAME_INFORMATION,
    )
    if result != 0:
        raise OSError("handle-bound rename failed")


def _publish_candidates(
    output: Path, candidates: list[tuple[str, dict[str, Any]]]
) -> tuple[Path, ...]:
    expected: dict[str, bytes] = {}
    for bundle_id, bundle in sorted(candidates):
        content = _json_bytes(bundle)
        if len(content) > _MAX_BUNDLE_BYTES:
            raise LiveEvidenceBundleError("serialised bundle exceeds the 1 MiB limit.")
        expected[f"{bundle_id}.json"] = content

    create_parent, known_parent = _preflight_output(output)
    if os.name != "nt":
        raise LiveEvidenceBundleError(
            "identity-bound live evidence publication is supported only on Windows."
        )
    if create_parent:
        try:
            os.mkdir(output.parent, 0o700)
            os.chmod(output.parent, 0o700)
        except OSError as exc:
            raise LiveEvidenceBundleError("live evidence output parent could not be created.") from exc
    parent_details = _ordinary_directory_details(output.parent, "live evidence output parent")
    if known_parent is not None and not _same_directory_identity(known_parent, parent_details):
        raise LiveEvidenceBundleError("live evidence output parent changed before publication.")
    _require_ordinary_ancestors(output.parent.parent, "live evidence output")
    _require_absent_destination(output)

    prefix = f".{output.name}.live-evidence-"
    staging = output.parent / f"{prefix}{uuid.uuid4().hex}.tmp"
    owned: _OwnedStaging | None = None
    try:
        try:
            os.mkdir(staging, 0o700)
            staging_details = _ordinary_directory_details(staging, "live evidence staging")
            owned = _OwnedStaging.pin(
                staging,
                parent=output.parent,
                prefix=prefix,
                parent_details=parent_details,
                staging_details=staging_details,
            )
            os.chmod(staging, 0o700)
            for name, content in expected.items():
                _write_new(staging / name, content)
                owned.remember_file(name)
        except OSError as exc:
            raise LiveEvidenceBundleError("live evidence output could not be written.") from exc
        _validate_staged_export(staging, expected)
        current_parent = _ordinary_directory_details(output.parent, "live evidence output parent")
        if not _same_directory_identity(parent_details, current_parent):
            raise LiveEvidenceBundleError("live evidence output parent changed before promotion.")
        _require_absent_destination(output)
        try:
            _promote_no_replace(staging, output, owned)
        except OSError as exc:
            raise LiveEvidenceBundleError("live evidence output could not be promoted.") from exc
    finally:
        if owned is not None:
            try:
                _cleanup_owned_staging(owned)
            finally:
                owned.close()
    return tuple(output / name for name in expected)


def _build_validated_snapshot(capture: Path) -> tuple[str, list[tuple[str, dict[str, Any]]]]:
    """Build v2 candidates from an exporter-owned, validated capture snapshot."""
    baseline_bytes, baseline = _read_json(capture / "monitor-baseline.json", "baseline")
    capture_bytes, capture_document = _read_json(
        capture / "register-capture.json", "capture"
    )
    observation_bytes, observation_document = _read_json(
        capture / "register-observation.json", "observation"
    )
    if (
        capture_document.get("schema_version") != "au-tax-register-capture.v1"
        or capture_document.get("mode") != "live"
        or observation_document.get("schema_version") != "au-tax-register-observation.v4"
        or observation_document.get("mode") != "live"
        or observation_document.get("scope_id") != "au-primary-tax-legislation.v4"
        or observation_document.get("complete") is not True
        or observation_document.get("run_status") != "VERIFIED"
    ):
        raise LiveEvidenceBundleError("capture must be a verified live Stage 3A graph.")
    baseline_sha256 = _sha256_id(baseline_bytes)
    observation_sha256 = _sha256_id(observation_bytes)
    if (
        capture_document.get("baseline_sha256") != baseline_sha256
        or observation_document.get("baseline_sha256") != baseline_sha256
        or observation_document.get("capture_sha256") != _sha256_id(capture_bytes)
    ):
        raise LiveEvidenceBundleError("capture graph digests are inconsistent.")
    titles = baseline.get("titles")
    results = capture_document.get("results")
    observations = observation_document.get("observations")
    if not isinstance(titles, list) or not isinstance(results, list) or not isinstance(observations, list):
        raise LiveEvidenceBundleError("capture graph collections must be arrays.")
    titles_by_id = {
        _register_id(
            _required_object(title, "baseline title").get("register_id"),
            "baseline title register_id",
        ): title
        for title in titles
    }
    results_by_id = {
        _register_id(
            _required_object(result, "capture result").get("register_id"),
            "capture result register_id",
        ): result
        for result in results
    }
    candidates: list[tuple[str, dict[str, Any]]] = []
    candidate_pairs: dict[str, tuple[str, str]] = {}
    for raw_observation in observations:
        observation = _required_object(raw_observation, "observation item")
        if observation.get("state") != "SUPERSEDED":
            continue
        register_id = _register_id(observation.get("register_id"), "observation register_id")
        try:
            title = _required_object(titles_by_id[register_id], "baseline title")
            result = _required_object(results_by_id[register_id], "capture result")
        except KeyError as exc:
            raise LiveEvidenceBundleError("SUPERSEDED observation is missing its capture data.") from exc
        requests = result.get("requests")
        if not isinstance(requests, list) or not requests:
            raise LiveEvidenceBundleError("SUPERSEDED capture result must have a request.")
        request = _required_object(requests[0], "capture request")
        response_sha256 = _required_sha256(
            request.get("response_sha256"), "capture response SHA-256"
        )
        expected_evidence_path = f"evidence/sha256-{response_sha256.removeprefix('sha256:')}.json"
        if request.get("evidence_path") != expected_evidence_path:
            raise LiveEvidenceBundleError("capture evidence path is invalid.")
        response = _read_response(capture / expected_evidence_path)
        candidate = _candidate_bundle(
            title,
            result,
            observation,
            version=_producer_version(),
            baseline_sha256=baseline_sha256,
            observation_sha256=observation_sha256,
            observed_at=_required_text(observation_document.get("observed_at"), "observed_at"),
            response=response,
        )
        pair = _candidate_identity_pair(candidate[0])
        previous_pair = candidate_pairs.get(candidate[0])
        if previous_pair is not None:
            if previous_pair != pair:
                raise LiveEvidenceBundleError("distinct candidate pairs collide.")
            raise LiveEvidenceBundleError("candidate identity is duplicated.")
        candidate_pairs[candidate[0]] = pair
        candidates.append(candidate)
    return f"live-evidence-v2-{observation_sha256.removeprefix('sha256:')}", candidates


def export_live_evidence_bundles(
    capture_dir: str | Path, output_dir: str | Path
) -> LiveEvidenceExport:
    """Write v2 candidates only from a private, verified Stage 3A snapshot."""
    source_capture = _absolute(capture_dir)
    output = _absolute(output_dir)
    if _output_collides_capture(output, source_capture):
        raise LiveEvidenceBundleError("live evidence output would replace a capture input.")
    try:
        with tempfile.TemporaryDirectory(prefix="tax-radar-live-evidence-") as temporary:
            snapshot = Path(temporary) / "capture"
            _snapshot_capture_graph(source_capture, snapshot)
            try:
                validate_capture_graph(snapshot)
            except CaptureRegisterError as exc:
                raise LiveEvidenceBundleError("capture graph is not a verified Stage 3A graph.") from exc
            release_tag, candidates = _build_validated_snapshot(snapshot)
    except LiveEvidenceBundleError:
        raise
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence snapshot cleanup failed.") from exc
    try:
        paths = _publish_candidates(output, candidates)
    except LiveEvidenceBundleError:
        raise
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence export failed.") from exc
    return LiveEvidenceExport(release_tag, paths)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Export live evidence-bundle.v2 candidates.")
    parser.add_argument("capture_dir")
    parser.add_argument("--out", required=True, dest="output_dir")
    arguments = parser.parse_args(argv)
    try:
        export = export_live_evidence_bundles(arguments.capture_dir, arguments.output_dir)
    except LiveEvidenceBundleError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except OSError:
        print("error: live evidence export failed", file=sys.stderr)
        return 1
    print(f"candidate_count={len(export.candidates)}")
    print(f"release_tag={export.release_tag}")
    return 0
