"""Contract tests for immutable publisher evidence bundles."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from fadden import export_monitor_contract as monitor_contract
from fadden import export_publication_bundles as exporter


FIXTURES = Path(__file__).parent / "fixtures" / "publication"
SOURCES = FIXTURES / "sample-sources.json"
FACTS = FIXTURES / "sample-observation-facts-v3.json"
PAYLOAD = FIXTURES / "sample-evidence-payload.json"


class PublicationBundleExportTests(unittest.TestCase):
    def _project_inputs(self) -> tuple[dict, dict, bytes, bytes]:
        source_bytes = SOURCES.read_bytes()
        facts_bytes = FACTS.read_bytes()
        sources = json.loads(source_bytes)
        facts = json.loads(facts_bytes)
        baseline = monitor_contract.project_baseline(sources)
        observation = monitor_contract.project_observation(sources, facts)
        return baseline, observation, source_bytes, facts_bytes

    def _build(self, baseline: dict, observation: dict) -> list[dict]:
        source_bytes = SOURCES.read_bytes()
        facts_bytes = FACTS.read_bytes()
        return exporter.build_publication_bundles(
            baseline,
            observation,
            baseline_sha256=hashlib.sha256(source_bytes).hexdigest(),
            observation_facts_sha256=hashlib.sha256(facts_bytes).hexdigest(),
            producer_version="0.1.3",
        )

    def _build_one_bundle(self) -> dict:
        baseline, observation, source_bytes, facts_bytes = self._project_inputs()
        bundles = exporter.build_publication_bundles(
            baseline,
            observation,
            baseline_sha256=hashlib.sha256(source_bytes).hexdigest(),
            observation_facts_sha256=hashlib.sha256(facts_bytes).hexdigest(),
            producer_version="0.1.3",
        )
        self.assertEqual(len(bundles), 1)
        return bundles[0]

    def test_artificial_payload_has_the_content_digest_declared_by_the_observation(self):
        digest = "sha256:" + hashlib.sha256(PAYLOAD.read_bytes()).hexdigest()
        facts = json.loads(FACTS.read_text(encoding="utf-8"))
        self.assertEqual(
            digest,
            "sha256:cace4b38f998a09ea28ebe1448e0756da72ee05d750e27f8278c15a66d0b5dc3",
        )
        self.assertEqual(facts["observations"][0]["content_sha256"], digest)

    def test_builds_one_source_only_bundle_from_a_supported_observation(self):
        bundle = self._build_one_bundle()
        self.assertEqual(bundle["schema_version"], "evidence-bundle.v1")
        self.assertEqual(bundle["bundle_id"], "bundle-frl-c2099a00001-c2099c00002-r1")
        self.assertEqual(bundle["development_id"], "dev-frl-c2099a00001-c2099c00002")
        self.assertEqual(bundle["mode"], "synthetic")
        self.assertEqual(bundle["development"]["authority_status"], "in-force")
        self.assertEqual(bundle["development"]["published_at"], "2026-08-05T00:00:00Z")
        self.assertEqual(bundle["development"]["topics"], [])
        self.assertEqual(bundle["development"]["affected_practice_areas"], [])
        self.assertEqual(bundle["source_event"]["kind"], "compilation-superseded")
        self.assertEqual(
            bundle["sources"][0]["content_sha256"],
            "sha256:cace4b38f998a09ea28ebe1448e0756da72ee05d750e27f8278c15a66d0b5dc3",
        )
        self.assertEqual(bundle["sources"][0]["rights"]["mode"], "metadata-only")
        self.assertEqual(bundle["sources"][0]["evidence"], [])

    def test_generated_bundle_matches_the_reviewed_golden_bytes(self):
        generated = exporter.bundle_bytes(self._build_one_bundle())
        expected = (FIXTURES / "evidence-bundle.v1.json").read_bytes()
        self.assertEqual(generated, expected)

    def test_refuses_incomplete_or_non_v3_observations(self):
        baseline, observation, _, _ = self._project_inputs()
        for field, value in (
            ("complete", False),
            ("schema_version", "au-tax-register-observation.v2"),
        ):
            with self.subTest(field=field):
                invalid = copy.deepcopy(observation)
                invalid[field] = value
                with self.assertRaises(exporter.PublicationBundleError):
                    self._build(baseline, invalid)

    def test_refuses_every_unsupported_changed_state(self):
        baseline, observation, _, _ = self._project_inputs()
        for state in (
            "LOOKUP_FAILED",
            "CURRENT_NO_PUBLISHED_COMPILATION",
            "NO_LONGER_IN_FORCE",
        ):
            with self.subTest(state=state):
                invalid = copy.deepcopy(observation)
                invalid["observations"][0]["state"] = state
                with self.assertRaises(exporter.PublicationBundleError):
                    self._build(baseline, invalid)

    def test_refuses_mismatched_collection_or_missing_content_evidence(self):
        baseline, observation, _, _ = self._project_inputs()

        mismatched = copy.deepcopy(observation)
        mismatched["observations"][0]["collection"] = "LegislativeInstrument"
        with self.assertRaises(exporter.PublicationBundleError):
            self._build(baseline, mismatched)

        for field in ("content_sha256", "content_kind", "content_media_type"):
            with self.subTest(field=field):
                missing = copy.deepcopy(observation)
                del missing["observations"][0][field]
                with self.assertRaises(exporter.PublicationBundleError):
                    self._build(baseline, missing)

    def test_refuses_an_observation_that_is_not_a_newer_compilation(self):
        baseline, observation, _, _ = self._project_inputs()

        same_number = copy.deepcopy(observation)
        same_number["observations"][0]["observed_compilation_number"] = "1"
        with self.assertRaises(exporter.PublicationBundleError):
            self._build(baseline, same_number)

        same_date = copy.deepcopy(observation)
        same_date["observations"][0]["observed_compilation_date"] = "2026-07-01"
        with self.assertRaises(exporter.PublicationBundleError):
            self._build(baseline, same_date)

    def test_refuses_values_outside_the_publisher_contract(self):
        baseline, observation, _, _ = self._project_inputs()
        cases = (
            (
                "title length",
                lambda candidate_baseline, _candidate_observation: candidate_baseline[
                    "titles"
                ][0].__setitem__("name", "x" * 201),
            ),
            (
                "title XML text",
                lambda candidate_baseline, _candidate_observation: candidate_baseline[
                    "titles"
                ][0].__setitem__("name", "Invalid \ufffe title"),
            ),
            (
                "previous compilation number",
                lambda candidate_baseline, _candidate_observation: candidate_baseline[
                    "titles"
                ][0].__setitem__("compilation_number", "1" * 81),
            ),
            (
                "current compilation number",
                lambda _candidate_baseline, candidate_observation: candidate_observation[
                    "observations"
                ][0].__setitem__("observed_compilation_number", "2" * 81),
            ),
            (
                "canonical URL length",
                lambda _candidate_baseline, candidate_observation: candidate_observation[
                    "observations"
                ][0].__setitem__(
                    "evidence_url", "https://example.invalid/" + "x" * 2025
                ),
            ),
            (
                "canonical URL Unicode",
                lambda _candidate_baseline, candidate_observation: candidate_observation[
                    "observations"
                ][0].__setitem__("evidence_url", "https://example.invalid/\ud800"),
            ),
            (
                "canonical URL identity",
                lambda _candidate_baseline, candidate_observation: candidate_observation[
                    "observations"
                ][0].__setitem__(
                    "evidence_url",
                    "https://example.invalid/C2099A99999/latest/text",
                ),
            ),
            (
                "content kind",
                lambda _candidate_baseline, candidate_observation: candidate_observation[
                    "observations"
                ][0].__setitem__("content_kind", "summary"),
            ),
        )
        for name, mutate in cases:
            with self.subTest(name=name):
                candidate_baseline = copy.deepcopy(baseline)
                candidate_observation = copy.deepcopy(observation)
                mutate(candidate_baseline, candidate_observation)
                with self.assertRaises(exporter.PublicationBundleError):
                    self._build(candidate_baseline, candidate_observation)

    def test_refuses_generated_identifiers_that_exceed_the_publisher_contract(self):
        baseline, observation, _, _ = self._project_inputs()
        register_id = "A" * 40
        document_id = "C" * 40
        baseline["titles"][0]["register_id"] = register_id
        observation["expected_register_ids"] = [register_id]
        observation["observations"][0]["register_id"] = register_id
        observation["observations"][0]["observed_register_document_id"] = document_id

        with self.assertRaisesRegex(
            exporter.PublicationBundleError, "publisher identifier"
        ):
            self._build(baseline, observation)

    def test_refuses_a_duplicate_output_identity(self):
        baseline, observation, _, _ = self._project_inputs()
        observation["observations"].append(copy.deepcopy(observation["observations"][0]))
        with self.assertRaises(exporter.PublicationBundleError):
            self._build(baseline, observation)

    def test_reversing_supported_observations_keeps_sorted_bundle_bytes(self):
        baseline, observation, _, _ = self._project_inputs()
        second_title = copy.deepcopy(baseline["titles"][0])
        second_title["register_id"] = "C2099A00002"
        second_title["name"] = "Sample Income Tax Act 2099"
        baseline["titles"].append(second_title)

        second_observation = copy.deepcopy(observation["observations"][0])
        second_observation["register_id"] = "C2099A00002"
        second_observation["observed_register_document_id"] = "C2099C00003"
        second_observation["evidence_id"] = "ev-c2099a00002-01"
        second_observation["evidence_url"] = (
            "https://example.invalid/C2099A00002/latest/text"
        )
        observation["expected_register_ids"].append("C2099A00002")
        observation["observations"].append(second_observation)

        forward = [exporter.bundle_bytes(bundle) for bundle in self._build(baseline, observation)]
        observation["observations"].reverse()
        reverse = [exporter.bundle_bytes(bundle) for bundle in self._build(baseline, observation)]
        self.assertEqual(forward, reverse)

    def test_exports_all_files_as_one_new_immutable_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "bundles"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(FACTS.read_bytes())

            written = exporter.export_publication_bundles(sources, facts, output)

            expected = output / "bundle-frl-c2099a00001-c2099c00002-r1.json"
            self.assertEqual(written, [expected])
            self.assertEqual(expected.read_bytes(), exporter.bundle_bytes(self._build_one_bundle()))
            self.assertEqual(list(root.glob(f".{output.name}.publication-bundles-*.tmp")), [])

    def test_creates_one_missing_output_parent_after_validating_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output_parent = root / "build"
            output = output_parent / "bundles"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(FACTS.read_bytes())

            written = exporter.export_publication_bundles(sources, facts, output)

            self.assertEqual(len(written), 1)
            self.assertTrue(written[0].is_file())
            self.assertTrue(output_parent.is_dir())

    def test_invalid_input_does_not_create_a_missing_output_parent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output_parent = root / "build"
            output = output_parent / "bundles"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(b'{"schema_version":"one","schema_version":"two"}\n')

            with self.assertRaises(exporter.PublicationBundleError):
                exporter.export_publication_bundles(sources, facts, output)

            self.assertFalse(output_parent.exists())

    def test_refuses_existing_destinations_without_mutating_them(self):
        for populated in (False, True):
            with self.subTest(populated=populated), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                sources = root / "sources.json"
                facts = root / "facts.json"
                output = root / "bundles"
                sources.write_bytes(SOURCES.read_bytes())
                facts.write_bytes(FACTS.read_bytes())
                output.mkdir()
                marker = output / "keep.txt"
                if populated:
                    marker.write_bytes(b"keep")

                with self.assertRaises(exporter.PublicationBundleError):
                    exporter.export_publication_bundles(sources, facts, output)

                self.assertEqual(
                    marker.read_bytes() if populated else None, b"keep" if populated else None
                )
                self.assertEqual(list(output.iterdir()), [marker] if populated else [])

    def test_refuses_an_input_alias_or_non_regular_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            sources_bytes = SOURCES.read_bytes()
            sources.write_bytes(sources_bytes)
            facts.write_bytes(FACTS.read_bytes())

            with self.assertRaises(exporter.PublicationBundleError):
                exporter.export_publication_bundles(sources, facts, sources)
            self.assertEqual(sources.read_bytes(), sources_bytes)

            directory_input = root / "directory-input"
            directory_input.mkdir()
            with self.assertRaises(exporter.PublicationBundleError):
                exporter.export_publication_bundles(directory_input, facts, root / "bundles")
            self.assertFalse((root / "bundles").exists())

    def test_refuses_duplicate_json_members_before_creating_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "bundles"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(b'{"schema_version":"one","schema_version":"two"}\n')

            with self.assertRaisesRegex(exporter.PublicationBundleError, "duplicate JSON members"):
                exporter.export_publication_bundles(sources, facts, output)
            self.assertFalse(output.exists())

    def test_staged_write_failure_removes_only_exporter_owned_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "bundles"
            marker = root / "unrelated.txt"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(FACTS.read_bytes())
            marker.write_bytes(b"keep")

            with mock.patch.object(
                exporter, "_write_bundle", side_effect=OSError("injected write failure")
            ):
                with self.assertRaisesRegex(
                    exporter.PublicationBundleError, "could not be written"
                ):
                    exporter.export_publication_bundles(sources, facts, output)

            self.assertFalse(output.exists())
            self.assertEqual(marker.read_bytes(), b"keep")
            self.assertEqual(list(root.glob(f".{output.name}.publication-bundles-*.tmp")), [])

    def test_promotion_failure_removes_only_exporter_owned_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            output = root / "bundles"
            marker = root / "unrelated.txt"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(FACTS.read_bytes())
            marker.write_bytes(b"keep")

            with mock.patch.object(
                exporter.os, "rename", side_effect=OSError("injected promotion failure")
            ):
                with self.assertRaisesRegex(
                    exporter.PublicationBundleError, "could not be promoted"
                ):
                    exporter.export_publication_bundles(sources, facts, output)

            self.assertFalse(output.exists())
            self.assertEqual(marker.read_bytes(), b"keep")
            self.assertEqual(list(root.glob(f".{output.name}.publication-bundles-*.tmp")), [])

    def test_refuses_a_destination_link_without_touching_its_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            target = root / "target"
            output = root / "bundles"
            sources.write_bytes(SOURCES.read_bytes())
            facts.write_bytes(FACTS.read_bytes())
            target.mkdir()
            marker = target / "keep.txt"
            marker.write_bytes(b"keep")
            try:
                output.symlink_to(target, target_is_directory=True)
            except OSError as exc:
                self.skipTest(f"directory links are unavailable: {exc}")

            with self.assertRaises(exporter.PublicationBundleError):
                exporter.export_publication_bundles(sources, facts, output)
            self.assertEqual(marker.read_bytes(), b"keep")


if __name__ == "__main__":
    unittest.main()
