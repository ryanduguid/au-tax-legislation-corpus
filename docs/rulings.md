# ATO Legal Database rulings stage

`python -m fadden rulings TARGETS.json --out NEW_DIR` fetches named ATO-authored
documents from the Legal Database and writes one JSONL row per paragraph. It is a
finding aid like the rest of the corpus: not the authorised text, not advice and not
a statement of the Commissioner's current view. Read the source document at its
`source_url` before relying on anything.

## Reuse basis

The ATO copyright notice at
<https://www.ato.gov.au/about-ato/using-our-website/copyright-notice> (last updated
27 August 2012, read 24 September 2026) says:

> You are free to copy, adapt, modify, transmit and distribute this material as you
> wish (but not in any way that suggests the ATO or the Commonwealth endorses you or
> any of your services or products).

Rulings, practical compliance guidelines and decision impact statements print the
same sentence at their foot; edited versions of private advice carry a `dc.Rights`
link to the notice instead. The stage accepts a page only when it shows the sentence
(`licence_basis: document-notice`) or that link (`site-notice-via-dc-rights`), and
refuses it otherwise. Legislation and court judgments are in the same database under
other terms, so only these docid families are accepted:

| docid prefix | Family |
| --- | --- |
| `TXR` | Taxation Ruling |
| `TXD` | Taxation Determination |
| `GST` | GST ruling or determination |
| `CLR` | Class Ruling |
| `PRR` | Product Ruling |
| `COG` | Practical Compliance Guideline |
| `PSR` | Law Administration Practice Statement |
| `LIT/ICD` | Decision Impact Statement |
| `EV` | Edited version of private advice |

Every run writes `LICENCE-NOTICE.md` with the notice and a non-endorsement statement.
Keep it with any copy of the rows.

## Scope and safety

- **Named targets only.** `TARGETS.json` is a JSON array of distinct docids, at most
  100 a run. There is no discovery crawl. A larger harvest needs its own
  authorisation, a stated page cap and a purpose before the cap changes.
- **New directory every run.** `--out` must not exist. The stage never overwrites,
  repairs or deletes an earlier run, and keeps each page's exact bytes under `raw/`
  beside their SHA-256 in `manifest.json`.
- **Personal data fails closed.** Each document's paragraphs go through the corpus
  patterns in `pii_patterns.py` (a name beside an 8-digit registration number,
  email addresses, phone numbers, tax file numbers). A match excludes the whole
  document, records only the kind of match, and makes the run exit 1. The stage has
  no allowlist yet, so a document that prints an ATO contact email (such as
  PCG 2024/1) is excluded until a person reviews it.
- **Fetch rules.** One request a second, the corpus user agent, at most 8 MiB a
  page, HTML only, and no redirect off `www.ato.gov.au`. An HTTP 429 or 5xx is
  retried with the shared `http_fetch` budget; any other HTTP error stops that
  document.
- **Not committed, not published.** Like the rest of the corpus, run output stays
  out of git. Publishing it is a separate decision a person makes.

## Output

| File | Contents |
| --- | --- |
| `rulings.jsonl` | One row per paragraph (fields below) |
| `manifest.json` | Run date, targets, per-document record, exclusions and reasons, row count, notice |
| `raw/<docid>.html` | The exact bytes each accepted page returned |
| `LICENCE-NOTICE.md` | Reuse notice and non-endorsement statement |

### `rulings.jsonl` fields

| Field | Meaning |
| --- | --- |
| `docid` | Legal Database document id, as targeted |
| `family` | Document family from the table above |
| `title` | `DC.Title` (or `DC_TITLE`) meta value |
| `paragraph` | Paragraph number: the document's own, or 1..n for edited versions |
| `numbering` | `document` when the ATO printed the number; `sequential` when the stage numbered unnumbered paragraphs |
| `heading` | Nearest preceding heading |
| `text` | Paragraph text with footnote markers removed; list items and continuation blocks join the paragraph they follow |
| `source_url` | Final URL the page came from |
| `source_sha256` | SHA-256 of the raw page bytes |
| `fetched_on` | Run date (ISO 8601) |
| `licence_basis` | `document-notice` or `site-notice-via-dc-rights` |

### How paragraphs are found

- Rulings and guidelines: blocks inside `div#LawBody` keyed by `P<n>` anchors, with
  headings from `H<n>` anchors. The table of contents, footnotes and everything after
  the copyright anchor are skipped.
- Decision impact statements: the same body, but numbered in the text (`1. ...`)
  without anchors. Printed numbers are used only when a page has no `P` anchors.
- Edited versions of private advice: the paragraphs after the `ev_disclaimer` box,
  ending at the "Back to top" bar; a paragraph whose text is all bold is a heading.

## Smoke run, 24 September 2026

Four targets, run from a Windows checkout into a scratch directory (not committed):

| docid | Result |
| --- | --- |
| `TXR/TR20231/NAT/ATO/00001` | 170 paragraphs, `document-notice` |
| `LIT/ICD/M47/2025/00001` | 47 paragraphs, `document-notice` |
| `EV/1052479123827` | 74 paragraphs, `site-notice-via-dc-rights` |
| `COG/PCG20241/NAT/ATO/00001` | Excluded: email pattern |
