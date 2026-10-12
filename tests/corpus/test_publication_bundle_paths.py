"""Refusal and platform paths of the publication evidence-bundle exporter."""

from __future__ import annotations

import contextlib
import errno
import io
import json
import os
import tempfile
import unittest
from importlib.metadata import PackageNotFoundError
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable
from unittest import mock

from fadden import export_monitor_contract as monitor_contract
from fadden import export_publication_bundles as exporter

from tests.corpus import test_publication_bundle_export as fixtures

DIGEST = "a" * 64
RID = "C2099A00001"


def projected() -> tuple[dict[str, Any], dict[str, Any]]:
    sources = json.loads(fixtures.SOURCES.read_bytes())
    facts = json.loads(fixtures.FACTS.read_bytes())
    baseline = monitor_contract.project_baseline(sources)
    return baseline, monitor_contract.project_observation(sources, facts)


def build(baseline: Any, observation: Any, baseline_digest: str = DIGEST) -> list[dict[str, Any]]:
    return exporter.build_publication_bundles(
        baseline, observation, baseline_sha256=baseline_digest,
        observation_facts_sha256=DIGEST, producer_version="1.0.0")


def add_title(baseline: dict[str, Any], observation: dict[str, Any], register_id: str,
              *, state: str = "UNCHANGED") -> None:
    title = dict(baseline["titles"][0], register_id=register_id)
    baseline["titles"].append(title)
    item = dict(observation["observations"][0], register_id=register_id)
    if state == "UNCHANGED":
        item.update(state="UNCHANGED", observed_compilation_number=None,
                    observed_compilation_date=None, observed_register_document_id=None)
    observation["observations"].append(item)
    observation["expected_register_ids"].append(register_id)


class BuildRefusalTests(unittest.TestCase):
    def assertRefused(self, mutate: Callable[[dict[str, Any], dict[str, Any]], Any],
                      message: str, baseline_digest: str = DIGEST) -> None:
        baseline, observation = projected()
        replaced = mutate(baseline, observation)
        if replaced is not None:
            baseline, observation = replaced
        with self.assertRaises(exporter.PublicationBundleError) as caught:
            build(baseline, observation, baseline_digest)
        self.assertEqual(str(caught.exception), message)

    def test_document_shape_refusals(self) -> None:
        def item(observation):
            return observation["observations"][0]

        def uncovered_title(baseline, observation):
            add_title(baseline, observation, "C2099A00002")
            observation["observations"].pop()

        cases = [
            (lambda b, o: ([], o), "baseline and observation must be objects."),
            (lambda b, o: o.update(mode="live"), "observation mode must be synthetic."),
            (lambda b, o: b.update(titles={}), "baseline titles and observations must be arrays."),
            (lambda b, o: o.update(expected_register_ids=RID),
             "expected_register_ids must be an array."),
            (lambda b, o: b["titles"].append("title"), "baseline title 2 must be an object."),
            (lambda b, o: b["titles"].append(dict(b["titles"][0])),
             "baseline contains duplicate register identifiers."),
            (lambda b, o: o.update(expected_register_ids=[]),
             "observation scope does not match the baseline."),
            (lambda b, o: o["observations"].append("item"), "observation 2 must be an object."),
            (lambda b, o: item(o).update(register_id="C2099A00009"),
             "observation contains an identifier outside the baseline."),
            (uncovered_title, "observation does not cover every baseline title."),
        ]
        for mutate, message in cases:
            with self.subTest(message=message):
                self.assertRefused(mutate, message)

    def test_field_refusals(self) -> None:
        def item(observation):
            return observation["observations"][0]

        def gazette(baseline, observation):
            baseline["titles"][0]["collection"] = "Gazette"
            item(observation)["collection"] = "Gazette"

        cases = [
            (lambda b, o: item(o).update(state=" SUPERSEDED"),
             "observation 1 state must be non-empty, trimmed text."),
            (lambda b, o: item(o).update(register_id="C2099/A00001"),
             "observation 1 register_id is not a safe identifier."),
            (gazette, "observation 1 collection is unsupported."),
            (lambda b, o: item(o).update(content_sha256="sha256:" + "A" * 64),
             "observation 1 content_sha256 is invalid."),
            (lambda b, o: item(o).update(content_media_type="text/plain"),
             "observation 1 content_media_type is unsupported."),
            (lambda b, o: item(o).update(observed_compilation_date="2026-02-30"),
             "observation 1 observed_compilation_date must be an ISO calendar date."),
            (lambda b, o: item(o).update(observed_compilation_date="20260805"),
             "observation 1 observed_compilation_date must be an ISO calendar date."),
        ]
        for mutate, message in cases:
            with self.subTest(message=message):
                self.assertRefused(mutate, message)
        self.assertRefused(lambda b, o: None, "baseline_sha256 must be a lowercase SHA-256 digest.",
                           baseline_digest="A" * 64)

    def test_ids_differing_only_in_case_cannot_share_a_bundle(self) -> None:
        self.assertRefused(lambda b, o: add_title(b, o, RID.lower(), state="SUPERSEDED"),
                           "observations produce duplicate bundle identities.")

    def test_unchanged_titles_produce_no_bundle(self) -> None:
        baseline, observation = projected()
        add_title(baseline, observation, "C2099A00002")
        self.assertEqual([bundle["source_event"]["register_id"]
                          for bundle in build(baseline, observation)], [RID])


class PromotionPlatformTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.staging = Path(temporary.name) / "staging"
        self.output = Path(temporary.name) / "output"
        self.staging.mkdir()

    def promote(self, name: str, platform: str, library: object = None) -> None:
        with mock.patch.object(exporter.os, "name", name), \
                mock.patch.object(exporter.sys, "platform", platform), \
                mock.patch.object(exporter.ctypes, "CDLL", return_value=library):
            exporter._promote_no_replace(self.staging, self.output)

    def test_windows_renames_into_the_absent_destination(self) -> None:
        self.promote("nt", "win32")
        self.assertTrue(self.output.is_dir())
        self.assertFalse(self.staging.exists())

    def test_platforms_without_a_no_replace_rename_are_refused(self) -> None:
        for platform, library, message in (
            ("darwin", None, "no-replace directory publication is unavailable"),
            ("linux", SimpleNamespace(), "renameat2 is unavailable"),
        ):
            with self.subTest(platform=platform):
                with self.assertRaises(OSError) as caught:
                    self.promote("posix", platform, library)
                self.assertEqual((caught.exception.errno, caught.exception.strerror),
                                 (errno.ENOTSUP, message))
        self.assertTrue(self.staging.is_dir())


class ExportBoundaryTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.sources = self.root / "sources.json"
        self.facts = self.root / "facts.json"
        self.sources.write_bytes(fixtures.SOURCES.read_bytes())
        self.facts.write_bytes(fixtures.FACTS.read_bytes())

    def export(self, output: Path | None = None) -> list[Path]:
        return exporter.export_publication_bundles(self.sources, self.facts,
                                                   output or self.root / "bundles")

    def assertRefused(self, message: str, output: Path | None = None) -> None:
        with self.assertRaises(exporter.PublicationBundleError) as caught:
            self.export(output)
        self.assertRegex(str(caught.exception), message)

    def test_version_metadata_falls_back_to_the_installed_package(self) -> None:
        missing = self.root / "VERSION"
        with mock.patch.object(exporter, "VERSION_PATH", missing), \
                mock.patch.object(exporter, "version", return_value="9.9.9"):
            self.assertEqual(exporter._producer_version(), "9.9.9")
        with mock.patch.object(exporter, "VERSION_PATH", missing), \
                mock.patch.object(exporter, "version", side_effect=PackageNotFoundError):
            with self.assertRaisesRegex(exporter.PublicationBundleError,
                                        "producer version metadata is unavailable."):
                exporter._producer_version()

    def test_unsafe_names_and_missing_inputs_are_refused(self) -> None:
        self.assertRefused("^publication bundle output name is unsafe.$", self.root / ".hidden")
        self.assertRefused("^publication bundle output parent name is unsafe.$",
                           self.root / ".parent" / "bundles")
        self.assertRefused("^publication bundle output parent does not exist: ",
                           self.root / "a" / "b" / "bundles")
        a_file = self.root / "a-file"
        a_file.write_text("", encoding="utf-8")
        self.assertRefused("^publication bundle output parent must be an ordinary directory: ",
                           a_file / "bundles")
        linked = self.root / "linked"
        (self.root / "a").mkdir()
        try:
            os.symlink(self.root / "a", linked, target_is_directory=True)
        except OSError:
            self.skipTest("symbolic links are unavailable to this account")
        self.assertRefused("^publication bundle output parent must be an ordinary directory: ",
                           linked / "child" / "bundles")
        self.facts.unlink()
        self.assertRefused("^observation facts input does not exist: ")

    def test_inspection_failures_are_reported(self) -> None:
        real_lstat = os.lstat

        def denied_for(target: Path):
            def lstat(path, *args, **kwargs):
                if Path(path) == target:
                    raise PermissionError(13, "denied")
                return real_lstat(path, *args, **kwargs)
            return lstat

        cases = [
            ("^sources input cannot be inspected: ", self.sources, None),
            ("^publication bundle output parent cannot be inspected: ", self.root, None),
            ("^publication bundle destination cannot be inspected: ", self.root / "bundles", None),
            ("^publication bundle output parent cannot be inspected: ", self.root,
             self.root / "new" / "bundles"),
        ]
        for message, target, output in cases:
            with self.subTest(message=message, output=output):
                with mock.patch.object(exporter.os, "lstat", side_effect=denied_for(target)):
                    self.assertRefused(message, output)

    def test_contract_input_and_version_failures_are_bounded(self) -> None:
        facts = json.loads(self.facts.read_bytes())
        facts["observations"][0].update(
            state="NO_LONGER_IN_FORCE", observed_compilation_number=None,
            observed_compilation_date=None, observed_register_document_id=None)
        self.facts.write_text(json.dumps(facts), encoding="utf-8")
        self.assertRefused("^observation state NO_LONGER_IN_FORCE cannot support")
        self.facts.write_bytes(fixtures.FACTS.read_bytes())
        with mock.patch.object(exporter, "VERSION_PATH", self.root):
            self.assertRefused("^publication bundle inputs could not be read: ")

    def test_a_missing_output_parent_that_cannot_be_created_is_reported(self) -> None:
        with mock.patch.object(exporter.os, "mkdir", side_effect=PermissionError(13, "denied")):
            self.assertRefused("^publication bundle output parent could not be created: ",
                               self.root / "new" / "bundles")

    def test_staging_cleanup_failures(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "outside the exporter boundary"):
            exporter._remove_owned_staging(self.root / "other", parent=self.root, prefix=".x")
        with mock.patch.object(exporter, "_remove_owned_staging",
                               side_effect=OSError(5, "cleanup failed")):
            with self.assertRaisesRegex(OSError, "cleanup failed"):
                self.export()
        self.assertTrue((self.root / "bundles").is_dir())


class MainTests(unittest.TestCase):
    def run_main(self, sources: Path, facts: Path, out: Path) -> tuple[int, str, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try:
                code = exporter.main([str(sources), str(facts), "--out", str(out)])
            except SystemExit as exit_info:
                code = exit_info.code
        return code, stdout.getvalue(), stderr.getvalue()

    def test_main_reports_each_bundle_and_bounds_errors(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "bundles"
            code, stdout, _ = self.run_main(fixtures.SOURCES, fixtures.FACTS, out)
            self.assertEqual(code, 0)
            self.assertEqual(stdout, "publication evidence bundle: %s\n"
                             % (out / "bundle-frl-c2099a00001-c2099c00002-r1.json"))
            code, _, stderr = self.run_main(fixtures.SOURCES, fixtures.FACTS, out)
            self.assertEqual(code, 2)
            self.assertIn("publication bundle destination must not exist", stderr)

