"""Parser, chunking and stage-level fallbacks of the EPUB extractor, offline."""

from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fadden import extract

from tests.corpus import test_regressions as regressions
from tests.corpus import test_security_remediation as security

META = {"id": "C2099A00001", "name": "Synthetic Act", "retrieved": "2099-01-01"}


def p(text: str, cls: str = "", sectno: bool = False) -> dict[str, object]:
    return {"k": "p", "cls": cls, "text": text, "sectno": sectno}


def rows(markdown_sections: list[dict[str, object]]) -> list[tuple[object, ...]]:
    return [(s["kind"], s["heading"], [t for t in s["text"] if t.strip()])
            for s in markdown_sections]


class DocParserTests(unittest.TestCase):
    def feed(self, html: str) -> list[dict[str, object]]:
        doc = extract.Doc()
        doc.feed(html)
        doc._flush()
        return doc.blocks

    def test_line_breaks_become_spaces(self) -> None:
        self.assertEqual(self.feed("<p>one<br/>two</p>")[0]["text"], "one two")

    def test_an_image_dimension_too_long_to_parse_counts_as_zero(self) -> None:
        [block] = self.feed('<img src="x.png" width="%s" height="9"/>' % ("1" * 5000))
        self.assertEqual(block["alt"], "[image not described in source: x.png]")

    def test_an_image_inside_a_cell_stays_in_that_cell(self) -> None:
        [table] = self.feed('<table><tr><td><img alt="Formula A"/></td></tr></table>')
        self.assertEqual(table["rows"], [["> Figure: Formula A"]])

    def test_empty_tables_and_stray_closing_tags_emit_nothing(self) -> None:
        self.assertEqual(self.feed("<table><tr><td> </td></tr></table></table><p>after</p>"),
                         [{"k": "p", "cls": "", "text": "after", "sectno": False}])


class TableSplitTests(unittest.TestCase):
    TABLE = ["| Name | Number |", "|---|---|", "| A | 1 |"]

    def test_too_few_or_too_wordy_tables_are_not_split(self) -> None:
        two_tables = "\n".join(self.TABLE + ["between"] + self.TABLE)
        self.assertEqual(extract.table_split(two_tables, "Title"), [])
        wordy = "\n".join(["word " * 60] + self.TABLE + self.TABLE[:0] + ["x"] + self.TABLE
                          + ["y"] + self.TABLE)
        self.assertEqual(extract.table_split(wordy, "Title"), [])

    def test_long_opening_prose_short_fragments_and_a_final_table(self) -> None:
        opening = " ".join(["provision"] * 121)
        body = "\n".join([opening, "Caption"] + self.TABLE + ["", "| lone row |", ""]
                         + self.TABLE * 0 + ["Second"] + ["| N | M |", "|---|---|"]
                         + ["| B | 2 |"] * 40 + [""] + ["| N | M |", "|---|---|"]
                         + ["| C | 3 |"] * 40)
        chunks = extract.table_split(body, "Title")
        self.assertEqual([c["heading"] for c in chunks], [
            "Title — opening provisions", "Title — Caption", "Title — Second",
            "Title — table 3 of 3"])
        self.assertEqual(chunks[0]["kind"], "text_block")
        self.assertTrue(chunks[2]["text"][0].startswith("| lone row |\nSecond\n"))


class ToMarkdownTests(unittest.TestCase):
    def test_bare_mode_reads_headings_from_contents_and_unstyled_lines(self) -> None:
        blocks = [
            {"k": "file"},
            p("Authority ........ 2", "TOC1"),
            p("No", "TOC1"),
            p("Authority"),
            p("This standard is made under the Act.", "BodyText1"),
            p("Application"),
            p("It applies to every entity.", "BodyText1"),
            p("This line ends with a stop."),
            p("Part 2 General rules", "Normal"),
            p("The rules follow.", "Normal"),
        ]
        _md, sections, _endnotes, _long = extract.to_markdown(blocks, META, force_bare=True)
        self.assertEqual(rows(sections), [
            ("container", "Authority", ["This standard is made under the Act."]),
            ("container", "Application", ["It applies to every entity.",
                                          "This line ends with a stop."]),
            ("container", "Part 2 General rules", ["The rules follow."]),
        ])

    def test_long_title_iasb_headings_paragraphs_and_endnotes(self) -> None:
        blocks = [
            {"k": "file"},
            {"k": "table", "rows": [["cover table"]]},
            p("A preamble that is replaced", "Preamble"),
            p("An Act about synthetic things", "LongTitle"),
            p("A second long title that is ignored", "LongTitle"),
            p("Objective", "IASBSectionTitle1"),
            p("The objective is stated here.", "subsection"),
            p("first point", "paragraph"),
            p("Endnotes", "ENotesHeading1"),
            p("Amendment history follows.", "ENotesText"),
            {"k": "img", "alt": "History chart"},
        ]
        md, sections, endnotes, long_title = extract.to_markdown(blocks, META)
        self.assertEqual(long_title, "An Act about synthetic things")
        self.assertIn("\n> An Act about synthetic things\n", md)
        self.assertNotIn("cover table", md)
        self.assertIn(
            ("container", "Objective", ["The objective is stated here.", "- first point"]),
            rows(sections))
        self.assertEqual(endnotes,
                         "Endnotes\n\nAmendment history follows.\n> Figure: History chart")


class ExtractStageTests(unittest.TestCase):
    """extract.main() from the checkout's source, pointed at a temporary build."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.build = self.base / "build"
        self.build.mkdir()
        (self.base / "epub").mkdir()
        self.extract = regressions.load_module("extract_paths", regressions.STAGE / "extract.py")
        self.extract.__file__ = str(self.build / "extract.py")

    def title(self, rid: str, *html: str, **fields: object) -> dict[str, object]:
        (self.base / "epub" / (rid + ".epub")).write_bytes(security.epub_bytes(*html))
        return {"id": rid, "name": "Synthetic %s" % rid, "epub": rid + ".epub",
                "versionStart": "2099-01-01", "collection": "LegislativeInstrument", **fields}

    def run_stage(self, *titles: dict[str, object], retrieved: str | None = "2099-02-01") -> str:
        (self.build / "manifest_raw.json").write_text(json.dumps(list(titles)), encoding="utf-8")
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            self.extract.main(retrieved)
        return stdout.getvalue()

    def jsonl(self, rid: str) -> list[dict[str, object]]:
        path = self.base / "markdown" / rid / "sections.jsonl"
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]

    def test_each_fallback_granularity(self) -> None:
        table = "<table><tr><td>Agent</td><td>Number</td></tr><tr><td>A</td><td>1</td></tr></table>"
        self.run_stage(
            self.title("F2099L00001", "<p>1 Name of instrument</p><p>This is the instrument.</p>"),
            self.title("F2099L00002", "<p>Plain prose with no headings at all.</p>"),
            self.title("F2099L00003", "<p>Made under s 1.</p>" + table * 3),
        )
        self.assertEqual([(r["kind"], r["section"]) for r in self.jsonl("F2099L00001")],
                         [("section", "1")])
        self.assertEqual([r["kind"] for r in self.jsonl("F2099L00002")], ["whole_act"])
        self.assertEqual([r["kind"] for r in self.jsonl("F2099L00003")], ["table_block"] * 3)
        manifest = json.loads((self.build / "manifest_md.json").read_text(encoding="utf-8"))
        self.assertEqual([m["granularity"] for m in manifest],
                         ["section", "whole_act", "table_block"])

    def test_endnotes_get_their_own_provenance_file(self) -> None:
        self.run_stage(self.title(
            "C2099A00002",
            '<p class="ActHead5"><span class="CharSectno">1</span> Short title</p>'
            '<p class="subsection">This Act is the Synthetic Act.</p>'
            '<p class="ENotesHeading1">Endnotes</p><p class="ENotesText">History.</p>'))
        endnotes = (self.base / "markdown" / "C2099A00002" / "endnotes.md").read_text(
            encoding="utf-8")
        self.assertIn('retrieved: "2099-02-01"', endnotes)
        self.assertIn("# Endnotes: Synthetic C2099A00002", endnotes)

    def test_bad_entries_dates_and_chunking_failures_refuse_the_manifest(self) -> None:
        good = self.title("F2099L00004", "<p>1 Name of instrument</p><p>Text.</p>")
        stale_side = self.base / "epub" / "F2099L00004.epub.meta.json"
        stale_side.write_text(json.dumps({"versionStart": "2098-01-01",
                                          "fetched_at": "2098-01-01T00:00:00Z"}), encoding="utf-8")
        naive = self.title("F2099L00005", "<p>Text.</p>", fetched_at="2099-01-01T00:00:00")
        unsplittable = self.title("F2099L00006", "<p>Plain prose.</p>")
        with mock.patch.object(self.extract, "table_split",
                               side_effect=[RuntimeError("chunking dropped 1 body line(s)")]):
            with self.assertRaisesRegex(RuntimeError, "extraction incomplete for 4 title") as caught:
                self.run_stage({"id": "C2099A00009", "name": "Wrong file", "epub": "other.epub"},
                               {"name": "No id", "epub": "x.epub"}, good, naive, unsplittable,
                               retrieved=None)
        message = str(caught.exception)
        for rid in ("C2099A00009", "?", "F2099L00005", "F2099L00006"):
            self.assertIn(rid, message)
        self.assertTrue((self.base / "markdown" / "F2099L00004" / "sections.jsonl").is_file())
        self.assertFalse((self.build / "manifest_md.json").exists())


if __name__ == "__main__":
    unittest.main()
