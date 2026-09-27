from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
from typing import Any

import pytest
from tax_radar_au import exposure as exposure_module
from tax_radar_au.cli import main
from tax_radar_au.errors import MonitorError
from tax_radar_au.exposure import exposure, render_exposure_markdown, write_exposure
from tax_radar_au.monitor import compare, write_queue
from tax_radar_au.persist import output_paths
from tax_radar_au.util import sample_path

PROFILES = sample_path("profiles", "sample-client-profiles.json")
WORKPAPER_MAP = sample_path("mappings", "sample-workpaper-map.json")
SKILL_MAP = sample_path("mappings", "sample-source-skill-map.json")


def _load(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def _write(path: Path, payload: Any) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    return path


def _queue(
    tmp_path: Path,
    *,
    mapping: Path = WORKPAPER_MAP,
    observation: dict[str, Any] | None = None,
    name: str = "queue",
) -> Path:
    observation_path = sample_path("observations", "sample-register-observation.json")
    if observation is not None:
        observation_path = _write(tmp_path / f"{name}-observation.json", observation)
    queue = compare(
        baseline_path=sample_path("baseline", "sample-sources.json"),
        observation_path=observation_path,
        mapping_path=mapping,
    )
    return write_queue(queue, tmp_path / name)["json"]


def _observation() -> dict[str, Any]:
    return copy.deepcopy(_load(sample_path("observations", "sample-register-observation.json")))


def _profiles(tmp_path: Path, profiles: list[dict[str, Any]], name: str = "profiles.json") -> Path:
    return _write(
        tmp_path / name,
        {
            "schema_version": "au-tax-client-profiles.v1",
            "profiles_version": "test.1",
            "profiles": profiles,
        },
    )


def test_sample_profiles_match_by_exact_skill_reference(tmp_path: Path) -> None:
    report = exposure(queue_path=_queue(tmp_path), profiles_path=PROFILES)

    [item] = report["items"]
    assert item["exposure_status"] == "PROFILES_MATCHED"
    assert item["matches"] == [
        {
            "profile_id": "FAB-CLIENT-001",
            "matched_skill_refs": ["bas-preparation", "cashflow-forecast"],
            "mapping_ids": [
                "map:sample-consumption-tax-to-bas",
                "map:sample-consumption-tax-to-cash-assumptions",
            ],
        },
        {
            "profile_id": "FAB-CLIENT-002",
            "matched_skill_refs": ["bas-preparation"],
            "mapping_ids": ["map:sample-consumption-tax-to-bas"],
        },
    ]
    assert report["profiles_without_candidate_match"] == ["FAB-CLIENT-003"]
    assert report["profiles"]["file_sha256"] == (
        "sha256:" + hashlib.sha256(PROFILES.read_bytes()).hexdigest()
    )
    assert report["queue"]["mode"] == "synthetic"


def test_the_digest_is_deterministic_and_binds_every_input(tmp_path: Path) -> None:
    queue_path = _queue(tmp_path)
    first = exposure(queue_path=queue_path, profiles_path=PROFILES)
    second = exposure(queue_path=queue_path, profiles_path=PROFILES)
    assert first == second

    reformatted = tmp_path / "reformatted.json"
    reformatted.write_text(json.dumps(_load(PROFILES)), encoding="utf-8")
    third = exposure(queue_path=queue_path, profiles_path=reformatted)
    # The same profiles in different bytes: same matches, different file hash,
    # so a report never claims bytes it did not read.
    assert third["items"] == first["items"]
    assert third["profiles"]["file_sha256"] != first["profiles"]["file_sha256"]
    assert third["exposure_digest"] != first["exposure_digest"]


def test_an_edited_queue_item_with_its_old_digest_is_refused(tmp_path: Path) -> None:
    queue_path = _queue(tmp_path)
    queue = _load(queue_path)
    queue["items"][0]["impact_candidates"][0]["skill_ref"] = "payroll-tax-contractors"
    queue_path.write_text(json.dumps(queue, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    queue_path.with_suffix(".md").unlink()

    with pytest.raises(MonitorError, match="queue_digest does not match"):
        exposure(queue_path=queue_path, profiles_path=PROFILES)


def test_a_stale_markdown_companion_is_refused(tmp_path: Path) -> None:
    queue_path = _queue(tmp_path)
    markdown = queue_path.with_suffix(".md")
    markdown.write_text(markdown.read_text(encoding="utf-8") + "edited\n", encoding="utf-8")

    with pytest.raises(MonitorError, match="Markdown does not match"):
        exposure(queue_path=queue_path, profiles_path=PROFILES)


def test_blocked_and_not_evaluated_items_keep_their_state(tmp_path: Path) -> None:
    observation = _observation()
    observation["complete"] = False
    observation["observations"] = observation["observations"][:1]
    observation["observations"][0].update(
        state="LOOKUP_FAILED",
        observed_compilation_number=None,
        observed_compilation_date=None,
        observed_register_document_id=None,
        error_category="timeout",
    )
    report = exposure(queue_path=_queue(tmp_path, observation=observation), profiles_path=PROFILES)

    statuses = {item["change_kind"]: item for item in report["items"]}
    assert report["queue"]["run_status"] == "BLOCKED"
    assert report["queue"]["observation_complete"] is False
    assert statuses["INCOMPLETE_SCOPE"]["exposure_status"] == "NOT_EVALUATED"
    assert statuses["INCOMPLETE_SCOPE"]["source"] is None
    assert statuses["MISSING_OBSERVATION"]["exposure_status"] == "NOT_EVALUATED"
    # A blocked item that names a skill still names it: the reviewer sees the
    # client prompt, and the item's own BLOCKED state travels with it.
    lookup = statuses["LOOKUP_FAILED"]
    assert lookup["state"] == "BLOCKED"
    assert lookup["exposure_status"] == "PROFILES_MATCHED"
    assert [match["profile_id"] for match in lookup["matches"]] == ["FAB-CLIENT-001", "FAB-CLIENT-002"]
    assert "- Source: none; the observation scope was incomplete." in render_exposure_markdown(report)


def test_unmapped_and_unmatched_items_are_distinguished(tmp_path: Path) -> None:
    observation = _observation()
    observation["observations"][1].update(
        state="SUPERSEDED",
        observed_compilation_number="2",
        observed_compilation_date="2099-08-01",
        observed_register_document_id="F2099C00002",
    )
    queue_path = _queue(tmp_path, mapping=SKILL_MAP, observation=observation)
    profiles = _profiles(tmp_path, [{"profile_id": "FAB-9", "skill_refs": ["cashflow-forecast"]}])
    report = exposure(queue_path=queue_path, profiles_path=profiles)

    statuses = {item["source"]["register_id"]: item["exposure_status"] for item in report["items"]}
    assert statuses == {"C2099A00001": "NO_PROFILE_MATCH", "F2099L00001": "UNMAPPED_SOURCE"}
    assert report["profiles_without_candidate_match"] == ["FAB-9"]
    assert "- No profile lists a candidate skill for this item." in render_exposure_markdown(report)


@pytest.mark.parametrize(
    "profiles, message",
    [
        ([{"profile_id": "FAB-1", "skill_refs": ["bas-preparation"], "name": "x"}], "must contain exactly"),
        ([{"profile_id": "Jane Citizen", "skill_refs": ["bas-preparation"]}], "no spaces"),
        ([{"profile_id": "FAB-1", "skill_refs": ["bas-preparation"]}] * 2, "repeats an earlier profile_id"),
        ([{"profile_id": "FAB-1", "skill_refs": [" bas-preparation"]}], "without surrounding spaces"),
        ([{"profile_id": "FAB-1", "skill_refs": ["bas-preparation", "bas-preparation"]}], "repeats a skill reference"),
        ([{"profile_id": "FAB-1", "skill_refs": []}], "must list 1 to"),
        ([], "non-empty profiles list"),
    ],
)
def test_malformed_profiles_are_refused_without_echoing_values(
    tmp_path: Path, profiles: list[dict[str, Any]], message: str
) -> None:
    with pytest.raises(MonitorError, match=message) as caught:
        exposure(queue_path=_queue(tmp_path), profiles_path=_profiles(tmp_path, profiles))
    assert "Jane" not in str(caught.value)


def test_an_unsupported_profile_schema_is_refused(tmp_path: Path) -> None:
    path = _write(tmp_path / "p.json", {**_load(PROFILES), "schema_version": "au-tax-client-profiles.v0"})
    with pytest.raises(MonitorError, match="unsupported schema"):
        exposure(queue_path=_queue(tmp_path), profiles_path=path)


def test_an_oversized_profiles_file_is_refused(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(exposure_module, "MAX_PROFILE_BYTES", 10)
    with pytest.raises(MonitorError, match="exceeds 10 bytes"):
        exposure(queue_path=_queue(tmp_path), profiles_path=PROFILES)


def test_the_report_never_replaces_an_input(tmp_path: Path) -> None:
    queue_path = _queue(tmp_path)
    out = tmp_path / "out"
    profiles = _write(out / "client-exposure.json", _load(PROFILES))
    report = exposure(queue_path=queue_path, profiles_path=profiles)
    with pytest.raises(MonitorError, match="would replace an input"):
        write_exposure(report, out, inputs=(queue_path, profiles))
    assert _load(profiles) == _load(PROFILES)


@pytest.mark.skipif(os.name != "posix", reason="POSIX permission bits only")
def test_the_report_files_are_owner_only(tmp_path: Path) -> None:
    queue_path = _queue(tmp_path)
    report = exposure(queue_path=queue_path, profiles_path=PROFILES)
    paths = write_exposure(report, tmp_path / "out", inputs=(queue_path, PROFILES))
    for path in paths.values():
        assert path.stat().st_mode & 0o777 == 0o600


def test_output_stems_are_package_names_not_paths(tmp_path: Path) -> None:
    with pytest.raises(MonitorError, match="stem"):
        output_paths(tmp_path, stem="../escape")


def test_cli_exposure_exit_status_follows_the_queue(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    queue_path = _queue(tmp_path)
    assert main(["exposure", "--queue", str(queue_path), "--profiles", str(PROFILES), "--out", "report"]) == 0
    written = _load(tmp_path / "report" / "client-exposure.json")
    assert written["exposure_digest"].startswith("sha256:")
    markdown = (tmp_path / "report" / "client-exposure.md").read_text(encoding="utf-8")
    assert markdown == render_exposure_markdown(written)

    observation = _observation()
    observation["complete"] = False
    blocked = _queue(tmp_path, observation=observation, name="blocked")
    assert main(["exposure", "--queue", str(blocked), "--profiles", str(PROFILES), "--out", "blocked-report"]) == 2


def test_cli_exposure_reports_a_bad_queue_as_blocked(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    missing = tmp_path / "missing.json"
    assert main(["exposure", "--queue", str(missing), "--profiles", str(PROFILES), "--out", str(tmp_path / "x")]) == 2
    assert "tax-radar-au: blocked:" in capsys.readouterr().err
