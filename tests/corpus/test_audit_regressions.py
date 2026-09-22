"""Fabricated boundary cases from the September portfolio audit."""
import base64
import hashlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fadden import http_fetch

from tests.corpus import test_regressions as fixtures
from tests.corpus.test_regressions import STAGE, load_module


class AuditRegressionTests(unittest.TestCase):
    def test_non_object_json_retries_and_fails_closed(self):
        for body in (b"null", b"[]", b"1", b'"text"', b"true"):
            with self.subTest(body=body), mock.patch.object(
                http_fetch.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(body)
            ) as request, mock.patch.object(http_fetch.time, "sleep"):
                self.assertIsNone(http_fetch.fetch_json("https://example.invalid"))
                self.assertEqual(request.call_count, http_fetch.TRIES)

    def test_unplaced_volume_fails_instead_of_disappearing(self):
        extract = load_module("audit_extract_unplaced", STAGE / "extract.py")
        blocks = extract.epub_blocks(io.BytesIO(fixtures.VolumeGateTests.epub_bytes(
            fixtures.VolumeGateTests.VOLUME_1, fixtures.VolumeGateTests.VOLUME_2_BARE
        )))
        with self.assertRaisesRegex(ValueError, "body boundary"):
            extract.to_markdown(blocks, fixtures.VolumeGateTests.META)

    def test_irregular_spans_keep_logical_columns(self):
        extract = load_module("audit_extract_spans", STAGE / "extract.py")
        doc = extract.Doc()
        doc.feed('<table><tr><td>Category</td><td>From</td><td>Rate</td></tr>'
                 '<tr><td colspan="2">All</td><td>12%</td></tr>'
                 '<tr><td>Other</td><td>2026</td><td>10%</td></tr></table>')
        self.assertEqual(doc.blocks[0]["rows"][1], ["All", "", "12%"])

    def test_nested_table_stays_inside_its_parent_cell(self):
        extract = load_module("audit_extract_nested", STAGE / "extract.py")
        doc = extract.Doc()
        doc.feed('<table><tr><td>Category A</td><td>Before'
                 '<table><tr><td>Nested rate</td><td>12%</td></tr></table>'
                 'After</td></tr></table>')
        self.assertEqual(len(doc.blocks), 1)
        row = doc.blocks[0]["rows"][0]
        self.assertEqual(row[0], "Category A")
        self.assertIn("Nested rate", row[1])
        self.assertIn("12%", row[1])
        self.assertLess(row[1].index("Before"), row[1].index("Nested rate"))
        self.assertLess(row[1].index("Nested rate"), row[1].index("After"))

    def test_sidecar_retrieval_date_overrides_copied_file_mtime(self):
        helper = fixtures.ExtractPipelineTests()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            build, epub = helper._fixture(root)
            epub.with_suffix(".epub.meta.json").write_text(json.dumps({
                "versionStart": "2025-01-01", "fetched_at": "2026-08-04T23:15:00+00:00"
            }), encoding="utf-8")
            os.utime(epub, (0, 0))
            helper._run(build, None)
            markdown, rows = helper._outputs(root)
            self.assertIn("retrieved: 2026-08-04", markdown)
            self.assertTrue(all("2026-08-04" in row["attribution"] for row in rows))

    def test_cached_download_preserves_authorisation_and_retrieval(self):
        download = load_module("audit_download_cache", STAGE / "download.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            epub = root / "epub"
            epub.mkdir()
            (root / "acts_resolved.json").write_text(json.dumps([{
                "id": "F2020L01498", "name": "Fabricated instrument",
                "versionStart": "2026-03-01", "compilationNumber": "1",
                "compilationRegisterId": "F2026C00001"
            }]), encoding="utf-8")

            def fetch(url, dst):
                payload = fixtures.VolumeGateTests.epub_bytes()
                Path(dst).write_bytes(payload)
                return True, "200", "application/epub+zip", len(payload), {
                    "registerId": "F2026C00001", "isAuthorised": True
                }

            with mock.patch.multiple(download, SCRATCH=str(root), EPUB_DIR=str(epub),
                                     CRAWL_DELAY=0), mock.patch.object(download, "fetch", fetch):
                download.main()
                before = json.loads((root / "manifest_raw.json").read_text())[0]
                with mock.patch.object(download, "fetch", side_effect=AssertionError("cache miss")):
                    download.main()
                after = json.loads((root / "manifest_raw.json").read_text())[0]
            self.assertEqual(after["status"], "cached")
            self.assertIs(after["isAuthorised"], True)
            self.assertEqual(after["fetched_at"], before["fetched_at"])

    def test_fixture_retained_response_matches_its_digest_and_length(self):
        fixture = Path(__file__).parent / "fixtures/live-evidence/evidence-bundle.v2.json"
        bundle = json.loads(fixture.read_text(encoding="utf-8"))
        payload = base64.b64decode(bundle["primary_response_base64"], validate=True)
        request = bundle["capture_result"]["requests"][0]
        digest = "sha256:" + hashlib.sha256(payload).hexdigest()
        self.assertEqual(request["response_length"], len(payload))
        self.assertEqual(request["response_sha256"], digest)
        self.assertEqual(bundle["observation"]["primary_response_sha256"], digest)
