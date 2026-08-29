"""Export deterministic live-only evidence-bundle.v2 candidates."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from .corpus_paths import register_id as _validate_register_id


_SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
_SEMVER = re.compile(
    r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z"
)
_VERSION_PATH = Path(__file__).resolve().parent.parent / "VERSION"
_PYPROJECT_PATH = Path(__file__).resolve().parent.parent / "pyproject.toml"


@dataclass(frozen=True)
class LiveEvidenceExport:
    release_tag: str
    candidates: tuple[Path, ...]


class LiveEvidenceBundleError(ValueError):
    pass


def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode(
        "utf-8"
    )


def _sha256_id(content: bytes) -> str:
    return f"sha256:{hashlib.sha256(content).hexdigest()}"


def _read_json(path: Path, label: str) -> tuple[bytes, dict[str, Any]]:
    try:
        content = path.read_bytes()
        value = json.loads(content.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
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
    register_id = _register_id(title.get("register_id"), "baseline title register_id")
    document_id = _register_id(
        observation.get("observed_register_document_id"), "observed register document id"
    )
    requests = result.get("requests")
    if not isinstance(requests, list) or len(requests) != 1:
        raise LiveEvidenceBundleError("SUPERSEDED capture result must have one request.")
    request = _required_object(requests[0], "capture request")
    response_sha256 = _required_sha256(request.get("response_sha256"), "response SHA-256")
    if _sha256_id(response) != response_sha256:
        raise LiveEvidenceBundleError("retained response digest does not match the capture request.")
    if request.get("response_length") != len(response):
        raise LiveEvidenceBundleError("retained response length does not match the capture request.")
    if observation.get("primary_response_sha256") != response_sha256:
        raise LiveEvidenceBundleError("observation response digest does not match the capture request.")
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
        "baseline_title": title,
        "capture_result": result,
        "observation": observation,
        "rights": _rights(
            _required_text(observation.get("checked_at"), "observation checked_at")
        ),
        "primary_response_base64": base64.b64encode(response).decode("ascii"),
    }


def export_live_evidence_bundles(
    capture_dir: str | Path, output_dir: str | Path
) -> LiveEvidenceExport:
    """Write v2 candidates from a verified live Stage 3A capture."""
    capture = Path(capture_dir)
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
        evidence_path = _required_text(request.get("evidence_path"), "capture evidence path")
        try:
            response = (capture / evidence_path).read_bytes()
        except OSError as exc:
            raise LiveEvidenceBundleError("retained response could not be read.") from exc
        candidates.append(
            _candidate_bundle(
                title,
                result,
                observation,
                version=_producer_version(),
                baseline_sha256=baseline_sha256,
                observation_sha256=observation_sha256,
                observed_at=_required_text(observation_document.get("observed_at"), "observed_at"),
                response=response,
            )
        )
    if output.exists():
        raise LiveEvidenceBundleError("output directory must not already exist.")
    try:
        output.mkdir(parents=True)
        paths = []
        for bundle_id, bundle in sorted(candidates):
            path = output / f"{bundle_id}.json"
            path.write_bytes(_json_bytes(bundle))
            paths.append(path)
    except OSError as exc:
        raise LiveEvidenceBundleError("live evidence bundles could not be written.") from exc
    return LiveEvidenceExport(
        f"live-evidence-v2-{observation_sha256.removeprefix('sha256:')}", tuple(paths)
    )


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
