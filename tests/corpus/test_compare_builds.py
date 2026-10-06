"""fadden/compare_builds.py: sections added, removed and changed between two builds."""

from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from fadden import compare_builds

ACT = "C2099A00001"
INSTRUMENT = "F2099L00002"


def row(rid: str, ordinal: int, section: str | None, text: str, *, heading: str = "Heading",
        container: str | None = None, compilation: int = 1) -> dict[str, object]:
    return {
        "register_id": rid, "act": "Sample Act 2099", "compilation_number": str(compilation),
        "compilation_date": f"2026-0{compilation}-01",
        "row_id": "%s:%04d:%s" % (rid, ordinal, section or "-"),
        "section": section, "heading": heading, "container": container, "text": text,
    }


def write_build(root: Path, titles: dict[str, list[dict[str, object]]]) -> None:
    for rid, rows in titles.items():
        directory = root / "markdown" / rid
        directory.mkdir(parents=True)
        (directory / "sections.jsonl").write_text(
            "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")


class CompareBuildsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.old = Path(self.tmp.name) / "old"
        self.new = Path(self.tmp.name) / "new"

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = compare_builds.main([str(self.old), str(self.new), *argv])
        return code, out.getvalue(), err.getvalue()

    def test_an_inserted_section_does_not_shift_its_neighbours_into_changes(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one"), row(ACT, 2, "2", "two")]})
        write_build(self.new, {ACT: [row(ACT, 1, "1", "one", compilation=2),
                                     row(ACT, 2, "1A", "inserted", compilation=2),
                                     row(ACT, 3, "2", "two", compilation=2)]})
        report = compare_builds.compare_title(
            compare_builds.load_rows(str(self.old), ACT), compare_builds.load_rows(str(self.new), ACT))
        self.assertEqual([p["section"] for p in report["added"]], ["1A"])
        self.assertEqual(report["removed"], [])
        self.assertEqual(report["changed"], [])
        self.assertEqual(report["old_compilation"], {"number": "1", "date": "2026-01-01"})
        self.assertEqual(report["new_compilation"], {"number": "2", "date": "2026-02-01"})

    def test_text_and_heading_changes_are_named(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one"), row(ACT, 2, "2", "two"),
                                     row(ACT, 3, "3", "three"), row(ACT, 4, "4", "gone")]})
        write_build(self.new, {ACT: [row(ACT, 1, "1", "one amended"),
                                     row(ACT, 2, "2", "two", heading="Renamed"),
                                     row(ACT, 3, "3", "three amended", heading="Renamed")]})
        report = compare_builds.compare_title(
            compare_builds.load_rows(str(self.old), ACT), compare_builds.load_rows(str(self.new), ACT))
        self.assertEqual([(p["section"], p["change"]) for p in report["changed"]],
                         [("1", "text"), ("2", "heading"), ("3", "text and heading")])
        self.assertEqual([p["section"] for p in report["removed"]], ["4"])

    def test_schedules_restart_numbering_and_repeated_labels_match_in_order(self) -> None:
        old_rows = [row(ACT, 1, "1", "body one"),
                    row(ACT, 2, "1", "schedule one", container="Schedule 1"),
                    row(ACT, 3, "1", "schedule one again", container="Schedule 1")]
        new_rows = [row(ACT, 1, "1", "body one"),
                    row(ACT, 2, "1", "schedule one", container="Schedule 1"),
                    row(ACT, 3, "1", "schedule one again, amended", container="Schedule 1")]
        report = compare_builds.compare_title(old_rows, new_rows)
        self.assertEqual(len(report["changed"]), 1)
        self.assertEqual(report["changed"][0]["container"], "Schedule 1")
        self.assertEqual((report["added"], report["removed"]), ([], []))

    def test_rows_without_a_section_label_match_on_their_heading(self) -> None:
        old_rows = [row(ACT, 1, None, "intro", heading="Introductory material")]
        new_rows = [row(ACT, 1, None, "intro amended", heading="Introductory material"),
                    row(ACT, 2, None, "table", heading="Table of rates")]
        report = compare_builds.compare_title(old_rows, new_rows)
        self.assertEqual([p["heading"] for p in report["changed"]], ["Introductory material"])
        self.assertEqual([p["heading"] for p in report["added"]], ["Table of rates"])

    def test_text_report_lists_changes_and_titles_in_one_build(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one")],
                               INSTRUMENT: [row(INSTRUMENT, 1, "1", "same")]})
        write_build(self.new, {ACT: [row(ACT, 1, "1", "one amended", compilation=2)],
                               "C2099A00003": [row("C2099A00003", 1, "1", "new")]})
        code, out, err = self.run_main()
        self.assertEqual((code, err), (0, ""))
        self.assertIn("C2099A00001: compilation 1 (2026-01-01) to 2 (2026-02-01)", out)
        self.assertIn("  changed (text): s 1", out)
        self.assertIn("1 titles compared, 1 with section changes; 1 only in the old build, "
                      "1 only in the new build.", out)
        self.assertIn("read both compilations on the Register", out)

    def test_json_and_title_filter(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one")],
                               INSTRUMENT: [row(INSTRUMENT, 1, "1", "same")]})
        write_build(self.new, {ACT: [row(ACT, 1, "1", "one", heading="New heading")],
                               INSTRUMENT: [row(INSTRUMENT, 1, "1", "same")]})
        code, out, _ = self.run_main("--title", ACT, "--json")
        self.assertEqual(code, 0)
        lines = [json.loads(line) for line in out.splitlines()]
        self.assertEqual([line["register_id"] for line in lines], [ACT])
        self.assertEqual(lines[0]["changed"][0]["change"], "heading")

    def test_json_lists_titles_present_in_only_one_build(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one")]})
        write_build(self.new, {INSTRUMENT: [row(INSTRUMENT, 1, "1", "same")]})
        code, out, _ = self.run_main("--json")
        self.assertEqual(code, 0)
        self.assertEqual([json.loads(line) for line in out.splitlines()],
                         [{"register_id": ACT, "only_in": "old"},
                          {"register_id": INSTRUMENT, "only_in": "new"}])

    def test_unusable_input_returns_one_with_the_reason(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one")]})
        write_build(self.new, {ACT: [row(ACT, 1, "1", "one")]})
        cases = {
            "not in both builds": ("--title", INSTRUMENT),
            "invalid Federal Register identifier": ("--title", "../escape"),
        }
        for message, argv in cases.items():
            with self.subTest(message=message):
                code, out, err = self.run_main(*argv)
                self.assertEqual((code, out), (1, ""))
                self.assertIn(message, err)
        (self.new / "markdown" / ACT / "sections.jsonl").write_text('["not a row"]\n', encoding="utf-8")
        code, _, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("is not a sections.jsonl row", err)

    def test_directories_that_are_not_titles_are_ignored(self) -> None:
        write_build(self.old, {ACT: [row(ACT, 1, "1", "one")]})
        write_build(self.new, {ACT: [row(ACT, 1, "1", "one")]})
        (self.new / "markdown" / "notes").mkdir()
        (self.new / "markdown" / INSTRUMENT).mkdir()
        self.assertEqual(compare_builds.titles(str(self.new)), {ACT})


if __name__ == "__main__":
    unittest.main()
