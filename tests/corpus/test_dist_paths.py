"""Refusal paths of the distribution builder and verifier that the scenario tests leave idle."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

from tests.corpus import test_regressions as regressions

STAGE = regressions.STAGE


class TemporaryTreeTestCase(unittest.TestCase):
    PUBLIC_ID = regressions.DistributionTests.PUBLIC_ID
    PRIVATE_ID = regressions.DistributionTests.PRIVATE_ID
    PUBLIC_NAME = regressions.DistributionTests.PUBLIC_NAME
    PRIVATE_NAME = regressions.DistributionTests.PRIVATE_NAME

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.dist = regressions.load_module("dist_paths", STAGE / "dist.py")
        self.verify = regressions.load_module("dist_verify_paths", STAGE / "dist_verify.py")

    def fixture(self) -> tuple[Path, Path]:
        root, build = regressions.DistributionTests._write_fixture(self, self.base)
        self.dist.ROOT = str(root)
        self.dist.HERE = str(build)
        self.dist.DIST = str(root / "dist")
        return root, build

    def publish(self) -> Path:
        with contextlib.redirect_stdout(io.StringIO()):
            self.dist.main()
        self.verify.DIST = self.dist.DIST
        return Path(self.dist.DIST)

    def run_verifier(self) -> tuple[int, str]:
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with self.assertRaises(SystemExit) as result:
                self.verify.main()
        return result.exception.code, stdout.getvalue()

    def status(self, output: str, label: str) -> str:
        [line] = [line.strip() for line in output.splitlines() if line.strip().startswith(label)]
        return line[len(label):].split()[0]


class TargetValidationTests(TemporaryTreeTestCase):
    def test_unusable_targets_are_refused(self) -> None:
        a_file = self.base / "file"
        a_file.write_text("", encoding="utf-8")
        cases = {
            "must be a named child directory": os.path.abspath(os.sep),
            "parent does not exist": self.base / "missing" / "dist",
            "must be a directory": a_file,
        }
        for message, target in cases.items():
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, message):
                    self.dist._validated_distribution_target(target)

    def test_a_linked_target_is_refused(self) -> None:
        real = self.base / "real"
        real.mkdir()
        link = self.base / "dist"
        try:
            os.symlink(real, link, target_is_directory=True)
        except OSError:
            self.skipTest("symbolic links are unavailable to this account")
        with self.assertRaisesRegex(ValueError, "must not be a link or reparse point"):
            self.dist._validated_distribution_target(link)

    def test_an_unknown_managed_kind_is_refused(self) -> None:
        target = self.base / "dist"
        with self.assertRaisesRegex(ValueError, "unknown managed distribution path kind"):
            self.dist._validated_managed_sibling(target, self.base / ".dist.cache-x", "cache")

    def test_a_parent_resolving_elsewhere_is_refused(self) -> None:
        target = self.base / "dist"
        candidate = self.base / (".dist.stage-" + "0" * 32)
        real_realpath = os.path.realpath
        moved = {"value": False}

        def swapped(path: str) -> str:
            # The first lookup sees the parent moved; the second sees the original.
            if not moved["value"]:
                moved["value"] = True
                return real_realpath(self.base / "elsewhere")
            return real_realpath(path)

        with mock.patch.object(self.dist.os.path, "realpath", side_effect=swapped):
            with self.assertRaisesRegex(ValueError, "resolves outside its parent"):
                self.dist._validated_managed_sibling(target, candidate, "stage")

    def test_sibling_allocation_gives_up_after_one_hundred_collisions(self) -> None:
        fixed = uuid.UUID(int=7)
        (self.base / (".dist.stage-" + fixed.hex)).mkdir()
        with mock.patch.object(self.dist.uuid, "uuid4", return_value=fixed) as uuid4:
            with self.assertRaisesRegex(RuntimeError, "could not allocate a unique distribution stage"):
                self.dist._new_managed_sibling(self.base / "dist", "stage")
        self.assertEqual(uuid4.call_count, 100)

    def test_a_managed_path_that_is_a_file_is_not_removed(self) -> None:
        stage = self.base / (".dist.stage-" + "0" * 32)
        stage.write_text("not a directory", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "refusing to remove a non-directory managed path"):
            self.dist._remove_managed_tree(self.base / "dist", stage, "stage")
        self.assertTrue(stage.is_file())


class PromotionTests(TemporaryTreeTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.target = self.base / "dist"
        self.stage = self.base / (".dist.stage-" + "1" * 32)

    def test_a_stage_that_is_not_a_directory_is_refused(self) -> None:
        self.stage.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "distribution stage must be a real directory"):
            self.dist._promote_distribution(self.stage, self.target)

    def test_a_first_publication_failure_leaves_nothing_to_restore(self) -> None:
        self.stage.mkdir()
        with mock.patch.object(self.dist.os, "rename", side_effect=PermissionError("denied")):
            with self.assertRaisesRegex(PermissionError, "denied"):
                self.dist._promote_distribution(self.stage, self.target)
        self.assertFalse(self.target.exists())
        self.assertTrue(self.stage.is_dir())

    def _fail_second_rename(self, before_raising) -> None:
        self.stage.mkdir()
        self.target.mkdir()
        (self.target / "old.txt").write_text("old", encoding="utf-8")
        real_rename = os.rename
        calls = []

        def rename(source, destination):
            calls.append(destination)
            if len(calls) == 2:
                before_raising(Path(calls[0]))
                raise PermissionError("second rename denied")
            return real_rename(source, destination)

        with mock.patch.object(self.dist.os, "rename", side_effect=rename):
            with self.assertRaises(RuntimeError) as caught:
                self.dist._promote_distribution(self.stage, self.target)
        self.message = str(caught.exception)

    def test_a_target_that_reappears_is_not_rolled_back_over(self) -> None:
        self._fail_second_rename(lambda _backup: self.target.mkdir())
        self.assertIn("automatic rollback is unsafe", self.message)

    def test_a_backup_replaced_by_a_file_is_not_restored(self) -> None:
        def replace_backup(backup: Path) -> None:
            shutil.rmtree(backup)
            backup.write_text("swapped", encoding="utf-8")

        self._fail_second_rename(replace_backup)
        self.assertEqual(self.message, "distribution backup is unsafe to restore")
        self.assertFalse(self.target.exists())


@unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes need a POSIX filesystem")
class NonRegularFileTests(TemporaryTreeTestCase):
    def test_both_tree_walkers_refuse_a_named_pipe(self) -> None:
        title = self.base / "title"
        title.mkdir()
        os.mkfifo(title / "pipe")
        with self.assertRaisesRegex(ValueError, "title tree contains a non-regular file"):
            self.verify._title_tree(title)
        with self.assertRaisesRegex(RuntimeError, "title tree contains a non-regular file"):
            self.dist._title_tree(title)


class VerifierHelperTests(TemporaryTreeTestCase):
    class Entry:
        def __init__(self, path: Path, error: OSError | None = None) -> None:
            self.path = str(path)
            self.error = error

        def is_symlink(self) -> bool:
            if self.error:
                raise self.error
            return False

        def is_dir(self, follow_symlinks: bool = True) -> bool:
            return False

        is_file = is_dir

    def test_entries_that_are_neither_files_nor_directories_are_named(self) -> None:
        def never(_path: str) -> bool:
            return False

        self.assertEqual(self.verify._entry_kind(self.Entry(self.base), never), "other")
        self.assertEqual(
            self.verify._entry_kind(self.Entry(self.base, PermissionError("denied")), never),
            "unreadable")

    def test_nested_directories_are_reported(self) -> None:
        (self.base / "inner").mkdir()
        (self.base / "inner" / "file.md").write_text("x", encoding="utf-8")
        files, directories = self.verify._title_tree(self.base)
        self.assertEqual(directories, ["inner"])
        self.assertEqual([Path(path).name for path in files], ["file.md"])


class RenderingTests(TemporaryTreeTestCase):
    def test_long_rate_sections_are_truncated_with_a_count(self) -> None:
        tables = [{"register_id": self.PUBLIC_ID, "act": "Act", "collection": "LegislativeInstrument",
                   "section": str(n), "heading": "Table", "topic": "income tax rates",
                   "kind": "table", "content": "| a |"} for n in range(41)]
        others = [{"register_id": self.PUBLIC_ID, "act": "Act", "collection": "Act",
                   "section": None, "topic": "income tax rates", "kind": "rate",
                   "amounts": ["1%"], "content": "One per cent."} for _ in range(121)]
        markdown = self.dist.render_rates_markdown(tables + others)
        self.assertIn("### Rate tables", markdown)
        self.assertIn("**Act** _(instrument)_ s 0 — Table", markdown)
        self.assertIn("_1 further tables in rates.jsonl._", markdown)
        self.assertIn("_1 further provisions in rates.jsonl._", markdown)

    def test_a_topic_with_only_a_few_tables_has_no_overflow_note(self) -> None:
        markdown = self.dist.render_rates_markdown([{
            "register_id": self.PUBLIC_ID, "act": "Act", "collection": "Act", "section": "1",
            "heading": "Table", "topic": "medicare levy", "kind": "table", "content": "| a |"}])
        self.assertIn("**Act** s 1 — Table", markdown)
        self.assertNotIn("further", markdown)
        self.assertNotIn("### Provisions carrying a number", markdown)

    def test_a_malformed_count_phrase_is_kept(self) -> None:
        text = "x Acts and 2 legislative and notifiable. 1 Acts and 2 legislative and notifiable"
        self.assertEqual(self.dist.replace_readme_collection_counts(text, 5, 6),
                         "x Acts and 2 legislative and notifiable. "
                         "5 Acts and 6 legislative and notifiable")


class ContactGateTests(TemporaryTreeTestCase):
    def test_a_listed_title_without_its_files_is_refused(self) -> None:
        root, _ = self.fixture()
        title = {"register_id": self.PUBLIC_ID}
        (root / "markdown" / self.PUBLIC_ID / "sections.jsonl").unlink()
        with self.assertRaisesRegex(RuntimeError, "C2004A00001 has no sections.jsonl"):
            self.dist.reject_unapproved_contacts([title], set())
        shutil.rmtree(root / "markdown" / self.PUBLIC_ID)
        with self.assertRaisesRegex(RuntimeError, "C2004A00001 has no markdown directory"):
            self.dist.reject_unapproved_contacts([title], set())

    def test_only_eight_unapproved_contacts_are_named(self) -> None:
        self.fixture()
        findings = {("email", "%064x" % n, self.PUBLIC_ID) for n in range(9)}
        with mock.patch.object(self.dist, "_title_privacy_findings", return_value=(0, findings)):
            with self.assertRaises(RuntimeError) as caught:
                self.dist.reject_unapproved_contacts([{"register_id": self.PUBLIC_ID}], set())
        message = str(caught.exception)
        self.assertTrue(message.startswith("unapproved contact identifiers: 9 ("))
        self.assertEqual(message.count("C2004A00001:email:"), 8)


class BuildRecheckTests(TemporaryTreeTestCase):
    """The copy step re-checks what the source gate saw, in case the tree changed."""

    def build(self) -> None:
        with mock.patch.object(self.dist, "reject_unapproved_contacts"):
            self.dist._build_distribution(str(self.base / "stage"))

    def test_a_title_removed_after_the_gate_stops_the_build(self) -> None:
        root, _ = self.fixture()
        (root / "markdown" / self.PUBLIC_ID / "sections.jsonl").unlink()
        with self.assertRaisesRegex(RuntimeError, "C2004A00001 has no sections.jsonl"):
            self.build()
        shutil.rmtree(root / "markdown" / self.PUBLIC_ID)
        shutil.rmtree(self.base / "stage")
        with self.assertRaisesRegex(RuntimeError, "C2004A00001 has no markdown directory"):
            self.build()

    def test_a_copied_title_with_new_findings_stops_the_build(self) -> None:
        self.fixture()
        cases = {
            "copied title contains private-person registration details": (1, set()),
            "copied title contains an unapproved contact identifier: C2004A00001:email:0000":
                (0, {("email", "0" * 64, self.PUBLIC_ID)}),
        }
        for message, findings in cases.items():
            with self.subTest(message=message):
                shutil.rmtree(self.base / "stage", ignore_errors=True)
                with mock.patch.object(self.dist, "_title_privacy_findings",
                                       return_value=findings):
                    with self.assertRaisesRegex(RuntimeError, message):
                        self.build()


class PublishedTreeTests(TemporaryTreeTestCase):
    def test_blank_lines_and_unnumbered_rows_are_counted_and_verified(self) -> None:
        root, _ = self.fixture()
        sections = root / "markdown" / self.PUBLIC_ID / "sections.jsonl"
        row = json.loads(sections.read_text(encoding="utf-8"))
        sections.write_text("\n" + json.dumps(row) + "\n\n" + json.dumps(
            dict(row, section=None, kind=None, text="Unnumbered words.")) + "\n",
            encoding="utf-8")
        rates = root / "rates" / "rates.jsonl"
        rates.write_text("\n" + rates.read_text(encoding="utf-8") + "\n", encoding="utf-8")
        sources_path = root / "sources.json"
        sources = json.loads(sources_path.read_text(encoding="utf-8"))
        sources["titles"][0]["jsonl_rows"] = 2
        sources_path.write_text(json.dumps(sources), encoding="utf-8")

        output = self.publish()

        published = json.loads((output / "sources.json").read_text(encoding="utf-8"))
        self.assertEqual(published["counts"]["jsonl_rows"], 2)
        code, out = self.run_verifier()
        self.assertEqual(code, 0, out)

    def test_malformed_rows_are_counted_not_raised(self) -> None:
        self.fixture()
        output = self.publish()
        sections = output / "markdown" / self.PUBLIC_ID / "sections.jsonl"
        row = json.loads(sections.read_text(encoding="utf-8"))
        cases = {
            "every JSONL row parses": [dict(row, text=["not", "text"]),
                                       dict(row, kind=7)],
            "each JSONL row matches its title directory": [dict(row, register_id=self.PRIVATE_ID)],
        }
        for label, rows in cases.items():
            with self.subTest(label=label):
                sections.write_text("".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")
                code, out = self.run_verifier()
                self.assertEqual((code, self.status(out, label)), (1, "FAIL"))

    def test_a_rates_row_without_a_title_is_malformed(self) -> None:
        self.fixture()
        output = self.publish()
        rates = output / "rates" / "rates.jsonl"
        rates.write_text("\n" + rates.read_text(encoding="utf-8") + "[]\n", encoding="utf-8")
        code, out = self.run_verifier()
        self.assertEqual((code, self.status(out, "every rates JSONL row parses")), (1, "FAIL"))

    def test_a_row_file_that_changes_after_its_privacy_scan_is_unreadable(self) -> None:
        self.fixture()
        output = self.publish()
        (output / "markdown" / self.PUBLIC_ID / "sections.jsonl").write_bytes(b"\xff\n")
        with mock.patch.object(self.verify, "privacy_findings_in_file", return_value=(False, [])):
            code, out = self.run_verifier()
        label = "all distributed title files are validated UTF-8 text"
        self.assertEqual((code, self.status(out, label)), (1, "FAIL"))


if __name__ == "__main__":
    unittest.main()
