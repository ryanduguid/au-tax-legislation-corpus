"""Derive paragraph JSONL from named ATO Legal Database documents.

This stage reads ATO-authored documents (rulings, determinations, practical
compliance guidelines, decision impact statements, edited versions of private
advice) from https://www.ato.gov.au/law and writes one JSONL row per numbered
paragraph. It is a finding aid, like the rest of the corpus: not the authorised
text, not advice, and not a statement of the Commissioner's current view.

Reuse rests on the ATO's copyright notice, which each fetched page must carry
or point to: "You are free to copy, adapt, modify, transmit and distribute this
material as you wish (but not in any way that suggests the ATO or the
Commonwealth endorses you or any of your services or products)." A page that
shows neither the notice nor a dc.Rights link to it is refused, not guessed at.
Only ATO-authored document families are accepted; legislation and court
judgments sit in the same database under other terms and are refused by
docid prefix.

Scope is an explicit target list, capped at MAX_TARGETS documents a run. There
is no discovery crawl: a larger harvest needs its own authorisation, stated
page cap and purpose before anyone raises the cap.

The output directory must not exist. Every run writes a new one, keeps the
exact bytes each page returned beside their SHA-256, and never repairs or
overwrites an earlier run. A document whose text trips the corpus PII patterns
is excluded and listed, and the run exits 1 so a person looks at it.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import html
import html.parser
import http.client
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Callable, Sequence

import http_fetch
import pii_patterns

BASE_URL = "https://www.ato.gov.au/law/view/document?docid="
UA = "au-tax-legislation-corpus (+https://github.com/ryanduguid/au-tax-legislation-corpus)"
TIMEOUT = 90
MAX_BYTES = 8 * 1024 * 1024
MAX_TARGETS = 100
REQUEST_SPACING = 1.0

NOTICE = (
    "You are free to copy, adapt, modify, transmit and distribute this material as you wish "
    "(but not in any way that suggests the ATO or the Commonwealth endorses you or any of your "
    "services or products)."
)
COPYRIGHT_NOTICE_URL = "https://www.ato.gov.au/about-ato/using-our-website/copyright-notice"
# dc.Rights on Legal Database pages points at the legacy address of the site
# copyright notice; the path fragment is stable where the host and scheme vary.
RIGHTS_HOSTS = {"www.ato.gov.au", "ato.gov.au"}
RIGHTS_PATH = "/content/corporate/about_this_site.htm"
RIGHTS_FRAGMENT = "copyright"

# docid prefix -> document family. ATO-authored families only. Legislation
# (PAC/...), judgments (JUD/...) and other third-party material are refused.
FAMILIES = {
    "TXR": "Taxation Ruling",
    "TXD": "Taxation Determination",
    "GST": "GST ruling or determination",
    "CLR": "Class Ruling",
    "PRR": "Product Ruling",
    "COG": "Practical Compliance Guideline",
    "PSR": "Law Administration Practice Statement",
    "LIT/ICD": "Decision Impact Statement",
    "EV": "Edited version of private advice",
}

_DOCID = re.compile(r"^[A-Za-z]{2,4}(?:/[A-Za-z0-9.-]+){1,5}$")
_PARA = re.compile(r"^P(\d+[A-Z]?)$")
_HEAD = re.compile(r"^H\d+[A-Z]?$")
# Some families (decision impact statements) print "1. " without a P anchor.
_PRINTED = re.compile(r"^(\d+[A-Z]?)\.\s")
_BLOCKS = {"p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "td", "th", "dd", "dt", "blockquote"}
_SPACE = re.compile(r"\s+")


class RulingsError(Exception):
    """A target that cannot be fetched, verified or extracted."""


def family_of(docid: str) -> str | None:
    """Return the ATO document family for ``docid``, or None when it is not ATO-authored."""
    upper = docid.upper()
    for prefix, family in FAMILIES.items():
        if upper == prefix or upper.startswith(prefix + "/"):
            return family
    return None


def _norm(text: str) -> str:
    return _SPACE.sub(" ", text).strip()


class _LawParser(html.parser.HTMLParser):
    """Collect meta tags and the paragraph text blocks of a Legal Database page.

    Most documents put numbered paragraphs inside div#LawBody. There the table
    of contents (div.panel) and superscript footnote markers are skipped, and
    everything after the COPYRIGHT anchor or inside div#footnotes is not
    paragraph text. Edited versions of private advice have no LawBody: their
    paragraphs follow the div#ev_disclaimer_* box and end at the next
    container-fluid div (the "Back to top" bar) or the end of the article.

    Each block records the anchor names it contains and whether all of its text
    was bold, which is how an edited version marks a heading.
    """

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.meta: dict[str, str] = {}
        self.blocks: list[tuple[list[str], str, bool]] = []
        self.page_text: list[str] = []
        # All text in the document region (Law* divs, or the edited-version
        # area from its disclaimer on), including the parts skipped as rows.
        self.document_text: list[str] = []
        self.edited_version = False
        self._law = 0
        self._div_stack: list[str] = []
        self._in_body = 0
        self._skip = 0
        self._sup = 0
        self._bold = 0
        self._stop = False
        self._ev = 0  # 0 none, 1 inside the disclaimer, 2 collecting, 3 finished
        self._block: list[str] | None = None
        self._plain = False
        self._anchors: list[str] = []
        self._depth = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        a = {k: (v or "") for k, v in attrs}
        if tag == "meta" and a.get("name"):
            self.meta.setdefault(a["name"], a.get("content", ""))
        if tag == "div":
            role = ""
            classes = a.get("class", "").split()
            if a.get("id") == "LawBody":
                role = "body"
                self._in_body += 1
                self._law += 1
            elif a.get("id", "").startswith("Law"):
                role = "law"
                self._law += 1
            elif a.get("id", "").lower().startswith("ev_disclaimer") and self._ev == 0:
                role = "evdisc"
                self._ev = 1
                self.edited_version = True
            elif self._ev == 2 and "container-fluid" in classes:
                self._ev = 3
            elif self._in_body and a.get("id") == "footnotes":
                role = "skip"
                self._skip += 1
            elif self._in_body and "panel" in classes:
                role = "skip"
                self._skip += 1
            self._div_stack.append(role)
        if tag == "sup":
            self._sup += 1
        if tag in ("strong", "b"):
            self._bold += 1
        if tag == "a" and a.get("name"):
            name = a["name"].lstrip("#")
            if name.upper() == "COPYRIGHT" and self._in_body:
                self._stop = True
            if self._block is not None and name not in self._anchors:
                self._anchors.append(name)
        if tag in _BLOCKS and self._collecting():
            if self._block is None:
                self._block, self._anchors, self._depth, self._plain = [], [], 0, False
            self._depth += 1

    def handle_endtag(self, tag: str) -> None:
        if tag == "div" and self._div_stack:
            role = self._div_stack.pop()
            if role == "body":
                self._in_body -= 1
                self._law -= 1
            elif role == "law":
                self._law -= 1
            elif role == "skip":
                self._skip -= 1
            elif role == "evdisc" and self._ev == 1:
                self._ev = 2
        if tag == "article" and self._ev == 2:
            self._ev = 3
        if tag == "sup" and self._sup:
            self._sup -= 1
        if tag in ("strong", "b") and self._bold:
            self._bold -= 1
        if tag in _BLOCKS and self._block is not None:
            self._depth -= 1
            if self._depth <= 0:
                text = _norm("".join(self._block))
                if text or self._anchors:
                    self.blocks.append((self._anchors, text, bool(text) and not self._plain))
                self._block, self._anchors = None, []

    def handle_data(self, data: str) -> None:
        self.page_text.append(data)
        if self._law or self._ev in (1, 2):
            self.document_text.append(data)
        if self._block is not None and not self._sup and self._collecting():
            self._block.append(data)
            if data.strip() and not self._bold:
                self._plain = True

    def _collecting(self) -> bool:
        if self._ev == 2:
            return True
        return bool(self._in_body) and not self._skip and not self._stop


def _canonical(docid: str) -> str:
    """Case-insensitive and without a trailing slash; nothing else is treated as the same id."""
    return docid.strip().rstrip("/").upper()


def _is_ato_rights_link(value: str) -> bool:
    """True only for the ATO copyright notice address the Legal Database cites in dc.Rights."""
    try:
        parts = urllib.parse.urlsplit(value.strip())
        port = parts.port
    except ValueError:  # malformed URL or port: not the ATO address
        return False
    return (parts.scheme in ("http", "https") and (parts.hostname or "") in RIGHTS_HOSTS
            and port is None and parts.path == RIGHTS_PATH
            and parts.fragment == RIGHTS_FRAGMENT and not parts.query)


def parse_document(raw: bytes, docid: str) -> dict[str, Any]:
    """Verify one fetched page and return its metadata, licence basis and paragraphs."""
    try:
        page = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise RulingsError(f"{docid}: response is not UTF-8") from exc
    parser = _LawParser()
    parser.feed(page)
    parser.close()
    meta = {k.lower(): v for k, v in parser.meta.items()}

    reference = meta.get("ato.reference.id", "")
    if not reference or _canonical(reference) != _canonical(docid):
        raise RulingsError(f"{docid}: page reference id {reference!r} does not match the target")

    text = _norm(html.unescape("".join(parser.page_text)))
    rights = meta.get("dc.rights", "") or meta.get("dc_rights", "")
    if NOTICE in text:
        basis = "document-notice"
    elif _is_ato_rights_link(rights):
        basis = "site-notice-via-dc-rights"
    else:
        raise RulingsError(f"{docid}: no ATO reuse notice and no dc.Rights link to it")

    paragraphs: list[dict[str, str]] = []
    heading = ""
    current: dict[str, str] | None = None
    if parser.edited_version:
        # No paragraph numbers in the source: number the non-heading blocks in
        # order and say so in the numbering field, so no row claims a number
        # the ATO did not print.
        for _anchors, block_text, bold in parser.blocks:
            if bold:
                heading = block_text
            elif block_text:
                paragraphs.append({"paragraph": str(len(paragraphs) + 1), "heading": heading,
                                   "text": block_text, "numbering": "sequential"})
        if not paragraphs:
            raise RulingsError(f"{docid}: no paragraphs found after the edited-version disclaimer")
    anchored = any(_PARA.match(name) for anchors, _text, _bold in parser.blocks for name in anchors)
    for anchors, block_text, _bold in ([] if parser.edited_version else parser.blocks):
        para = next((m.group(1) for m in map(_PARA.match, anchors) if m), None)
        if para is None and not anchored and not any(_HEAD.match(n) for n in anchors):
            printed = _PRINTED.match(block_text)
            para = printed.group(1) if printed else None
        if any(_HEAD.match(name) for name in anchors) and para is None:
            heading = block_text
            current = None
            continue
        if para is not None:
            current = {"paragraph": para, "heading": heading, "text": block_text,
                       "numbering": "document"}
            paragraphs.append(current)
        elif current is not None and block_text:
            current["text"] = f"{current['text']} {block_text}"
    if not paragraphs:
        raise RulingsError(f"{docid}: no numbered paragraphs found in LawBody")

    return {
        "reference": reference,
        "title": meta.get("dc.title", "") or meta.get("dc_title", ""),
        "document_type": meta.get("dc.type.documenttype", "") or meta.get("dc.description", ""),
        "issued": meta.get("dc.date.issued", "") or meta.get("dc_date_issued", ""),
        "licence_basis": basis,
        "paragraphs": paragraphs,
        "document_text": _norm(" ".join(parser.document_text)),
    }


def pii_findings(texts: Sequence[str]) -> list[str]:
    """Kinds of personal data the corpus patterns find in ``texts``, never the identifiers."""
    kinds: set[str] = set()
    for text in texts:
        if pii_patterns.has_private_person_registration_pair(text):
            kinds.add("name-with-registration-number")
        for kind, _digest in pii_patterns.contact_fingerprints(text):
            kinds.add(kind)
    return sorted(kinds)


def scanned_texts(parsed: dict[str, Any]) -> list[str]:
    """Every piece of text that reaches the output or sits in the retained document region."""
    texts = [parsed["title"], parsed["document_type"], parsed["issued"], parsed["document_text"]]
    for row in parsed["paragraphs"]:
        texts.extend((row["heading"], row["text"]))
    return texts


def fetch(docid: str) -> tuple[bytes, str]:
    """Return the page bytes and final URL for ``docid``, refusing oversize or off-site responses."""
    url = BASE_URL + urllib.parse.quote(docid, safe="/")
    request = urllib.request.Request(url, headers={"User-Agent": UA})
    failure = "no attempt made"
    for _attempt in http_fetch.attempts():
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                final = response.geturl()
                kind = response.headers.get("Content-Type", "")
                body = response.read(MAX_BYTES + 1)
            break
        except urllib.error.HTTPError as exc:
            failure = f"HTTP {exc.code}"
            # Only a rate limit or a server fault is worth another attempt.
            if exc.code != 429 and exc.code < 500:
                raise RulingsError(f"{docid}: fetch failed ({failure})") from exc
        except (urllib.error.URLError, http.client.HTTPException, OSError) as exc:
            failure = type(exc).__name__
    else:
        raise RulingsError(f"{docid}: fetch failed ({failure})")
    try:
        parts = urllib.parse.urlsplit(final)
        port = parts.port
    except ValueError as exc:
        raise RulingsError(f"{docid}: malformed final URL") from exc
    host = parts.hostname or ""
    if parts.scheme != "https" or host != "www.ato.gov.au" or port not in (None, 443):
        raise RulingsError(f"{docid}: redirected off https://www.ato.gov.au to {parts.scheme}://{parts.netloc}")
    if not kind.startswith("text/html"):
        raise RulingsError(f"{docid}: unexpected content type {kind!r}")
    if len(body) > MAX_BYTES:
        raise RulingsError(f"{docid}: response larger than {MAX_BYTES} bytes")
    return body, final


def load_targets(path: Path) -> list[str]:
    """Read and validate the target list: a JSON array of distinct ATO-authored docids."""
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, list) or not all(isinstance(item, str) for item in data):
        raise RulingsError("targets must be a JSON array of docid strings")
    targets = [item.strip() for item in data]
    if len(set(_canonical(t) for t in targets)) != len(targets):
        raise RulingsError("targets contain a duplicate docid")
    if len(targets) > MAX_TARGETS:
        raise RulingsError(f"{len(targets)} targets exceed the cap of {MAX_TARGETS}")
    for docid in targets:
        if not _DOCID.match(docid):
            raise RulingsError(f"{docid!r} is not a Legal Database docid")
        if family_of(docid) is None:
            raise RulingsError(f"{docid}: not an ATO-authored document family")
    return targets


def _safe_name(docid: str) -> str:
    """A readable file stem plus a digest of the canonical docid, so distinct ids never collide."""
    readable = re.sub(r"[^A-Za-z0-9]+", "_", docid).strip("_")
    return f"{readable}-{hashlib.sha256(_canonical(docid).encode()).hexdigest()[:16]}"


def run(targets: Sequence[str], out: Path, fetcher: Callable[[str], tuple[bytes, str]] = fetch,
        spacing: float = REQUEST_SPACING, today: str | None = None) -> int:
    """Fetch, verify and extract each target into the new directory ``out``."""
    if out.exists():
        raise RulingsError(f"{out} already exists; every run writes a new directory")
    fetched_on = today or datetime.date.today().isoformat()
    documents: list[dict[str, Any]] = []
    excluded: list[dict[str, str]] = []
    rows: list[str] = []
    raw_files: dict[str, bytes] = {}
    for index, docid in enumerate(targets):
        if index and spacing:
            time.sleep(spacing)
        try:
            raw, final_url = fetcher(docid)
            parsed = parse_document(raw, docid)
        except RulingsError as exc:
            excluded.append({"docid": docid, "reason": str(exc)})
            continue
        kinds = pii_findings(scanned_texts(parsed))
        if kinds:
            excluded.append({"docid": docid, "reason": "personal data patterns: " + ", ".join(kinds)})
            continue
        digest = hashlib.sha256(raw).hexdigest()
        raw_name = f"raw/{_safe_name(docid)}.html"
        if raw_name in raw_files:
            raise RulingsError(f"{docid}: raw file name {raw_name} collides with an earlier target")
        raw_files[raw_name] = raw
        documents.append({
            "docid": docid,
            "reference": parsed["reference"],
            "family": family_of(docid),
            "title": parsed["title"],
            "document_type": parsed["document_type"],
            "issued": parsed["issued"],
            "source_url": final_url,
            "fetched_on": fetched_on,
            "sha256": digest,
            "bytes": len(raw),
            "raw_file": raw_name,
            "licence_basis": parsed["licence_basis"],
            "paragraphs": len(parsed["paragraphs"]),
        })
        for row in parsed["paragraphs"]:
            rows.append(json.dumps({
                "docid": docid,
                "family": family_of(docid),
                "title": parsed["title"],
                "paragraph": row["paragraph"],
                "numbering": row["numbering"],
                "heading": row["heading"],
                "text": row["text"],
                "source_url": final_url,
                "source_sha256": digest,
                "fetched_on": fetched_on,
                "licence_basis": parsed["licence_basis"],
            }, ensure_ascii=False, sort_keys=True))

    out.mkdir(parents=True)
    (out / "raw").mkdir()
    for name, raw in raw_files.items():
        with open(out / name, "xb") as handle:
            handle.write(raw)
    with open(out / "rulings.jsonl", "x", encoding="utf-8", newline="\n") as handle:
        handle.write("".join(line + "\n" for line in rows))
    manifest = {
        "stage": "rulings",
        "fetched_on": fetched_on,
        "targets": list(targets),
        "documents": documents,
        "excluded": excluded,
        "rows": len(rows),
        "reuse_notice": NOTICE,
        "reuse_notice_url": COPYRIGHT_NOTICE_URL,
    }
    with open(out / "manifest.json", "x", encoding="utf-8", newline="\n") as handle:
        handle.write(json.dumps(manifest, indent=2, ensure_ascii=False, sort_keys=True) + "\n")
    with open(out / "LICENCE-NOTICE.md", "x", encoding="utf-8", newline="\n") as handle:
        handle.write(
            "# ATO Legal Database material\n\n"
            "The rows in rulings.jsonl are derived from documents published by the Australian "
            "Taxation Office on its Legal Database. They are reproduced under the ATO copyright "
            f"notice ({COPYRIGHT_NOTICE_URL}):\n\n"
            f"> {NOTICE}\n\n"
            "Copyright in the source documents remains with the Australian Taxation Office for "
            "the Commonwealth of Australia. Neither the ATO nor the Commonwealth endorses this "
            "corpus. The rows are not the authorised text; read the source document at its "
            "source_url before relying on it. An edited version of private advice cannot be "
            "relied on by anyone.\n"
        )
    print(f"rulings: {len(documents)} documents, {len(rows)} rows, {len(excluded)} excluded -> {out}")
    for item in excluded:
        print(f"  excluded {item['docid']}: {item['reason']}", file=sys.stderr)
    return 1 if excluded else 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden rulings",
        description="Derive paragraph JSONL from named ATO Legal Database documents.",
    )
    parser.add_argument("targets", type=Path, help="JSON array of docids (at most %d)" % MAX_TARGETS)
    parser.add_argument("--out", type=Path, required=True, help="new output directory; must not exist")
    args = parser.parse_args(list(argv) if argv is not None else None)
    try:
        return run(load_targets(args.targets), args.out)
    except RulingsError as exc:
        print(f"rulings: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
