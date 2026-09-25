"""The rulings stage with fabricated Legal Database pages and no network traffic."""

from __future__ import annotations

import hashlib
import io
import json
import re
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import fadden.__main__  # noqa: F401  (puts the stage directory on sys.path for bare imports)
from fadden import rulings

DOCID = "TXR/TR20991/NAT/ATO/00001"


def page(docid=DOCID, notice=True, rights="", body_extra="", reference=None):
    """A fabricated page shaped like a Legal Database document."""
    ref = docid if reference is None else reference
    rights_meta = f'<meta content="{rights}" name="dc.Rights" />' if rights else ""
    notice_html = f"<p>{rulings.NOTICE}</p>" if notice else ""
    return f"""<html><head>
<meta content="{ref}" name="ato.reference.id" />
<meta content="TR 2099/1 - Fabricated ruling for tests" name="DC.Title" />
<meta content="ATO Ruling" name="dc.Type.documentType" />
<meta content="2099/01/31" name="dc.Date.Issued" />
{rights_meta}
</head><body><div id="main-content">
<div id="LawFront"><p>Front matter that is not paragraph text</p></div>
<div id="LawBody">
  <div class="panel panel-default"><table><tr><td>Table of Contents</td><td>1</td></tr></table></div>
  <p><strong>What this Ruling is about</strong><a name="H1" id="H1"></a></p>
  <p class="indentlevel0"><a name="P1" id="P1"></a>1. A fabricated first paragraph.<sup><a href="#fp1">[1]</a></sup></p>
  <p class="indentlevel0"><a name="P2" id="P2"></a>2. A second paragraph with a list:</p>
  <ul><li>first item</li><li>second item</li></ul>
  <h3><a name="H2" id="H2"></a>Ruling</h3>
  <p class="indentlevel0"><a name="P3" id="P3"></a>3. The ruling paragraph.</p>
  {body_extra}
  <p class="indentlevel0"><a name="COPYRIGHT" id="COPYRIGHT"></a>Copyright line</p>
  {notice_html}
  <div id="footnotes"><p>[1] A footnote that is not a paragraph.</p></div>
</div></div></body></html>""".encode("utf-8")


class ParseTests(unittest.TestCase):
    def test_paragraphs_headings_and_continuations(self) -> None:
        parsed = rulings.parse_document(page(), DOCID)
        self.assertEqual(parsed["licence_basis"], "document-notice")
        self.assertEqual(parsed["title"], "TR 2099/1 - Fabricated ruling for tests")
        self.assertEqual(parsed["issued"], "2099/01/31")
        self.assertEqual(
            [(p["paragraph"], p["heading"], p["text"]) for p in parsed["paragraphs"]],
            [
                ("1", "What this Ruling is about", "1. A fabricated first paragraph."),
                ("2", "What this Ruling is about",
                 "2. A second paragraph with a list: first item second item"),
                ("3", "Ruling", "3. The ruling paragraph."),
            ],
        )

    def test_table_of_contents_footnotes_and_copyright_are_not_rows(self) -> None:
        text = " ".join(p["text"] for p in rulings.parse_document(page(), DOCID)["paragraphs"])
        for absent in ("Table of Contents", "[1]", "footnote", "Copyright line", "Front matter"):
            self.assertNotIn(absent, text)

    def test_reference_mismatch_is_refused(self) -> None:
        with self.assertRaisesRegex(rulings.RulingsError, "does not match"):
            rulings.parse_document(page(reference="TXR/TR20992/NAT/ATO/00001"), DOCID)

    def test_missing_notice_is_refused_unless_dc_rights_points_to_it(self) -> None:
        with self.assertRaisesRegex(rulings.RulingsError, "no ATO reuse notice"):
            rulings.parse_document(page(notice=False), DOCID)
        parsed = rulings.parse_document(
            page(notice=False, rights="http://www.ato.gov.au/content/corporate/about_this_site.htm#copyright"),
            DOCID,
        )
        self.assertEqual(parsed["licence_basis"], "site-notice-via-dc-rights")

    def test_printed_numbers_are_used_only_when_no_paragraph_anchors_exist(self) -> None:
        unanchored = re.sub(rb'<a name="P\d+" id="P\d+"></a>', b"", page())
        parsed = rulings.parse_document(unanchored, DOCID)
        self.assertEqual([p["paragraph"] for p in parsed["paragraphs"]], ["1", "2", "3"])
        self.assertEqual(parsed["paragraphs"][1]["text"],
                         "2. A second paragraph with a list: first item second item")
        # With anchors present, a block that merely starts with a number is a continuation.
        mixed = page(body_extra="<p>4. Printed but unanchored.</p>")
        numbers = [p["paragraph"] for p in rulings.parse_document(mixed, DOCID)["paragraphs"]]
        self.assertEqual(numbers, ["1", "2", "3"])

    def test_a_page_without_numbered_paragraphs_is_refused(self) -> None:
        # Neither a P anchor nor a printed "n. " number: nothing to key a row on.
        empty = re.sub(rb">\d\. ", b">", page().replace(b'name="P', b'name="X'))
        with self.assertRaisesRegex(rulings.RulingsError, "no numbered paragraphs"):
            rulings.parse_document(empty, DOCID)


EV_ID = "EV/1052000000001"
EV_PAGE = f"""<html><head>
<meta content="{EV_ID}" name="ato.reference.id" />
<meta content="GST - Fabricated arrangement" name="DC_TITLE" />
<meta content="2099/01/31" name="DC_DATE_ISSUED" />
<meta content="http://www.ato.gov.au/content/corporate/about_this_site.htm#copyright" name="DC_RIGHTS" />
</head><body><main><article>
<div class="container-fluid"><a href="#">Back to browse</a></div>
<DIV ID="ev_disclaimer_v0"><table><tr><td>You cannot rely on this record.</td></tr></table></DIV>
<P><STRONG>Question 1</STRONG></P>
<P>Is Entity X making a supply?</P>
<P><STRONG>Answer</STRONG></P>
<P>Yes. Entity X makes a supply under section 9-5.</P>
<div class="container-fluid"><a href="#main-content">Back to top</a></div>
</article></main><footer><p>Footer text</p></footer></body></html>""".encode("utf-8")


class EditedVersionTests(unittest.TestCase):
    def test_paragraphs_are_numbered_in_order_and_marked_sequential(self) -> None:
        parsed = rulings.parse_document(EV_PAGE, EV_ID)
        self.assertEqual(parsed["licence_basis"], "site-notice-via-dc-rights")
        self.assertEqual(parsed["title"], "GST - Fabricated arrangement")
        self.assertEqual(
            [(p["paragraph"], p["heading"], p["text"], p["numbering"]) for p in parsed["paragraphs"]],
            [
                ("1", "Question 1", "Is Entity X making a supply?", "sequential"),
                ("2", "Answer", "Yes. Entity X makes a supply under section 9-5.", "sequential"),
            ],
        )

    def test_disclaimer_navigation_and_footer_are_not_rows(self) -> None:
        text = " ".join(p["text"] for p in rulings.parse_document(EV_PAGE, EV_ID)["paragraphs"])
        for absent in ("cannot rely", "Back to", "Footer"):
            self.assertNotIn(absent, text)

    def test_numbered_documents_say_their_numbers_are_the_documents(self) -> None:
        numbering = {p["numbering"] for p in rulings.parse_document(page(), DOCID)["paragraphs"]}
        self.assertEqual(numbering, {"document"})


class TargetTests(unittest.TestCase):
    def write(self, value) -> Path:
        handle = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8")
        with handle:
            json.dump(value, handle)
        self.addCleanup(Path(handle.name).unlink)
        return Path(handle.name)

    def test_only_ato_authored_families_are_accepted(self) -> None:
        self.assertEqual(rulings.load_targets(self.write([DOCID, "EV/1052000000001"])),
                         [DOCID, "EV/1052000000001"])
        for docid in ("PAC/19970038/6-5", "JUD/2099ATC20-001"):
            with self.subTest(docid=docid), self.assertRaisesRegex(rulings.RulingsError, "not an ATO"):
                rulings.load_targets(self.write([docid]))

    def test_duplicates_malformed_ids_and_oversize_lists_are_refused(self) -> None:
        cases = [
            ([DOCID, DOCID.lower()], "duplicate"),
            (["not a docid"], "not a Legal Database docid"),
            ([f"EV/{n:013d}" for n in range(rulings.MAX_TARGETS + 1)], "exceed the cap"),
            ({"docid": DOCID}, "JSON array"),
        ]
        for value, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(rulings.RulingsError, message):
                rulings.load_targets(self.write(value))


class RunTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.tmp))

    def test_run_writes_rows_manifest_notice_and_exact_bytes(self) -> None:
        raw = page()
        out = self.tmp / "out"
        code = rulings.run([DOCID], out, fetcher=lambda d: (raw, rulings.BASE_URL + d),
                           spacing=0, today="2099-02-01")
        self.assertEqual(code, 0)
        rows = [json.loads(line) for line in (out / "rulings.jsonl").read_text(encoding="utf-8").splitlines()]
        self.assertEqual([r["paragraph"] for r in rows], ["1", "2", "3"])
        self.assertEqual({r["source_sha256"] for r in rows}, {hashlib.sha256(raw).hexdigest()})
        manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["documents"][0]["family"], "Taxation Ruling")
        self.assertEqual((out / manifest["documents"][0]["raw_file"]).read_bytes(), raw)
        self.assertIn(rulings.NOTICE, (out / "LICENCE-NOTICE.md").read_text(encoding="utf-8"))

    def test_an_existing_destination_is_never_reused(self) -> None:
        out = self.tmp / "out"
        out.mkdir()
        with self.assertRaisesRegex(rulings.RulingsError, "already exists"):
            rulings.run([DOCID], out, fetcher=lambda d: (page(), ""), spacing=0)
        self.assertEqual(list(out.iterdir()), [])

    def test_personal_data_and_failures_are_excluded_and_fail_the_run(self) -> None:
        other = "EV/1052000000001"
        pages = {
            DOCID: page(body_extra='<p><a name="P4"></a>4. Call 02 6216 1111 for help.</p>'),
            other: None,
        }

        def fetcher(docid):
            if pages[docid] is None:
                raise rulings.RulingsError(f"{docid}: fetch failed (URLError)")
            return pages[docid], rulings.BASE_URL + docid

        out = self.tmp / "out"
        with mock.patch("sys.stderr", io.StringIO()) as err:
            code = rulings.run([DOCID, other], out, fetcher=fetcher, spacing=0)
        self.assertEqual(code, 1)
        manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["documents"], [])
        self.assertEqual([e["docid"] for e in manifest["excluded"]], [DOCID, other])
        self.assertIn("personal data patterns: phone", manifest["excluded"][0]["reason"])
        self.assertNotIn("6216", err.getvalue() + json.dumps(manifest))
        self.assertEqual((out / "rulings.jsonl").read_text(encoding="utf-8"), "")


class FetchTests(unittest.TestCase):
    def response(self, url, kind="text/html; charset=utf-8", body=b"<html></html>"):
        handle = mock.MagicMock()
        handle.__enter__.return_value = handle
        handle.geturl.return_value = url
        handle.headers = {"Content-Type": kind}
        handle.read.return_value = body
        return handle

    def test_off_site_redirects_and_non_html_are_refused(self) -> None:
        cases = [
            (self.response("https://example.invalid/x"), "redirected off"),
            (self.response(rulings.BASE_URL + DOCID, kind="application/pdf"), "content type"),
            (self.response(rulings.BASE_URL + DOCID, body=b"x" * (rulings.MAX_BYTES + 1)), "larger than"),
        ]
        for handle, message in cases:
            with self.subTest(message=message), \
                    mock.patch.object(rulings.urllib.request, "urlopen", return_value=handle), \
                    self.assertRaisesRegex(rulings.RulingsError, message):
                rulings.fetch(DOCID)

    def test_client_errors_stop_and_server_errors_retry(self) -> None:
        import urllib.error

        def error(code):
            return urllib.error.HTTPError(rulings.BASE_URL + DOCID, code, "synthetic", {}, None)

        calls = []

        def not_found(*args, **kwargs):
            calls.append("request")
            raise error(404)

        with mock.patch.object(rulings.urllib.request, "urlopen", not_found), \
                mock.patch.object(rulings.http_fetch.time, "sleep", calls.append), \
                self.assertRaisesRegex(rulings.RulingsError, "HTTP 404"):
            rulings.fetch(DOCID)
        self.assertEqual(calls, ["request"])

        responses = iter([error(503), self.response(rulings.BASE_URL + DOCID, body=page())])
        calls.clear()

        def flaky(*args, **kwargs):
            calls.append("request")
            item = next(responses)
            if isinstance(item, Exception):
                raise item
            return item

        with mock.patch.object(rulings.urllib.request, "urlopen", flaky), \
                mock.patch.object(rulings.http_fetch.time, "sleep", calls.append):
            raw, _final = rulings.fetch(DOCID)
        self.assertEqual(raw, page())
        self.assertEqual(calls, ["request", rulings.http_fetch.RETRY_DELAY, "request"])

    def test_a_valid_page_is_returned_with_its_final_url(self) -> None:
        handle = self.response(rulings.BASE_URL + DOCID, body=page())
        with mock.patch.object(rulings.urllib.request, "urlopen", return_value=handle):
            raw, final = rulings.fetch(DOCID)
        self.assertEqual(raw, page())
        self.assertEqual(final, rulings.BASE_URL + DOCID)


class ReviewRegressionTests(unittest.TestCase):
    """Defects raised in review of the first version of this stage."""

    def test_personal_data_outside_paragraph_text_still_excludes_the_document(self) -> None:
        contact = "Call 02 6216 1111."
        pages = {
            "heading": page().replace(b"<strong>What this Ruling is about</strong>",
                                      f"<strong>{contact}</strong>".encode()),
            "title": page().replace(b"TR 2099/1 - Fabricated ruling for tests", contact.encode()),
            "footnote": page().replace(b"[1] A footnote that is not a paragraph.", contact.encode()),
            "table of contents": page().replace(b"Table of Contents", contact.encode()),
        }
        for where, raw in pages.items():
            with self.subTest(where=where):
                parsed = rulings.parse_document(raw, DOCID)
                self.assertEqual(rulings.pii_findings(rulings.scanned_texts(parsed)), ["phone"])

    def test_site_chrome_outside_the_document_region_is_not_scanned(self) -> None:
        raw = page().replace(b'<div id="main-content">',
                             b'<header><p>Phone 13 28 61 or 02 6216 1111</p></header><div id="main-content">')
        parsed = rulings.parse_document(raw, DOCID)
        self.assertEqual(rulings.pii_findings(rulings.scanned_texts(parsed)), [])

    def test_a_longer_reference_does_not_match_a_shorter_target(self) -> None:
        with self.assertRaisesRegex(rulings.RulingsError, "does not match"):
            rulings.parse_document(page(reference=DOCID + "0"), DOCID)
        self.assertEqual(rulings.parse_document(page(reference=DOCID.lower() + "/"), DOCID)["reference"],
                         DOCID.lower() + "/")

    def test_dc_rights_must_be_the_ato_copyright_address(self) -> None:
        accepted = [
            "http://www.ato.gov.au/content/corporate/about_this_site.htm#copyright",
            "https://ato.gov.au/content/corporate/about_this_site.htm#copyright",
        ]
        refused = [
            "http://example.invalid/content/corporate/about_this_site.htm#copyright",
            "http://www.ato.gov.au.example.invalid/content/corporate/about_this_site.htm#copyright",
            "http://www.ato.gov.au/elsewhere/about_this_site.htm#copyright",
            "http://www.ato.gov.au:8080/content/corporate/about_this_site.htm#copyright",
            "about_this_site.htm#copyright",
        ]
        for value in accepted:
            with self.subTest(value=value):
                parsed = rulings.parse_document(page(notice=False, rights=value), DOCID)
                self.assertEqual(parsed["licence_basis"], "site-notice-via-dc-rights")
        for value in refused:
            with self.subTest(value=value), self.assertRaisesRegex(rulings.RulingsError, "no ATO reuse"):
                rulings.parse_document(page(notice=False, rights=value), DOCID)

    def test_redirects_must_stay_on_https_and_the_default_port(self) -> None:
        fetch = FetchTests()
        for url in ("http://www.ato.gov.au/law/view/document?docid=" + DOCID,
                    "https://www.ato.gov.au:8443/law/view/document?docid=" + DOCID):
            handle = fetch.response(url, body=page())
            with self.subTest(url=url), \
                    mock.patch.object(rulings.urllib.request, "urlopen", return_value=handle), \
                    self.assertRaisesRegex(rulings.RulingsError, "redirected off"):
                rulings.fetch(DOCID)

    def test_a_dropped_response_is_retried_then_excluded_without_aborting_the_run(self) -> None:
        import http.client

        def dropped(*args, **kwargs):
            raise http.client.IncompleteRead(b"partial")

        with mock.patch.object(rulings.urllib.request, "urlopen", dropped), \
                mock.patch.object(rulings.http_fetch.time, "sleep", lambda _s: None), \
                self.assertRaisesRegex(rulings.RulingsError, "IncompleteRead"):
            rulings.fetch(DOCID)

        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(tmp))
        other = "TXR/TR20992/NAT/ATO/00001"

        def fetcher(docid):
            if docid == DOCID:
                raise rulings.RulingsError(f"{docid}: fetch failed (IncompleteRead)")
            return page(docid=other), rulings.BASE_URL + other

        with mock.patch("sys.stderr", io.StringIO()):
            code = rulings.run([DOCID, other], tmp / "out", fetcher=fetcher, spacing=0)
        manifest = json.loads((tmp / "out" / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(code, 1)
        self.assertEqual([d["docid"] for d in manifest["documents"]], [other])

    def test_malformed_urls_refuse_the_document_without_raising_value_error(self) -> None:
        for value in ("http://www.ato.gov.au:notaport/content/corporate/about_this_site.htm#copyright",
                      "http://www.ato.gov.au:99999/content/corporate/about_this_site.htm#copyright"):
            with self.subTest(value=value), self.assertRaisesRegex(rulings.RulingsError, "no ATO reuse"):
                rulings.parse_document(page(notice=False, rights=value), DOCID)
        handle = FetchTests().response("https://www.ato.gov.au:notaport/law", body=page())
        with mock.patch.object(rulings.urllib.request, "urlopen", return_value=handle), \
                self.assertRaisesRegex(rulings.RulingsError, "malformed final URL"):
            rulings.fetch(DOCID)

    def test_raw_file_names_are_distinct_for_ids_that_differ_only_in_punctuation(self) -> None:
        names = {rulings._safe_name(d) for d in ("TXR/A-B", "TXR/A.B", "TXR/A_B")}
        self.assertEqual(len(names), 3)


if __name__ == "__main__":
    unittest.main()
