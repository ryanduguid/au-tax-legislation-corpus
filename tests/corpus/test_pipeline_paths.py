"""Failure and edge paths of the smaller builder stages and their shared file helpers."""

from __future__ import annotations

import contextlib
import io
import json
import re
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

from fadden import corpus_paths, pii_patterns

from tests.corpus import test_regressions as regressions

STAGE = regressions.STAGE
RID = "C2099A00001"


class TemporaryDirectoryTestCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)


class BuilderLayoutTests(TemporaryDirectoryTestCase):
    def test_a_fadden_directory_outside_a_checkout_is_refused(self) -> None:
        script = self.base / "fadden" / "stage.py"
        script.parent.mkdir()
        with self.assertRaisesRegex(RuntimeError, "corpus stages require a source checkout"):
            corpus_paths.require_builder_layout(script)

    def test_an_unrecognised_script_directory_is_refused(self) -> None:
        script = self.base / "elsewhere" / "stage.py"
        script.parent.mkdir()
        with self.assertRaisesRegex(RuntimeError, "corpus stages require a source checkout"):
            corpus_paths.require_builder_layout(script)

    def test_a_root_without_markdown_has_no_unlisted_titles(self) -> None:
        self.assertIsNone(corpus_paths.refuse_unlisted_titles(self.base, set()))


class AtomicTextWriterTests(TemporaryDirectoryTestCase):
    def test_a_colliding_staging_name_is_skipped(self) -> None:
        destination = self.base / "out.txt"
        taken, free = uuid.UUID(int=1), uuid.UUID(int=2)
        (self.base / f".out.txt.{taken.hex}.tmp").write_text("someone else's", encoding="utf-8")
        with mock.patch.object(corpus_paths.uuid, "uuid4", side_effect=[taken, free]):
            with corpus_paths.atomic_text_writer(destination) as target:
                target.write("mine")
        self.assertEqual(destination.read_text(encoding="utf-8"), "mine")
        self.assertEqual(
            (self.base / f".out.txt.{taken.hex}.tmp").read_text(encoding="utf-8"), "someone else's")

    def _fail_to_open(self, cleanup_error: OSError | None) -> OSError:
        original = OSError(24, "simulated descriptor exhaustion")
        unlink = mock.patch.object(Path, "unlink", side_effect=cleanup_error) if cleanup_error \
            else contextlib.nullcontext()
        with mock.patch.object(corpus_paths.os, "fdopen", side_effect=original), unlink:
            with self.assertRaises(OSError) as caught:
                with corpus_paths.atomic_text_writer(self.base / "out.txt"):
                    self.fail("the writer must not yield without a stream")
        self.assertIs(caught.exception, original)
        self.assertFalse((self.base / "out.txt").exists())
        return caught.exception

    def test_a_stream_that_cannot_open_removes_its_staging_file(self) -> None:
        self._fail_to_open(None)
        self.assertEqual(list(self.base.iterdir()), [])

    def test_a_staging_file_already_gone_keeps_the_original_error(self) -> None:
        error = self._fail_to_open(FileNotFoundError(2, "already gone"))
        self.assertIsNone(error.__cause__)

    def test_a_staging_file_that_cannot_be_removed_is_chained(self) -> None:
        cleanup = PermissionError(13, "locked")
        error = self._fail_to_open(cleanup)
        self.assertIs(error.__cause__, cleanup)


class ContactAllowlistTests(TemporaryDirectoryTestCase):
    VALID = {"kind": "email", "sha256": "0" * 64, "register_id": RID, "reason": "official contact"}

    def test_malformed_documents_and_fields_are_refused(self) -> None:
        cases = {
            "invalid contact allowlist document": ["not an object"],
            "invalid contact allowlist digest": {"schema_version": 1, "entries": [
                dict(self.VALID, sha256="A" * 64)]},
            "invalid contact allowlist register id": {"schema_version": 1, "entries": [
                dict(self.VALID, register_id="c2099a00001")]},
            "invalid contact allowlist reason": {"schema_version": 1, "entries": [
                dict(self.VALID, reason="  ")]},
        }
        policy = self.base / "policy.json"
        for message, document in cases.items():
            with self.subTest(message=message):
                policy.write_text(json.dumps(document), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "^%s$" % message):
                    pii_patterns.load_contact_allowlist(policy)


class DiscoverTests(TemporaryDirectoryTestCase):
    def test_a_title_matched_by_two_keywords_is_listed_once(self) -> None:
        discover = regressions.load_module("discover_paths", STAGE / "discover.py")
        discover.SCRATCH = str(self.base)
        discover.COLLECTIONS = ["Act"]
        discover.KEYWORDS = ["Tax", "Income"]
        title = {"id": RID, "name": "Income Tax Act", "isPrincipal": True}
        stdout = io.StringIO()
        with mock.patch.object(discover, "page_titles", return_value=[title]), \
                mock.patch.object(discover.time, "sleep"), contextlib.redirect_stdout(stdout):
            discover.main()
        self.assertEqual(json.loads((self.base / "titles_all.json").read_text(encoding="utf-8")),
                         [title])
        self.assertRegex(stdout.getvalue(), r"Act +Income +matched +1, new +0")


class Probe13Tests(TemporaryDirectoryTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.probe13 = regressions.load_module("probe13_paths", STAGE / "probe13.py")
        self.probe13.SCRATCH = str(self.base)
        (self.base / "manifest_raw.json").write_text(json.dumps([
            {"id": RID, "name": "Synthetic Tax Act", "epub": None},
            {"id": "C2099A00002", "name": "Downloaded Act", "epub": "C2099A00002.epub"},
        ]), encoding="utf-8")

    def run_probe(self, response: object) -> str:
        stdout = io.StringIO()
        with mock.patch.object(self.probe13, "fetch_json", return_value=response), \
                mock.patch.object(self.probe13.time, "sleep"), contextlib.redirect_stdout(stdout):
            self.probe13.main()
        return stdout.getvalue()

    def test_the_latest_published_compilation_is_recorded(self) -> None:
        versions = [
            {"titleId": RID, "start": "2099-07-01T00:00:00", "isCurrent": True,
             "compilationNumber": "3", "registerId": None},
            {"titleId": RID, "start": "2099-01-01T00:00:00", "isCurrent": False,
             "compilationNumber": "2", "registerId": "C2099C00002"},
            {"titleId": RID, "start": "2098-01-01T00:00:00", "isCurrent": False,
             "compilationNumber": "1", "registerId": "C2098C00001"},
        ]
        stdout = self.run_probe({"value": versions})
        [probe] = json.loads((self.base / "probe13.json").read_text(encoding="utf-8"))
        self.assertEqual(probe, {"id": RID, "name": "Synthetic Tax Act", "n_versions": 3,
                                 "current_has_doc": False, "latest_doc": versions[1]})
        self.assertRegex(stdout, r"vers=3 +cur_doc=False latest=C2099C00002 2099-01-01")

    def test_a_title_with_no_published_document_has_no_latest(self) -> None:
        self.run_probe({"value": []})
        [probe] = json.loads((self.base / "probe13.json").read_text(encoding="utf-8"))
        self.assertIsNone(probe["latest_doc"])

    def test_a_failed_lookup_refuses_to_write(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "version lookup failed for C2099A00001"):
            self.run_probe(None)
        self.assertFalse((self.base / "probe13.json").exists())


class Retry13Tests(TemporaryDirectoryTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.retry13 = regressions.load_module("retry13_paths", STAGE / "retry13.py")
        self.retry13.SCRATCH = str(self.base)
        self.retry13.EPUB_DIR = str(self.base / "epub")
        (self.base / "epub").mkdir()
        self.manifest = [{"id": RID, "name": "Synthetic Tax Act", "epub": None,
                          "versionStart": "2099-03-01"}]
        (self.base / "manifest_raw.json").write_text(json.dumps(self.manifest), encoding="utf-8")

    def write_probe(self, entries: list[dict[str, object]]) -> None:
        (self.base / "probe13.json").write_text(json.dumps(entries), encoding="utf-8")

    def recoverable(self) -> None:
        self.write_probe([{"id": RID, "latest_doc": {
            "start": "2099-01-01T00:00:00", "compilationNumber": "2", "registerId": "C2099C00002"}}])

    @staticmethod
    def fetch(_url: str, destination: str) -> tuple[bool, str, str, int, None]:
        Path(destination).write_bytes(b"PK-fake-epub")
        return True, "200", "application/epub+zip", 12, None

    def test_nothing_to_retry_writes_only_the_empty_audit_record(self) -> None:
        self.write_probe([])
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            self.retry13.main()
        self.assertEqual(json.loads((self.base / "retry13_patch.json").read_text(encoding="utf-8")), [])
        self.assertEqual(json.loads((self.base / "manifest_raw.json").read_text(encoding="utf-8")),
                         self.manifest)
        self.assertIn("recovered 0 / 0", stdout.getvalue())
        self.assertNotIn("patched", stdout.getvalue())

    def test_retained_rollback_files_are_reported(self) -> None:
        self.recoverable()
        stdout = io.StringIO()
        with mock.patch.object(self.retry13.dl, "fetch", self.fetch), \
                mock.patch.object(self.retry13.dl, "CRAWL_DELAY", 0), \
                mock.patch.object(self.retry13.dl, "discard_snapshots", return_value=["kept"]), \
                contextlib.redirect_stdout(stdout):
            self.retry13.main()
        self.assertIn("WARNING retained 1 rollback file(s) after commit", stdout.getvalue())
        self.assertIn("patched 1 entry in manifest_raw.json", stdout.getvalue())

    def test_an_incomplete_rollback_is_reported_over_the_original_failure(self) -> None:
        self.recoverable()
        rollback_error = OSError(5, "simulated rollback failure")
        with mock.patch.object(self.retry13.dl, "fetch", side_effect=OSError("network down")), \
                mock.patch.object(self.retry13.dl, "rollback_snapshots", side_effect=rollback_error), \
                contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "retry failed and rollback was incomplete") as caught:
                self.retry13.main()
        self.assertIs(caught.exception.__cause__, rollback_error)


class CheckCurrentTests(TemporaryDirectoryTestCase):
    """Every lookup outcome of the staleness check, from the checkout's own source."""

    UNCHANGED = {"start": "2099-01-01T00:00:00", "compilationNumber": "1",
                 "registerId": "C2099C00001", "status": "InForce"}

    def run_check(self, titles: list[dict[str, str]], answers: dict[str, list[object]],
                  *argv: str) -> str:
        (self.base / "sources.json").write_text(json.dumps({
            "retrieved": "2099-06-01", "titles": titles}), encoding="utf-8")
        module = regressions.load_module("check_current_paths", STAGE / "check_current.py")
        module.ROOT = str(self.base)

        def fetch(url: str) -> object:
            rid = re.search(r"titleId%20eq%20%27([A-Z0-9]+)%27", url)[1]
            answer = answers[rid].pop(0)
            if answer is None:
                return None
            return {"value": [dict(row, titleId=rid) for row in answer]}

        stdout = io.StringIO()
        with mock.patch.object(module, "fetch_json", side_effect=fetch), \
                mock.patch.object(module.sys, "argv", ["check_current.py", *argv]), \
                mock.patch.object(module.time, "sleep"), contextlib.redirect_stdout(stdout):
            module.main()
        return stdout.getvalue()

    @staticmethod
    def title(number: int, collection: str = "Act") -> dict[str, str]:
        return {"register_id": "C2099A%05d" % number, "name": "Synthetic Act %d" % number,
                "collection": collection, "compilation_number": "1",
                "compilation_date": "2099-01-01"}

    def test_every_outcome_is_reported_in_its_own_bucket(self) -> None:
        titles = [self.title(number) for number in range(1, 26)]
        answers: dict[str, list[object]] = {
            t["register_id"]: [[self.UNCHANGED]] for t in titles}
        answers["C2099A00001"] = [None]
        answers["C2099A00002"] = [[], [{"start": "2098-01-01"}]]
        answers["C2099A00003"] = [[], None]
        answers["C2099A00004"] = [[], []]
        answers["C2099A00005"] = [[dict(self.UNCHANGED, start="2099-02-30")]]
        answers["C2099A00006"] = [[dict(self.UNCHANGED, compilationNumber="2",
                                        registerId="C2099C00002")]]
        stdout = self.run_check(titles, answers)

        self.assertIn("Roughly 1 minutes.", stdout)
        self.assertIn("  checked 25/25", stdout)
        self.assertIn("unchanged: 19   superseded: 1   no compilation published: 0   "
                      "no longer in force: 1   lookup failed: 4", stdout)
        self.assertIn("SUPERSEDED, re-download these:\n  C2099A00006  comp 1 (2099-01-01) "
                      "-> comp 2 (2099-01-01)  Synthetic Act 6", stdout)
        failed = stdout.split("LOOKUP FAILED:\n")[1].split()
        self.assertEqual(failed[::4], ["C2099A00001", "C2099A00003", "C2099A00004", "C2099A00005"])

    def test_a_collection_argument_narrows_the_check(self) -> None:
        titles = [self.title(1), self.title(2, "LegislativeInstrument")]
        stdout = self.run_check(titles, {"C2099A00001": [[self.UNCHANGED]]}, "Act")
        self.assertIn("Checking 1 titles against the Register.", stdout)
        self.assertIn("unchanged: 1", stdout)

    def test_a_collection_with_no_titles_stops_before_any_lookup(self) -> None:
        stdout = self.run_check([self.title(1)], {}, "Gazette")
        self.assertEqual(stdout, "No titles in collection 'Gazette'.\n")


class PiiScanTests(TemporaryDirectoryTestCase):
    """Both scans run from the checkout's source, pointed at a deployed build layout."""

    REGISTER_ROW = regressions.PiiNameGateTests.ALL_CAPS_ROW
    FLAGGED = "F2026N00001"
    PLAIN = "C2004A00001"

    def setUp(self) -> None:
        super().setUp()
        self.build = self.base / "build"
        self.build.mkdir()

    def write_rows(self, rid: str, *rows: dict[str, str]) -> None:
        folder = self.base / "markdown" / rid
        folder.mkdir(parents=True)
        (folder / "sections.jsonl").write_text(
            "\n".join(json.dumps(row) for row in rows) + "\n\n", encoding="utf-8")

    def load(self, name: str):
        module = regressions.load_module(name + "_paths", STAGE / (name + ".py"))
        module.__file__ = str(self.build / (name + ".py"))
        module.ROOT = str(self.base)
        return module

    def test_the_first_scan_flags_only_a_register_shaped_title(self) -> None:
        (self.build / "manifest_md.json").write_text(json.dumps([
            {"id": self.FLAGGED, "name": "All Caps Register", "collection": "NotifiableInstrument"},
        ]), encoding="utf-8")
        self.write_rows(self.FLAGGED, {"row_id": "r1", "text": self.REGISTER_ROW},
                        {"row_id": "r2", "text": "Signed, SMITH, John 12345678."})
        self.write_rows(self.PLAIN, {"row_id": "r1", "text": "The Commissioner may determine."})
        scan = self.load("pii_scan")
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            scan.main()

        [flagged] = json.loads((self.build / "pii_flagged.json").read_text(encoding="utf-8"))
        self.assertEqual((flagged["register_id"], flagged["rows_total"], flagged["rows_flagged"],
                          flagged["row_ids"]), (self.FLAGGED, 2, 1, ["r1"]))
        self.assertIn("titles carrying person-name rows: 1", stdout.getvalue())
        self.assertRegex(stdout.getvalue(), r"F2026N00001 +2 +1 +\d+  All Caps Register")

    def write_second_scan_inputs(self, policy_entries: list[dict[str, str]]) -> None:
        (self.build / "pii_flagged.json").write_text(
            json.dumps([{"register_id": self.FLAGGED}]), encoding="utf-8")
        (self.build / "pii_contact_allowlist.json").write_text(
            json.dumps({"schema_version": 1, "entries": policy_entries}), encoding="utf-8")

    def run_second_scan(self) -> tuple[int, str]:
        scan = self.load("pii_scan2")
        scan.ALLOWLIST = str(self.build / "pii_contact_allowlist.json")
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            result = scan.main()
        return result, stdout.getvalue()

    def test_the_second_scan_reports_weak_pairs_and_caps_its_examples(self) -> None:
        self.write_second_scan_inputs([])
        contacts = " ".join("person%d@example.test" % n for n in range(6))
        self.write_rows(self.FLAGGED, {"row_id": "known", "text": self.REGISTER_ROW})
        self.write_rows(self.PLAIN, {"row_id": "weak", "text": "Agent Jane Citizen 12345678"},
                        {"row_id": "contacts", "text": contacts})
        result, stdout = self.run_second_scan()

        self.assertEqual(result, 1)
        self.assertRegex(stdout, r"C2004A00001 +weak +names=1 nums=1")
        self.assertNotRegex(stdout, r"F2026N00001 +known")
        self.assertIn("unapproved contact details: {'email': 6}", stdout)
        self.assertNotIn("@example.test", stdout)

    def test_the_second_scan_passes_an_approved_contact(self) -> None:
        contact = "official@example.test"
        digest = next(pii_patterns.contact_fingerprints(contact))[1]
        self.write_second_scan_inputs([{"kind": "email", "sha256": digest,
                                        "register_id": self.PLAIN, "reason": "office"}])
        self.write_rows(self.PLAIN, {"row_id": "r1", "text": contact})
        result, stdout = self.run_second_scan()

        self.assertEqual(result, 0)
        self.assertIn("contact details across the whole corpus: {'email': 1}", stdout)
        self.assertIn("unapproved contact details: none", stdout)


if __name__ == "__main__":
    unittest.main()
