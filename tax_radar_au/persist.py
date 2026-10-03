"""Staged queue writes with rollback; readers detect interrupted generations."""
from __future__ import annotations

import os
import re
import stat
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
    else:
        # Renaming two links to the same inode can leave the backup name.
        _remove_quietly(parked)


def _sibling_partial(destination: Path, *, private: bool = False) -> tuple[Path, int]:
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
            descriptor = os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                                 0o600 if private else 0o666)
        except FileExistsError:  # pragma: no cover - a 48-bit name collision.
            continue
        return candidate, descriptor


def _backup_existing(destination: Path) -> Path | None:
    """Link the old inode without changing its name, permissions or ownership."""
    try:
        details = destination.lstat()
    except FileNotFoundError:
        return None
    if (not stat.S_ISREG(details.st_mode) or destination.is_symlink()
            or getattr(details, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)):
        raise MonitorError("queue destinations must be ordinary files.")
    parked, descriptor = _sibling_partial(destination, private=True)
    os.close(descriptor)
    parked.unlink()
    # Link creation never replaces a collision; failure leaves the original intact.
    os.link(destination, parked, follow_symlinks=False)
    return parked


def output_paths(output_dir: Path, *, stem: str = "impact-queue") -> tuple[Path, Path]:
    """The JSON and Markdown destinations a write to output_dir will use.

    A relative directory must stay within the working directory. The stem is
    one of this package's own file names, never caller-supplied path text.
    """
    if not STEM_PATTERN.fullmatch(stem):
        raise MonitorError("output file stem must be a lower-case hyphenated name.")
    output_dir = contained_output(output_dir)
    return output_dir / f"{stem}.json", output_dir / f"{stem}.md"


def contained_output(path: Path) -> Path:
    """Absolute outputs are explicit; relative outputs must stay within cwd."""
    if not path.is_absolute():
        root = Path.cwd().resolve()
        resolved = (root / path).resolve()
        try:
            resolved.relative_to(root)
        except ValueError as exc:
            raise MonitorError(f"output path must stay within {root}.") from exc
        absolute = root / path
        path = absolute.parent.resolve() / absolute.name
    return path


def write_receipt(text: str, path: Path, *, inputs: tuple[Path, ...]) -> Path:
    """Publish a new receipt atomically without replacing any existing path."""
    path = contained_output(path)
    refuse_input_overwrite((path,), inputs)
    try:
        path.lstat()
    except FileNotFoundError:
        pass
    else:
        raise MonitorError("The validation output must not exist; choose another --out.")
    path.parent.mkdir(parents=True, exist_ok=True)
    staged, descriptor = _sibling_partial(path)
    stream = None
    published = False
    try:
        # Descriptor belongs to exclusive staging for the explicit local CLI destination.
        stream = os.fdopen(descriptor, "w", encoding="utf-8")  # NOSONAR
        with stream:
            stream.write(text)  # NOSONAR - retained staging inode, published without clobber
            stream.flush()
            os.fsync(stream.fileno())
        if os.name == "nt":
            # Windows rename fails if a destination appears after validation.
            os.rename(staged, path)
        else:
            # A hard link publishes the fully written inode with no clobber.
            os.link(staged, path)
        published = True
    except BaseException as error:
        if stream is None:
            os.close(descriptor)
        try:
            staged.unlink()
        except FileNotFoundError:
            pass
        except OSError as cleanup_error:
            raise error from cleanup_error
        raise
    finally:
        if published:
            try:
                staged.unlink()
            except FileNotFoundError:
                pass
            except OSError as exc:
                raise MonitorError("Validation receipt was published, but its staging file could not be removed.") from exc
    return path


def refuse_input_overwrite(destinations: tuple[Path, ...], inputs: tuple[Path, ...]) -> None:
    """Reject output paths that name an input, including symbolic and hard links."""
    try:
        protected = {path.resolve() for path in inputs}
        for destination in destinations:
            resolved = destination.resolve()
            if resolved in protected or (
                resolved.exists()
                and any(path.exists() and resolved.samefile(path) for path in protected)
            ):
                raise MonitorError("The output path would replace an input file; choose another --out.")
    except (OSError, RuntimeError) as exc:
        raise MonitorError("Output paths could not be checked against input files.") from exc


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
            staged_path, descriptor = _sibling_partial(destination, private=private)
            staged.append((staged_path, destination))
            stream = None
            try:
                stream = os.fdopen(descriptor, "w", encoding="utf-8")
                with stream:
                    stream.write(text)
                    stream.flush()
                    os.fsync(stream.fileno())
            finally:
                if stream is None:
                    os.close(descriptor)
    except BaseException:
        for staged_path, _ in staged:
            _remove_quietly(staged_path)
        raise

    backups: list[tuple[Path, Path | None]] = []
    replaced: list[tuple[Path, Path | None]] = []
    try:
        for _staged_path, destination in staged:
            backups.append((destination, _backup_existing(destination)))
        for (staged_path, destination), backup in zip(staged, backups):
            replaced.append(backup)
            os.replace(staged_path, destination)
    except BaseException:
        for destination, parked in reversed(replaced):
            if parked is None:
                _remove_quietly(destination)
            else:
                _restore_quietly(parked, destination)
        raise
    else:
        for _, parked in backups:
            if parked is not None:
                _remove_quietly(parked)
    finally:
        # Unattempted destinations still hold their originals; these links are expendable.
        for _, parked in backups[len(replaced):]:
            if parked is not None:
                _remove_quietly(parked)
        for staged_path, _ in staged:
            _remove_quietly(staged_path)
    return {"json": json_path, "markdown": markdown_path}
