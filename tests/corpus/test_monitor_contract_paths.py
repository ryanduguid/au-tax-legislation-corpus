"""Validation refusals and publication recovery of the monitor-contract export."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fadden import export_monitor_contract as contract

from tests.corpus import test_monitor_contract as fixtures


class FieldValidationTests(unittest.TestCase):
    def test_scalar_refusals(self) -> None:
        cases = [
            (contract._date, "2099-7-1", "must be an ISO calendar date."),
            (contract._date, "2099-02-30", "must be an ISO calendar date."),
            (contract._utc_timestamp, "2099-02-30T00:00:00Z",
             "must be an explicit UTC timestamp ending in Z."),
            (contract._https_url, "https://[::1", "must be an https URL."),
            (contract._object, ["not", "an", "object"], "must be an object."),
        ]
        for function, value, message in cases:
            with self.subTest(function=function.__name__, value=value):
                with self.assertRaises(contract.ContractError) as caught:
                    function(value, "probe")
                self.assertEqual(str(caught.exception), "probe " + message)

    def test_a_missing_input_is_unreadable_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(contract.ContractError, "^sources is unreadable JSON: "):
                contract._load_json(Path(temporary) / "absent.json", "sources")


class ProjectionRefusalTests(unittest.TestCase):
    def assertBaselineRefused(self, mutate, message: str) -> None:
        sources = fixtures.sources_document()
        mutate(sources)
        with self.assertRaises(contract.ContractError) as caught:
            contract.project_baseline(sources)
        self.assertEqual(str(caught.exception), message)

    def test_baseline_refusals(self) -> None:
        def first(sources):
            return sources["titles"][0]

        cases = [
            (lambda s: s.pop("source_api"), "sources is missing source_api."),
            (lambda s: s.update(source="Somewhere else"),
             "sources source must be Federal Register of Legislation."),
            (lambda s: s.update(titles=[]), "sources titles must be a non-empty list."),
            (lambda s: first(s).pop("name"),
             "sources title 1 is missing required field(s): name."),
            (lambda s: first(s).update(collection="Gazette"),
             "sources title 1 collection is unsupported."),
            (lambda s: first(s).update(version_is_current="yes"),
             "sources title 1 version_is_current must be a boolean."),
            (lambda s: first(s).update(version_is_current=False),
             "sources title 1 current_version_start is required when version_is_current is false."),
        ]
        for mutate, message in cases:
            with self.subTest(message=message):
                self.assertBaselineRefused(mutate, message)

    def test_observation_refusals(self) -> None:
        def first(facts):
            return facts["observations"][0]

        cases = [
            (lambda f: f.pop("schema_version"), "observation facts is missing schema_version."),
            (lambda f: f.update(complete="yes"),
             "observation facts complete/observations fields are invalid."),
            (lambda f: first(f).update(register_id="C2099A00099"),
             "observation facts contains a register_id outside the baseline scope."),
            (lambda f: first(f).update(state="REPEALED"),
             "observation fact 1 state is unsupported."),
        ]
        for mutate, message in cases:
            with self.subTest(message=message):
                facts = fixtures.observation_facts()
                mutate(facts)
                with self.assertRaises(contract.ContractError) as caught:
                    contract.project_observation(fixtures.sources_document(), facts)
                self.assertEqual(str(caught.exception), message)


class FileHandlingTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)

    def test_a_failed_staging_write_leaves_nothing_behind(self) -> None:
        for cleanup in (None, PermissionError("locked")):
            with self.subTest(cleanup=cleanup):
                unlink = (mock.patch.object(Path, "unlink", side_effect=cleanup) if cleanup
                          else contextlib.nullcontext())
                with unlink, self.assertRaises(TypeError):
                    contract._write_staged(self.base / "out.json", {"value": object()})
                if cleanup is None:
                    self.assertEqual(list(self.base.iterdir()), [])

    def test_an_uninspectable_destination_is_refused(self) -> None:
        with mock.patch.object(contract.os, "lstat", side_effect=PermissionError("denied")):
            with self.assertRaisesRegex(contract.ContractError,
                                        "monitor output path cannot be inspected"):
                contract._validate_existing_destination(self.base / "out.json")

    def test_a_lock_token_that_cannot_be_written_releases_the_lock(self) -> None:
        with mock.patch.object(contract.os, "write", side_effect=OSError(28, "no space")):
            with self.assertRaisesRegex(OSError, "no space"):
                with contract._OutputDirectoryLock(self.base):
                    self.fail("the lock must not be acquired")
        self.assertEqual(list(self.base.iterdir()), [])

    def test_a_lock_lost_during_a_failure_keeps_the_failure(self) -> None:
        with self.assertRaises(ValueError) as caught:
            with contract._OutputDirectoryLock(self.base) as lock:
                lock.path.unlink()
                raise ValueError("publication failed")
        self.assertIsInstance(caught.exception.__cause__, FileNotFoundError)


class RollbackTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.sources = self.base / "sources.json"
        self.facts = self.base / "facts.json"
        self.sources.write_text(json.dumps(fixtures.sources_document()), encoding="utf-8")
        self.facts.write_text(json.dumps(fixtures.observation_facts()), encoding="utf-8")
        self.out = self.base / "out"

    def publish(self, replace) -> None:
        with mock.patch.object(contract.os, "replace", side_effect=replace):
            contract.publish_pair(self.sources, self.facts, self.out)

    def names(self) -> list[str]:
        return sorted(path.name for path in self.out.iterdir())

    def test_a_first_publication_failure_removes_the_promoted_file(self) -> None:
        real_replace = os.replace

        def fail_observation(source, destination):
            if Path(destination).name == "register-observation.json":
                raise OSError(5, "simulated observation failure")
            return real_replace(source, destination)

        with self.assertRaisesRegex(OSError, "simulated observation failure"):
            self.publish(fail_observation)
        self.assertEqual(self.names(), [])

    def test_a_failure_to_park_the_second_file_restores_the_first(self) -> None:
        contract.publish_pair(self.sources, self.facts, self.out)
        before = {name: (self.out / name).read_bytes() for name in self.names()}
        real_replace = os.replace

        def fail_parking(source, destination):
            if Path(source).name == "register-observation.json":
                raise OSError(5, "simulated parking failure")
            return real_replace(source, destination)

        changed = fixtures.sources_document()
        changed["retrieved"] = "2026-08-22"
        self.sources.write_text(json.dumps(changed), encoding="utf-8")
        with self.assertRaisesRegex(OSError, "simulated parking failure"):
            self.publish(fail_parking)
        self.assertEqual({name: (self.out / name).read_bytes() for name in self.names()}, before)

    def test_a_backup_lost_before_rollback_retains_the_lock(self) -> None:
        contract.publish_pair(self.sources, self.facts, self.out)
        real_replace = os.replace

        def lose_backup(source, destination):
            if Path(destination).name == "register-observation.json" and \
                    Path(source).name.endswith(".tmp"):
                for backup in self.out.glob(".register-observation.json.*.bak"):
                    backup.unlink()
                raise OSError(5, "simulated promotion failure")
            return real_replace(source, destination)

        with self.assertRaisesRegex(contract.ContractError, "rollback failed; retain the lock"):
            self.publish(lose_backup)
        self.assertIn(contract.PUBLISH_LOCK_FILENAME, self.names())


class MainTests(unittest.TestCase):
    def test_a_contract_error_is_a_usage_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            sources = copy.deepcopy(fixtures.sources_document())
            sources["source"] = "Somewhere else"
            (base / "sources.json").write_text(json.dumps(sources), encoding="utf-8")
            (base / "facts.json").write_text(json.dumps(fixtures.observation_facts()),
                                             encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as exit_info:
                contract.main([str(base / "sources.json"), str(base / "facts.json"),
                               "--out", str(base / "out")])
        self.assertEqual(exit_info.exception.code, 2)
        self.assertIn("sources source must be Federal Register of Legislation.", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
