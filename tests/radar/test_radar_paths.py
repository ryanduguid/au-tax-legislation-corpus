"""Refusal and failure paths of the radar half that the scenario suites leave unexercised."""

from __future__ import annotations

import copy
import io
import json
import runpy
import sys
from pathlib import Path
from typing import Any

import pytest
import tax_radar_au.exposure as exposure_module
import tax_radar_au.monitor as monitor_module
import tax_radar_au.persist as persist_module
import tax_radar_au.util as util_module
from tax_radar_au.cli import main
from tax_radar_au.errors import MonitorError
from tax_radar_au.exposure import exposure, render_exposure_markdown
from tax_radar_au.persist import refuse_input_overwrite, write_queue_files, write_receipt
from tax_radar_au.util import SourceSnapshot, SourceTooLargeError, load_json, sample_path

from tests.radar import test_monitor as monitor_fixtures
from tests.radar import test_radar_cli as cli_fixtures

ANOTHER_DIGEST = "sha256:" + "b" * 64


def _write(path: Path, payload: Any) -> Path:
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


def _unchanged_observation() -> dict[str, Any]:
    observation = monitor_fixtures._payload("observations", "sample-register-observation.json")
    monitor_fixtures._clear_compilation(observation["observations"][0])
    observation["observations"][0]["state"] = "UNCHANGED"
    return observation


# monitor: loaders


def test_a_calendar_impossible_date_is_refused_after_the_pattern_matches() -> None:
    with pytest.raises(MonitorError, match="^probe must be an ISO date.$"):
        monitor_module._iso_date("2026-02-30", field="probe")


def test_a_calendar_impossible_timestamp_is_refused_after_the_pattern_matches() -> None:
    with pytest.raises(MonitorError, match="^probe must be an ISO 8601 timestamp.$"):
        monitor_module._iso_timestamp("2026-02-30T00:00:00Z", field="probe")


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda raw: raw.update(source="Somewhere else"),
            "Baseline must be a Federal Register title index with a titles list.",
        ),
        (
            lambda raw: raw["titles"][0].pop("name"),
            "Baseline title 1 has an invalid shape.",
        ),
        (
            lambda raw: raw["titles"][0].update(version_is_current="yes"),
            "title 1 version_is_current must be a boolean.",
        ),
    ],
    ids=["source", "title-shape", "current-flag"],
)
def test_baseline_shape_refusals(tmp_path: Path, mutate: Any, message: str) -> None:
    baseline = monitor_fixtures._payload("baseline", "sample-sources.json")
    mutate(baseline)
    with pytest.raises(MonitorError) as caught:
        monitor_fixtures._compare_fixtures(tmp_path, baseline=baseline)
    assert str(caught.value) == message  # nosec B101: pytest outcome assertion


def test_an_observation_that_is_not_an_object_is_refused(tmp_path: Path) -> None:
    with pytest.raises(MonitorError) as caught:
        monitor_fixtures._compare_fixtures(tmp_path, observation=[])
    assert str(caught.value) == (  # nosec B101: pytest outcome assertion
        "Register observation must be a JSON object with schema_version."
    )


def test_an_observation_with_a_non_boolean_complete_flag_is_refused(tmp_path: Path) -> None:
    observation = monitor_fixtures._payload("observations", "sample-register-observation.json")
    observation["complete"] = "true"
    with pytest.raises(MonitorError) as caught:
        monitor_fixtures._compare_fixtures(tmp_path, observation=observation)
    assert str(caught.value) == "Observation complete/observations fields are invalid."  # nosec B101: pytest outcome assertion


def test_an_observation_repeating_a_register_id_is_refused(tmp_path: Path) -> None:
    observation = monitor_fixtures._payload("observations", "sample-register-observation.json")
    observation["observations"].append(copy.deepcopy(observation["observations"][0]))
    with pytest.raises(MonitorError) as caught:
        monitor_fixtures._compare_fixtures(tmp_path, observation=observation)
    assert str(caught.value) == "Observation contains duplicate register IDs."  # nosec B101: pytest outcome assertion


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda raw: raw.update(schema_version="au-tax-source-skill-map.v0"),
            "Source-to-skill map has an unsupported schema.",
        ),
        (
            lambda raw: raw["entries"][0].pop("owner_role"),
            "Mapping entry 1 has an invalid shape.",
        ),
    ],
    ids=["schema", "entry-shape"],
)
def test_mapping_shape_refusals(tmp_path: Path, mutate: Any, message: str) -> None:
    mapping = monitor_fixtures._payload("mappings", "sample-source-skill-map.json")
    mutate(mapping)
    with pytest.raises(MonitorError) as caught:
        monitor_fixtures._compare_fixtures(tmp_path, mapping=mapping)
    assert str(caught.value) == message  # nosec B101: pytest outcome assertion


def test_a_fully_unchanged_observation_reports_no_change(tmp_path: Path) -> None:
    queue = monitor_fixtures._compare_fixtures(tmp_path, observation=_unchanged_observation())

    assert queue["run_status"] == "NO_CHANGE_DETECTED"  # nosec B101: pytest outcome assertion
    assert queue["items"] == []  # nosec B101: pytest outcome assertion
    assert (  # nosec B101: pytest outcome assertion
        "No changed items were identified within the complete synthetic observation scope."
        in monitor_module.render_markdown(queue)
    )


# monitor: stored queue validation


def _item(queue: dict[str, Any]) -> dict[str, Any]:
    return queue["items"][0]


def _drop_observed_compilation(queue: dict[str, Any]) -> None:
    _item(queue)["source"]["observed_compilation"] = None


def _duplicate_candidate(queue: dict[str, Any]) -> None:
    candidates = _item(queue)["impact_candidates"]
    candidates.append(copy.deepcopy(candidates[0]))


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda queue: queue.update(schema_version="au-tax-impact-queue.v1"),
            "Queue or decision schema version is unsupported.",
        ),
        (
            lambda queue: queue["baseline"].update(id=ANOTHER_DIGEST),
            "Impact queue baseline id must equal the baseline source digest.",
        ),
        (
            lambda queue: queue["observation"].update(complete="yes"),
            "Impact queue observation complete must be a boolean.",
        ),
        (
            lambda queue: queue["observation"].update(id=ANOTHER_DIGEST),
            "Impact queue observation id must equal the observation source digest.",
        ),
        (
            lambda queue: queue.update(items={}),
            "Impact queue items must be a list.",
        ),
        (
            lambda queue: _item(queue).update(change_kind="REPEALED"),
            "Impact queue item 1 has an unsupported change_kind.",
        ),
        (
            _drop_observed_compilation,
            "Impact queue item 1 source must include observed_compilation for SUPERSEDED.",
        ),
        (
            lambda queue: _item(queue).update(impact_candidates={}),
            "Impact queue item 1 impact_candidates must be a list.",
        ),
        (
            lambda queue: _item(queue)["impact_candidates"][0].update(mapping_basis="title_match"),
            "Impact queue item 1 candidate 1 has an unsupported mapping_basis.",
        ),
        (
            _duplicate_candidate,
            "Impact queue item 1 contains duplicate candidate mapping IDs.",
        ),
        (
            lambda queue: _item(queue).update(mapping_status="GUESSED"),
            "Impact queue item 1 has an unsupported mapping_status.",
        ),
        (
            lambda queue: _item(queue).update(impact_candidates=[]),
            "Impact queue item 1 cannot be MAPPED without a candidate.",
        ),
        (
            lambda queue: _item(queue).update(state="BLOCKED"),
            "Impact queue item 1 change_kind must remain OPEN.",
        ),
        (
            lambda queue: queue.update(run_id=ANOTHER_DIGEST),
            "Impact queue run_id does not match its exact source digests.",
        ),
    ],
    ids=[
        "schema",
        "baseline-id",
        "complete-flag",
        "observation-id",
        "items",
        "change-kind",
        "superseded-compilation",
        "candidates",
        "mapping-basis",
        "duplicate-candidate",
        "mapping-status",
        "mapped-without-candidate",
        "state",
        "run-id",
    ],
)
def test_stored_queue_refusals(mutate: Any, message: str) -> None:
    queue = monitor_fixtures._queue()
    mutate(queue)
    with pytest.raises(MonitorError) as caught:
        monitor_module.verify_queue_integrity(queue)
    assert str(caught.value) == message  # nosec B101: pytest outcome assertion


def test_an_incomplete_scope_item_cannot_name_a_source() -> None:
    with pytest.raises(MonitorError) as caught:
        monitor_module._validate_source({}, item_index=3, change_kind="INCOMPLETE_SCOPE")
    assert str(caught.value) == (  # nosec B101: pytest outcome assertion
        "Impact queue item 3 source must be null for INCOMPLETE_SCOPE."
    )


def test_an_observed_compilation_is_refused_outside_superseded_kinds() -> None:
    source = copy.deepcopy(_item(monitor_fixtures._queue())["source"])
    with pytest.raises(MonitorError) as caught:
        monitor_module._validate_source(source, item_index=1, change_kind="NO_LONGER_IN_FORCE")
    assert str(caught.value) == (  # nosec B101: pytest outcome assertion
        "Impact queue item 1 source observed_compilation is unsupported for NO_LONGER_IN_FORCE."
    )


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda decision: decision.update(schema_version="au-tax-technical-review.v1"),
            "Queue or decision schema version is unsupported.",
        ),
        (
            lambda decision: decision.update(decisions=[]),
            "Technical review decision must include at least one decision.",
        ),
    ],
    ids=["schema", "no-decisions"],
)
def test_decision_refusals(tmp_path: Path, mutate: Any, message: str) -> None:
    queue = monitor_fixtures._queue()
    decision = monitor_fixtures._matching_decision(queue)
    mutate(decision)
    with pytest.raises(MonitorError) as caught:
        monitor_fixtures._validate_payloads(tmp_path, queue, decision)
    assert str(caught.value) == message  # nosec B101: pytest outcome assertion


# exposure


def _queue_file(tmp_path: Path, observation: dict[str, Any] | None = None) -> Path:
    observation_path = sample_path("observations", "sample-register-observation.json")
    if observation is not None:
        observation_path = _write(tmp_path / "observation.json", observation)
    queue = monitor_module.compare(
        baseline_path=sample_path("baseline", "sample-sources.json"),
        observation_path=observation_path,
        mapping_path=sample_path("mappings", "sample-workpaper-map.json"),
    )
    return monitor_module.write_queue(queue, tmp_path / "queue")["json"]


def _profiles_file(tmp_path: Path, profiles: list[dict[str, Any]]) -> Path:
    return _write(
        tmp_path / "profiles.json",
        {
            "schema_version": "au-tax-client-profiles.v1",
            "profiles_version": "test.1",
            "profiles": profiles,
        },
    )


def test_profiles_beyond_the_limit_are_refused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(exposure_module, "MAX_PROFILES", 1)
    profiles = _profiles_file(
        tmp_path,
        [
            {"profile_id": "FAB-A", "skill_refs": ["bas-preparation"]},
            {"profile_id": "FAB-B", "skill_refs": ["bas-preparation"]},
        ],
    )
    with pytest.raises(MonitorError) as caught:
        exposure(queue_path=_queue_file(tmp_path), profiles_path=profiles)
    assert str(caught.value) == "Client profiles exceed 1 profiles."  # nosec B101: pytest outcome assertion


def test_an_empty_queue_lists_no_profile(tmp_path: Path) -> None:
    report = exposure(
        queue_path=_queue_file(tmp_path, _unchanged_observation()),
        profiles_path=sample_path("profiles", "sample-client-profiles.json"),
    )

    assert report["items"] == []  # nosec B101: pytest outcome assertion
    assert (  # nosec B101: pytest outcome assertion
        "The queue holds no items, so no profile is listed." in render_exposure_markdown(report)
    )


def test_every_profile_matching_is_stated(tmp_path: Path) -> None:
    profiles = _profiles_file(
        tmp_path, [{"profile_id": "FAB-A", "skill_refs": ["bas-preparation"]}]
    )
    report = exposure(queue_path=_queue_file(tmp_path), profiles_path=profiles)

    assert report["profiles_without_candidate_match"] == []  # nosec B101: pytest outcome assertion
    assert (  # nosec B101: pytest outcome assertion
        "Every profile matched at least one candidate." in render_exposure_markdown(report)
    )


# util


def test_a_negative_source_limit_is_a_programming_error(tmp_path: Path) -> None:
    source = _write(tmp_path / "source.json", {})
    with pytest.raises(ValueError, match="^source byte limit must be non-negative$"):
        SourceSnapshot.capture(source, label="probe", limit=-1)


def test_an_oversized_snapshot_is_refused_before_parsing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(util_module, "MAX_JSON_BYTES", 4)
    snapshot = SourceSnapshot(path=tmp_path / "probe.json", content=b"[1, 2]", sha256="")
    with pytest.raises(SourceTooLargeError, match="^probe exceeds 4 bytes.$"):
        load_json(snapshot, label="probe")


# persist


def _partials(directory: Path) -> list[str]:
    return sorted(path.name for path in directory.iterdir() if path.name.endswith(".partial"))


def test_a_failed_rollback_still_raises_the_original_commit_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "pair"
    write_queue_files("old json\n", "old markdown\n", output)
    real_replace = persist_module.os.replace
    failing = {"value": False}

    def fail_from_the_markdown_commit(source: Path, destination: Path) -> None:
        if Path(destination).suffix == ".md":
            failing["value"] = True
        if failing["value"]:
            raise OSError(5, "simulated commit failure")
        real_replace(source, destination)

    monkeypatch.setattr(persist_module.os, "replace", fail_from_the_markdown_commit)
    with pytest.raises(OSError, match="simulated commit failure"):
        write_queue_files("new json\n", "new markdown\n", output)

    # The restore could not run, so both parked originals stay beside the pair.
    assert (output / "impact-queue.json").read_text(encoding="utf-8") == "new json\n"  # nosec B101: pytest outcome assertion
    assert len(_partials(output)) == 2  # nosec B101: pytest outcome assertion


def test_a_directory_at_a_queue_destination_is_refused(tmp_path: Path) -> None:
    output = tmp_path / "pair"
    (output / "impact-queue.json").mkdir(parents=True)
    with pytest.raises(MonitorError) as caught:
        write_queue_files("json\n", "markdown\n", output)
    assert str(caught.value) == "queue destinations must be ordinary files."  # nosec B101: pytest outcome assertion
    assert _partials(output) == []  # nosec B101: pytest outcome assertion


def test_a_failed_first_commit_removes_the_new_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "pair"
    real_replace = persist_module.os.replace

    def fail_markdown(source: Path, destination: Path) -> None:
        if Path(destination).suffix == ".md":
            raise OSError(5, "simulated markdown failure")
        real_replace(source, destination)

    monkeypatch.setattr(persist_module.os, "replace", fail_markdown)
    with pytest.raises(OSError, match="simulated markdown failure"):
        write_queue_files("json\n", "markdown\n", output)
    assert list(output.iterdir()) == []  # nosec B101: pytest outcome assertion


def test_a_staging_stream_that_cannot_open_leaves_no_partial(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "pair"

    def no_stream(*_args: object, **_kwargs: object) -> None:
        raise OSError(24, "simulated descriptor exhaustion")

    monkeypatch.setattr(persist_module.os, "fdopen", no_stream)
    with pytest.raises(OSError, match="simulated descriptor exhaustion"):
        write_queue_files("json\n", "markdown\n", output)
    assert list(output.iterdir()) == []  # nosec B101: pytest outcome assertion


def test_a_second_write_removes_the_parked_originals(tmp_path: Path) -> None:
    output = tmp_path / "pair"
    write_queue_files("old json\n", "old markdown\n", output)
    write_queue_files("new json\n", "new markdown\n", output)

    assert sorted(path.name for path in output.iterdir()) == [  # nosec B101: pytest outcome assertion
        "impact-queue.json",
        "impact-queue.md",
    ]
    assert (output / "impact-queue.md").read_text(encoding="utf-8") == "new markdown\n"  # nosec B101: pytest outcome assertion


@pytest.mark.parametrize("platform_name", ["nt", "posix"])
def test_a_receipt_publishes_through_either_platform_primitive(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, platform_name: str
) -> None:
    receipt = tmp_path / "receipt.json"
    with monkeypatch.context() as patch:
        patch.setattr(persist_module.os, "name", platform_name)
        assert write_receipt("{}\n", receipt, inputs=()) == receipt  # nosec B101: pytest outcome assertion
    assert receipt.read_text(encoding="utf-8") == "{}\n"  # nosec B101: pytest outcome assertion
    assert _partials(tmp_path) == []  # nosec B101: pytest outcome assertion


@pytest.mark.parametrize(
    "cleanup_error", [FileNotFoundError(2, "already gone"), PermissionError(13, "locked")]
)
def test_a_receipt_stream_failure_keeps_the_original_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, cleanup_error: OSError
) -> None:
    original = OSError(24, "simulated descriptor exhaustion")

    def no_stream(*_args: object, **_kwargs: object) -> None:
        raise original

    def failing_unlink(self: Path, missing_ok: bool = False) -> None:
        raise cleanup_error

    monkeypatch.setattr(persist_module.os, "fdopen", no_stream)
    monkeypatch.setattr(Path, "unlink", failing_unlink)
    with pytest.raises(OSError) as caught:
        write_receipt("{}\n", tmp_path / "receipt.json", inputs=())
    assert caught.value is original  # nosec B101: pytest outcome assertion
    expected_cause = None if isinstance(cleanup_error, FileNotFoundError) else cleanup_error
    assert caught.value.__cause__ is expected_cause  # nosec B101: pytest outcome assertion
    assert not (tmp_path / "receipt.json").exists()  # nosec B101: pytest outcome assertion


def test_a_published_receipt_tolerates_an_already_removed_staging_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def already_gone(self: Path, missing_ok: bool = False) -> None:
        raise FileNotFoundError(2, "already gone")

    receipt = tmp_path / "receipt.json"
    monkeypatch.setattr(Path, "unlink", already_gone)
    assert write_receipt("{}\n", receipt, inputs=()) == receipt  # nosec B101: pytest outcome assertion
    assert receipt.read_text(encoding="utf-8") == "{}\n"  # nosec B101: pytest outcome assertion


def test_a_published_receipt_reports_a_staging_file_it_cannot_remove(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def locked(self: Path, missing_ok: bool = False) -> None:
        raise PermissionError(13, "locked")

    receipt = tmp_path / "receipt.json"
    monkeypatch.setattr(Path, "unlink", locked)
    with pytest.raises(MonitorError) as caught:
        write_receipt("{}\n", receipt, inputs=())
    assert str(caught.value) == (  # nosec B101: pytest outcome assertion
        "Validation receipt was published, but its staging file could not be removed."
    )
    assert receipt.read_text(encoding="utf-8") == "{}\n"  # nosec B101: pytest outcome assertion


def test_an_input_that_cannot_be_resolved_blocks_the_write(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def unresolvable(self: Path, strict: bool = False) -> Path:
        raise OSError(40, "simulated symbolic link loop")

    monkeypatch.setattr(Path, "resolve", unresolvable)
    with pytest.raises(MonitorError) as caught:
        refuse_input_overwrite((tmp_path / "out.json",), (tmp_path / "in.json",))
    assert str(caught.value) == "Output paths could not be checked against input files."  # nosec B101: pytest outcome assertion


# cli


class _LostStream(io.TextIOBase):
    """A stream whose every write fails, like a pipe whose reader has gone."""

    def __init__(self, *, fail_close: bool) -> None:
        super().__init__()
        self.fail_close = fail_close

    def write(self, text: str) -> int:
        raise BrokenPipeError(32, "broken pipe")

    def close(self) -> None:
        super().close()
        # Fail one explicit close only, so the finaliser's close stays quiet.
        if self.fail_close:
            self.fail_close = False
            raise OSError(32, "broken pipe")


def test_a_run_with_both_streams_gone_keeps_its_status(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    stdout = _LostStream(fail_close=True)
    monkeypatch.setattr(sys, "stdout", stdout)
    monkeypatch.setattr(sys, "stderr", _LostStream(fail_close=False))

    assert main(cli_fixtures._compare_argv(tmp_path / "queue")) == 0  # nosec B101: pytest outcome assertion
    assert stdout.closed and not stdout.fail_close  # nosec B101: pytest outcome assertion
    assert (tmp_path / "queue" / "impact-queue.json").is_file()  # nosec B101: pytest outcome assertion


def test_exposure_into_an_occupied_output_path_is_blocked(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert main(cli_fixtures._compare_argv(tmp_path / "queue")) == 0  # nosec B101: pytest outcome assertion
    occupied = tmp_path / "occupied"
    occupied.write_text("already here\n", encoding="utf-8")
    capsys.readouterr()

    code = main([
        "exposure",
        "--queue", str(tmp_path / "queue" / "impact-queue.json"),
        "--profiles", str(sample_path("profiles", "sample-client-profiles.json")),
        "--out", str(occupied),
    ])

    assert code == 2  # nosec B101: pytest outcome assertion
    assert "blocked: the output directory could not be written:" in capsys.readouterr().err  # nosec B101: pytest outcome assertion


def _review_argv(tmp_path: Path) -> list[str]:
    assert main(cli_fixtures._compare_argv(tmp_path / "queue")) == 0  # nosec B101: pytest outcome assertion
    return [
        "validate-review",
        "--queue", str(tmp_path / "queue" / "impact-queue.json"),
        "--decision", str(sample_path("decisions", "sample-technical-review.json")),
    ]


def test_validate_review_without_out_only_reports(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    argv = _review_argv(tmp_path)
    capsys.readouterr()

    assert main(argv) == 0  # nosec B101: pytest outcome assertion
    assert capsys.readouterr().out == "tax-radar-au: DECISION_RECORDED; 1 decision(s)\n"  # nosec B101: pytest outcome assertion


def test_a_receipt_under_a_regular_file_is_blocked(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    argv = _review_argv(tmp_path)
    blocker = tmp_path / "blocker"
    blocker.write_text("", encoding="utf-8")
    capsys.readouterr()

    assert main([*argv, "--out", str(blocker / "receipt.json")]) == 2  # nosec B101: pytest outcome assertion
    assert "blocked: the validation output file could not be written:" in capsys.readouterr().err  # nosec B101: pytest outcome assertion


def test_the_package_runs_as_a_module(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(sys, "argv", ["tax-radar-au", *cli_fixtures._compare_argv(tmp_path / "queue")])
    with pytest.raises(SystemExit) as exit_info:
        runpy.run_module("tax_radar_au", run_name="__main__")
    assert exit_info.value.code == 0  # nosec B101: pytest outcome assertion
    assert (tmp_path / "queue" / "impact-queue.md").is_file()  # nosec B101: pytest outcome assertion

