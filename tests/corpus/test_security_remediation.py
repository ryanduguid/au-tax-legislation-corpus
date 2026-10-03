"""Bounded hostile inputs and failure recovery for the supplied security audit."""

from __future__ import annotations

import contextlib
import io
import json
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile
import zlib
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from fadden import corpus_paths, download, extract
from fadden import export_monitor_contract as contract
from fadden import export_publication_bundles as publisher

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


class PathAndWriteTests(unittest.TestCase):
    def test_full_fraction_grammar_is_portable_and_flat_fadden_roots_work(self):
        for fraction in ("", *("." + "1" * length for length in range(1, 8))):
            for offset in ("", "Z", "+10:00", "-00:59"):
                value = "2099-01-01T12:34:56" + fraction + offset
                with self.subTest(timestamp=value):
                    self.assertEqual(corpus_paths.version_date(value), "2099-01-01")
        with tempfile.TemporaryDirectory() as temporary:
            flat = Path(temporary) / "fadden"
            flat.mkdir()
            (flat / "sources.json").write_text("{}", encoding="utf-8")
            corpus_paths.require_builder_layout(flat / "check_current.py")

    def test_unpublished_title_preserves_an_unowned_fixed_temporary_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            epub = root / "epub"
            epub.mkdir()
            foreign = epub / "C2099A00001.epub.meta.json.tmp"
            foreign.write_bytes(b"unowned")
            (root / "acts_resolved.json").write_text(json.dumps([{
                "id": "C2099A00001", "name": "Synthetic unpublished title", "versionStart": "2099-01-01",
                "compilationRegisterId": None,
            }]), encoding="utf-8")
            with mock.patch.object(download, "SCRATCH", str(root)), \
                    mock.patch.object(download, "EPUB_DIR", str(epub)), \
                    mock.patch.object(download, "fetch") as fetch, contextlib.redirect_stdout(io.StringIO()):
                download.main()
            self.assertEqual(foreign.read_bytes(), b"unowned")
            fetch.assert_not_called()

    def test_network_stages_reject_filter_injection_before_requesting(self):
        for name in ("versions", "probe13", "check_current"):
            with self.subTest(stage=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                module = regressions.load_module("injection_" + name, STAGE / (name + ".py"))
                injected = "C2099A00001' or isCurrent eq true"
                if name == "versions":
                    module.SCRATCH = str(root)
                    (root / "titles_all.json").write_text(json.dumps([
                        {"id": injected, "name": "Synthetic", "isPrincipal": True}
                    ]), encoding="utf-8")
                elif name == "probe13":
                    module.SCRATCH = str(root)
                    (root / "manifest_raw.json").write_text(json.dumps([
                        {"id": injected, "name": "Synthetic", "epub": None}
                    ]), encoding="utf-8")
                else:
                    module.ROOT = str(root)
                    (root / "sources.json").write_text(json.dumps({
                        "retrieved": "2099-01-01", "titles": [{"register_id": injected}]
                    }), encoding="utf-8")
                with mock.patch.object(module, "fetch_json") as fetch, \
                        mock.patch.object(sys, "argv", [name]), contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(ValueError):
                        module.main()
                fetch.assert_not_called()

    def test_malformed_version_rows_cannot_replace_resolved_or_probe_manifests(self):
        for name in ("versions", "probe13"):
            for response in ({"value": 1}, {"value": ["wrong shape"]},
                             {"value": [{"titleId": "C2099A00002"}]},
                             {"value": [{"titleId": "C2099A00001", "start": "2099-02-30", "registerId": None}]},
                             {"value": [{"titleId": "C2099A00001", "start": "2099-01-01"}]}):
                with self.subTest(stage=name, response=response), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    module = regressions.load_module("bad_rows_" + name, STAGE / (name + ".py"))
                    module.SCRATCH = str(root)
                    input_name = "titles_all.json" if name == "versions" else "manifest_raw.json"
                    output_name = "acts_resolved.json" if name == "versions" else "probe13.json"
                    (root / input_name).write_text(json.dumps([{
                        "id": "C2099A00001", "name": "Synthetic", "isPrincipal": True, "epub": None
                    }]), encoding="utf-8")
                    output = root / output_name
                    output.write_text("previous manifest", encoding="utf-8")
                    with mock.patch.object(module, "fetch_json", return_value=response), \
                            mock.patch.object(module.time, "sleep"), contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaises((ValueError, RuntimeError)):
                            module.main()
                    self.assertEqual(output.read_text(encoding="utf-8"), "previous manifest")

    def test_bad_download_dates_never_reach_fetch_or_change_existing_documents(self):
        for name in ("download", "retry13"):
            with self.subTest(stage=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                module = regressions.load_module("date_url_" + name, STAGE / (name + ".py"))
                module.SCRATCH = str(root)
                module.EPUB_DIR = str(root / "epub")
                item = {"id": "C2099A00001", "name": "Synthetic", "epub": None,
                        "versionStart": "2099-01-01/../../unexpected", "compilationRegisterId": "C2099C00001"}
                (root / ("acts_resolved.json" if name == "download" else "manifest_raw.json")).write_text(
                    json.dumps([item]), encoding="utf-8")
                if name == "retry13":
                    (root / "probe13.json").write_text(json.dumps([{
                        "id": item["id"], "latest_doc": {"start": "2099-01-01/../../unexpected", "registerId": "C2099C00001"}
                    }]), encoding="utf-8")
                owner = module if name == "download" else module.dl
                with mock.patch.object(owner, "fetch") as fetch, contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(ValueError):
                        module.main()
                fetch.assert_not_called()
                self.assertEqual(list((root / "epub").glob("*")), [])

    def test_rate_rows_must_belong_to_their_directory_even_without_rate_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            module = regressions.load_module("rate_identity", STAGE / "rates.py")
            module.ROOT, module.OUT = str(root), str(root / "rates")
            title = root / "markdown" / "C2099A00001"
            title.mkdir(parents=True)
            (root / "sources.json").write_text(json.dumps({"titles": [{"register_id": title.name}]}), encoding="utf-8")
            (title / "sections.jsonl").write_text(json.dumps({"register_id": "C2099A00002", "text": "No rate here."}) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "does not match"):
                module.main()
            self.assertEqual(list((root / "rates").iterdir()), [])

    def test_identifiers_and_complete_dates_are_checked(self):
        for value in ("C2099A00001' or true", "C２０９９A00001", "../C2099A00001", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus_paths.register_id(value)
        for value in ("2099-02-30", "2099-01-01/../../", "2099-01-01T99:00:00Z",
                      "2099-01-01T00:00:00+00:99", "2099-01-01junk", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus_paths.version_date(value)
        self.assertEqual(corpus_paths.version_date("2099-01-01T00:00:00.1234567Z"), "2099-01-01")

    def test_atomic_writer_preserves_prior_bytes_and_unowned_temp(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manifest.json"
            path.write_text("previous\n", encoding="utf-8")
            foreign = Path(str(path) + ".tmp")
            foreign.write_text("foreign staging\n", encoding="utf-8")
            with self.assertRaises(KeyboardInterrupt):
                with corpus_paths.atomic_text_writer(path) as target:
                    target.write("partial replacement")
                    raise KeyboardInterrupt
            self.assertEqual(path.read_text(encoding="utf-8"), "previous\n")
            self.assertEqual(foreign.read_text(encoding="utf-8"), "foreign staging\n")
            self.assertEqual(set(path.parent.iterdir()), {path, foreign})
            with corpus_paths.atomic_text_writer(path) as target:
                target.write("complete replacement\n")
            self.assertEqual(path.read_text(encoding="utf-8"), "complete replacement\n")

    def test_installed_locations_refuse_legacy_stages_before_side_effects(self):
        for name in ("discover", "versions", "download", "probe13", "retry13", "extract",
                     "finalize", "rates", "check_current", "pii_scan", "pii_scan2", "dist"):
            module = regressions.load_module("installed_guard_" + name, STAGE / (name + ".py"))
            with self.subTest(stage=name), tempfile.TemporaryDirectory() as temporary, \
                    mock.patch.object(module, "__file__", str(Path(temporary) / "fadden" / (name + ".py"))):
                with self.assertRaisesRegex(RuntimeError, "source checkout"):
                    module.main("2026-10-01") if name == "finalize" else module.main()
                self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_clean_package_imports_ignore_bare_helper_modules(self):
        with tempfile.TemporaryDirectory() as temporary:
            hostile = Path(temporary)
            for name in ("corpus_paths", "download", "http_fetch", "pii_patterns", "rates", "dist_verify"):
                (hostile / (name + ".py")).write_text('raise AssertionError("bare helper imported")\n', encoding="utf-8")
            script = ("import sys, importlib; sys.path[:0] = " + repr([str(REPOSITORY), str(hostile)]) +
                      "; before = list(sys.path); from fadden import STAGES; import fadden.__main__; "
                      "[importlib.import_module('fadden.' + name) for name in STAGES]; "
                      "assert sys.path == before; assert 'corpus_paths' not in sys.modules")
            result = subprocess.run([sys.executable, "-I", "-c", script], cwd=temporary,
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)


class PublicationRecoveryTests(unittest.TestCase):
    def test_interrupt_before_rollback_iteration_preserves_backups_and_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destinations = {name: root / (name + ".json") for name in ("baseline", "observation")}
            staged = {name: root / (name + ".tmp") for name in destinations}
            for name in destinations:
                destinations[name].write_bytes(("old " + name).encode())
                staged[name].write_bytes(("new " + name).encode())
            replace = contract.os.replace

            def fail_last_promotion(source, destination):
                if Path(source) == staged["observation"]:
                    raise OSError("promotion failed")
                replace(source, destination)

            with mock.patch.object(contract.os, "replace", side_effect=fail_last_promotion), \
                    mock.patch.object(contract, "list", side_effect=KeyboardInterrupt("recovery interrupted"), create=True):
                with self.assertRaises(KeyboardInterrupt):
                    contract._publish(staged, destinations)
            self.assertTrue((root / contract.PUBLISH_LOCK_FILENAME).exists())
            self.assertEqual({path.read_bytes() for path in root.glob(".*.bak")},
                             {b"old baseline", b"old observation"})

    def test_backup_collision_preserves_a_dangling_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destinations = {name: root / (name + ".json") for name in ("baseline", "observation")}
            staged = {name: root / (name + ".tmp") for name in destinations}
            for name in destinations:
                destinations[name].write_bytes(("old " + name).encode())
                staged[name].write_bytes(("new " + name).encode())
            collision = root / ".baseline.json.monitor-contract-fixed.bak"
            try:
                collision.symlink_to(root / "missing")
            except OSError as exc:
                self.skipTest(f"file symlinks unavailable: {exc}")
            with mock.patch.object(contract.uuid, "uuid4", return_value=SimpleNamespace(hex="fixed")):
                with self.assertRaisesRegex(contract.ContractError, "already exists"):
                    contract._publish(staged, destinations)
            self.assertTrue(collision.is_symlink())
            self.assertEqual(destinations["baseline"].read_bytes(), b"old baseline")
            self.assertFalse((root / contract.PUBLISH_LOCK_FILENAME).exists())

    def test_producer_fractional_timestamp_grammar_is_portable(self):
        for length in range(1, 7):
            timestamp = "2099-01-01T12:34:56." + "1" * length + "Z"
            with self.subTest(timestamp=timestamp):
                self.assertEqual(contract._utc_timestamp(timestamp, "checked_at"), timestamp)

    def test_monitor_staging_collision_preserves_unowned_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "baseline.json"
            collision = destination.with_name(".baseline.json.monitor-contract-fixed.tmp")
            collision.write_bytes(b"unowned")
            with mock.patch.object(contract.uuid, "uuid4", return_value=SimpleNamespace(hex="fixed")):
                with self.assertRaises(FileExistsError):
                    contract._write_staged(destination, {})
            self.assertEqual(collision.read_bytes(), b"unowned")

    def test_monitor_interruptions_after_each_rename_restore_the_old_pair(self):
        replace = contract.os.replace
        for interrupt_at in range(1, 5):
            with self.subTest(rename=interrupt_at), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                destinations = {name: root / (name + ".json") for name in ("baseline", "observation")}
                staged = {name: root / (name + ".tmp") for name in destinations}
                for name in destinations:
                    destinations[name].write_bytes(("old " + name).encode())
                    staged[name].write_bytes(("new " + name).encode())
                calls = 0
                interrupted = False

                def interrupt_after_replace(source, destination):
                    nonlocal calls, interrupted
                    replace(source, destination)
                    if not interrupted:
                        calls += 1
                        if calls == interrupt_at:
                            interrupted = True
                            raise KeyboardInterrupt("interrupted after a completed rename")

                with mock.patch.object(contract.os, "replace", side_effect=interrupt_after_replace):
                    with self.assertRaises(KeyboardInterrupt):
                        contract._publish(staged, destinations)
                for name in destinations:
                    self.assertEqual(destinations[name].read_bytes(), ("old " + name).encode())
                self.assertFalse((root / contract.PUBLISH_LOCK_FILENAME).exists())
                self.assertEqual(list(root.glob(".*.bak")), [])

    def test_empty_destination_race_is_refused_by_the_promotion_primitive(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "bundles"
            promote = publisher._promote_no_replace
            identity = []

            def competitor_appears(staging, destination):
                destination.mkdir()
                identity.append(destination.stat().st_ino)
                promote(staging, destination)

            with mock.patch.object(publisher, "_promote_no_replace", side_effect=competitor_appears):
                with self.assertRaisesRegex(publisher.PublicationBundleError, "could not be promoted"):
                    publisher.export_publication_bundles(PUBLICATION / "sample-sources.json",
                                                        PUBLICATION / "sample-observation-facts-v3.json", output)
            self.assertEqual(output.stat().st_ino, identity[0])
            self.assertEqual(list(output.iterdir()), [])
            self.assertEqual(list(output.parent.glob(".*.tmp")), [])

    def test_installed_producer_version_comes_from_distribution_metadata(self):
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(publisher, "VERSION_PATH", Path(temporary) / "absent"), \
                mock.patch.object(publisher, "version", return_value="0.1.7") as installed_version:
            self.assertEqual(publisher._producer_version(), "0.1.7")
        installed_version.assert_called_once_with("tax-radar-au")

    def test_publisher_url_rejects_other_hosts_ports_and_suffixes(self):
        path = "/C2099A00001/latest/text"
        for url in ("https://example.test" + path, "https://www.legislation.gov.au:444" + path,
                    "https://www.legislation.gov.au" + path + "?redirect=other",
                    "https://www.legislation.gov.au" + path + "#other"):
            with self.subTest(url=url), self.assertRaises(publisher.PublicationBundleError):
                publisher._publisher_https_url(url, "canonical URL", expected_path=path)
        self.assertEqual(publisher._publisher_https_url("https://www.legislation.gov.au" + path,
                         "canonical URL", expected_path=path), "https://www.legislation.gov.au" + path)

    def test_reparse_staging_is_not_recursively_removed(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            staging = parent / ".output.publication-bundles-test.tmp"
            staging.mkdir()
            details = SimpleNamespace(st_mode=stat.S_IFDIR,
                                      st_file_attributes=getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))
            with mock.patch.object(publisher.os, "lstat", return_value=details), \
                    mock.patch.object(publisher.shutil, "rmtree") as remove:
                with self.assertRaises(publisher.PublicationBundleError):
                    publisher._remove_owned_staging(staging, parent=parent, prefix=".output.publication-bundles-")
            remove.assert_not_called()

    def test_staging_collision_preserves_the_existing_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            staging = root / ".output.publication-bundles-fixed.tmp"
            staging.mkdir()
            marker = staging / "unrelated.txt"
            marker.write_text("keep", encoding="utf-8")
            with mock.patch.object(publisher.uuid, "uuid4", return_value=SimpleNamespace(hex="fixed")):
                with self.assertRaisesRegex(publisher.PublicationBundleError, "could not be written"):
                    publisher.export_publication_bundles(PUBLICATION / "sample-sources.json",
                        PUBLICATION / "sample-observation-facts-v3.json", root / "output")
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep")

    def test_cleanup_failure_keeps_the_primary_failure(self):
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(publisher, "_write_bundle", side_effect=OSError("primary write failure")), \
                mock.patch.object(publisher, "_remove_owned_staging", side_effect=publisher.PublicationBundleError("cleanup failure")):
            with self.assertRaisesRegex(publisher.PublicationBundleError, "primary write failure") as caught:
                publisher.export_publication_bundles(PUBLICATION / "sample-sources.json",
                    PUBLICATION / "sample-observation-facts-v3.json", Path(temporary) / "output")
            self.assertIn("cleanup failure", str(caught.exception.__cause__))

    def test_owned_lock_cleanup_failure_is_reported(self):
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.object(contract, "_remove", side_effect=OSError("unlink failed")), \
                    self.assertRaisesRegex(contract.ContractError, "lock could not be removed"):
                with contract._OutputDirectoryLock(Path(temporary)):
                    pass


if __name__ == "__main__":
    unittest.main()
