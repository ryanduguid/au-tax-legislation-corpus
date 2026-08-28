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


def _normalised_headers(headers: Mapping[str, str]) -> dict[str, str]:
    selected: dict[str, str] = {}
    for name, value in headers.items():
        lower_name = name.lower()
        if lower_name in RETAINED_HEADER_NAMES:
            selected[lower_name] = _required_text(value, f"response header {lower_name}")
    return {name: selected[name] for name in sorted(selected)}


def _version_row(exchange: RegisterExchange, title_id: str) -> dict[str, Any]:
    _utc_timestamp(exchange.checked_at, "exchange checked_at")
    if exchange.status != 200 or exchange.error_category is not None:
        raise CaptureRegisterError("Register current-version lookup failed.")
    if not isinstance(exchange.attempts, int) or isinstance(exchange.attempts, bool) or exchange.attempts < 1:
        raise CaptureRegisterError("Register exchange attempt count is invalid.")
    if not isinstance(exchange.body, bytes) or len(exchange.body) > MAX_RESPONSE_BYTES:
        raise CaptureRegisterError("Register response body is missing or too large.")
    headers = _normalised_headers(exchange.headers)
    content_type = headers.get("content-type", "").split(";", 1)[0].strip().lower()
    if content_type != "application/json":
        raise CaptureRegisterError("Register response media type is unsupported.")
    try:
        document = json.loads(exchange.body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise CaptureRegisterError("Register response is invalid JSON.") from exc
    if not isinstance(document, dict) or set(document) != {"@odata.context", "value"}:
        raise CaptureRegisterError("Register response has an invalid OData envelope.")
    if document["@odata.context"] != ODATA_CONTEXT:
        raise CaptureRegisterError("Register response has an invalid OData context.")
    values = document["value"]
    if not isinstance(values, list) or len(values) != 1 or not isinstance(values[0], dict):
        raise CaptureRegisterError("Register response has an invalid version row count.")
    row = values[0]
    if set(row) != ROW_FIELDS:
        raise CaptureRegisterError("Register response version row has an invalid shape.")
    if row["titleId"] != title_id:
        raise CaptureRegisterError("Register response title does not match the request.")
    if row["isCurrent"] is not True or row["status"] != "InForce":
        raise CaptureRegisterError("Register response does not contain an in-force current version.")
    return row


def _start_date(value: Any) -> str:
    text = _required_text(value, "Register version start")
    try:
        parsed = dt.datetime.fromisoformat(text)
    except ValueError as exc:
        raise CaptureRegisterError("Register version start is invalid.") from exc
    return parsed.date().isoformat()


def _request_record(
    *,
    role: str,
    url: str,
    exchange: RegisterExchange,
    evidence_path: str,
    response_sha256: str,
) -> dict[str, Any]:
    return {
        "role": role,
        "url": url,
        "checked_at": exchange.checked_at,
        "http_status": exchange.status,
        "transport_error_category": exchange.error_category,
        "attempt_count": exchange.attempts,
        "response_headers": _normalised_headers(exchange.headers),
        "response_length": len(exchange.body or b""),
        "response_sha256": response_sha256,
        "evidence_path": evidence_path,
    }


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
        title_id = title["register_id"]
        url = _current_url(title_id)
        exchange = session.get(url)
        row = _version_row(exchange, title_id)
        observed_date = _start_date(row["start"])
        observed_number = _required_text(
            row["compilationNumber"], "Register compilation number"
        )
        document_id = validate_register_id(row["registerId"])
        _required_text(row["registeredAt"], "Register registration timestamp")
        if (
            observed_number != title["compilation_number"]
            or observed_date != title["compilation_date"]
        ):
            raise CaptureRegisterError("Register compilation does not match the baseline yet.")

        body = exchange.body
        assert body is not None
        response_sha256 = _sha256_id(body)
        response_hex = response_sha256.removeprefix("sha256:")
        relative_evidence = f"evidence/sha256-{response_hex}.json"
        evidence[relative_evidence] = body
        result = {
            "register_id": title_id,
            "collection": title["collection"],
            "checked_at": exchange.checked_at,
            "state": "UNCHANGED",
            "error_category": None,
            "requests": [
                _request_record(
                    role="current",
                    url=url,
                    exchange=exchange,
                    evidence_path=relative_evidence,
                    response_sha256=response_sha256,
                )
            ],
        }
        results.append(result)
        result_sha256 = _sha256_id(_json_bytes(result))
        observations.append(
            {
                "register_id": title_id,
                "collection": title["collection"],
                "state": "UNCHANGED",
                "evidence_id": (
                    f"frl:{title_id}:{result_sha256.removeprefix('sha256:')[:32]}"
                ),
                "observed_compilation_number": None,
                "observed_compilation_date": None,
                "observed_register_document_id": None,
                "current_version_start": None,
                "evidence_url": title["register_page"],
                "checked_at": exchange.checked_at,
                "error_category": None,
                "capture_result_sha256": result_sha256,
                "primary_response_sha256": response_sha256,
                "primary_response_media_type": "application/json",
            }
        )

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
        "run_status": "VERIFIED",
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
