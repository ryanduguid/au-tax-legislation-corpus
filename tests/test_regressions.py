"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock


REPO = Path(__file__).resolve().parents[1]


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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
        title = {"id": "C0000000001", "name": "Example Tax Act", "isPrincipal": True}
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
        title = {"id": "C0000000001", "name": "Example Tax Act", "isPrincipal": True}
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            (scratch / "titles_all.json").write_text(json.dumps([title]), encoding="utf-8")
            versions.SCRATCH = str(scratch)
            versions.time.sleep = lambda _seconds: None

            def response(url):
                if "C2004A05138" in url:
                    return {"value": [{"titleId": "C2004A05138", "start": "2026-01-01"}]}
                return {"value": [{
                    "titleId": "C0000000001", "start": "2026-02-03T00:00:00Z",
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
            extract = load_module("extract_regression", source)

            root = tmp_path / "corpus"
            epub = root / "epub" / "C0000000001.epub"
            epub.parent.mkdir(parents=True)
            epub.write_bytes(b"not-an-epub")
            manifest = [{
                "id": "C0000000001", "name": "Example Tax Act",
                "epub": epub.name, "versionStart": "2026-01-01",
                "compilationNumber": "1", "sourceUrl": "https://example.test/C0000000001",
            }]
            (build / "manifest_raw.json").write_text(json.dumps(manifest), encoding="utf-8")
            extract.epub_blocks = lambda _path: (_ for _ in ()).throw(ValueError("bad EPUB"))

            with mock.patch.dict(os.environ, {"ATO_KB_ROOT": str(root)}, clear=False):
                with contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "refusing to write manifest_md"):
                        extract.main("2026-08-07")

            self.assertFalse((build / "manifest_md.json").exists())


class DistributionTests(unittest.TestCase):
    PUBLIC_ID = "C0000000001"
    PRIVATE_ID = "C0000000002"
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
            "## Acts (1)\n| [Public Tax Act](markdown/C0000000001) | C0000000001 |\n\n"
            "## Notifiable instruments (1)\n"
            "| [Private Disciplinary Register](markdown/C0000000002) | C0000000002 |\n",
            encoding="utf-8")
        (root / "README.md").write_text(
            "2 in-force principal titles covering tax.\n\n"
            "1 Acts and 1 legislative and notifiable instruments. 2 retrieval rows, 10 words.\n",
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

            rates_md = (output / "rates" / "RATES.md").read_text(encoding="utf-8")
            self.assertIn("1 entries across 1 titles.", rates_md)
            self.assertIn(self.PUBLIC_NAME, rates_md)
            self.assertNotIn(self.PRIVATE_NAME, rates_md)

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


if __name__ == "__main__":
    unittest.main()
