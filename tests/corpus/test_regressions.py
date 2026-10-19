"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import contextlib
import datetime
import importlib.util
import io
import json
import os
import re
import shutil
import sys
import tempfile
import unittest
import urllib.parse
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

    def test_epub_extraction_closes_the_archive(self):
        extract = load_module("extract_archive_lifecycle", STAGE / "extract.py")
        epub_bytes = self._epub_bytes()
        real_zipfile = zipfile.ZipFile
        opened = []

        def tracked_zipfile(*args, **kwargs):
            archive = real_zipfile(*args, **kwargs)
            opened.append(archive)
            return archive

        with mock.patch("zipfile.ZipFile", side_effect=tracked_zipfile):
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

    def test_a_volume_with_no_boundary_blocks_extraction(self):
        with self.assertRaisesRegex(ValueError, "body boundary"):
            self._parse("extract_volume_gate_bare", self.VOLUME_1, self.VOLUME_2_BARE)

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
    """extract.main() end to end over a 2-volume EPUB."""

    REGISTER_ID = "F2025L00281"

    def test_missing_epub_records_failure_and_processes_later_titles(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            build, _ = self._fixture(base)
            path = build / "manifest_raw.json"
            manifest = json.loads(path.read_text(encoding="utf-8"))
            missing = dict(manifest[0], id="F2025L00280", epub="F2025L00280.epub")
            path.write_text(json.dumps([missing, *manifest]), encoding="utf-8")
            extract = self._load("extract_missing_epub", build)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(RuntimeError, "refusing to write manifest_md"):
                    extract.main()
            self.assertIn("FAIL F2025L00280", output.getvalue())
            self.assertTrue((base / "markdown" / self.REGISTER_ID / "sections.jsonl").is_file())
            self.assertFalse((build / "manifest_md.json").exists())

    def _fixture(self, tmp_path):
        build = tmp_path / "build"
        build.mkdir()
        for name in ("extract.py", "corpus_paths.py", "download.py"):
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

    @staticmethod
    def _load(name, build):
        """Run the checkout's source, measured by coverage, as if deployed in build/."""
        extract = load_module(name, STAGE / "extract.py")
        extract.__file__ = str(build / "extract.py")
        return extract

    def _run(self, build, retrieved):
        extract = self._load("extract_pipeline_%s" % (retrieved or "mtime"), build)
        with contextlib.redirect_stdout(io.StringIO()):
            extract.main(retrieved)

    def _outputs(self, tmp_path):
        folder = tmp_path / "markdown" / self.REGISTER_ID
        rows = [json.loads(line) for line in
                (folder / "sections.jsonl").read_text(encoding="utf-8").splitlines()
                if line.strip()]
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

    def test_every_row_carries_its_compilation_window(self):
        # fadden/as_at.py and consumers read a row's window from the row, so
        # each row must agree with the title it came from.
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, _epub = self._fixture(tmp_path)
            self._run(build, "2026-08-04")
            _markdown, rows = self._outputs(tmp_path)
            self.assertTrue(rows)
            for row in rows:
                self.assertEqual(row["compilation_date"], "2025-01-01")
                self.assertEqual(row["superseded_from"], "2026-03-01")
                self.assertEqual(row["retrieved"], "2026-08-04")

    def test_the_retrieved_date_defaults_to_the_epub_mtime(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, epub = self._fixture(tmp_path)
            stamp = datetime.datetime(2026, 2, 3, 12, 0).timestamp()
            os.utime(epub, (stamp, stamp))
            expected = datetime.date.fromtimestamp(stamp).isoformat()
            self._run(build, None)
            markdown, rows = self._outputs(tmp_path)
            self.assertIn('retrieved: "%s"' % expected, markdown)
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
            self.assertIn('retrieved: "2026-08-04"', markdown)
            self.assertNotIn('retrieved: "2026-02-03"', markdown)
            self.assertEqual(rows[0]["attribution"].count("2026-08-04"), 1)
            manifest = json.loads((build / "manifest_md.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest[0]["retrieved"], "2026-08-04")

    def test_a_retrieved_argument_that_is_not_a_date_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            build, _epub = self._fixture(tmp_path)
            extract = self._load("extract_bad_date", build)
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
                self.assertIn('retrieved: "%s"' % expected, markdown)
                self.assertNotIn(supplied, markdown)
                self.assertNotIn(supplied, rows[0]["attribution"])
                self.assertIn(expected, rows[0]["attribution"])
                manifest = json.loads(
                    (build / "manifest_md.json").read_text(encoding="utf-8"))
                self.assertEqual(manifest[0]["retrieved"], expected)


def basin_table(field):
    """One markdown table of the shape Excise By-law No 127 uses."""
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

    def test_a_valueless_class_attribute_is_not_a_section_number(self):
        """html.parser reports `<span class>` with the value None, which an
        `in` test on the attribute cannot take."""
        extract = load_module("extract_valueless_class", STAGE / "extract.py")
        parser = extract.Doc()
        parser.feed('<html><body><p class>Preamble <span class>1</span> text</p>'
                    '<p class="ActHead5"><span class="CharSectno">2</span> Title</p>'
                    '</body></html>')
        parser._flush()
        paragraphs = [(b["cls"], b["sectno"]) for b in parser.blocks if b["k"] == "p"]
        self.assertEqual(paragraphs, [("", False), ("ActHead5", True)])

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
        self.assertEqual(len(tables), 1)
        self.assertEqual(tables[0][0], ["Item 1", "Rate 5%"])
        self.assertEqual(tables[0][2], ["Item 3", "Rate 7%"])
        self.assertIn("sub a", tables[0][1][0])
        self.assertIn("1%", tables[0][1][0])
        self.assertEqual(tables[0][1][1], "Rate 6%")

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
        self.assertEqual(tables[-1][0][1:], ["", "Rate"])
        self.assertIn("Band", tables[-1][0][0])
        self.assertIn("note", tables[-1][0][0])
        self.assertEqual(tables[-1][1], ["Low", "High", "5%"])


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
                "| current\\_version\\_has\\_no\\_document |", index_md)
            self.assertNotIn("| None | None |", index_md)


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
        finalize.py, so nothing held the sibling.  BUILD.md and check_current.py
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
        for folder in ("benchmarks", "fadden", "tax_radar_au", "tests", "tools"):
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


class DiscoveryLabelTests(unittest.TestCase):
    """Totals spanning three collections are titles, not Acts."""

    TITLES = [
        {"id": "C2099A00001", "name": "Tax Example Act", "collection": "Act",
         "isPrincipal": True, "isInForce": True},
        {"id": "F2099L00001", "name": "Tax Example Instrument",
         "collection": "LegislativeInstrument", "isPrincipal": True, "isInForce": True},
        {"id": "F2099N00001", "name": "Tax Example Notice",
         "collection": "NotifiableInstrument", "isPrincipal": True, "isInForce": True},
    ]

    def test_discovery_totals_count_titles_and_keep_the_collection_breakdown(self):
        discover = load_module("discover_label_regression", STAGE / "discover.py")
        with tempfile.TemporaryDirectory() as temporary:
            stdout = io.StringIO()
            with mock.patch.object(discover, "SCRATCH", temporary), \
                    mock.patch.object(
                        discover, "page_titles",
                        side_effect=lambda kw, coll: [
                            dict(t) for t in self.TITLES if t["collection"] == coll]
                        if kw == "Tax" else []), \
                    mock.patch.object(discover.time, "sleep"), \
                    contextlib.redirect_stdout(stdout):
                discover.main()
            output = stdout.getvalue()
        self.assertIn("total distinct in-force titles: 3", output)
        self.assertNotIn("in-force Act titles", output)
        self.assertNotIn("sample principal Acts", output)
        # The per-collection breakdown the labels rely on is still printed.
        for collection in ("Act", "LegislativeInstrument", "NotifiableInstrument"):
            self.assertIn(collection, output)

    def test_version_resolution_totals_count_titles_and_report_collections(self):
        versions = load_module("versions_label_regression", STAGE / "versions.py")
        with tempfile.TemporaryDirectory() as temporary:
            scratch = Path(temporary)
            (scratch / "titles_all.json").write_text(
                json.dumps(self.TITLES), encoding="utf-8")

            def response(url):
                title_ids = re.findall(r"titleId eq '([^']+)'", urllib.parse.unquote(url))
                return {"value": [{"titleId": rid, "start": "2099-01-01T00:00:00Z",
                                   "compilationNumber": "1", "registerId": "C2099C00001"}
                                  for rid in title_ids]}

            stdout = io.StringIO()
            with mock.patch.object(versions, "SCRATCH", str(scratch)), \
                    mock.patch.object(versions, "fetch_json", side_effect=response), \
                    mock.patch.object(versions.time, "sleep"), \
                    contextlib.redirect_stdout(stdout):
                versions.main()
            output = stdout.getvalue()
        self.assertIn("distinct in-force titles: 3", output)
        self.assertIn("distinct principal titles:   3", output)
        self.assertNotIn("distinct principal Acts", output)
        self.assertIn("by collection: Act 1, LegislativeInstrument 1, "
                      "NotifiableInstrument 1", output)


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
            rows = [json.loads(line) for line in
                    (tmp_path / "markdown" / self.REGISTER_ID / "sections.jsonl")
                    .read_text(encoding="utf-8").splitlines() if line.strip()]

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


if __name__ == "__main__":
    unittest.main()
