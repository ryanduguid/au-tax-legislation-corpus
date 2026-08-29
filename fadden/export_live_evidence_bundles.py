"""Export deterministic live-only evidence-bundle.v2 candidates."""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import re
import stat
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from .capture_register import (
    ODATA_CONTEXT,
    ROW_FIELDS,
    CaptureRegisterError,
    validate_capture_graph,
)
from .corpus_paths import register_id as _validate_register_id


_SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
_SEMVER = re.compile(
    r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)
_VERSION_PATH = Path(__file__).resolve().parent.parent / "VERSION"
_PYPROJECT_PATH = Path(__file__).resolve().parent.parent / "pyproject.toml"
_MAX_RESPONSE_BYTES = 256 * 1024
_MAX_BUNDLE_BYTES = 1024 * 1024
_EVIDENCE_NAME = re.compile(r"sha256-[0-9a-f]{64}\.json\Z")
_VERSION_TIMESTAMP = re.compile(
    r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}"
    r"(?:\.\d{1,7})?(?:Z|[+-]\d{2}:\d{2})?\Z"
)
_BASELINE_TITLE_FIELDS = (
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
)
_CAPTURE_RESULT_FIELDS = (
    "register_id",
    "collection",
    "checked_at",
    "state",
    "error_category",
    "requests",
)
_REQUEST_FIELDS = (
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
)
_OBSERVATION_FIELDS = (
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
)


@dataclass(frozen=True)
class LiveEvidenceExport:
    release_tag: str
    candidates: tuple[Path, ...]


class LiveEvidenceBundleError(ValueError):
    pass


class _DuplicateJsonMemberError(ValueError):
    """Internal signal for ambiguous source JSON."""


def _reject_duplicate_json_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateJsonMemberError
        result[key] = value
    return result


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
        raise LiveEvidenceBundleError(f"{label} must be an ordinary directory.") from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(path)
        or _path_is_junction(path)
        or _details_are_reparse_point(details)
    ):
        raise LiveEvidenceBundleError(f"{label} must be an ordinary directory.")


def _require_ordinary_ancestors(path: Path, label: str) -> None:
    current = path
    while True:
        _require_ordinary_directory(current, label)
        parent = current.parent
        if parent == current:
            return
        current = parent


def _same_regular_file_identity(expected: os.stat_result, observed: os.stat_result) -> bool:
    return (
        stat.S_ISREG(expected.st_mode)
        and stat.S_ISREG(observed.st_mode)
        and os.path.samestat(expected, observed)
        and getattr(expected, "st_nlink", 1) == 1
        and getattr(observed, "st_nlink", 1) == 1
    )


def _copy_regular_file(source_path: Path, target_path: Path) -> None:
    try:
        expected = os.lstat(source_path)
    except OSError as exc:
        raise LiveEvidenceBundleError("capture snapshot source file is unavailable.") from exc
    if (
        not stat.S_ISREG(expected.st_mode)
        or os.path.islink(source_path)
        or _path_is_junction(source_path)
        or _details_are_reparse_point(expected)
        or getattr(expected, "st_nlink", 1) != 1
    ):
        raise LiveEvidenceBundleError("capture snapshot source file is not ordinary.")
    try:
        with open(source_path, "rb") as source, open(target_path, "xb") as target:
            opened = os.fstat(source.fileno())
            if not _same_regular_file_identity(expected, opened):
                raise LiveEvidenceBundleError("capture snapshot source file changed.")
            while block := source.read(64 * 1024):
                target.write(block)
            read_details = os.fstat(source.fileno())
        final = os.lstat(source_path)
    except OSError as exc:
        raise LiveEvidenceBundleError("capture snapshot source file could not be copied.") from exc
    if not (
        _same_regular_file_identity(expected, read_details)
        and _same_regular_file_identity(read_details, final)
    ):
        raise LiveEvidenceBundleError("capture snapshot source file changed.")


def _snapshot_capture_graph(capture_dir: str | Path, snapshot: Path) -> None:
    source = Path(os.path.abspath(os.fspath(capture_dir)))
    _require_ordinary_ancestors(source.parent, "capture input")
    _require_ordinary_directory(source, "capture input")
    try:
        root_entries = {entry.name for entry in source.iterdir()}
    except OSError as exc:
        raise LiveEvidenceBundleError("capture input could not be inspected.") from exc
    if root_entries != {
        "monitor-baseline.json",
        "register-capture.json",
        "register-observation.json",
        "evidence",
    }:
        raise LiveEvidenceBundleError("capture input layout is invalid.")
    source_evidence = source / "evidence"
    _require_ordinary_directory(source_evidence, "capture evidence")
    _require_ordinary_ancestors(source_evidence.parent, "capture input")
    try:
        evidence_entries = list(source_evidence.iterdir())
    except OSError as exc:
        raise LiveEvidenceBundleError("capture evidence could not be inspected.") from exc
    if any(_EVIDENCE_NAME.fullmatch(entry.name) is None for entry in evidence_entries):
        raise LiveEvidenceBundleError("capture evidence layout is invalid.")
    try:
        os.mkdir(snapshot, 0o700)
        os.mkdir(snapshot / "evidence", 0o700)
    except OSError as exc:
        raise LiveEvidenceBundleError("capture snapshot could not be created.") from exc
    for name in (
        "monitor-baseline.json",
        "register-capture.json",
        "register-observation.json",
    ):
        _copy_regular_file(source / name, snapshot / name)
    for entry in evidence_entries:
        _copy_regular_file(entry, snapshot / "evidence" / entry.name)


def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode(
        "utf-8"
    )


def _sha256_id(content: bytes) -> str:
    return f"sha256:{hashlib.sha256(content).hexdigest()}"


def _read_json(path: Path, label: str) -> tuple[bytes, dict[str, Any]]:
    try:
        content = path.read_bytes()
        value = json.loads(
            content.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_members
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
        raise LiveEvidenceBundleError(f"{label} could not be read as UTF-8 JSON.") from exc
    if not isinstance(value, dict):
        raise LiveEvidenceBundleError(f"{label} must be an object.")
    return content, value


def _required_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise LiveEvidenceBundleError(f"{label} must be an object.")
    return value


def _required_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or value.strip() != value:
        raise LiveEvidenceBundleError(f"{label} must be non-empty text.")
    return value


def _required_sha256(value: Any, label: str) -> str:
    text = _required_text(value, label)
    if _SHA256_ID.fullmatch(text) is None:
        raise LiveEvidenceBundleError(f"{label} must be a lowercase SHA-256 identifier.")
    return text


def _producer_version() -> str:
    try:
        version = _VERSION_PATH.read_text(encoding="utf-8").strip()
        pyproject = _PYPROJECT_PATH.read_text(encoding="utf-8")
    except OSError as exc:
        raise LiveEvidenceBundleError("producer version could not be read.") from exc
    package_version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', pyproject, re.MULTILINE)
    if (
        _SEMVER.fullmatch(version) is None
        or package_version is None
        or package_version.group(1) != version
    ):
        raise LiveEvidenceBundleError("producer version must be strict Semantic Version 2.0.0.")
    return version


def _register_id(value: Any, label: str) -> str:
    try:
        return _validate_register_id(_required_text(value, label))
    except ValueError as exc:
        raise LiveEvidenceBundleError(f"{label} is invalid.") from exc


def _rights(checked_at: str) -> dict[str, str]:
    capture_date = _required_text(checked_at, "observation checked_at")[:10]
    return {
        "mode": "metadata-only",
        "attribution": (
            "Based on content from the Federal Register of Legislation at "
            f"{capture_date}. For the latest information on Australian Government legislation "
            "please go to https://www.legislation.gov.au. Changes: selected and reformatted "
            "Federal Register metadata into a bounded evidence bundle and factual source update; "
            "no legislation text is reproduced."
        ),
        "licence_url": "https://creativecommons.org/licenses/by/4.0/",
    }


def _project_members(
    value: dict[str, Any], fields: tuple[str, ...], label: str
) -> dict[str, Any]:
    try:
        return {field: value[field] for field in fields}
    except KeyError as exc:
        raise LiveEvidenceBundleError(f"{label} is missing {exc.args[0]}.") from exc


def _project_capture_result(value: dict[str, Any]) -> dict[str, Any]:
    result = _project_members(value, _CAPTURE_RESULT_FIELDS, "capture result")
    requests = result["requests"]
    if not isinstance(requests, list):
        raise LiveEvidenceBundleError("capture result requests must be an array.")
    result["requests"] = [
        _project_members(
            _required_object(request, "capture request"),
            _REQUEST_FIELDS,
            "capture request",
        )
        for request in requests
    ]
    return result


def _read_response(path: Path) -> bytes:
    try:
        with open(path, "rb") as source:
            response = source.read(_MAX_RESPONSE_BYTES + 1)
    except OSError as exc:
        raise LiveEvidenceBundleError("retained response could not be read.") from exc
    if len(response) > _MAX_RESPONSE_BYTES:
        raise LiveEvidenceBundleError("retained response exceeds the 256 KiB limit.")
    return response


def _version_timestamp(value: Any, label: str) -> str:
    text = _required_text(value, label)
    if _VERSION_TIMESTAMP.fullmatch(text) is None:
        raise LiveEvidenceBundleError(f"{label} is invalid.")
    try:
        dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError as exc:
        raise LiveEvidenceBundleError(f"{label} is invalid.") from exc
    return text


def _raw_current_row(response: bytes, register_id: str) -> dict[str, Any]:
    try:
        document = json.loads(
            response.decode("utf-8"), object_pairs_hook=_reject_duplicate_json_members
        )
    except (UnicodeDecodeError, json.JSONDecodeError, _DuplicateJsonMemberError) as exc:
        raise LiveEvidenceBundleError("retained response is not unambiguous OData JSON.") from exc
    if (
        not isinstance(document, dict)
        or set(document) != {"@odata.context", "value"}
        or document["@odata.context"] != ODATA_CONTEXT
        or not isinstance(document["value"], list)
        or len(document["value"]) != 1
    ):
        raise LiveEvidenceBundleError("retained response OData shape is invalid.")
    row = document["value"][0]
    if not isinstance(row, dict) or set(row) != ROW_FIELDS:
        raise LiveEvidenceBundleError("retained response OData row is invalid.")
    if row["titleId"] != register_id or row["isCurrent"] is not True or row["status"] != "InForce":
        raise LiveEvidenceBundleError("retained response is not the declared current row.")
    if row["registerId"] is None:
        raise LiveEvidenceBundleError("retained response current document identifier is missing.")
    _register_id(row["registerId"], "raw current document identifier")
    _required_text(row["compilationNumber"], "raw current compilation number")
    _version_timestamp(row["start"], "raw current compilation start")
    _version_timestamp(row["registeredAt"], "raw current registration timestamp")
    return row


def _candidate_bundle(
    title: dict[str, Any],
    result: dict[str, Any],
    observation: dict[str, Any],
    *,
    version: str,
    baseline_sha256: str,
    observation_sha256: str,
    observed_at: str,
    response: bytes,
) -> tuple[str, dict[str, Any]]:
    baseline_title = _project_members(title, _BASELINE_TITLE_FIELDS, "baseline title")
    capture_result = _project_capture_result(result)
    bundle_observation = _project_members(observation, _OBSERVATION_FIELDS, "observation")
    register_id = _register_id(
        baseline_title.get("register_id"), "baseline title register_id"
    )
    document_id = _register_id(
        bundle_observation.get("observed_register_document_id"),
        "observed register document id",
    )
    requests = capture_result["requests"]
    if not isinstance(requests, list) or len(requests) != 1:
        raise LiveEvidenceBundleError("SUPERSEDED capture result must have one request.")
    request = _required_object(requests[0], "capture request")
    response_sha256 = _required_sha256(request.get("response_sha256"), "response SHA-256")
    if _sha256_id(response) != response_sha256:
        raise LiveEvidenceBundleError(
            "retained response digest does not match the capture request."
        )
    if request.get("response_length") != len(response):
        raise LiveEvidenceBundleError(
            "retained response length does not match the capture request."
        )
    if bundle_observation.get("primary_response_sha256") != response_sha256:
        raise LiveEvidenceBundleError(
            "observation response digest does not match the capture request."
        )
    if capture_result.get("state") != "SUPERSEDED" or bundle_observation.get("state") != "SUPERSEDED":
        raise LiveEvidenceBundleError("candidate state is not SUPERSEDED.")
    if request.get("role") != "current" or request.get("http_status") != 200:
        raise LiveEvidenceBundleError("candidate current request is invalid.")
    if bundle_observation.get("primary_response_media_type") != "application/json":
        raise LiveEvidenceBundleError("candidate primary response media type is invalid.")
    raw_row = _raw_current_row(response, register_id)
    if (
        raw_row["compilationNumber"] != bundle_observation.get("observed_compilation_number")
        or raw_row["start"][:10] != bundle_observation.get("observed_compilation_date")
        or raw_row["registerId"] != bundle_observation.get("observed_register_document_id")
    ):
        raise LiveEvidenceBundleError("retained response does not prove the observed compilation.")
    if bundle_observation.get("capture_result_sha256") != _sha256_id(
        _json_bytes(capture_result)
    ):
        raise LiveEvidenceBundleError("observation capture result digest is invalid.")
    bundle_id = f"bundle-frl-{register_id.lower()}-{document_id.lower()}-r1"
    return bundle_id, {
        "schema_version": "evidence-bundle.v2",
        "producer": {"name": "tax-radar-au", "version": version},
        "run": {
            "observed_at": observed_at,
            "scope_id": "au-primary-tax-legislation.v4",
            "complete": True,
            "run_status": "VERIFIED",
            "baseline_sha256": baseline_sha256,
            "observation_sha256": observation_sha256,
        },
        "baseline_title": baseline_title,
        "capture_result": capture_result,
        "observation": bundle_observation,
        "rights": _rights(
            _required_text(bundle_observation.get("checked_at"), "observation checked_at")
        ),
        "primary_response_base64": base64.b64encode(response).decode("ascii"),
    }


def _export_validated_snapshot(capture: Path, output_dir: str | Path) -> LiveEvidenceExport:
    """Write v2 candidates from an exporter-owned, validated capture snapshot."""
    output = Path(output_dir)
    baseline_bytes, baseline = _read_json(capture / "monitor-baseline.json", "baseline")
    capture_bytes, capture_document = _read_json(
        capture / "register-capture.json", "capture"
    )
    observation_bytes, observation_document = _read_json(
        capture / "register-observation.json", "observation"
    )
    if (
        capture_document.get("schema_version") != "au-tax-register-capture.v1"
        or capture_document.get("mode") != "live"
        or observation_document.get("schema_version") != "au-tax-register-observation.v4"
        or observation_document.get("mode") != "live"
        or observation_document.get("scope_id") != "au-primary-tax-legislation.v4"
        or observation_document.get("complete") is not True
        or observation_document.get("run_status") != "VERIFIED"
    ):
        raise LiveEvidenceBundleError("capture must be a verified live Stage 3A graph.")
    baseline_sha256 = _sha256_id(baseline_bytes)
    observation_sha256 = _sha256_id(observation_bytes)
    if (
        capture_document.get("baseline_sha256") != baseline_sha256
        or observation_document.get("baseline_sha256") != baseline_sha256
        or observation_document.get("capture_sha256") != _sha256_id(capture_bytes)
    ):
        raise LiveEvidenceBundleError("capture graph digests are inconsistent.")
    titles = baseline.get("titles")
    results = capture_document.get("results")
    observations = observation_document.get("observations")
    if not isinstance(titles, list) or not isinstance(results, list) or not isinstance(observations, list):
        raise LiveEvidenceBundleError("capture graph collections must be arrays.")
    titles_by_id = {
        _register_id(
            _required_object(title, "baseline title").get("register_id"),
            "baseline title register_id",
        ): title
        for title in titles
    }
    results_by_id = {
        _register_id(
            _required_object(result, "capture result").get("register_id"),
            "capture result register_id",
        ): result
        for result in results
    }
    candidates: list[tuple[str, dict[str, Any]]] = []
    for raw_observation in observations:
        observation = _required_object(raw_observation, "observation item")
        if observation.get("state") != "SUPERSEDED":
            continue
        register_id = _register_id(observation.get("register_id"), "observation register_id")
        try:
            title = _required_object(titles_by_id[register_id], "baseline title")
            result = _required_object(results_by_id[register_id], "capture result")
        except KeyError as exc:
            raise LiveEvidenceBundleError("SUPERSEDED observation is missing its capture data.") from exc
        requests = result.get("requests")
        if not isinstance(requests, list) or not requests:
            raise LiveEvidenceBundleError("SUPERSEDED capture result must have a request.")
        request = _required_object(requests[0], "capture request")
        response_sha256 = _required_sha256(
            request.get("response_sha256"), "capture response SHA-256"
        )
        expected_evidence_path = f"evidence/sha256-{response_sha256.removeprefix('sha256:')}.json"
        if request.get("evidence_path") != expected_evidence_path:
            raise LiveEvidenceBundleError("capture evidence path is invalid.")
        response = _read_response(capture / expected_evidence_path)
        candidate = _candidate_bundle(
            title,
            result,
            observation,
            version=_producer_version(),
            baseline_sha256=baseline_sha256,
            observation_sha256=observation_sha256,
            observed_at=_required_text(observation_document.get("observed_at"), "observed_at"),
            response=response,
        )
        candidates.append(candidate)
    candidate_ids = [candidate[0] for candidate in candidates]
    if len(candidate_ids) != len(set(candidate_ids)):
        raise LiveEvidenceBundleError("candidate identities are duplicated.")
    if output.exists():
        raise LiveEvidenceBundleError("output directory must not already exist.")
    try:
        output.mkdir(parents=True)
        paths = []
        for bundle_id, bundle in sorted(candidates):
            path = output / f"{bundle_id}.json"
            content = _json_bytes(bundle)
            if len(content) > _MAX_BUNDLE_BYTES:
                raise LiveEvidenceBundleError("serialised bundle exceeds the 1 MiB limit.")
            path.write_bytes(content)
            paths.append(path)
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence bundles could not be written.") from exc
    return LiveEvidenceExport(
        f"live-evidence-v2-{observation_sha256.removeprefix('sha256:')}", tuple(paths)
    )


def export_live_evidence_bundles(
    capture_dir: str | Path, output_dir: str | Path
) -> LiveEvidenceExport:
    """Write v2 candidates only from a private, verified Stage 3A snapshot."""
    with tempfile.TemporaryDirectory(prefix="tax-radar-live-evidence-") as temporary:
        snapshot = Path(temporary) / "capture"
        _snapshot_capture_graph(capture_dir, snapshot)
        try:
            validate_capture_graph(snapshot)
        except CaptureRegisterError as exc:
            raise LiveEvidenceBundleError("capture graph is not a verified Stage 3A graph.") from exc
        return _export_validated_snapshot(snapshot, output_dir)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Export live evidence-bundle.v2 candidates.")
    parser.add_argument("capture_dir")
    parser.add_argument("output_dir")
    arguments = parser.parse_args(argv)
    try:
        export = export_live_evidence_bundles(arguments.capture_dir, arguments.output_dir)
    except LiveEvidenceBundleError as exc:
        parser.error(str(exc))
    print(f"candidate_count={len(export.candidates)}")
    print(f"release_tag={export.release_tag}")
    return 0
