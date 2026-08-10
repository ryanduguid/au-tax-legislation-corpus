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
from pathlib import Path
from typing import Union


_REGISTER_ID = re.compile(r"[A-Z]\d{4}[A-Z]\d{5}\Z")
PathPart = Union[str, os.PathLike[str]]


def corpus_root(script_file: PathPart) -> str:
    """Return the deterministic output root for a builder script."""
    script_dir = Path(script_file).resolve().parent
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
    """Reject a tree containing links or junctions before a recursive copy."""
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    root = os.fspath(directory)
    if os.path.islink(root) or is_junction(root):
        raise ValueError("corpus tree contains a symbolic link or junction")
    for current, directories, files in os.walk(root, followlinks=False):
        for name in [*directories, *files]:
            candidate = os.path.join(current, name)
            if os.path.islink(candidate) or is_junction(candidate):
                raise ValueError("corpus tree contains a symbolic link or junction")
