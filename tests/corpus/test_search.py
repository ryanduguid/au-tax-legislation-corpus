"""fadden/search.py: the local full-text, definitions and reference index.

Every title, row and definition here is fabricated. The integrity cases carry
the defect classes other legislation indexes have reported: rows indexed twice,
files on disk that no inventory lists, and counts that disagree with the
inventory.
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from fadden import search

ACT = "C2099A00001"
OTHER = "C2099A00002"
INSTRUMENT = "F2099L00003"
NBH = "\u2011"  # the Register's non-breaking hyphen

DICTIONARY = "\n\n".join([
    "(1) In this Act:",
    "*widget has the meaning given by section 2" + NBH + "1.",
    "approved gadget means a gadget that:",
    "- (a) is listed in the register; and",
    "- (b) is not a *widget.",
    "but does not include a toy.",
    "R&D entity has the meaning given by section 3" + NBH + "1.",
    "lodgment day: a day on which the office is open.",
    "Note 1: This note is not a definition.",
    "However: this is not one either.",
    "sample levy, in relation to a widget, means the levy imposed by this Act.",
    "\u201cquoted term\u201d includes a term in curly quotes.",
    "(2) In this Act, a reference to a gadget means one in Australia.",
])


def row(rid: str, ordinal: int, section: str | None, text: str, *, heading: str = "Heading",
        container: str | None = None, compilation: str = "1") -> dict[str, Any]:
    return {
        "register_id": rid, "act": "fabricated", "collection": "Act",
        "compilation_number": compilation, "compilation_date": "2099-07-01",
        "version_is_current": True,
        "row_id": "%s:%04d:%s" % (rid, ordinal, section or "-"),
        "section": section, "heading": heading, "container": container,
        "kind": "section", "granularity": "section", "text": text,
        "source_url": "https://example.invalid/%s.epub" % rid,
        "register_page": "https://example.invalid/%s" % rid,
        "authorised": False,
    }


def sample_titles() -> dict[str, tuple[str, list[dict[str, Any]]]]:
    return {
        ACT: ("Sample Levy Act 2099", [
            row(ACT, 1, "1", "This Act is the Sample Levy Act 2099. Divisions 2 and 3 apply, "
                "and Subdivision 2-A sets the rules.", heading="1 Short title"),
            row(ACT, 2, "2" + NBH + "1", "A widget attracts the sample levy. See section 3"
                + NBH + "1 and section 5 of the Other Sample Act 2099.",
                heading="2" + NBH + "1 Widgets"),
            row(ACT, 3, "3" + NBH + "1", "Research entities are covered by paragraph 2"
                + NBH + "1(1)(a) of this Act and section 9 of the ITAA 1997. "
                "The \ufb01nancial year matters.", heading="3" + NBH + "1 Research"),
            row(ACT, 4, "4", DICTIONARY, heading="4 Definitions"),
            row(ACT, 5, "1", "A gadget means a thing in this schedule only.",
                heading="1 Schedule item", container="Schedule 1"),
        ]),
        OTHER: ("Other Sample Act 2099", [
            row(OTHER, 1, "5", "Refer to section 2" + NBH + "1 of the Sample Levy Act 2099. "
                "See Subdivision 2-A of the Sample Levy Act 2099.",
                heading="5 Cross reference"),
            row(OTHER, 2, "6", "widget means a small device.", heading="6 Interpretation"),
        ]),
        INSTRUMENT: ("Sample Levy Rules 2099", [
            row(INSTRUMENT, 1, "1", "Section 1 of the Act and section 1 in Schedule 1 apply.",
                heading="1 Application"),
        ]),
    }


# Structure headings the fabricated Markdown carries before a row, by ordinal.
ACT_STRUCTURE = {
    1: ["## Part 1\u2014Preliminary"],
    2: ["## Part 2\u2014Levy", "### Division 2\u2014Widgets",
        "#### Subdivision 2" + NBH + "A\u2014Basic rules"],
    3: ["### Division 3\u2014Research", "#### Subdivision B\u2014Lettered"],
    4: ["### Division 6.1\u2014Decimal"],
    5: ["## Schedule 1\u2014Items"],
}


def markdown(name: str, rows: list[dict[str, Any]],
             structure: dict[int, list[str]]) -> str:
    lines = ["# " + name]
    for ordinal, entry in enumerate(rows, 1):
        lines += structure.get(ordinal, [])
        lines += ["##### " + entry["heading"], entry["text"]]
    return "\n\n".join(lines) + "\n"


def write_corpus(root: Path, titles: dict[str, tuple[str, list[dict[str, Any]]]] | None = None,
                 *, jsonl_rows: dict[str, int] | None = None) -> None:
    titles = sample_titles() if titles is None else titles
    listed = []
    for rid, (name, rows) in titles.items():
        directory = root / "markdown" / rid
        directory.mkdir(parents=True)
        (directory / "sections.jsonl").write_text(
            "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
        (directory / f"{rid}.md").write_text(
            markdown(name, rows, ACT_STRUCTURE if rid == ACT else {}), encoding="utf-8")
        listed.append({
            "register_id": rid, "name": name, "collection": "Act",
            "compilation_number": "1", "compilation_date": "2099-07-01",
            "version_is_current": rid != OTHER, "retrieved": "2099-08-01",
            "jsonl_rows": (jsonl_rows or {}).get(rid, len(rows)),
            "register_page": "https://example.invalid/%s" % rid,
            "source_url": "https://example.invalid/%s.epub" % rid,
        })
    (root / "sources.json").write_text(
        json.dumps({"retrieved": "2099-08-01", "titles": listed}), encoding="utf-8")


class SearchTestCase(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name) / "corpus"
        self.root.mkdir()
        self.out = Path(tmp.name) / "index.sqlite"

    def built(self) -> sqlite3.Connection:
        write_corpus(self.root)
        search.build(str(self.root), str(self.out))
        connection = search.open_index(str(self.out))
        self.addCleanup(connection.close)
        return connection

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = search.main(list(argv))
        return code, out.getvalue(), err.getvalue()


class BuildTests(SearchTestCase):
    def test_build_records_counts_and_source_digests(self) -> None:
        write_corpus(self.root)
        counts = search.build(str(self.root), str(self.out))
        self.assertEqual(counts["titles"], 3)
        self.assertEqual(counts["rows"], 8)
        connection = search.open_index(str(self.out))
        self.addCleanup(connection.close)
        meta = dict(connection.execute("SELECT key, value FROM meta").fetchall())
        self.assertEqual(meta["sources_sha256"],
                         hashlib.sha256((self.root / "sources.json").read_bytes()).hexdigest())
        self.assertEqual(meta["corpus_retrieved"], "2099-08-01")
        digest = connection.execute(
            "SELECT sections_sha256 FROM titles WHERE register_id = ?", (ACT,)).fetchone()[0]
        self.assertEqual(digest, hashlib.sha256(
            (self.root / "markdown" / ACT / "sections.jsonl").read_bytes()).hexdigest())
        self.assertEqual([p.name for p in self.out.parent.iterdir() if p.name.startswith(".")], [])

    def assert_refused(self, message: str) -> None:
        with self.assertRaisesRegex(search.SearchIndexError, message):
            search.build(str(self.root), str(self.out))
        self.assertFalse(self.out.exists())
        self.assertEqual([p.name for p in self.out.parent.iterdir()], ["corpus"])

    def test_an_existing_destination_is_never_replaced(self) -> None:
        write_corpus(self.root)
        self.out.write_bytes(b"earlier index")
        with self.assertRaisesRegex(search.SearchIndexError, "must not already exist"):
            search.build(str(self.root), str(self.out))
        self.assertEqual(self.out.read_bytes(), b"earlier index")

    def test_a_missing_destination_parent_is_refused(self) -> None:
        write_corpus(self.root)
        with self.assertRaisesRegex(search.SearchIndexError, "parent directory"):
            search.build(str(self.root), str(self.out.parent / "absent" / "index.sqlite"))

    def test_a_title_directory_the_inventory_does_not_list_is_refused(self) -> None:
        write_corpus(self.root)
        (self.root / "markdown" / "C2099A00009").mkdir()
        self.assert_refused("C2099A00009")

    def test_rows_indexed_twice_are_refused(self) -> None:
        titles = sample_titles()
        name, rows = titles[OTHER]
        titles[OTHER] = (name, [rows[0], rows[0]])
        write_corpus(self.root, titles)
        self.assert_refused("expected 'C2099A00002:0002:5'")

    def test_a_row_count_that_disagrees_with_the_inventory_is_refused(self) -> None:
        write_corpus(self.root, jsonl_rows={ACT: 4})
        self.assert_refused("holds 5 rows; sources.json records 4")

    def test_a_row_naming_another_title_is_refused(self) -> None:
        titles = sample_titles()
        name, rows = titles[OTHER]
        rows[0]["register_id"] = ACT
        write_corpus(self.root, titles)
        self.assert_refused("names another title")

    def test_a_row_from_another_compilation_is_refused(self) -> None:
        titles = sample_titles()
        titles[OTHER][1][1]["compilation_number"] = "2"
        write_corpus(self.root, titles)
        self.assert_refused("compilation_number differs")

    def test_a_row_not_marked_unauthorised_is_refused(self) -> None:
        titles = sample_titles()
        del titles[OTHER][1][1]["authorised"]
        write_corpus(self.root, titles)
        self.assert_refused("authorised: false")

    def test_a_malformed_row_or_inventory_is_refused(self) -> None:
        write_corpus(self.root)
        path = self.root / "markdown" / OTHER / "sections.jsonl"
        path.write_text(path.read_text(encoding="utf-8") + "{not json\n", encoding="utf-8")
        self.assert_refused("sections.jsonl:3 is not JSON")
        (self.root / "sources.json").write_text('{"titles": {}}', encoding="utf-8")
        self.assert_refused("not a finished corpus inventory")
        (self.root / "sources.json").unlink()
        self.assert_refused("cannot read sources.json")

    def test_unreadable_or_foreign_files_are_refused(self) -> None:
        write_corpus(self.root)
        path = self.root / "markdown" / OTHER / "sections.jsonl"
        path.write_bytes(b"\n[1]\n")
        self.assert_refused("sections.jsonl:2 is not a sections.jsonl row")
        path.write_bytes(b"\xff\n")
        self.assert_refused("is not UTF-8")
        path.unlink()
        self.assert_refused("cannot read sections.jsonl")

    def test_a_title_without_its_markdown_is_refused(self) -> None:
        write_corpus(self.root)
        (self.root / "markdown" / OTHER / f"{OTHER}.md").unlink()
        self.assert_refused(f"cannot read {OTHER}.md")

    def test_a_failed_promotion_leaves_no_staging_file(self) -> None:
        write_corpus(self.root)
        with mock.patch("fadden.search.os.link", side_effect=OSError("no links here")):
            with self.assertRaisesRegex(OSError, "no links here"):
                search.build(str(self.root), str(self.out))
        self.assertEqual([p.name for p in self.out.parent.iterdir()], ["corpus"])


class QueryTests(SearchTestCase):
    def test_the_non_breaking_hyphen_and_ligatures_match_plain_input(self) -> None:
        connection = self.built()
        self.assertIn(f"{ACT}:0003:3{NBH}1",
                      [h["row_id"] for h in search.query(connection, ["financial"])])
        hits = search.query(connection, ["2-1"])
        self.assertEqual(hits[0]["row_id"], f"{ACT}:0002:2{NBH}1")

    def test_a_row_whose_section_is_the_query_ranks_first(self) -> None:
        connection = self.built()
        hits = search.query(connection, ["3-1"])
        self.assertEqual(hits[0]["section"], f"3{NBH}1")
        self.assertGreater(len(hits), 1)

    def test_query_syntax_in_user_input_is_searched_as_text(self) -> None:
        connection = self.built()
        for words in (['AND('], ['"widget'], ['widget*'], ['NEAR(widget', 'levy)']):
            with self.subTest(words=words):
                search.query(connection, words)
        with self.assertRaisesRegex(search.SearchIndexError, "no searchable words"):
            search.query(connection, ["()", '"'])

    def test_hits_carry_provenance_and_respect_title_and_limit(self) -> None:
        connection = self.built()
        hits = search.query(connection, ["widget"], title=OTHER)
        self.assertEqual({h["register_id"] for h in hits}, {OTHER})
        hit = hits[0]
        self.assertFalse(hit["authorised"])
        self.assertFalse(hit["version_is_current"])
        for field in ("act", "compilation_number", "compilation_date", "retrieved",
                      "register_page", "source_url", "sections_sha256", "snippet", "text"):
            self.assertIn(field, hit)
        self.assertEqual(len(search.query(connection, ["widget"], limit=1)), 1)


class DefinitionTests(SearchTestCase):
    def test_definitions_split_on_their_opening_words(self) -> None:
        found = dict(search.definitions(DICTIONARY))
        self.assertEqual(list(found), ["widget", "approved gadget", "R&D entity",
                                       "lodgment day", "sample levy", "quoted term"])
        self.assertEqual(found["approved gadget"].split("\n\n"), [
            "approved gadget means a gadget that:", "- (a) is listed in the register; and",
            "- (b) is not a *widget.", "but does not include a toy."])
        self.assertTrue(found["lodgment day"].startswith("lodgment day: a day"))
        self.assertNotIn("(2)", found["quoted term"])

    def test_text_before_the_first_definition_is_not_attached_to_one(self) -> None:
        self.assertEqual(search.definitions("In this Act:\n\n- (a) stray item\n\n\n\n"
                                            "plain text\n\nthing means an item."),
                         [("thing", "thing means an item.")])

    def test_a_lead_in_is_not_part_of_the_term(self) -> None:
        self.assertEqual(search.definitions(
            "In this Division, unless the contrary intention appears, thing means an item."),
            [("thing", "In this Division, unless the contrary intention appears, "
                       "thing means an item.")])

    def test_define_matches_folded_terms_across_titles(self) -> None:
        connection = self.built()
        found = search.define(connection, "WIDGET")
        self.assertEqual([(d["register_id"], d["section"]) for d in found],
                         [(ACT, "4"), (OTHER, "6")])
        self.assertEqual(found[0]["term"], "widget")
        self.assertEqual([d["term"] for d in search.define(connection, "r&d entity")],
                         ["R&D entity"])
        self.assertEqual([d["register_id"] for d in search.define(connection, "widget",
                                                                     title=OTHER)], [OTHER])

    def test_a_term_prefix_finds_longer_terms_only_without_an_exact_match(self) -> None:
        connection = self.built()
        self.assertEqual([d["term"] for d in search.define(connection, "approved")],
                         ["approved gadget"])
        self.assertEqual(search.define(connection, "approved_"), [])
        self.assertEqual(search.define(connection, "approved%"), [])

    def test_only_rows_headed_as_definitions_are_read(self) -> None:
        connection = self.built()
        self.assertEqual(search.define(connection, "gadget"), [])
        code, out, _err = self.run_main("define", str(self.out), "gadget")
        self.assertEqual(code, 0)
        self.assertIn("not a finding that it has its ordinary meaning", out)


class ReferenceTests(SearchTestCase):
    def cited(self, text: str) -> list[tuple[str, str, str | None]]:
        return [(kind, key, act) for kind, key, act, _ in search.citations(text)]

    def test_citations_name_the_section_and_any_other_instrument(self) -> None:
        text = ("section 8" + NBH + "1, paragraph 8-1(2)(a) of this Act, section 5 of the "
                "Other Sample Act 2099, subsection 995-1(1) of the *ITAA 1997 but includes "
                "the rest, section 12-35 in Schedule 1 to the Taxation Administration Act 1953, "
                "section 2 of the A New Tax System (Goods and Services Tax) Act 1999, "
                "section 3 of the Act (with changes), section 4 in Schedule 2 and "
                "subsection (3).")
        self.assertEqual([(key, act) for _, key, act in self.cited(text)], [
            ("8-1", None), ("8-1", None), ("5", "Other Sample Act 2099"),
            ("995-1", "ITAA 1997"), ("12-35", "Taxation Administration Act 1953"),
            ("2", "A New Tax System (Goods and Services Tax) Act 1999"), ("3", "Act"),
            ("4", None)])

    def test_a_list_gives_one_reference_per_item(self) -> None:
        cases = {
            "sections 8-1 and 8-5 apply": [("section", "8-1", None), ("section", "8-5", None)],
            "sections 8" + NBH + "1, 8" + NBH + "5 and 8" + NBH + "10 of the ITAA 1997.": [
                ("section", "8-1", "ITAA 1997"), ("section", "8-5", "ITAA 1997"),
                ("section", "8-10", "ITAA 1997")],
            "subsections 6-5(2), 6-10(4), or 6-15(1)": [
                ("section", "6-5", None), ("section", "6-10", None), ("section", "6-15", None)],
            "Section 23 and section 23AA": [("section", "23", None), ("section", "23AA", None)],
            "Divisions 40, 43 or 83A of this Act": [
                ("division", "40", None), ("division", "43", None), ("division", "83A", None)],
            "Subdivisions 20-A and 20" + NBH + "B": [
                ("subdivision", "20-A", None), ("subdivision", "20-B", None)],
            "Division 40 and Subdivision 20-A": [
                ("division", "40", None), ("subdivision", "20-A", None)],
            "section 10.": [("section", "10", None)],
            "section 8-10.": [("section", "8-10", None)],
            "section 123 and Division 10A": [
                ("section", "123", None), ("division", "10A", None)],
        }
        for text, expected in cases.items():
            with self.subTest(text=text):
                self.assertEqual(self.cited(text), expected)

    def test_text_that_only_looks_like_a_reference_is_not_read(self) -> None:
        cases = {
            "sections 8-1 to 8-10": [("section", "8-1", None)],
            "section 8-1 and 2 other provisions": [("section", "8-1", None)],
            "section 23 and 2-1 apply": [("section", "23", None)],
            "subsections 8-1(1) and (2)": [("section", "8-1", None)],
            "section 8-1, the Commissioner": [("section", "8-1", None)],
            "Subdivision B of Division 3": [("division", "3", None)],
            "Division 6.1 applies": [],
            "Division 8-1, Subdivision 40 and section 20-A": [],
            "this Division, the Subdivision and Part 3-1": [],
            "the divisional manager 4 and subsectional 5": [],
        }
        for text, expected in cases.items():
            with self.subTest(text=text):
                self.assertEqual(self.cited(text), expected)

    def test_decimal_labels_are_not_truncated_to_integer_prefixes(self) -> None:
        for keyword in ("section", "Division"):
            for label in ("10.1", "123.45", "10A.1"):
                with self.subTest(keyword=keyword, label=label):
                    self.assertEqual(self.cited(f"{keyword} {label} applies."), [])
        for label in ("8-10.1", "8" + NBH + "10.1"):
            with self.subTest(label=label):
                self.assertEqual(self.cited(f"section {label} applies."), [])

    def test_decimal_divisions_do_not_create_outbound_or_inbound_references(self) -> None:
        rows = [row(ACT, 1, "1", "Division 10.1 applies.", heading="1 Scope"),
                row(ACT, 2, "2", "Text.", heading="2 Item")]
        write_corpus(self.root, {ACT: ("Sample Levy Act 2099", rows)})
        (self.root / "markdown" / ACT / f"{ACT}.md").write_text(
            markdown("Sample Levy Act 2099", rows, {2: ["### Division 1\u2014Items"]}),
            encoding="utf-8")
        search.build(str(self.root), str(self.out))
        connection = search.open_index(str(self.out))
        self.addCleanup(connection.close)
        self.assertEqual(search.refs(connection, f"{ACT}:0001:1")["cites"], [])
        self.assertEqual(search.refs(connection, f"{ACT}:0002:2")["cited_by"], [])

    def test_structure_follows_the_markdown_headings(self) -> None:
        connection = self.built()
        places = connection.execute(
            "SELECT division_key, subdivision_key FROM rows WHERE register_id = ? ORDER BY id",
            (ACT,)).fetchall()
        self.assertEqual([tuple(place) for place in places], [
            (None, None), ("2", "2-A"), ("3", None), (None, None), (None, None)])
        rows = [{"heading": "Introductory material"}, {"heading": "1  Short\ntitle"},
                {"heading": "1 Short title"}]
        self.assertEqual(list(search.structure(rows, "### Division 7A\u2014Loans\n"
                                                     "##### 1 Short title\n")),
                         [(None, None), ("7A", None), (None, None)])

    def test_refs_resolve_within_and_across_titles(self) -> None:
        connection = self.built()
        report = search.refs(connection, f"{ACT}:0002:2{NBH}1")
        self.assertEqual((report["division"], report["subdivision"]), ("2", "2-A"))
        self.assertEqual([(c["register_id"], [r["row_id"] for r in c["rows"]])
                          for c in report["cites"]],
                         [(ACT, [f"{ACT}:0003:3{NBH}1"]), (OTHER, [f"{OTHER}:0001:5"])])
        self.assertEqual(sorted({(c["row_id"], c["kind"]) for c in report["cited_by"]}), [
            (f"{ACT}:0001:1", "division"), (f"{ACT}:0001:1", "subdivision"),
            (f"{ACT}:0003:3{NBH}1", "section"), (f"{ACT}:0004:4", "section"),
            (f"{OTHER}:0001:5", "section"), (f"{OTHER}:0001:5", "subdivision")])

    def test_division_and_subdivision_references_resolve_to_their_rows(self) -> None:
        connection = self.built()
        cites = search.refs(connection, f"{ACT}:0001:1")["cites"]
        self.assertEqual([(c["kind"], c["key"], [r["row_id"] for r in c["rows"]]) for c in cites], [
            ("division", "2", [f"{ACT}:0002:2{NBH}1"]),
            ("division", "3", [f"{ACT}:0003:3{NBH}1"]),
            ("subdivision", "2-A", [f"{ACT}:0002:2{NBH}1"])])
        across = search.refs(connection, f"{OTHER}:0001:5")["cites"]
        self.assertEqual([(c["kind"], c["register_id"], len(c["rows"])) for c in across],
                         [("section", ACT, 1), ("subdivision", ACT, 1)])

    def test_text_output_shortens_a_long_target_list(self) -> None:
        rows = [row(ACT, 1, "1", "Division 5 applies.", heading="1 Scope")] + [
            row(ACT, n, f"5-{n}", "Text.", heading=f"5-{n} Item") for n in range(2, 9)]
        write_corpus(self.root, {ACT: ("Sample Levy Act 2099", rows)})
        (self.root / "markdown" / ACT / f"{ACT}.md").write_text(
            markdown("Sample Levy Act 2099", rows, {2: ["### Division 5\u2014Items"]}),
            encoding="utf-8")
        search.build(str(self.root), str(self.out))
        code, out, _err = self.run_main("refs", str(self.out), f"{ACT}:0001:1")
        self.assertEqual(code, 0)
        self.assertIn(f"cites division 5 (Division 5): {ACT}:0002:5-2, ", out)
        self.assertIn("and 2 more", out)
        code, out, _err = self.run_main("refs", str(self.out), f"{ACT}:0002:5-2")
        self.assertIn(f"cited by {ACT}:0001:1 (1 Scope): Division 5", out)

    def test_unresolved_and_repeated_targets_are_reported_as_they_are(self) -> None:
        connection = self.built()
        report = search.refs(connection, f"{ACT}:0003:3{NBH}1")
        unresolved = [c for c in report["cites"] if c["register_id"] is None]
        self.assertEqual([(c["cited_act"], c["rows"]) for c in unresolved], [("ITAA 1997", [])])
        rules = search.refs(connection, f"{INSTRUMENT}:0001:1")
        self.assertEqual([c["cited_act"] for c in rules["cites"]], ["Act", None])
        self.assertEqual([r["row_id"] for r in rules["cites"][1]["rows"]],
                         [f"{INSTRUMENT}:0001:1"])
        with self.assertRaisesRegex(search.SearchIndexError, "not in the index"):
            search.refs(connection, "C2099A00001:0099:9")

    def test_a_schedule_label_repeated_in_a_title_returns_every_row(self) -> None:
        connection = self.built()
        cites = search.refs(connection, f"{OTHER}:0001:5")["cites"]
        self.assertEqual([r["row_id"] for r in cites[0]["rows"]], [f"{ACT}:0002:2{NBH}1"])
        report = search.refs(connection, f"{ACT}:0001:1")
        self.assertEqual(report["cited_by"], [])


class CommandLineTests(SearchTestCase):
    def test_commands_print_provenance_and_the_notice(self) -> None:
        write_corpus(self.root)
        code, out, _err = self.run_main("build", str(self.root), "--out", str(self.out))
        self.assertEqual((code, out), (0, "indexed 3 titles, 8 rows, 7 definitions and "
                                          "13 section references\n"))
        for argv in (("query", str(self.out), "widget"),
                     ("define", str(self.out), "approved", "gadget"),
                     ("refs", str(self.out), f"{ACT}:0002:2{NBH}1")):
            with self.subTest(command=argv[0]):
                code, out, _err = self.run_main(*argv)
                self.assertEqual(code, 0)
                self.assertIn(search.NOTICE, out)
                self.assertIn("2099-08-01", out)

    def test_json_output_is_one_object_per_line(self) -> None:
        self.built()
        code, out, _err = self.run_main("query", str(self.out), "widget", "--json")
        self.assertEqual(code, 0)
        hits = [json.loads(line) for line in out.splitlines()]
        self.assertTrue(hits and all(h["authorised"] is False for h in hits))
        code, out, _err = self.run_main("refs", str(self.out), f"{ACT}:0001:1", "--json")
        self.assertEqual(json.loads(out)["cited_by"], [])
        code, out, _err = self.run_main("define", str(self.out), "widget", "--json")
        self.assertEqual([json.loads(line)["register_id"] for line in out.splitlines()],
                         [ACT, OTHER])
        code, _out, err = self.run_main("define", str(self.out), "*")
        self.assertEqual((code, err), (1, "error: the term is empty\n"))

    def test_errors_exit_one_without_a_traceback(self) -> None:
        self.out.write_bytes(b"not sqlite")
        for argv in (("query", str(self.out), "widget"),
                     ("query", str(self.out.parent / "absent.sqlite"), "widget"),
                     ("build", str(self.root), "--out", str(self.out))):
            with self.subTest(argv=argv):
                code, out, err = self.run_main(*argv)
                self.assertEqual((code, out), (1, ""))
                self.assertTrue(err.startswith("error: "))

    def test_an_index_from_another_schema_is_refused(self) -> None:
        self.built()
        connection = sqlite3.connect(self.out)
        with connection:
            connection.execute("UPDATE meta SET value = '0' WHERE key = 'schema'")
        connection.close()
        with self.assertRaisesRegex(search.SearchIndexError, "different schema"):
            search.open_index(str(self.out))


if __name__ == "__main__":
    unittest.main()
