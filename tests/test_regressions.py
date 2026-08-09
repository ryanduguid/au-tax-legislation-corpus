"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import sys
import tempfile
import time
import unittest


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
            "**1 titles name people.** The corpus carries about 3 name mentions\n"
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
