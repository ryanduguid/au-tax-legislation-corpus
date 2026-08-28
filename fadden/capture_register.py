"""Capture immutable live metadata evidence from the Federal Register.

The public function owns the whole capture transaction.  Callers provide a
rich corpus manifest and an absent destination; tests replace only the network
session, leaving request construction, interpretation and evidence storage in
this module.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import http.client
import json
import os
import re
import shutil
import stat
import time
import urllib.error
import urllib.request
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol, Sequence
from urllib.parse import quote, urlencode, urlsplit

from .corpus_paths import is_reparse_point
from .corpus_paths import register_id as validate_register_id
from .http_fetch import TIMEOUT, UA


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
MAX_ATTEMPTS = 3
RETRY_DELAY_SECONDS = 6.0
REQUEST_DELAY_SECONDS = 1.5
SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
EVIDENCE_NAME = re.compile(r"sha256-([0-9a-f]{64})\.json\Z")
SAFE_OUTPUT_LEAF = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,99}\Z")
OBSERVATION_STATES = {
    "UNCHANGED",
    "SUPERSEDED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "NO_LONGER_IN_FORCE",
    "LOOKUP_FAILED",
}
FAILURE_CATEGORIES = {
    "TRANSPORT_ERROR",
    "HTTP_STATUS",
    "RESPONSE_TOO_LARGE",
    "UNSUPPORTED_MEDIA_TYPE",
    "INVALID_JSON",
    "INVALID_ODATA_SHAPE",
    "IDENTITY_MISMATCH",
    "INCONSISTENT_CHRONOLOGY",
    "INVALID_EXCHANGE",
    "NO_VERSION_EVIDENCE",
}


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


def _utc_now() -> str:
    return (
        dt.datetime.now(dt.timezone.utc)
        .isoformat(timespec="seconds")
        .replace("+00:00", "Z")
    )


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Turn every redirect into its original HTTP response status."""

    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: Any,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> None:
        return None


def _selected_response_headers(headers: Any) -> dict[str, str]:
    """Copy only the four contract headers before a response object closes."""
    selected: dict[str, str] = {}
    seen: set[str] = set()
    try:
        items = headers.items()
    except AttributeError:
        return selected
    for name, value in items:
        if not isinstance(name, str) or name.lower() not in RETAINED_HEADER_NAMES:
            continue
        key = name
        if name.lower() in seen:
            key = name.swapcase()
        seen.add(name.lower())
        selected[key] = value if isinstance(value, str) else str(value)
    return selected


def _retryable_status(status: int) -> bool:
    return status in {408, 429} or 500 <= status <= 599


class _HttpsRegisterSession:
    """Paced standard-library HTTPS adapter for the exact Register host."""

    def __init__(self) -> None:
        self.observed_at = _utc_now()
        self._opener = urllib.request.build_opener(_NoRedirectHandler())
        self._last_request_started: float | None = None

    def _pace(self) -> None:
        now = time.monotonic()
        if self._last_request_started is not None:
            remaining = REQUEST_DELAY_SECONDS - (now - self._last_request_started)
            if remaining > 0:
                time.sleep(remaining)
                now = time.monotonic()
        self._last_request_started = now

    def _request(self, url: str) -> urllib.request.Request:
        try:
            parsed = urlsplit(url)
            port = parsed.port
        except ValueError as exc:
            raise CaptureRegisterError("Register request URL is invalid.") from exc
        if (
            parsed.scheme != "https"
            or parsed.hostname != "api.prod.legislation.gov.au"
            or port is not None
            or parsed.username is not None
            or parsed.password is not None
            or parsed.fragment
            or parsed.path != "/v1/versions"
        ):
            raise CaptureRegisterError("Register request URL is outside the fixed API boundary.")
        return urllib.request.Request(url, headers={"User-Agent": UA}, method="GET")

    def get(self, url: str) -> RegisterExchange:
        request = self._request(url)
        for attempt in range(1, MAX_ATTEMPTS + 1):
            self._pace()
            try:
                with self._opener.open(request, timeout=TIMEOUT) as response:
                    status = response.status
                    headers = _selected_response_headers(response.headers)
                    body = (
                        response.read(MAX_RESPONSE_BYTES + 1)
                        if status == 200
                        else None
                    )
            except urllib.error.HTTPError as exc:
                status = exc.code
                headers = _selected_response_headers(exc.headers)
                exc.close()
                if _retryable_status(status) and attempt < MAX_ATTEMPTS:
                    time.sleep(RETRY_DELAY_SECONDS)
                    continue
                return RegisterExchange(
                    checked_at=_utc_now(),
                    status=status,
                    headers=headers,
                    body=None,
                    attempts=attempt,
                )
            except (urllib.error.URLError, http.client.HTTPException, OSError):
                if attempt < MAX_ATTEMPTS:
                    time.sleep(RETRY_DELAY_SECONDS)
                    continue
                return RegisterExchange(
                    checked_at=_utc_now(),
                    status=None,
                    headers={},
                    body=None,
                    attempts=attempt,
                    error_category="TRANSPORT_ERROR",
                )

            if _retryable_status(status) and attempt < MAX_ATTEMPTS:
                time.sleep(RETRY_DELAY_SECONDS)
                continue
            return RegisterExchange(
                checked_at=_utc_now(),
                status=status,
                headers=headers,
                body=body,
                attempts=attempt,
            )
        raise AssertionError("bounded Register attempt loop did not return")


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


def _details_are_reparse_point(details: os.stat_result) -> bool:
    return bool(
        getattr(details, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    )


def _path_is_junction(path: Path) -> bool:
    return getattr(os.path, "isjunction", lambda _path: False)(path)


def _require_ordinary_directory(path: Path, label: str) -> None:
    try:
        details = os.lstat(path)
    except OSError as exc:
        raise CaptureRegisterError(f"{label} must be an ordinary directory.") from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(path)
        or _path_is_junction(path)
        or _details_are_reparse_point(details)
    ):
        raise CaptureRegisterError(f"{label} must be an ordinary directory.")


def _require_ordinary_ancestors(path: Path, label: str) -> None:
    current = path
    while True:
        try:
            details = os.lstat(current)
        except OSError as exc:
            raise CaptureRegisterError(f"{label} must have ordinary ancestors.") from exc
        if (
            not stat.S_ISDIR(details.st_mode)
            or os.path.islink(current)
            or _path_is_junction(current)
            or _details_are_reparse_point(details)
        ):
            raise CaptureRegisterError(f"{label} must have ordinary ancestors.")
        parent = current.parent
        if parent == current:
            return
        current = parent


def _load_manifest(path: Path) -> list[dict[str, Any]]:
    _require_ordinary_ancestors(path.parent, "manifest")
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
            if (
                isinstance(exchange.attempts, int)
                and not isinstance(exchange.attempts, bool)
                and 1 <= exchange.attempts <= MAX_ATTEMPTS
            )
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


def _require_staged_directory(path: Path) -> None:
    try:
        details = os.lstat(path)
    except OSError as exc:
        raise CaptureRegisterError("staged capture directory is unavailable") from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(path)
        or getattr(os.path, "isjunction", lambda _path: False)(path)
        or is_reparse_point(path)
    ):
        raise CaptureRegisterError("staged capture contains a non-ordinary directory")


def _read_staged_file(path: Path) -> bytes:
    try:
        details = os.lstat(path)
    except OSError as exc:
        raise CaptureRegisterError("staged capture file is unavailable") from exc
    if (
        not stat.S_ISREG(details.st_mode)
        or os.path.islink(path)
        or getattr(os.path, "isjunction", lambda _path: False)(path)
        or is_reparse_point(path)
    ):
        raise CaptureRegisterError("staged capture contains a non-ordinary file")
    try:
        with open(path, "rb") as source:
            return source.read()
    except OSError as exc:
        raise CaptureRegisterError("staged capture file could not be read") from exc


def _load_generated_object(path: Path) -> tuple[bytes, dict[str, Any]]:
    content = _read_staged_file(path)
    try:
        document = json.loads(
            content.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_members
        )
    except (UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
        raise CaptureRegisterError("staged capture JSON is invalid") from exc
    if not isinstance(document, dict) or content != _json_bytes(document):
        raise CaptureRegisterError("staged capture JSON bytes are non-canonical")
    return content, document


def _require_exact_fields(value: dict[str, Any], fields: set[str]) -> None:
    if set(value) != fields:
        raise CaptureRegisterError("staged capture object shape is invalid")


def _validate_staged_graph_inner(staging: Path) -> None:
    _require_staged_directory(staging)
    try:
        root_entries = {path.name for path in staging.iterdir()}
    except OSError as exc:
        raise CaptureRegisterError("staged capture layout could not be read") from exc
    if root_entries != {
        "monitor-baseline.json",
        "register-capture.json",
        "register-observation.json",
        "evidence",
    }:
        raise CaptureRegisterError("staged capture root layout is invalid")

    evidence_directory = staging / "evidence"
    _require_staged_directory(evidence_directory)
    baseline_content, baseline = _load_generated_object(staging / "monitor-baseline.json")
    capture_content, capture = _load_generated_object(staging / "register-capture.json")
    _observation_content, observation = _load_generated_object(
        staging / "register-observation.json"
    )

    _require_exact_fields(
        baseline, {"corpus", "retrieved", "source", "source_api", "titles"}
    )
    if (
        baseline["corpus"] != "Commonwealth tax statutes and legislative instruments"
        or baseline["source"] != "Federal Register of Legislation"
        or baseline["source_api"] != SOURCE_API
        or not isinstance(baseline["titles"], list)
        or not baseline["titles"]
    ):
        raise CaptureRegisterError("staged capture baseline is invalid")
    _date(baseline["retrieved"], "baseline retrieved")
    title_fields = {
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
    }
    titles_by_id: dict[str, dict[str, Any]] = {}
    title_order: list[str] = []
    for title in baseline["titles"]:
        if not isinstance(title, dict):
            raise CaptureRegisterError("staged capture baseline title is invalid")
        _require_exact_fields(title, title_fields)
        try:
            title_id = validate_register_id(title["register_id"])
        except ValueError as exc:
            raise CaptureRegisterError("staged capture baseline identity is invalid") from exc
        if title_id in titles_by_id:
            raise CaptureRegisterError("staged capture baseline identity is duplicated")
        if title["collection"] not in {
            "Act",
            "LegislativeInstrument",
            "NotifiableInstrument",
        }:
            raise CaptureRegisterError("staged capture baseline collection is invalid")
        compilation_date = _date(title["compilation_date"], "baseline compilation date")
        _date(title["retrieved"], "baseline title retrieved")
        if not isinstance(title["version_is_current"], bool):
            raise CaptureRegisterError("staged capture baseline current flag is invalid")
        current_start = title["current_version_start"]
        if current_start is not None:
            _date(current_start, "baseline current version start")
        if not title["version_is_current"] and current_start is None:
            raise CaptureRegisterError("staged capture baseline current date is missing")
        _required_text(title["name"], "baseline title name")
        _required_text(title["compilation_number"], "baseline compilation number")
        expected_source = (
            f"{REGISTER_SITE}/{title_id}/{compilation_date}/{compilation_date}/"
            "text/original/epub"
        )
        if title["source_url"] != expected_source:
            raise CaptureRegisterError("staged capture baseline source URL is invalid")
        if title["register_page"] != f"{REGISTER_SITE}/{title_id}/latest/text":
            raise CaptureRegisterError("staged capture baseline page URL is invalid")
        titles_by_id[title_id] = title
        title_order.append(title_id)
    if title_order != sorted(title_order):
        raise CaptureRegisterError("staged capture baseline order is invalid")
    if baseline["retrieved"] != max(title["retrieved"] for title in baseline["titles"]):
        raise CaptureRegisterError("staged capture baseline retrieval date is invalid")

    capture_fields = {
        "schema_version",
        "mode",
        "observed_at",
        "source_api",
        "baseline_sha256",
        "expected_register_ids",
        "complete",
        "results",
    }
    _require_exact_fields(capture, capture_fields)
    if (
        capture["schema_version"] != CAPTURE_SCHEMA
        or capture["mode"] != "live"
        or capture["source_api"] != SOURCE_API
        or capture["baseline_sha256"] != _sha256_id(baseline_content)
        or capture["expected_register_ids"] != title_order
        or capture["complete"] is not True
        or not isinstance(capture["results"], list)
        or len(capture["results"]) != len(title_order)
    ):
        raise CaptureRegisterError("staged capture manifest is inconsistent")
    _utc_timestamp(capture["observed_at"], "capture observed_at")

    result_fields = {
        "register_id",
        "collection",
        "checked_at",
        "state",
        "error_category",
        "requests",
    }
    request_fields = {
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
    }
    declared_evidence: dict[str, tuple[str, int]] = {}
    results_by_id: dict[str, dict[str, Any]] = {}
    for index, result in enumerate(capture["results"]):
        if not isinstance(result, dict):
            raise CaptureRegisterError("staged capture result is invalid")
        _require_exact_fields(result, result_fields)
        title_id = result["register_id"]
        if title_id != title_order[index] or title_id in results_by_id:
            raise CaptureRegisterError("staged capture result identity is invalid")
        title = titles_by_id[title_id]
        if result["collection"] != title["collection"]:
            raise CaptureRegisterError("staged capture result collection is invalid")
        state = result["state"]
        if state not in OBSERVATION_STATES:
            raise CaptureRegisterError("staged capture result state is invalid")
        error_category = result["error_category"]
        if (state == "LOOKUP_FAILED") != (error_category in FAILURE_CATEGORIES):
            raise CaptureRegisterError("staged capture result failure category is invalid")
        requests = result["requests"]
        if not isinstance(requests, list) or not 1 <= len(requests) <= 2:
            raise CaptureRegisterError("staged capture request list is invalid")
        if [request.get("role") for request in requests if isinstance(request, dict)] != (
            ["current"] if len(requests) == 1 else ["current", "history"]
        ):
            raise CaptureRegisterError("staged capture request roles are invalid")
        for request in requests:
            if not isinstance(request, dict):
                raise CaptureRegisterError("staged capture request is invalid")
            _require_exact_fields(request, request_fields)
            role = request["role"]
            expected_url = _current_url(title_id) if role == "current" else _history_url(title_id)
            if request["url"] != expected_url:
                raise CaptureRegisterError("staged capture request URL is invalid")
            _utc_timestamp(request["checked_at"], "request checked_at")
            if (
                not isinstance(request["attempt_count"], int)
                or isinstance(request["attempt_count"], bool)
                or not 0 <= request["attempt_count"] <= MAX_ATTEMPTS
                or not isinstance(request["response_headers"], dict)
                or set(request["response_headers"]) - RETAINED_HEADER_NAMES
            ):
                raise CaptureRegisterError("staged capture request metadata is invalid")
            try:
                if _normalised_headers(request["response_headers"]) != request["response_headers"]:
                    raise CaptureRegisterError("staged capture headers are non-canonical")
            except CaptureRegisterError as exc:
                raise CaptureRegisterError("staged capture request headers are invalid") from exc
            digest = request["response_sha256"]
            evidence_path = request["evidence_path"]
            response_length = request["response_length"]
            if digest is None:
                if evidence_path is not None or response_length is not None:
                    raise CaptureRegisterError("staged capture empty response declaration is invalid")
            else:
                if (
                    not isinstance(digest, str)
                    or SHA256_ID.fullmatch(digest) is None
                    or request["http_status"] != 200
                    or not isinstance(response_length, int)
                    or isinstance(response_length, bool)
                    or not 0 <= response_length <= MAX_RESPONSE_BYTES
                ):
                    raise CaptureRegisterError("staged capture response declaration is invalid")
                digest_hex = digest.removeprefix("sha256:")
                expected_path = f"evidence/sha256-{digest_hex}.json"
                if evidence_path != expected_path:
                    raise CaptureRegisterError("staged capture evidence path is invalid")
                declared_evidence[expected_path] = (digest, response_length)
        if result["checked_at"] != requests[-1]["checked_at"]:
            raise CaptureRegisterError("staged capture result timestamp is invalid")
        results_by_id[title_id] = result

    try:
        evidence_files = list(evidence_directory.iterdir())
    except OSError as exc:
        raise CaptureRegisterError("staged capture evidence layout could not be read") from exc
    actual_evidence = {f"evidence/{path.name}" for path in evidence_files}
    if actual_evidence != set(declared_evidence):
        raise CaptureRegisterError("staged capture evidence inventory is invalid")
    for path in evidence_files:
        match = EVIDENCE_NAME.fullmatch(path.name)
        if match is None:
            raise CaptureRegisterError("staged capture evidence filename is invalid")
        content = _read_staged_file(path)
        digest, expected_length = declared_evidence[f"evidence/{path.name}"]
        if len(content) != expected_length or _sha256_id(content) != digest:
            raise CaptureRegisterError("staged capture evidence digest is invalid")

    observation_fields = {
        "schema_version",
        "mode",
        "observed_at",
        "scope_id",
        "baseline_sha256",
        "capture_sha256",
        "expected_register_ids",
        "complete",
        "run_status",
        "observations",
    }
    _require_exact_fields(observation, observation_fields)
    expected_run_status = (
        "BLOCKED"
        if any(result["state"] == "LOOKUP_FAILED" for result in capture["results"])
        else "VERIFIED"
    )
    if (
        observation["schema_version"] != OBSERVATION_SCHEMA
        or observation["mode"] != "live"
        or observation["observed_at"] != capture["observed_at"]
        or observation["scope_id"] != OBSERVATION_SCOPE
        or observation["baseline_sha256"] != capture["baseline_sha256"]
        or observation["capture_sha256"] != _sha256_id(capture_content)
        or observation["expected_register_ids"] != title_order
        or observation["complete"] is not True
        or observation["run_status"] != expected_run_status
        or not isinstance(observation["observations"], list)
        or len(observation["observations"]) != len(title_order)
    ):
        raise CaptureRegisterError("staged capture observation is inconsistent")

    item_fields = {
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
    }
    for index, item in enumerate(observation["observations"]):
        if not isinstance(item, dict):
            raise CaptureRegisterError("staged capture observation item is invalid")
        _require_exact_fields(item, item_fields)
        title_id = title_order[index]
        result = results_by_id[title_id]
        title = titles_by_id[title_id]
        result_sha256 = _sha256_id(_json_bytes(result))
        if (
            item["register_id"] != title_id
            or item["collection"] != title["collection"]
            or item["state"] != result["state"]
            or item["checked_at"] != result["checked_at"]
            or item["error_category"] != result["error_category"]
            or item["evidence_url"] != title["register_page"]
            or item["capture_result_sha256"] != result_sha256
            or item["evidence_id"]
            != f"frl:{title_id}:{result_sha256.removeprefix('sha256:')[:32]}"
        ):
            raise CaptureRegisterError("staged capture observation item is inconsistent")
        primary_request = result["requests"][0]
        if item["primary_response_sha256"] != primary_request["response_sha256"]:
            raise CaptureRegisterError("staged capture primary digest is inconsistent")
        content_type = primary_request["response_headers"].get("content-type")
        expected_media = (
            content_type.split(";", 1)[0].strip().lower()
            if primary_request["response_sha256"] is not None and content_type is not None
            else None
        )
        if item["primary_response_media_type"] != expected_media:
            raise CaptureRegisterError("staged capture primary media type is inconsistent")
        conditional = {
            "observed_compilation_number",
            "observed_compilation_date",
            "observed_register_document_id",
            "current_version_start",
            "error_category",
        }
        required = {
            "UNCHANGED": set(),
            "SUPERSEDED": {
                "observed_compilation_number",
                "observed_compilation_date",
                "observed_register_document_id",
            },
            "CURRENT_NO_PUBLISHED_COMPILATION": {"current_version_start"},
            "NO_LONGER_IN_FORCE": set(),
            "LOOKUP_FAILED": {"error_category"},
        }[item["state"]]
        if any(item[field] is not None for field in conditional - required):
            raise CaptureRegisterError("staged capture state fields are inconsistent")
        if any(item[field] is None for field in required):
            raise CaptureRegisterError("staged capture required state field is missing")
        if item["observed_compilation_date"] is not None:
            _date(item["observed_compilation_date"], "observed compilation date")
        if item["current_version_start"] is not None:
            _date(item["current_version_start"], "observed current version start")
        if item["observed_register_document_id"] is not None:
            try:
                validate_register_id(item["observed_register_document_id"])
            except ValueError as exc:
                raise CaptureRegisterError("staged capture document identity is invalid") from exc


def _validate_staged_graph(staging: Path) -> None:
    """Re-read and prove the complete staged graph immediately before promotion."""
    try:
        _validate_staged_graph_inner(staging)
    except CaptureRegisterError as exc:
        if str(exc).startswith("staged capture"):
            raise
        raise CaptureRegisterError("staged capture graph is invalid") from exc
    except (OSError, TypeError, ValueError) as exc:
        raise CaptureRegisterError("staged capture graph is invalid") from exc


def _require_absent_destination(path: Path) -> None:
    try:
        os.lstat(path)
    except FileNotFoundError:
        return
    except OSError as exc:
        raise CaptureRegisterError("capture destination could not be inspected.") from exc
    raise CaptureRegisterError("capture destination must not exist.")


def _same_location(left: Path, right: Path) -> bool:
    return os.path.normcase(os.path.realpath(left)) == os.path.normcase(os.path.realpath(right))


def _preflight_output(output: Path, manifest: Path) -> bool:
    if SAFE_OUTPUT_LEAF.fullmatch(output.name) is None:
        raise CaptureRegisterError("capture destination name is unsafe.")
    _require_absent_destination(output)
    if _same_location(output, manifest):
        raise CaptureRegisterError("capture destination must not replace its manifest input.")
    try:
        os.lstat(output.parent)
    except FileNotFoundError:
        if SAFE_OUTPUT_LEAF.fullmatch(output.parent.name) is None:
            raise CaptureRegisterError("capture output parent name is unsafe.")
        try:
            os.lstat(output.parent.parent)
        except OSError as exc:
            raise CaptureRegisterError(
                "capture permits only one missing output parent."
            ) from exc
        _require_ordinary_ancestors(output.parent.parent, "capture output")
        return True
    except OSError as exc:
        raise CaptureRegisterError("capture output parent could not be inspected.") from exc
    _require_ordinary_directory(output.parent, "capture output parent")
    _require_ordinary_ancestors(output.parent.parent, "capture output")
    return False


def _remove_owned_staging(staging: Path, *, parent: Path, prefix: str) -> None:
    if staging.parent != parent or not staging.name.startswith(prefix):
        raise CaptureRegisterError("refusing to remove staging outside the capture boundary")
    try:
        details = os.lstat(staging)
    except FileNotFoundError:
        return
    except OSError as exc:
        raise CaptureRegisterError("capture staging could not be inspected") from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(staging)
        or _path_is_junction(staging)
        or _details_are_reparse_point(details)
    ):
        raise CaptureRegisterError("capture staging is no longer an ordinary directory")
    try:
        shutil.rmtree(staging)
    except OSError as exc:
        raise CaptureRegisterError("capture staging could not be removed") from exc


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
        session = _HttpsRegisterSession()
    observed_at = _utc_timestamp(session.observed_at, "session observed_at")
    manifest = Path(os.path.abspath(os.fspath(manifest_path)))
    output = Path(os.path.abspath(os.fspath(destination)))
    baseline = _project_baseline(_load_manifest(manifest))
    create_output_parent = _preflight_output(output, manifest)

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

    if create_output_parent:
        try:
            os.mkdir(output.parent, 0o700)
        except OSError as exc:
            raise CaptureRegisterError("capture output parent could not be created.") from exc
    _require_ordinary_directory(output.parent, "capture output parent")
    _require_ordinary_ancestors(output.parent.parent, "capture output")
    _require_absent_destination(output)

    prefix = f".{output.name}.register-capture-"
    staging = output.parent / f"{prefix}{uuid.uuid4().hex}.tmp"
    try:
        try:
            os.mkdir(staging, 0o700)
            evidence_directory = staging / "evidence"
            os.mkdir(evidence_directory, 0o700)
            _write_new(staging / "monitor-baseline.json", baseline_content)
            for relative_path, content in evidence.items():
                _write_new(staging / relative_path, content)
            _write_new(staging / "register-capture.json", capture_content)
            _write_new(staging / "register-observation.json", observation_content)
        except OSError as exc:
            raise CaptureRegisterError("capture output could not be written.") from exc
        _validate_staged_graph(staging)
        _require_ordinary_directory(output.parent, "capture output parent")
        _require_absent_destination(output)
        try:
            os.rename(staging, output)
        except OSError as exc:
            raise CaptureRegisterError("capture output could not be promoted.") from exc
    finally:
        _remove_owned_staging(staging, parent=output.parent, prefix=prefix)

    return {
        "baseline": output / "monitor-baseline.json",
        "capture": output / "register-capture.json",
        "observation": output / "register-observation.json",
    }


def main(argv: Sequence[str] | None = None) -> int:
    """Capture complete live Register metadata into one immutable directory."""
    parser = argparse.ArgumentParser(
        description="Capture complete live Federal Register metadata evidence."
    )
    parser.add_argument("manifest", type=Path, help="rich corpus manifest JSON")
    parser.add_argument(
        "--out",
        required=True,
        type=Path,
        help="new immutable capture directory",
    )
    args = parser.parse_args(argv)
    try:
        paths = capture_register_run(args.manifest, args.out)
    except CaptureRegisterError as exc:
        parser.error(str(exc))
    for role in ("baseline", "capture", "observation"):
        print(f"{role}: {paths[role]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
