"""Build source-only publisher evidence bundles from verified monitor inputs."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import stat
import uuid
from pathlib import Path
from typing import Any, Sequence
from urllib.parse import urlsplit

from fadden import export_monitor_contract as monitor_contract
from tax_radar_au.errors import MonitorError
from tax_radar_au.util import SourceSnapshot, load_json


class PublicationBundleError(ValueError):
    """Raised when monitor inputs cannot support a publication bundle."""


SHA256 = re.compile(r"[0-9a-f]{64}")
SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}")
SAFE_UPSTREAM_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}")
PUBLISHER_ID = re.compile(r"[a-z0-9][a-z0-9._-]{2,79}")
SAFE_OUTPUT_LEAF = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,99}")
VERSION_PATH = Path(__file__).resolve().parent.parent / "VERSION"


def _required_text(value: Any, field: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value.strip() != value
        or any(ord(character) < 32 or ord(character) == 127 for character in value)
    ):
        raise PublicationBundleError(f"{field} must be non-empty, trimmed text.")
    return value


def _publisher_text(value: Any, field: str, maximum: int) -> str:
    text = _required_text(value, field)
    utf16_length = sum(2 if ord(character) > 0xFFFF else 1 for character in text)
    xml_safe = all(
        ord(character) in {0x09, 0x0A, 0x0D}
        or 0x20 <= ord(character) <= 0xD7FF
        or 0xE000 <= ord(character) <= 0xFFFD
        or 0x10000 <= ord(character) <= 0x10FFFF
        for character in text
    )
    if utf16_length > maximum or not xml_safe:
        raise PublicationBundleError(
            f"{field} must fit the publisher's XML text limit of {maximum} characters."
        )
    return text


def _publisher_identifier(value: str, field: str) -> str:
    if PUBLISHER_ID.fullmatch(value) is None:
        raise PublicationBundleError(f"{field} is not a safe publisher identifier.")
    return value


def _publisher_https_url(value: Any, field: str, *, expected_path: str) -> str:
    text = _required_text(value, field)
    utf16_length = sum(2 if ord(character) > 0xFFFF else 1 for character in text)
    try:
        text.encode("utf-8")
        parsed = urlsplit(text)
        hostname = parsed.hostname
        _ = parsed.port
    except (UnicodeEncodeError, ValueError) as exc:
        raise PublicationBundleError(f"{field} must be a permitted HTTPS URL.") from exc
    if (
        utf16_length > 2048
        or parsed.scheme.lower() != "https"
        or not hostname
        or parsed.username is not None
        or parsed.password is not None
        or "\\" in parsed.netloc
        or any(character.isspace() for character in parsed.netloc)
        or parsed.path != expected_path
    ):
        raise PublicationBundleError(f"{field} must be a permitted HTTPS URL.")
    return text


def _safe_upstream_id(value: Any, field: str) -> str:
    text = _required_text(value, field)
    if SAFE_UPSTREAM_ID.fullmatch(text) is None:
        raise PublicationBundleError(f"{field} is not a safe identifier.")
    return text


def _input_digest(value: Any, field: str) -> str:
    text = _required_text(value, field)
    if SHA256.fullmatch(text) is None:
        raise PublicationBundleError(f"{field} must be a lowercase SHA-256 digest.")
    return f"sha256:{text}"


def _required_member(value: dict[str, Any], field: str, label: str) -> Any:
    try:
        return value[field]
    except KeyError as exc:
        raise PublicationBundleError(f"{label} is missing {field}.") from exc


def _iso_date(value: Any, field: str) -> str:
    text = _required_text(value, field)
    try:
        parsed = dt.date.fromisoformat(text)
    except ValueError as exc:
        raise PublicationBundleError(f"{field} must be an ISO calendar date.") from exc
    if parsed.isoformat() != text:
        raise PublicationBundleError(f"{field} must be an ISO calendar date.")
    return text


def build_publication_bundles(
    baseline: dict[str, Any],
    observation: dict[str, Any],
    *,
    baseline_sha256: str,
    observation_facts_sha256: str,
    producer_version: str,
) -> list[dict[str, Any]]:
    """Transform a projected monitor observation into deterministic bundles."""
    if not isinstance(baseline, dict) or not isinstance(observation, dict):
        raise PublicationBundleError("baseline and observation must be objects.")
    if observation.get("schema_version") != "au-tax-register-observation.v3":
        raise PublicationBundleError("observation must use au-tax-register-observation.v3.")
    if observation.get("mode") != "synthetic":
        raise PublicationBundleError("observation mode must be synthetic.")
    if observation.get("complete") is not True:
        raise PublicationBundleError("observation must cover the complete baseline scope.")

    raw_titles = baseline.get("titles")
    raw_observations = observation.get("observations")
    expected_register_ids = observation.get("expected_register_ids")
    if not isinstance(raw_titles, list) or not isinstance(raw_observations, list):
        raise PublicationBundleError("baseline titles and observations must be arrays.")
    if not isinstance(expected_register_ids, list):
        raise PublicationBundleError("expected_register_ids must be an array.")

    titles: dict[str, dict[str, Any]] = {}
    for index, raw_title in enumerate(raw_titles, start=1):
        if not isinstance(raw_title, dict):
            raise PublicationBundleError(f"baseline title {index} must be an object.")
        register_id = _safe_upstream_id(
            _required_member(raw_title, "register_id", f"baseline title {index}"),
            f"baseline title {index} register_id",
        )
        if register_id in titles:
            raise PublicationBundleError("baseline contains duplicate register identifiers.")
        titles[register_id] = raw_title

    if (
        any(not isinstance(value, str) for value in expected_register_ids)
        or len(set(expected_register_ids)) != len(expected_register_ids)
        or set(expected_register_ids) != set(titles)
    ):
        raise PublicationBundleError("observation scope does not match the baseline.")

    baseline_digest = _input_digest(baseline_sha256, "baseline_sha256")
    facts_digest = _input_digest(observation_facts_sha256, "observation_facts_sha256")
    version = _publisher_text(producer_version, "producer_version", 100)
    bundles: list[dict[str, Any]] = []
    seen_register_ids: set[str] = set()
    seen_bundle_ids: set[str] = set()

    for index, observed in enumerate(raw_observations, start=1):
        if not isinstance(observed, dict):
            raise PublicationBundleError(f"observation {index} must be an object.")
        register_id = _safe_upstream_id(
            _required_member(observed, "register_id", f"observation {index}"),
            f"observation {index} register_id",
        )
        if register_id in seen_register_ids:
            raise PublicationBundleError("observation contains duplicate register identifiers.")
        seen_register_ids.add(register_id)
        if register_id not in titles:
            raise PublicationBundleError("observation contains an identifier outside the baseline.")

        state = _required_text(
            _required_member(observed, "state", f"observation {index}"),
            f"observation {index} state",
        )
        if state == "UNCHANGED":
            continue
        if state != "SUPERSEDED":
            raise PublicationBundleError(
                f"observation state {state} cannot support a publication bundle."
            )

        title = titles[register_id]
        collection = _required_text(
            _required_member(observed, "collection", f"observation {index}"),
            f"observation {index} collection",
        )
        if collection != title.get("collection"):
            raise PublicationBundleError(
                f"observation {index} collection does not match the baseline."
            )
        if collection not in monitor_contract.COLLECTIONS:
            raise PublicationBundleError(f"observation {index} collection is unsupported.")
        document_id = _safe_upstream_id(
            _required_member(observed, "observed_register_document_id", f"observation {index}"),
            f"observation {index} observed_register_document_id",
        )
        compilation_number = _publisher_text(
            _required_member(observed, "observed_compilation_number", f"observation {index}"),
            f"observation {index} observed_compilation_number",
            80,
        )
        compilation_date = _iso_date(
            _required_member(observed, "observed_compilation_date", f"observation {index}"),
            f"observation {index} observed_compilation_date",
        )
        content_sha256 = _required_text(
            _required_member(observed, "content_sha256", f"observation {index}"),
            f"observation {index} content_sha256",
        )
        if SHA256_ID.fullmatch(content_sha256) is None:
            raise PublicationBundleError(f"observation {index} content_sha256 is invalid.")
        content_kind = _required_text(
            _required_member(observed, "content_kind", f"observation {index}"),
            f"observation {index} content_kind",
        )
        if content_kind not in monitor_contract.CONTENT_KINDS:
            raise PublicationBundleError(f"observation {index} content_kind is unsupported.")
        content_media_type = _required_text(
            _required_member(observed, "content_media_type", f"observation {index}"),
            f"observation {index} content_media_type",
        )
        if content_media_type not in monitor_contract.CONTENT_MEDIA_TYPES:
            raise PublicationBundleError(
                f"observation {index} content_media_type is unsupported."
            )
        title_name = _publisher_text(title.get("name"), "baseline title name", 200)
        previous_compilation_number = _publisher_text(
            title.get("compilation_number"),
            "baseline title compilation_number",
            80,
        )
        previous_compilation_date = _iso_date(
            title.get("compilation_date"),
            "baseline title compilation_date",
        )
        if (
            compilation_number == previous_compilation_number
            or compilation_date <= previous_compilation_date
        ):
            raise PublicationBundleError(
                f"observation {index} is not a newer published compilation."
            )
        published_at = f"{compilation_date}T00:00:00Z"
        identity = f"{register_id.lower()}-{document_id.lower()}"
        bundle_id = _publisher_identifier(
            f"bundle-frl-{identity}-r1", "generated bundle_id"
        )
        development_id = _publisher_identifier(
            f"dev-frl-{identity}", "generated development_id"
        )
        source_id = _publisher_identifier(
            f"frl-{document_id.lower()}", "generated source_id"
        )
        if bundle_id in seen_bundle_ids:
            raise PublicationBundleError("observations produce duplicate bundle identities.")
        seen_bundle_ids.add(bundle_id)

        bundles.append(
            {
                "schema_version": "evidence-bundle.v1",
                "bundle_id": bundle_id,
                "development_id": development_id,
                "mode": "synthetic",
                "generated_at": observation["observed_at"],
                "producer": {
                    "name": "tax-radar-au",
                    "version": version,
                    "baseline_sha256": baseline_digest,
                    "observation_facts_sha256": facts_digest,
                },
                "development": {
                    "title": title_name,
                    "authority_status": "in-force",
                    "evidence_status": "verified",
                    "publication_status": "source-only",
                    "published_at": published_at,
                    "effective_at": None,
                    "topics": [],
                    "affected_practice_areas": [],
                },
                "source_event": {
                    "kind": "compilation-superseded",
                    "register_id": register_id,
                    "collection": collection,
                    "previous_compilation": {
                        "number": previous_compilation_number,
                        "date": previous_compilation_date,
                    },
                    "current_compilation": {
                        "number": compilation_number,
                        "date": compilation_date,
                        "register_document_id": document_id,
                    },
                },
                "sources": [
                    {
                        "source_id": source_id,
                        "publisher": "Federal Register of Legislation",
                        "document_class": "legislation",
                        "title": title_name,
                        "canonical_url": _publisher_https_url(
                            _required_member(observed, "evidence_url", f"observation {index}"),
                            f"observation {index} evidence_url",
                            expected_path=f"/{register_id}/latest/text",
                        ),
                        "published_at": published_at,
                        "retrieved_at": _required_text(
                            _required_member(observed, "checked_at", f"observation {index}"),
                            f"observation {index} checked_at",
                        ),
                        "evidence_id": _safe_upstream_id(
                            _required_member(observed, "evidence_id", f"observation {index}"),
                            f"observation {index} evidence_id",
                        ),
                        "content_sha256": content_sha256,
                        "content_kind": content_kind,
                        "content_media_type": content_media_type,
                        "rights": {
                            "mode": "metadata-only",
                            "attribution": "Synthetic Federal Register fixture",
                            "licence_url": None,
                        },
                        "evidence": [],
                    }
                ],
                "revision": {
                    "number": 1,
                    "updated_at": observation["observed_at"],
                    "change_note": "Initial source-only evidence bundle",
                    "replaces_bundle_id": None,
                },
            }
        )

    if seen_register_ids != set(titles):
        raise PublicationBundleError("observation does not cover every baseline title.")
    return sorted(bundles, key=lambda bundle: bundle["bundle_id"])


def bundle_bytes(bundle: dict[str, Any]) -> bytes:
    """Serialise one bundle using the contract's reviewed byte representation."""
    return (json.dumps(bundle, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def _absolute(path: str | Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _is_reparse_point(details: os.stat_result) -> bool:
    return bool(
        getattr(details, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    )


def _is_junction(path: Path) -> bool:
    return getattr(os.path, "isjunction", lambda _path: False)(path)


def _require_regular_input(path: Path, label: str) -> None:
    try:
        details = os.lstat(path)
    except FileNotFoundError as exc:
        raise PublicationBundleError(f"{label} does not exist: {path}.") from exc
    except OSError as exc:
        raise PublicationBundleError(f"{label} cannot be inspected: {path} ({exc}).") from exc
    if (
        not stat.S_ISREG(details.st_mode)
        or os.path.islink(path)
        or _is_junction(path)
        or _is_reparse_point(details)
    ):
        raise PublicationBundleError(f"{label} must be an ordinary file: {path}.")


def _require_ordinary_parent(path: Path) -> None:
    try:
        details = os.lstat(path)
    except FileNotFoundError as exc:
        raise PublicationBundleError(
            f"publication bundle output parent does not exist: {path}."
        ) from exc
    except OSError as exc:
        raise PublicationBundleError(
            f"publication bundle output parent cannot be inspected: {path} ({exc})."
        ) from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(path)
        or _is_junction(path)
        or _is_reparse_point(details)
    ):
        raise PublicationBundleError(
            f"publication bundle output parent must be an ordinary directory: {path}."
        )


def _output_parent_needs_creation(path: Path) -> bool:
    try:
        details = os.lstat(path)
    except FileNotFoundError as exc:
        if SAFE_OUTPUT_LEAF.fullmatch(path.name) is None:
            raise PublicationBundleError("publication bundle output parent name is unsafe.") from exc
        _require_ordinary_parent(path.parent)
        return True
    except OSError as exc:
        raise PublicationBundleError(
            f"publication bundle output parent cannot be inspected: {path} ({exc})."
        ) from exc
    if (
        not stat.S_ISDIR(details.st_mode)
        or os.path.islink(path)
        or _is_junction(path)
        or _is_reparse_point(details)
    ):
        raise PublicationBundleError(
            f"publication bundle output parent must be an ordinary directory: {path}."
        )
    return False


def _require_absent(path: Path) -> None:
    try:
        os.lstat(path)
    except FileNotFoundError:
        return
    except OSError as exc:
        raise PublicationBundleError(
            f"publication bundle destination cannot be inspected: {path} ({exc})."
        ) from exc
    raise PublicationBundleError(f"publication bundle destination must not exist: {path}.")


def _same_location(left: Path, right: Path) -> bool:
    return os.path.normcase(os.path.realpath(left)) == os.path.normcase(os.path.realpath(right))


def _write_bundle(path: Path, content: bytes) -> None:
    with open(path, "xb") as target:
        target.write(content)


def _remove_owned_staging(staging: Path, *, parent: Path, prefix: str) -> None:
    if staging.parent != parent or not staging.name.startswith(prefix):
        raise RuntimeError("refusing to remove a staging path outside the exporter boundary")
    try:
        details = os.lstat(staging)
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(details.st_mode) or os.path.islink(staging) or _is_junction(staging):
        raise PublicationBundleError(
            f"publication bundle staging path is no longer an ordinary directory: {staging}."
        )
    shutil.rmtree(staging)


def export_publication_bundles(
    sources_path: str | Path,
    facts_path: str | Path,
    output_dir: str | Path,
) -> list[Path]:
    """Validate exact input snapshots and publish all bundles in one rename."""
    sources = _absolute(sources_path)
    facts = _absolute(facts_path)
    output = _absolute(output_dir)
    if SAFE_OUTPUT_LEAF.fullmatch(output.name) is None:
        raise PublicationBundleError("publication bundle output name is unsafe.")

    _require_regular_input(sources, "sources input")
    _require_regular_input(facts, "observation facts input")
    if _same_location(output, sources) or _same_location(output, facts):
        raise PublicationBundleError("publication bundle output would replace an input.")
    create_output_parent = _output_parent_needs_creation(output.parent)
    _require_absent(output)

    try:
        source_snapshot = SourceSnapshot.capture(sources, label="sources input")
        facts_snapshot = SourceSnapshot.capture(facts, label="observation facts input")
        source_document = load_json(source_snapshot, label="sources input")
        facts_document = load_json(facts_snapshot, label="observation facts input")
        baseline = monitor_contract.project_baseline(source_document)
        observation = monitor_contract.project_observation(baseline, facts_document)
        producer_version = VERSION_PATH.read_text(encoding="utf-8").strip()
        bundles = build_publication_bundles(
            baseline,
            observation,
            baseline_sha256=source_snapshot.sha256,
            observation_facts_sha256=facts_snapshot.sha256,
            producer_version=producer_version,
        )
    except PublicationBundleError:
        raise
    except (MonitorError, monitor_contract.ContractError) as exc:
        raise PublicationBundleError(str(exc)) from exc
    except OSError as exc:
        raise PublicationBundleError(
            f"publication bundle inputs could not be read: {exc}."
        ) from exc

    if create_output_parent:
        try:
            os.mkdir(output.parent, 0o700)
        except OSError as exc:
            raise PublicationBundleError(
                f"publication bundle output parent could not be created: {output.parent} ({exc})."
            ) from exc
        _require_ordinary_parent(output.parent)

    prefix = f".{output.name}.publication-bundles-"
    staging = output.parent / f"{prefix}{uuid.uuid4().hex}.tmp"
    try:
        try:
            os.mkdir(staging, 0o700)
            for bundle in bundles:
                destination = staging / f"{bundle['bundle_id']}.json"
                _write_bundle(destination, bundle_bytes(bundle))
        except OSError as exc:
            raise PublicationBundleError(
                f"publication bundle output could not be written: {exc}."
            ) from exc

        _require_absent(output)
        try:
            os.rename(staging, output)
        except OSError as exc:
            raise PublicationBundleError(
                f"publication bundle output could not be promoted: {exc}."
            ) from exc
    finally:
        _remove_owned_staging(staging, parent=output.parent, prefix=prefix)

    return [output / f"{bundle['bundle_id']}.json" for bundle in bundles]


def main(argv: Sequence[str] | None = None) -> int:
    """Export immutable source-only publication evidence bundles."""
    parser = argparse.ArgumentParser(
        description="Export complete v3 monitor observations as publication evidence bundles."
    )
    parser.add_argument("sources", type=Path, help="completed corpus sources.json")
    parser.add_argument("facts", type=Path, help="complete v3 observation facts JSON")
    parser.add_argument(
        "--out", required=True, type=Path, help="new immutable publication bundle directory"
    )
    args = parser.parse_args(argv)
    try:
        paths = export_publication_bundles(args.sources, args.facts, args.out)
    except PublicationBundleError as exc:
        parser.error(str(exc))
    for path in paths:
        print(f"publication evidence bundle: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
