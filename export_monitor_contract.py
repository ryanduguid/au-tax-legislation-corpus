"""Project corpus metadata into the monitor's fixed, review-only v1 inputs.

This is deliberately a producer-side adapter.  It does not import the monitor,
call the Federal Register, calculate an impact, or write a review decision.
The caller supplies already collected structured observation facts and receives
the monitor's reduced baseline plus its exact observation contract.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import stat
import time
import uuid
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


class ContractError(ValueError):
    """Raised when a source or observation cannot support monitor evidence."""


class _DuplicateJsonMemberError(ValueError):
    """Internal signal for a JSON object with ambiguous duplicate keys."""


def _reject_duplicate_json_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    payload: dict[str, Any] = {}
    for key, value in pairs:
        if key in payload:
            raise _DuplicateJsonMemberError
        payload[key] = value
    return payload


COLLECTIONS = {"Act", "LegislativeInstrument", "NotifiableInstrument"}
OBSERVATION_STATES = {
    "UNCHANGED",
    "SUPERSEDED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "NO_LONGER_IN_FORCE",
    "LOOKUP_FAILED",
}
BASELINE_FIELDS = {
    "register_id", "name", "collection", "compilation_number",
    "compilation_date", "version_is_current", "current_version_start",
    "retrieved", "source_url", "register_page",
}
OBSERVATION_FIELDS = {
    "register_id", "collection", "state", "observed_compilation_number",
    "observed_compilation_date", "observed_register_document_id",
    "current_version_start", "evidence_url", "checked_at", "error_category",
}
UTC_TIMESTAMP = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?Z")
PUBLISH_LOCK_FILENAME = ".monitor-contract.publish.lock"
PUBLISH_LOCK_TIMEOUT_SECONDS = 30
PUBLISH_LOCK_RETRY_SECONDS = 0.05


def _non_empty(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ContractError(f"{field} must be a non-empty string.")
    text = value.strip()
    if any(ord(char) < 32 or ord(char) == 127 for char in text):
        raise ContractError(f"{field} must not contain control characters.")
    return text


def _date(value: Any, field: str, *, nullable: bool = False) -> str | None:
    if value is None and nullable:
        return None
    text = _non_empty(value, field)
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", text):
        raise ContractError(f"{field} must be an ISO calendar date.")
    try:
        dt.date.fromisoformat(text)
    except ValueError as exc:
        raise ContractError(f"{field} must be an ISO calendar date.") from exc
    return text


def _utc_timestamp(value: Any, field: str) -> str:
    text = _non_empty(value, field)
    if UTC_TIMESTAMP.fullmatch(text) is None:
        raise ContractError(f"{field} must be an explicit UTC timestamp ending in Z.")
    try:
        dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ContractError(f"{field} must be an explicit UTC timestamp ending in Z.") from exc
    return text


def _https_url(value: Any, field: str) -> str:
    text = _non_empty(value, field)
    try:
        parsed = urlsplit(text)
        hostname = parsed.hostname
        _ = parsed.port
    except ValueError as exc:
        raise ContractError(f"{field} must be an https URL.") from exc
    if (
        parsed.scheme.lower() != "https"
        or not hostname
        or parsed.username is not None
        or parsed.password is not None
        or "\\" in parsed.netloc
        or any(char.isspace() for char in parsed.netloc)
    ):
        raise ContractError(f"{field} must be an https URL.")
    return text


def _object(value: Any, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{field} must be an object.")
    return value


def _load_json(path: str | Path, label: str) -> dict[str, Any]:
    try:
        with open(path, encoding="utf-8") as source:
            value = json.load(source, object_pairs_hook=_reject_duplicate_json_members)
    except _DuplicateJsonMemberError as exc:
        raise ContractError(f"{label} contains duplicate JSON members.") from exc
    except (OSError, json.JSONDecodeError) as exc:
        raise ContractError(f"{label} is unreadable JSON: {exc}") from exc
    return _object(value, label)


def project_baseline(sources: dict[str, Any]) -> dict[str, Any]:
    """Reduce a rich corpus `sources.json` to the monitor's exact baseline."""
    source = _object(sources, "sources")
    for field in ("corpus", "retrieved", "source", "source_api", "titles"):
        if field not in source:
            raise ContractError(f"sources is missing {field}.")
    corpus = _non_empty(source["corpus"], "sources corpus")
    retrieved = _date(source["retrieved"], "sources retrieved")
    source_name = _non_empty(source["source"], "sources source")
    if source_name != "Federal Register of Legislation":
        raise ContractError("sources source must be Federal Register of Legislation.")
    source_api = _https_url(source["source_api"], "sources source_api")
    if not isinstance(source["titles"], list) or not source["titles"]:
        raise ContractError("sources titles must be a non-empty list.")

    titles: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, raw in enumerate(source["titles"], start=1):
        title = _object(raw, f"sources title {index}")
        missing = BASELINE_FIELDS - set(title)
        if missing:
            raise ContractError(
                f"sources title {index} is missing required field(s): {', '.join(sorted(missing))}."
            )
        register_id = _non_empty(title["register_id"], f"sources title {index} register_id")
        if register_id in seen:
            raise ContractError("sources contains duplicate register_id values.")
        seen.add(register_id)
        collection = _non_empty(title["collection"], f"sources title {index} collection")
        if collection not in COLLECTIONS:
            raise ContractError(f"sources title {index} collection is unsupported.")
        current = title["version_is_current"]
        if not isinstance(current, bool):
            raise ContractError(f"sources title {index} version_is_current must be a boolean.")
        current_start = _date(
            title["current_version_start"],
            f"sources title {index} current_version_start",
            nullable=True,
        )
        if not current and current_start is None:
            raise ContractError(
                f"sources title {index} current_version_start is required when version_is_current is false."
            )
        titles.append(
            {
                "register_id": register_id,
                "name": _non_empty(title["name"], f"sources title {index} name"),
                "collection": collection,
                "compilation_number": _non_empty(
                    title["compilation_number"], f"sources title {index} compilation_number"
                ),
                "compilation_date": _date(
                    title["compilation_date"], f"sources title {index} compilation_date"
                ),
                "version_is_current": current,
                "current_version_start": current_start,
                "retrieved": _date(title["retrieved"], f"sources title {index} retrieved"),
                "source_url": _https_url(title["source_url"], f"sources title {index} source_url"),
                "register_page": _https_url(title["register_page"], f"sources title {index} register_page"),
            }
        )
    return {
        "corpus": corpus,
        "retrieved": retrieved,
        "source": "Federal Register of Legislation",
        "source_api": source_api,
        "titles": sorted(titles, key=lambda item: (item["register_id"], item["collection"])),
    }


def project_observation(
    baseline: dict[str, Any], facts: dict[str, Any]
) -> dict[str, Any]:
    """Validate structured facts and produce the monitor's exact v1 observation."""
    projected_baseline = project_baseline(baseline)
    raw = _object(facts, "observation facts")
    expected_fields = {"schema_version", "observed_at", "complete", "observations"}
    if set(raw) != expected_fields:
        raise ContractError("observation facts has an invalid shape.")
    schema_version = _non_empty(
        raw["schema_version"], "observation facts schema_version"
    )
    if schema_version != "au-tax-register-observation-facts.v1":
        raise ContractError("observation facts schema_version is unsupported.")
    observed_at = _utc_timestamp(raw["observed_at"], "observation facts observed_at")
    if not isinstance(raw["complete"], bool) or not isinstance(raw["observations"], list):
        raise ContractError("observation facts complete/observations fields are invalid.")
    expected_by_id = {title["register_id"]: title for title in projected_baseline["titles"]}
    observations: list[dict[str, Any]] = []
    seen: set[str] = set()
    required_by_state = {
        "UNCHANGED": set(),
        "SUPERSEDED": {
            "observed_compilation_number", "observed_compilation_date",
            "observed_register_document_id",
        },
        "CURRENT_NO_PUBLISHED_COMPILATION": {"current_version_start"},
        "NO_LONGER_IN_FORCE": set(),
        "LOOKUP_FAILED": {"error_category"},
    }
    conditional = {
        "observed_compilation_number", "observed_compilation_date",
        "observed_register_document_id", "current_version_start", "error_category",
    }
    for index, value in enumerate(raw["observations"], start=1):
        item = _object(value, f"observation fact {index}")
        if set(item) != OBSERVATION_FIELDS:
            raise ContractError(f"observation fact {index} has an invalid shape.")
        register_id = _non_empty(item["register_id"], f"observation fact {index} register_id")
        if register_id not in expected_by_id:
            raise ContractError("observation facts contains a register_id outside the baseline scope.")
        if register_id in seen:
            raise ContractError("observation facts contains duplicate register_id values.")
        seen.add(register_id)
        collection = _non_empty(item["collection"], f"observation fact {index} collection")
        if collection != expected_by_id[register_id]["collection"]:
            raise ContractError(f"observation fact {index} collection does not match baseline.")
        state = _non_empty(item["state"], f"observation fact {index} state")
        if state not in OBSERVATION_STATES:
            raise ContractError(f"observation fact {index} state is unsupported.")
        for field in conditional - required_by_state[state]:
            if item[field] is not None:
                raise ContractError(f"{state} requires null {field}.")
        projected = {
            "register_id": register_id,
            "collection": collection,
            "state": state,
            "observed_compilation_number": None,
            "observed_compilation_date": None,
            "observed_register_document_id": None,
            "current_version_start": None,
            "evidence_url": _https_url(item["evidence_url"], f"observation fact {index} evidence_url"),
            "checked_at": _utc_timestamp(item["checked_at"], f"observation fact {index} checked_at"),
            "error_category": None,
        }
        if state == "SUPERSEDED":
            projected["observed_compilation_number"] = _non_empty(
                item["observed_compilation_number"],
                f"observation fact {index} observed_compilation_number",
            )
            projected["observed_compilation_date"] = _date(
                item["observed_compilation_date"],
                f"observation fact {index} observed_compilation_date",
            )
            projected["observed_register_document_id"] = _non_empty(
                item["observed_register_document_id"],
                f"observation fact {index} observed_register_document_id",
            )
        elif state == "CURRENT_NO_PUBLISHED_COMPILATION":
            projected["current_version_start"] = _date(
                item["current_version_start"],
                f"observation fact {index} current_version_start",
            )
        elif state == "LOOKUP_FAILED":
            projected["error_category"] = _non_empty(
                item["error_category"], f"observation fact {index} error_category"
            )
        observations.append(projected)
    if raw["complete"] and seen != set(expected_by_id):
        raise ContractError("a complete observation must cover every baseline register_id exactly once.")
    return {
        "schema_version": "au-tax-register-observation.v1",
        "mode": "synthetic",
        "observed_at": observed_at,
        "expected_register_ids": sorted(expected_by_id),
        "complete": raw["complete"],
        "observations": sorted(observations, key=lambda item: (item["register_id"], item["collection"])),
    }


def _write_staged(path: Path, value: dict[str, Any]) -> Path:
    staged = path.with_name(f".{path.name}.monitor-contract-{uuid.uuid4().hex}.tmp")
    try:
        with open(staged, "x", encoding="utf-8", newline="\n") as target:
            json.dump(value, target, indent=2, sort_keys=True, ensure_ascii=False)
            target.write("\n")
        return staged
    except BaseException:
        try:
            staged.unlink()
        except OSError:
            pass
        raise


def _remove(path: Path | None) -> None:
    if path is not None:
        try:
            path.unlink()
        except FileNotFoundError:
            pass


def _validate_existing_destination(path: Path) -> None:
    """Reject an existing destination that is not an ordinary file."""
    try:
        details = os.lstat(path)
    except FileNotFoundError:
        return
    except OSError as exc:
        raise ContractError(
            f"monitor output path cannot be inspected: {path} ({exc})."
        ) from exc
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    is_reparse_point = bool(
        getattr(details, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    )
    if (
        not stat.S_ISREG(details.st_mode)
        or os.path.islink(path)
        or is_junction(path)
        or is_reparse_point
    ):
        raise ContractError(
            f"monitor output path must be absent or a regular file: {path}."
        )


class _OutputDirectoryLock:
    """An exclusive, recoverable publisher lock for one output directory."""

    def __init__(self, directory: Path) -> None:
        self.path = directory / PUBLISH_LOCK_FILENAME
        self._token: bytes | None = None
        self._recovery_required = False

    def retain_for_recovery(self) -> None:
        """Leave this writer's lock in place until an operator recovers the pair."""
        self._recovery_required = True

    def __enter__(self) -> _OutputDirectoryLock:
        deadline = time.monotonic() + PUBLISH_LOCK_TIMEOUT_SECONDS
        while True:
            try:
                descriptor = os.open(
                    self.path,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                    0o600,
                )
            except FileExistsError:
                if time.monotonic() >= deadline:
                    raise ContractError(
                        f"monitor output directory is locked by another writer: {self.path}."
                    )
                time.sleep(PUBLISH_LOCK_RETRY_SECONDS)
                continue
            token = f"pid={os.getpid()} token={uuid.uuid4().hex}".encode("ascii")
            try:
                os.write(descriptor, token)
            except BaseException:
                _remove(self.path)
                raise
            finally:
                os.close(descriptor)
            self._token = token
            return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        try:
            if (
                not self._recovery_required
                and self._token is not None
                and self.path.read_bytes() == self._token
            ):
                _remove(self.path)
        except OSError:
            pass
        return False


def _publish(staged: dict[str, Path], destinations: dict[str, Path]) -> None:
    try:
        with _OutputDirectoryLock(destinations["baseline"].parent) as publisher_lock:
            for destination in destinations.values():
                _validate_existing_destination(destination)
            backups: dict[str, Path | None] = {}
            promoted: list[str] = []
            recovery_required = False
            try:
                for name, destination in destinations.items():
                    backup = destination.with_name(f".{destination.name}.monitor-contract-{uuid.uuid4().hex}.bak")
                    if destination.exists():
                        os.replace(destination, backup)
                        backups[name] = backup
                    else:
                        backups[name] = None
                    os.replace(staged[name], destination)
                    promoted.append(name)
            except BaseException:
                rollback_errors: list[BaseException] = []
                for name in reversed(promoted):
                    try:
                        _remove(destinations[name])
                        if backups[name] is not None:
                            os.replace(backups[name], destinations[name])
                            backups[name] = None
                    except BaseException as rollback_error:
                        rollback_errors.append(rollback_error)
                for name, backup in backups.items():
                    if name not in promoted and backup is not None:
                        try:
                            os.replace(backup, destinations[name])
                            backups[name] = None
                        except BaseException as rollback_error:
                            rollback_errors.append(rollback_error)
                if rollback_errors:
                    recovery_required = True
                    publisher_lock.retain_for_recovery()
                    raise ContractError(
                        "monitor output publication rollback failed; retain the lock and any remaining .bak recovery files for operator recovery."
                    ) from rollback_errors[0]
                raise
            finally:
                if not recovery_required:
                    for backup in backups.values():
                        _remove(backup)
    finally:
        for path in staged.values():
            _remove(path)


def publish_pair(
    sources_path: str | Path, facts_path: str | Path, output_directory: str | Path
) -> dict[str, Path]:
    """Project, serialise and replace both monitor inputs as one recoverable pair."""
    directory = Path(output_directory).resolve()
    destinations = {
        "baseline": directory / "monitor-baseline.json",
        "observation": directory / "register-observation.json",
    }
    inputs = {
        "sources": Path(sources_path).resolve(),
        "observation facts": Path(facts_path).resolve(),
    }
    for label, path in inputs.items():
        if path in destinations.values():
            raise ContractError(
                f"monitor output would replace an input ({label}): {path}."
            )
    for destination in destinations.values():
        _validate_existing_destination(destination)
    baseline = project_baseline(_load_json(inputs["sources"], "sources"))
    observation = project_observation(
        baseline, _load_json(inputs["observation facts"], "observation facts")
    )
    directory.mkdir(parents=True, exist_ok=True)
    staged: dict[str, Path] = {}
    try:
        for name, path, value in (
            ("baseline", destinations["baseline"], baseline),
            ("observation", destinations["observation"], observation),
        ):
            staged[name] = _write_staged(path, value)
    except BaseException:
        for path in staged.values():
            _remove(path)
        raise
    _publish(staged, destinations)
    return destinations


def main(argv: list[str] | None = None) -> int:
    """Write monitor inputs from an existing corpus index and supplied facts."""
    parser = argparse.ArgumentParser(
        description="Project corpus sources and observation facts into monitor v1 inputs."
    )
    parser.add_argument("sources", type=Path, help="completed corpus sources.json")
    parser.add_argument("facts", type=Path, help="structured observation facts JSON")
    parser.add_argument("--out", required=True, type=Path, help="directory for the monitor input pair")
    args = parser.parse_args(argv)
    try:
        paths = publish_pair(args.sources, args.facts, args.out)
    except ContractError as exc:
        parser.error(str(exc))
    print(f"monitor baseline: {paths['baseline']}")
    print(f"register observation: {paths['observation']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
