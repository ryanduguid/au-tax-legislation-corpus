"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import contextlib
import datetime
import hashlib
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
import warnings
import zipfile
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

    def test_epub_extraction_closes_the_archive(self):
        extract = load_module("extract_archive_lifecycle", STAGE / "extract.py")
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
        extract = load_module("extract_volume_gate", STAGE / "extract.py")
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
        extract = load_module(name, STAGE / "extract.py")
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
        extract = load_module("extract_volume_endnote_trigger", STAGE / "extract.py")
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
            shutil.copy2(stage_file(name), build / name)
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
        extract = load_module("extract_table_split", STAGE / "extract.py")
        chunks = extract.table_split(self.BODY, "TPB Terminations")
        self.assertTrue(chunks)
        kept = "\n".join(c["text"][0] for c in chunks)
        for line in self.BODY.split("\n"):
            if line.strip() and not line.startswith("#"):
                self.assertIn(line.strip(), kept)

    def test_the_completeness_guard_reports_a_dropped_line(self):
        extract = load_module("extract_chunk_guard", STAGE / "extract.py")
        with self.assertRaisesRegex(RuntimeError, "C. PERTH BASIN"):
            extract.check_chunks_complete(
                ["A table row", "C. PERTH BASIN"],
                [{"text": ["A table row"]}])

    def test_the_completeness_guard_runs_on_the_split_path(self):
        extract = load_module("extract_chunk_guard_wiring", STAGE / "extract.py")
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
            shutil.copy2(stage_file(name), build / name)
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
        extract = load_module("extract_parser_rules", STAGE / "extract.py")
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
        extract = load_module("extract_arms_gate", STAGE / "extract.py")
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
        extract = load_module("extract_colspan", STAGE / "extract.py")
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
        extract = load_module("extract_nested_table", STAGE / "extract.py")
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
        extract = load_module("extract_nested_colspan", STAGE / "extract.py")
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
        discover = load_module("discover_regression", STAGE / "discover.py")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            discover.SCRATCH = str(scratch)
            discover.COLLECTIONS = ["Act"]
            discover.KEYWORDS = ["Tax"]
            discover.fetch_json = lambda _url: None

            # module.time is the process-wide time module: patch, never assign.
            with mock.patch.object(discover.time, "sleep"), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to write an incomplete"):
                    discover.main()

            self.assertFalse((scratch / "titles_all.json").exists())
            self.assertFalse((scratch / "titles_principal.json").exists())

    def test_versions_does_not_write_a_partial_resolution_manifest(self):
        versions = load_module("versions_regression", STAGE / "versions.py")
        title = {"id": "C2004A00001", "name": "Example Tax Act", "isPrincipal": True}
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            (scratch / "titles_all.json").write_text(json.dumps([title]), encoding="utf-8")
            versions.SCRATCH = str(scratch)
            versions.fetch_json = lambda _url: None

            with mock.patch.object(versions.time, "sleep"), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to write acts_resolved"):
                    versions.main()

            self.assertFalse((scratch / "acts_resolved.json").exists())

    def test_versions_writes_a_manifest_only_after_a_complete_resolution(self):
        versions = load_module("versions_success_regression", STAGE / "versions.py")
        title = {"id": "C2004A00001", "name": "Example Tax Act", "isPrincipal": True}
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            (scratch / "titles_all.json").write_text(json.dumps([title]), encoding="utf-8")
            versions.SCRATCH = str(scratch)

            def response(url):
                if "C2004A05138" in url:
                    return {"value": [{"titleId": "C2004A05138", "start": "2026-01-01"}]}
                return {"value": [{
                    "titleId": "C2004A00001", "start": "2026-02-03T00:00:00Z",
                    "compilationNumber": "7", "registerId": "F2026C00001",
                }]}

            versions.fetch_json = response
            with mock.patch.object(versions.time, "sleep"), \
                    contextlib.redirect_stdout(io.StringIO()):
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
            shutil.copy2(STAGE / "extract.py", source)
            shutil.copy2(STAGE / "corpus_paths.py", build / "corpus_paths.py")
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
            shutil.copy2(STAGE / "extract.py", source)
            shutil.copy2(STAGE / "corpus_paths.py", build / "corpus_paths.py")
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
    def test_readme_pii_totals_derive_from_the_scan_and_refuse_without_it(self):
        finalize = load_module("finalize_pii_regression", STAGE / "finalize.py")
        # The committed scan: 12 titles, 5,404 name mentions, rounded to 5,400.
        self.assertEqual(finalize.pii_summary(), (12, 5400))
        with tempfile.TemporaryDirectory() as tmp:
            finalize.SCRATCH = tmp
            # The documented pipeline runs pii_scan.py before finalize.py, so
            # a missing scan output means the stages ran out of order: refuse
            # rather than print counts no scan produced.
            with self.assertRaisesRegex(RuntimeError, "pii_flagged.json"):
                finalize.pii_summary()
            (Path(tmp) / "pii_flagged.json").write_text(json.dumps([
                {"register_id": "F2023N00096", "names_est": 130},
                {"register_id": "F2021N00219", "names_est": 220},
            ]), encoding="utf-8")
            self.assertEqual(finalize.pii_summary(), (2, 350))


class FinalizeMissingTitleTests(unittest.TestCase):
    def test_a_missing_title_reports_the_reason_download_records(self):
        """sources.json and INDEX.md read httpCode/contentType keys that
        download.py never writes - it fails the stage on HTTP and content
        errors instead of recording them - so every missing title rendered as
        None/None. The one recorded cause is the 'reason' field of the
        explicit no-document case."""
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            build = base / "build"
            build.mkdir()
            for name in ("discover.py", "versions.py", "download.py",
                         "extract.py", "finalize.py", "check_current.py",
                         "corpus_paths.py", "http_fetch.py"):
                shutil.copy2(stage_file(name), build / name)
            finalize = load_module("finalize_missing_reason", build / "finalize.py")

            ok = {"id": "C2004A00001", "name": "Example Tax Act",
                  "collection": "Act", "status": "ok",
                  "versionStart": "2026-01-01", "compilationNumber": "1",
                  "epub": "C2004A00001.epub", "bytes": 100,
                  "sourceUrl": "https://example.test/C2004A00001",
                  "markdown": "C2004A00001/C2004A00001.md",
                  "retrieved": "2026-08-03", "sections": 1,
                  "granularity": "section", "words": 4, "endnotes": False}
            missing = {"id": "F2020L01498", "name": "Unpublished Instrument",
                       "collection": "LegislativeInstrument", "epub": None,
                       "bytes": 0, "status": "no_epub",
                       "reason": "current_version_has_no_document",
                       "versionStart": "2026-03-01",
                       "sourceUrl": "https://example.test/F2020L01498"}
            (build / "manifest_md.json").write_text(json.dumps([ok]), encoding="utf-8")
            (build / "manifest_raw.json").write_text(
                json.dumps([ok, missing]), encoding="utf-8")
            # finalize.py refuses to run without the PII scan output.
            (build / "pii_flagged.json").write_text("[]\n", encoding="utf-8")
            folder = base / "markdown" / "C2004A00001"
            folder.mkdir(parents=True)
            (folder / "sections.jsonl").write_text(json.dumps({
                "register_id": "C2004A00001", "section": "1",
                "kind": "section", "text": "The rate is 10%.",
            }) + "\n", encoding="utf-8")

            with contextlib.redirect_stdout(io.StringIO()):
                finalize.main("2026-08-03")

            sources = json.loads((base / "sources.json").read_text(encoding="utf-8"))
            entry = sources["titles_without_epub"][0]
            self.assertEqual(entry["reason"], "current_version_has_no_document")
            self.assertNotIn("http_code", entry)
            index_md = (base / "INDEX.md").read_text(encoding="utf-8")
            self.assertIn("| Reason |", index_md)
            self.assertIn(
                "| Unpublished Instrument | LegislativeInstrument | F2020L01498 "
                "| current_version_has_no_document |", index_md)
            self.assertNotIn("| None | None |", index_md)


class Retry13MergeTests(unittest.TestCase):
    def test_probe13_is_safe_to_import(self):
        with mock.patch("subprocess.run") as run:
            module = load_module("probe13_import_safety", STAGE / "probe13.py")

        run.assert_not_called()
        self.assertTrue(callable(module.main))

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
                if (source == str(manifest_path) + ".tmp"
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
                    if source == str(sidecar) + ".tmp" and destination == str(sidecar):
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


class DiscoveryPagingTests(unittest.TestCase):
    def test_paging_orders_by_id_and_refuses_a_page_that_repeats_one(self):
        """Unordered paging is how 142 titles, the Tax Agent Services Act 2009
        among them, went missing."""
        discover = load_module("discover_paging", STAGE / "discover.py")
        patcher = mock.patch.object(discover.time, "sleep")
        patcher.start()
        self.addCleanup(patcher.stop)
        first = {"value": [{"id": "C2004A%05d" % n} for n in range(100)]}
        requested = []

        def responses(url):
            requested.append(url)
            return first if "$skip=0" in url else {"value": [{"id": "C2004A00200"}]}

        discover.fetch_json = responses
        rows = discover.page_titles("Tax", "Act")
        self.assertEqual(len(rows), 101)
        self.assertEqual(len(requested), 2)
        self.assertTrue(all("$orderby=id" in url for url in requested), requested)

        discover.fetch_json = lambda url: (
            first if "$skip=0" in url else {"value": [{"id": "C2004A00007"}]})
        with self.assertRaises(SystemExit):
            discover.page_titles("Tax", "Act")


class DownloadValidationTests(unittest.TestCase):
    def _fetch(self, download, dst, body, code="200", content_type="text/html",
               returncode=0):
        def fake_run(args, **kwargs):
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
        for name in ("README.md", "BUILD.md", "retry13.py", "extract.py"):
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
        source = (STAGE / "finalize.py").read_text(encoding="utf-8")
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
        source = (STAGE / "finalize.py").read_text(encoding="utf-8")
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
        # The headline table is where a reader meets those figures first.
        readme = " ".join((REPO / "README.md").read_text(encoding="utf-8").split())
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
        for name in ("README.md", "RELEASE_NOTES.md"):
            with self.subTest(document=name):
                text = " ".join((REPO / name).read_text(encoding="utf-8").split())
                self.assertRegex(text, subset)
                for figure in re.findall(r"([\d,]+) (?:body )?words", text):
                    self.assertLessEqual(int(figure.replace(",", "")), corpus_words)


class DocumentedCommandTests(unittest.TestCase):
    """The commands and module paths the repository's own documents hand a
    reader.  Each one here was wrong on an unmodified checkout."""

    def test_the_documented_verification_command_is_the_one_verify_runs(self):
        """CONTRIBUTING.md promises a standard-library-only run and then
        discovered from `tests`, which errors on the two radar modules that
        import pytest, so the file's only verification instruction was red on a
        clean clone.  verify.yml already scopes discovery to the corpus half."""
        contributing = (REPO / "CONTRIBUTING.md").read_text(encoding="utf-8")
        verify = (REPO / ".github" / "workflows" / "verify.yml").read_text(encoding="utf-8")
        command = "python -m unittest discover -s tests/corpus -t . -v"
        self.assertIn(command, verify)
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
        for folder in ("benchmarks", "fadden", "tax_radar_au", "tests", "tools"):
            for path in sorted((REPO / folder).rglob("*.py")):
                with self.subTest(module=path.relative_to(REPO).as_posix()):
                    with warnings.catch_warnings(record=True) as caught:
                        warnings.simplefilter("always")
                        compile(path.read_text(encoding="utf-8"), str(path), "exec")
                    self.assertEqual([str(w.message) for w in caught], [])


class PiiNameGateTests(unittest.TestCase):
    """PII gates shared by the scans, distribution builder and verifier."""

    # A disciplinary-register row in the all-caps style the old
    # Capitalised-lowercase pair could not see: three surnames the old pattern
    # missed (all-caps, internal capital, apostrophe), three 8-digit
    # registration numbers.
    ALL_CAPS_ROW = ("| SMITH, John | 12345678 | s 30-15 |\n"
                    "| McDonald, Anne | 23456789 | s 30-15 |\n"
                    "| O'Brien, Patrick | 34567890 | s 30-20 |")

    def test_caps_mac_and_apostrophe_surnames_are_visible_to_the_gate(self):
        patterns = load_module("pii_patterns_regression", STAGE / "pii_patterns.py")
        names = patterns.person_names(self.ALL_CAPS_ROW)
        self.assertIn("SMITH, John", names)
        self.assertIn("McDonald, Anne", names)
        self.assertIn("O'Brien, Patrick", names)
        self.assertGreaterEqual(len(names), 3)
        self.assertGreaterEqual(
            len(set(patterns.REGNO.findall(self.ALL_CAPS_ROW))), 3)

    def test_statutory_vocabulary_is_filtered_case_insensitively(self):
        patterns = load_module("pii_patterns_statutory", STAGE / "pii_patterns.py")
        # The statutory list holds capitalised forms, so an all-caps candidate
        # must be checked against it case-insensitively, not waved through.
        self.assertEqual(patterns.person_names(
            "TAXATION ADMINISTRATION PROVISIONS 12345678 23456789 34567890"),
            set())
        self.assertEqual(
            patterns.person_names("Deputy Commissioner of Taxation"), set())

    def test_one_name_and_one_registration_number_trigger_the_distribution_gate(self):
        patterns = load_module("pii_patterns_low_density", STAGE / "pii_patterns.py")

        self.assertTrue(patterns.has_private_person_registration_pair(
            "| Smith, John | 12345678 | s 30-15 |"))
        self.assertFalse(patterns.has_private_person_registration_pair(
            "Deputy Commissioner of Taxation referred to section 12345678."))

    def test_contact_fingerprints_are_normalised_and_tfn_shaped(self):
        patterns = load_module("pii_patterns_contacts", STAGE / "pii_patterns.py")
        first = list(patterns.contact_fingerprints(
            "Email Privacy.Review@Example.Test or phone (02) 1234 5678. "
            "Tax file number: 123 456 789."))
        second = list(patterns.contact_fingerprints(
            "email privacy.review@example.test or phone 02 1234 5678. "
            "tax file number 123456789."))
        self.assertEqual(first, second)
        self.assertEqual([kind for kind, _ in first], ["email", "phone", "tfn"])
        self.assertEqual(list(patterns.contact_fingerprints(
            "The expression tax file number 7/subsection 2 is a heading.")), [])

    def test_phone_gate_sees_unspaced_international_and_short_code_forms(self):
        patterns = load_module("pii_patterns_phone_forms", STAGE / "pii_patterns.py")
        for text in ("0412345678", "0212345678", "0412 345 678",
                     "+61 2 1234 5678", "+61412345678", "+61 (0)2 1234 5678",
                     "+61 (02) 1234 5678", "13 24 68", "132468",
                     "1300 123 456", "1800123456"):
            self.assertTrue(patterns.PHONE.search(text), text)
        for text in ("s 12345678", "section 123 456", "the year 2026",
                     "$1,234,567", "a $1 300 000 000 appropriation",
                     "$100 000 000", "ABN 12 345 678 901"):
            self.assertFalse(patterns.PHONE.search(text), text)
        # National and international notation of one number fingerprint
        # identically, so a single allowlist decision covers both spellings.
        self.assertEqual(list(patterns.contact_fingerprints("(02) 1234 5678")),
                         list(patterns.contact_fingerprints("+61 2 1234 5678")))

    def test_tfn_gate_sees_abbreviated_labels_and_bare_nine_digit_runs(self):
        patterns = load_module("pii_patterns_tfn_forms", STAGE / "pii_patterns.py")
        labelled = list(patterns.contact_fingerprints("TFN: 123 456 782"))
        self.assertEqual([kind for kind, _ in labelled], ["tfn"])
        # The abbreviation, the full label, and the bare grouped or contiguous
        # run all normalise to one fingerprint feeding the same gate.
        for text in ("Tax file number: 123456782", "TFN 123-456-782",
                     "123 456 782", "123456782"):
            self.assertEqual(list(patterns.contact_fingerprints(text)),
                             labelled, text)
        # A bare run must carry the TFN check digit; statute references,
        # years, amounts and ABN groupings stay outside the gate.
        for text in ("section 123 456", "s 12345678", "the year 2026",
                     "$1,234,567", "123 456 789", "$123 456 782",
                     "worth $100 000 000 in total", "ABN 12 345 678 901"):
            self.assertEqual(list(patterns.contact_fingerprints(text)),
                             [], text)

    def test_contact_allowlist_is_bound_to_kind_digest_and_title(self):
        patterns = load_module("pii_patterns_policy", STAGE / "pii_patterns.py")
        rid = "C2004A00001"
        contact = "privacy-review@example.test"
        kind, digest = next(patterns.contact_fingerprints(contact))
        approved = {(kind, digest, rid)}
        self.assertFalse(patterns.unapproved_contact_fingerprints(
            contact.upper(), rid, approved))
        self.assertTrue(patterns.unapproved_contact_fingerprints(
            contact, "F2026N00001", approved))

    def test_tfn_can_never_be_approved_as_an_organisational_contact(self):
        patterns = load_module("pii_patterns_tfn_policy", STAGE / "pii_patterns.py")
        rid = "C2004A00001"
        text = "Tax file number: 123 456 789."
        kind, digest = next(patterns.contact_fingerprints(text))
        self.assertEqual(kind, "tfn")
        self.assertTrue(patterns.unapproved_contact_fingerprints(
            text, rid, {(kind, digest, rid)}))

        document = {"schema_version": 1, "entries": [{
            "kind": kind, "sha256": digest, "register_id": rid,
            "reason": "must never be accepted",
        }]}
        with tempfile.TemporaryDirectory() as tmp:
            policy = Path(tmp) / "policy.json"
            policy.write_text(json.dumps(document), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "allowlist kind"):
                patterns.load_contact_allowlist(policy)

    def test_contact_allowlist_reason_cannot_store_a_raw_identifier(self):
        patterns = load_module("pii_patterns_reason_policy", STAGE / "pii_patterns.py")
        document = {"schema_version": 1, "entries": [{
            "kind": "email", "sha256": "0" * 64,
            "register_id": "C2004A00001",
            "reason": "reviewed privacy-review@example.test",
        }]}
        with tempfile.TemporaryDirectory() as tmp:
            policy = Path(tmp) / "policy.json"
            policy.write_text(json.dumps(document), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "raw identifier"):
                patterns.load_contact_allowlist(policy)

    def test_contact_allowlist_rejects_malformed_and_duplicate_entries(self):
        patterns = load_module("pii_patterns_policy_schema", STAGE / "pii_patterns.py")
        valid = {
            "kind": "email", "sha256": "0" * 64,
            "register_id": "C2004A00001", "reason": "official contact",
        }
        with tempfile.TemporaryDirectory() as tmp:
            policy = Path(tmp) / "policy.json"
            for document in (
                {"schema_version": 2, "entries": []},
                {"schema_version": 1, "entries": [dict(valid, raw="not allowed")]},
                {"schema_version": 1, "entries": [valid, valid]},
            ):
                policy.write_text(json.dumps(document), encoding="utf-8")
                with self.assertRaises(ValueError):
                    patterns.load_contact_allowlist(policy)

    def test_pii_scan_flags_a_register_written_in_capitals(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            build = base / "build"
            build.mkdir()
            for name in ("pii_scan.py", "pii_patterns.py", "corpus_paths.py"):
                shutil.copy2(stage_file(name), build / name)

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

    def test_second_scan_does_not_copy_contact_identifiers_into_logs(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            build = base / "build"
            build.mkdir()
            for name in ("pii_scan2.py", "pii_patterns.py", "corpus_paths.py",
                         "pii_contact_allowlist.json"):
                shutil.copy2(stage_file(name), build / name)
            (build / "pii_flagged.json").write_text("[]\n", encoding="utf-8")

            contact = "privacy-review@example.test"
            rid = "C2004A00001"
            folder = base / "markdown" / rid
            folder.mkdir(parents=True)
            (folder / "sections.jsonl").write_text(
                json.dumps({"row_id": f"{rid}:0001:-", "text": contact}) + "\n",
                encoding="utf-8",
            )

            scan = load_module("pii_scan2_redaction", build / "pii_scan2.py")
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                result = scan.main()

            logged = output.getvalue()
            self.assertEqual(result, 1)
            self.assertNotIn(contact, logged)
            self.assertIn(
                hashlib.sha256(contact.encode("utf-8")).hexdigest()[:16],
                logged,
            )
            self.assertIn(f"{rid}:0001:-", logged)

    def test_second_scan_accepts_only_a_title_bound_organisational_contact(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            build = base / "build"
            build.mkdir()
            for name in ("pii_scan2.py", "pii_patterns.py", "corpus_paths.py"):
                shutil.copy2(stage_file(name), build / name)
            (build / "pii_flagged.json").write_text("[]\n", encoding="utf-8")

            contact = "OFFICIAL@example.test"
            rid = "C2004A00001"
            digest = hashlib.sha256(contact.casefold().encode("utf-8")).hexdigest()
            policy = {"schema_version": 1, "entries": [{
                "kind": "email", "sha256": digest, "register_id": rid,
                "reason": "synthetic organisational contact",
            }]}
            (build / "pii_contact_allowlist.json").write_text(
                json.dumps(policy), encoding="utf-8")
            folder = base / "markdown" / rid
            folder.mkdir(parents=True)
            (folder / "sections.jsonl").write_text(
                json.dumps({"row_id": f"{rid}:0001:-", "text": contact}) + "\n",
                encoding="utf-8")

            scan = load_module("pii_scan2_approved", build / "pii_scan2.py")
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(scan.main(), 0)


class DistributionTests(unittest.TestCase):
    PUBLIC_ID = "C2004A00001"
    PRIVATE_ID = "C2004A00002"
    PUBLIC_NAME = "Public Tax Act"
    PRIVATE_NAME = "Private Disciplinary Register"

    def _managed_publish_paths(self, target):
        target = Path(target)
        return sorted([
            *target.parent.glob(".%s.stage-*" % target.name),
            *target.parent.glob(".%s.backup-*" % target.name),
        ])

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
                "markdown": "markdown/%s/%s.md" % (
                    row["register_id"], row["register_id"]),
                "sections_jsonl": "markdown/%s/sections.jsonl" % row["register_id"],
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
            # generation actually reads. The rewrite must strip this form
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
        dist = load_module("dist_regression", STAGE / "dist.py")
        verify = load_module("dist_verify_regression", STAGE / "dist_verify.py")
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

            policy = json.loads(
                (STAGE / "pii_contact_allowlist.json").read_text(encoding="utf-8"))
            unique_contacts = len({
                (entry["kind"], entry["sha256"]) for entry in policy["entries"]
            })
            removed_md = (output / "REMOVED.md").read_text(encoding="utf-8")
            self.assertIn(
                "%d unique organisational identifiers" % unique_contacts,
                removed_md)

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
        dist = load_module("dist_claims", STAGE / "dist.py")
        verify = load_module("dist_verify_claims", STAGE / "dist_verify.py")
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
            self.assertEqual(self._status(
                out, "no unapproved contact identifiers"), "PASS")
            self.assertEqual(self._status(
                out, "title files stay inside real link-free directories"), "PASS")
            self.assertEqual(self._status(
                out, "all distributed title files are validated UTF-8 text"), "PASS")
            self.assertEqual(self._status(
                out, "title file inventory matches declared outputs"), "PASS")
            self.assertEqual(self._status(
                out, "no title file names private individuals"), "PASS")

            # A distributed title that names disciplined agents with their
            # registration numbers: the one thing dist.py exists to remove.
            rows = output / "markdown" / self.PUBLIC_ID / "sections.jsonl"
            clean_rows = rows.read_text(encoding="utf-8")
            sources_path = output / "sources.json"
            clean_sources = sources_path.read_text(encoding="utf-8")
            row = json.loads(clean_rows)
            rows.write_text(
                json.dumps(dict(row, text=PiiNameGateTests.ALL_CAPS_ROW)) + "\n",
                encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(out, "no row names private individuals"), "FAIL")
            rows.write_text(clean_rows, encoding="utf-8")

            # Organisational contacts already present in the source corpus are
            # explicitly approved by hash and title. A new identifier must not
            # enter the redistributable dataset merely because it is public.
            row = json.loads(clean_rows)
            contact = "new-contact@example.test"
            rows.write_text(
                json.dumps(dict(row, text=contact)) + "\n", encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "no unapproved contact identifiers"), "FAIL")
            self.assertNotIn(contact, out)
            rows.write_text(clean_rows, encoding="utf-8")

            # Valid JSON with the wrong top-level type is malformed JSONL, not
            # an exception that aborts the verifier.
            rows.write_text("[]\n", encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(out, "every JSONL row parses"), "FAIL")
            rows.write_text(clean_rows, encoding="utf-8")

            # An invalid UTF-8 row file is a named failure rather than a
            # decoding traceback from the second row-counting pass.
            rows.write_bytes(b"\xff\xfe")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "all distributed title files are validated UTF-8 text"), "FAIL")
            rows.write_text(clean_rows, encoding="utf-8")

            # The human-readable title and endnotes are redistributed too.
            # Their privacy result must not be inferred from sections.jsonl.
            markdown = output / "markdown" / self.PUBLIC_ID / (self.PUBLIC_ID + ".md")
            clean_markdown = markdown.read_text(encoding="utf-8")
            markdown.write_text(clean_markdown + "\n" + contact + "\n", encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "no unapproved contact identifiers"), "FAIL")
            self.assertNotIn(contact, out)
            markdown.write_text(clean_markdown, encoding="utf-8")

            # The private-person gate covers every redistributed
            # representation, not only the machine-readable row file.
            private_pair = "| Smith, John | 12345678 | s 30-15 |"
            markdown.write_text(
                clean_markdown + "\n" + private_pair + "\n", encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "no title file names private individuals"), "FAIL")
            self.assertEqual(
                self._status(out, "no row names private individuals"), "PASS")
            self.assertNotIn("Smith, John", out)
            markdown.write_text(clean_markdown, encoding="utf-8")

            # One person and one registration number is enough to make a
            # machine-readable row searchable at scale. The diagnostic second
            # pass has always reported this lower threshold; distribution must
            # enforce the same predicate.
            row = json.loads(clean_rows)
            rows.write_text(
                json.dumps(dict(row, text="| Smith, John | 12345678 | s 30-15 |")) + "\n",
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
            self.assertEqual(self._status(
                out, "all distributed title files are validated UTF-8 text"), "FAIL")
            self.assertEqual(self._status(
                out, "title file inventory matches declared outputs"), "FAIL")
            planted.unlink()

            # A binary payload can be strictly decodable UTF-8. Control bytes
            # still make an expected Markdown file non-text and fail closed.
            markdown.write_bytes(b"GIF89a\x00\x01")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "all distributed title files are validated UTF-8 text"), "FAIL")
            markdown.write_text(clean_markdown, encoding="utf-8")

            # A decodable dotfile must not become a silent fourth title output.
            hidden_title_file = markdown.parent / ".operator-notes"
            hidden_title_file.write_text("scratch\n", encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "title file inventory matches declared outputs"), "FAIL")
            hidden_title_file.unlink()

            sources_document = json.loads(clean_sources)
            sources_document["titles"][0]["markdown"] = "../../outside.md"
            sources_path.write_text(json.dumps(sources_document), encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "title file inventory matches declared outputs"), "FAIL")
            sources_path.write_text(clean_sources, encoding="utf-8")

            sources_document = json.loads(clean_sources)
            sources_document["counts"]["acts"] = 9
            sources_document["counts"]["instruments"] = 9
            sources_path.write_text(json.dumps(sources_document), encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "sources count: acts and instruments"), "FAIL")
            sources_path.write_text(clean_sources, encoding="utf-8")

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

            drifted_readme = clean_readme.replace(
                "1 Acts and 0 legislative and notifiable",
                "9 Acts and 9 legislative and notifiable")
            self.assertNotEqual(drifted_readme, clean_readme)
            readme.write_text(drifted_readme, encoding="utf-8")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "README headline matches generated counts"), "FAIL")
            readme.write_text(clean_readme, encoding="utf-8")

            code, _out = self._verify(verify)
            self.assertEqual(code, 0)

    def test_a_stray_entry_under_markdown_is_reported_not_raised(self):
        """The verifier is the last gate before a distribution is published,
        so anything it cannot explain has to come back as a named FAIL.

        register_id() rejects a name that is not a Federal Register
        identifier, which is what keeps a crafted directory name out of a
        path.  Calling it inside the set comprehension that builds `present`
        meant an ordinary stray - a .DS_Store, a hand-copied notes.md, an
        editor's backup directory - raised ValueError out of main() instead.
        The operator saw a traceback rather than a verdict, and nothing in the
        output said which entry caused it or whether any other check had
        passed.  Reject the name, keep it out of every path, and report it."""
        dist = load_module("dist_stray", STAGE / "dist.py")
        verify = load_module("dist_verify_stray", STAGE / "dist_verify.py")
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
            self.assertEqual(
                self._status(out, "no directory beyond what is listed"), "PASS")

            # A hidden file, a loose file and a directory carrying a
            # sections.jsonl so the complete inventory and per-row pass both
            # have to survive them.
            hidden_file = output / "markdown" / ".DS_Store"
            hidden_file.write_text("metadata\n", encoding="utf-8")
            register_shaped_file = output / "markdown" / "F2026N00001"
            register_shaped_file.write_text("not a title directory\n", encoding="utf-8")
            stray_file = output / "markdown" / "notes.md"
            stray_file.write_text("scratch\n", encoding="utf-8")
            stray_dir = output / "markdown" / "draft-backup"
            stray_dir.mkdir()
            (stray_dir / "sections.jsonl").write_text(
                json.dumps({"register_id": "draft-backup", "text": "x"}) + "\n",
                encoding="utf-8")

            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(
                self._status(out, "no directory beyond what is listed"), "FAIL")
            self.assertIn(".DS_Store [file]", out)
            self.assertIn("F2026N00001 [file]", out)
            self.assertIn("notes.md [file]", out)
            self.assertIn("draft-backup [directory]", out)
            # The rest of the run still happens: a stray entry must not cost
            # the operator every other answer the verifier had.
            self.assertEqual(
                self._status(out, "no row names private individuals"), "PASS")

            hidden_file.unlink()
            register_shaped_file.unlink()
            stray_file.unlink()
            shutil.rmtree(stray_dir)
            code, _out = self._verify(verify)
            self.assertEqual(code, 0)

    def test_top_level_title_symlink_is_a_named_failure(self):
        dist = load_module("dist_verify_symlink_fixture", STAGE / "dist.py")
        verify = load_module("dist_verify_top_symlink", STAGE / "dist_verify.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()
            output = Path(dist.DIST)
            verify.DIST = str(output)

            outside = Path(tmp) / "outside-title"
            outside.mkdir()
            link = output / "markdown" / "F2026N00001"
            try:
                link.symlink_to(outside, target_is_directory=True)
            except OSError:
                self.skipTest("symbolic links are unavailable on this platform")
            code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(
                self._status(out, "no directory beyond what is listed"), "FAIL")
            self.assertIn("F2026N00001 [symlink]", out)

    def test_top_level_title_junction_is_a_named_failure(self):
        dist = load_module("dist_verify_junction_fixture", STAGE / "dist.py")
        verify = load_module("dist_verify_top_junction", STAGE / "dist_verify.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()
            output = Path(dist.DIST)
            verify.DIST = str(output)

            junction = output / "markdown" / "F2026N00002"
            junction.mkdir()
            real_isjunction = getattr(os.path, "isjunction", lambda _path: False)

            def classify(candidate):
                return (os.path.normcase(os.fspath(candidate)) ==
                        os.path.normcase(os.fspath(junction))) or real_isjunction(candidate)

            with mock.patch.object(
                    verify.os.path, "isjunction", side_effect=classify, create=True):
                code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(
                self._status(out, "no directory beyond what is listed"), "FAIL")
            self.assertIn("F2026N00002 [junction]", out)

    def test_top_level_title_reparse_point_is_a_named_failure(self):
        dist = load_module("dist_verify_reparse_fixture", STAGE / "dist.py")
        verify = load_module("dist_verify_top_reparse", STAGE / "dist_verify.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()
            output = Path(dist.DIST)
            verify.DIST = str(output)

            reparse = output / "markdown" / "F2026N00003"
            reparse.mkdir()
            real_reparse = verify.is_reparse_point

            def classify(candidate):
                return (os.path.normcase(os.fspath(candidate)) ==
                        os.path.normcase(os.fspath(reparse))) or real_reparse(candidate)

            with mock.patch.object(
                    verify, "is_reparse_point", side_effect=classify):
                code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(
                self._status(out, "no directory beyond what is listed"), "FAIL")
            self.assertIn("F2026N00003 [reparse point]", out)

    def test_nested_title_link_is_a_named_containment_failure(self):
        dist = load_module("dist_verify_nested_link_fixture", STAGE / "dist.py")
        verify = load_module("dist_verify_nested_link", STAGE / "dist_verify.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()
            output = Path(dist.DIST)
            verify.DIST = str(output)
            rows = output / "markdown" / self.PUBLIC_ID / "sections.jsonl"
            real_islink = os.path.islink

            def classify(candidate):
                return (os.path.normcase(os.fspath(candidate)) ==
                        os.path.normcase(os.fspath(rows))) or real_islink(candidate)

            with mock.patch.object(verify.os.path, "islink", side_effect=classify):
                code, out = self._verify(verify)
            self.assertEqual(code, 1)
            self.assertEqual(self._status(
                out, "title files stay inside real link-free directories"), "FAIL")
            self.assertIn(self.PUBLIC_ID, out)

    def test_distribution_rejects_nested_symlinks_before_copying(self):
        dist = load_module("dist_symlink_regression", STAGE / "dist.py")
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

    def test_contact_preflight_preserves_the_existing_distribution(self):
        dist = load_module("dist_contact_preflight", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            policy = build / "pii_contact_allowlist.json"
            policy.write_text(json.dumps({"schema_version": 1, "entries": []}),
                              encoding="utf-8")
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            dist.CONTACT_ALLOWLIST = str(policy)

            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")
            rows = root / "markdown" / self.PUBLIC_ID / "sections.jsonl"
            row = json.loads(rows.read_text(encoding="utf-8"))
            contact = "new-contact@example.test"
            rows.write_text(json.dumps(dict(row, text=contact)) + "\n",
                            encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError,
                                        "unapproved contact identifiers") as raised:
                dist.main()
            self.assertTrue(marker.is_file())
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertNotIn(contact, str(raised.exception))

    def test_contact_preflight_covers_markdown_and_endnotes(self):
        contact = "markdown-only-contact@example.test"
        for relative in (self.PUBLIC_ID + ".md", "endnotes.md"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as tmp:
                dist = load_module(
                    "dist_contact_%s" % relative.replace(".", "_"), STAGE / "dist.py")
                root, build = self._write_fixture(Path(tmp))
                policy = build / "pii_contact_allowlist.json"
                policy.write_text(json.dumps({"schema_version": 1, "entries": []}),
                                  encoding="utf-8")
                dist.ROOT = str(root)
                dist.HERE = str(build)
                dist.DIST = str(root / "dist")
                dist.CONTACT_ALLOWLIST = str(policy)

                existing = Path(dist.DIST)
                existing.mkdir()
                marker = existing / "last-publishable-build.txt"
                marker.write_text("keep me", encoding="utf-8")
                title_file = root / "markdown" / self.PUBLIC_ID / relative
                title_file.write_text(contact + "\n", encoding="utf-8")
                if relative == "endnotes.md":
                    sources_path = root / "sources.json"
                    sources = json.loads(sources_path.read_text(encoding="utf-8"))
                    for title in sources["titles"]:
                        if title["register_id"] == self.PUBLIC_ID:
                            title["endnotes"] = (
                                "markdown/%s/endnotes.md" % self.PUBLIC_ID)
                    sources_path.write_text(json.dumps(sources), encoding="utf-8")

                with self.assertRaisesRegex(
                        RuntimeError, "unapproved contact identifiers") as raised:
                    dist.main()
                self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
                self.assertEqual(self._managed_publish_paths(existing), [])
                self.assertNotIn(contact, str(raised.exception))

    def test_private_pair_preflight_covers_markdown_and_endnotes(self):
        private_pair = "| Smith, John | 12345678 | s 30-15 |"
        for relative in (self.PUBLIC_ID + ".md", "endnotes.md"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as tmp:
                dist = load_module(
                    "dist_private_%s" % relative.replace(".", "_"), STAGE / "dist.py")
                root, build = self._write_fixture(Path(tmp))
                dist.ROOT = str(root)
                dist.HERE = str(build)
                dist.DIST = str(root / "dist")

                existing = Path(dist.DIST)
                existing.mkdir()
                marker = existing / "last-publishable-build.txt"
                marker.write_text("keep me", encoding="utf-8")
                title_file = root / "markdown" / self.PUBLIC_ID / relative
                title_file.write_text(private_pair + "\n", encoding="utf-8")
                if relative == "endnotes.md":
                    sources_path = root / "sources.json"
                    sources = json.loads(sources_path.read_text(encoding="utf-8"))
                    for title in sources["titles"]:
                        if title["register_id"] == self.PUBLIC_ID:
                            title["endnotes"] = (
                                "markdown/%s/endnotes.md" % self.PUBLIC_ID)
                    sources_path.write_text(json.dumps(sources), encoding="utf-8")

                with self.assertRaisesRegex(
                        RuntimeError, "private-person registration pairs") as raised:
                    dist.main()
                self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
                self.assertEqual(self._managed_publish_paths(existing), [])
                self.assertNotIn("Smith, John", str(raised.exception))

    def test_binary_control_preflight_preserves_existing_distribution(self):
        dist = load_module("dist_binary_preflight", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")
            markdown = (root / "markdown" / self.PUBLIC_ID /
                        (self.PUBLIC_ID + ".md"))
            markdown.write_bytes(b"GIF89a\x00\x01")

            with self.assertRaisesRegex(RuntimeError, "unreadable or non-text"):
                dist.main()
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_unexpected_title_dotfile_preflight_preserves_existing_distribution(self):
        dist = load_module("dist_dotfile_preflight", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")
            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")
            hidden = root / "markdown" / self.PUBLIC_ID / ".operator-notes"
            hidden.write_text("scratch\n", encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError, "unexpected file inventory"):
                dist.main()
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_mid_build_failure_preserves_the_prior_distribution(self):
        dist = load_module("dist_atomic_mid_build", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")

            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")

            with mock.patch.object(
                    dist.shutil, "copytree",
                    side_effect=RuntimeError("injected mid-build failure")):
                with self.assertRaisesRegex(RuntimeError, "injected mid-build"):
                    dist.main()

            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_failed_staging_validation_preserves_the_prior_distribution(self):
        dist = load_module("dist_atomic_validation", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")

            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")

            with mock.patch.object(
                    dist, "verify_distribution", return_value=["injected check"]
                    ) as verify:
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "injected check"):
                        dist.main()

            staged = Path(verify.call_args.args[0])
            self.assertEqual(staged.parent, existing.parent)
            self.assertNotEqual(staged, existing)
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_first_promotion_rename_failure_leaves_old_dist_untouched(self):
        dist = load_module("dist_atomic_first_rename", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")

            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")

            with mock.patch.object(
                    dist.os, "rename",
                    side_effect=PermissionError("injected first rename failure")
                    ) as rename:
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(PermissionError, "first rename"):
                        dist.main()

            self.assertEqual(rename.call_count, 1)
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_second_promotion_rename_failure_rolls_back_old_dist(self):
        dist = load_module("dist_atomic_second_rename", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")

            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("keep me", encoding="utf-8")

            real_rename = os.rename
            calls = []

            def fail_second_rename(source, destination):
                calls.append((source, destination))
                if len(calls) == 2:
                    raise PermissionError("injected second rename failure")
                return real_rename(source, destination)

            with mock.patch.object(dist.os, "rename", side_effect=fail_second_rename):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(PermissionError, "second rename"):
                        dist.main()

            self.assertEqual(len(calls), 3)
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep me")
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_successful_promotion_removes_stage_and_backup(self):
        dist = load_module("dist_atomic_success", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            root, build = self._write_fixture(Path(tmp))
            dist.ROOT = str(root)
            dist.HERE = str(build)
            dist.DIST = str(root / "dist")

            existing = Path(dist.DIST)
            existing.mkdir()
            marker = existing / "last-publishable-build.txt"
            marker.write_text("replace me", encoding="utf-8")

            with contextlib.redirect_stdout(io.StringIO()):
                dist.main()

            self.assertFalse(marker.exists())
            self.assertTrue(
                (existing / "markdown" / self.PUBLIC_ID / "sections.jsonl").is_file())
            self.assertEqual(self._managed_publish_paths(existing), [])

    def test_managed_cleanup_refuses_a_path_outside_the_dist_siblings(self):
        dist = load_module("dist_atomic_path_boundary", STAGE / "dist.py")
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "corpus" / "dist"
            target.parent.mkdir()
            unrelated = Path(tmp) / ".dist.stage-unrelated"
            unrelated.mkdir()

            with self.assertRaisesRegex(ValueError, "validated sibling"):
                dist._remove_managed_tree(target, unrelated, "stage")
            self.assertTrue(unrelated.is_dir())

    def test_readme_count_replacement_is_precise_and_linear(self):
        dist = load_module("dist_count_regression", STAGE / "dist.py")
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


class RateParsingTests(unittest.TestCase):
    def test_sentence_boundaries_and_percentage_pattern_remain_precise(self):
        rates = load_module("rates_regression", STAGE / "rates.py")
        text = "The first rule applies.  Next sentence carries 10%.\n\nThird sentence is here."
        self.assertEqual(list(rates.sentences(text)), [
            "The first rule applies.",
            "Next sentence carries 10%.",
            "Third sentence is here.",
        ])
        self.assertEqual(rates.percentage_values("10% and 12.5 %"), ["10%", "12.5 %"])

    def test_amount_scanners_keep_tax_formats_and_ownership_tests(self):
        rates = load_module("rates_amount_regression", STAGE / "rates.py")
        self.assertEqual(rates.money_values("$1,234.50 and $ 20"), ["$1,234.50", "$ 20"])
        self.assertEqual(rates.percentage_values("10% and 12.5 %"), ["10%", "12.5 %"])
        self.assertTrue(rates.is_rate_table(["| $1,000 | 12.5 % |"]))
        self.assertTrue(rates.is_ownership_test("A 75% voting interest is required."))
        self.assertTrue(rates.is_ownership_test("The company is a 100% subsidiary."))
        self.assertFalse(rates.is_ownership_test("The tax rate is 10%."))

    def test_numeric_scanners_complete_linearly_on_long_digit_runs(self):
        rates = load_module("rates_linear_regression", STAGE / "rates.py")
        digits = "9" * 200_000
        started = time.perf_counter()
        self.assertEqual(rates.percentage_values(digits + "."), [])
        self.assertEqual(len(rates.money_values("$" + digits + ".")), 1)
        self.assertTrue(rates.is_ownership_test(digits + "% stake"))
        self.assertLess(time.perf_counter() - started, 2.0)


class PreSectionTextTests(unittest.TestCase):
    """Text ahead of the first section, and across a volume seam."""

    META = {"id": "C2004A00074", "name": "Demo Tax Act",
            "retrieved": "2026-08-11", "versionStart": "2026-01-01"}

    def markdown(self, blocks, **kwargs):
        extract = load_module("extract_presection", STAGE / "extract.py")
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


class BareModeFallbackTests(unittest.TestCase):
    """The bare-mode pass must still run when the only rows the structural pass
    produced are emit's fallback rows."""

    REGISTER_ID = "F2024L00659"

    @staticmethod
    def epub_bytes():
        """An instrument from a plain Word file: no structural classes, no
        CharSectno span, headings as bare numbered paragraphs. Only the
        bare-mode pass can find its sections."""
        body = ["<p>This determination is made under section 30-35.</p>"]
        for n, title in ((1, "Name"), (2, "Commencement"), (3, "Definitions")):
            body.append("<p>%d %s</p>" % (n, title))
            body.append("<p>Operative text for paragraph %d.</p>" % n)
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            archive.writestr("document_1.xhtml",
                             "<html><body>%s</body></html>" % "".join(body))
        return payload.getvalue()

    def _fixture(self, tmp_path):
        build = tmp_path / "build"
        build.mkdir()
        for name in ("extract.py", "corpus_paths.py"):
            shutil.copy2(stage_file(name), build / name)
        epub = tmp_path / "epub" / ("%s.epub" % self.REGISTER_ID)
        epub.parent.mkdir(parents=True)
        epub.write_bytes(self.epub_bytes())
        manifest = [{"id": self.REGISTER_ID, "name": "Demo Notice of Intention",
                     "epub": epub.name, "versionStart": "2024-01-01",
                     "compilationNumber": "1",
                     "collection": "LegislativeInstrument",
                     "sourceUrl": "https://example.test/%s" % self.REGISTER_ID}]
        (build / "manifest_raw.json").write_text(json.dumps(manifest),
                                                 encoding="utf-8")
        return build

    def test_an_emit_fallback_row_does_not_suppress_the_bare_pass(self):
        """The gate before the bare pass asked whether any row carried text.
        Once pre-section text opened a row of its own, that answer became yes
        for a document with no sections at all, so the pass that would have
        found them never ran. The check below it then discarded the same row
        as not evidence of structure, and the document fell through to
        whole_act: 294 titles lost their sections this way, F2024L00659 going
        from 183 rows to 1. Both places must treat the row the same."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build = self._fixture(tmp_path)
            extract = load_module("extract_bare_fallback", build / "extract.py")
            with contextlib.redirect_stdout(io.StringIO()):
                extract.main(None)
            rows = [json.loads(l) for l in
                    (tmp_path / "markdown" / self.REGISTER_ID / "sections.jsonl")
                    .read_text(encoding="utf-8").splitlines() if l.strip()]

            self.assertNotEqual([r["granularity"] for r in rows], ["whole_act"],
                                "the bare pass found sections; do not ship one blob")
            self.assertEqual(sorted(r["section"] for r in rows if r["section"]),
                             ["1", "2", "3"])
            retrievable = "\n".join(r["text"] for r in rows)
            self.assertIn("This determination is made under section 30-35.",
                          retrievable, "pre-section text must still be retrievable")


class ExtractManifestWriteTests(unittest.TestCase):
    def test_a_failed_manifest_dump_leaves_the_previous_one_intact(self):
        """download.py and retry13.py both stage and rename so neither can
        truncate the raw manifest. extract.py opened manifest_md.json directly,
        so a full disk during the dump destroyed it after the whole markdown
        tree had already been written."""
        extract = load_module("extract_manifest_write", STAGE / "extract.py")
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
        return load_module("dist_table_block", STAGE / "dist.py")

    def test_finalize_still_writes_the_paragraph_dist_rewrites(self):
        """If finalize.py rewords the lead, the rewrite silently stops firing
        and the subset ships the full corpus's claim."""
        finalize = (STAGE / "finalize.py").read_text(encoding="utf-8")
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


if __name__ == "__main__":
    unittest.main()
