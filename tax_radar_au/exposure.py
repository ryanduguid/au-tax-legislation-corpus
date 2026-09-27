"""List pseudonymous client profiles whose workflows an intact impact queue names.

The queue records which workflows a source change may touch. A profiles file
records, under a code rather than a name, which workflows a firm runs for each
client. A profile is listed against a queue item only when one of the item's
candidate skill references appears in the profile's own list: exact membership,
no inference from attributes. A listed profile is a prompt to review that
client's work, never a finding that the client is affected.
"""
from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from .errors import MonitorError
from .monitor import QUEUE_FIELDS, verify_queue_integrity, verify_queue_markdown
from .persist import output_paths, write_queue_files
from .util import SourceSnapshot, load_json_exact, safe_markdown, sha256_json

PROFILES_SCHEMA = "au-tax-client-profiles.v1"
EXPOSURE_SCHEMA = "au-tax-client-exposure.v1"
MATCHING_BASIS = "exact_candidate_skill_ref_in_profile_skill_refs"
EXPOSURE_STEM = "client-exposure"
PROFILE_FILE_FIELDS = {"schema_version", "profiles_version", "profiles"}
PROFILE_FIELDS = {"profile_id", "skill_refs"}
# A code, not a name: no spaces, so "Jane Citizen" cannot be an id. A pattern
# cannot prove that an id is pseudonymous; RADAR.md says what may go here.
PROFILE_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}")
MAX_QUEUE_BYTES = 50_000_000
MAX_PROFILE_BYTES = 4_000_000
MAX_PROFILES = 10_000
MAX_SKILL_REFS = 200
MAX_SKILL_REF_CHARS = 200
MAX_MATCHES = 200_000
LIMITATIONS = (
    "A listed profile is a prompt to review that client's work, not a finding that the client is affected or that a source change has any legal effect.",
    "Matching is exact skill-reference membership. A profile that lists none of an item's skills is not thereby shown to be unaffected: the mapping may be incomplete.",
    "The impact queue is a synthetic metadata review. When the profiles describe real clients, this report is confidential client information.",
)


def _capture(path: Path, *, label: str, limit: int) -> SourceSnapshot:
    try:
        size = path.stat().st_size
    except OSError:
        size = 0  # SourceSnapshot.capture reports the missing or unreadable file.
    if size > limit:
        raise MonitorError(f"{label} exceeds {limit} bytes.")
    snapshot = SourceSnapshot.capture(path, label=label)
    if len(snapshot.content) > limit:
        raise MonitorError(f"{label} exceeds {limit} bytes.")
    return snapshot


def _skill_ref(value: Any, *, field: str) -> str:
    # Compared exactly against the queue's candidates, so padding or a
    # different case would silently never match: refuse it instead.
    if (
        not isinstance(value, str)
        or not value
        or value != value.strip()
        or len(value) > MAX_SKILL_REF_CHARS
        or any(ord(character) < 32 for character in value)
    ):
        raise MonitorError(
            f"{field} must be a skill reference of at most {MAX_SKILL_REF_CHARS} characters "
            "without surrounding spaces or control characters."
        )
    return value


def _load_profiles(snapshot: SourceSnapshot) -> tuple[str, list[tuple[str, tuple[str, ...]]]]:
    raw = load_json_exact(snapshot, PROFILE_FILE_FIELDS, label="client profiles")
    if raw["schema_version"] != PROFILES_SCHEMA:
        raise MonitorError("Client profiles have an unsupported schema.")
    version = raw["profiles_version"]
    if not isinstance(version, str) or not version.strip():
        raise MonitorError("Client profiles profiles_version must be a non-empty string.")
    entries = raw["profiles"]
    if not isinstance(entries, list) or not entries:
        raise MonitorError("Client profiles must contain a non-empty profiles list.")
    if len(entries) > MAX_PROFILES:
        raise MonitorError(f"Client profiles exceed {MAX_PROFILES} profiles.")
    profiles: list[tuple[str, tuple[str, ...]]] = []
    seen: set[str] = set()
    # Messages name positions, never values: a profile file can describe real
    # clients, and an error message travels further than the report does.
    for index, entry in enumerate(entries, start=1):
        if not isinstance(entry, dict) or set(entry) != PROFILE_FIELDS:
            raise MonitorError(
                f"Client profile {index} must contain exactly: profile_id, skill_refs."
            )
        profile_id = entry["profile_id"]
        if not isinstance(profile_id, str) or not PROFILE_ID.fullmatch(profile_id):
            raise MonitorError(
                f"Client profile {index} profile_id must be a code of up to 64 letters, digits "
                "and . _ : - characters, with no spaces."
            )
        if profile_id in seen:
            raise MonitorError(f"Client profile {index} repeats an earlier profile_id.")
        seen.add(profile_id)
        refs = entry["skill_refs"]
        if not isinstance(refs, list) or not refs or len(refs) > MAX_SKILL_REFS:
            raise MonitorError(
                f"Client profile {index} skill_refs must list 1 to {MAX_SKILL_REFS} skill references."
            )
        cleaned: list[str] = []
        for ref_index, ref in enumerate(refs, start=1):
            value = _skill_ref(ref, field=f"Client profile {index} skill reference {ref_index}")
            if value in cleaned:
                raise MonitorError(f"Client profile {index} repeats a skill reference.")
            cleaned.append(value)
        profiles.append((profile_id, tuple(cleaned)))
    return version, profiles


def _exposure_status(mapping_status: str, matched: bool) -> str:
    if mapping_status == "NOT_EVALUATED":
        return "NOT_EVALUATED"
    if mapping_status == "UNMAPPED_SOURCE":
        return "UNMAPPED_SOURCE"
    return "PROFILES_MATCHED" if matched else "NO_PROFILE_MATCH"


def exposure(*, queue_path: Path, profiles_path: Path) -> dict[str, Any]:
    """Build the exposure report from an intact queue and a profiles file.

    Each input is read once, and the bytes that are hashed are the bytes that
    are parsed. The queue must pass the same complete checks validate-review
    applies, including the recomputed digests and the Markdown companion.
    """
    queue_source = _capture(queue_path, label="impact queue", limit=MAX_QUEUE_BYTES)
    queue = load_json_exact(queue_source, QUEUE_FIELDS, label="impact queue")
    verify_queue_integrity(queue)
    verify_queue_markdown(queue_path, queue)
    profiles_source = _capture(
        profiles_path, label="client profiles", limit=MAX_PROFILE_BYTES
    )
    version, profiles = _load_profiles(profiles_source)

    by_skill: dict[str, list[str]] = {}
    for profile_id, refs in profiles:
        for ref in refs:
            by_skill.setdefault(ref, []).append(profile_id)

    items: list[dict[str, Any]] = []
    matched_profiles: set[str] = set()
    match_count = 0
    for item in queue["items"]:
        found: dict[str, tuple[set[str], set[str]]] = {}
        for candidate in item["impact_candidates"]:
            for profile_id in by_skill.get(candidate["skill_ref"], ()):
                skills, mappings = found.setdefault(profile_id, (set(), set()))
                skills.add(candidate["skill_ref"])
                mappings.add(candidate["mapping_id"])
        match_count += len(found)
        if match_count > MAX_MATCHES:
            raise MonitorError(
                f"Exposure exceeds {MAX_MATCHES} profile matches; split the profiles file."
            )
        matched_profiles.update(found)
        items.append(
            {
                "item_id": item["item_id"],
                "state": item["state"],
                "change_kind": item["change_kind"],
                "mapping_status": item["mapping_status"],
                "exposure_status": _exposure_status(item["mapping_status"], bool(found)),
                "source": item["source"],
                "matches": [
                    {
                        "profile_id": profile_id,
                        "matched_skill_refs": sorted(found[profile_id][0]),
                        "mapping_ids": sorted(found[profile_id][1]),
                    }
                    for profile_id in sorted(found)
                ],
                "limitations": item["limitations"],
            }
        )

    report: dict[str, Any] = {
        "schema_version": EXPOSURE_SCHEMA,
        "matching_basis": MATCHING_BASIS,
        "queue": {
            "run_id": queue["run_id"],
            "queue_digest": queue["queue_digest"],
            "file_sha256": "sha256:" + queue_source.sha256,
            "mode": queue["mode"],
            "run_status": queue["run_status"],
            "observation_complete": queue["observation"]["complete"],
        },
        "profiles": {
            "profiles_version": version,
            "file_sha256": "sha256:" + profiles_source.sha256,
            "profile_count": len(profiles),
        },
        "items": items,
        "profiles_without_candidate_match": sorted(
            profile_id for profile_id, _ in profiles if profile_id not in matched_profiles
        ),
        "limitations": list(LIMITATIONS),
    }
    report["exposure_digest"] = "sha256:" + sha256_json(report)
    return report


def render_exposure_markdown(report: dict[str, Any]) -> str:
    queue = report["queue"]
    profiles = report["profiles"]
    lines = [
        "# Client exposure report",
        "",
        f"**Queue run status: {queue['run_status']}**",
        f"Exposure digest: `{report['exposure_digest']}`",
        f"Queue digest: `{queue['queue_digest']}`",
        f"Profiles file: `{profiles['file_sha256']}`, version {safe_markdown(profiles['profiles_version'])}, {profiles['profile_count']} profile(s)",
        "",
    ]
    lines += [f"- {limitation}" for limitation in report["limitations"]]
    lines += ["", "## Queue items", ""]
    if not report["items"]:
        lines.append("The queue holds no items, so no profile is listed. This is not a statement about live law.")
    for item in report["items"]:
        lines += [f"### {item['state']}: {item['change_kind']} ({item['exposure_status']})", ""]
        source = item["source"]
        if source is None:
            lines.append("- Source: none; the observation scope was incomplete.")
        else:
            lines.append(
                f"- Source: {safe_markdown(source['title'])} (`{safe_markdown(source['register_id'])}`, {safe_markdown(source['collection'])})"
            )
        lines.append(f"- Mapping status: {item['mapping_status']}")
        for match in item["matches"]:
            skills = ", ".join(f"`{safe_markdown(ref)}`" for ref in match["matched_skill_refs"])
            mappings = ", ".join(f"`{safe_markdown(ref)}`" for ref in match["mapping_ids"])
            lines.append(
                f"- Profile `{safe_markdown(match['profile_id'])}`: skills {skills} (mappings {mappings})"
            )
        if item["exposure_status"] == "NO_PROFILE_MATCH":
            lines.append("- No profile lists a candidate skill for this item.")
        for limitation in item["limitations"]:
            lines.append(f"- Limitation: {safe_markdown(limitation)}")
        lines.append("")
    lines += ["## Profiles without a candidate match", ""]
    if report["profiles_without_candidate_match"]:
        lines += [
            f"- `{safe_markdown(profile_id)}`"
            for profile_id in report["profiles_without_candidate_match"]
        ]
    else:
        lines.append("Every profile matched at least one candidate.")
    lines.append("")
    return "\n".join(lines)


def write_exposure(
    report: dict[str, Any], output_dir: Path, *, inputs: tuple[Path, ...]
) -> dict[str, Path]:
    """Write the report pair owner-only, refusing to replace any input file."""
    destinations = output_paths(output_dir, stem=EXPOSURE_STEM)
    protected = set()
    for path in inputs:
        protected.add(path.resolve())
        protected.add(path.with_suffix(".md").resolve())
    if any(destination.resolve() in protected for destination in destinations):
        raise MonitorError("The output directory would replace an input file; choose another --out.")
    return write_queue_files(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        render_exposure_markdown(report),
        output_dir,
        stem=EXPOSURE_STEM,
        private=True,
    )
