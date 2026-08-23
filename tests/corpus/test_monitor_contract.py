"""Tests for the producer-side AU tax monitor-contract export.

The monitor owns its own strict schemas and evidence digests.  These tests
therefore prove that the corpus projects an independently serialised pair that
the existing monitor can consume unchanged; this repository never imports the
monitor at runtime.
"""

from __future__ import annotations

from contextlib import contextmanager
import json
import importlib
import os
import stat
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock
from pathlib import Path

from fadden import export_monitor_contract as contract


def sources_document() -> dict:
    return {
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": "2026-08-20",
        "source": "Federal Register of Legislation",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "licence": "CC BY 4.0",
        "titles": [
            {
                "register_id": "F2099L00001",
                "name": "Sample Instrument",
                "collection": "LegislativeInstrument",
                "compilation_number": "1",
                "compilation_date": "2099-07-01",
                "version_is_current": True,
                "current_version_start": None,
                "retrieved": "2026-08-20",
                "source_url": "https://example.test/F2099L00001/1",
                "register_page": "https://example.test/F2099L00001/latest/text",
                "words": 42,
            },
            {
                "register_id": "C2099A00001",
                "name": "Sample Act",
                "collection": "Act",
                "compilation_number": "2",
                "compilation_date": "2099-08-01",
                "version_is_current": True,
                "current_version_start": None,
                "retrieved": "2026-08-20",
                "source_url": "https://example.test/C2099A00001/2",
                "register_page": "https://example.test/C2099A00001/latest/text",
                "sections_jsonl": "markdown/C2099A00001/sections.jsonl",
            },
        ],
    }


def observation_facts(*, complete: bool = True) -> dict:
    return {
        "schema_version": "au-tax-register-observation-facts.v1",
        "observed_at": "2026-08-21T10:00:00Z",
        "complete": complete,
        "observations": [
            {
                "register_id": "F2099L00001",
                "collection": "LegislativeInstrument",
                "state": "UNCHANGED",
                "observed_compilation_number": None,
                "observed_compilation_date": None,
                "observed_register_document_id": None,
                "current_version_start": None,
                "evidence_url": "https://example.test/F2099L00001/latest/text",
                "checked_at": "2026-08-21T10:00:01Z",
                "error_category": None,
            },
            {
                "register_id": "C2099A00001",
                "collection": "Act",
                "state": "SUPERSEDED",
                "observed_compilation_number": "3",
                "observed_compilation_date": "2099-09-01",
                "observed_register_document_id": "C2099C00003",
                "current_version_start": None,
                "evidence_url": "https://example.test/C2099A00001/latest/text",
                "checked_at": "2026-08-21T10:00:02Z",
                "error_category": None,
            },
        ],
    }


def observation_facts_v2(*, complete: bool = True) -> dict:
    return {
        "schema_version": "au-tax-register-observation-facts.v2",
        "observed_at": "2026-08-21T10:00:00Z",
        "scope_id": "au-primary-tax-legislation.v2",
        "complete": complete,
        "observations": [
            {
                "register_id": "F2099L00001",
                "collection": "LegislativeInstrument",
                "state": "UNCHANGED",
                "evidence_id": "EV-F2099L00001-01",
                "observed_compilation_number": None,
                "observed_compilation_date": None,
                "observed_register_document_id": None,
                "current_version_start": None,
                "evidence_url": "https://example.test/F2099L00001/latest/text",
                "checked_at": "2026-08-21T10:00:01Z",
                "error_category": None,
            },
            {
                "register_id": "C2099A00001",
                "collection": "Act",
                "state": "SUPERSEDED",
                "evidence_id": "EV-C2099A00001-01",
                "observed_compilation_number": "3",
                "observed_compilation_date": "2099-09-01",
                "observed_register_document_id": "C2099C00003",
                "current_version_start": None,
                "evidence_url": "https://example.test/C2099A00001/latest/text",
                "checked_at": "2026-08-21T10:00:02Z",
                "error_category": None,
            },
        ],
    }


def _local_monitor_repository() -> Path:
    """Return the checkout holding the monitor.

    Before the merge this reached sideways into a separate clone of
    tax-radar-au, so the compatibility test only ran on a machine that
    happened to have one. The monitor now lives in this repository, so the
    answer is the repository root and the test always runs.
    """
    return Path(__file__).resolve().parents[2]


@contextmanager
def _import_optional_monitor():
    """Import the consumer from a fixed checkout or the interpreter environment."""
    package_name = "tax_radar_au"
    module_name = f"{package_name}.monitor"
    fixed_repository = _local_monitor_repository()
    fixed_package = fixed_repository / package_name
    previous_modules = {
        name: module
        for name, module in sys.modules.items()
        if name == package_name or name.startswith(f"{package_name}.")
    }
    previous_path = sys.path.copy()

    for name in previous_modules:
        sys.modules.pop(name, None)

    try:
        if fixed_package.is_dir():
            sys.path.insert(0, str(fixed_repository))

        try:
            monitor = importlib.import_module(module_name)
        except ModuleNotFoundError as error:
            if error.name not in {package_name, module_name}:
                raise
            monitor = None

        yield monitor
    finally:
        sys.path[:] = previous_path
        for name in list(sys.modules):
            if name == package_name or name.startswith(f"{package_name}."):
                sys.modules.pop(name, None)
        sys.modules.update(previous_modules)


class MonitorContractTests(unittest.TestCase):
    def test_optional_monitor_import_falls_back_to_the_existing_interpreter_path(self):
        package_name = "tax_radar_au"
        prior_modules = {
            name: module
            for name, module in sys.modules.items()
            if name == package_name or name.startswith(f"{package_name}.")
        }
        with tempfile.TemporaryDirectory() as temporary:
            import_root = Path(temporary) / "interpreter-path"
            package = import_root / "tax_radar_au"
            package.mkdir(parents=True)
            (package / "__init__.py").write_text("", encoding="utf-8")
            (package / "monitor.py").write_text(
                "ORIGIN = 'interpreter-path'\n",
                encoding="utf-8",
            )
            missing_fixed_checkout = Path(temporary) / "missing-fixed-checkout"
            existing_path = [str(import_root), *sys.path]

            with (
                mock.patch.object(sys, "path", existing_path.copy()),
                mock.patch(
                    f"{__name__}._local_monitor_repository",
                    return_value=missing_fixed_checkout,
                ),
            ):
                before = sys.path.copy()
                with _import_optional_monitor() as monitor:
                    self.assertIsNotNone(monitor)
                    self.assertEqual("interpreter-path", monitor.ORIGIN)
                    self.assertEqual(before, sys.path)

                self.assertEqual(before, sys.path)
                restored_modules = {
                    name: module
                    for name, module in sys.modules.items()
                    if name == package_name or name.startswith(f"{package_name}.")
                }
                self.assertEqual(set(prior_modules), set(restored_modules))
                for name, module in prior_modules.items():
                    self.assertIs(module, restored_modules[name])

    def test_optional_monitor_import_prefers_fixed_checkout_and_restores_import_state(self):
        package_name = "tax_radar_au"
        modules_before_test = {
            name: module
            for name, module in sys.modules.items()
            if name == package_name or name.startswith(f"{package_name}.")
        }
        for name in modules_before_test:
            sys.modules.pop(name, None)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fallback_root = root / "interpreter-path"
            fixed_root = root / "fixed-checkout"
            for package_root, origin in (
                (fallback_root, "interpreter-path"),
                (fixed_root, "fixed-checkout"),
            ):
                package = package_root / "tax_radar_au"
                package.mkdir(parents=True)
                (package / "__init__.py").write_text("", encoding="utf-8")
                (package / "monitor.py").write_text(
                    f"ORIGIN = {origin!r}\n",
                    encoding="utf-8",
                )

            try:
                existing_path = [str(fallback_root), *sys.path]
                with mock.patch.object(sys, "path", existing_path.copy()):
                    prior_package = importlib.import_module(package_name)
                    prior_monitor = importlib.import_module(f"{package_name}.monitor")
                    self.assertEqual("interpreter-path", prior_monitor.ORIGIN)
                    before = sys.path.copy()

                    with mock.patch(
                        f"{__name__}._local_monitor_repository",
                        return_value=fixed_root,
                    ):
                        with _import_optional_monitor() as monitor:
                            self.assertIsNotNone(monitor)
                            self.assertEqual("fixed-checkout", monitor.ORIGIN)
                            self.assertEqual(str(fixed_root), sys.path[0])

                    self.assertEqual(before, sys.path)
                    self.assertIs(prior_package, sys.modules[package_name])
                    self.assertIs(prior_monitor, sys.modules[f"{package_name}.monitor"])
            finally:
                for name in list(sys.modules):
                    if name == package_name or name.startswith(f"{package_name}."):
                        sys.modules.pop(name, None)
                sys.modules.update(modules_before_test)

    def test_build_docs_state_the_adapter_is_review_only_and_never_contacts_the_register(self):
        root = Path(__file__).resolve().parents[2]
        readme = (root / "README.md").read_text(encoding="utf-8")
        build = (root / "BUILD.md").read_text(encoding="utf-8")

        self.assertIn("export_monitor_contract.py", readme)
        self.assertIn("au-tax-register-observation-facts.v1", readme)
        self.assertIn("does not call the Register", readme)
        self.assertIn("export_monitor_contract.py", build)
        self.assertIn("monitor-baseline.json", build)
        self.assertIn("register-observation.json", build)
        self.assertIn("replaced individually", readme)
        self.assertIn("one writer", readme)
        self.assertIn("power loss", readme)
        self.assertIn("operator", readme)
        self.assertIn("replaced individually", build)
        self.assertIn("one writer", build)
        self.assertIn("power loss", build)
        self.assertIn("operator", build)
        self.assertIn("rollback itself fails", readme)
        self.assertIn("rollback itself fails", build)
        self.assertIn(".bak", readme)
        self.assertIn(".bak", build)
        self.assertIn("restore or", readme)
        self.assertIn("restore or", build)
        self.assertIn("deliberately retire", readme)
        self.assertIn("deliberately retire", build)

    def test_projects_rich_sources_to_the_monitor_exact_baseline_shape(self):
        baseline = contract.project_baseline(sources_document())

        self.assertEqual(
            set(baseline),
            {"corpus", "retrieved", "source", "source_api", "titles"},
        )
        self.assertEqual([item["register_id"] for item in baseline["titles"]], [
            "C2099A00001",
            "F2099L00001",
        ])
        self.assertEqual(
            set(baseline["titles"][0]),
            {
                "register_id", "name", "collection", "compilation_number",
                "compilation_date", "version_is_current", "current_version_start",
                "retrieved", "source_url", "register_page",
            },
        )

    def test_builds_the_exact_observation_v1_contract_with_sorted_scope(self):
        baseline = contract.project_baseline(sources_document())
        observation = contract.project_observation(baseline, observation_facts())

        self.assertEqual(
            observation["schema_version"], "au-tax-register-observation.v1"
        )
        self.assertEqual(observation["mode"], "synthetic")
        self.assertEqual(
            observation["expected_register_ids"], ["C2099A00001", "F2099L00001"]
        )
        self.assertEqual(
            [item["register_id"] for item in observation["observations"]],
            ["C2099A00001", "F2099L00001"],
        )

    def test_refuses_a_complete_observation_with_a_missing_baseline_title(self):
        facts = observation_facts()
        facts["observations"].pop()

        with self.assertRaisesRegex(contract.ContractError, "complete.*every baseline"):
            contract.project_observation(contract.project_baseline(sources_document()), facts)

    def test_refuses_a_state_specific_field_that_would_make_evidence_ambiguous(self):
        facts = observation_facts()
        facts["observations"][0]["current_version_start"] = "2099-08-20"

        with self.assertRaisesRegex(contract.ContractError, "UNCHANGED.*current_version_start"):
            contract.project_observation(contract.project_baseline(sources_document()), facts)

    def test_refuses_non_utc_timestamps_bad_https_and_duplicate_titles(self):
        baseline_source = sources_document()
        baseline_source["titles"].append(dict(baseline_source["titles"][0]))
        with self.assertRaisesRegex(contract.ContractError, "duplicate register_id"):
            contract.project_baseline(baseline_source)

        facts = observation_facts()
        facts["observed_at"] = "2026-08-21T10:00:00+10:00"
        with self.assertRaisesRegex(contract.ContractError, "UTC"):
            contract.project_observation(contract.project_baseline(sources_document()), facts)

        facts = observation_facts()
        facts["observations"][0]["evidence_url"] = "http://example.test/not-secure"
        with self.assertRaisesRegex(contract.ContractError, "https"):
            contract.project_observation(contract.project_baseline(sources_document()), facts)

    def test_refuses_duplicate_observations_and_collection_mismatches(self):
        facts = observation_facts()
        facts["observations"].append(dict(facts["observations"][0]))
        with self.assertRaisesRegex(contract.ContractError, "duplicate register_id"):
            contract.project_observation(contract.project_baseline(sources_document()), facts)

        facts = observation_facts()
        facts["observations"][0]["collection"] = "Act"
        with self.assertRaisesRegex(contract.ContractError, "does not match baseline"):
            contract.project_observation(contract.project_baseline(sources_document()), facts)

    def test_state_specific_fields_cover_no_document_and_lookup_failure(self):
        facts = observation_facts()
        facts["observations"][0].update(
            state="CURRENT_NO_PUBLISHED_COMPILATION",
            current_version_start="2099-08-20",
        )
        facts["observations"][1].update(
            state="LOOKUP_FAILED",
            observed_compilation_number=None,
            observed_compilation_date=None,
            observed_register_document_id=None,
            error_category="register_unavailable",
        )

        projected = contract.project_observation(
            contract.project_baseline(sources_document()), facts
        )

        self.assertEqual(
            [item["state"] for item in projected["observations"]],
            ["LOOKUP_FAILED", "CURRENT_NO_PUBLISHED_COMPILATION"],
        )

    def test_main_publishes_the_named_pair(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")

            self.assertEqual(
                contract.main([str(sources), str(facts), "--out", str(root / "out")]),
                0,
            )
            self.assertTrue((root / "out" / "monitor-baseline.json").is_file())
            self.assertTrue((root / "out" / "register-observation.json").is_file())

    def test_preflight_refuses_every_input_output_collision_without_changing_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "out"
            output.mkdir()
            for input_name in ("sources", "facts"):
                for output_name in (
                    "monitor-baseline.json",
                    "register-observation.json",
                ):
                    with self.subTest(input_name=input_name, output_name=output_name):
                        collision = output / output_name
                        original = b'{"must":"remain byte-for-byte unchanged"}\n'
                        collision.write_bytes(original)
                        sources = root / "sources.json"
                        facts = root / "facts.json"
                        sources.write_text(json.dumps(sources_document()), encoding="utf-8")
                        facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
                        if input_name == "sources":
                            sources = collision
                        else:
                            facts = collision

                        with self.assertRaisesRegex(contract.ContractError, "would replace an input"):
                            contract.publish_pair(sources, facts, output)

                        self.assertEqual(original, collision.read_bytes())
                        collision.unlink()

    def test_preflight_refuses_an_existing_output_directory_without_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "out"
            destination = output / "monitor-baseline.json"
            output.mkdir()
            destination.mkdir()
            marker = destination / "must-remain"
            marker.write_bytes(b"directory contents must remain")
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")

            with self.assertRaisesRegex(contract.ContractError, "regular file"):
                contract.publish_pair(sources, facts, output)

            self.assertTrue(destination.is_dir())
            self.assertEqual(b"directory contents must remain", marker.read_bytes())
            self.assertFalse((output / "register-observation.json").exists())
            self.assertFalse(list(output.glob(".*.monitor-contract-*")))

    def test_preflight_refuses_an_existing_output_symlink_without_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "out"
            output.mkdir()
            target = root / "outside.json"
            original = b"outside bytes must remain"
            target.write_bytes(original)
            destination = output / "register-observation.json"
            try:
                destination.symlink_to(target)
            except OSError:
                self.skipTest("symbolic links are unavailable on this platform")
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")

            with self.assertRaisesRegex(contract.ContractError, "regular file"):
                contract.publish_pair(sources, facts, output)

            self.assertTrue(destination.is_symlink())
            self.assertEqual(original, target.read_bytes())
            self.assertFalse((output / "monitor-baseline.json").exists())
            self.assertFalse(list(output.glob(".*.monitor-contract-*")))

    def test_preflight_refuses_an_existing_output_junction_without_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "out"
            output.mkdir()
            destination = output / "monitor-baseline.json"
            original = b"existing bytes must remain"
            destination.write_bytes(original)
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            real_isjunction = getattr(os.path, "isjunction", lambda _path: False)

            def classify(candidate):
                return os.path.normcase(os.fspath(candidate)) == os.path.normcase(os.fspath(destination)) or real_isjunction(candidate)

            with mock.patch.object(contract.os.path, "isjunction", side_effect=classify, create=True):
                with self.assertRaisesRegex(contract.ContractError, "regular file"):
                    contract.publish_pair(sources, facts, output)

            self.assertEqual(original, destination.read_bytes())
            self.assertFalse((output / "register-observation.json").exists())
            self.assertFalse(list(output.glob(".*.monitor-contract-*")))

    def test_preflight_refuses_an_existing_output_reparse_point_without_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "out"
            output.mkdir()
            destination = output / "monitor-baseline.json"
            original = b"existing bytes must remain"
            destination.write_bytes(original)
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            real_lstat = contract.os.lstat

            def marked_reparse(candidate):
                if os.path.normcase(os.fspath(candidate)) == os.path.normcase(os.fspath(destination)):
                    return mock.Mock(
                        st_mode=stat.S_IFREG,
                        st_file_attributes=getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400),
                    )
                return real_lstat(candidate)

            with mock.patch.object(contract.os, "lstat", side_effect=marked_reparse):
                with self.assertRaisesRegex(contract.ContractError, "regular file"):
                    contract.publish_pair(sources, facts, output)

            self.assertEqual(original, destination.read_bytes())
            self.assertFalse((output / "register-observation.json").exists())
            self.assertFalse(list(output.glob(".*.monitor-contract-*")))

    def test_rejects_duplicate_json_members_at_top_and_nested_levels(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            sources.write_text(
                json.dumps(sources_document()).replace(
                    '"corpus": "Commonwealth tax statutes and legislative instruments"',
                    '"corpus": "first", "corpus": "second"',
                    1,
                ),
                encoding="utf-8",
            )
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            with self.assertRaisesRegex(contract.ContractError, "duplicate JSON members"):
                contract.publish_pair(sources, facts, root / "out")

            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(
                json.dumps(observation_facts()).replace(
                    '"register_id": "F2099L00001"',
                    '"register_id": "F2099L00001", "register_id": "F2099L00001"',
                    1,
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(contract.ContractError, "duplicate JSON members"):
                contract.publish_pair(sources, facts, root / "out")

    def test_rejects_monitor_control_characters_in_text_and_urls(self):
        source_cases = (
            ("corpus", lambda document: document.__setitem__("corpus", "corpus" + chr(1))),
            ("source", lambda document: document.__setitem__("source", "Federal Register" + chr(127))),
            ("source_api", lambda document: document.__setitem__("source_api", "https://example.test/a" + chr(31) + "b")),
            ("title name", lambda document: document["titles"][0].__setitem__("name", "name" + chr(1))),
            ("title URL", lambda document: document["titles"][0].__setitem__("source_url", "https://example.test/" + chr(127))),
        )
        for label, mutate in source_cases:
            with self.subTest(label=label):
                document = sources_document()
                mutate(document)
                with self.assertRaisesRegex(contract.ContractError, "control characters"):
                    contract.project_baseline(document)

        fact_cases = (
            ("schema version", lambda facts: facts.__setitem__("schema_version", "au-tax-register-observation-facts.v1" + chr(1))),
            ("state", lambda facts: facts["observations"][0].__setitem__("state", "UNCHANGED" + chr(1))),
            ("evidence URL", lambda facts: facts["observations"][0].__setitem__("evidence_url", "https://example.test/" + chr(127))),
            ("compilation number", lambda facts: facts["observations"][1].__setitem__("observed_compilation_number", "3" + chr(1))),
            ("document id", lambda facts: facts["observations"][1].__setitem__("observed_register_document_id", "C2099C00003" + chr(127))),
        )
        baseline = contract.project_baseline(sources_document())
        for label, mutate in fact_cases:
            with self.subTest(label=label):
                facts = observation_facts()
                mutate(facts)
                with self.assertRaisesRegex(contract.ContractError, "control characters"):
                    contract.project_observation(baseline, facts)

    def test_staging_failure_removes_a_previous_temp_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            original_writer = contract._write_staged
            calls = 0

            def fail_second_staging(path, value):
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("simulated second staging failure")
                return original_writer(path, value)

            with mock.patch.object(contract, "_write_staged", side_effect=fail_second_staging):
                with self.assertRaisesRegex(OSError, "simulated second staging failure"):
                    contract.publish_pair(sources, facts, root / "out")

            self.assertFalse(list((root / "out").glob(".*.monitor-contract-*.tmp")))

    def test_live_and_aged_output_locks_fail_closed_without_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "out"
            output.mkdir()
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            lock = output / contract.PUBLISH_LOCK_FILENAME
            lock.write_text("live writer", encoding="utf-8")
            with mock.patch.object(contract, "PUBLISH_LOCK_TIMEOUT_SECONDS", 0):
                with self.assertRaisesRegex(contract.ContractError, "locked by another writer"):
                    contract.publish_pair(sources, facts, output)
            self.assertTrue(lock.exists())

            stale = time.time() - 600
            os.utime(lock, (stale, stale))
            with mock.patch.object(contract, "PUBLISH_LOCK_TIMEOUT_SECONDS", 0):
                with self.assertRaisesRegex(contract.ContractError, "locked by another writer"):
                    contract.publish_pair(sources, facts, output)
            self.assertEqual("live writer", lock.read_text(encoding="utf-8"))
            self.assertFalse((output / "monitor-baseline.json").exists())
            self.assertFalse((output / "register-observation.json").exists())

    def test_competing_writers_leave_an_aged_lock_untouched(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "out"
            output.mkdir()
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            lock = output / contract.PUBLISH_LOCK_FILENAME
            lock.write_text("failed writer requires operator recovery", encoding="utf-8")
            stale = time.time() - 600
            os.utime(lock, (stale, stale))
            barrier = threading.Barrier(2)
            errors: list[BaseException] = []

            def publish() -> None:
                try:
                    barrier.wait(5)
                    contract.publish_pair(sources, facts, output)
                except BaseException as exc:
                    errors.append(exc)

            with mock.patch.object(contract, "PUBLISH_LOCK_TIMEOUT_SECONDS", 0):
                writers = [threading.Thread(target=publish) for _ in range(2)]
                for writer in writers:
                    writer.start()
                for writer in writers:
                    writer.join(5)

            self.assertTrue(all(not writer.is_alive() for writer in writers))
            self.assertEqual(2, len(errors))
            self.assertTrue(all(isinstance(exc, contract.ContractError) for exc in errors))
            self.assertTrue(all("locked by another writer" in str(exc) for exc in errors))
            self.assertEqual("failed writer requires operator recovery", lock.read_text(encoding="utf-8"))
            self.assertFalse((output / "monitor-baseline.json").exists())
            self.assertFalse((output / "register-observation.json").exists())

    def test_concurrent_writers_cannot_interleave_a_published_pair(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "out"
            sources_a = root / "sources-a.json"
            sources_b = root / "sources-b.json"
            facts_a = root / "facts-a.json"
            facts_b = root / "facts-b.json"
            source_a = sources_document()
            source_b = sources_document()
            source_a["corpus"] = "writer A"
            source_b["corpus"] = "writer B"
            observation_a = observation_facts()
            observation_b = observation_facts()
            observation_a["observed_at"] = "2026-08-21T10:01:00Z"
            observation_b["observed_at"] = "2026-08-21T10:02:00Z"
            for path, value in (
                (sources_a, source_a), (sources_b, source_b),
                (facts_a, observation_a), (facts_b, observation_b),
            ):
                path.write_text(json.dumps(value), encoding="utf-8")

            baseline_promoted = threading.Event()
            allow_first_writer = threading.Event()
            second_reached_publisher = threading.Event()
            errors: list[BaseException] = []
            original_replace = contract.os.replace

            def pause_first_baseline(source, destination):
                result = original_replace(source, destination)
                if (
                    threading.current_thread().name == "writer-a"
                    and Path(source).name.endswith(".tmp")
                    and Path(destination) == output / "monitor-baseline.json"
                ):
                    baseline_promoted.set()
                    allow_first_writer.wait(5)
                if threading.current_thread().name == "writer-b":
                    second_reached_publisher.set()
                return result

            def write_pair(sources, facts):
                try:
                    contract.publish_pair(sources, facts, output)
                except BaseException as exc:  # Test must re-raise after both threads finish.
                    errors.append(exc)

            with mock.patch.object(contract.os, "replace", side_effect=pause_first_baseline):
                first = threading.Thread(target=write_pair, args=(sources_a, facts_a), name="writer-a")
                second = threading.Thread(target=write_pair, args=(sources_b, facts_b), name="writer-b")
                first.start()
                self.assertTrue(baseline_promoted.wait(5))
                second.start()
                try:
                    self.assertFalse(second_reached_publisher.wait(0.25))
                finally:
                    allow_first_writer.set()
                first.join(5)
                second.join(5)

            self.assertFalse(first.is_alive())
            self.assertFalse(second.is_alive())
            self.assertEqual([], errors)
            self.assertEqual("writer B", json.loads((output / "monitor-baseline.json").read_text(encoding="utf-8"))["corpus"])
            self.assertEqual("2026-08-21T10:02:00Z", json.loads((output / "register-observation.json").read_text(encoding="utf-8"))["observed_at"])

    def test_second_writer_cannot_enter_while_first_writer_rolls_back(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "out"
            initial_sources = root / "initial-sources.json"
            initial_facts = root / "initial-facts.json"
            sources_a = root / "sources-a.json"
            sources_b = root / "sources-b.json"
            facts_a = root / "facts-a.json"
            facts_b = root / "facts-b.json"
            for path, value in (
                (initial_sources, sources_document()), (initial_facts, observation_facts()),
                (sources_a, sources_document()), (facts_a, observation_facts()),
                (sources_b, sources_document()), (facts_b, observation_facts()),
            ):
                path.write_text(json.dumps(value), encoding="utf-8")
            contract.publish_pair(initial_sources, initial_facts, output)

            second_promotion_ready = threading.Event()
            allow_promotion_failure = threading.Event()
            rollback_started = threading.Event()
            allow_rollback = threading.Event()
            second_reached_publisher = threading.Event()
            errors: list[BaseException] = []
            original_replace = contract.os.replace
            original_remove = contract._remove

            def fail_second_promotion(source, destination):
                if (
                    threading.current_thread().name == "writer-a"
                    and Path(source).name.endswith(".tmp")
                    and Path(destination) == output / "register-observation.json"
                ):
                    second_promotion_ready.set()
                    allow_promotion_failure.wait(5)
                    raise OSError("simulated second promotion failure")
                if threading.current_thread().name == "writer-b":
                    second_reached_publisher.set()
                return original_replace(source, destination)

            def pause_rollback(path):
                if (
                    threading.current_thread().name == "writer-a"
                    and path == output / "monitor-baseline.json"
                ):
                    rollback_started.set()
                    allow_rollback.wait(5)
                return original_remove(path)

            def write_pair(sources, facts):
                try:
                    contract.publish_pair(sources, facts, output)
                except BaseException as exc:
                    errors.append(exc)

            with (
                mock.patch.object(contract.os, "replace", side_effect=fail_second_promotion),
                mock.patch.object(contract, "_remove", side_effect=pause_rollback),
            ):
                first = threading.Thread(target=write_pair, args=(sources_a, facts_a), name="writer-a")
                second = threading.Thread(target=write_pair, args=(sources_b, facts_b), name="writer-b")
                first.start()
                self.assertTrue(second_promotion_ready.wait(5))
                allow_promotion_failure.set()
                self.assertTrue(rollback_started.wait(5))
                second.start()
                try:
                    self.assertFalse(second_reached_publisher.wait(0.25))
                finally:
                    allow_rollback.set()
                first.join(5)
                second.join(5)

            self.assertFalse(first.is_alive())
            self.assertFalse(second.is_alive())
            self.assertEqual(1, len(errors))
            self.assertIsInstance(errors[0], OSError)

    def test_rollback_failure_retains_prior_bytes_and_blocks_publishers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "out"
            sources = root / "sources.json"
            facts = root / "facts.json"
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            paths = contract.publish_pair(sources, facts, output)
            prior = {name: path.read_bytes() for name, path in paths.items()}

            changed_sources = sources_document()
            changed_sources["corpus"] = "attempted replacement"
            sources.write_text(json.dumps(changed_sources), encoding="utf-8")
            original_replace = contract.os.replace

            def fail_promotion_and_every_restore(source, destination):
                source_path = Path(source)
                if (
                    source_path.name.endswith(".tmp")
                    and Path(destination) == output / "register-observation.json"
                ):
                    raise OSError("simulated observation promotion failure")
                if source_path.name.endswith(".bak"):
                    raise OSError("simulated rollback restoration failure")
                return original_replace(source, destination)

            with mock.patch.object(
                contract.os,
                "replace",
                side_effect=fail_promotion_and_every_restore,
            ):
                with self.assertRaisesRegex(contract.ContractError, "rollback failed"):
                    contract.publish_pair(sources, facts, output)

            backups = list(output.glob(".*.monitor-contract-*.bak"))
            self.assertEqual(2, len(backups))
            self.assertEqual(
                {prior["baseline"], prior["observation"]},
                {path.read_bytes() for path in backups},
            )
            self.assertTrue((output / contract.PUBLISH_LOCK_FILENAME).exists())
            with mock.patch.object(contract, "PUBLISH_LOCK_TIMEOUT_SECONDS", 0):
                with self.assertRaisesRegex(contract.ContractError, "locked by another writer"):
                    contract.publish_pair(sources, facts, output)
            self.assertEqual(2, len(list(output.glob(".*.monitor-contract-*.bak"))))

    def test_generated_pair_is_accepted_by_a_local_monitor_when_available(self):
        with _import_optional_monitor() as monitor:
            if monitor is None:
                self.skipTest(
                    "tax-radar-au is absent from the fixed checkout "
                    "and interpreter import path"
                )
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                sources = root / "sources.json"
                facts = root / "facts.json"
                mapping = root / "map.json"
                sources.write_text(json.dumps(sources_document()), encoding="utf-8")
                facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
                mapping.write_text(
                    json.dumps(
                        {
                            "schema_version": "au-tax-source-skill-map.v1",
                            "mapping_version": "test.1",
                            "entries": [],
                        }
                    ),
                    encoding="utf-8",
                )
                paths = contract.publish_pair(sources, facts, root / "out")
                queue = monitor.compare(
                    baseline_path=paths["baseline"],
                    observation_path=paths["observation"],
                    mapping_path=mapping,
                )

        self.assertEqual(queue["run_status"], "REVIEW_REQUIRED")
        self.assertEqual(queue["items"][0]["change_kind"], "SUPERSEDED")

    def test_serialises_deterministically_and_restores_both_previous_files_on_publish_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            output = root / "out"
            first = contract.publish_pair(sources, facts, output)
            first_bytes = {name: path.read_bytes() for name, path in first.items()}
            second = contract.publish_pair(sources, facts, output)
            self.assertEqual(first_bytes, {name: path.read_bytes() for name, path in second.items()})

            original_replace = contract.os.replace
            calls = 0

            def fail_second_promotion(source, destination):
                nonlocal calls
                calls += 1
                if calls == 4:
                    raise OSError("simulated second promotion failure")
                return original_replace(source, destination)

            contract.os.replace = fail_second_promotion
            try:
                with self.assertRaisesRegex(OSError, "simulated second promotion failure"):
                    contract.publish_pair(sources, facts, output)
            finally:
                contract.os.replace = original_replace

            self.assertEqual(first_bytes, {name: path.read_bytes() for name, path in first.items()})
            self.assertFalse(list(output.glob(".*.monitor-contract-*")))

    def test_project_observation_v2_nominal(self):
        projected = contract.project_observation(sources_document(), observation_facts_v2())
        self.assertEqual(projected["schema_version"], "au-tax-register-observation.v2")
        self.assertEqual(projected["scope_id"], "au-primary-tax-legislation.v2")
        self.assertTrue(projected["complete"])
        self.assertEqual(len(projected["observations"]), 2)
        self.assertEqual(projected["observations"][0]["evidence_id"], "EV-C2099A00001-01")
        self.assertEqual(projected["observations"][1]["evidence_id"], "EV-F2099L00001-01")

    def test_project_observation_v2_requires_scope_id(self):
        facts = observation_facts_v2()
        del facts["scope_id"]
        with self.assertRaisesRegex(contract.ContractError, "invalid shape"):
            contract.project_observation(sources_document(), facts)

        facts = observation_facts_v2()
        facts["scope_id"] = "   "
        with self.assertRaisesRegex(contract.ContractError, "scope_id must be a non-empty string"):
            contract.project_observation(sources_document(), facts)

    def test_project_observation_v2_rejects_duplicate_evidence_id(self):
        facts = observation_facts_v2()
        facts["observations"][1]["evidence_id"] = facts["observations"][0]["evidence_id"]
        with self.assertRaisesRegex(contract.ContractError, "duplicate evidence_id"):
            contract.project_observation(sources_document(), facts)

    def test_project_observation_v2_requires_evidence_id_per_item(self):
        facts = observation_facts_v2()
        del facts["observations"][0]["evidence_id"]
        with self.assertRaisesRegex(contract.ContractError, "invalid shape"):
            contract.project_observation(sources_document(), facts)


if __name__ == "__main__":
    unittest.main()
