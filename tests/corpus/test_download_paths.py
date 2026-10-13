"""Hostile archives, failed transfers and rollback edges of the download stage, all offline."""

from __future__ import annotations

import contextlib
import io
import json
import os
import struct
import tempfile
import unittest
import zipfile
import zlib
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from fadden import download

from tests.corpus import test_regressions as regressions
from tests.corpus import test_security_remediation as security

NAME = b"document_1.xhtml"


def single_member(raw: bytes, content: bytes, *, method: int = 8) -> bytes:
    """One-member archive whose compressed bytes are exactly ``raw``."""
    crc = zlib.crc32(content)
    local = struct.pack("<4s5H3I2H", b"PK\x03\x04", 20, 0, method, 0, 33,
                        crc, len(raw), len(content), len(NAME), 0) + NAME
    central = struct.pack("<4s6H3I5H2I", b"PK\x01\x02", 20, 20, 0, method, 0, 33,
                          crc, len(raw), len(content), len(NAME), 0, 0, 0, 0, 0, 0) + NAME
    end = struct.pack("<4s4H2IH", b"PK\x05\x06", 0, 0, 1, 1,
                      len(central), len(local) + len(raw), 0)
    return local + raw + central + end


def deflated(content: bytes) -> bytes:
    compressor = zlib.compressobj(wbits=-zlib.MAX_WBITS)
    return compressor.compress(content) + compressor.flush()


def read_all(payload: bytes) -> None:
    with download.open_epub(io.BytesIO(payload)) as archive:
        for member in archive.infolist():
            download.read_epub_member(archive, member)


class ArchiveRefusalTests(unittest.TestCase):
    def assertRefused(self, payload: bytes, message: str) -> None:
        with self.assertRaisesRegex(download.DownloadError, message):
            read_all(bytes(payload))

    def test_an_empty_archive_is_admitted(self) -> None:
        stream = io.BytesIO()
        zipfile.ZipFile(stream, "w").close()
        self.assertTrue(download.valid_zip(io.BytesIO(stream.getvalue())))

    def test_directory_records_must_be_complete_and_counted(self) -> None:
        raw = security.epub_bytes("body")
        directory = raw.index(b"PK\x01\x02")
        eocd = len(raw) - 22
        bad_signature = bytearray(raw)
        bad_signature[directory + 3] = 0x09
        self.assertRefused(bad_signature, "invalid ZIP directory member")
        overlong_name = bytearray(raw)
        struct.pack_into("<H", overlong_name, directory + 28, 999)
        self.assertRefused(overlong_name, "invalid ZIP directory extent")
        overcounted = bytearray(raw)
        struct.pack_into("<HH", overcounted, eocd + 8, 2, 2)
        self.assertRefused(overcounted, "member count is inconsistent")

    def test_members_sharing_a_local_header_are_refused(self) -> None:
        raw = bytearray(security.epub_bytes("one", "two"))
        second = raw.index(b"PK\x01\x02", raw.index(b"PK\x01\x02") + 1)
        struct.pack_into("<I", raw, second + 42, 0)
        self.assertRefused(raw, "share a local header")

    def test_the_member_budget_is_rechecked_after_parsing(self) -> None:
        with zipfile.ZipFile(io.BytesIO(security.epub_bytes("one", "two"))) as archive, \
                mock.patch.object(download, "MAX_EPUB_MEMBERS", 1):
            with self.assertRaisesRegex(download.DownloadError, "member count budget"):
                download._admit_epub_members(archive)

    def test_local_headers_must_match_their_directory_records(self) -> None:
        raw = security.epub_bytes("body")
        with download.open_epub(io.BytesIO(raw)) as archive:
            [info] = archive.infolist()
            info.header_offset = -1
            with self.assertRaisesRegex(download.DownloadError, "outside the supported byte budget"):
                download.read_epub_member(archive, info)
        bad_signature = bytearray(raw)
        bad_signature[3] = 0x09
        self.assertRefused(bad_signature, "invalid local header")
        undecodable = bytearray(raw)
        struct.pack_into("<H", undecodable, 6, 0x800)
        undecodable[30] = 0xFF
        self.assertRefused(undecodable, "invalid filename")
        renamed = bytearray(raw)
        renamed[30:31] = b"D"
        self.assertRefused(renamed, "metadata does not match its local data")

    def test_a_stored_member_must_declare_equal_lengths(self) -> None:
        content = b"<p>stored</p>"
        payload = bytearray(single_member(content, content, method=0))
        directory = payload.index(b"PK\x01\x02")
        struct.pack_into("<I", payload, directory + 20, len(content) - 1)
        self.assertRefused(payload, "stored member has inconsistent lengths")

    def test_member_data_cut_short_after_admission_is_refused(self) -> None:
        stream = io.BytesIO(security.epub_bytes("body"))
        with download.open_epub(stream) as archive:
            stream.truncate(30 + len(NAME))
            with self.assertRaisesRegex(download.DownloadError, "member data is truncated"):
                download.read_epub_member(archive, "document_1.xhtml")

    def test_deflate_streams_must_be_valid_and_end_exactly(self) -> None:
        content = b"<p>body</p>"
        self.assertRefused(single_member(deflated(content) + b"xx", content),
                           "trailing compressed data")
        self.assertRefused(single_member(b"\xff\xff\xff\xff", content), "invalid deflate data")


class EnvelopeAndSniffTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "response"

    def test_a_missing_file_sniffs_as_empty(self) -> None:
        self.assertEqual(download.sniff(self.path), b"")

    def test_unusable_envelopes_are_invalid(self) -> None:
        cases = {"not json": b"{not json", "a list": b"[1]"}
        for label, body in cases.items():
            with self.subTest(label=label):
                self.path.write_bytes(body)
                self.assertEqual(download.decode_envelope(self.path), {"_invalid": True})

    def test_envelope_size_limits_apply_before_and_after_decoding(self) -> None:
        with mock.patch.object(download, "MAX_EPUB_BYTES", 2):
            for body in ('{"bytes": "AAAAAAAA"}', '{"bytes": "AAAA"}'):
                with self.subTest(body=body):
                    self.path.write_text(body, encoding="utf-8")
                    self.assertEqual(download.decode_envelope(self.path), {"_invalid": True})
                    self.assertEqual(self.path.read_text(encoding="utf-8"), body)


class FetchTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.dst = str(Path(temporary.name) / "title.epub")
        self.part = self.dst + ".part"
        for patcher in (mock.patch.object(download, "_require_bounded_curl"),
                        mock.patch.object(download.time, "sleep")):
            patcher.start()
            self.addCleanup(patcher.stop)

    def fetch(self, transfers, tries=1):
        responses = list(transfers)

        def run(args, **_kwargs):
            body, status = responses.pop(0)
            if body is not None:
                Path(args[args.index("-o") + 1]).write_bytes(body)
            return SimpleNamespace(returncode=0, stdout=status)

        with mock.patch.object(download.subprocess, "run", side_effect=run):
            return download.fetch("https://example.test/epub", self.dst, tries=tries)

    def test_a_stale_partial_file_is_discarded_first(self) -> None:
        Path(self.part).write_bytes(b"left by an earlier run")
        payload = security.epub_bytes("body")
        self.assertTrue(self.fetch([(payload, "200|application/epub+zip")])[0])
        self.assertEqual(Path(self.dst).read_bytes(), payload)

    def test_a_success_without_a_body_is_refused(self) -> None:
        with self.assertRaisesRegex(download.DownloadError,
                                    "missing response body after 1 attempt "):
            self.fetch([(None, "200|application/epub+zip")])
        self.assertFalse(os.path.exists(self.part))

    def test_an_oversized_body_is_refused(self) -> None:
        with mock.patch.object(download, "MAX_DOWNLOAD_BYTES", 4):
            with self.assertRaisesRegex(download.DownloadError,
                                        "download exceeds the response budget"):
                self.fetch([(b"PK\x03\x04-too-long", "200|application/epub+zip")])

    def test_a_resume_with_no_budget_left_stops(self) -> None:
        with mock.patch.object(download, "MAX_DOWNLOAD_BYTES", 4):
            with self.assertRaisesRegex(download.DownloadError,
                                        "download exceeds the response budget after 2 attempts"):
                self.fetch([(b"PK\x03\x04", "500|text/html")], tries=2)
        self.assertFalse(os.path.exists(self.part))


class SnapshotTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.first, self.second = str(self.base / "a.epub"), str(self.base / "a.epub.meta.json")
        Path(self.first).write_text("epub", encoding="utf-8")
        Path(self.second).write_text("{}", encoding="utf-8")

    def test_an_unfinished_rollback_file_stops_a_new_snapshot(self) -> None:
        Path(self.first + ".rollback").write_text("earlier", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "unfinished rollback file"):
            download.snapshot_paths(self.first)

    def test_a_failed_backup_removes_the_backups_already_made(self) -> None:
        real_copy = download.shutil.copy2

        def copy_then_fail(source, destination):
            if source == self.second:
                raise OSError(28, "no space left")
            return real_copy(source, destination)

        with mock.patch.object(download.shutil, "copy2", side_effect=copy_then_fail):
            with self.assertRaisesRegex(OSError, "no space left"):
                download.snapshot_paths(self.first, self.second)
        self.assertEqual(sorted(p.name for p in self.base.iterdir()),
                         ["a.epub", "a.epub.meta.json"])

    def test_a_missing_backup_stops_the_rollback(self) -> None:
        snapshot = download.snapshot_paths(self.first)
        os.remove(self.first + ".rollback")
        with self.assertRaisesRegex(RuntimeError, "missing rollback file"):
            download.rollback_snapshots([snapshot])

    def test_backups_that_cannot_be_removed_are_reported(self) -> None:
        snapshot = download.snapshot_paths(self.first)
        with mock.patch.object(download.os, "remove", side_effect=PermissionError("locked")):
            self.assertEqual(download.discard_snapshots([snapshot]), [self.first + ".rollback"])


class MainTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.scratch = Path(temporary.name)
        self.epub_dir = self.scratch / "epub"
        self.epub_dir.mkdir()
        self.download = regressions.load_module("download_paths", regressions.STAGE / "download.py")
        self.download.SCRATCH = str(self.scratch)
        self.download.EPUB_DIR = str(self.epub_dir)
        self.download.CRAWL_DELAY = 0
        self.title = {"id": "C2099A00001", "name": "Synthetic Act", "versionStart": "2099-01-01",
                      "compilationNumber": "1", "compilationRegisterId": "C2099C00001"}

    def run_main(self, *titles: dict[str, object]) -> str:
        (self.scratch / "acts_resolved.json").write_text(json.dumps(list(titles)), encoding="utf-8")
        with contextlib.redirect_stdout(io.StringIO()):
            self.download.main()
        return (self.scratch / "download_log.txt").read_text(encoding="utf-8")

    def fake_fetch(self, _url: str, dst: str):
        payload = security.epub_bytes("body")
        Path(dst).write_bytes(payload)
        return True, "200", "application/epub+zip", len(payload), None

    def test_an_unreadable_sidecar_forces_a_fresh_download(self) -> None:
        (self.epub_dir / "C2099A00001.epub").write_bytes(security.epub_bytes("old"))
        (self.epub_dir / "C2099A00001.epub.meta.json").write_text("{broken", encoding="utf-8")
        self.download.fetch = mock.Mock(side_effect=self.fake_fetch)
        log = self.run_main(self.title)
        self.download.fetch.assert_called_once()
        self.assertIn("C2099A00001  OK", log)

    def test_a_title_without_a_document_drops_its_partial_file(self) -> None:
        partial = self.epub_dir / "C2099A00001.epub.part"
        partial.write_bytes(b"partial")
        self.download.fetch = mock.Mock(side_effect=AssertionError("must not fetch"))
        self.run_main(dict(self.title, compilationRegisterId=None))
        self.assertFalse(partial.exists())

    def test_retained_rollback_files_are_logged(self) -> None:
        self.download.fetch = mock.Mock(side_effect=self.fake_fetch)
        with mock.patch.object(self.download, "discard_snapshots", return_value=["kept"]):
            log = self.run_main(self.title)
        self.assertIn("WARNING retained 1 rollback file(s) after commit", log)

    def test_an_incomplete_rollback_is_reported_over_the_original_failure(self) -> None:
        rollback_error = OSError(5, "simulated rollback failure")
        self.download.fetch = mock.Mock(side_effect=self.download.DownloadError("HTTP 500"))
        with mock.patch.object(self.download, "rollback_snapshots", side_effect=rollback_error):
            with self.assertRaisesRegex(RuntimeError,
                                        "download failed and rollback was incomplete") as caught:
                self.run_main(self.title)
        self.assertIs(caught.exception.__cause__, rollback_error)


if __name__ == "__main__":
    unittest.main()
