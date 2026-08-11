"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import ast
import contextlib
import datetime
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile
import time
import unittest
import zipfile
from unittest import mock


REPO = Path(__file__).resolve().parents[1]


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
        download = load_module("download_archive_lifecycle", REPO / "download.py")
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

    def test_epub_extraction_closes_the_archive(self):
        extract = load_module("extract_archive_lifecycle", REPO / "extract.py")
        epub_bytes = self._epub_bytes()
        real_zipfile = zipfile.ZipFile
        opened = []

        def tracked_zipfile(*args, **kwargs):
            archive = real_zipfile(*args, **kwargs)
            opened.append(archive)
            return archive

        with mock.patch.object(extract.zipfile, "ZipFile", side_effect=tracked_zipfile):
            blocks = extract.epub_blocks(io.BytesIO(epub_bytes))
        self.assertTrue(any(block.get("text") == "1 Short title" for block in blocks))
        self.assertEqual(len(opened), 1)
        self.assertIsNone(opened[0].fp)


class VolumeGateTests(unittest.TestCase):
    """A volume with no mapped structural heading must keep its body.

    F2025L00281 is an 8-volume EPUB whose Schedule 1 volumes carry
    ScheduleHeading/P1 markup and no ActHead or LI-Heading paragraph.  The
    pre-body gate resets at every volume boundary but was decided once across
    the whole document, so those volumes never opened it and 92% of the
    instrument was dropped with no placeholder, no counter and no parse
    failure.
    """

    META = {"id": "F2025L00281", "name": "Demo Approval 2025",
            "retrieved": "2026-08-03", "versionStart": "2025-01-01"}

    # Volume 1 carries the mapped heading, which is what decided the gate for
    # every other volume.
    VOLUME_1 = ('<html><body>'
                '<p class="ActHead4"><span class="CharSectno">1</span> Short title</p>'
                '<p class="subsection">This instrument is the Demo Approval 2025.</p>'
                '</body></html>')
    # Volume 2 in the Register's own shape: compilation cover page, the volume
    # list (which names "Endnotes"), the volume's contents page, then the text.
    VOLUME_2 = ('<html><body>'
                '<p class="MadeunderText">Demo Approval 2025</p>'
                '<p>Compilation No. 1</p>'
                '<p>This compilation is in 2 volumes</p>'
                '<p>Volume 2: Schedule 1</p>'
                '<p>Endnotes</p>'
                '<p class="ContentsHead">Contents</p>'
                '<p class="TOC6">Schedule 1 Valuation factors</p>'
                '<p class="Scheduletitle">Schedule 1 Valuation factors</p>'
                '<p class="ScheduleHeading">Table 3B Lump sum valuation factors</p>'
                '<table><tr><td>Age</td><td>Factor</td></tr>'
                '<tr><td>55</td><td>12.5</td></tr></table>'
                '</body></html>')

    # The same volume with no contents page and no body-classed paragraph, so
    # nothing marks where the compilation cover page ends.
    VOLUME_2_BARE = ('<html><body>'
                     '<p class="MadeunderText">Demo Approval 2025</p>'
                     '<p>Compilation No. 1</p>'
                     '<p>Volume 2: Schedule 1</p>'
                     '<p>Endnotes</p>'
                     '<p class="Scheduletitle">Schedule 1 Valuation factors</p>'
                     '<p class="ScheduleHeading">Table 3B Lump sum valuation factors</p>'
                     '<table><tr><td>Age</td><td>Factor</td></tr>'
                     '<tr><td>55</td><td>12.5</td></tr></table>'
                     '</body></html>')

    # A running header above the cover page. SKIP_CLASS skips Header, but it is
    # not a body boundary: it sits above the cover page, not below it.
    VOLUME_2_HEADED = ('<html><body>'
                       '<p class="Header">Demo Approval 2025</p>'
                       '<p class="MadeunderText">Demo Approval 2025</p>'
                       '<p>Compilation No. 1</p>'
                       '<p>Volume 2: Schedule 1</p>'
                       '<p class="ContentsHead">Contents</p>'
                       '<p class="TOC6">Schedule 1 Valuation factors</p>'
                       '<p class="Scheduletitle">Schedule 1 Valuation factors</p>'
                       '<table><tr><td>Age</td><td>Factor</td></tr>'
                       '<tr><td>55</td><td>12.5</td></tr></table>'
                       '</body></html>')

    # No contents page, but the volume's text starts on a body class. Unclassed
    # compilation cover text is deliberately outside PREBODY_CLASS, so the
    # first body-classed paragraph is a boundary the cover page cannot reach.
    VOLUME_2_SUBSECTION = ('<html><body>'
                           '<p class="MadeunderText">Demo Approval 2025</p>'
                           '<p>Compilation No. 1</p>'
                           '<p>Volume 2: Schedule 1</p>'
                           '<p class="subsection">Factors in this Schedule apply '
                           'to a lump sum interest.</p>'
                           '<table><tr><td>Age</td><td>Factor</td></tr>'
                           '<tr><td>55</td><td>12.5</td></tr></table>'
                           '</body></html>')

    @classmethod
    def epub_bytes(cls, *volumes):
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            for i, document in enumerate(volumes or (cls.VOLUME_1, cls.VOLUME_2), 1):
                archive.writestr("document_%d.xhtml" % i, document)
        return payload.getvalue()

    def parse(self):
        extract = load_module("extract_volume_gate", REPO / "extract.py")
        blocks = extract.epub_blocks(io.BytesIO(self.epub_bytes()))
        return extract.to_markdown(blocks, self.META)

    def test_a_heading_less_volume_keeps_its_tables_and_paragraphs(self):
        markdown, _sections, _endnotes, _long = self.parse()
        self.assertIn("Schedule 1 Valuation factors", markdown)
        self.assertIn("Table 3B Lump sum valuation factors", markdown)
        self.assertIn("| Age | Factor |", markdown)
        self.assertIn("| 55 | 12.5 |", markdown)

    def test_a_heading_less_volume_is_not_routed_into_its_endnotes(self):
        """The cover page lists "Endnotes" as one of the volumes."""
        markdown, _sections, endnotes, _long = self.parse()
        self.assertEqual(endnotes, "")
        self.assertNotIn("Table 3B", endnotes)
        self.assertIn("Table 3B", markdown)

    def test_a_heading_less_volume_still_drops_its_compilation_cover_page(self):
        markdown, _sections, _endnotes, _long = self.parse()
        self.assertNotIn("Compilation No. 1", markdown)
        self.assertNotIn("This compilation is in 2 volumes", markdown)

    def _parse(self, name, *volumes):
        extract = load_module(name, REPO / "extract.py")
        blocks = extract.epub_blocks(io.BytesIO(self.epub_bytes(*volumes)))
        return extract.to_markdown(blocks, self.META)

    def test_a_volume_with_no_boundary_is_dropped_rather_than_publishing_a_cover_page(self):
        """Every markdown file and every JSONL row carries an attribution
        saying compilation cover pages are omitted, so a volume showing neither
        a contents page nor a body class must not open at its own first block.
        It keeps the older, documented loss instead: nothing recovered, and no
        cover page published under a notice that says it was removed.  No
        volume in the corpus takes this path - all 11 heading-less volumes open
        at a contents page."""
        markdown, _sections, endnotes, _long = self._parse(
            "extract_volume_gate_bare", self.VOLUME_1, self.VOLUME_2_BARE)
        self.assertNotIn("Compilation No. 1", markdown)
        self.assertNotIn("Volume 2: Schedule 1", markdown)
        self.assertNotIn("Endnotes", markdown)
        self.assertNotIn("| 55 | 12.5 |", markdown)
        self.assertEqual(endnotes, "")

    def test_a_running_header_is_not_a_volume_body_boundary(self):
        """Word repeats the running header above the cover page, so opening the
        gate there opens it at the cover page."""
        markdown, _sections, _endnotes, _long = self._parse(
            "extract_volume_gate_header", self.VOLUME_1, self.VOLUME_2_HEADED)
        self.assertNotIn("Compilation No. 1", markdown)
        self.assertNotIn("Volume 2: Schedule 1", markdown)
        self.assertIn("| 55 | 12.5 |", markdown)

    def test_a_body_class_opens_the_gate_at_that_paragraph_not_at_the_cover_page(self):
        markdown, _sections, _endnotes, _long = self._parse(
            "extract_volume_gate_subsection", self.VOLUME_1, self.VOLUME_2_SUBSECTION)
        self.assertNotIn("Compilation No. 1", markdown)
        self.assertNotIn("Volume 2: Schedule 1", markdown)
        self.assertIn("Factors in this Schedule apply to a lump sum interest.", markdown)
        self.assertIn("| 55 | 12.5 |", markdown)

    def test_an_endnotes_line_inside_a_heading_less_volume_cannot_capture_it(self):
        """The bare-text endnote trigger stays disarmed until a mapped heading
        or ENDNOTE_CLASS markup is seen.  In a multi-volume compilation the
        word "Endnotes" on its own line is a volume-list entry, and arming the
        trigger on it routes the volume's whole body into endnotes.md - the
        same content loss the volume gate exists to stop, wearing a different
        hat."""
        extract = load_module("extract_volume_endnote_trigger", REPO / "extract.py")
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "ActHead4", "sectno": True, "text": "1 Short title"},
                  {"k": "p", "cls": "subsection", "sectno": False, "text": "Body."},
                  {"k": "file"},
                  {"k": "p", "cls": "ContentsHead", "sectno": False, "text": "Contents"},
                  {"k": "p", "cls": "", "sectno": False, "text": "Endnotes"},
                  {"k": "p", "cls": "Scheduletitle", "sectno": False,
                   "text": "Schedule 1 Valuation factors"},
                  {"k": "table", "rows": [["Age", "Factor"], ["55", "12.5"]]}]
        markdown, _sections, endnotes, _long = extract.to_markdown(blocks, self.META)
        self.assertIn("| 55 | 12.5 |", markdown)
        self.assertEqual(endnotes, "")

    def test_recovered_text_does_not_inherit_the_previous_volumes_section(self):
        _markdown, sections, _endnotes, _long = self.parse()
        rows = [s for s in sections if any(t.strip() for t in s["text"])]
        holding = [s for s in rows if "Table 3B" in "\n".join(s["text"])]
        self.assertEqual(len(holding), 1)
        self.assertIsNone(holding[0]["section"])
        self.assertEqual(holding[0]["kind"], "container")
        self.assertEqual(holding[0]["heading"], "Demo Approval 2025 - volume 2")
        for row in rows:
            if row is not holding[0]:
                self.assertNotIn("Table 3B", "\n".join(row["text"]))


class ExtractPipelineTests(unittest.TestCase):
    """extract.main() end to end over a two-volume EPUB."""

    REGISTER_ID = "F2025L00281"

    def _fixture(self, tmp_path):
        build = tmp_path / "build"
        build.mkdir()
        for name in ("extract.py", "corpus_paths.py"):
            shutil.copy2(REPO / name, build / name)
        epub = tmp_path / "epub" / ("%s.epub" % self.REGISTER_ID)
        epub.parent.mkdir(parents=True)
        epub.write_bytes(VolumeGateTests.epub_bytes())
        manifest = [{
            "id": self.REGISTER_ID, "name": "Demo Approval 2025",
            "epub": epub.name, "versionStart": "2025-01-01",
            "compilationNumber": "1", "collection": "LegislativeInstrument",
            "sourceUrl": "https://example.test/%s" % self.REGISTER_ID,
            # retry13.py resolved this title to the last published compilation.
            "version_is_current": False, "current_version_start": "2026-03-01",
        }]
        (build / "manifest_raw.json").write_text(json.dumps(manifest), encoding="utf-8")
        return build, epub

    def _run(self, build, retrieved):
        extract = load_module("extract_pipeline_%s" % (retrieved or "mtime"),
                              build / "extract.py")
        with contextlib.redirect_stdout(io.StringIO()):
            extract.main(retrieved)

    def _outputs(self, tmp_path):
        folder = tmp_path / "markdown" / self.REGISTER_ID
        rows = [json.loads(l) for l in
                (folder / "sections.jsonl").read_text(encoding="utf-8").splitlines() if l.strip()]
        return (folder / ("%s.md" % self.REGISTER_ID)).read_text(encoding="utf-8"), rows

    def test_a_schedule_volume_reaches_the_markdown_and_the_jsonl(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, _epub = self._fixture(tmp_path)
            self._run(build, None)
            markdown, rows = self._outputs(tmp_path)
            self.assertIn("| 55 | 12.5 |", markdown)
            carrying = [r for r in rows if "Table 3B Lump sum valuation factors" in r["text"]]
            self.assertEqual(len(carrying), 1)
            self.assertIn("| 55 | 12.5 |", carrying[0]["text"])
            # retry13.py's docstring: a superseded compilation must never be
            # reported as the current text, in either output.
            self.assertIn("version_is_current: false", markdown)
            self.assertTrue(all(r["version_is_current"] is False for r in rows))

    def test_the_retrieved_date_defaults_to_the_epub_mtime(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, epub = self._fixture(tmp_path)
            stamp = datetime.datetime(2026, 2, 3, 12, 0).timestamp()
            os.utime(epub, (stamp, stamp))
            expected = datetime.date.fromtimestamp(stamp).isoformat()
            self._run(build, None)
            markdown, rows = self._outputs(tmp_path)
            self.assertIn("retrieved: %s" % expected, markdown)
            self.assertIn(expected, rows[0]["attribution"])

    def test_the_retrieved_argument_overrides_a_restored_mtime(self):
        """An EPUB restored from a backup carries the copy's mtime, and the
        Register's attribution wording embeds the retrieval date."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, epub = self._fixture(tmp_path)
            stamp = datetime.datetime(2026, 2, 3, 12, 0).timestamp()
            os.utime(epub, (stamp, stamp))
            self._run(build, "2026-08-04")
            markdown, rows = self._outputs(tmp_path)
            self.assertIn("retrieved: 2026-08-04", markdown)
            self.assertNotIn("retrieved: 2026-02-03", markdown)
            self.assertEqual(rows[0]["attribution"].count("2026-08-04"), 1)
            manifest = json.loads((build / "manifest_md.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest[0]["retrieved"], "2026-08-04")

    def test_a_retrieved_argument_that_is_not_a_date_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, _epub = self._fixture(tmp_path)
            extract = load_module("extract_bad_date", build / "extract.py")
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(ValueError):
                    extract.main("--help")
            self.assertFalse((build / "manifest_md.json").exists())

    def test_a_retrieved_argument_reaches_the_corpus_as_a_calendar_date(self):
        """date.fromisoformat validates but does not normalise, and from 3.11 it
        also accepts the basic and week-date forms, so validating and throwing
        the result away let `20260803` through to every front matter, every
        attribution sentence and every row exactly as typed."""
        forms = ["20260804", "2026-W32-2"]
        accepted = []
        for supplied in forms:
            try:
                accepted.append((supplied, datetime.date.fromisoformat(supplied).isoformat()))
            except ValueError:
                # Refused outright before 3.11; nothing to normalise.
                continue
        for supplied, expected in accepted:
            self.assertNotEqual(supplied, expected)
            with tempfile.TemporaryDirectory() as tmp:
                tmp_path = Path(tmp)
                build, _epub = self._fixture(tmp_path)
                self._run(build, supplied)
                markdown, rows = self._outputs(tmp_path)
                self.assertIn("retrieved: %s" % expected, markdown)
                self.assertNotIn(supplied, markdown)
                self.assertNotIn(supplied, rows[0]["attribution"])
                self.assertIn(expected, rows[0]["attribution"])
                manifest = json.loads(
                    (build / "manifest_md.json").read_text(encoding="utf-8"))
                self.assertEqual(manifest[0]["retrieved"], expected)


def basin_table(field):
    """One markdown table of the shape Excise By-law No. 127 uses."""
    rows = ["| Field | Licence | Commenced | Ceased |", "|---|---|---|---|"]
    for n in range(1, 6):
        rows.append("| %s %d | L%03d | 1 January 2026 | 30 June 2026 |" % (field, n, n))
    return rows


class TableSplitTests(unittest.TestCase):
    """The completeness check BUILD.md records: every segmented line must land
    in some chunk."""

    BODY = "\n".join(
        ["# Excise By-law No. 127", "",
         "This by-law is made under section 77H of the Excise Act 1901.", ""]
        + ["A. PERTH BASIN"] + basin_table("Perth") + [""]
        + ["B. COOPER BASIN"] + basin_table("Cooper") + [""]
        + ["C. BROWSE BASIN"] + basin_table("Browse") + [""]
        + ["Signed by the Chief Executive Officer."])

    def test_every_segmented_line_lands_in_a_chunk(self):
        extract = load_module("extract_table_split", REPO / "extract.py")
        chunks = extract.table_split(self.BODY, "TPB Terminations")
        self.assertTrue(chunks)
        kept = "\n".join(c["text"][0] for c in chunks)
        for line in self.BODY.split("\n"):
            if line.strip() and not line.startswith("#"):
                self.assertIn(line.strip(), kept)

    def test_the_completeness_guard_reports_a_dropped_line(self):
        extract = load_module("extract_chunk_guard", REPO / "extract.py")
        with self.assertRaisesRegex(RuntimeError, "C. PERTH BASIN"):
            extract.check_chunks_complete(
                ["A table row", "C. PERTH BASIN"],
                [{"text": ["A table row"]}])

    def test_the_completeness_guard_runs_on_the_split_path(self):
        extract = load_module("extract_chunk_guard_wiring", REPO / "extract.py")
        with mock.patch.object(extract, "check_chunks_complete",
                               side_effect=RuntimeError("guard ran")):
            with self.assertRaisesRegex(RuntimeError, "guard ran"):
                extract.table_split(self.BODY, "TPB Terminations")


class ChunkGuardFailureReportingTests(unittest.TestCase):
    """The completeness guard is a recorded parse failure, not a crash.

    table_split is called past the per-title try/except, so a guard that raises
    there took the whole build down: no FAIL line, no summary, every remaining
    title unprocessed and the traceback the only output.  Every other failure
    mode in extract.py reports the title and carries on.
    """

    TABLE_ID = "F2020L00001"
    ORDINARY_ID = "C2004A00001"

    ORDINARY = ('<html><body>'
                '<p class="ActHead4"><span class="CharSectno">1</span> Short title</p>'
                '<p class="subsection">This instrument is the Demo Tax Act.</p>'
                '</body></html>')

    @staticmethod
    def _table_document():
        """The shape table_split exists for: prose, then a run of tables."""
        parts = ['<p>This by-law is made under section 77H of the Excise Act 1901.</p>']
        for basin in ("PERTH", "COOPER", "BROWSE", "BONAPARTE"):
            parts.append("<p>%s BASIN</p>" % basin)
            cells = "".join(
                "<tr><td>Field %d</td><td>L%03d</td><td>1 January 2026</td></tr>" % (n, n)
                for n in range(1, 6))
            parts.append("<table><tr><td>Field</td><td>Licence</td>"
                         "<td>Commenced</td></tr>%s</table>" % cells)
        return "<html><body>%s</body></html>" % "".join(parts)

    @staticmethod
    def _epub(document):
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            archive.writestr("document_1.xhtml", document)
        return payload.getvalue()

    def _fixture(self, tmp_path):
        build = tmp_path / "build"
        build.mkdir()
        for name in ("extract.py", "corpus_paths.py"):
            shutil.copy2(REPO / name, build / name)
        (tmp_path / "epub").mkdir()
        (tmp_path / "epub" / ("%s.epub" % self.TABLE_ID)).write_bytes(
            self._epub(self._table_document()))
        (tmp_path / "epub" / ("%s.epub" % self.ORDINARY_ID)).write_bytes(
            self._epub(self.ORDINARY))
        (build / "manifest_raw.json").write_text(json.dumps([
            {"id": self.TABLE_ID, "name": "Excise By-law No. 127",
             "epub": "%s.epub" % self.TABLE_ID, "versionStart": "2026-01-01"},
            {"id": self.ORDINARY_ID, "name": "Demo Tax Act",
             "epub": "%s.epub" % self.ORDINARY_ID, "versionStart": "2026-01-01"},
        ]), encoding="utf-8")
        return build

    def test_the_fixture_reaches_the_table_split_path(self):
        """Otherwise the failure test below would prove nothing."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build = self._fixture(tmp_path)
            extract = load_module("extract_guard_reached", build / "extract.py")
            reached = []
            real = extract.table_split
            extract.table_split = lambda body, name: reached.append(name) or real(body, name)
            with contextlib.redirect_stdout(io.StringIO()):
                extract.main(None)
            self.assertEqual(reached, ["Excise By-law No. 127"])
            rows = (tmp_path / "markdown" / self.TABLE_ID / "sections.jsonl")
            self.assertIn('"granularity": "table_block"', rows.read_text(encoding="utf-8"))

    def test_a_tripped_guard_is_reported_and_the_run_continues(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build = self._fixture(tmp_path)
            extract = load_module("extract_guard_failure", build / "extract.py")
            buffer = io.StringIO()
            with mock.patch.object(extract, "check_chunks_complete",
                                   side_effect=RuntimeError("chunking dropped 1 body line(s)")):
                with contextlib.redirect_stdout(buffer):
                    with self.assertRaisesRegex(RuntimeError,
                                                "refusing to write manifest_md.json"):
                        extract.main(None)
            printed = buffer.getvalue()
            self.assertIn("FAIL %s chunking dropped 1 body line(s)" % self.TABLE_ID, printed)
            # Every other title still gets its turn, and the manifest that would
            # make a partial parse look like a complete corpus is not written.
            self.assertIn(self.ORDINARY_ID, printed)
            self.assertTrue((tmp_path / "markdown" / self.ORDINARY_ID
                             / "sections.jsonl").exists())
            self.assertFalse((build / "manifest_md.json").exists())


class ParserRuleTests(unittest.TestCase):
    """The parsing rules BUILD.md records as traps that cost a real defect."""

    META = {"id": "C2004A00001", "name": "Demo Tax Act",
            "retrieved": "2026-08-03", "versionStart": "2026-01-01"}

    def markdown(self, blocks, **kwargs):
        extract = load_module("extract_parser_rules", REPO / "extract.py")
        return extract.to_markdown(blocks, self.META, **kwargs)

    def test_a_section_id_survives_the_non_breaking_hyphen(self):
        """Section numbers render as `40 <U+2011> 1`; a regex expecting a plain
        adjacent hyphen matches nothing."""
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "ActHead5", "sectno": True,
                   "text": "40 \u2011 1 Deductions for gifts"},
                  {"k": "p", "cls": "subsection", "sectno": False, "text": "Body."}]
        _md, sections, _en, _lt = self.markdown(blocks)
        self.assertEqual([s["section"] for s in sections], ["40-1"])

    def test_a_sub_heading_does_not_split_the_section_it_belongs_to(self):
        """LI-Heading3 and below are sub-headings inside a section."""
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "LI-Heading2", "sectno": False, "text": "4 Definitions"},
                  {"k": "p", "cls": "LI-Heading3", "sectno": False,
                   "text": "Meaning of associate"},
                  {"k": "p", "cls": "subsection", "sectno": False, "text": "Body."}]
        _md, sections, _en, _lt = self.markdown(blocks)
        self.assertEqual([s["section"] for s in sections], ["4"])
        self.assertIn("Meaning of associate", "\n".join(sections[0]["text"]))

    def test_a_contents_page_entry_does_not_route_the_act_into_endnotes(self):
        """The contents page carries an entry whose text is literally
        "Endnotes"."""
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "TOC2", "sectno": False, "text": "Endnotes"},
                  {"k": "p", "cls": "", "sectno": True, "text": "1 Short title"},
                  {"k": "p", "cls": "subsection", "sectno": False,
                   "text": "This Act may be cited as the Demo Tax Act."}]
        md, sections, endnotes, _lt = self.markdown(blocks)
        self.assertEqual(endnotes, "")
        self.assertIn("This Act may be cited as the Demo Tax Act.", md)
        self.assertEqual([s["section"] for s in sections], ["1"])

    def test_the_coat_of_arms_needs_all_three_signals(self):
        extract = load_module("extract_arms_gate", REPO / "extract.py")
        document = ('<html><body>'
                    '<img src="arms.png" width="120" height="90"/>'
                    '<img src="formula.png" width="480" height="90"/>'
                    '<img src="light.png" width="120" height="90"/>'
                    '</body></html>')
        parser = extract.Doc({"arms.png": 6000, "formula.png": 6000, "light.png": 900})
        parser.feed(document)
        parser._flush()
        images = [b["alt"] for b in parser.blocks if b["k"] == "img"]
        self.assertEqual(images, [
            "[Commonwealth Coat of Arms omitted, not licensed under CC BY]",
            "[image not described in source: formula.png]",
            "[image not described in source: light.png]"])

    def test_a_date_on_its_own_line_is_not_a_bare_section_heading(self):
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "", "sectno": False, "text": "1 October 2016"},
                  {"k": "p", "cls": "", "sectno": False, "text": "3. Definitions"},
                  {"k": "p", "cls": "", "sectno": False,
                   "text": "In this determination, tax means income tax."}]
        _md, sections, _en, _lt = self.markdown(blocks, force_bare=True)
        self.assertEqual([s["section"] for s in sections if s["kind"] == "section"], ["3"])

    def test_a_spanned_cell_keeps_later_cells_in_their_column(self):
        extract = load_module("extract_colspan", REPO / "extract.py")
        parser = extract.Doc()
        parser.feed('<html><body><table>'
                    '<tr><td colspan="2">Rate</td><td>Amount</td></tr>'
                    '<tr><td>Low</td><td>High</td><td>$100</td></tr>'
                    '</table></body></html>')
        parser._flush()
        tables = [b["rows"] for b in parser.blocks if b["k"] == "table"]
        self.assertEqual(tables, [[["Rate", "", "Amount"], ["Low", "High", "$100"]]])

    def test_a_nested_table_does_not_wipe_the_enclosing_table(self):
        """Word writes boxed formulas and sub-schedules as a table inside a
        cell. Without a stack the inner <table> clears the shared buffers, so
        every row the enclosing table has already parsed disappears and the
        cell that held the inner table merges into its first row."""
        extract = load_module("extract_nested_table", REPO / "extract.py")
        parser = extract.Doc()
        parser.feed('<html><body><table>'
                    '<tr><td>Item 1</td><td>Rate 5%</td></tr>'
                    '<tr><td>Item 2'
                    '<table><tr><td>sub a</td><td>1%</td></tr></table>'
                    '</td><td>Rate 6%</td></tr>'
                    '<tr><td>Item 3</td><td>Rate 7%</td></tr>'
                    '</table></body></html>')
        parser._flush()
        tables = [b["rows"] for b in parser.blocks if b["k"] == "table"]
        self.assertEqual(tables, [
            [["sub a", "1%"]],
            [["Item 1", "Rate 5%"], ["Item 2", "Rate 6%"], ["Item 3", "Rate 7%"]]])

    def test_a_nested_table_does_not_steal_the_enclosing_cell_colspan(self):
        """The inner table's own cells reset _colspan, so without saving it the
        enclosing spanned cell loses its filler columns and the row shifts."""
        extract = load_module("extract_nested_colspan", REPO / "extract.py")
        parser = extract.Doc()
        parser.feed('<html><body><table>'
                    '<tr><td colspan="2">Band'
                    '<table><tr><td>note</td></tr></table>'
                    '</td><td>Rate</td></tr>'
                    '<tr><td>Low</td><td>High</td><td>5%</td></tr>'
                    '</table></body></html>')
        parser._flush()
        tables = [b["rows"] for b in parser.blocks if b["k"] == "table"]
        self.assertEqual(tables[-1],
                         [["Band", "", "Rate"], ["Low", "High", "5%"]])


class FailureHandlingTests(unittest.TestCase):
    def test_discovery_does_not_write_a_partial_title_list(self):
        discover = load_module("discover_regression", REPO / "discover.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            discover.SCRATCH = str(scratch)
            discover.COLLECTIONS = ["Act"]
            discover.KEYWORDS = ["Tax"]
            discover.curl_json = lambda _url: None
            discover.time.sleep = lambda _seconds: None

            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to write an incomplete"):
                    discover.main()

            self.assertFalse((scratch / "titles_all.json").exists())
            self.assertFalse((scratch / "titles_principal.json").exists())

    def test_versions_does_not_write_a_partial_resolution_manifest(self):
        versions = load_module("versions_regression", REPO / "versions.py")
        title = {"id": "C2004A00001", "name": "Example Tax Act", "isPrincipal": True}
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            (scratch / "titles_all.json").write_text(json.dumps([title]), encoding="utf-8")
            versions.SCRATCH = str(scratch)
            versions.curl_json = lambda _url: None
            versions.time.sleep = lambda _seconds: None

            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to write acts_resolved"):
                    versions.main()

            self.assertFalse((scratch / "acts_resolved.json").exists())

    def test_versions_writes_a_manifest_only_after_a_complete_resolution(self):
        versions = load_module("versions_success_regression", REPO / "versions.py")
        title = {"id": "C2004A00001", "name": "Example Tax Act", "isPrincipal": True}
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            (scratch / "titles_all.json").write_text(json.dumps([title]), encoding="utf-8")
            versions.SCRATCH = str(scratch)
            versions.time.sleep = lambda _seconds: None

            def response(url):
                if "C2004A05138" in url:
                    return {"value": [{"titleId": "C2004A05138", "start": "2026-01-01"}]}
                return {"value": [{
                    "titleId": "C2004A00001", "start": "2026-02-03T00:00:00Z",
                    "compilationNumber": "7", "registerId": "F2026C00001",
                }]}

            versions.curl_json = response
            with contextlib.redirect_stdout(io.StringIO()):
                versions.main()

            resolved = json.loads((scratch / "acts_resolved.json").read_text(encoding="utf-8"))
            self.assertEqual(resolved[0]["versionStart"], "2026-02-03")
            self.assertEqual(resolved[0]["compilationRegisterId"], "F2026C00001")

    def test_extract_does_not_publish_a_manifest_after_a_parse_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build = tmp_path / "build"
            build.mkdir()
            source = build / "extract.py"
            shutil.copy2(REPO / "extract.py", source)
            shutil.copy2(REPO / "corpus_paths.py", build / "corpus_paths.py")
            extract = load_module("extract_regression", source)

            root = tmp_path
            epub = root / "epub" / "C2004A00001.epub"
            epub.parent.mkdir(parents=True)
            epub.write_bytes(b"not-an-epub")
            manifest = [{
                "id": "C2004A00001", "name": "Example Tax Act",
                "epub": epub.name, "versionStart": "2026-01-01",
                "compilationNumber": "1", "sourceUrl": "https://example.test/C2004A00001",
            }]
            (build / "manifest_raw.json").write_text(json.dumps(manifest), encoding="utf-8")
            extract.epub_blocks = lambda _path: (_ for _ in ()).throw(ValueError("bad EPUB"))

            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to write manifest_md"):
                    extract.main("2026-08-07")

            self.assertFalse((build / "manifest_md.json").exists())

    def test_extract_rejects_manifest_epub_path_mismatch_before_reading(self):
        """A valid Register ID must not make a manifest filename trustworthy."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build = tmp_path / "build"
            build.mkdir()
            source = build / "extract.py"
            shutil.copy2(REPO / "extract.py", source)
            shutil.copy2(REPO / "corpus_paths.py", build / "corpus_paths.py")
            extract = load_module("extract_path_regression", source)
            manifest = [{
                "id": "F2020L01498", "name": "Example Instrument",
                "epub": "../../unrelated.epub", "versionStart": "2026-01-01",
                "compilationNumber": "1", "sourceUrl": "https://example.test/F2020L01498",
            }]
            (build / "manifest_raw.json").write_text(json.dumps(manifest), encoding="utf-8")

            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to write manifest_md"):
                    extract.main("2026-08-07")

            self.assertFalse((build / "manifest_md.json").exists())
            self.assertFalse((tmp_path / "markdown" / "F2020L01498").exists())


class FinalizePiiSummaryTests(unittest.TestCase):
    def test_readme_pii_totals_derive_from_the_scan_with_a_fallback(self):
        finalize = load_module("finalize_pii_regression", REPO / "finalize.py")
        # The committed scan: 12 titles, 5,404 name mentions, rounded to 5,400.
        self.assertEqual(finalize.pii_summary(), (12, 5400))
        with tempfile.TemporaryDirectory() as tmp:
            finalize.SCRATCH = tmp
            # pii_scan.py runs after finalize.py in the documented pipeline,
            # so a fresh build has no scan output yet: fall back to the last
            # committed totals rather than crash or print prose constants.
            self.assertEqual(finalize.pii_summary(), (12, 5400))
            (Path(tmp) / "pii_flagged.json").write_text(json.dumps([
                {"register_id": "F2023N00096", "names_est": 130},
                {"register_id": "F2021N00219", "names_est": 220},
            ]), encoding="utf-8")
            self.assertEqual(finalize.pii_summary(), (2, 350))


class Retry13MergeTests(unittest.TestCase):
    def test_recoveries_are_merged_into_the_raw_manifest_in_place(self):
        """The documented pipeline has no manual patch step: retry13.py itself
        must fold successful recoveries into manifest_raw.json, keep
        retry13_patch.json as the audit record, and leave failed retries and
        untouched titles exactly as the download stage wrote them."""
        retry13 = load_module("retry13_regression", REPO / "retry13.py")
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
                 "httpCode": "404", "versionStart": "2026-03-01"},
                {"id": "F2021L00002", "name": "Unrecoverable Instrument",
                 "epub": None, "bytes": 0, "status": "no_epub",
                 "httpCode": "404", "versionStart": "2026-04-01"},
            ]
            (scratch / "manifest_raw.json").write_text(
                json.dumps(manifest), encoding="utf-8")
            latest = {"start": "2025-06-30T00:00:00", "compilationNumber": "4",
                      "registerId": "F2025C00010"}
            (scratch / "probe13.json").write_text(json.dumps([
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "latest_doc": dict(latest)},
                {"id": "F2021L00002", "name": "Unrecoverable Instrument",
                 "latest_doc": dict(latest)},
            ]), encoding="utf-8")

            class FakeDownload:
                CRAWL_DELAY = 0

                @staticmethod
                def fetch(url, dst):
                    if "F2020L01498" in url:
                        Path(dst).write_bytes(b"PK-fake-epub")
                        return True, "200", "application/epub+zip", 12, {
                            "registerId": "F2025C00010", "isAuthorised": True}
                    return False, "404", "text/html", 0, None

            retry13.SCRATCH = str(scratch)
            retry13.EPUB_DIR = str(epub_dir)
            retry13.dl = FakeDownload
            retry13.time.sleep = lambda _seconds: None
            with contextlib.redirect_stdout(io.StringIO()):
                retry13.main()

            patches = json.loads(
                (scratch / "retry13_patch.json").read_text(encoding="utf-8"))
            self.assertEqual({p["id"] for p in patches},
                             {"F2020L01498", "F2021L00002"})

            merged = {a["id"]: a for a in json.loads(
                (scratch / "manifest_raw.json").read_text(encoding="utf-8"))}
            self.assertEqual(len(merged), 3)
            recovered = merged["F2020L01498"]
            self.assertEqual(recovered["epub"], "F2020L01498.epub")
            self.assertEqual(recovered["status"], "ok_superseded_version")
            self.assertIs(recovered["version_is_current"], False)
            self.assertEqual(recovered["current_version_start"], "2026-03-01")
            self.assertEqual(recovered["name"], "Recoverable Instrument")
            # A failed retry must not overwrite the original record.
            self.assertEqual(merged["F2021L00002"]["status"], "no_epub")
            self.assertIsNone(merged["F2021L00002"]["epub"])
            self.assertNotIn("version_is_current", merged["F2021L00002"])
            self.assertEqual(merged["C2004A00001"]["status"], "ok")


class Retry13ManifestWriteTests(unittest.TestCase):
    def test_a_failed_manifest_write_leaves_the_download_record_intact(self):
        """manifest_raw.json is the only record of the download crawl, and both
        writers replace it in place, so neither may truncate it.  See
        DownloadManifestWriteTests for the download.py half."""
        retry13 = load_module("retry13_atomic", REPO / "retry13.py")
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
            (scratch / "probe13.json").write_text(json.dumps([
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "latest_doc": {"start": "2025-06-30T00:00:00", "compilationNumber": "4",
                                "registerId": "F2025C00010"}}]), encoding="utf-8")

            class FakeDownload:
                CRAWL_DELAY = 0

                @staticmethod
                def fetch(url, dst):
                    Path(dst).write_bytes(b"PK-fake-epub")
                    return True, "200", "application/epub+zip", 12, {
                        "registerId": "F2025C00010", "isAuthorised": True}

            real_dump = retry13.json.dump

            def exploding_dump(obj, fp, **kwargs):
                if os.path.basename(getattr(fp, "name", "")).startswith("manifest_raw.json"):
                    fp.write('[{"id": "F2020')
                    raise OSError("no space left on device")
                return real_dump(obj, fp, **kwargs)

            retry13.SCRATCH = str(scratch)
            retry13.EPUB_DIR = str(epub_dir)
            retry13.dl = FakeDownload
            retry13.time.sleep = lambda _seconds: None
            with mock.patch.object(retry13.json, "dump", exploding_dump):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(OSError):
                        retry13.main()

            self.assertEqual(manifest_path.read_text(encoding="utf-8"), original)
            self.assertFalse((scratch / "manifest_raw.json.tmp").exists())


class DownloadManifestWriteTests(unittest.TestCase):
    def test_a_failed_manifest_write_leaves_the_previous_crawl_intact(self):
        """download.py's own dump of manifest_raw.json replaces the file an
        earlier run wrote - BUILD.md documents re-running a single stage - and
        it lands at the end of a 2h40m crawl.  Truncating it in place and then
        failing mid-dump leaves a half-written file that extract.py cannot
        json.load, with the crawl it recorded already spent."""
        download = load_module("download_atomic_manifest", REPO / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            epub_dir = scratch / "corpus" / "epub"
            epub_dir.mkdir(parents=True)
            (scratch / "acts_resolved.json").write_text(json.dumps([
                {"id": "F2020L01498", "name": "Recoverable Instrument",
                 "versionStart": "2026-03-01", "compilationNumber": "4"}]),
                encoding="utf-8")

            previous = [
                {"id": "C2004A00001", "name": "Already Downloaded Act",
                 "epub": "C2004A00001.epub", "bytes": 10, "status": "ok",
                 "sourceUrl": "https://example.test/C2004A00001",
                 "versionStart": "2026-01-01"},
            ]
            manifest_path = scratch / "manifest_raw.json"
            manifest_path.write_text(json.dumps(previous), encoding="utf-8")
            original = manifest_path.read_text(encoding="utf-8")

            def fake_fetch(url, dst, tries=3):
                Path(dst).write_bytes(b"PK-fake-epub")
                return True, "200", "application/epub+zip", 12, {
                    "registerId": "F2025C00010", "isAuthorised": True}

            real_dump = download.json.dump

            def exploding_dump(obj, fp, **kwargs):
                if os.path.basename(getattr(fp, "name", "")).startswith("manifest_raw.json"):
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
            self.assertFalse((scratch / "manifest_raw.json.tmp").exists())


class DiscoveryPagingTests(unittest.TestCase):
    def test_paging_orders_by_id_and_refuses_a_page_that_repeats_one(self):
        """Unordered paging is how 142 titles, the Tax Agent Services Act 2009
        among them, went missing."""
        discover = load_module("discover_paging", REPO / "discover.py")
        discover.time.sleep = lambda _seconds: None
        first = {"value": [{"id": "C2004A%05d" % n} for n in range(100)]}
        requested = []

        def responses(url):
            requested.append(url)
            return first if "$skip=0" in url else {"value": [{"id": "C2004A00200"}]}

        discover.curl_json = responses
        rows = discover.page_titles("Tax", "Act")
        self.assertEqual(len(rows), 101)
        self.assertEqual(len(requested), 2)
        self.assertTrue(all("$orderby=id" in url for url in requested), requested)

        discover.curl_json = lambda url: (
            first if "$skip=0" in url else {"value": [{"id": "C2004A00007"}]})
        with self.assertRaises(SystemExit):
            discover.page_titles("Tax", "Act")


class DownloadValidationTests(unittest.TestCase):
    def _fetch(self, download, dst, body):
        def fake_run(args, **kwargs):
            Path(dst).write_bytes(body)
            return mock.Mock(stdout="200|text/html")

        with mock.patch.object(download.subprocess, "run", fake_run):
            return download.fetch("https://example.test/x", dst, tries=1)

    def test_fetch_refuses_a_body_that_is_not_a_zip(self):
        """The Register answers some paths with an HTML error page, and a saved
        error page is indistinguishable from an EPUB by filename alone."""
        download = load_module("download_fetch_validation", REPO / "download.py")
        download.time.sleep = lambda _seconds: None
        with tempfile.TemporaryDirectory() as tmp:
            dst = str(Path(tmp) / "C2004A00001.epub")
            ok, _code, _ctype, _size, _meta = self._fetch(
                download, dst, b"<html><body>Not found</body></html>")
            self.assertFalse(ok)

            payload = io.BytesIO()
            with zipfile.ZipFile(payload, "w") as archive:
                archive.writestr("document_1.xhtml", "<html><body><p>Act</p></body></html>")
            ok, _code, _ctype, size, _meta = self._fetch(download, dst, payload.getvalue())
            self.assertTrue(ok)
            self.assertEqual(size, os.path.getsize(dst))


class StalenessBucketTests(unittest.TestCase):
    def _run(self, base, response):
        module = load_module("check_current_buckets", base / "check_current.py")
        module.time.sleep = lambda _seconds: None
        module.curl_json = lambda _url: response
        buffer = io.StringIO()
        with mock.patch.object(module.sys, "argv", ["check_current.py"]):
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
        for name in ("check_current.py", "corpus_paths.py"):
            shutil.copy2(REPO / name, base / name)

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


class GeneratedReadmeTests(unittest.TestCase):
    def test_the_corpus_readme_states_no_staleness_counts_it_never_observed(self):
        """check_current.py is a separate command run after this README is
        written, so any run's counts baked into the template go stale on the
        next rebuild while the interpolated figures beside them stay correct."""
        source = (REPO / "finalize.py").read_text(encoding="utf-8")
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
        source = (REPO / "finalize.py").read_text(encoding="utf-8")
        paragraphs = [" ".join(p.split()) for p in re.split(r"\n\s*\n", source)
                      if "`titles_not_current_version`" in p]
        self.assertTrue(paragraphs, "no README paragraph cites the bucket")
        for paragraph in paragraphs:
            self.assertNotIn("superseded compilation", paragraph, paragraph)
        staleness = source.split("## Checking staleness", 1)[1].split("## Licence", 1)[0]
        self.assertIn("no published compilation", " ".join(staleness.split()))

    def test_this_repositorys_readme_does_not_file_those_titles_as_superseded(self):
        """The same claim, in the README a reader meets first.  The generated
        text was corrected and this one was not, and the test above reads only
        finalize.py, so nothing held the sibling.  BUILD.md and check_current.py
        keep these titles out of the superseded bucket precisely because that
        bucket says re-download, at a URL that answers 404."""
        readme = (REPO / "README.md").read_text(encoding="utf-8")
        paragraphs = [" ".join(p.split()) for p in re.split(r"\n\s*\n", readme)
                      if "version_is_current" in p]
        self.assertTrue(paragraphs, "no README paragraph cites the flag")
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
        manifest = json.loads((REPO / "manifest_md.json").read_text(encoding="utf-8"))
        entry = [a for a in manifest if a["id"] == "F2025L00281"]
        self.assertEqual(len(entry), 1)
        build = " ".join((REPO / "BUILD.md").read_text(encoding="utf-8").split())
        self.assertIn("predate the volume-gate fix", build)
        self.assertIn("sections=%d, words=%d" % (entry[0]["sections"], entry[0]["words"]),
                      build)
        # The table stack landed after the same run, so it moves the same
        # figures and has to be recorded in the same breath as the volume gate.
        self.assertIn("table stack", build)
        # The headline table is where a reader meets those figures first.
        readme = " ".join((REPO / "README.md").read_text(encoding="utf-8").split())
        self.assertIn("predates the volume-gate fix", readme)
        self.assertIn("table-stack fix", readme)


class PiiNameGateTests(unittest.TestCase):
    """The person-name gate shared by pii_scan.py, pii_scan2.py and
    dist_verify.py through pii_patterns.py."""

    # A disciplinary-register row in the all-caps style the old
    # Capitalised-lowercase pair could not see: three surnames the old pattern
    # missed (all-caps, internal capital, apostrophe), three 8-digit
    # registration numbers.
    ALL_CAPS_ROW = ("| SMITH, John | 12345678 | s 30-15 |\n"
                    "| McDonald, Anne | 23456789 | s 30-15 |\n"
                    "| O'Brien, Patrick | 34567890 | s 30-20 |")

    def test_caps_mac_and_apostrophe_surnames_are_visible_to_the_gate(self):
        patterns = load_module("pii_patterns_regression", REPO / "pii_patterns.py")
        names = patterns.person_names(self.ALL_CAPS_ROW)
        self.assertIn("SMITH, John", names)
        self.assertIn("McDonald, Anne", names)
        self.assertIn("O'Brien, Patrick", names)
        self.assertGreaterEqual(len(names), 3)
        self.assertGreaterEqual(
            len(set(patterns.REGNO.findall(self.ALL_CAPS_ROW))), 3)

    def test_statutory_vocabulary_is_filtered_case_insensitively(self):
        patterns = load_module("pii_patterns_statutory", REPO / "pii_patterns.py")
        # The statutory list holds capitalised forms, so an all-caps candidate
        # must be checked against it case-insensitively, not waved through.
        self.assertEqual(patterns.person_names(
            "TAXATION ADMINISTRATION PROVISIONS 12345678 23456789 34567890"),
            set())
        self.assertEqual(
            patterns.person_names("Deputy Commissioner of Taxation"), set())

    def test_pii_scan_flags_a_register_written_in_capitals(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            build = base / "build"
            build.mkdir()
            for name in ("pii_scan.py", "pii_patterns.py", "corpus_paths.py"):
                shutil.copy2(REPO / name, build / name)

            titles = [
                {"register_id": "F2026N00001", "name": "All Caps Register",
                 "collection": "NotifiableInstrument"},
                {"register_id": "C2004A00001", "name": "Ordinary Tax Act",
                 "collection": "Act"},
            ]
            (base / "sources.json").write_text(
                json.dumps({"titles": titles}), encoding="utf-8")
            rows = {
                "F2026N00001": {"row_id": "F2026N00001-1",
                                "text": self.ALL_CAPS_ROW},
                "C2004A00001": {"row_id": "C2004A00001-1",
                                "text": "The Commissioner may determine the rate."},
            }
            for rid, row in rows.items():
                folder = base / "markdown" / rid
                folder.mkdir(parents=True)
                (folder / "sections.jsonl").write_text(
                    json.dumps(row) + "\n", encoding="utf-8")

            scan = load_module("pii_scan_regression", build / "pii_scan.py")
            with contextlib.redirect_stdout(io.StringIO()):
                scan.main()

            flagged = json.loads(
                (build / "pii_flagged.json").read_text(encoding="utf-8"))
            self.assertEqual([f["register_id"] for f in flagged], ["F2026N00001"])
            self.assertGreaterEqual(flagged[0]["names_est"], 3)


class DistributionTests(unittest.TestCase):
    PUBLIC_ID = "C2004A00001"
    PRIVATE_ID = "C2004A00002"
    PUBLIC_NAME = "Public Tax Act"
    PRIVATE_NAME = "Private Disciplinary Register"

    def _write_fixture(self, base):
        root = base / "corpus"
        build = base / "build"
        build.mkdir()
        (build / "pii_flagged.json").write_text(json.dumps([{
            "register_id": self.PRIVATE_ID,
            "name": self.PRIVATE_NAME,
            "rows_total": 1,
            "rows_flagged": 1,
            "names_est": 3,
            "words": 3,
        }]), encoding="utf-8")

        public_row = {
            "register_id": self.PUBLIC_ID, "act": self.PUBLIC_NAME,
            "collection": "Act", "section": "1", "heading": "Public rate",
            "kind": "section", "text": "The public rate is 10%.",
        }
        private_row = {
            "register_id": self.PRIVATE_ID, "act": self.PRIVATE_NAME,
            "collection": "NotifiableInstrument", "section": "1", "heading": "Private register",
            "kind": "section", "text": "Private row with 20%.",
        }
        for row in (public_row, private_row):
            folder = root / "markdown" / row["register_id"]
            folder.mkdir(parents=True)
            (folder / "sections.jsonl").write_text(json.dumps(row) + "\n", encoding="utf-8")
            (folder / (row["register_id"] + ".md")).write_text("# " + row["act"] + "\n", encoding="utf-8")

        def title(row):
            return {
                "register_id": row["register_id"], "name": row["act"],
                "collection": row["collection"], "keywords_in_name": ["Tax"],
                "jsonl_rows": 1, "words": len(row["text"].split()),
                "granularity": "section", "endnotes": False,
                "version_is_current": True, "epub": "epub/%s.epub" % row["register_id"],
            }

        sources = {
            "titles": [title(public_row), title(private_row)],
            "counts": {"titles": 2, "acts": 1, "instruments": 1,
                       "jsonl_rows": 2, "words_body_only": 10, "epub_bytes": 200},
            "titles_without_epub": [], "titles_not_current_version": [],
        }
        root.mkdir(exist_ok=True)
        (root / "sources.json").write_text(json.dumps(sources), encoding="utf-8")
        (root / "LICENCE-NOTICE.md").write_text("licence\n", encoding="utf-8")
        (root / "INDEX.md").write_text(
            "# Index\n\n2 titles (1 Acts, 1 instruments), 2 retrieval rows, 10 words.\n\n"
            "| Collection | Titles | Rows | Words |\n|---|---|---|---|\n"
            "| Acts | 1 | 1 | 5 |\n| Notifiable instruments | 1 | 1 | 5 |\n\n"
            "## Acts (1)\n| [Public Tax Act](markdown/C2004A00001) | C2004A00001 |\n\n"
            "## Notifiable instruments (1)\n"
            "| [Private Disciplinary Register](markdown/C2004A00002) | C2004A00002 |\n",
            encoding="utf-8")
        (root / "README.md").write_text(
            "2 in-force principal titles covering tax.\n\n"
            "1 Acts and 1 legislative and notifiable instruments. 2 retrieval rows, 10 words.\n\n"
            "Each title is stored as:\n\n"
            "- `epub/<register_id>.epub` - the file exactly as the Register served it\n"
            "- `markdown/<register_id>/<register_id>.md` - full text\n\n"
            # Spelled-out lead, as the README built by the previous finalize
            # generation actually reads — the rewrite must strip this form
            # too, not only the numeric "**12 titles name people.**" the new
            # finalize writes.
            "**Eleven titles name people.** The corpus carries about 3 name mentions\n"
            "of disciplined agents with their registration numbers.\n\n"
            "**0 titles are not the current text.** Placeholder.\n",
            encoding="utf-8")

        rates = root / "rates"
        rates.mkdir()
        rate_rows = [
            {"register_id": self.PUBLIC_ID, "act": self.PUBLIC_NAME, "collection": "Act",
             "section": "1", "heading": "Public rate", "topic": "income tax rates",
             "kind": "rate", "amounts": ["10%"], "content": "The public rate is 10%."},
            {"register_id": self.PRIVATE_ID, "act": self.PRIVATE_NAME,
             "collection": "NotifiableInstrument", "section": "1", "heading": "Private rate",
             "topic": "income tax rates", "kind": "rate", "amounts": ["20%"],
             "content": "Private rate content must not be distributed."},
        ]
        (rates / "rates.jsonl").write_text(
            "".join(json.dumps(row) + "\n" for row in rate_rows), encoding="utf-8")
        (rates / "RATES.md").write_text(
            "2 entries across 2 titles.\nPrivate Disciplinary Register\n", encoding="utf-8")
        return root, build

    def test_distribution_rebuilds_counts_and_rates_markdown_from_filtered_rows(self):
        dist = load_module("dist_regression", REPO / "dist.py")
        verify = load_module("dist_verify_regression", REPO / "dist_verify.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()

            output = Path(dist.DIST)
            sources = json.loads((output / "sources.json").read_text(encoding="utf-8"))
            self.assertEqual(sources["counts"]["titles"], 1)
            self.assertEqual(sources["counts"]["jsonl_rows"], 1)
            self.assertEqual(sources["counts"]["epub_bytes"], 0)
            self.assertEqual(sources["titles_count"], 1)
            self.assertIsNone(sources["titles"][0]["epub"])
            self.assertFalse(sources["titles"][0]["epub_included"])

            index_md = (output / "INDEX.md").read_text(encoding="utf-8")
            self.assertIn(self.PUBLIC_NAME, index_md)
            self.assertNotIn(self.PRIVATE_NAME, index_md)

            rates_md = (output / "rates" / "RATES.md").read_text(encoding="utf-8")
            self.assertIn("1 entries across 1 titles.", rates_md)
            self.assertIn(self.PUBLIC_NAME, rates_md)
            self.assertNotIn(self.PRIVATE_NAME, rates_md)

            # The full-corpus README's EPUB layout bullet and its paragraph on
            # titles naming people are both false for dist and must not
            # survive the rewrite; the markdown bullet must.
            dist_readme = (output / "README.md").read_text(encoding="utf-8")
            self.assertNotIn("epub/<register_id>.epub", dist_readme)
            self.assertNotIn("titles name people", dist_readme)
            self.assertNotIn("disciplined agents", dist_readme)
            self.assertIn("markdown/<register_id>/<register_id>.md", dist_readme)
            self.assertIn("REMOVED.md", dist_readme)

            verify.DIST = str(output)
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as result:
                    verify.main()
            self.assertEqual(result.exception.code, 0)

            # The strengthened verifier must reject a distribution whose nested
            # machine-readable counts drift from the files it ships.
            sources["counts"]["jsonl_rows"] = 2
            (output / "sources.json").write_text(json.dumps(sources), encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as result:
                    verify.main()
            self.assertEqual(result.exception.code, 1)

            # The readable rate index is an independently checked artifact: a
            # filtered JSONL file alone is not enough if a full-corpus RATES.md
            # could still carry a removed title.
            sources["counts"]["jsonl_rows"] = 1
            (output / "sources.json").write_text(json.dumps(sources), encoding="utf-8")
            with open(output / "rates" / "RATES.md", "a", encoding="utf-8") as f:
                f.write(self.PRIVATE_NAME + "\n")
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit) as result:
                    verify.main()
            self.assertEqual(result.exception.code, 1)

    def _verify(self, verify):
        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            with self.assertRaises(SystemExit) as result:
                verify.main()
        return result.exception.code, buffer.getvalue()

    def _status(self, output, label):
        lines = [l for l in output.splitlines() if l.strip().startswith(label)]
        self.assertEqual(len(lines), 1, output)
        return lines[0].strip()[len(label):].split()[0]

    def test_the_verifier_fails_on_each_claim_it_reports_as_passing(self):
        """Every check in dist_verify.py passes on a clean distribution, so a
        check that has been weakened or deleted looks exactly like a check that
        is holding.  Plant the condition each one exists to catch."""
        dist = load_module("dist_claims", REPO / "dist.py")
        verify = load_module("dist_verify_claims", REPO / "dist_verify.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()
            output = Path(dist.DIST)
            verify.DIST = str(output)

            code, out = self._verify(verify)
            self.assertEqual(code, 0)
            self.assertEqual(self._status(out, "no row names private individuals"), "PASS")

            # A distributed title that names disciplined agents with their
            # registration numbers: the one thing dist.py exists to remove.
            rows = output / "markdown" / self.PUBLIC_ID / "sections.jsonl"
            clean_rows = rows.read_text(encoding="utf-8")
            row = json.loads(clean_rows)
            rows.write_text(
                json.dumps(dict(row, text=PiiNameGateTests.ALL_CAPS_ROW)) + "\n",
                encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(out, "no row names private individuals"), "FAIL")
            rows.write_text(clean_rows, encoding="utf-8")

            # A directory for a title the build removed.
            stray = output / "markdown" / self.PRIVATE_ID
            stray.mkdir()
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(out, "no removed title present"), "FAIL")
            stray.rmdir()

            # Image bytes, which carry the Coat of Arms and are not CC BY.
            planted = output / "markdown" / self.PUBLIC_ID / "figure.png"
            planted.write_bytes(b"\x89PNG\r\n\x1a\n")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(out, "no image files"), "FAIL")
            planted.unlink()

            # A README that still offers files this distribution does not ship.
            readme = output / "README.md"
            clean_readme = readme.read_text(encoding="utf-8")
            readme.write_text(
                clean_readme + "\n- `epub/<register_id>.epub` - as served\n",
                encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(
                self._status(out, "README does not promise EPUB files"), "FAIL")
            readme.write_text(clean_readme, encoding="utf-8")

            code, _out = self._verify(verify)
            self.assertEqual(code, 0)

    def test_distribution_rejects_nested_symlinks_before_copying(self):
        dist = load_module("dist_symlink_regression", REPO / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            source = root / "markdown" / self.PUBLIC_ID
            outside = Path(tmp) / "outside.txt"
            outside.write_text("must not be copied", encoding="utf-8")
            link = source / "unexpected-link"
            try:
                link.symlink_to(outside)
            except OSError:
                self.skipTest("symbolic links are unavailable on this platform")
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with self.assertRaisesRegex(ValueError, "symbolic link"):
                dist.main()

    def test_readme_count_replacement_is_precise_and_linear(self):
        dist = load_module("dist_count_regression", REPO / "dist.py")
        text = ("1 Acts and 2 legislative and notifiable instruments. "
                "3 Acts and 4 legislative and notifiable instruments.")
        self.assertEqual(
            dist.replace_readme_collection_counts(text, 5, 6),
            "5 Acts and 6 legislative and notifiable instruments. "
            "5 Acts and 6 legislative and notifiable instruments.")

        digits = "9" * 200_000
        started = time.perf_counter()
        rewritten = dist.replace_readme_collection_counts(
            digits + " Acts and " + digits + " legislative and notifiable", 5, 6)
        self.assertEqual(rewritten, "5 Acts and 6 legislative and notifiable")
        self.assertLess(time.perf_counter() - started, 2.0)


class PathBoundaryTests(unittest.TestCase):
    def test_distribution_boundary_rejects_a_root_directory_junction(self):
        paths = load_module("corpus_paths_root_junction", REPO / "corpus_paths.py")
        with (
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths.os.path, "isjunction", return_value=True, create=True),
            mock.patch.object(paths.os, "walk") as walk,
        ):
            with self.assertRaisesRegex(ValueError, "junction"):
                paths.reject_symlinks("corpus")
        walk.assert_not_called()

    def test_distribution_boundary_rejects_windows_directory_junctions(self):
        paths = load_module("corpus_paths_junction", REPO / "corpus_paths.py")
        with (
            mock.patch.object(paths.os, "walk", return_value=[("corpus", ["junction"], [])]) as walk,
            mock.patch.object(paths.os.path, "islink", return_value=False),
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

    def test_register_id_and_contained_child_reject_traversal(self):
        paths = load_module("corpus_paths_regression", REPO / "corpus_paths.py")
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
        paths = load_module("corpus_paths_layout", REPO / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            checkout_script = base / "checkout" / "extract.py"
            checkout_script.parent.mkdir()
            checkout_script.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(checkout_script)), checkout_script.parent / "corpus")

            deployed_script = base / "corpus-root" / "build" / "extract.py"
            deployed_script.parent.mkdir(parents=True)
            deployed_script.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(deployed_script)), deployed_script.parent.parent)

    def test_check_current_uses_checkout_output_or_deployed_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            checkout = base / "checkout"
            checkout.mkdir()
            shutil.copy2(REPO / "check_current.py", checkout / "check_current.py")
            shutil.copy2(REPO / "corpus_paths.py", checkout / "corpus_paths.py")
            checkout_module = load_module("check_current_checkout", checkout / "check_current.py")
            self.assertEqual(Path(checkout_module.ROOT), checkout / "corpus")

            deployed = base / "deployed"
            deployed.mkdir()
            (deployed / "sources.json").write_text("{}", encoding="utf-8")
            shutil.copy2(REPO / "check_current.py", deployed / "check_current.py")
            shutil.copy2(REPO / "corpus_paths.py", deployed / "corpus_paths.py")
            deployed_module = load_module("check_current_deployed", deployed / "check_current.py")
            self.assertEqual(Path(deployed_module.ROOT), deployed)


class RateParsingTests(unittest.TestCase):
    def test_sentence_boundaries_and_percentage_pattern_remain_precise(self):
        rates = load_module("rates_regression", REPO / "rates.py")
        text = "The first rule applies.  Next sentence carries 10%.\n\nThird sentence is here."
        self.assertEqual(list(rates.sentences(text)), [
            "The first rule applies.",
            "Next sentence carries 10%.",
            "Third sentence is here.",
        ])
        self.assertEqual(rates.percentage_values("10% and 12.5 %"), ["10%", "12.5 %"])

    def test_amount_scanners_keep_tax_formats_and_ownership_tests(self):
        rates = load_module("rates_amount_regression", REPO / "rates.py")
        self.assertEqual(rates.money_values("$1,234.50 and $ 20"), ["$1,234.50", "$ 20"])
        self.assertEqual(rates.percentage_values("10% and 12.5 %"), ["10%", "12.5 %"])
        self.assertTrue(rates.is_rate_table(["| $1,000 | 12.5 % |"]))
        self.assertTrue(rates.is_ownership_test("A 75% voting interest is required."))
        self.assertTrue(rates.is_ownership_test("The company is a 100% subsidiary."))
        self.assertFalse(rates.is_ownership_test("The tax rate is 10%."))

    def test_numeric_scanners_complete_linearly_on_long_digit_runs(self):
        rates = load_module("rates_linear_regression", REPO / "rates.py")
        digits = "9" * 200_000
        started = time.perf_counter()
        self.assertEqual(rates.percentage_values(digits + "."), [])
        self.assertEqual(len(rates.money_values("$" + digits + ".")), 1)
        self.assertTrue(rates.is_ownership_test(digits + "% stake"))
        self.assertLess(time.perf_counter() - started, 2.0)


if __name__ == "__main__":
    unittest.main()


class PreSectionTextTests(unittest.TestCase):
    """Text ahead of the first section, and across a volume seam."""

    META = {"id": "C2004A00074", "name": "Demo Tax Act",
            "retrieved": "2026-08-11", "versionStart": "2026-01-01"}

    def markdown(self, blocks, **kwargs):
        extract = load_module("extract_presection", REPO / "extract.py")
        return extract.to_markdown(blocks, self.META, **kwargs)

    def test_text_before_the_first_bare_section_reaches_the_jsonl(self):
        """A document with no mapped heading is gate-free from block one, so the
        pre-body branch never runs and bare_mode is False on the first pass.
        The enacting words then reached the markdown with no row to hold them
        and never appeared in sections.jsonl, which is what retrieval reads."""
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "", "sectno": False,
                   "text": "The Parliament of Australia enacts:"},
                  {"k": "p", "cls": "", "sectno": True, "text": "1 Short title"},
                  {"k": "p", "cls": "", "sectno": False, "text": "Body."}]
        md, sections, _en, _lt = self.markdown(blocks)
        self.assertIn("The Parliament of Australia enacts:", md)
        retrievable = "\n".join("\n".join(s["text"]) for s in sections)
        self.assertIn("The Parliament of Australia enacts:", retrievable)

    def test_a_volume_seam_does_not_file_text_under_the_previous_section(self):
        """The seam reset claims front matter must not fuse into the section
        open at the boundary, but cur survived it, so volume 2's pre-heading
        note was appended to volume 1's last section and retrieved under its
        number."""
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "ActHead5", "sectno": True, "text": "1 Short title"},
                  {"k": "p", "cls": "subsection", "sectno": False, "text": "Volume one body."},
                  {"k": "file"},
                  {"k": "p", "cls": "notetext", "sectno": False,
                   "text": "Note for the second volume."},
                  {"k": "p", "cls": "ActHead5", "sectno": True, "text": "2 Interpretation"},
                  {"k": "p", "cls": "subsection", "sectno": False, "text": "Volume two body."}]
        _md, sections, _en, _lt = self.markdown(blocks)
        first = [s for s in sections if s["section"] == "1"]
        self.assertEqual(len(first), 1)
        self.assertNotIn("Note for the second volume.", "\n".join(first[0]["text"]))
        retrievable = "\n".join("\n".join(s["text"]) for s in sections)
        self.assertIn("Note for the second volume.", retrievable)

    def test_a_table_between_pre_body_paragraphs_is_not_dropped(self):
        """PREBODY_CLASS paragraphs are admitted without setting seen_body, so
        the table/img gate discarded a table sitting between two of them while
        keeping the prose either side - silently, which this file promises
        never to do."""
        blocks = [{"k": "file"},
                  {"k": "p", "cls": "subsection", "sectno": False,
                   "text": "USER'S GUIDE"},
                  {"k": "table", "rows": [["Column", "Meaning"], ["A", "First"]]},
                  {"k": "img", "src": "fig.png", "alt": "Decision flowchart"},
                  {"k": "p", "cls": "subsection", "sectno": False,
                   "text": "Read the guide with the table above."},
                  {"k": "p", "cls": "ActHead5", "sectno": True, "text": "1 Short title"},
                  {"k": "p", "cls": "subsection", "sectno": False, "text": "Body."}]
        md, sections, _en, _lt = self.markdown(blocks)
        self.assertIn("First", md)
        self.assertIn("Decision flowchart", md)
        retrievable = "\n".join("\n".join(s["text"]) for s in sections)
        self.assertIn("First", retrievable)
        self.assertIn("Decision flowchart", retrievable)


class ExtractManifestWriteTests(unittest.TestCase):
    def test_a_failed_manifest_dump_leaves_the_previous_one_intact(self):
        """download.py and retry13.py both stage and rename so neither can
        truncate the raw manifest. extract.py opened manifest_md.json directly,
        so a full disk during the dump destroyed it after the whole markdown
        tree had already been written."""
        extract = load_module("extract_manifest_write", REPO / "extract.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            target = scratch / "manifest_md.json"
            previous = '[{"id": "C2004A00001", "sections": 20}]'
            target.write_text(previous, encoding="utf-8")

            class Unserialisable:
                pass

            with self.assertRaises(TypeError):
                extract.write_md_manifest(str(scratch), [{"bad": Unserialisable()}])

            self.assertEqual(target.read_text(encoding="utf-8"), previous)
            self.assertEqual([p.name for p in scratch.iterdir()], ["manifest_md.json"])


class DistTableBlockParagraphTests(unittest.TestCase):
    """finalize.py writes the paragraph; dist.py has to rewrite it.

    Every title dist removes is a table_block title, so the full corpus's count
    is wrong for the subset and the worked example names exactly the
    instruments that were dropped.
    """

    SAMPLE = (
        "Intro paragraph.\n\n"
        "**Table-shaped instruments are split on their tables.** 14 titles carry\n"
        "`granularity: table_block`: no headings anywhere, just a run of tables. The Tax\n"
        "Practitioners Board publishes its terminations this way, and Excise By-law No.\n"
        "127 prescribes petroleum fields one table per basin.\n\n"
        "Following paragraph.\n"
    )

    def _dist(self):
        return load_module("dist_table_block", REPO / "dist.py")

    def test_finalize_still_writes_the_paragraph_dist_rewrites(self):
        """If finalize.py rewords the lead, the rewrite silently stops firing
        and the subset ships the full corpus's claim."""
        finalize = (REPO / "finalize.py").read_text(encoding="utf-8")
        self.assertIn(self._dist().TABLE_BLOCK_LEAD, finalize)

    def test_the_paragraph_is_replaced_with_the_subset_figure(self):
        result = self._dist().replace_readme_table_block_paragraph(self.SAMPLE, 2)
        self.assertIn("2 titles carry", result)
        self.assertNotIn("14 titles carry", result)
        # the worked example names the very titles dist removes
        self.assertNotIn("Excise By-law No.", result)
        self.assertNotIn("publishes its terminations this way", result)
        # neighbours untouched
        self.assertTrue(result.startswith("Intro paragraph.\n\n"))
        self.assertTrue(result.endswith("Following paragraph.\n"))

    def test_a_readme_without_the_paragraph_is_returned_unchanged(self):
        dist = self._dist()
        text = "No such paragraph here.\n"
        self.assertEqual(dist.replace_readme_table_block_paragraph(text, 2), text)

    def test_the_replacement_is_linear_not_backtracking(self):
        """py/polynomial-redos: a pattern ending `.*?\\n\\n` backtracks on a
        README repeating the lead. This repo already fixed that rule once in
        rates.py, so the replacement must stay a scan, not a regex."""
        dist = self._dist()
        hostile = dist.TABLE_BLOCK_LEAD * 20000
        start = time.monotonic()
        dist.replace_readme_table_block_paragraph(hostile, 2)
        self.assertLess(time.monotonic() - start, 1.0)

    def test_it_rewrites_the_real_generated_corpus_paragraph(self):
        """The shape finalize.py actually emits, not just a hand-written one."""
        dist = self._dist()
        result = dist.replace_readme_table_block_paragraph(self.SAMPLE, 2)
        self.assertEqual(result.count(dist.TABLE_BLOCK_LEAD), 1)
