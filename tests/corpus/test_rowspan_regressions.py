"""Fabricated table coordinates and corpus outputs through the real parser."""
import contextlib
import io
import json
import shutil
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from fadden import extract

from tests.corpus import test_regressions as fixtures
from tests.corpus.test_regressions import STAGE, load_module

LEADING = (
    '<table><thead><tr><th>Category</th><th>Rate</th></tr></thead>'
    '<tbody><tr><td rowspan="2">A</td><td>5%</td></tr>'
    '<tr><td>10%</td></tr></tbody></table>'
)


def rows_for(html):
    parser = extract.Doc()
    parser.feed(html)
    parser.close()
    parser._flush()
    return next(block["rows"] for block in parser.blocks if block["k"] == "table")


def epub_bytes(*tables):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w") as archive:
        archive.writestr("mimetype", "application/epub+zip")
        for number, table in enumerate(tables, 1):
            archive.writestr(f"OEBPS/document_{number}.xhtml", (
                '<?xml version="1.0" encoding="utf-8"?>'
                '<html xmlns="http://www.w3.org/1999/xhtml"><body>'
                '<p class="ActHead5"><span class="CharSectno">1</span> Synthetic rates</p>'
                + table + '</body></html>'
            ).encode("utf-8"))
    return stream.getvalue()


class RowspanCoordinateTests(unittest.TestCase):
    def test_rates_keep_their_columns(self):
        cases = [
            (LEADING, ["", "10%"]),
            ('<table><thead><tr><th>Item</th><th>Band</th><th>Rate</th></tr></thead>'
             '<tbody><tr><td>A</td><td rowspan="2">Group</td><td>5%</td></tr>'
             '<tr><td>B</td><td>10%</td></tr></tbody></table>', ["B", "", "10%"]),
            ('<table><thead><tr><th>Category</th><th>From</th><th>Rate</th></tr></thead>'
             '<tbody><tr><td rowspan="2" colspan="2">All</td><td>5%</td></tr>'
             '<tr><td>10%</td></tr></tbody></table>', ["", "", "10%"]),
        ]
        for html, expected in cases:
            with self.subTest(expected=expected):
                self.assertEqual(rows_for(html)[2], expected)

    def test_zero_span_stops_at_each_explicit_group(self):
        for group in ("thead", "tbody", "tfoot"):
            with self.subTest(group=group):
                html = (f'<table><{group}><tr><td rowspan="0">A</td><td>5%</td></tr>'
                        f'<tr><td>10%</td></tr></{group}>'
                        '<tbody><tr><td>B</td><td>15%</td></tr></tbody></table>')
                self.assertEqual(rows_for(html), [["A", "5%"], ["", "10%"], ["B", "15%"]])

    def test_zero_span_covers_the_direct_row_group(self):
        html = ('<table><tr><td rowspan="0">A</td><td>5%</td></tr>'
                '<tr><td>10%</td></tr><tr><td>15%</td></tr>'
                '<tbody><tr><td>B</td><td>20%</td></tr></tbody></table>')
        self.assertEqual(rows_for(html), [
            ["A", "5%"], ["", "10%"], ["", "15%"], ["B", "20%"],
        ])

    def test_spans_in_a_nested_table_preserve_the_outer_pending_rectangle(self):
        html = ('<table><tr><th>Category</th><th>From</th><th>Rate</th></tr>'
                '<tbody><tr><td rowspan="2" colspan="2">All'
                '<table><tbody><tr><td rowspan="2">Nested</td><td>1%</td></tr>'
                '<tr><td>2%</td></tr></tbody></table></td><td>5%</td></tr>'
                '<tr><td>10%</td></tr></tbody></table>')
        rows = rows_for(html)
        self.assertEqual(rows[2], ["", "", "10%"])
        self.assertEqual(rows[1][1:], ["", "5%"])
        self.assertIn("Nested", rows[1][0])
        self.assertIn("|  | 2% |", rows[1][0])

    def test_missing_and_malformed_attributes_keep_the_single_row_policy(self):
        for value in (None, "", "unknown", "-2", "1", "+1"):
            with self.subTest(value=value):
                attribute = "" if value is None else f' rowspan="{value}"'
                html = (f'<table><tr><td{attribute}>A</td><td>5%</td></tr>'
                        '<tr><td>B</td><td>10%</td></tr></table>')
                self.assertEqual(rows_for(html), [["A", "5%"], ["B", "10%"]])

    def test_excessive_attribute_text_and_positive_span_refuse(self):
        for value, message in (("9" * 10_000, "rowspan"),
                               (str(extract.MAX_TABLE_CELLS + 1), "cell budget")):
            with self.subTest(length=len(value)), self.assertRaisesRegex(ValueError, message):
                rows_for(f'<table><tr><td rowspan="{value}">A</td></tr></table>')

    def test_a_colspan_cannot_overlap_an_inherited_slot(self):
        with self.assertRaisesRegex(ValueError, "overlap"):
            rows_for('<table><tr><td>A</td><td rowspan="2">B</td></tr>'
                     '<tr><td colspan="2">C</td></tr></table>')

    def test_a_positive_span_ends_at_its_group_without_fabricating_rows(self):
        html = ('<table><tbody><tr><td rowspan="4">A</td><td>5%</td></tr></tbody>'
                '<tbody><tr><td>B</td><td>10%</td></tr></tbody></table>')
        self.assertEqual(rows_for(html), [["A", "5%"], ["B", "10%"]])

    def test_a_large_bounded_span_does_not_emit_implied_source_rows(self):
        html = f'<table><tr><td rowspan="{extract.MAX_TABLE_CELLS}">A</td></tr></table>'
        self.assertEqual(rows_for(html), [["A"]])

    def test_positive_rectangles_are_charged_before_expansion(self):
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 5):
            with self.assertRaisesRegex(ValueError, "cell budget"):
                rows_for('<table><tr><td rowspan="3" colspan="2">A</td></tr></table>')

    def test_zero_spans_are_charged_for_explicit_and_implied_rows(self):
        cases = [
            '<table><tbody><tr><td rowspan="0">A</td><td>B</td></tr>'
            '<tr><td>C</td></tr><tr><td>D</td></tr></tbody></table>',
            '<table><tbody><tr><td rowspan="0">A</td><td rowspan="3">B</td></tr>'
            '</tbody></table>',
        ]
        for html in cases:
            with self.subTest(html=html), mock.patch.object(extract, "MAX_TABLE_CELLS", 5):
                with self.assertRaisesRegex(ValueError, "cell budget"):
                    rows_for(html)

    def test_inherited_columns_count_towards_the_row_limit(self):
        html = ('<table><tr><td rowspan="2" colspan="3">A</td></tr>'
                '<tr><td>B</td></tr></table>')
        with mock.patch.object(extract, "MAX_TABLE_COLUMNS", 3):
            with self.assertRaisesRegex(ValueError, "column budget"):
                rows_for(html)

    def test_inherited_blanks_are_bounded_before_the_table_closes(self):
        parser = extract.Doc()
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 20):
            parser.feed('<table><tr><td colspan="4">A</td><td rowspan="0">B</td></tr>')
            with self.assertRaisesRegex(ValueError, "cell budget"):
                parser.feed('<tr><td>C</td></tr>' * 6)
        buffered = sum(map(len, parser._table)) + len(parser._row)
        self.assertLessEqual(buffered, 20)

    def test_inherited_leading_blanks_are_bounded_before_cell_allocation(self):
        parser = extract.Doc()
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 20):
            parser.feed('<table><tr><td>A</td><td>B</td><td>C</td><td>D</td>'
                        '<td rowspan="0">E</td></tr>')
            with self.assertRaisesRegex(ValueError, "cell budget"):
                parser.feed('<tr><td rowspan="0">F</td></tr>' * 6)
        buffered = sum(map(len, parser._table)) + len(parser._row)
        self.assertLessEqual(buffered, 20)

    def test_an_inherited_trailing_slot_remains_blank(self):
        html = ('<table><tr><td>A</td><td rowspan="2">5%</td></tr>'
                '<tr><td>B</td></tr></table>')
        self.assertEqual(rows_for(html), [["A", "5%"], ["B", ""]])

    def test_self_closing_cells_preserve_their_logical_slot(self):
        html = ('<table><tr><td rowspan="2"/><td>5%</td></tr>'
                '<tr><td>10%</td></tr></table>')
        self.assertEqual(rows_for(html), [["", "5%"], ["", "10%"]])

    def test_rowspan_budget_remains_shared_across_epub_members(self):
        table = '<table><tr><td rowspan="3">A</td></tr></table>'
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 5):
            with self.assertRaisesRegex(ValueError, "cell budget"):
                extract.epub_blocks(io.BytesIO(epub_bytes(table, table)))

    def test_positive_lifetimes_expire_after_their_last_covered_row(self):
        html = ('<table><tr><td rowspan="2">A</td><td>5%</td></tr>'
                '<tr><td>10%</td></tr><tr><td>B</td><td>15%</td></tr></table>')
        self.assertEqual(rows_for(html), [["A", "5%"], ["", "10%"], ["B", "15%"]])

    def test_a_caption_ends_the_implicit_zero_span_group(self):
        html = ('<table><tr><td rowspan="0">A</td><td>5%</td></tr>'
                '<caption>Later rows</caption><tr><td>B</td><td>10%</td></tr></table>')
        self.assertEqual(rows_for(html), [["A", "5%"], ["B", "10%"]])

    def test_span_coverage_is_charged_once_and_refusal_preserves_the_counter(self):
        parser = extract.Doc()
        parser.feed('<table><tbody><tr><td rowspan="0">A</td>'
                    '<td rowspan="3">B</td></tr></tbody></table>')
        parser.close()
        self.assertEqual(parser._expanded_cells, 6)
        parser = extract.Doc()
        with mock.patch.object(extract, "MAX_TABLE_CELLS", 5):
            parser.feed('<table><tr><td rowspan="4">A</td><td rowspan="2">B')
            with self.assertRaisesRegex(ValueError, "cell budget"):
                parser.feed('</td></tr></table>')
        self.assertEqual(parser._expanded_cells, 4)
        self.assertEqual(parser._row, ["A"])

    def test_unbalanced_table_structures_refuse_instead_of_shifting_cells(self):
        cases = [
            '<table><td>A</td></table>',
            '<table><tr><td>A<td>B</td></tr></table>',
            '<table><tr><td>A</td><tr><td>B</td></tr></table>',
            '<table><tbody><thead></thead></tbody></table>',
            '<table><tbody><tr><td>A</td></tr></table>',
            '<table><tr><td>A</td></tr>',
        ]
        for html in cases:
            with self.subTest(html=html), self.assertRaisesRegex(ValueError, "table"):
                rows_for(html)


class RowspanPipelineTests(unittest.TestCase):
    def test_columns_and_provenance_reach_sections_and_indexed_rates(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = fixtures.ExtractPipelineTests()
            build, epub = fixture._fixture(root)
            epub.write_bytes(epub_bytes(LEADING))
            fixture._run(build, "2026-10-12")
            markdown, rows = fixture._outputs(root)
            expected = "|  | 10% |"
            self.assertIn(expected, markdown)
            section = next(row for row in rows if "10%" in row["text"])
            self.assertIn(expected, section["text"])
            self.assertIs(section["authorised"], False)
            self.assertEqual(section["compilation_number"], "1")
            shutil.copy2(STAGE / "rates.py", build / "rates.py")
            (root / "sources.json").write_text(json.dumps({
                "titles": [{"register_id": fixture.REGISTER_ID}],
            }), encoding="utf-8")
            rates = load_module("rowspan_rates", build / "rates.py")
            with contextlib.redirect_stdout(io.StringIO()):
                rates.main()
            indexed = [json.loads(line) for line in
                       (Path(rates.OUT) / "rates.jsonl").read_text(encoding="utf-8").splitlines()]
            record = next(row for row in indexed if row["kind"] == "table")
            self.assertIn(expected, record["content"])
            self.assertEqual(record["content"].count("5%"), 1)
            self.assertEqual(record["content"].count("10%"), 1)
            for key in ("register_id", "compilation_number", "source_url", "authorised", "attribution"):
                self.assertEqual(record[key], section[key])

    def test_an_overlap_fails_continues_and_preserves_the_prior_manifest(self):
        for prior in (False, True):
            with self.subTest(prior=prior), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = fixtures.ExtractPipelineTests()
                build, epub = fixture._fixture(root)
                epub.write_bytes(epub_bytes(LEADING))
                manifest = json.loads((build / "manifest_raw.json").read_text(encoding="utf-8"))
                failed = dict(manifest[0], id="F2025L00280", epub="F2025L00280.epub")
                (root / "epub" / failed["epub"]).write_bytes(epub_bytes(
                    '<table><tr><td>A</td><td rowspan="2">B</td></tr>'
                    '<tr><td colspan="2">C</td></tr></table>',
                ))
                (build / "manifest_raw.json").write_text(json.dumps([failed, *manifest]), encoding="utf-8")
                destination = build / "manifest_md.json"
                previous = b'{"previous": true}\n'
                if prior:
                    destination.write_bytes(previous)
                module = load_module("rowspan_failure", build / "extract.py")
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    with self.assertRaisesRegex(RuntimeError, "refusing to write manifest_md"):
                        module.main("2026-10-12")
                self.assertIn("FAIL F2025L00280", output.getvalue())
                self.assertIn(fixture.REGISTER_ID, output.getvalue())
                self.assertTrue((root / "markdown" / fixture.REGISTER_ID / "sections.jsonl").is_file())
                if prior:
                    self.assertEqual(destination.read_bytes(), previous)
                else:
                    self.assertFalse(destination.exists())
