"""Fabricated boundary cases from the September portfolio audit."""
import base64
import hashlib
import io
import json
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from fadden import http_fetch

from tests.corpus.test_regressions import STAGE, load_module

# The 2-volume EPUB VolumeGateTests built before extract.py moved to Rust.
VOLUMES = (
    '<html><body>'
    '<p class="ActHead4"><span class="CharSectno">1</span> Short title</p>'
    '<p class="subsection">This instrument is the Demo Approval 2025.</p>'
    '</body></html>',
    '<html><body>'
    '<p class="MadeunderText">Demo Approval 2025</p>'
    '<p>Compilation No. 1</p>'
    '<p>This compilation is in 2 volumes</p>'
    '<p>Volume 2: Schedule 1</p>'
    '<p>Endnotes</p>'
    '<p class="ContentsHead">Contents</p>'
    '<p class="TOC6">Schedule 1 Valuation factors</p>'
    '<p class="Scheduletitle">Schedule 1 Valuation factors</p>'
    '<p class="ScheduleHeading">Table 3B Lump sum valuation factors</p>'
    '<table><tr><td>Age</td><td>Factor</td></tr>'
    '<tr><td>55</td><td>12.5</td></tr></table>'
    '</body></html>',
)


def epub_bytes():
    payload = io.BytesIO()
    with zipfile.ZipFile(payload, "w") as archive:
        for i, document in enumerate(VOLUMES, 1):
            archive.writestr("document_%d.xhtml" % i, document)
    return payload.getvalue()


class AuditRegressionTests(unittest.TestCase):
    def test_non_object_json_retries_and_fails_closed(self):
        for body in (b"null", b"[]", b"1", b'"text"', b"true"):
            with self.subTest(body=body), mock.patch.object(
                http_fetch.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(body)
            ) as request, mock.patch.object(http_fetch.time, "sleep"):
                self.assertIsNone(http_fetch.fetch_json("https://example.invalid"))
                self.assertEqual(request.call_count, http_fetch.TRIES)

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
                payload = epub_bytes()
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
