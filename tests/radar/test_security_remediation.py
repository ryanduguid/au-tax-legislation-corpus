"""Input and receipt regressions; B101 marks pytest outcome assertions."""

from __future__ import annotations

import json
import os
from pathlib import Path
from unittest import mock

import pytest
from tax_radar_au import exposure as exposure_module
from tax_radar_au import persist, util
from tax_radar_au.cli import main
from tax_radar_au.errors import MonitorError, SourceTooLargeError
from tax_radar_au.monitor import compare, render_markdown
from tax_radar_au.util import sample_path

from tests.radar import test_exposure as exposure_fixtures


def test_relative_symlink_and_parent_components_use_one_interpretation(tmp_path, monkeypatch):
    root = tmp_path / "work"
    deep = root / "safe" / "deep"
    deep.mkdir(parents=True)
    outside = tmp_path / "outside"
    outside.mkdir()
    try:
        (root / "jump").symlink_to(deep, target_is_directory=True)
        (root / "out").symlink_to(outside, target_is_directory=True)
    except OSError as exc:
        pytest.skip(f"directory symlinks unavailable: {exc}")
    monkeypatch.chdir(root)
    try:
        receipt = persist.write_receipt("complete", Path("jump/../out/receipt.json"), inputs=())
    except MonitorError:
        with pytest.raises(MonitorError):
            persist.output_paths(Path("jump/../out"))
    else:
        assert receipt == root / "safe" / "out" / "receipt.json"  # nosec B101
        assert receipt.read_text(encoding="utf-8") == "complete"  # nosec B101
        assert persist.output_paths(Path("jump/../out"))[0].parent == root / "safe" / "out"  # nosec B101
    assert list(outside.iterdir()) == []  # nosec B101


def test_interrupt_after_completed_queue_promotion_restores_old_bytes(tmp_path, monkeypatch):
    output = tmp_path / "queue"
    paths = persist.write_queue_files("old json", "old markdown", output)
    replace = persist.os.replace
    interrupted = False

    def interrupt_after_replace(source, destination):
        nonlocal interrupted
        replace(source, destination)
        if not interrupted and Path(destination) == paths["json"]:
            interrupted = True
            raise KeyboardInterrupt("interrupted after promotion")

    monkeypatch.setattr(persist.os, "replace", interrupt_after_replace)
    with pytest.raises(KeyboardInterrupt):
        persist.write_queue_files("new json", "new markdown", output)
    assert paths["json"].read_text(encoding="utf-8") == "old json"  # nosec B101
    assert paths["markdown"].read_text(encoding="utf-8") == "old markdown"  # nosec B101
    assert list(output.glob("*.partial")) == []  # nosec B101


def test_backup_link_failure_keeps_the_original_path_and_bytes(tmp_path, monkeypatch):
    output = tmp_path / "queue"
    paths = persist.write_queue_files("old json", "old markdown", output)

    def fail_link(_source, _target, **_kwargs):
        assert paths["json"].read_text(encoding="utf-8") == "old json"  # nosec B101
        raise OSError("backup links unavailable")

    monkeypatch.setattr(persist.os, "link", fail_link)
    with pytest.raises(OSError, match="backup links unavailable"):
        persist.write_queue_files("new json", "new markdown", output)
    assert paths["json"].read_text(encoding="utf-8") == "old json"  # nosec B101
    assert paths["markdown"].read_text(encoding="utf-8") == "old markdown"  # nosec B101
    assert list(output.glob("*.partial")) == []  # nosec B101


@pytest.mark.parametrize("json_exists", (False, True))
def test_all_backups_are_prepared_before_any_promotion(tmp_path, monkeypatch, json_exists):
    output = tmp_path / "queue"
    output.mkdir()
    json_path = output / "impact-queue.json"
    markdown_path = output / "impact-queue.md"
    if json_exists:
        json_path.write_bytes(b"old json")
    markdown_path.write_bytes(b"old markdown")
    link = persist.os.link

    def fail_markdown_backup(source, target, **kwargs):
        if Path(source) == markdown_path:
            raise OSError("backup links unavailable")
        return link(source, target, **kwargs)

    promote = mock.Mock(wraps=persist.os.replace)
    monkeypatch.setattr(persist.os, "link", fail_markdown_backup)
    monkeypatch.setattr(persist.os, "replace", promote)
    with pytest.raises(OSError, match="backup links unavailable"):
        persist.write_queue_files("new json", "new markdown", output)
    promote.assert_not_called()
    assert json_path.exists() == json_exists  # nosec B101
    if json_exists:
        assert json_path.read_bytes() == b"old json"  # nosec B101
    assert markdown_path.read_bytes() == b"old markdown"  # nosec B101
    assert list(output.glob("*.partial")) == []  # nosec B101


def test_backup_retains_the_original_inode_and_access_controls(tmp_path):
    destination = tmp_path / "old.json"
    destination.write_bytes(b"old private bytes")
    destination.chmod(0o640)
    original = destination.stat()
    backup = persist._backup_existing(destination)
    assert backup is not None  # nosec B101
    retained = backup.stat()
    assert (retained.st_dev, retained.st_ino, retained.st_uid, retained.st_gid, retained.st_mode) == (  # nosec B101
        original.st_dev, original.st_ino, original.st_uid, original.st_gid, original.st_mode
    )
    persist._restore_quietly(backup, destination)
    assert destination.read_bytes() == b"old private bytes"  # nosec B101
    assert not backup.exists()  # nosec B101


def oversized_file(path: Path) -> Path:
    with path.open("wb") as stream:
        stream.truncate(util.MAX_JSON_BYTES + 1)
    return path


def test_json_depth_and_parser_recursion_fail_as_monitor_errors(tmp_path, monkeypatch):
    path = tmp_path / "input.json"
    path.write_text("[" * 65 + "0" + "]" * 65, encoding="utf-8")
    with pytest.raises(MonitorError, match="nesting levels"):
        util.load_json(path, label="input")
    path.write_text(json.dumps({"literal": "[" * 1000 + '\\"'}), encoding="utf-8")
    assert util.load_json(path, label="input")["literal"].startswith("[")  # nosec B101
    path.write_text("[" * 64 + "0" + "]" * 64, encoding="utf-8")
    assert isinstance(util.load_json(path, label="input"), list)  # nosec B101

    def recurse(*_args, **_kwargs):
        raise RecursionError("parser recursion")

    monkeypatch.setattr(util.json, "loads", recurse)
    with pytest.raises(MonitorError, match="not valid JSON"):
        util.load_json(path, label="input")


def test_path_loader_is_bounded_by_default(tmp_path):
    path = oversized_file(tmp_path / "input.json")
    with pytest.raises(SourceTooLargeError, match="50000000"):
        util.load_json(path, label="input")


@pytest.mark.parametrize("option", ["baseline", "observation", "mapping"])
def test_every_compare_json_input_is_bounded(tmp_path, option):
    inputs = {
        "baseline_path": sample_path("baseline", "sample-sources.json"),
        "observation_path": sample_path("observations", "sample-register-observation.json"),
        "mapping_path": sample_path("mappings", "sample-source-skill-map.json"),
    }
    inputs[option + "_path"] = oversized_file(tmp_path / "oversized.json")
    with pytest.raises(SourceTooLargeError):
        compare(**inputs)


@pytest.mark.parametrize("option", ["queue", "decision"])
def test_review_budget_blocks_before_receipt_creation(tmp_path, capsys, option):
    queue = exposure_fixtures._queue(tmp_path, mapping=exposure_fixtures.SKILL_MAP)
    decision = sample_path("decisions", "sample-technical-review.json")
    if option == "queue":
        queue = oversized_file(tmp_path / "oversized.json")
    else:
        decision = oversized_file(tmp_path / "oversized.json")
    receipt = tmp_path / "new-parent" / "receipt.json"
    assert main(["validate-review", "--queue", str(queue), "--decision", str(decision),  # nosec B101
                 "--out", str(receipt)]) == 2
    assert "exceeds 50000000 bytes" in capsys.readouterr().err  # nosec B101
    assert not receipt.parent.exists()  # nosec B101


def test_receipt_relative_escape_is_refused_before_parents(tmp_path, monkeypatch):
    working = tmp_path / "working"
    working.mkdir()
    monkeypatch.chdir(working)
    with pytest.raises(MonitorError, match="stay within"):
        persist.write_receipt("{}\n", Path("../outside/receipt.json"), inputs=())
    assert not (tmp_path / "outside").exists()  # nosec B101
    absolute = tmp_path / "explicit" / "receipt.json"
    assert persist.write_receipt("{}\n", absolute, inputs=()) == absolute  # nosec B101
    assert absolute.read_text(encoding="utf-8") == "{}\n"  # nosec B101


def test_receipt_dangling_link_is_preserved(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    path = tmp_path / "receipt.json"
    try:
        path.symlink_to(tmp_path / "absent.json")
    except OSError as exc:
        pytest.skip(f"symbolic links unavailable: {exc}")
    with pytest.raises(MonitorError, match="must not exist"):
        persist.write_receipt("{}\n", Path("receipt.json"), inputs=())
    assert path.is_symlink()  # nosec B101
    assert not (tmp_path / "absent.json").exists()  # nosec B101


def test_receipt_destination_race_cannot_overwrite_another_file(tmp_path, monkeypatch):
    path = tmp_path / "receipt.json"
    operation = "rename" if os.name == "nt" else "link"
    publish = getattr(persist.os, operation)

    def competing_file(source, destination):
        Path(destination).write_text("another writer\n", encoding="utf-8")
        return publish(source, destination)

    monkeypatch.setattr(persist.os, operation, competing_file)
    with pytest.raises(FileExistsError):
        persist.write_receipt("{}\n", path, inputs=())
    assert path.read_text(encoding="utf-8") == "another writer\n"  # nosec B101
    assert list(tmp_path.iterdir()) == [path]  # nosec B101


def test_receipt_sync_failure_does_not_publish_partial_bytes(tmp_path, monkeypatch):
    def fail(_descriptor):
        raise OSError("sync failure")

    monkeypatch.setattr(persist.os, "fsync", fail)
    with pytest.raises(OSError, match="sync failure"):
        persist.write_receipt("{}\n", tmp_path / "receipt.json", inputs=())
    assert not list(tmp_path.iterdir())  # nosec B101


def test_staged_writes_keep_the_exclusive_descriptors(tmp_path, monkeypatch):
    def reopened(*_args, **_kwargs):
        raise AssertionError("staged file was reopened by path")

    monkeypatch.setattr(Path, "write_text", reopened)
    pair = persist.write_queue_files("{}\n", "Literal Markdown\n", tmp_path / "pair")
    receipt = persist.write_receipt("{}\n", tmp_path / "receipt.json", inputs=())
    assert pair["json"].read_text(encoding="utf-8") == "{}\n"  # nosec B101
    assert pair["markdown"].read_text(encoding="utf-8") == "Literal Markdown\n"  # nosec B101
    assert receipt.read_text(encoding="utf-8") == "{}\n"  # nosec B101


def test_interrupt_between_backup_and_promotion_restores_the_pair(tmp_path, monkeypatch):
    output = tmp_path / "pair"
    paths = persist.write_queue_files("old json", "old markdown", output)
    replace = persist.os.replace
    interrupted = False

    def interrupt(source, destination):
        nonlocal interrupted
        if not interrupted and Path(source).suffix == ".partial" and Path(destination) == paths["json"]:
            interrupted = True
            raise KeyboardInterrupt("promotion interrupted")
        return replace(source, destination)

    monkeypatch.setattr(persist.os, "replace", interrupt)
    with pytest.raises(KeyboardInterrupt):
        persist.write_queue_files("new json", "new markdown", output)
    assert paths["json"].read_text(encoding="utf-8") == "old json"  # nosec B101
    assert paths["markdown"].read_text(encoding="utf-8") == "old markdown"  # nosec B101
    assert list(output.glob("*.partial")) == []  # nosec B101


@pytest.mark.parametrize("limit", ["MAX_ASSOCIATIONS", "MAX_MATCH_WORK", "MAX_MATCHES"])
def test_exposure_aggregate_budgets_block_the_complete_report(tmp_path, monkeypatch, limit):
    queue = exposure_fixtures._queue(tmp_path)
    profiles = exposure_fixtures._profiles(tmp_path, [
        {"profile_id": "FAB-001", "skill_refs": ["bas-preparation", "cashflow-forecast"]},
        {"profile_id": "FAB-002", "skill_refs": ["bas-preparation", "cashflow-forecast"]},
    ])
    monkeypatch.setattr(exposure_module, limit, 1)
    with pytest.raises(MonitorError, match="exceed") as caught:
        exposure_module.exposure(queue_path=queue, profiles_path=profiles)
    assert "FAB-001" not in str(caught.value)  # nosec B101
    assert "FAB-002" not in str(caught.value)  # nosec B101


def test_metadata_is_literal_text_outside_code_spans(tmp_path):
    queue_path = exposure_fixtures._queue(tmp_path)
    queue = json.loads(queue_path.read_text(encoding="utf-8"))
    source = queue["items"][0]["source"]
    source["title"] = "# **Title** _italic_ <img> ![link](url)"
    source["register_id"] = "id`close`"
    text = render_markdown(queue)
    assert r"\# \*\*Title\*\* \_italic\_ \<img\> \!\[link\]\(url\)" in text  # nosec B101
    assert r"id\`close\`" in text  # nosec B101
    assert r"`id\`close\``" not in text  # nosec B101
