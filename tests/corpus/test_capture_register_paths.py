"""Refusal paths of the live Register capture: session, manifest, responses, graph and output."""

from __future__ import annotations

import json
import os
import shutil
import tempfile
import unittest
import urllib.error
from pathlib import Path
from typing import Any, Callable
from unittest import mock

import fadden.capture_register as capture_module
from fadden.capture_register import CaptureRegisterError, RegisterExchange

from tests.corpus import test_live_register_capture as live

TITLE = "C2004A00467"


def capture_graph(root: Path, body: bytes = live.CURRENT_BODY) -> Path:
    root.mkdir()
    manifest = root / "manifest.json"
    manifest.write_bytes(live.json_bytes([live.manifest_row()]))
    destination = root / "capture"
    capture_module.capture_register_run(
        manifest, destination,
        session=live.MemorySession(
            {live.CURRENT_URL: [live.successful_exchange(body, "2026-08-29T14:00:01Z")]}))
    return destination


class HelperTests(unittest.TestCase):
    def test_the_clock_reads_whole_utc_seconds(self) -> None:
        self.assertRegex(capture_module._utc_now(), r"\A\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ\Z")

    def test_selected_headers_tolerate_a_missing_mapping_and_repeated_names(self) -> None:
        self.assertEqual(capture_module._selected_response_headers(None), {})
        self.assertEqual(
            capture_module._selected_response_headers(
                {"Content-Type": "a", "content-type": "b", "Server": "x", 7: "y"}),
            {"Content-Type": "a", "CONTENT-TYPE": "b"})

    def test_scalar_refusals(self) -> None:
        cases = [
            (lambda: capture_module._date("20260630", "probe"),
             "probe must be an ISO calendar date."),
            (lambda: capture_module._utc_timestamp("2026-02-30T00:00:00Z", "probe"),
             "probe must be a UTC timestamp ending in Z."),
            (lambda: capture_module._version_timestamp("2026-02-30T00:00:00", "probe"),
             "probe is invalid."),
            (lambda: capture_module._normalised_headers({1: "x"}),
             "response header names must be strings."),
        ]
        for call, message in cases:
            with self.subTest(message=message):
                with self.assertRaises(CaptureRegisterError) as caught:
                    call()
                self.assertEqual(str(caught.exception), message)

    def test_missing_directories_are_not_ordinary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            missing = Path(temporary) / "missing"
            with self.assertRaisesRegex(CaptureRegisterError, "^probe must be an ordinary directory.$"):
                capture_module._require_ordinary_directory(missing, "probe")
            with self.assertRaisesRegex(CaptureRegisterError, "^probe must have ordinary ancestors.$"):
                capture_module._require_ordinary_ancestors(missing, "probe")


class SessionTests(unittest.TestCase):
    def session(self, effects: list[object]) -> capture_module._HttpsRegisterSession:
        opener = live.FakeOpener(effects)
        clock = live.FakeClock()
        for patcher in (
            mock.patch.object(capture_module.urllib.request, "build_opener",
                              lambda *_handlers: opener),
            mock.patch.object(capture_module.time, "monotonic", clock.monotonic),
            mock.patch.object(capture_module.time, "sleep", clock.sleep),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)
        return capture_module._HttpsRegisterSession()

    def test_urls_outside_the_api_boundary_are_refused(self) -> None:
        session = self.session([])
        with self.assertRaisesRegex(CaptureRegisterError, "Register request URL is invalid."):
            session.get("https://api.prod.legislation.gov.au:port/v1/versions")
        with self.assertRaisesRegex(CaptureRegisterError, "outside the fixed API boundary"):
            session.get("https://example.test/v1/versions")

    def test_a_transport_failure_on_every_attempt_is_reported(self) -> None:
        session = self.session([urllib.error.URLError("down")] * 3)
        exchange = session.get(live.CURRENT_URL)
        self.assertEqual((exchange.status, exchange.error_category, exchange.attempts),
                         (None, "TRANSPORT_ERROR", 3))

    def test_a_retryable_status_on_every_attempt_is_returned(self) -> None:
        session = self.session([live.FakeHttpResponse(b"", status=503) for _ in range(3)])
        exchange = session.get(live.CURRENT_URL)
        self.assertEqual((exchange.status, exchange.attempts), (503, 3))


class ManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.manifest = Path(temporary.name) / "manifest.json"
        self.manifest.write_bytes(live.json_bytes([live.manifest_row()]))

    def assertRefused(self, message: str) -> None:
        with self.assertRaises(CaptureRegisterError) as caught:
            capture_module._load_manifest(self.manifest)
        self.assertEqual(str(caught.exception), message)

    def test_unreadable_or_malformed_manifests_are_refused(self) -> None:
        cases: dict[str, bytes] = {
            "manifest is invalid JSON.": b"[",
            "manifest title 1 must be an object.": b"[1]",
        }
        for message, content in cases.items():
            with self.subTest(message=message):
                self.manifest.write_bytes(content)
                self.assertRefused(message)
        self.manifest.unlink()
        self.assertRefused("manifest must be an ordinary file.")

    def test_a_read_failure_is_reported(self) -> None:
        with mock.patch.object(capture_module.os, "fstat", side_effect=OSError(5, "io")):
            self.assertRefused("manifest could not be read.")

    def test_a_manifest_replaced_during_the_read_is_refused(self) -> None:
        with mock.patch.object(capture_module, "_same_regular_file_identity",
                               side_effect=[True, False]):
            self.assertRefused("manifest changed during capture.")

    def test_a_manifest_that_grew_past_the_limit_while_open_is_refused(self) -> None:
        size = self.manifest.stat().st_size
        real_lstat = os.lstat
        first = {"value": True}

        class Shrunk:
            """The first look at the manifest reports it empty."""

            def __init__(self, details: os.stat_result) -> None:
                self.details = details

            def __getattr__(self, name: str) -> Any:
                return 0 if name == "st_size" else getattr(self.details, name)

        def shrunk(path, *args, **kwargs):
            details = real_lstat(path, *args, **kwargs)
            if Path(path) == self.manifest and first["value"]:
                first["value"] = False
                return Shrunk(details)
            return details

        with mock.patch.object(capture_module, "MAX_MANIFEST_BYTES", size - 1), \
                mock.patch.object(capture_module.os, "lstat", side_effect=shrunk):
            self.assertRefused("manifest exceeds the 8 MiB size limit.")


class ExchangeEvaluationTests(unittest.TestCase):
    ROW = json.loads(live.CURRENT_BODY)["value"][0]

    def test_rows_must_match_their_request_role(self) -> None:
        cases = [
            (dict(self.ROW, isCurrent=False), "current"),
            (dict(self.ROW), "history"),
            (dict(self.ROW, registerId=None, registeredAt=None, compilationNumber=" "), "current"),
        ]
        for row, role in cases:
            with self.subTest(role=role, row=row):
                with self.assertRaisesRegex(CaptureRegisterError, "^INVALID_ODATA_SHAPE$"):
                    capture_module._validated_row(row, TITLE, role)

    def evaluate(self, **changes: Any) -> capture_module._EvaluatedExchange:
        exchange = live.successful_exchange(live.CURRENT_BODY, "2026-08-29T14:00:01Z")
        exchange = RegisterExchange(**{**exchange.__dict__, **changes})
        return capture_module._evaluate_exchange(
            exchange, role="current", url=live.CURRENT_URL, title_id=TITLE)

    def test_a_success_without_bytes_or_a_media_type_fails_closed(self) -> None:
        self.assertEqual(self.evaluate(body=None).error_category, "INVALID_EXCHANGE")
        evaluated = self.evaluate(headers={"OData-Version": "4.0"})
        self.assertEqual((evaluated.error_category, evaluated.media_type),
                         ("UNSUPPORTED_MEDIA_TYPE", None))

    def test_two_bodies_under_one_digest_path_are_refused(self) -> None:
        evaluated = self.evaluate()
        evidence = {evaluated.request["evidence_path"]: b"different bytes"}
        with self.assertRaisesRegex(CaptureRegisterError, "response digest collision detected"):
            capture_module._retain_exchange_evidence(evaluated, evidence)

    def test_a_failed_history_lookup_names_its_own_category(self) -> None:
        exchanges = {
            live.CURRENT_URL: [live.successful_exchange(live.empty_versions_body(),
                                                        "2026-08-29T14:00:01Z")],
            live.HISTORY_URL: [RegisterExchange(checked_at="2026-08-29T14:00:03Z", status=500,
                                                headers={}, body=None, attempts=3)],
        }
        _capture, observation, _evidence = live.LiveRegisterCaptureStateTests._capture_documents(
            self, exchanges)
        item = observation["observations"][0]
        self.assertEqual((item["state"], item["error_category"]), ("LOOKUP_FAILED", "HTTP_STATUS"))


class StagedGraphTests(unittest.TestCase):
    """Each refusal of the final graph proof, reached with every earlier check satisfied."""

    @classmethod
    def setUpClass(cls) -> None:
        temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(temporary.cleanup)
        cls.root = Path(temporary.name)
        cls.unchanged = capture_graph(cls.root / "unchanged")
        superseded = live.version_body(TITLE, "2026-07-30", "33", "C2026C00300")
        cls.superseded = capture_graph(cls.root / "superseded", superseded)

    def copy(self, source: Path) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        target = Path(temporary.name) / "capture"
        shutil.copytree(source, target)
        return target

    def edit(self, graph: Path, name: str, mutate: Callable[[dict[str, Any]], None]) -> None:
        path = graph / name
        document = json.loads(path.read_bytes())
        mutate(document)
        path.write_bytes(capture_module._json_bytes(document))

    def assertRefused(self, mutations: dict[str, tuple[str, Callable[[dict[str, Any]], None]]],
                      source: Path | None = None) -> None:
        for message, (name, mutate) in mutations.items():
            with self.subTest(message=message):
                graph = self.copy(source or self.unchanged)
                self.edit(graph, name, mutate)
                with self.assertRaises(CaptureRegisterError) as caught:
                    capture_module.validate_capture_graph(graph)
                self.assertEqual(str(caught.exception), message)

    def test_baseline_refusals(self) -> None:
        def title(document):
            return document["titles"][0]

        def add_unsorted_title(document):
            extra = dict(title(document), register_id="C2004A00466")
            extra["source_url"] = title(document)["source_url"].replace(TITLE, "C2004A00466")
            extra["register_page"] = title(document)["register_page"].replace(TITLE, "C2004A00466")
            document["titles"].append(extra)

        baseline = "monitor-baseline.json"
        self.assertRefused({
            "staged capture baseline title is invalid":
                (baseline, lambda d: d["titles"].__setitem__(0, "title")),
            "staged capture baseline identity is invalid":
                (baseline, lambda d: title(d).update(register_id="bad")),
            "staged capture baseline collection is invalid":
                (baseline, lambda d: title(d).update(collection="Gazette")),
            "staged capture baseline current flag is invalid":
                (baseline, lambda d: title(d).update(version_is_current="yes")),
            "staged capture baseline current date is missing":
                (baseline, lambda d: title(d).update(version_is_current=False)),
            "staged capture baseline source URL is invalid":
                (baseline, lambda d: title(d).update(source_url="https://example.test/")),
            "staged capture baseline page URL is invalid":
                (baseline, lambda d: title(d).update(register_page="https://example.test/")),
            "staged capture baseline order is invalid": (baseline, add_unsorted_title),
            "staged capture baseline retrieval date is invalid":
                (baseline, lambda d: d.update(retrieved="2026-01-01")),
            "staged capture graph is invalid":
                (baseline, lambda d: d.update(retrieved="not a date")),
        })

    def test_capture_result_refusals(self) -> None:
        def result(document):
            return document["results"][0]

        def request(document):
            return result(document)["requests"][0]

        capture = "register-capture.json"
        self.assertRefused({
            "staged capture result is invalid":
                (capture, lambda d: d["results"].__setitem__(0, "result")),
            "staged capture result identity is invalid":
                (capture, lambda d: result(d).update(register_id="C2004A00466")),
            "staged capture result collection is invalid":
                (capture, lambda d: result(d).update(collection="LegislativeInstrument")),
            "staged capture result state is invalid":
                (capture, lambda d: result(d).update(state="REPEALED")),
            "staged capture result failure category is invalid":
                (capture, lambda d: result(d).update(state="LOOKUP_FAILED")),
            "staged capture request list is invalid":
                (capture, lambda d: result(d).update(requests=[])),
            "staged capture request roles are invalid":
                (capture, lambda d: request(d).update(role="history")),
            "staged capture request URL is invalid":
                (capture, lambda d: request(d).update(url=live.HISTORY_URL.replace("%24top=1", "x"))),
            "staged capture request metadata is invalid":
                (capture, lambda d: request(d).update(attempt_count=9)),
            "staged capture request headers are invalid":
                (capture, lambda d: request(d).update(response_headers={"odata-version": " 4.0"})),
            "staged capture empty response declaration is invalid":
                (capture, lambda d: request(d).update(response_sha256=None)),
            "staged capture result timestamp is invalid":
                (capture, lambda d: result(d).update(checked_at="2026-08-29T14:00:02Z")),
        })

    def test_observation_refusals(self) -> None:
        def item(document):
            return document["observations"][0]

        observation = "register-observation.json"
        self.assertRefused({
            "staged capture observation item is invalid":
                (observation, lambda d: d["observations"].__setitem__(0, "item")),
            "staged capture primary digest is inconsistent":
                (observation, lambda d: item(d).update(primary_response_sha256=None)),
            "staged capture primary media type is inconsistent":
                (observation, lambda d: item(d).update(primary_response_media_type="text/html")),
            "staged capture state fields are inconsistent":
                (observation, lambda d: item(d).update(observed_compilation_number="33")),
        })
        self.assertRefused({
            "staged capture required state field is missing":
                (observation, lambda d: item(d).update(observed_compilation_number=None)),
            "staged capture document identity is invalid":
                (observation, lambda d: item(d).update(observed_register_document_id="bad")),
        }, self.superseded)

    def test_layout_and_file_refusals(self) -> None:
        graph = self.copy(self.unchanged)
        (graph / "notes.txt").write_text("extra", encoding="utf-8")
        with self.assertRaisesRegex(CaptureRegisterError, "staged capture root layout is invalid"):
            capture_module.validate_capture_graph(graph)

        graph = self.copy(self.unchanged)
        (graph / "monitor-baseline.json").write_bytes(b"{")
        with self.assertRaisesRegex(CaptureRegisterError, "staged capture JSON is invalid"):
            capture_module.validate_capture_graph(graph)

        graph = self.copy(self.unchanged)
        (graph / "monitor-baseline.json").unlink()
        (graph / "monitor-baseline.json").mkdir()
        with self.assertRaisesRegex(CaptureRegisterError, "staged capture contains a non-ordinary file"):
            capture_module.validate_capture_graph(graph)

        with self.assertRaisesRegex(CaptureRegisterError, "staged capture directory is unavailable"):
            capture_module._validate_staged_graph(self.root / "absent")
        a_file = self.root / "a-file"
        a_file.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(CaptureRegisterError, "contains a non-ordinary directory"):
            capture_module._validate_staged_graph(a_file)

    def test_read_failures_and_races_are_refused(self) -> None:
        graph = self.copy(self.unchanged)
        real_lstat = os.lstat
        real_iterdir = Path.iterdir

        def lstat_failing_for(name: str):
            def lstat(path, *args, **kwargs):
                if Path(path).name == name:
                    raise PermissionError(13, "denied")
                return real_lstat(path, *args, **kwargs)
            return lstat

        def iterdir_failing_for(name: str):
            def iterdir(self):
                if self.name == name:
                    raise PermissionError(13, "denied")
                return real_iterdir(self)
            return iterdir

        cases = {
            "staged capture file is unavailable":
                mock.patch.object(capture_module.os, "lstat",
                                  side_effect=lstat_failing_for("monitor-baseline.json")),
            "staged capture file could not be read":
                mock.patch.object(capture_module.os, "fstat", side_effect=OSError(5, "io")),
            "staged capture file changed during validation":
                mock.patch.object(capture_module, "_same_regular_file_identity",
                                  side_effect=[True, False]),
            "staged capture layout could not be read":
                mock.patch.object(Path, "iterdir", iterdir_failing_for("capture")),
            "staged capture evidence layout could not be read":
                mock.patch.object(Path, "iterdir", iterdir_failing_for("evidence")),
            "staged capture graph is invalid":
                mock.patch.object(capture_module, "_validate_staged_graph_inner",
                                  side_effect=TypeError("unexpected value")),
        }
        for message, patcher in cases.items():
            with self.subTest(message=message), patcher:
                with self.assertRaises(CaptureRegisterError) as caught:
                    capture_module._validate_staged_graph(graph)
                self.assertEqual(str(caught.exception), message)


class OutputBoundaryTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.manifest = self.root / "manifest.json"
        self.manifest.write_bytes(live.json_bytes([live.manifest_row()]))

    def test_preflight_refusals(self) -> None:
        output = self.root / "capture"
        with mock.patch.object(capture_module.os, "lstat", side_effect=PermissionError(13, "x")):
            with self.assertRaisesRegex(CaptureRegisterError,
                                        "capture destination could not be inspected."):
                capture_module._preflight_output(output, self.manifest)
        with mock.patch.object(capture_module, "_same_location", return_value=True):
            with self.assertRaisesRegex(CaptureRegisterError, "must not replace its manifest input"):
                capture_module._preflight_output(output, self.manifest)
        with self.assertRaisesRegex(CaptureRegisterError, "capture output parent name is unsafe."):
            capture_module._preflight_output(self.root / ".hidden" / "capture", self.manifest)
        real_lstat = os.lstat

        def parent_denied(path, *args, **kwargs):
            if Path(path) == self.root:
                raise PermissionError(13, "denied")
            return real_lstat(path, *args, **kwargs)

        with mock.patch.object(capture_module.os, "lstat", side_effect=parent_denied):
            with self.assertRaisesRegex(CaptureRegisterError,
                                        "capture output parent could not be inspected."):
                capture_module._preflight_output(output, self.manifest)

    def test_staging_removal_refusals(self) -> None:
        prefix = ".capture.register-capture-"
        staging = self.root / (prefix + "x.tmp")
        with self.assertRaisesRegex(CaptureRegisterError, "outside the capture boundary"):
            capture_module._remove_owned_staging(self.root / "other.tmp", parent=self.root,
                                                 prefix=prefix)
        with mock.patch.object(capture_module.os, "lstat", side_effect=PermissionError(13, "x")):
            with self.assertRaisesRegex(CaptureRegisterError, "capture staging could not be inspected"):
                capture_module._remove_owned_staging(staging, parent=self.root, prefix=prefix)
        staging.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(CaptureRegisterError, "no longer an ordinary directory"):
            capture_module._remove_owned_staging(staging, parent=self.root, prefix=prefix)
        staging.unlink()
        staging.mkdir()
        with mock.patch.object(capture_module.shutil, "rmtree", side_effect=OSError(5, "busy")):
            with self.assertRaisesRegex(CaptureRegisterError, "capture staging could not be removed"):
                capture_module._remove_owned_staging(staging, parent=self.root, prefix=prefix)

    def test_a_missing_output_parent_that_cannot_be_created_is_reported(self) -> None:
        session = live.MemorySession({live.CURRENT_URL: [
            live.successful_exchange(live.CURRENT_BODY, "2026-08-29T14:00:01Z")]})
        with mock.patch.object(capture_module.os, "mkdir", side_effect=PermissionError(13, "x")):
            with self.assertRaisesRegex(CaptureRegisterError,
                                        "capture output parent could not be created."):
                capture_module.capture_register_run(
                    self.manifest, self.root / "new-parent" / "capture", session=session)
        self.assertFalse((self.root / "new-parent").exists())

    def test_a_staging_name_collision_is_never_removed(self) -> None:
        """A staging directory this run did not create is not its to remove."""
        fixed = mock.Mock(hex="0" * 32)
        staging = self.root / f".capture.register-capture-{'0' * 32}.tmp"
        staging.mkdir()
        (staging / "sentinel").write_bytes(b"keep")
        session = live.MemorySession({live.CURRENT_URL: [
            live.successful_exchange(live.CURRENT_BODY, "2026-08-29T14:00:01Z")]})
        with mock.patch.object(capture_module.uuid, "uuid4", return_value=fixed):
            with self.assertRaisesRegex(CaptureRegisterError, "capture output could not be written."):
                capture_module.capture_register_run(
                    self.manifest, self.root / "capture", session=session)
        self.assertEqual((staging / "sentinel").read_bytes(), b"keep")
        self.assertFalse((self.root / "capture").exists())

    def test_promotion_never_replaces_a_destination(self) -> None:
        """An empty destination that appears before the rename is kept, not replaced."""
        source, target = self.root / "source", self.root / "target"
        source.mkdir()
        (source / "file").write_bytes(b"new")
        target.mkdir()
        before = target.stat()
        with self.assertRaises(OSError):
            capture_module._rename_no_replace(source, target)
        after = target.stat()
        self.assertEqual((before.st_dev, before.st_ino), (after.st_dev, after.st_ino))
        self.assertEqual(list(target.iterdir()), [])
        self.assertEqual((source / "file").read_bytes(), b"new")

    def test_promotion_off_linux_is_a_plain_rename(self) -> None:
        source, target = self.root / "source", self.root / "target"
        source.mkdir()
        (source / "file").write_bytes(b"new")
        with mock.patch.object(capture_module, "sys", mock.Mock(platform="darwin")):
            capture_module._rename_no_replace(source, target)
        self.assertFalse(source.exists())
        self.assertEqual((target / "file").read_bytes(), b"new")

    @unittest.skipUnless(capture_module.sys.platform == "linux", "renameat2 is Linux-only")
    def test_promotion_fails_closed_without_renameat2(self) -> None:
        source, target = self.root / "source", self.root / "target"
        source.mkdir()
        with mock.patch.object(capture_module.ctypes, "CDLL", return_value=object()):
            with self.assertRaises(OSError):
                capture_module._rename_no_replace(source, target)
        self.assertTrue(source.is_dir())
        self.assertFalse(target.exists())


if __name__ == "__main__":
    unittest.main()
