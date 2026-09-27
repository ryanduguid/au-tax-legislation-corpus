"""Staged queue writes with rollback; readers detect interrupted generations."""
from __future__ import annotations

import os
import re
import uuid
from pathlib import Path

from .errors import MonitorError

STEM_PATTERN = re.compile(r"[a-z]+(?:-[a-z]+)*")


def _remove_quietly(path: Path) -> None:
    try:
        path.unlink()
    except OSError:
        # Cleanup is best effort; the caller re-raises the original failure.
        pass


def _restore_quietly(parked: Path, destination: Path) -> None:
    try:
        os.replace(parked, destination)
    except OSError:
        pass


def _sibling_partial(destination: Path, *, private: bool = False) -> Path:
    """Create a unique staging file beside a queue destination.

    A private staging file is created owner-only where the platform honours
    POSIX permission bits, so a report naming client codes is not readable by
    other accounts even before it replaces the destination. Windows ignores
    the bits; there the output directory's own access control applies.
    """
    while True:
        candidate = destination.with_name(
            f"{destination.name}.{uuid.uuid4().hex[:12]}.partial"
        )
        try:
            if private:
                os.close(os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
            else:
                with candidate.open("x", encoding="utf-8"):
                    pass
        except FileExistsError:  # pragma: no cover - a 48-bit name collision.
            continue
        return candidate


def _swap_into_place(staged_path: Path, destination: Path) -> Path | None:
    """Commit one staged file while retaining the previous file for rollback."""
    parked: Path | None = None
    if destination.is_file():
        parked = _sibling_partial(destination)
        try:
            os.replace(destination, parked)
        except OSError:
            _remove_quietly(parked)
            raise
    try:
        os.replace(staged_path, destination)
    except OSError:
        if parked is not None:
            _restore_quietly(parked, destination)
        raise
    return parked


def output_paths(output_dir: Path, *, stem: str = "impact-queue") -> tuple[Path, Path]:
    """The JSON and Markdown destinations a write to output_dir will use.

    A relative directory must stay within the working directory. The stem is
    one of this package's own file names, never caller-supplied path text.
    """
    if not STEM_PATTERN.fullmatch(stem):
        raise MonitorError("output file stem must be a lower-case hyphenated name.")
    if not output_dir.is_absolute():
        root = Path.cwd().resolve()
        resolved = (root / output_dir).resolve()
        try:
            resolved.relative_to(root)
        except ValueError as exc:
            raise MonitorError(f"output directory must stay within {root}.") from exc
        output_dir = resolved
    return output_dir / f"{stem}.json", output_dir / f"{stem}.md"


def write_queue_files(
    json_text: str,
    markdown_text: str,
    output_dir: Path,
    *,
    stem: str = "impact-queue",
    private: bool = False,
) -> dict[str, Path]:
    """Stage and commit the JSON/Markdown pair, restoring the old pair on failure.

    The two replacements are sequential and rollback is best effort, so this
    assumes a single writer per output directory; readers detect a mixed pair
    by the companion check rather than by any lock.
    """
    json_path, markdown_path = output_paths(output_dir, stem=stem)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    rendered = (
        (json_path, json_text),
        (markdown_path, markdown_text),
    )

    staged: list[tuple[Path, Path]] = []
    try:
        for destination, text in rendered:
            staged_path = _sibling_partial(destination, private=private)
            staged.append((staged_path, destination))
            staged_path.write_text(text, encoding="utf-8")
    except BaseException:
        for staged_path, _ in staged:
            _remove_quietly(staged_path)
        raise

    replaced: list[tuple[Path, Path | None]] = []
    try:
        for staged_path, destination in staged:
            replaced.append(
                (destination, _swap_into_place(staged_path, destination))
            )
    except OSError:
        for destination, parked in reversed(replaced):
            if parked is None:
                _remove_quietly(destination)
            else:
                _restore_quietly(parked, destination)
        for staged_path, _ in staged:
            _remove_quietly(staged_path)
        raise
    for _, parked in replaced:
        if parked is not None:
            _remove_quietly(parked)
    return {"json": json_path, "markdown": markdown_path}

