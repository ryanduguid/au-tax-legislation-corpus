"""Capture immutable live metadata evidence from the Federal Register.

The public function owns the whole capture transaction.  Callers provide a
rich corpus manifest and an absent destination; tests replace only the network
session, leaving request construction, interpretation and evidence storage in
this module.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import shutil
import stat
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol
from urllib.parse import quote, urlencode

from .corpus_paths import is_reparse_point
from .corpus_paths import register_id as validate_register_id


SOURCE_API = "https://api.prod.legislation.gov.au/v1/"
REGISTER_SITE = "https://www.legislation.gov.au"
CAPTURE_SCHEMA = "au-tax-register-capture.v1"
OBSERVATION_SCHEMA = "au-tax-register-observation.v4"
OBSERVATION_SCOPE = "au-primary-tax-legislation.v4"
ODATA_CONTEXT = (
    f"{SOURCE_API}$metadata#Versions("
    "titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)"
)
SELECT_FIELDS = (
    "titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt"
)
ROW_FIELDS = {
    "titleId",
    "start",
    "compilationNumber",
    "registerId",
    "isCurrent",
    "status",
    "registeredAt",
}
RETAINED_HEADER_NAMES = {
    "date",
    "content-type",
    "odata-version",
    "x-frl-version",
}
MAX_RESPONSE_BYTES = 256 * 1024
MAX_MANIFEST_BYTES = 8 * 1024 * 1024


class CaptureRegisterError(ValueError):
    """Raised when a complete, self-consistent capture cannot be produced."""


class _DuplicateJsonMemberError(ValueError):
    """Internal signal for an ambiguous JSON object at any depth."""


def _reject_duplicate_json_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateJsonMemberError
        result[key] = value
    return result


@dataclass(frozen=True)
class RegisterExchange:
    """One bounded exchange returned by a Register session."""

    checked_at: str
    status: int | None
    headers: Mapping[str, str]
    body: bytes | None
    attempts: int
    error_category: str | None = None


class RegisterSession(Protocol):
    """The complete external-I/O seam used by ``capture_register_run``."""

    observed_at: str

    def get(self, url: str) -> RegisterExchange:
        """Return one already-retried, bounded exchange for *url*."""


def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def _sha256_id(content: bytes) -> str:
    return f"sha256:{hashlib.sha256(content).hexdigest()}"


def _required_text(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise CaptureRegisterError(f"{field} must be a non-empty string.")
    text = value.strip()
    if any(ord(character) < 32 or ord(character) == 127 for character in text):
        raise CaptureRegisterError(f"{field} must not contain control characters.")
    return text


def _date(value: Any, field: str) -> str:
    text = _required_text(value, field)
    try:
        parsed = dt.date.fromisoformat(text)
    except ValueError as exc:
        raise CaptureRegisterError(f"{field} must be an ISO calendar date.") from exc
    if parsed.isoformat() != text:
        raise CaptureRegisterError(f"{field} must be an ISO calendar date.")
    return text


def _utc_timestamp(value: Any, field: str) -> str:
    text = _required_text(value, field)
    if not text.endswith("Z"):
        raise CaptureRegisterError(f"{field} must be a UTC timestamp ending in Z.")
    try:
        dt.datetime.fromisoformat(text[:-1] + "+00:00")
    except ValueError as exc:
        raise CaptureRegisterError(f"{field} must be a UTC timestamp ending in Z.") from exc
    return text


def _load_manifest(path: Path) -> list[dict[str, Any]]:
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    try:
        details = os.lstat(path)
    except OSError as exc:
        raise CaptureRegisterError("manifest must be an ordinary file.") from exc
    if (
        not stat.S_ISREG(details.st_mode)
        or os.path.islink(path)
        or is_junction(path)
        or is_reparse_point(path)
    ):
        raise CaptureRegisterError("manifest must be an ordinary file.")
    if details.st_size > MAX_MANIFEST_BYTES:
        raise CaptureRegisterError("manifest exceeds the 8 MiB size limit.")
    try:
        with open(path, "rb") as source:
            raw = source.read(MAX_MANIFEST_BYTES + 1)
    except OSError as exc:
        raise CaptureRegisterError("manifest could not be read.") from exc
    if len(raw) > MAX_MANIFEST_BYTES:
        raise CaptureRegisterError("manifest exceeds the 8 MiB size limit.")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise CaptureRegisterError("manifest must be strict UTF-8.") from exc
    try:
        value = json.loads(text, object_pairs_hook=_reject_duplicate_json_members)
    except _DuplicateJsonMemberError as exc:
        raise CaptureRegisterError("manifest contains a duplicate JSON member.") from exc
    except json.JSONDecodeError as exc:
        raise CaptureRegisterError("manifest is invalid JSON.") from exc
    if not isinstance(value, list) or not value:
        raise CaptureRegisterError("manifest must be a non-empty array.")
    rows: list[dict[str, Any]] = []
    for index, item in enumerate(value, start=1):
        if not isinstance(item, dict):
            raise CaptureRegisterError(f"manifest title {index} must be an object.")
        rows.append(item)
    return rows


def _project_baseline(rows: list[dict[str, Any]]) -> dict[str, Any]:
    titles: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, row in enumerate(rows, start=1):
        try:
            title_id = validate_register_id(row.get("id"))
        except ValueError as exc:
            raise CaptureRegisterError(str(exc)) from exc
        if title_id in seen:
            raise CaptureRegisterError("manifest contains duplicate Register identifiers.")
        seen.add(title_id)
        collection = _required_text(row.get("collection"), f"manifest title {index} collection")
        if collection not in {"Act", "LegislativeInstrument", "NotifiableInstrument"}:
            raise CaptureRegisterError(f"manifest title {index} collection is unsupported.")
        compilation_date = _date(
            row.get("versionStart"), f"manifest title {index} versionStart"
        )
        source_url = _required_text(
            row.get("sourceUrl"), f"manifest title {index} sourceUrl"
        )
        expected_source_url = (
            f"{REGISTER_SITE}/{title_id}/{compilation_date}/{compilation_date}/"
            "text/original/epub"
        )
        if source_url != expected_source_url:
            raise CaptureRegisterError(
                f"manifest title {index} sourceUrl does not match its title and version."
            )
        current = row.get("version_is_current", True)
        if not isinstance(current, bool):
            raise CaptureRegisterError(
                f"manifest title {index} version_is_current must be a boolean."
            )
        current_start_value = row.get("current_version_start")
        current_start = (
            _date(current_start_value, f"manifest title {index} current_version_start")
            if current_start_value is not None
            else None
        )
        if not current and current_start is None:
            raise CaptureRegisterError(
                f"manifest title {index} current_version_start is required when not current."
            )
        titles.append(
            {
                "register_id": title_id,
                "name": _required_text(row.get("name"), f"manifest title {index} name"),
                "collection": collection,
                "compilation_number": _required_text(
                    row.get("compilationNumber"),
                    f"manifest title {index} compilationNumber",
                ),
                "compilation_date": compilation_date,
                "version_is_current": current,
                "current_version_start": current_start,
                "retrieved": _date(
                    row.get("retrieved"), f"manifest title {index} retrieved"
                ),
                "source_url": source_url,
                "register_page": f"{REGISTER_SITE}/{title_id}/latest/text",
            }
        )
    titles.sort(key=lambda title: (title["register_id"], title["collection"]))
    return {
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": max(title["retrieved"] for title in titles),
        "source": "Federal Register of Legislation",
        "source_api": SOURCE_API,
        "titles": titles,
    }


def _current_url(title_id: str) -> str:
    query = urlencode(
        [
            ("$top", "1"),
            ("$filter", f"titleId eq '{title_id}' and isCurrent eq true"),
            ("$select", SELECT_FIELDS),
        ],
        quote_via=quote,
    )
    return f"{SOURCE_API}versions?{query}"


def _history_url(title_id: str) -> str:
    query = urlencode(
        [
            ("$top", "1"),
            ("$filter", f"titleId eq '{title_id}'"),
            ("$orderby", "start desc"),
            ("$select", SELECT_FIELDS),
        ],
        quote_via=quote,
    )
    return f"{SOURCE_API}versions?{query}"


def _normalised_headers(headers: Mapping[str, str]) -> dict[str, str]:
    selected: dict[str, str] = {}
    for name, value in headers.items():
        if not isinstance(name, str):
            raise CaptureRegisterError("response header names must be strings.")
        lower_name = name.lower()
        if lower_name in RETAINED_HEADER_NAMES:
            if lower_name in selected:
                raise CaptureRegisterError("response contains an ambiguous selected header.")
            selected[lower_name] = _required_text(value, f"response header {lower_name}")
    return {name: selected[name] for name in sorted(selected)}


def _version_timestamp(value: Any, field: str) -> str:
    text = _required_text(value, field)
    try:
        dt.datetime.fromisoformat(text)
    except ValueError as exc:
        raise CaptureRegisterError(f"{field} is invalid.") from exc
    return text


def _start_date(value: Any) -> str:
    text = _required_text(value, "Register version start")
    try:
        parsed = dt.datetime.fromisoformat(text)
    except ValueError as exc:
        raise CaptureRegisterError("Register version start is invalid.") from exc
    return parsed.date().isoformat()


@dataclass(frozen=True)
class _EvaluatedExchange:
    request: dict[str, Any]
    rows: list[dict[str, Any]] | None
    error_category: str | None
    retained_body: bytes | None
    media_type: str | None


def _validated_row(row: Any, title_id: str, role: str) -> dict[str, Any]:
    if not isinstance(row, dict) or set(row) != ROW_FIELDS:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    if row["titleId"] != title_id:
        raise CaptureRegisterError("IDENTITY_MISMATCH")
    try:
        _start_date(row["start"])
    except CaptureRegisterError as exc:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE") from exc
    if not isinstance(row["isCurrent"], bool):
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    if row["status"] not in {"InForce", "Ceased", "Repealed", "NeverEffective"}:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    if role == "current" and (row["isCurrent"] is not True or row["status"] != "InForce"):
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    if role == "history" and row["isCurrent"] is not False:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")

    document_id = row["registerId"]
    compilation_number = row["compilationNumber"]
    registered_at = row["registeredAt"]
    if document_id is None:
        if registered_at is not None:
            raise CaptureRegisterError("INVALID_ODATA_SHAPE")
        if compilation_number is not None:
            try:
                _required_text(compilation_number, "Register compilation number")
            except CaptureRegisterError as exc:
                raise CaptureRegisterError("INVALID_ODATA_SHAPE") from exc
    else:
        try:
            validate_register_id(document_id)
        except ValueError as exc:
            raise CaptureRegisterError("INVALID_ODATA_SHAPE") from exc
        try:
            _required_text(compilation_number, "Register compilation number")
            _version_timestamp(registered_at, "Register registration timestamp")
        except CaptureRegisterError as exc:
            raise CaptureRegisterError("INVALID_ODATA_SHAPE") from exc
    return row


def _parse_odata_rows(body: bytes, title_id: str, role: str) -> list[dict[str, Any]]:
    try:
        text = body.decode("utf-8")
        document = json.loads(text, object_pairs_hook=_reject_duplicate_json_members)
    except (UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
        raise CaptureRegisterError("INVALID_JSON") from exc
    if not isinstance(document, dict) or set(document) != {"@odata.context", "value"}:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    if document["@odata.context"] != ODATA_CONTEXT:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    values = document["value"]
    if not isinstance(values, list) or len(values) > 1:
        raise CaptureRegisterError("INVALID_ODATA_SHAPE")
    return [_validated_row(row, title_id, role) for row in values]


def _evaluate_exchange(
    exchange: RegisterExchange,
    *,
    role: str,
    url: str,
    title_id: str,
) -> _EvaluatedExchange:
    try:
        checked_at = _utc_timestamp(exchange.checked_at, "exchange checked_at")
        if (
            not isinstance(exchange.attempts, int)
            or isinstance(exchange.attempts, bool)
            or not 1 <= exchange.attempts <= 3
        ):
            raise CaptureRegisterError("invalid attempt count")
        headers = _normalised_headers(exchange.headers)
    except (AttributeError, CaptureRegisterError):
        checked_at = "1970-01-01T00:00:00Z"
        headers = {}
        problem = "INVALID_EXCHANGE"
    else:
        problem = None

    response_sha256: str | None = None
    evidence_path: str | None = None
    response_length: int | None = None
    retained_body: bytes | None = None
    media_type: str | None = None
    rows: list[dict[str, Any]] | None = None

    status = exchange.status
    transport_error = exchange.error_category
    if problem is None:
        if status is None:
            if transport_error != "TRANSPORT_ERROR" or exchange.body is not None:
                problem = "INVALID_EXCHANGE"
            else:
                problem = "TRANSPORT_ERROR"
        elif (
            not isinstance(status, int)
            or isinstance(status, bool)
            or not 100 <= status <= 599
            or transport_error is not None
        ):
            problem = "INVALID_EXCHANGE"
        elif status != 200:
            problem = "HTTP_STATUS"
        elif not isinstance(exchange.body, bytes):
            problem = "INVALID_EXCHANGE"
        elif len(exchange.body) > MAX_RESPONSE_BYTES:
            problem = "RESPONSE_TOO_LARGE"
        else:
            retained_body = exchange.body
            response_length = len(retained_body)
            response_sha256 = _sha256_id(retained_body)
            evidence_path = (
                "evidence/sha256-"
                f"{response_sha256.removeprefix('sha256:')}.json"
            )
            raw_content_type = headers.get("content-type")
            if raw_content_type is not None:
                media_type = raw_content_type.split(";", 1)[0].strip().lower()
            if media_type != "application/json":
                problem = "UNSUPPORTED_MEDIA_TYPE"
            else:
                try:
                    rows = _parse_odata_rows(retained_body, title_id, role)
                except CaptureRegisterError as exc:
                    problem = str(exc)

    request = {
        "role": role,
        "url": url,
        "checked_at": checked_at,
        "http_status": status if isinstance(status, int) and not isinstance(status, bool) else None,
        "transport_error_category": (
            "TRANSPORT_ERROR" if problem == "TRANSPORT_ERROR" else None
        ),
        "attempt_count": (
            exchange.attempts
            if isinstance(exchange.attempts, int) and not isinstance(exchange.attempts, bool)
            else 0
        ),
        "response_headers": headers,
        "response_length": response_length,
        "response_sha256": response_sha256,
        "evidence_path": evidence_path,
    }
    return _EvaluatedExchange(
        request=request,
        rows=rows,
        error_category=problem,
        retained_body=retained_body,
        media_type=media_type,
    )


def _retain_exchange_evidence(
    evaluated: _EvaluatedExchange, evidence: dict[str, bytes]
) -> None:
    path = evaluated.request["evidence_path"]
    body = evaluated.retained_body
    if path is None or body is None:
        return
    prior = evidence.setdefault(path, body)
    if prior != body:
        raise CaptureRegisterError("response digest collision detected")


def _observe_title(
    title: dict[str, Any],
    session: RegisterSession,
    evidence: dict[str, bytes],
) -> tuple[dict[str, Any], dict[str, Any]]:
    title_id = title["register_id"]
    current_url = _current_url(title_id)
    current = _evaluate_exchange(
        session.get(current_url), role="current", url=current_url, title_id=title_id
    )
    _retain_exchange_evidence(current, evidence)
    requests = [current.request]
    state = "LOOKUP_FAILED"
    error_category = current.error_category
    observed_number: str | None = None
    observed_date: str | None = None
    observed_document_id: str | None = None
    current_version_start: str | None = None

    if error_category is None and current.rows is not None:
        if not current.rows:
            history_url = _history_url(title_id)
            history = _evaluate_exchange(
                session.get(history_url),
                role="history",
                url=history_url,
                title_id=title_id,
            )
            _retain_exchange_evidence(history, evidence)
            requests.append(history.request)
            if history.error_category is not None:
                error_category = history.error_category
            elif not history.rows:
                error_category = "NO_VERSION_EVIDENCE"
            else:
                state = "NO_LONGER_IN_FORCE"
                error_category = None
        else:
            row = current.rows[0]
            current_date = _start_date(row["start"])
            if current_date < title["compilation_date"]:
                error_category = "INCONSISTENT_CHRONOLOGY"
            elif row["registerId"] is None:
                state = "CURRENT_NO_PUBLISHED_COMPILATION"
                current_version_start = current_date
                error_category = None
            else:
                compilation_number = _required_text(
                    row["compilationNumber"], "Register compilation number"
                )
                if (
                    compilation_number == title["compilation_number"]
                    and current_date == title["compilation_date"]
                ):
                    state = "UNCHANGED"
                    error_category = None
                elif (
                    compilation_number != title["compilation_number"]
                    and current_date > title["compilation_date"]
                ):
                    state = "SUPERSEDED"
                    error_category = None
                    observed_number = compilation_number
                    observed_date = current_date
                    observed_document_id = row["registerId"]
                else:
                    error_category = "INCONSISTENT_CHRONOLOGY"

    checked_at = requests[-1]["checked_at"]
    result = {
        "register_id": title_id,
        "collection": title["collection"],
        "checked_at": checked_at,
        "state": state,
        "error_category": error_category,
        "requests": requests,
    }
    result_sha256 = _sha256_id(_json_bytes(result))
    observation = {
        "register_id": title_id,
        "collection": title["collection"],
        "state": state,
        "evidence_id": f"frl:{title_id}:{result_sha256.removeprefix('sha256:')[:32]}",
        "observed_compilation_number": observed_number,
        "observed_compilation_date": observed_date,
        "observed_register_document_id": observed_document_id,
        "current_version_start": current_version_start,
        "evidence_url": title["register_page"],
        "checked_at": checked_at,
        "error_category": error_category,
        "capture_result_sha256": result_sha256,
        "primary_response_sha256": current.request["response_sha256"],
        "primary_response_media_type": (
            current.media_type if current.request["response_sha256"] is not None else None
        ),
    }
    return result, observation


def _write_new(path: Path, content: bytes) -> None:
    with open(path, "xb") as target:
        target.write(content)


def capture_register_run(
    manifest_path: str | Path,
    destination: str | Path,
    *,
    session: RegisterSession | None = None,
) -> dict[str, Path]:
    """Capture complete Register evidence into one new immutable directory."""
    if session is None:
        raise CaptureRegisterError("the production Register session is not available yet")
    observed_at = _utc_timestamp(session.observed_at, "session observed_at")
    manifest = Path(os.path.abspath(os.fspath(manifest_path)))
    output = Path(os.path.abspath(os.fspath(destination)))
    baseline = _project_baseline(_load_manifest(manifest))
    if output.exists() or output.is_symlink():
        raise CaptureRegisterError(f"capture destination must not exist: {output}.")
    if not output.parent.is_dir():
        raise CaptureRegisterError(f"capture destination parent does not exist: {output.parent}.")

    baseline_content = _json_bytes(baseline)
    baseline_sha256 = _sha256_id(baseline_content)
    evidence: dict[str, bytes] = {}
    results: list[dict[str, Any]] = []
    observations: list[dict[str, Any]] = []

    for title in baseline["titles"]:
        result, item = _observe_title(title, session, evidence)
        results.append(result)
        observations.append(item)

    expected_ids = [title["register_id"] for title in baseline["titles"]]
    capture = {
        "schema_version": CAPTURE_SCHEMA,
        "mode": "live",
        "observed_at": observed_at,
        "source_api": SOURCE_API,
        "baseline_sha256": baseline_sha256,
        "expected_register_ids": expected_ids,
        "complete": len(results) == len(expected_ids),
        "results": results,
    }
    capture_content = _json_bytes(capture)
    observation = {
        "schema_version": OBSERVATION_SCHEMA,
        "mode": "live",
        "observed_at": observed_at,
        "scope_id": OBSERVATION_SCOPE,
        "baseline_sha256": baseline_sha256,
        "capture_sha256": _sha256_id(capture_content),
        "expected_register_ids": expected_ids,
        "complete": capture["complete"],
        "run_status": (
            "VERIFIED"
            if capture["complete"]
            and all(item["state"] != "LOOKUP_FAILED" for item in observations)
            else "BLOCKED"
        ),
        "observations": observations,
    }
    observation_content = _json_bytes(observation)

    prefix = f".{output.name}.register-capture-"
    staging = output.parent / f"{prefix}{uuid.uuid4().hex}.tmp"
    try:
        os.mkdir(staging, 0o700)
        evidence_directory = staging / "evidence"
        os.mkdir(evidence_directory, 0o700)
        _write_new(staging / "monitor-baseline.json", baseline_content)
        for relative_path, content in evidence.items():
            _write_new(staging / relative_path, content)
        _write_new(staging / "register-capture.json", capture_content)
        _write_new(staging / "register-observation.json", observation_content)
        if output.exists() or output.is_symlink():
            raise CaptureRegisterError(f"capture destination must not exist: {output}.")
        os.rename(staging, output)
    except OSError as exc:
        raise CaptureRegisterError(f"capture output could not be written: {exc}.") from exc
    finally:
        if staging.is_dir() and staging.parent == output.parent and staging.name.startswith(prefix):
            shutil.rmtree(staging)

    return {
        "baseline": output / "monitor-baseline.json",
        "capture": output / "register-capture.json",
        "observation": output / "register-observation.json",
    }
