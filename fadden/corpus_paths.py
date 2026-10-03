"""Filesystem boundaries for the corpus builder.

The builder deliberately does not accept a filesystem root from an environment
variable.  A poisoned process environment must not be able to redirect a
download, deletion, or distribution build into an unrelated directory.

When the scripts are deployed in ``<corpus-root>/build`` (the documented
production layout), the corpus root is their parent.  Running the checked-out
builder directly keeps generated data under ``<checkout>/corpus`` instead.
"""

from __future__ import annotations

import contextlib
import datetime
import json
import os
import re
import stat
import string
import uuid
from pathlib import Path
from typing import Any, Iterator, TextIO, Union

_REGISTER_ID = re.compile(r"[A-Z][0-9]{4}[A-Z][0-9]{5}\Z")
_DATE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}\Z")
_START = re.compile(
    r"[0-9]{4}-[0-9]{2}-[0-9]{2}"
    r"(?:T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,7})?"
    r"(?:Z|[+-][0-9]{2}:[0-9]{2})?)?\Z"
)
PathPart = Union[str, os.PathLike[str]]


def iso_date(value: object) -> str:
    """Validate one calendar date before it becomes a URL component."""
    if not isinstance(value, str) or not _DATE.fullmatch(value):
        raise ValueError("invalid ISO calendar date")
    datetime.date.fromisoformat(value)
    return value


def version_date(value: object) -> str:
    """Validate the Register's date or timestamp and retain its calendar day."""
    if not isinstance(value, str) or not _START.fullmatch(value):
        raise ValueError("invalid Register version start")
    if len(value) > 10:
        offset = re.search(r"[+-]([0-9]{2}):([0-9]{2})$", value)
        if offset and (int(offset[1]) > 23 or int(offset[2]) > 59):
            raise ValueError("invalid Register timestamp offset")
        # The day is the result; validate the clock without relying on the
        # fraction grammar that changed after Python 3.10.
        datetime.time(int(value[11:13]), int(value[14:16]), int(value[17:19]))
    return iso_date(value[:10])


def version_rows(payload: object, title_id: str) -> list[dict[str, Any]]:
    """Reject malformed or misattributed API rows before consuming their fields."""
    if not isinstance(payload, dict) or not isinstance(payload.get("value"), list):
        raise ValueError("invalid Register versions response")
    rows = payload["value"]
    for row in rows:
        if not isinstance(row, dict) or row.get("titleId") != title_id:
            raise ValueError("Register version belongs to another or unknown title")
    return rows


def compilation_id(row: dict[str, Any]) -> str | None:
    """A missing field is an API failure; an explicit null means no document."""
    if "registerId" not in row:
        raise ValueError("Register version has no document identity field")
    value = row["registerId"]
    return None if value is None else register_id(value)


def require_builder_layout(script_file: PathPart) -> None:
    """Refuse checkout-only stages in an installed wheel before any side effect."""
    directory = Path(script_file).resolve().parent
    if directory.name == "build" or (directory / "sources.json").is_file():
        return
    if directory.name == "fadden":
        project = directory.parent
        if project.name == "build" or (
            (project / "pyproject.toml").is_file() and (project / "VERSION").is_file()
        ):
            return
    raise RuntimeError(
        "corpus stages require a source checkout or deployed build directory; "
        "installed package directories are read-only inputs"
    )


def markdown_text(value: object) -> str:
    """Render metadata as one literal Markdown line, leaving body markup alone."""
    text = " ".join(str(value).splitlines())
    return "".join("\\" + char if char in string.punctuation else char for char in text)


@contextlib.contextmanager
def atomic_text_writer(path: PathPart) -> Iterator[TextIO]:
    """Replace one mutable file after writing and syncing its unique sibling."""
    destination = Path(path)
    while True:
        temporary = destination.with_name(
            f".{destination.name}.{uuid.uuid4().hex}.tmp"
        )
        try:
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
        except FileExistsError:
            continue
        break
    target = None
    try:
        target = os.fdopen(descriptor, "w", encoding="utf-8")
        with target:
            yield target
            target.flush()
            os.fsync(target.fileno())
        if destination.exists():
            os.chmod(temporary, stat.S_IMODE(destination.stat().st_mode))
        os.replace(os.fspath(temporary), os.fspath(destination))
    except BaseException as error:
        if target is None:
            os.close(descriptor)
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        except OSError as cleanup_error:
            raise error from cleanup_error
        raise


def write_json_atomic(path: PathPart, value: Any, **kwargs: Any) -> None:
    """Keep caller-specific JSON formatting behind the shared file transaction."""
    with atomic_text_writer(path) as target:
        json.dump(value, target, **kwargs)


def is_reparse_point(path: PathPart) -> bool:
    """Return whether *path* is any Windows reparse point.

    ``islink`` and ``isjunction`` cover the common cases.  The attribute check
    also catches other reparse-point types before a recursive copy or removal
    treats them as ordinary contained files or directories.
    """
    attributes = getattr(os.lstat(os.fspath(path)), "st_file_attributes", 0)
    return bool(attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def _details_are_reparse_point(details: os.stat_result) -> bool:
    return bool(
        getattr(details, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    )


def _path_is_junction(path: Path) -> bool:
    return getattr(os.path, "isjunction", lambda _path: False)(path)


def _absolute(path: str | Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _same_location(left: Path, right: Path) -> bool:
    return os.path.normcase(os.path.realpath(left)) == os.path.normcase(os.path.realpath(right))


class _DuplicateJsonMemberError(ValueError):
    """Internal signal for an ambiguous JSON object at any depth."""


def _reject_duplicate_json_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateJsonMemberError
        result[key] = value
    return result


def corpus_root(script_file: PathPart) -> str:
    """Return the deterministic output root for a builder script."""
    script_dir = Path(script_file).resolve().parent
    if script_dir.name == "fadden":
        parent = script_dir.parent
        if parent.name == "build":
            return str(parent.parent)
        return str(parent / "corpus")
    if script_dir.name == "build":
        return str(script_dir.parent)
    return str(script_dir / "corpus")


def register_id(value: object) -> str:
    """Validate a Federal Register identifier before it becomes a path part."""
    if not isinstance(value, str) or not _REGISTER_ID.fullmatch(value):
        raise ValueError("invalid Federal Register identifier")
    # Keep the basename operation explicit as a defence in depth barrier for
    # filesystem consumers, even though the allowlist above forbids separators.
    return os.path.basename(value)


def child(root: PathPart, *parts: PathPart) -> str:
    """Return a realpath-contained descendant of *root* or raise ValueError."""
    root_path = os.path.realpath(os.fspath(root))
    candidate = os.path.realpath(os.path.join(root_path, *(os.fspath(p) for p in parts)))
    prefix = root_path if root_path.endswith(os.sep) else root_path + os.sep
    if candidate == root_path or not candidate.startswith(prefix):
        raise ValueError("path escapes the corpus root")
    return candidate


def reject_symlinks(directory: PathPart) -> None:
    """Reject a tree containing links or reparse points before recursive use."""
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    root = os.fspath(directory)
    if os.path.islink(root) or is_junction(root) or is_reparse_point(root):
        raise ValueError(
            "corpus tree contains a symbolic link, junction or reparse point")
    for current, directories, files in os.walk(root, followlinks=False):
        for name in [*directories, *files]:
            candidate = os.path.join(current, name)
            if (os.path.islink(candidate) or is_junction(candidate)
                    or is_reparse_point(candidate)):
                raise ValueError(
                    "corpus tree contains a symbolic link, junction or reparse point")


def refuse_unlisted_titles(root: PathPart, listed: set[str]) -> None:
    """Stop when markdown/ holds a title entry the build does not list.

    A title a rebuild dropped kept its directory, so the rates index and MCP
    search went on serving it as in force. Every entry counts whatever its
    type, so a link whose target is missing cannot slip past as "not a
    directory". Dot-entries are tooling, not titles. Nothing is deleted: the
    operator moves the entries aside.
    """
    base = child(root, "markdown")
    if not os.path.isdir(base):
        return
    stale = sorted(
        entry.name for entry in os.scandir(base)
        if not entry.name.startswith(".") and entry.name not in listed
    )
    if stale:
        raise RuntimeError(
            "markdown/ holds entries this build does not list, which "
            "would stay searchable as in force: %s. Move them out of the corpus "
            "and re-run; nothing was written or deleted." % ", ".join(stale))
