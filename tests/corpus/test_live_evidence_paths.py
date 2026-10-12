"""Platform-neutral refusals of the live-evidence exporter: helpers, snapshot, candidates, CLI.

The Windows-only publication transaction is exercised by the Windows CI leg in
test_live_evidence_bundle_export.py; these cases cover what every platform runs.
"""

from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Any, Callable
from unittest import mock

import fadden.export_live_evidence_bundles as export_module
from fadden.export_live_evidence_bundles import LiveEvidenceBundleError, LiveEvidenceExport

from tests.corpus import test_live_evidence_bundle_export as live

POSIX_PINNING = (os.name == "posix" and export_module._posix_descriptor_pinning_available())


def write_capture(root: Path) -> Path:
    capture, _ = live.LiveEvidenceBundleContractTests()._write_capture(root)
    return capture


class TemporaryCaptureTestCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.capture = write_capture(self.root)

    def documents(self) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
        return live.LiveEvidenceGraphTests._documents(self.capture)

    def write_documents(self, baseline: dict[str, Any], register: dict[str, Any],
                        observation: dict[str, Any]) -> None:
        live.LiveEvidenceGraphTests._write_documents(self.capture, baseline, register, observation)

    def response(self) -> bytes:
        return next((self.capture / "evidence").iterdir()).read_bytes()


class HelperTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_scalar_and_document_refusals(self) -> None:
        listed = self.root / "list.json"
        listed.write_text("[]", encoding="utf-8")
        cases: list[tuple[Callable[[], object], str]] = [
            (lambda: export_module._read_json(self.root / "absent.json", "probe"),
             "probe could not be read as UTF-8 JSON."),
            (lambda: export_module._read_json(listed, "probe"), "probe must be an object."),
            (lambda: export_module._required_object([], "probe"), "probe must be an object."),
            (lambda: export_module._required_sha256("sha256:" + "A" * 64, "probe"),
             "probe must be a lowercase SHA-256 identifier."),
            (lambda: export_module._project_members({}, ("state",), "probe"),
             "probe is missing state."),
            (lambda: export_module._project_capture_result(
                {field: [] for field in export_module._CAPTURE_RESULT_FIELDS} | {"requests": {}}),
             "capture result requests must be an array."),
            (lambda: export_module._read_response(self.root / "absent.json"),
             "retained response could not be read."),
            (lambda: export_module._version_timestamp("2026-02-30T00:00:00", "probe"),
             "probe is invalid."),
            (lambda: export_module._ordinary_directory_details(listed, "probe"),
             "probe must be an ordinary directory."),
        ]
        for call, message in cases:
            with self.subTest(message=message):
                with self.assertRaises(LiveEvidenceBundleError) as caught:
                    call()
                self.assertEqual(str(caught.exception), message)

    def test_windows_handle_values_are_plain_integers(self) -> None:
        handle_type = export_module.wintypes.HANDLE
        self.assertEqual(export_module._windows_handle_value(None), 0)
        self.assertEqual(export_module._windows_handle_value(handle_type()), 0)
        self.assertEqual(export_module._windows_handle_value(handle_type(42)), 42)
        self.assertEqual(export_module._windows_handle_value(7), 7)

    def test_an_oversized_response_is_refused(self) -> None:
        response = self.root / "response.json"
        response.write_bytes(b"12345")
        with mock.patch.object(export_module, "_MAX_RESPONSE_BYTES", 4):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "exceeds the 256 KiB limit"):
                export_module._read_response(response)

    def test_unreadable_version_metadata_is_refused(self) -> None:
        with mock.patch.object(export_module, "_VERSION_PATH", self.root / "VERSION"):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "producer version could not be read."):
                export_module._producer_version()

    def test_raw_rows_must_be_one_current_document(self) -> None:
        row = json.loads(live.DEFAULT_RESPONSE)["value"][0]
        context = json.loads(live.DEFAULT_RESPONSE)["@odata.context"]

        def body(value: object) -> bytes:
            return json.dumps({"@odata.context": context, "value": value}).encode("utf-8")

        cases = {
            "retained response OData shape is invalid.": body([row, row]),
            "retained response OData row is invalid.": body([dict(row, extra=1)]),
            "retained response current document identifier is missing.":
                body([dict(row, registerId=None)]),
        }
        for message, response in cases.items():
            with self.subTest(message=message):
                with self.assertRaises(LiveEvidenceBundleError) as caught:
                    export_module._raw_current_row(response, row["titleId"])
                self.assertEqual(str(caught.exception), message)

    def test_paths_on_different_roots_never_collide(self) -> None:
        with mock.patch.object(export_module.os.path, "commonpath", side_effect=ValueError):
            self.assertFalse(export_module._output_collides_capture(self.root / "a", self.root))

    def test_an_uninspectable_destination_is_refused(self) -> None:
        with mock.patch.object(export_module.os, "lstat", side_effect=PermissionError(13, "x")):
            with self.assertRaisesRegex(LiveEvidenceBundleError,
                                        "live evidence output could not be inspected."):
                export_module._require_absent_destination(self.root / "output")


class FallbackInspectionTests(unittest.TestCase):
    """The pathname fallbacks used where descriptor pinning is unavailable."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.file = self.root / "file.json"
        self.file.write_text("{}", encoding="utf-8")
        patcher = mock.patch.object(export_module, "_posix_descriptor_pinning_available",
                                    return_value=False)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_directory_details_refuse_missing_and_non_directories(self) -> None:
        for path in (self.root / "absent", self.file):
            with self.subTest(path=path.name):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "capture input inventory changed."):
                    export_module._pinned_directory_details(path, None)
        self.assertFalse(export_module._pinned_path_identity_matches(
            self.root / "absent", os.lstat(self.root)))

    def test_children_and_inventories_are_read_by_path(self) -> None:
        self.assertEqual(export_module._child_details(self.file).st_size, 2)
        self.assertEqual(set(export_module._directory_inventory(self.root)), {"file.json"})
        with self.assertRaisesRegex(LiveEvidenceBundleError, "source file is unavailable."):
            export_module._child_details(self.root / "absent")
        with self.assertRaisesRegex(LiveEvidenceBundleError, "capture input inventory changed."):
            export_module._directory_inventory(self.root / "absent")

    def test_only_single_link_regular_files_are_ordinary(self) -> None:
        with self.assertRaisesRegex(LiveEvidenceBundleError, "source file is not ordinary."):
            export_module._ordinary_file_details(self.root)
        try:
            os.link(self.file, self.root / "second-link.json")
        except OSError:
            self.skipTest("hard links are unavailable here")
        with self.assertRaisesRegex(LiveEvidenceBundleError, "source file is not ordinary."):
            export_module._ordinary_file_details(self.file)

    @unittest.skipIf(os.name == "nt", "Windows copies through its own pathname branch")
    def test_copying_without_pinning_is_refused_off_windows(self) -> None:
        with self.assertRaisesRegex(LiveEvidenceBundleError, "secure directory pinning is unavailable."):
            export_module._copy_regular_file(self.file, self.root / "copy.json")


@unittest.skipUnless(POSIX_PINNING, "requires POSIX descriptor-relative no-follow primitives")
class PosixPinningTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.file = self.root / "file.json"
        self.file.write_text("{}", encoding="utf-8")

    def test_descriptor_reads_need_a_pinned_directory(self) -> None:
        for call in (lambda: export_module._child_details(self.file),
                     lambda: export_module._directory_inventory(self.root)):
            with self.assertRaisesRegex(LiveEvidenceBundleError,
                                        "secure directory pinning is unavailable."):
                call()

    def test_pinning_and_closing_failures_are_bounded(self) -> None:
        with self.assertRaisesRegex(LiveEvidenceBundleError, "capture input could not be pinned."):
            with export_module._PinnedDirectoryChain() as pinned:
                pinned.pin(self.root / "absent", "capture input")
        with self.assertRaisesRegex(LiveEvidenceBundleError, "capture snapshot cleanup failed."):
            with export_module._PinnedDirectoryChain() as pinned:
                os.close(pinned.pin(self.root, "capture input"))

    def copy(self, **patches: Any) -> None:
        with export_module._PinnedDirectoryChain() as pinned:
            handle = pinned.pin(self.root, "capture input")
            with contextlib.ExitStack() as stack:
                for name, value in patches.items():
                    stack.enter_context(mock.patch.object(*value) if name.startswith("obj")
                                        else mock.patch.object(export_module, name, value))
                export_module._copy_regular_file(self.file, self.root / "copy.json",
                                                 directory_handle=handle)

    def test_copy_refusals(self) -> None:
        cases = {
            "capture snapshot source file could not be copied.":
                {"obj": (export_module.os, "fdopen", mock.Mock(side_effect=OSError(24, "x")))},
            "capture snapshot source file changed.":
                {"_same_regular_file_identity": mock.Mock(return_value=False)},
        }
        for message, patches in cases.items():
            with self.subTest(message=message):
                (self.root / "copy.json").unlink(missing_ok=True)
                with self.assertRaises(LiveEvidenceBundleError) as caught:
                    self.copy(**patches)
                self.assertEqual(str(caught.exception), message)
        (self.root / "copy.json").unlink(missing_ok=True)
        with mock.patch.object(export_module, "_same_regular_file_identity",
                               side_effect=[True, False]):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "source file changed."):
                self.copy()


class SnapshotTests(TemporaryCaptureTestCase):
    def snapshot(self) -> None:
        target = self.root / ("snapshot-%d" % len(list(self.root.glob("snapshot-*"))))
        export_module._snapshot_capture_graph(self.capture, target)

    def assertSnapshotRefused(self, message: str) -> None:
        with self.assertRaises(LiveEvidenceBundleError) as caught:
            self.snapshot()
        self.assertEqual(str(caught.exception), message)

    def test_a_default_copy_reads_its_own_identity(self) -> None:
        if os.name != "nt" and not POSIX_PINNING:
            self.skipTest("no supported pinning on this platform")
        with export_module._PinnedDirectoryChain() as pinned:
            handle = pinned.pin(self.capture, "capture input") if POSIX_PINNING else None
            target = self.root / "baseline-copy.json"
            details = export_module._copy_regular_file(
                self.capture / "monitor-baseline.json", target, directory_handle=handle)
        self.assertEqual(details.st_size, target.stat().st_size)

    def test_layout_refusals(self) -> None:
        (self.capture / "notes.txt").write_text("", encoding="utf-8")
        self.assertSnapshotRefused("capture input layout is invalid.")
        (self.capture / "notes.txt").unlink()
        (self.capture / "evidence" / "notes.txt").write_text("", encoding="utf-8")
        self.assertSnapshotRefused("capture evidence layout is invalid.")

    def test_identity_and_creation_refusals(self) -> None:
        cases = {
            "capture input inventory changed.":
                mock.patch.object(export_module, "_pinned_path_identity_matches", return_value=False),
            "capture snapshot could not be created.":
                mock.patch.object(export_module.os, "mkdir", side_effect=PermissionError(13, "x")),
        }
        for message, patcher in cases.items():
            with self.subTest(message=message), patcher:
                self.assertSnapshotRefused(message)

    def test_a_source_changed_after_copying_is_refused(self) -> None:
        real_child_details = export_module._child_details
        calls = {"count": 0}

        def fail_late(path: Path, **kwargs: Any) -> os.stat_result:
            calls["count"] += 1
            # Seven sources each read once to admit and once after copying; fail the re-checks.
            if calls["count"] > 2 * len(list(self.capture.rglob("*.json"))):
                raise OSError(5, "gone")
            return real_child_details(path, **kwargs)

        with mock.patch.object(export_module, "_child_details", side_effect=fail_late):
            self.assertSnapshotRefused("capture snapshot source file changed.")
        identity = mock.patch.object(export_module, "_same_regular_file_identity",
                                     side_effect=[True] * 12 + [False])
        with identity:
            self.assertSnapshotRefused("capture snapshot source file changed.")


class CandidateTests(TemporaryCaptureTestCase):
    def candidate(self, mutate: Callable[[dict, dict, dict], None] | None = None,
                  response: bytes | None = None) -> tuple[str, dict[str, Any]]:
        baseline, register, observation = self.documents()
        title = baseline["titles"][0]
        result = register["results"][0]
        item = observation["observations"][0]
        if mutate is not None:
            mutate(title, result, item)
        return export_module._candidate_bundle(
            title, result, item, version="1.0.0", baseline_sha256="sha256:" + "0" * 64,
            observation_sha256="sha256:" + "0" * 64, observed_at="2026-08-29T00:00:00Z",
            response=self.response() if response is None else response)

    def test_the_valid_candidate_is_built(self) -> None:
        bundle_id, bundle = self.candidate()
        self.assertTrue(bundle_id.startswith("bundle-frl-"))
        self.assertEqual(bundle["schema_version"], "evidence-bundle.v2")

    def test_candidate_refusals(self) -> None:
        def request(result):
            return result["requests"][0]

        cases: list[tuple[str, Callable[[dict, dict, dict], None] | None, bytes | None]] = [
            ("SUPERSEDED capture result must have one request.",
             lambda t, r, o: r["requests"].append(copy.deepcopy(request(r))), None),
            ("retained response digest does not match the capture request.", None, b"other"),
            ("retained response length does not match the capture request.",
             lambda t, r, o: request(r).update(response_length=1), None),
            ("observation response digest does not match the capture request.",
             lambda t, r, o: o.update(primary_response_sha256="sha256:" + "0" * 64), None),
            ("candidate state is not SUPERSEDED.", lambda t, r, o: o.update(state="UNCHANGED"), None),
            ("candidate current request is invalid.",
             lambda t, r, o: request(r).update(http_status=203), None),
            ("candidate primary response media type is invalid.",
             lambda t, r, o: o.update(primary_response_media_type="text/html"), None),
            ("observation capture result digest is invalid.",
             lambda t, r, o: o.update(capture_result_sha256="sha256:" + "0" * 64), None),
        ]
        for message, mutate, response in cases:
            with self.subTest(message=message):
                with self.assertRaises(LiveEvidenceBundleError) as caught:
                    self.candidate(mutate, response)
                self.assertEqual(str(caught.exception), message)


class ValidatedSnapshotTests(TemporaryCaptureTestCase):
    def build(self) -> tuple[str, list[tuple[str, dict[str, Any]]]]:
        return export_module._build_validated_snapshot(self.capture)

    def assertBuildRefused(self, message: str) -> None:
        with self.assertRaises(LiveEvidenceBundleError) as caught:
            self.build()
        self.assertEqual(str(caught.exception), message)

    def test_a_non_candidate_observation_is_skipped(self) -> None:
        live.LiveEvidenceGraphTests()._set_non_candidate_state(self.capture, "UNCHANGED")
        _tag, candidates = self.build()
        self.assertEqual(candidates, [])

    def write_raw(self, register: dict[str, Any], observation: dict[str, Any]) -> None:
        """Write the pair with only the digests this stage re-checks brought up to date."""
        register_bytes = export_module._json_bytes(register)
        observation["capture_sha256"] = export_module._sha256_id(register_bytes)
        (self.capture / "register-capture.json").write_bytes(register_bytes)
        (self.capture / "register-observation.json").write_bytes(
            export_module._json_bytes(observation))

    def test_graph_refusals(self) -> None:
        def changed_baseline() -> None:
            baseline, _register, _observation = self.documents()
            (self.capture / "monitor-baseline.json").write_bytes(
                export_module._json_bytes(dict(baseline, retrieved="2026-01-01")))

        def titles_not_an_array() -> None:
            self.write_documents(*self.with_baseline(titles={}))

        def no_titles() -> None:
            self.write_documents(*self.with_baseline(titles=[]))

        def without_requests() -> None:
            _baseline, register, observation = self.documents()
            register["results"][0]["requests"] = []
            self.write_raw(register, observation)

        def moved_evidence() -> None:
            _baseline, register, observation = self.documents()
            register["results"][0]["requests"][0]["evidence_path"] = "evidence/elsewhere.json"
            self.write_raw(register, observation)

        def repeated_observation() -> None:
            _baseline, register, observation = self.documents()
            observation["observations"].append(copy.deepcopy(observation["observations"][0]))
            self.write_raw(register, observation)

        cases: list[tuple[str, Callable[[], None]]] = [
            ("capture graph digests are inconsistent.", changed_baseline),
            ("capture graph collections must be arrays.", titles_not_an_array),
            ("SUPERSEDED observation is missing its capture data.", no_titles),
            ("SUPERSEDED capture result must have a request.", without_requests),
            ("capture evidence path is invalid.", moved_evidence),
            ("candidate identity is duplicated.", repeated_observation),
        ]
        originals = {name: (self.capture / name).read_bytes() for name in (
            "monitor-baseline.json", "register-capture.json", "register-observation.json")}
        for message, mutate in cases:
            with self.subTest(message=message):
                for name, content in originals.items():
                    (self.capture / name).write_bytes(content)
                mutate()
                self.assertBuildRefused(message)

    def with_baseline(self, **changes: Any) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
        baseline, register, observation = self.documents()
        return dict(baseline, **changes), register, observation


class ExportAndCliTests(TemporaryCaptureTestCase):
    def test_preflight_refusals(self) -> None:
        cases = [
            ("live evidence output name is unsafe.", self.root / ".hidden"),
            ("live evidence output parent name is unsafe.", self.root / ".parent" / "output"),
        ]
        for message, output in cases:
            with self.subTest(message=message):
                with self.assertRaises(LiveEvidenceBundleError) as caught:
                    export_module._preflight_output(output)
                self.assertEqual(str(caught.exception), message)
        real_lstat = os.lstat

        def parent_denied(path, *args, **kwargs):
            if Path(path) == self.root:
                raise PermissionError(13, "denied")
            return real_lstat(path, *args, **kwargs)

        with mock.patch.object(export_module.os, "lstat", side_effect=parent_denied):
            with self.assertRaisesRegex(LiveEvidenceBundleError,
                                        "live evidence output parent could not be inspected."):
                export_module._preflight_output(self.root / "output")

    def test_publication_results_and_raw_failures(self) -> None:
        published = (self.root / "output" / "bundle.json",)
        with mock.patch.object(export_module, "_publish_candidates", return_value=published):
            export = export_module.export_live_evidence_bundles(self.capture, self.root / "output")
        self.assertEqual(export.candidates, published)
        self.assertTrue(export.release_tag.startswith("live-evidence-v2-"))
        with mock.patch.object(export_module, "_publish_candidates",
                               side_effect=OSError(5, "disk")):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "^live evidence export failed.$"):
                export_module.export_live_evidence_bundles(self.capture, self.root / "output")

    def test_the_command_reports_the_candidates(self) -> None:
        export = LiveEvidenceExport("live-evidence-v2-abc", (self.root / "a.json",))
        stdout = io.StringIO()
        with mock.patch.object(export_module, "export_live_evidence_bundles", return_value=export), \
                contextlib.redirect_stdout(stdout):
            self.assertEqual(export_module.main([str(self.capture), "--out", "output"]), 0)
        self.assertEqual(stdout.getvalue(), "candidate_count=1\nrelease_tag=live-evidence-v2-abc\n")


if __name__ == "__main__":
    unittest.main()
