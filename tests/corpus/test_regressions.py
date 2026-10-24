"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import contextlib
import importlib.util
import io
import json
import os
import re
import shutil
import sys
import tempfile
import unittest
import warnings
import zipfile
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
STAGE = REPO / "fadden"


def stage_file(name):
    candidate = STAGE / name
    return candidate if candidate.exists() else REPO / name


def load_module(name, path):
    module_dir = str(Path(path).parent)
    sys.path.insert(0, module_dir)
    try:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.remove(module_dir)


class WorkText(str):
    """Count character inspections and native scans without a machine-speed limit."""

    def __new__(cls, value, budget=None):
        instance = super().__new__(cls, value)
        instance.budget = budget if budget is not None else [0, 128 * max(1, len(value))]
        return instance

    def _charge(self, units):
        self.budget[0] += units
        if self.budget[0] > self.budget[1]:
            raise AssertionError("text scanner exceeded its linear work budget")

    def __getitem__(self, key):
        value = super().__getitem__(key)
        self._charge(len(value))
        return value

    def __contains__(self, needle):
        self._charge(len(self) + len(needle))
        return super().__contains__(needle)

    def casefold(self):
        self._charge(len(self))
        return WorkText(super().casefold(), self.budget)

    def find(self, needle, start=0, end=None):
        end = len(self) if end is None else end
        found = super().find(needle, start, end)
        self._charge(max(0, end - start) if found < 0 else found - start + len(needle))
        return found

    def startswith(self, prefix, start=0, end=None):
        self._charge(len(prefix))
        return super().startswith(prefix, start, len(self) if end is None else end)


@contextlib.contextmanager
def linear_work(value):
    """Reject a native regex bypass and stop excessive Python work immediately."""
    text = WorkText(value)
    previous = sys.getprofile()

    def profile(_frame, event, function):
        if event == "c_call" and isinstance(getattr(function, "__self__", None), re.Pattern):
            raise AssertionError("unbounded input must use a linear text scan")

    sys.setprofile(profile)
    try:
        yield text
        assert 0 < text.budget[0] <= text.budget[1]
    finally:
        sys.setprofile(previous)


class ArchiveLifecycleTests(unittest.TestCase):
    @staticmethod
    def _epub_bytes():
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            archive.writestr(
                "document_1.xhtml",
                '<html><body><p class="ActHead4"><span class="CharSectno">1</span> '
                "Short title</p><p class=\"subsection\">Text.</p></body></html>",
            )
        return payload.getvalue()

    def test_zip_validation_closes_the_archive(self):
        download = load_module("download_archive_lifecycle", STAGE / "download.py")
        epub_bytes = self._epub_bytes()
        real_zipfile = zipfile.ZipFile
        opened = []

        def tracked_zipfile(*args, **kwargs):
            archive = real_zipfile(*args, **kwargs)
            opened.append(archive)
            return archive

        with mock.patch.object(download.zipfile, "ZipFile", side_effect=tracked_zipfile):
            self.assertTrue(download.valid_zip(io.BytesIO(epub_bytes)))
        self.assertEqual(len(opened), 1)
        self.assertIsNone(opened[0].fp)

class Retry13MergeTests(unittest.TestCase):
    def test_recoveries_are_merged_into_the_raw_manifest_in_place(self):
        """The documented pipeline has no manual patch step: retry13.py itself
        must fold successful recoveries into manifest_raw.json, keep
        retry13_patch.json as the audit record, and leave untouched titles
        exactly as the download stage wrote them."""
        retry13 = load_module("retry13_regression", STAGE / "retry13.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)

            manifest = [
                {"id": "C2004A00001", "name": "Already Downloaded Act",
                 "epub": "C2004A00001.epub", "bytes": 10, "status": "ok",
                 "versionStart": "2026-01-01"},
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "epub": None, "bytes": 0, "status": "no_epub",
                 "reason": "current_version_has_no_document",
                 "versionStart": "2026-03-01"},
                {"id": "F2021L00002", "name": "Unrecoverable Instrument",
                 "epub": None, "bytes": 0, "status": "no_epub",
                 "reason": "current_version_has_no_document",
                 "versionStart": "2026-04-01"},
            ]
            (scratch / "manifest_raw.json").write_text(
                json.dumps(manifest), encoding="utf-8")
            latest = {"start": "2025-06-30T00:00:00", "compilationNumber": "4",
                      "registerId": "F2025C00010"}
            (scratch / "probe13.json").write_text(json.dumps([
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "latest_doc": dict(latest)},
            ]), encoding="utf-8")

            def successful_fetch(_url, dst):
                Path(dst).write_bytes(b"PK-fake-epub")
                return True, "200", "application/epub+zip", 12, {
                    "registerId": "F2025C00010", "isAuthorised": True}

            retry13.SCRATCH = str(scratch)
            retry13.EPUB_DIR = str(epub_dir)
            with mock.patch.object(retry13.dl, "fetch", successful_fetch), \
                    mock.patch.object(retry13.dl, "CRAWL_DELAY", 0):
                with contextlib.redirect_stdout(io.StringIO()):
                    retry13.main()

            patches = json.loads(
                (scratch / "retry13_patch.json").read_text(encoding="utf-8"))
            self.assertEqual({p["id"] for p in patches}, {"F2020L01498"})

            merged = {a["id"]: a for a in json.loads(
                (scratch / "manifest_raw.json").read_text(encoding="utf-8"))}
            self.assertEqual(len(merged), 3)
            recovered = merged["F2020L01498"]
            self.assertEqual(recovered["epub"], "F2020L01498.epub")
            self.assertEqual(recovered["status"], "ok_superseded_version")
            self.assertIs(recovered["version_is_current"], False)
            self.assertEqual(recovered["current_version_start"], "2026-03-01")
            self.assertEqual(recovered["name"], "Recoverable Instrument")
            # A title without a probe recovery stays exactly as downloaded.
            self.assertEqual(merged["F2021L00002"]["status"], "no_epub")
            self.assertIsNone(merged["F2021L00002"]["epub"])
            self.assertNotIn("version_is_current", merged["F2021L00002"])
            self.assertEqual(merged["C2004A00001"]["status"], "ok")


class Retry13ManifestWriteTests(unittest.TestCase):
    def test_a_later_retry_failure_restores_every_earlier_change(self):
        retry13 = load_module("retry13_later_failure", STAGE / "retry13.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            ids = ("F2020L01498", "F2021L00002")
            manifest = [
                {"id": rid, "name": "Recoverable Instrument %d" % index,
                 "epub": None, "bytes": 0, "status": "no_epub",
                 "versionStart": "2026-03-0%d" % index}
                for index, rid in enumerate(ids, 1)
            ]
            manifest_path = scratch / "manifest_raw.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            old_manifest = manifest_path.read_text(encoding="utf-8")
            patch_path = scratch / "retry13_patch.json"
            patch_path.write_text('[{"prior":true}]', encoding="utf-8")
            old_patch = patch_path.read_text(encoding="utf-8")
            (scratch / "probe13.json").write_text(json.dumps([
                {"id": rid, "name": "Recoverable Instrument %d" % index,
                 "latest_doc": {"start": "2025-06-30T00:00:00",
                                "compilationNumber": str(index),
                                "registerId": "F2025C0000%d" % index}}
                for index, rid in enumerate(ids, 1)
            ]), encoding="utf-8")

            first_epub = epub_dir / (ids[0] + ".epub")
            first_sidecar = epub_dir / (ids[0] + ".epub.meta.json")
            first_epub.write_bytes(b"prior EPUB")
            first_sidecar.write_text('{"prior":true}', encoding="utf-8")

            def fail_second(_url, dst):
                Path(dst).write_bytes(b"replacement EPUB")
                if ids[1] in dst:
                    Path(dst + ".part").write_bytes(b"blocked response")
                    raise RuntimeError("HTTP 403 after 1 attempt")
                return True, "200", "application/epub+zip", 16, None

            retry13.SCRATCH = str(scratch)
            retry13.EPUB_DIR = str(epub_dir)
            with mock.patch.object(retry13.dl, "fetch", fail_second), \
                    mock.patch.object(retry13.dl, "CRAWL_DELAY", 0):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "HTTP 403"):
                        retry13.main()

            self.assertEqual(manifest_path.read_text(encoding="utf-8"), old_manifest)
            self.assertEqual(patch_path.read_text(encoding="utf-8"), old_patch)
            self.assertEqual(first_epub.read_bytes(), b"prior EPUB")
            self.assertEqual(first_sidecar.read_text(encoding="utf-8"),
                             '{"prior":true}')
            self.assertFalse((epub_dir / (ids[1] + ".epub")).exists())
            self.assertFalse((epub_dir / (ids[1] + ".epub.part")).exists())
            self.assertEqual(list(scratch.rglob("*.rollback")), [])

    def test_a_failed_manifest_write_restores_the_entire_retry_graph(self):
        retry13 = load_module("retry13_atomic", STAGE / "retry13.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            manifest = [
                {"id": "C2004A00001", "name": "Already Downloaded Act",
                 "epub": "C2004A00001.epub", "bytes": 10, "status": "ok",
                 "sourceUrl": "https://example.test/C2004A00001",
                 "versionStart": "2026-01-01"},
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "epub": None, "bytes": 0, "status": "no_epub",
                 "versionStart": "2026-03-01"},
            ]
            manifest_path = scratch / "manifest_raw.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            original = manifest_path.read_text(encoding="utf-8")
            patch_path = scratch / "retry13_patch.json"
            patch_path.write_text('[{"prior":true}]', encoding="utf-8")
            original_patch = patch_path.read_text(encoding="utf-8")
            (scratch / "probe13.json").write_text(json.dumps([
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "latest_doc": {"start": "2025-06-30T00:00:00", "compilationNumber": "4",
                                "registerId": "F2025C00010"}}]), encoding="utf-8")

            epub = epub_dir / "F2020L01498.epub"
            sidecar = epub_dir / "F2020L01498.epub.meta.json"
            epub.write_bytes(b"prior EPUB")
            sidecar.write_text('{"prior":true}', encoding="utf-8")

            def successful_fetch(_url, dst):
                Path(dst).write_bytes(b"replacement EPUB")
                return True, "200", "application/epub+zip", 16, {
                    "registerId": "F2025C00010", "isAuthorised": True}

            real_replace = retry13.dl.os.replace

            def fail_manifest_replace(source, destination):
                if (Path(source).name.startswith(".manifest_raw.json.") and Path(source).suffix == ".tmp"
                        and destination == str(manifest_path)):
                    raise OSError("no space left on device")
                return real_replace(source, destination)

            retry13.SCRATCH = str(scratch)
            retry13.EPUB_DIR = str(epub_dir)
            with mock.patch.object(retry13.dl, "fetch", successful_fetch), \
                    mock.patch.object(retry13.dl, "CRAWL_DELAY", 0), \
                    mock.patch.object(retry13.dl.os, "replace", fail_manifest_replace):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(OSError):
                        retry13.main()

            self.assertEqual(manifest_path.read_text(encoding="utf-8"), original)
            self.assertEqual(patch_path.read_text(encoding="utf-8"), original_patch)
            self.assertEqual(epub.read_bytes(), b"prior EPUB")
            self.assertEqual(sidecar.read_text(encoding="utf-8"),
                             '{"prior":true}')
            self.assertFalse((scratch / "manifest_raw.json.tmp").exists())
            self.assertEqual(list(scratch.rglob("*.rollback")), [])


class DownloadManifestWriteTests(unittest.TestCase):
    def test_a_failed_manifest_write_leaves_the_previous_crawl_intact(self):
        """download.py's own dump of manifest_raw.json replaces the file an
        earlier run wrote - BUILD.md documents re-running a single stage - and
        it lands at the end of a 2h40m crawl.  Truncating it in place and then
        failing mid-dump leaves a half-written file that extract.py cannot
        json.load, with the crawl it recorded already spent."""
        download = load_module("download_atomic_manifest", STAGE / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            (scratch / "acts_resolved.json").write_text(json.dumps([
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "versionStart": "2026-03-01", "compilationNumber": "4",
                 "compilationRegisterId": "F2026C00001"}]),
                encoding="utf-8")

            previous = [{
                "id": "F2020L01498", "name": "Prior Instrument",
                "epub": "F2020L01498.epub", "bytes": 10, "status": "ok",
                "sourceUrl": "https://example.test/F2020L01498",
                "versionStart": "2025-06-30",
            }]
            manifest_path = scratch / "manifest_raw.json"
            manifest_path.write_text(json.dumps(previous), encoding="utf-8")
            original = manifest_path.read_text(encoding="utf-8")

            old_payload = io.BytesIO()
            with zipfile.ZipFile(old_payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html>prior version</html>")
            epub = epub_dir / "F2020L01498.epub"
            sidecar = epub_dir / "F2020L01498.epub.meta.json"
            old_epub = old_payload.getvalue()
            old_sidecar = '{"versionStart":"2025-06-30"}'
            epub.write_bytes(old_epub)
            sidecar.write_text(old_sidecar, encoding="utf-8")

            def fake_fetch(url, dst, tries=3):
                new_payload = io.BytesIO()
                with zipfile.ZipFile(new_payload, "w") as archive:
                    archive.writestr("document_1.xhtml", "<html>new version</html>")
                Path(dst).write_bytes(new_payload.getvalue())
                return True, "200", "application/epub+zip", len(new_payload.getvalue()), {
                    "registerId": "F2025C00010", "isAuthorised": True}

            real_dump = download.json.dump

            def exploding_dump(obj, fp, **kwargs):
                if isinstance(obj, list):
                    fp.write('[{"id": "F2020')
                    raise OSError("no space left on device")
                return real_dump(obj, fp, **kwargs)

            download.SCRATCH = str(scratch)
            download.EPUB_DIR = str(epub_dir)
            # CRAWL_DELAY rather than a patched time.sleep: the sleep patches
            # elsewhere in this file mutate the shared time module for the whole
            # process, and this test needs no such reach.
            download.CRAWL_DELAY = 0
            download.fetch = fake_fetch
            with mock.patch.object(download.json, "dump", exploding_dump):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(OSError):
                        download.main()

            self.assertEqual(manifest_path.read_text(encoding="utf-8"), original)
            self.assertEqual(epub.read_bytes(), old_epub)
            self.assertEqual(sidecar.read_text(encoding="utf-8"), old_sidecar)
            self.assertFalse((scratch / "manifest_raw.json.tmp").exists())
            self.assertFalse(Path(str(epub) + ".rollback").exists())
            self.assertFalse(Path(str(sidecar) + ".rollback").exists())

    def test_a_response_error_is_logged_then_aborts_before_manifest_replacement(self):
        download = load_module("download_response_failure", STAGE / "download.py")
        cases = (
            ("http", b"<html>BODY_MUST_NOT_REACH_LOG</html>",
             "403|text/html\r\nX-Untrusted: value", "HTTP 403"),
            ("malformed-base64", b'{"bytes":"%%%NOT_BASE64%%%"}',
             "200|application/json", "invalid JSON envelope"),
            ("invalid-type", b'{"bytes":{"BODY_MUST_NOT_REACH_LOG":true}}',
             "200|application/json", "invalid JSON envelope"),
        )
        for label, body, response_meta, expected_error in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as tmp:
                scratch = Path(tmp)
                epub_dir = scratch / "corpus" / "epub"
                epub_dir.mkdir(parents=True)
                (scratch / "acts_resolved.json").write_text(json.dumps([{
                    "id": "F2020L01498", "name": "Blocked Instrument",
                    "versionStart": "2026-03-01", "compilationNumber": "4",
                    "compilationRegisterId": "F2026C00001",
                }]), encoding="utf-8")
                manifest_path = scratch / "manifest_raw.json"
                original = '[{"id": "previous-crawl"}]'
                manifest_path.write_text(original, encoding="utf-8")

                def failed_response(args, **kwargs):
                    if len(args) == 2 and Path(args[0]).name.lower() in ("curl", "curl.exe") and args[1] == "--version":
                        return mock.Mock(stdout="curl 8.4.0", returncode=0)
                    Path(args[args.index("-o") + 1]).write_bytes(body)
                    return mock.Mock(stdout=response_meta, returncode=0)

                download.SCRATCH = str(scratch)
                download.EPUB_DIR = str(epub_dir)
                download.CRAWL_DELAY = 0
                with mock.patch.object(download.subprocess, "run", failed_response), \
                        mock.patch.object(download.time, "sleep"):
                    with contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaisesRegex(download.DownloadError,
                                                    expected_error):
                            download.main()

                self.assertEqual(manifest_path.read_text(encoding="utf-8"), original)
                self.assertFalse((epub_dir / "F2020L01498.epub").exists())
                self.assertFalse((epub_dir / "F2020L01498.epub.part").exists())
                logged = (scratch / "download_log.txt").read_text(encoding="utf-8")
                self.assertIn(expected_error, logged)
                expected_ctype = " ".join(response_meta.split("|", 1)[1].split())
                self.assertIn(expected_ctype, logged)
                self.assertNotIn(body.decode("utf-8"), logged)
                lines = logged.splitlines()
                self.assertEqual(len(lines), 2)
                self.assertLessEqual(len(lines[0]), 180)
                self.assertEqual(lines[1], "ROLLBACK restored 1 changed title(s)")

    def test_a_failed_upgrade_preserves_the_prior_epub_and_sidecar(self):
        download = load_module("download_upgrade_failure", STAGE / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            (scratch / "acts_resolved.json").write_text(json.dumps([{
                "id": "F2020L01498", "name": "Upgraded Instrument",
                "versionStart": "2026-03-01", "compilationNumber": "5",
                "compilationRegisterId": "F2026C00001",
            }]), encoding="utf-8")
            manifest_path = scratch / "manifest_raw.json"
            old_manifest = '[{"id":"F2020L01498","epub":"F2020L01498.epub"}]'
            manifest_path.write_text(old_manifest, encoding="utf-8")

            payload = io.BytesIO()
            with zipfile.ZipFile(payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html>old version</html>")
            epub = epub_dir / "F2020L01498.epub"
            sidecar = epub_dir / "F2020L01498.epub.meta.json"
            old_epub = payload.getvalue()
            old_sidecar = '{"versionStart":"2025-06-30","bytes":123}'
            epub.write_bytes(old_epub)
            sidecar.write_text(old_sidecar, encoding="utf-8")

            def blocked(args, **kwargs):
                if len(args) == 2 and Path(args[0]).name.lower() in ("curl", "curl.exe") and args[1] == "--version":
                    return mock.Mock(stdout="curl 8.4.0", returncode=0)
                Path(args[args.index("-o") + 1]).write_bytes(b"<html>blocked</html>")
                return mock.Mock(stdout="403|text/html", returncode=0)

            download.SCRATCH = str(scratch)
            download.EPUB_DIR = str(epub_dir)
            with mock.patch.object(download.subprocess, "run", blocked), \
                    mock.patch.object(download.time, "sleep"):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(download.DownloadError, "HTTP 403"):
                        download.main()

            self.assertEqual(manifest_path.read_text(encoding="utf-8"), old_manifest)
            self.assertEqual(epub.read_bytes(), old_epub)
            self.assertEqual(sidecar.read_text(encoding="utf-8"), old_sidecar)
            self.assertFalse(Path(str(epub) + ".part").exists())

    def test_a_later_fetch_failure_rolls_back_every_earlier_upgrade(self):
        download = load_module("download_run_rollback", STAGE / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            ids = ("F2020L01498", "F2021L00002")
            (scratch / "acts_resolved.json").write_text(json.dumps([
                {"id": rid, "name": "Instrument %d" % index,
                 "versionStart": "2026-03-0%d" % index,
                 "compilationNumber": str(index),
                 "compilationRegisterId": "F2026C0000%d" % index}
                for index, rid in enumerate(ids, 1)
            ]), encoding="utf-8")
            manifest_path = scratch / "manifest_raw.json"
            old_manifest = '[{"id":"F2020L01498","epub":"F2020L01498.epub"}]'
            manifest_path.write_text(old_manifest, encoding="utf-8")

            old_payload = io.BytesIO()
            with zipfile.ZipFile(old_payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html>prior version</html>")
            new_payload = io.BytesIO()
            with zipfile.ZipFile(new_payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html>new version</html>")
            first_epub = epub_dir / (ids[0] + ".epub")
            first_sidecar = epub_dir / (ids[0] + ".epub.meta.json")
            old_epub = old_payload.getvalue()
            old_sidecar = '{"versionStart":"2025-06-30"}'
            first_epub.write_bytes(old_epub)
            first_sidecar.write_text(old_sidecar, encoding="utf-8")

            def fetch(_url, dst, tries=3):
                if ids[0] in dst:
                    Path(dst).write_bytes(new_payload.getvalue())
                    return True, "200", "application/epub+zip", len(new_payload.getvalue()), None
                raise download.DownloadError(
                    "HTTP 403 after 1 attempt (content-type text/html, 919 bytes)")

            download.SCRATCH = str(scratch)
            download.EPUB_DIR = str(epub_dir)
            download.CRAWL_DELAY = 0
            download.fetch = fetch
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(download.DownloadError, "HTTP 403"):
                    download.main()

            self.assertEqual(manifest_path.read_text(encoding="utf-8"), old_manifest)
            self.assertEqual(first_epub.read_bytes(), old_epub)
            self.assertEqual(first_sidecar.read_text(encoding="utf-8"), old_sidecar)
            self.assertFalse((epub_dir / (ids[1] + ".epub")).exists())
            self.assertFalse((epub_dir / (ids[1] + ".epub.meta.json")).exists())
            self.assertEqual(list(epub_dir.glob("*.rollback")), [])

    def test_a_first_build_failure_keeps_the_new_titles_it_fetched(self):
        # A first build fetches for hours. One persistent failure used to delete
        # every new EPUB before it; the complete pairs now survive for the cache.
        download = load_module("download_first_build_failure", STAGE / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            ids = ("F2020L01498", "F2021L00002", "F2022L00003")
            (scratch / "acts_resolved.json").write_text(json.dumps([
                {"id": rid, "name": "Instrument %d" % index,
                 "versionStart": "2026-03-0%d" % index,
                 "compilationNumber": str(index),
                 "compilationRegisterId": "F2026C0000%d" % index}
                for index, rid in enumerate(ids, 1)
            ]), encoding="utf-8")
            payload = io.BytesIO()
            with zipfile.ZipFile(payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html>current version</html>")
            fetched = []

            def fetch(_url, dst, tries=3):
                if ids[2] in dst and not fetched.count("retry"):
                    raise download.DownloadError("HTTP 403 after 1 attempt")
                fetched.append(os.path.basename(dst))
                Path(dst).write_bytes(payload.getvalue())
                return True, "200", "application/epub+zip", len(payload.getvalue()), None

            download.SCRATCH = str(scratch)
            download.EPUB_DIR = str(epub_dir)
            download.CRAWL_DELAY = 0
            download.fetch = fetch
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(download.DownloadError, "HTTP 403"):
                    download.main()
            self.assertFalse((scratch / "manifest_raw.json").exists())
            for rid in ids[:2]:
                self.assertTrue((epub_dir / (rid + ".epub")).exists(), rid)
                self.assertTrue((epub_dir / (rid + ".epub.meta.json")).exists(), rid)
            self.assertFalse((epub_dir / (ids[2] + ".epub")).exists())
            self.assertEqual(list(epub_dir.glob("*.rollback")), [])

            fetched.append("retry")
            with contextlib.redirect_stdout(io.StringIO()):
                download.main()
            manifest = json.loads((scratch / "manifest_raw.json").read_text(encoding="utf-8"))
            self.assertEqual([entry["status"] for entry in manifest], ["cached", "cached", "ok"])

    def test_a_sidecar_failure_rolls_back_the_epub_publication(self):
        download = load_module("download_sidecar_rollback", STAGE / "download.py")
        for had_prior in (True, False):
            with self.subTest(had_prior=had_prior), tempfile.TemporaryDirectory() as tmp:
                scratch = Path(tmp)
                epub_dir = scratch / "corpus" / "epub"
                epub_dir.mkdir(parents=True)
                (scratch / "acts_resolved.json").write_text(json.dumps([{
                    "id": "F2020L01498", "name": "Upgraded Instrument",
                    "versionStart": "2026-03-01", "compilationNumber": "5",
                    "compilationRegisterId": "F2026C00001",
                }]), encoding="utf-8")

                epub = epub_dir / "F2020L01498.epub"
                sidecar = epub_dir / "F2020L01498.epub.meta.json"
                old_payload = io.BytesIO()
                with zipfile.ZipFile(old_payload, "w") as archive:
                    archive.writestr("document_1.xhtml", "<html>prior version</html>")
                old_epub = old_payload.getvalue()
                old_sidecar = '{"versionStart":"2025-06-30"}'
                if had_prior:
                    epub.write_bytes(old_epub)
                    sidecar.write_text(old_sidecar, encoding="utf-8")

                new_payload = io.BytesIO()
                with zipfile.ZipFile(new_payload, "w") as archive:
                    archive.writestr("document_1.xhtml", "<html>new version</html>")

                def successful_fetch(_url, dst, tries=3):
                    Path(dst).write_bytes(new_payload.getvalue())
                    return True, "200", "application/epub+zip", len(new_payload.getvalue()), None

                real_replace = download.os.replace

                def fail_sidecar_replace(source, destination):
                    if Path(source).name.startswith("." + sidecar.name + ".") and Path(source).suffix == ".tmp" and destination == str(sidecar):
                        raise OSError("injected sidecar failure")
                    return real_replace(source, destination)

                download.SCRATCH = str(scratch)
                download.EPUB_DIR = str(epub_dir)
                download.CRAWL_DELAY = 0
                download.fetch = successful_fetch
                with mock.patch.object(download.os, "replace", fail_sidecar_replace):
                    with contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaisesRegex(OSError, "injected sidecar failure"):
                            download.main()

                if had_prior:
                    self.assertEqual(epub.read_bytes(), old_epub)
                    self.assertEqual(sidecar.read_text(encoding="utf-8"), old_sidecar)
                else:
                    self.assertFalse(epub.exists())
                    self.assertFalse(sidecar.exists())
                self.assertFalse(Path(str(epub) + ".rollback").exists())
                self.assertFalse(Path(str(sidecar) + ".tmp").exists())


class DownloadValidationTests(unittest.TestCase):
    def _fetch(self, download, dst, body, code="200", content_type="text/html",
               returncode=0):
        def fake_run(args, **kwargs):
            if len(args) == 2 and Path(args[0]).name.lower() in ("curl", "curl.exe") and args[1] == "--version":
                return mock.Mock(stdout="curl 8.4.0", returncode=0)
            output = Path(args[args.index("-o") + 1])
            self.assertNotEqual(output, Path(dst))
            output.write_bytes(body)
            return mock.Mock(stdout="%s|%s" % (code, content_type),
                             returncode=returncode)

        with mock.patch.object(download.subprocess, "run", fake_run):
            return download.fetch("https://example.test/x", dst, tries=1)

    def test_fetch_fails_closed_for_http_and_content_errors(self):
        """Access blocks and server errors must not become missing editions."""
        download = load_module("download_fetch_validation", STAGE / "download.py")
        patcher = mock.patch.object(download.time, "sleep")
        patcher.start()
        self.addCleanup(patcher.stop)
        with tempfile.TemporaryDirectory() as tmp:
            dst = str(Path(tmp) / "C2004A00001.epub")
            for code in ("403", "404", "429", "503"):
                with self.subTest(code=code):
                    with self.assertRaisesRegex(download.DownloadError,
                                                "HTTP %s" % code):
                        self._fetch(download, dst, b"<html>blocked</html>", code=code)
                    self.assertFalse(Path(dst).exists())

            with self.assertRaisesRegex(download.DownloadError,
                                        "invalid EPUB response"):
                self._fetch(download, dst, b"<html>not an EPUB</html>")
            self.assertFalse(Path(dst).exists())
            with self.assertRaisesRegex(download.DownloadError,
                                        "curl exit 28"):
                self._fetch(download, dst, b"", code="000", returncode=28)
            self.assertFalse(Path(dst).exists())

            payload = io.BytesIO()
            with zipfile.ZipFile(payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html><body><p>Act</p></body></html>")
            ok, _code, _ctype, size, _meta = self._fetch(
                download, dst, payload.getvalue(),
                content_type="application/epub+zip")
            self.assertTrue(ok)
            self.assertEqual(size, os.path.getsize(dst))

            envelope = json.dumps({
                "bytes": download.base64.b64encode(payload.getvalue()).decode("ascii"),
                "registerId": "C2026C00001", "isAuthorised": True,
            }).encode("utf-8")
            ok, _code, _ctype, size, meta = self._fetch(
                download, dst, envelope, content_type="application/json")
            self.assertTrue(ok)
            self.assertEqual(meta["registerId"], "C2026C00001")
            self.assertEqual(size, os.path.getsize(dst))
            valid_before_failure = Path(dst).read_bytes()

            invalid_envelopes = (
                b'{"bytes":"%%%NOT_BASE64%%%"}',
                b'{"bytes":17}',
                b'{"bytes":null}',
            )
            for index, envelope in enumerate(invalid_envelopes):
                bad_dst = str(Path(tmp) / ("invalid-%d.epub" % index))
                with self.subTest(envelope=envelope):
                    with self.assertRaisesRegex(download.DownloadError,
                                                "invalid JSON envelope"):
                        self._fetch(download, bad_dst, envelope,
                                    content_type="application/json")
                    self.assertFalse(Path(bad_dst).exists())
                    self.assertFalse(Path(bad_dst + ".part").exists())
            self.assertEqual(Path(dst).read_bytes(), valid_before_failure)

    def test_a_failed_envelope_cannot_taint_a_later_raw_epub(self):
        download = load_module("download_retry_metadata", STAGE / "download.py")
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            archive.writestr("document_1.xhtml", "<html>valid retry</html>")
        envelope = json.dumps({
            "bytes": download.base64.b64encode(b"not a ZIP").decode("ascii"),
            "registerId": "STALE-REGISTER-ID",
            "isAuthorised": True,
        }).encode("utf-8")
        responses = [
            (envelope, "application/json"),
            (payload.getvalue(), "application/epub+zip"),
        ]

        def fake_run(args, **_kwargs):
            if len(args) == 2 and Path(args[0]).name.lower() in ("curl", "curl.exe") and args[1] == "--version":
                return mock.Mock(stdout="curl 8.4.0", returncode=0)
            body, content_type = responses.pop(0)
            Path(args[args.index("-o") + 1]).write_bytes(body)
            return mock.Mock(stdout="200|%s" % content_type, returncode=0)

        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(download.subprocess, "run", fake_run), \
                mock.patch.object(download.time, "sleep"):
            dst = str(Path(tmp) / "retry.epub")
            ok, _code, _ctype, _size, meta = download.fetch(
                "https://example.test/x", dst, tries=2)

        self.assertTrue(ok)
        self.assertIsNone(meta)
        self.assertEqual(responses, [])

    def test_only_a_null_compilation_register_id_is_no_epub(self):
        """A current version with no registerId is the API's no-edition case;
        it needs no document request and stays eligible for retry13.py."""
        download = load_module("download_no_document", STAGE / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            (scratch / "acts_resolved.json").write_text(json.dumps([{
                "id": "F2020L01498", "name": "Unpublished current version",
                "versionStart": "2026-03-01", "compilationNumber": "5",
                "compilationRegisterId": None,
            }]), encoding="utf-8")
            download.SCRATCH = str(scratch)
            download.EPUB_DIR = str(epub_dir)
            download.CRAWL_DELAY = 0
            download.fetch = mock.Mock(side_effect=AssertionError("must not fetch"))
            stale_epub = epub_dir / "F2020L01498.epub"
            stale_sidecar = epub_dir / "F2020L01498.epub.meta.json"
            stale_epub.write_bytes(b"stale prior-version bytes")
            stale_sidecar.write_text('{"versionStart":"2025-06-30"}', encoding="utf-8")

            with contextlib.redirect_stdout(io.StringIO()):
                download.main()

            download.fetch.assert_not_called()
            record = json.loads(
                (scratch / "manifest_raw.json").read_text(encoding="utf-8"))[0]
            self.assertEqual(record["status"], "no_epub")
            self.assertEqual(record["reason"], "current_version_has_no_document")
            self.assertEqual(stale_epub.read_bytes(), b"stale prior-version bytes")
            self.assertEqual(stale_sidecar.read_text(encoding="utf-8"),
                             '{"versionStart":"2025-06-30"}')

            del record["compilationRegisterId"]
            (scratch / "acts_resolved.json").write_text(
                json.dumps([record]), encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(ValueError,
                                            "missing compilationRegisterId"):
                    download.main()

    def test_no_document_count_is_not_hard_coded_in_maintained_prose(self):
        for name in ("README.md", "BUILD.md", "retry13.py", "src/fadden/extract.rs"):
            with self.subTest(name=name):
                text = (stage_file(name)).read_text(encoding="utf-8")
                self.assertNotRegex(text, r"(?i)\bthirteen titles\b")


class StalenessBucketTests(unittest.TestCase):
    def _run(self, base, response):
        module = load_module("check_current_buckets", base / "check_current.py")
        module.fetch_json = response if callable(response) else lambda _url: response
        buffer = io.StringIO()
        with mock.patch.object(module.sys, "argv", ["check_current.py"]), \
                mock.patch.object(module.time, "sleep"):
            with contextlib.redirect_stdout(buffer):
                module.main()
        return buffer.getvalue()

    def _corpus(self, base):
        (base / "sources.json").write_text(json.dumps({
            "retrieved": "2026-08-03",
            "titles": [{"register_id": "F2020L01498", "name": "Demo Instrument",
                        "collection": "LegislativeInstrument",
                        "compilation_number": "4", "compilation_date": "2025-06-30"}],
        }), encoding="utf-8")
        for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
            shutil.copy2(stage_file(name), base / name)

    def test_a_current_version_with_no_document_is_its_own_bucket(self):
        """Filing these under "re-download these" sends the reader at a URL
        that answers 404: the compilation does not exist yet."""
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F2020L01498", "start": "2026-07-01T00:00:00Z",
                "compilationNumber": "5", "registerId": None}]})
            self.assertIn("no compilation published: 1", out)
            self.assertIn("NO COMPILATION PUBLISHED", out)
            self.assertNotIn("SUPERSEDED, re-download these", out)
            self.assertIn("superseded: 0", out)

    def test_a_published_later_compilation_is_reported_as_superseded(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F2020L01498", "start": "2026-07-01T00:00:00Z",
                "compilationNumber": "5", "registerId": "F2026C00099"}]})
            self.assertIn("superseded: 1", out)
            self.assertIn("SUPERSEDED, re-download these", out)
            self.assertIn("no compilation published: 0", out)

    def test_a_failed_current_lookup_is_not_inferred_from_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            module = load_module("check_current_transport_failure", base / "check_current.py")
            responses = [None, {"value": [{"titleId": "F2020L01498"}]}]
            calls = []

            def response(_url):
                calls.append(_url)
                return responses.pop(0)

            module.fetch_json = response
            buffer = io.StringIO()
            with mock.patch.object(module.sys, "argv", ["check_current.py"]), \
                    mock.patch.object(module.time, "sleep"):
                with contextlib.redirect_stdout(buffer):
                    module.main()

            out = buffer.getvalue()
            self.assertEqual(len(calls), 1)
            self.assertIn("no longer in force: 0", out)
            self.assertIn("lookup failed: 1", out)

    def test_a_wrong_title_response_is_a_lookup_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F9999L99999", "start": "2026-07-01T00:00:00Z",
                "compilationNumber": "5", "registerId": "F2026C00099"}]})
            self.assertIn("no longer in force: 0", out)
            self.assertIn("lookup failed: 1", out)

    def test_a_title_with_history_but_no_current_version_is_no_longer_in_force(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            responses = iter([
                {"value": []},
                {"value": [{"titleId": "F2020L01498"}]},
            ])
            out = self._run(base, lambda _url: next(responses))
            self.assertIn("no longer in force: 1", out)
            self.assertIn("NO LONGER IN FORCE", out)
            self.assertIn("lookup failed: 0", out)

    def test_an_unchanged_current_version_stays_out_of_action_buckets(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F2020L01498", "start": "2025-06-30T00:00:00Z",
                "compilationNumber": "4", "registerId": "F2025C00042",
            }]})
            self.assertIn("unchanged: 1", out)
            self.assertIn("superseded: 0", out)
            self.assertIn("no compilation published: 0", out)
            self.assertIn("no longer in force: 0", out)
            self.assertIn("lookup failed: 0", out)


class GeneratedReadmeTests(unittest.TestCase):
    def test_the_corpus_readme_states_no_staleness_counts_it_never_observed(self):
        """check_current.py is a separate command run after this README is
        written, so any run's counts baked into the template go stale on the
        next rebuild while the interpolated figures beside them stay correct."""
        source = (REPO / "src" / "fadden" / "finalize.rs").read_text(encoding="utf-8")
        stale = re.search(r"\d[\d,]* unchanged", source)
        self.assertIsNone(stale, stale.group(0) if stale else "")
        self.assertIn("{notcurrent} titles are not the current text", source)

    def test_the_corpus_readme_does_not_file_those_titles_as_superseded(self):
        """They are their own bucket.  check_current.py separates them, and
        sources.json gives their reason as the in-force version having no
        published compilation on the Register.  Calling them superseded tells
        the reader to re-download a document that does not exist, at a URL that
        answers 404 - the miscategorisation the pipeline was fixed to stop
        making."""
        source = (REPO / "src" / "fadden" / "finalize.rs").read_text(encoding="utf-8")
        paragraphs = [" ".join(p.split()) for p in re.split(r"\n\s*\n", source)
                      if "`titles_not_current_version`" in p]
        self.assertTrue(paragraphs, "no README paragraph cites the bucket")
        for paragraph in paragraphs:
            self.assertNotIn("superseded compilation", paragraph, paragraph)
        staleness = source.split("## Checking staleness", 1)[1].split("## Licence", 1)[0]
        self.assertIn("no published compilation", " ".join(staleness.split()))

    def test_scope_states_the_measured_corpus_size(self):
        """scope.md called the corpus about a gigabyte; the EPUBs, markdown and
        rates measure about 150 MB (27 September 2026). Hold the figure so the
        old claim cannot return beside the reasons the corpus is not shipped."""
        scope = " ".join((REPO / "docs" / "scope.md").read_text(encoding="utf-8").split())
        self.assertIn("It is about 150 MB", scope)
        self.assertNotIn("gigabyte", scope)

    def test_this_repositorys_readme_does_not_file_those_titles_as_superseded(self):
        """The same claim, in the README a reader meets first.  The generated
        text was corrected and this one was not, and the test above reads only
        finalize's template, so nothing held the sibling.  BUILD.md and check_current.py
        keep these titles out of the superseded bucket precisely because that
        bucket says re-download, at a URL that answers 404."""
        readme = (REPO / "docs" / "scope.md").read_text(encoding="utf-8")
        paragraphs = [" ".join(p.split()) for p in re.split(r"\n\s*\n", readme)
                      if "version_is_current" in p]
        self.assertTrue(paragraphs, "no scope paragraph cites the flag")
        for paragraph in paragraphs:
            self.assertNotIn("superseded compilation", paragraph, paragraph)
            self.assertIn("no published compilation", paragraph, paragraph)


class ShippedIntermediateTests(unittest.TestCase):
    def test_build_md_records_that_the_shipped_manifest_predates_the_fix(self):
        """manifest_md.json still holds the pre-fix parse of F2025L00281 - the
        published corpus was built before the volume gate was per volume - and
        BUILD.md cites those same figures as the evidence of the bug.  Quote the
        manifest's own numbers here, so regenerating it fails this test rather
        than leaving BUILD.md describing a state that no longer exists."""
        manifest = json.loads((STAGE / "manifest_md.json").read_text(encoding="utf-8"))
        entry = [a for a in manifest if a["id"] == "F2025L00281"]
        self.assertEqual(len(entry), 1)
        build = " ".join((REPO / "BUILD.md").read_text(encoding="utf-8").split())
        self.assertIn("predate the volume-gate fix", build)
        self.assertIn("sections=%d, words=%d" % (entry[0]["sections"], entry[0]["words"]),
                      build)
        # The table stack landed after the same run, so it moves the same
        # figures and has to be recorded in the same breath as the volume gate.
        self.assertIn("table stack", build)
        # The dated evidence document owns the historical table and caveat.
        readme = " ".join((REPO / "docs" / "build-evidence.md").read_text(encoding="utf-8").split())
        self.assertIn("predates the volume-gate fix", readme)
        self.assertIn("table-stack fix", readme)

    def test_the_published_dist_figures_are_the_shipped_manifests_arithmetic(self):
        """README and RELEASE_NOTES sized the redistributable subset at 21,728
        rows and 6,068,848 words for this snapshot.  dist.py counts the kept
        titles' sections.jsonl lines, so the subset is the manifest less the
        titles pii_flagged.json names - 21,596 rows - and its word total cannot
        exceed the whole corpus's, which the same README gives as 6,041,512.
        Derive the figures here, so regenerating the manifest fails this test
        rather than leaving both files sizing a distribution nobody can build.
        """
        manifest = json.loads((STAGE / "manifest_md.json").read_text(encoding="utf-8"))
        flagged = json.loads((STAGE / "pii_flagged.json").read_text(encoding="utf-8"))
        dropped = {entry["register_id"] for entry in flagged}
        kept = [a for a in manifest if a["id"] not in dropped]
        # "934 titles and 21,596 rows", however each file qualifies "titles".
        subset = r"%s(?: \w+)? titles and %s rows" % (
            re.escape(f"{len(kept):,}"),
            re.escape(f"{sum(a['sections'] for a in kept):,}"))
        corpus_words = sum(a["words"] for a in manifest)
        for name in ("docs/scope.md", "RELEASE_NOTES.md"):
            with self.subTest(document=name):
                text = " ".join((REPO / name).read_text(encoding="utf-8").split())
                self.assertRegex(text, subset)
                for figure in re.findall(r"([\d,]+) (?:body )?words", text):
                    self.assertLessEqual(int(figure.replace(",", "")), corpus_words)

    def test_the_removed_row_total_is_not_described_as_the_flagged_row_count(self):
        """README and RELEASE_NOTES both called the 188 rows dist.py removes
        the rows "naming private individuals".  pii_flagged.json records only
        169 that way - 188 is the total row count of the 12 flagged titles, and
        the other 19 rows are signature blocks and figure placeholders that no
        scan matched.  They still go, because dist.py drops a flagged title by
        register id (`if rid in removed: continue`) and never reads row_ids, so
        the removal is whole-title.  Both figures and the relationship between
        them are asserted here, so restating either as the other fails.
        """
        flagged = json.loads((STAGE / "pii_flagged.json").read_text(encoding="utf-8"))
        rows_total = sum(entry["rows_total"] for entry in flagged)
        rows_flagged = sum(entry["rows_flagged"] for entry in flagged)
        # The flagged count is exactly the enumerated rows, and is the smaller
        # of the two.  A rescan that collapses the gap needs the documents
        # reworded, not this test weakened.
        self.assertEqual(rows_flagged,
                         sum(len(entry["row_ids"]) for entry in flagged))
        self.assertLess(rows_flagged, rows_total)
        # The prose claim is only true because of what the dist stage does.
        dist = (REPO / "src" / "fadden" / "dist.rs").read_text(encoding="utf-8")
        self.assertIn("if removed.contains(&rid)", dist)
        self.assertNotIn("row_ids", dist)
        # "12 titles hold 188 rows between them, of which ... 169 ... naming
        # private individuals", however each document phrases it.
        accounting = re.compile(
            r"\b%s titles\b.{0,80}?\b%s rows\b.{0,120}?\b%s\b.{0,40}?"
            r"nam\w* private individuals" % (
                len(flagged), f"{rows_total:,}", f"{rows_flagged:,}"))
        for name in ("docs/scope.md", "RELEASE_NOTES.md"):
            with self.subTest(document=name):
                text = " ".join((REPO / name).read_text(encoding="utf-8").split())
                self.assertRegex(text, accounting)
                # ... and the consequence of the whole-title drop, which is why
                # the larger figure is the one subtracted from the manifest.
                self.assertRegex(text, r"drops a flagged title whole")
                self.assertRegex(
                    text, r"all %s\b.{0,60}?leave the distribution" % f"{rows_total:,}")


class DocumentedCommandTests(unittest.TestCase):
    """The commands and module paths the repository's own documents hand a
    reader.  Each one here was wrong on an unmodified checkout."""

    def test_the_documented_verification_command_scopes_discovery_to_the_corpus(self):
        """CONTRIBUTING.md promises a standard-library-only run and then
        discovered from `tests`, which errors on the 2 radar modules that
        import pytest, so the file's only verification instruction was red on a
        clean clone.  The command must scope discovery to the corpus half."""
        contributing = (REPO / "CONTRIBUTING.md").read_text(encoding="utf-8")
        command = "python -m unittest discover -s tests/corpus -t . -v"
        self.assertIn(command, contributing)
        self.assertNotIn("unittest discover -s tests -v", contributing)

    def test_every_test_module_the_documents_name_exists(self):
        """CONTRIBUTING.md and .gitignore named pre-merge paths: the modules
        moved under tests/corpus/ when the radar half merged in.  The
        .gitignore reference is the justification for keeping manifest_md.json
        committed, so a reader who follows it cannot check the reason."""
        for name in ("CONTRIBUTING.md", ".gitignore"):
            text = (REPO / name).read_text(encoding="utf-8")
            for reference in re.findall(r"\btests[./][\w./]*test_\w+", text):
                with self.subTest(document=name, reference=reference):
                    parts = reference.replace(".py", "").replace("/", ".").split(".")
                    # A dotted reference may carry a class and method after the
                    # module, so accept the longest prefix that is a real file.
                    self.assertTrue(
                        any(REPO.joinpath(*parts[:stop - 1],
                                          parts[stop - 1] + ".py").exists()
                            for stop in range(len(parts), 1, -1)),
                        "no module answers this path")


class PostMergeNamingTests(unittest.TestCase):
    """Names left behind by the radar half's merge into this repository."""

    def test_llms_txt_names_the_repository_pyproject_declares(self):
        """llms.txt is the file an agent reads for orientation and it gave the
        archived tax-radar-au repository as this project's home, which RADAR.md
        records as archived and pyproject.toml contradicts."""
        pyproject = (REPO / "pyproject.toml").read_text(encoding="utf-8")
        homepage = re.search(r'^Homepage = "([^"]+)"$', pyproject, flags=re.M)
        self.assertIsNotNone(homepage)
        llms = (REPO / "llms.txt").read_text(encoding="utf-8")
        self.assertIn("- **Repository**: %s" % homepage.group(1), llms)

    def test_llms_txt_names_the_release_version_declares(self):
        """llms.txt told agents the current release was 0.1.6 after VERSION had
        moved on, so it has to follow the version file."""
        version = (REPO / "VERSION").read_text(encoding="utf-8").strip()
        llms = (REPO / "llms.txt").read_text(encoding="utf-8")
        self.assertIn("- **Version**: %s, released as the tag `v%s`." % (version, version), llms)

    def test_no_document_sends_a_reader_to_the_archived_repository(self):
        """README.md linked the consumer half to the repository it merged out
        of rather than to RADAR.md beside it."""
        for name in ("README.md", "BUILD.md", "CONTRIBUTING.md", "RADAR.md",
                     "RELEASE_NOTES.md", "RELEASING.md", "llms.txt"):
            with self.subTest(document=name):
                text = (REPO / name).read_text(encoding="utf-8")
                self.assertNotIn("github.com/ryanduguid/tax-radar-au", text)

    def test_no_document_claims_a_rename_from_a_name_to_itself(self):
        """BUILD.md and CONTRIBUTING.md both read "`tax-radar-au` (formerly
        `tax-radar-au`)".  RADAR.md carries the real former artefact name."""
        for name in ("README.md", "BUILD.md", "CONTRIBUTING.md", "RADAR.md",
                     "RELEASE_NOTES.md", "RELEASING.md", "llms.txt"):
            text = (REPO / name).read_text(encoding="utf-8")
            for current, former in re.findall(r"`([^`\n]+)` \(formerly `([^`\n]+)`\)", text):
                with self.subTest(document=name, name=current):
                    self.assertNotEqual(current, former)


class SourceWarningTests(unittest.TestCase):
    def test_no_tracked_module_compiles_with_a_warning(self):
        """An invalid escape in a non-raw literal is a DeprecationWarning now
        and a SyntaxError later, and ci.yml records why that matters here: a
        test file that stops being collected leaves every leg green, so the
        module would disappear from the run without a red anywhere."""
        for folder in ("fadden", "tax_radar_au", "tests", "tools"):
            for path in sorted((REPO / folder).rglob("*.py")):
                with self.subTest(module=path.relative_to(REPO).as_posix()):
                    with warnings.catch_warnings(record=True) as caught:
                        warnings.simplefilter("always")
                        compile(path.read_text(encoding="utf-8"), str(path), "exec")
                    self.assertEqual([str(w.message) for w in caught], [])


class PathBoundaryTests(unittest.TestCase):
    def test_distribution_boundary_rejects_a_root_directory_junction(self):
        paths = load_module("corpus_paths_root_junction", STAGE / "corpus_paths.py")
        with (
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths.os.path, "isjunction", return_value=True, create=True),
            mock.patch.object(paths.os, "walk") as walk,
        ):
            with self.assertRaisesRegex(ValueError, "junction"):
                paths.reject_symlinks("corpus")
        walk.assert_not_called()

    def test_distribution_boundary_rejects_windows_directory_junctions(self):
        paths = load_module("corpus_paths_junction", STAGE / "corpus_paths.py")
        with (
            mock.patch.object(paths.os, "walk", return_value=[("corpus", ["junction"], [])]) as walk,
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths, "is_reparse_point", return_value=False),
            mock.patch.object(
                paths.os.path,
                "isjunction",
                side_effect=lambda candidate: candidate == paths.os.path.join("corpus", "junction"),
                create=True,
            ),
        ):
            with self.assertRaisesRegex(ValueError, "junction"):
                paths.reject_symlinks("corpus")
        walk.assert_called_once_with("corpus", followlinks=False)

    def test_distribution_boundary_rejects_other_windows_reparse_points(self):
        paths = load_module("corpus_paths_reparse", STAGE / "corpus_paths.py")
        with (
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths.os.path, "isjunction", return_value=False, create=True),
            mock.patch.object(paths, "is_reparse_point", return_value=True),
            mock.patch.object(paths.os, "walk") as walk,
        ):
            with self.assertRaisesRegex(ValueError, "reparse point"):
                paths.reject_symlinks("corpus")
        walk.assert_not_called()

    def test_distribution_boundary_accepts_an_ordinary_tree(self):
        paths = load_module("corpus_paths_ordinary", STAGE / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "corpus"
            (root / "nested").mkdir(parents=True)
            (root / "nested" / "file.txt").write_bytes(b"x")
            self.assertIsNone(paths.reject_symlinks(root))

    def test_register_id_and_contained_child_reject_traversal(self):
        paths = load_module("corpus_paths_regression", STAGE / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "corpus"
            root.mkdir()
            self.assertEqual(paths.register_id("F2020L01498"), "F2020L01498")
            self.assertEqual(paths.register_id("C2004A05138"), "C2004A05138")
            with self.assertRaises(ValueError):
                paths.register_id("../../outside")
            with self.assertRaises(ValueError):
                paths.child(root, "..", "outside")
            with self.assertRaises(ValueError):
                paths.child(root, ".")

    def test_root_layout_supports_checkout_and_deployed_build(self):
        paths = load_module("corpus_paths_layout", STAGE / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            checkout_script = base / "checkout" / "extract.py"
            checkout_script.parent.mkdir()
            checkout_script.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(checkout_script)), checkout_script.parent / "corpus")

            packaged = base / "checkout-pkg" / "fadden" / "extract.py"
            packaged.parent.mkdir(parents=True)
            packaged.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(packaged)), packaged.parent.parent / "corpus")

            deployed_script = base / "corpus-root" / "build" / "extract.py"
            deployed_script.parent.mkdir(parents=True)
            deployed_script.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(deployed_script)), deployed_script.parent.parent)

            deployed_pkg = base / "corpus-root-pkg" / "build" / "fadden" / "extract.py"
            deployed_pkg.parent.mkdir(parents=True)
            deployed_pkg.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(deployed_pkg)), deployed_pkg.parent.parent.parent)

    def test_check_current_uses_checkout_output_or_deployed_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            checkout = base / "checkout"
            checkout.mkdir()
            for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
                shutil.copy2(stage_file(name), checkout / name)
            checkout_module = load_module("check_current_checkout", checkout / "check_current.py")
            self.assertEqual(Path(checkout_module.ROOT), checkout / "corpus")

            deployed = base / "deployed"
            deployed.mkdir()
            (deployed / "sources.json").write_text("{}", encoding="utf-8")
            for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
                shutil.copy2(stage_file(name), deployed / name)
            deployed_module = load_module("check_current_deployed", deployed / "check_current.py")
            self.assertEqual(Path(deployed_module.ROOT), deployed)


class CheckCurrentStatusTests(unittest.TestCase):
    """The staleness checker must read the Register's status before compilations."""

    TITLE = {"register_id": "F2016L01256", "name": "Fabricated repealed instrument",
             "collection": "LegislativeInstrument", "compilation_number": "1",
             "compilation_date": "2016-08-02"}

    def run_check(self, responses):
        current = load_module("check_current_status_regression", STAGE / "check_current.py")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "sources.json").write_text(json.dumps(
                {"retrieved": "2016-08-02", "titles": [self.TITLE]}), encoding="utf-8")
            stdout = io.StringIO()
            with mock.patch.object(current, "ROOT", str(root)), \
                    mock.patch.object(current, "fetch_json", side_effect=responses), \
                    mock.patch.object(current.time, "sleep"), \
                    mock.patch.object(sys, "argv", ["check_current.py"]), \
                    contextlib.redirect_stdout(stdout):
                current.main()
        return stdout.getvalue()

    def test_a_repealed_current_row_is_reported_as_no_longer_in_force(self):
        # The shape of the public API record for F2016L01256: isCurrent true,
        # status Repealed and no document id. Read compilation-first it looked
        # like a compilation the Register had yet to publish.
        output = self.run_check([{"value": [{
            "titleId": "F2016L01256", "start": "2026-08-12T00:00:00Z",
            "compilationNumber": None, "registerId": None,
            "isCurrent": True, "status": "Repealed"}]}])
        self.assertIn("no longer in force: 1", output)
        self.assertIn("no compilation published: 0", output)
        self.assertIn("NO LONGER IN FORCE", output)
        self.assertNotIn("NO COMPILATION PUBLISHED", output)

    def test_an_in_force_row_without_a_document_is_still_an_unpublished_compilation(self):
        output = self.run_check([{"value": [{
            "titleId": "F2016L01256", "start": "2026-08-12T00:00:00Z",
            "compilationNumber": None, "registerId": None,
            "isCurrent": True, "status": "InForce"}]}])
        self.assertIn("no compilation published: 1", output)
        self.assertIn("no longer in force: 0", output)
        self.assertIn("NO COMPILATION PUBLISHED", output)

    def test_the_query_asks_the_register_for_the_status_field(self):
        current = load_module("check_current_select_regression", STAGE / "check_current.py")
        seen = []

        def record(url):
            seen.append(url)
            return {"value": [{"titleId": "F2016L01256", "start": "2016-08-02T00:00:00Z",
                               "compilationNumber": "1", "registerId": "F2016C00001",
                               "isCurrent": True, "status": "InForce"}]}

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "sources.json").write_text(json.dumps(
                {"retrieved": "2016-08-02", "titles": [self.TITLE]}), encoding="utf-8")
            with mock.patch.object(current, "ROOT", str(root)), \
                    mock.patch.object(current, "fetch_json", side_effect=record), \
                    mock.patch.object(current.time, "sleep"), \
                    mock.patch.object(sys, "argv", ["check_current.py"]), \
                    contextlib.redirect_stdout(io.StringIO()):
                current.main()
        self.assertTrue(seen)
        self.assertIn("compilationNumber,registerId,status", seen[0])


if __name__ == "__main__":
    unittest.main()
