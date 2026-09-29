"""fadden/as_at.py: compilation windows against a date, and literal quotations."""

from __future__ import annotations

import contextlib
import datetime
import io
import json
import tempfile
import unittest
from pathlib import Path

from fadden import as_at

D = datetime.date.fromisoformat
CURRENT = {"id": "C2099A00001", "name": "Sample Act 2099",
           "versionStart": "2026-07-01", "retrieved": "2026-08-04"}
SUPERSEDED = {"id": "F2099L00002", "name": "Sample Instrument 2099",
              "versionStart": "2023-09-01", "retrieved": "2026-08-04",
              "version_is_current": False, "current_version_start": "2026-07-07"}


class ClassifyTests(unittest.TestCase):
    def test_a_current_title_is_captured_from_its_start_to_its_retrieval(self) -> None:
        self.assertEqual(as_at.classify(CURRENT, D("2026-07-01")), as_at.CAPTURED)
        self.assertEqual(as_at.classify(CURRENT, D("2026-08-04")), as_at.CAPTURED)

    def test_the_day_before_the_compilation_is_outside_it(self) -> None:
        self.assertEqual(as_at.classify(CURRENT, D("2026-06-30")), as_at.BEFORE_COMPILATION)
        self.assertEqual(as_at.classify(SUPERSEDED, D("2023-08-31")), as_at.BEFORE_COMPILATION)

    def test_the_day_after_retrieval_is_after_retrieval(self) -> None:
        self.assertEqual(as_at.classify(CURRENT, D("2026-08-05")), as_at.AFTER_RETRIEVAL)

    def test_a_superseded_title_stops_the_day_its_successor_starts(self) -> None:
        self.assertEqual(as_at.classify(SUPERSEDED, D("2026-07-06")), as_at.CAPTURED)
        self.assertEqual(as_at.classify(SUPERSEDED, D("2026-07-07")), as_at.SUPERSEDED)

    def test_a_superseded_title_stays_superseded_past_retrieval(self) -> None:
        self.assertEqual(as_at.classify(SUPERSEDED, D("2027-01-01")), as_at.SUPERSEDED)

    def test_dates_that_cannot_all_be_true_are_refused(self) -> None:
        broken = (
            dict(CURRENT, versionStart="2026-99-01"),
            dict(CURRENT, retrieved="4 August 2026"),
            dict(CURRENT, versionStart="2026-09-01"),
            dict(SUPERSEDED, current_version_start="2023-09-01"),
            dict(SUPERSEDED, current_version_start="2026-09-01"),
            {k: v for k, v in SUPERSEDED.items() if k != "current_version_start"},
            dict(SUPERSEDED, version_is_current="false"),
            dict(CURRENT, version_is_current=None),
        )
        for title in broken:
            with self.subTest(title=title), self.assertRaises(ValueError):
                as_at.classify(title, D("2026-07-15"))

    def test_the_committed_manifest_is_consistent(self) -> None:
        titles = json.loads(Path(as_at.MANIFEST).read_text(encoding="utf-8"))
        statuses = {as_at.classify(title, D("2026-08-01")) for title in titles}
        self.assertEqual(statuses, {as_at.CAPTURED, as_at.SUPERSEDED})


class QuoteTests(unittest.TestCase):
    ROW = "(1) You can deduct from your assessable income any loss or outgoing"

    def test_a_word_for_word_quotation_matches(self) -> None:
        self.assertTrue(as_at.quote_in_text("deduct from your assessable income", self.ROW))

    def test_a_paraphrase_or_a_changed_case_does_not(self) -> None:
        self.assertFalse(as_at.quote_in_text("deduct from assessable income", self.ROW))
        self.assertFalse(as_at.quote_in_text("Deduct from your assessable income", self.ROW))

    def test_compatibility_forms_and_the_register_hyphen_match(self) -> None:
        row = "section 8‑1 of the Income Tax Assessment Act 1997"
        self.assertTrue(as_at.quote_in_text("section 8-1 of the Income Tax Assessment Act", row))

    def test_a_blank_quotation_is_refused(self) -> None:
        for quote in ("", "   "):
            with self.subTest(quote=quote), self.assertRaises(ValueError):
                as_at.quote_in_text(quote, self.ROW)


class CommandTests(unittest.TestCase):
    def _run(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = as_at.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def _manifest(self, folder: str, titles: list) -> str:
        path = Path(folder) / "manifest_md.json"
        path.write_text(json.dumps(titles), encoding="utf-8")
        return str(path)

    def test_the_summary_counts_every_status(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            code, out, _ = self._run("2026-07-10", "--manifest", self._manifest(tmp, [CURRENT, SUPERSEDED]))
        self.assertEqual(code, 0)
        self.assertIn("captured: 1", out)
        self.assertIn("superseded: 1 (a later version applies; read the Register)", out)
        self.assertIn("not a finding that the text was the law then", out)

    def test_json_names_each_title_and_its_window(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            code, out, _ = self._run("2025-01-01", "--manifest", self._manifest(tmp, [SUPERSEDED]), "--json")
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out), {
            "register_id": "F2099L00002", "name": "Sample Instrument 2099",
            "status": "captured", "compilation_date": "2023-09-01",
            "superseded_from": "2026-07-07", "retrieved": "2026-08-04",
        })

    def test_an_inconsistent_manifest_exits_one_with_the_reason(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            code, out, err = self._run("2026-07-10", "--manifest",
                                       self._manifest(tmp, [dict(CURRENT, retrieved="bad")]))
        self.assertEqual((code, out), (1, ""))
        self.assertIn("C2099A00001: retrieved is not a YYYY-MM-DD date", err)

    def test_a_date_that_is_not_iso_is_a_usage_error(self) -> None:
        with self.assertRaises(SystemExit) as raised, contextlib.redirect_stderr(io.StringIO()):
            as_at.main(["10/07/2026"])
        self.assertEqual(raised.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
