from __future__ import annotations

import hashlib
import json
import os
import stat
import string
from dataclasses import dataclass
from importlib.resources import files
from pathlib import Path
from typing import Any

from .errors import MonitorError, SourceTooLargeError

MAX_JSON_BYTES = 50_000_000
MAX_JSON_DEPTH = 64


def _open_nonblocking(file: str, flags: int) -> int:
    # Windows has no O_NONBLOCK and no FIFOs for it to guard against.
    return os.open(file, flags | getattr(os, "O_NONBLOCK", 0))


class _DuplicateJsonMemberError(ValueError):
    """Internal signal for an ambiguous JSON object."""


def _reject_duplicate_json_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    payload: dict[str, Any] = {}
    for key, value in pairs:
        if key in payload:
            raise _DuplicateJsonMemberError
        payload[key] = value
    return payload


@dataclass(frozen=True, slots=True)
class SourceSnapshot:
    """One immutable read of a source and the digest of those exact bytes."""

    path: Path
    content: bytes
    sha256: str

    @classmethod
    def capture(cls, path: Path, *, label: str, limit: int = MAX_JSON_BYTES) -> SourceSnapshot:
        """Read path once; with a limit, read only a regular file and at most limit bytes."""
        try:
            if limit < 0:
                raise ValueError("source byte limit must be non-negative")
            # A non-blocking open returns at once even for a FIFO, and fstat then
            # checks the object actually opened, not whatever the path named earlier.
            with open(path, "rb", opener=_open_nonblocking) as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode):
                    raise MonitorError(f"{label} must be a regular file: {path}.")
                if info.st_size > limit:
                    raise SourceTooLargeError(f"{label} exceeds {limit} bytes.")
                # The first read is sized from metadata, since read(n) allocates n
                # bytes. A growing or size-zero virtual file that holds more than
                # it reported is then read only to one byte past the limit.
                content = stream.read(info.st_size + 1)
                if len(content) > info.st_size:
                    content += stream.read(limit + 1 - len(content))
            if len(content) > limit:
                raise SourceTooLargeError(f"{label} exceeds {limit} bytes.")
        except FileNotFoundError as exc:
            raise MonitorError(f"{label} does not exist: {path}.") from exc
        except OSError as exc:
            raise MonitorError(f"{label} could not be read: {path} ({exc}).") from exc
        return cls(path=path, content=content, sha256=hashlib.sha256(content).hexdigest())

    def text(self, *, label: str) -> str:
        try:
            return self.content.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise MonitorError(f"{label} could not be read as UTF-8: {self.path}.") from exc


def sample_path(*parts: str) -> Path:
    """Locate a shipped sample fixture inside the installed package.

    The samples ship as package data, so this resolves correctly for editable
    checkouts and plain ``pip install`` alike. The package installs as a real
    directory; zip imports are not supported.
    """
    resource = files(__package__)
    for part in ("samples", *parts):
        resource = resource.joinpath(part)
    return Path(str(resource))


def canonical_json(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")


def sha256_json(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def load_json(
    path: Path | SourceSnapshot, *, label: str
) -> Any:
    snapshot = (
        path if isinstance(path, SourceSnapshot) else SourceSnapshot.capture(path, label=label)
    )
    source_path = snapshot.path
    if len(snapshot.content) > MAX_JSON_BYTES:
        raise SourceTooLargeError(f"{label} exceeds {MAX_JSON_BYTES} bytes.")
    text = snapshot.text(label=label)
    depth = 0
    quoted = escaped = False
    for character in text:
        if quoted:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quoted = False
        elif character == '"':
            quoted = True
        elif character in "[{":
            depth += 1
            if depth > MAX_JSON_DEPTH:
                raise MonitorError(f"{label} exceeds {MAX_JSON_DEPTH} JSON nesting levels.")
        elif character in "]}":
            depth -= 1
    try:
        return json.loads(
            text,
            object_pairs_hook=_reject_duplicate_json_members,
        )
    except _DuplicateJsonMemberError as exc:
        raise MonitorError(
            f"{label} contains duplicate JSON members: {source_path}."
        ) from exc
    except (ValueError, RecursionError) as exc:
        raise MonitorError(f"{label} is not valid JSON: {source_path}.") from exc


def load_json_exact(
    path: Path | SourceSnapshot, required: set[str], *, label: str
) -> dict[str, Any]:
    payload = load_json(path, label=label)
    if not isinstance(payload, dict) or set(payload) != required:
        raise MonitorError(f"{label} must contain exactly: {', '.join(sorted(required))}.")
    return payload


def safe_markdown(value: str) -> str:
    """Escape metadata as ordinary Markdown text, outside code spans."""
    text = " ".join(value.splitlines())
    return "".join("\\" + char if char in string.punctuation else char for char in text)
