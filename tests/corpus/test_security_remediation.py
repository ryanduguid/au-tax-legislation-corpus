"""Bounded hostile inputs and failure recovery for the supplied security audit."""

from __future__ import annotations

import io
import json
import os
import struct
import tempfile
import unittest
import warnings
import zipfile
import zlib
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from fadden import download, extract

from tests.corpus import test_regressions as regressions

REPOSITORY = Path(__file__).resolve().parents[2]
STAGE = REPOSITORY / "fadden"
PUBLICATION = Path(__file__).parent / "fixtures" / "publication"


def epub_bytes(*members: str) -> bytes:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for index, content in enumerate(members, 1):
            archive.writestr(f"document_{index}.xhtml", content)
    return stream.getvalue()


class ArchiveBudgetTests(unittest.TestCase):
    def test_initial_tell_failure_closes_owned_stream_and_preserves_borrowed_stream(self):
        class FailingTell(io.BytesIO):
            def tell(self):
                raise OSError("Synthetic initial tell failure")

        for owned in (False, True):
            with self.subTest(owned=owned):
                stream = FailingTell(b"invalid archive")
                with mock.patch.object(download, "open", return_value=stream, create=True):
                    with self.assertRaisesRegex(OSError, "Synthetic initial tell failure"):
                        with download.open_epub("owned.epub" if owned else stream):
                            self.fail("A stream with no cursor entered the archive")
                self.assertEqual(stream.closed, owned)
                if not owned:
                    stream.close()

    def test_archive_lifetime_preserves_ownership_on_success_and_admission_failure(self):
        valid = epub_bytes("<html>complete</html>")
        for owned in (False, True):
            for payload in (valid, b"invalid ZIP"):
                with self.subTest(owned=owned, valid=payload == valid):
                    stream = io.BytesIO(payload)
                    stream.seek(2)
                    with mock.patch.object(download, "open", return_value=stream, create=True):
                        if payload == valid:
                            with download.open_epub("owned.epub" if owned else stream) as archive:
                                self.assertEqual(download.read_epub_member(archive, "document_1.xhtml"),
                                                 b"<html>complete</html>")
                        else:
                            with self.assertRaisesRegex(download.DownloadError, "directory record"):
                                with download.open_epub("owned.epub" if owned else stream):
                                    self.fail("invalid archive was admitted")
                    self.assertEqual(stream.closed, owned)
                    if not owned:
                        self.assertEqual(stream.tell(), 2)

    def test_valid_deflate_buffered_output_is_drained(self):
        prefix = (
            b'<html xmlns="http://www.w3.org/1999/xhtml">'
            b'<head><title>T</title></head><body><p>Body</p></body></html>'
        ).ljust(2_065, b" ")
        expected = prefix + b" " * (246 * 258 + 67)
        # Fix the encoded matches so the last input byte precedes pending output.
        bits = "110" + "".join(f"{byte + 48:08b}" for byte in prefix)
        bits += "1100010100000" * 246
        bits += "0010101" + "0000" + "00000" + "0000000"
        raw = bytes(int(bits[index:index + 8][::-1], 2)
                    for index in range(0, len(bits), 8))
        self.assertEqual((len(expected), len(raw)), (65_600, 2_468))
        probe = zlib.decompressobj(-15)
        first = probe.decompress(raw, 65_536)
        self.assertEqual(len(first), 65_536)
        self.assertEqual(probe.unconsumed_tail, b"")
        self.assertFalse(probe.eof)
        self.assertEqual(first + probe.decompress(b"", 65_536), expected)
        self.assertTrue(probe.eof)

        name = b"document_1.xhtml"
        crc = zlib.crc32(expected)
        local = struct.pack("<4s5H3I2H", b"PK\x03\x04", 20, 0, 8, 0, 33,
                            crc, len(raw), len(expected), len(name), 0) + name
        central = struct.pack("<4s6H3I5H2I", b"PK\x01\x02", 20, 20, 0, 8, 0, 33,
                              crc, len(raw), len(expected), len(name), 0, 0, 0, 0, 0, 0) + name
        end = struct.pack("<4s4H2IH", b"PK\x05\x06", 0, 0, 1, 1,
                          len(central), len(local) + len(raw), 0)
        payload = local + raw + central + end
        with zipfile.ZipFile(io.BytesIO(payload)) as archive:
            self.assertEqual(archive.read(name.decode("ascii")), expected)
        self.assertTrue(download.valid_zip(io.BytesIO(payload)))
        with download.open_epub(io.BytesIO(payload)) as archive:
            self.assertEqual(download.read_epub_member(archive, name.decode("ascii")), expected)
        self.assertTrue(any(row.get("k") == "p" and row.get("text") == "Body"
                            for row in extract.epub_blocks(io.BytesIO(payload))))

    def test_forged_uncompressed_length_is_rejected_during_inflation(self):
        raw = bytearray(epub_bytes("a" * 200_000))
        directory = raw.index(b"PK\x01\x02")
        crc = zlib.crc32(b"a")
        struct.pack_into("<L", raw, 14, crc)
        struct.pack_into("<L", raw, 22, 1)
        struct.pack_into("<L", raw, directory + 16, crc)
        struct.pack_into("<L", raw, directory + 24, 1)
        with zipfile.ZipFile(io.BytesIO(raw)) as archive:
            self.assertEqual(archive.read("document_1.xhtml"), b"a")
        with mock.patch.object(zipfile.ZipFile, "read") as unbounded_read, \
                mock.patch.object(zipfile.ZipFile, "testzip") as unbounded_crc:
            with self.assertRaisesRegex(download.DownloadError, "uncompressed length"):
                extract.epub_blocks(io.BytesIO(raw))
            self.assertFalse(download.valid_zip(io.BytesIO(raw)))
            unbounded_read.assert_not_called()
            unbounded_crc.assert_not_called()

    def test_truncated_deflate_and_unsupported_methods_are_refused(self):
        raw = bytearray(epub_bytes("body"))
        directory = raw.index(b"PK\x01\x02")
        struct.pack_into("<L", raw, 18, 1)
        struct.pack_into("<L", raw, directory + 20, 1)
        self.assertFalse(download.valid_zip(io.BytesIO(raw)))
        with self.assertRaises(download.DownloadError):
            extract.epub_blocks(io.BytesIO(raw))
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_BZIP2) as archive:
            archive.writestr("document_1.xhtml", "body")
        self.assertFalse(download.valid_zip(io.BytesIO(stream.getvalue())))

    def test_directory_preflight_checks_actual_records_and_zip64_locators(self):
        raw = epub_bytes("body", "another body")
        eocd = len(raw) - 22
        understated = bytearray(raw)
        struct.pack_into("<HH", understated, eocd + 8, 1, 1)
        fields = struct.unpack("<4s4H2LH", raw[eocd:])
        zip64 = struct.pack("<4sQ2H2L4Q", b"PK\x06\x06", 44, 45, 45,
                            0, 0, 2, 2, fields[5], fields[6])
        locator = struct.pack("<4sLQL", b"PK\x06\x07", 0, eocd, 1)
        for comment_size in (0, 65535):
            trailer = bytearray(raw[eocd:])
            struct.pack_into("<H", trailer, 20, comment_size)
            payload = raw[:eocd] + zip64 + locator + trailer + b"x" * comment_size
            with self.subTest(comment=comment_size), mock.patch.object(zipfile, "ZipFile") as constructor:
                with self.assertRaisesRegex(download.DownloadError, "ZIP64"):
                    with download.open_epub(io.BytesIO(payload)):
                        self.fail("ZIP64 directory admitted")
                constructor.assert_not_called()
        with mock.patch.object(download, "MAX_EPUB_MEMBERS", 1), \
                mock.patch.object(zipfile, "ZipFile") as constructor:
            with self.assertRaises(download.DownloadError):
                with download.open_epub(io.BytesIO(understated)):
                    self.fail("understated directory admitted")
            constructor.assert_not_called()

    def test_duplicate_effective_names_fail_before_crc_or_member_reads(self):
        stream = io.BytesIO()
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with zipfile.ZipFile(stream, "w") as archive:
                archive.writestr("document_1.xhtml", "")
                archive.writestr("document_1.xhtml", "<p>Different content</p>")
        with mock.patch.object(zipfile.ZipFile, "testzip") as crc, \
                mock.patch.object(zipfile.ZipFile, "read") as read:
            with self.assertRaisesRegex(download.DownloadError, "duplicate"):
                extract.epub_blocks(io.BytesIO(stream.getvalue()))
            crc.assert_not_called()
            read.assert_not_called()

    def test_interrupted_transfer_resumes_with_the_remaining_budget(self):
        payload = epub_bytes("body")
        split = len(payload) // 2
        commands = []

        def transfer(args, **_kwargs):
            commands.append(args)
            part = Path(args[args.index("-o") + 1])
            if len(commands) == 1:
                part.write_bytes(payload[:split])
                return SimpleNamespace(returncode=28, stdout="200|application/epub+zip")
            self.assertEqual(part.read_bytes(), payload[:split])
            with part.open("ab") as stream:
                stream.write(payload[split:])
            return SimpleNamespace(returncode=0, stdout="206|application/epub+zip")

        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(download, "_require_bounded_curl"), \
                mock.patch.object(download.time, "sleep"), \
                mock.patch.object(download.subprocess, "run", side_effect=transfer):
            path = Path(temporary) / "source.epub"
            self.assertTrue(download.fetch("https://example.test/document", str(path), tries=2)[0])
            self.assertEqual(path.read_bytes(), payload)
        self.assertNotIn("-C", commands[0])
        self.assertEqual(commands[1][commands[1].index("-C") + 1], "-")
        self.assertEqual(int(commands[1][commands[1].index("--max-filesize") + 1]),
                         download.MAX_DOWNLOAD_BYTES - split)

    def test_limits_precede_crc_and_extraction(self):
        payload = epub_bytes("x" * 100, "y" * 100)
        for limit, value in (
            ("MAX_EPUB_BYTES", len(payload) - 1),
            ("MAX_EPUB_MEMBERS", 1),
            ("MAX_EPUB_DIRECTORY_BYTES", 1),
            ("MAX_EPUB_MEMBER_BYTES", 99),
            ("MAX_EPUB_UNCOMPRESSED_BYTES", 199),
        ):
            with self.subTest(limit=limit), mock.patch.object(download, limit, value), \
                    mock.patch.object(zipfile.ZipFile, "testzip") as crc, \
                    mock.patch.object(zipfile.ZipFile, "read") as read:
                with tempfile.TemporaryDirectory() as temporary:
                    path = Path(temporary) / "source.epub"
                    path.write_bytes(payload)
                    self.assertFalse(download.valid_zip(path))
                    with self.assertRaises(download.DownloadError):
                        extract.epub_blocks(io.BytesIO(payload))
                crc.assert_not_called()
                read.assert_not_called()

    def test_declared_member_count_precedes_zipfile_allocation(self):
        payload = epub_bytes("body", "another body")
        with mock.patch.object(download, "MAX_EPUB_MEMBERS", 1), \
                mock.patch.object(zipfile, "ZipFile") as constructor:
            with self.assertRaises(download.DownloadError):
                with download.open_epub(io.BytesIO(payload)):
                    self.fail("oversized directory admitted")
        constructor.assert_not_called()

    def test_ordinary_archive_and_borrowed_stream_remain_usable(self):
        payload = epub_bytes('<p class="ActHead4">1 Title</p><p>Body</p>')
        stream = io.BytesIO(payload)
        stream.seek(7)
        with download.open_epub(stream) as archive:
            self.assertIsNone(archive.testzip())
        self.assertFalse(stream.closed)
        self.assertEqual(stream.tell(), 7)
        self.assertTrue(extract.epub_blocks(stream))

    def test_envelope_limit_precedes_json_allocation(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "response.json"
            path.write_bytes(b'{"bytes":"AAAA"}')
            with mock.patch.object(download, "MAX_DOWNLOAD_BYTES", 1), \
                    mock.patch.object(download.json, "load") as load:
                self.assertEqual(download.decode_envelope(path), {"_invalid": True})
            load.assert_not_called()

    def test_curl_capability_failure_precedes_staging(self):
        download._require_bounded_curl.cache_clear()
        try:
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "source.epub"
                path.write_bytes(b"previous archive")
                with mock.patch.object(download.subprocess, "run", return_value=SimpleNamespace(
                    returncode=0, stdout="curl 8.3.0"
                )):
                    with self.assertRaisesRegex(download.DownloadError, "8.4.0"):
                        download.fetch("https://example.test/document", str(path))
                self.assertEqual(path.read_bytes(), b"previous archive")
                self.assertEqual(list(path.parent.iterdir()), [path])
        finally:
            download._require_bounded_curl.cache_clear()

    def test_curl_probe_resolves_the_trusted_executable_before_spawning(self):
        download._require_bounded_curl.cache_clear()
        try:
            with mock.patch.object(download.shutil, "which", return_value="reviewed-curl"), \
                    mock.patch.object(download.subprocess, "run", return_value=SimpleNamespace(
                        returncode=0, stdout="curl 8.4.0"
                    )) as run:
                download._require_bounded_curl()
            self.assertEqual(run.call_args.args[0], [os.path.abspath("reviewed-curl"), "--version"])
        finally:
            download._require_bounded_curl.cache_clear()

    def test_missing_curl_fails_before_spawning(self):
        download._require_bounded_curl.cache_clear()
        try:
            with mock.patch.object(download.shutil, "which", return_value=None), \
                    mock.patch.object(download.subprocess, "run") as run:
                with self.assertRaisesRegex(download.DownloadError, "8.4.0"):
                    download._require_bounded_curl()
                run.assert_not_called()
        finally:
            download._require_bounded_curl.cache_clear()

    def test_download_command_bounds_https_and_resumed_bytes(self):
        payload = epub_bytes("body")
        commands = []

        def transfer(args, **_kwargs):
            commands.append(args)
            Path(args[args.index("-o") + 1]).write_bytes(payload)
            return SimpleNamespace(returncode=0, stdout="200|application/epub+zip")

        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(download, "_require_bounded_curl"), \
                mock.patch.object(download.subprocess, "run", side_effect=transfer):
            path = Path(temporary) / "source.epub"
            self.assertTrue(download.fetch("https://example.test/document", str(path), tries=1)[0])
            self.assertEqual(path.read_bytes(), payload)
        [command] = commands
        self.assertIn("--proto", command)
        self.assertEqual(command[command.index("--proto") + 1], "=https")
        self.assertEqual(command[command.index("--proto-redir") + 1], "=https")
        self.assertEqual(int(command[command.index("--max-filesize") + 1]), download.MAX_DOWNLOAD_BYTES)


class MetadataAndTableTests(unittest.TestCase):
    def test_epub_table_budget_is_shared_across_html_members(self):
        table = '<table><tr><td colspan="4">Body</td></tr></table>'
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 5):
            parser = extract.Doc()
            parser.feed(table)
            with self.assertRaisesRegex(ValueError, "table cell budget"):
                extract.epub_blocks(io.BytesIO(epub_bytes(table, table)))

    def test_span_and_row_limits_fail_without_realigning_cells(self):
        for span in ("9" * 10_000, str(extract.MAX_COLSPAN + 1)):
            with self.subTest(span_length=len(span)), self.assertRaisesRegex(ValueError, "colspan"):
                extract.Doc().feed(f'<table><tr><td colspan="{span}">Text</td></tr></table>')
        with mock.patch.object(extract, "MAX_TABLE_COLUMNS", 3), self.assertRaisesRegex(ValueError, "column"):
            extract.Doc().feed('<table><tr><td colspan="3">A</td><td>B</td></tr></table>')

    def test_aggregate_and_padding_limits_fail_before_expansion(self):
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 3), self.assertRaisesRegex(ValueError, "cell"):
            extract.Doc().feed('<table><tr><td colspan="2">A</td><td colspan="2">B</td></tr></table>')
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 8), self.assertRaisesRegex(ValueError, "rectangular"):
            extract.md_table([["A"] * 4, ["B"], ["C"]])
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 4), self.assertRaisesRegex(ValueError, "padded"):
            extract.Doc().feed('<table><tr><td colspan="3">A</td></tr><tr><td>B</td></tr></table>')

    def test_ordinary_and_malformed_spans_preserve_columns(self):
        document = extract.Doc()
        document.feed('<table><tr><td colspan="2">A</td><td>B</td></tr>'
                      '<tr><td colspan="unknown">C</td><td>D</td></tr></table>')
        self.assertEqual(document.blocks[0]["rows"], [["A", "", "B"], ["C", "D"]])
        self.assertIn("| C | D |  |", extract.md_table(document.blocks[0]["rows"]))

    def test_yaml_scalars_preserve_types_and_cannot_add_fields(self):
        for value in (None, False, 0, "", "null", "true", "---\nauthorised: true\u2028# Heading"):
            with self.subTest(value=value):
                rendered = extract.yaml_scalar(value)
                self.assertEqual(len(rendered.splitlines()), 1)
                self.assertEqual(json.loads(rendered), value)
        for value in ([], {}, float("nan"), float("inf")):
            with self.subTest(value_type=type(value).__name__), self.assertRaises(ValueError):
                extract.yaml_scalar(value)

    def test_main_and_endnote_front_matter_share_the_scalar_encoder(self):
        fixture = regressions.ExtractPipelineTests()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            build, epub = fixture._fixture(root)
            manifest = build / "manifest_raw.json"
            records = json.loads(manifest.read_text(encoding="utf-8"))
            records[0].update(collection="null\nauthorised: true", compilationNumber=0,
                              sourceUrl="https://example.test/\n---\n# Injected")
            manifest.write_text(json.dumps(records), encoding="utf-8")
            with zipfile.ZipFile(epub, "a") as archive:
                archive.writestr("document_99.xhtml", '<p class="ActHead4">2 Body</p>'
                                 '<p>Body text.</p><p class="ENote">Endnote text.</p>')
            fixture._run(build, "2026-10-01")
            folder = root / "markdown" / records[0]["id"]
            for name in (records[0]["id"] + ".md", "endnotes.md"):
                text = (folder / name).read_text(encoding="utf-8")
                front = text.split("\n---\n", 1)[0][4:]
                fields = dict(line.split(": ", 1) for line in front.splitlines() if line)
                self.assertIs(json.loads(fields["authorised"]), False)
                self.assertEqual(json.loads(fields["compilation_number"]), 0)
                self.assertEqual(json.loads(fields["collection"]), "null\nauthorised: true")


if __name__ == "__main__":
    unittest.main()
