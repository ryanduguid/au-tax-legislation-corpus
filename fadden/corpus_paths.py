"""Filesystem boundaries for the corpus builder.

The builder deliberately does not accept a filesystem root from an environment
variable.  A poisoned process environment must not be able to redirect a
download, deletion, or distribution build into an unrelated directory.

When the scripts are deployed in ``<corpus-root>/build`` (the documented
production layout), the corpus root is their parent.  Running the checked-out
builder directly keeps generated data under ``<checkout>/corpus`` instead.
"""

from __future__ import annotations

import os
import re
import stat
from pathlib import Path
from typing import Any, Union

_REGISTER_ID = re.compile(r"[A-Z]\d{4}[A-Z]\d{5}\Z")
PathPart = Union[str, os.PathLike[str]]


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
    """Stop when markdown/ holds a title directory the build does not list.

    A title a rebuild dropped kept its directory, so the rates index and MCP
    search went on serving it as in force. Dot-directories are tooling, not
    titles. Nothing is deleted: the operator moves the directories aside.
    """
    base = child(root, "markdown")
    if not os.path.isdir(base):
        return
    stale = sorted(
        entry.name for entry in os.scandir(base)
        if entry.is_dir() and not entry.name.startswith(".") and entry.name not in listed
    )
    if stale:
        raise RuntimeError(
            "markdown/ holds title directories this build does not list, which "
            "would stay searchable as in force: %s. Move them out of the corpus "
            "and re-run; nothing was written or deleted." % ", ".join(stale))
