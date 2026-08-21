# Commonwealth tax legislation: a corpus builder

[![Verify](https://github.com/ryanduguid/SirArthurFadden/actions/workflows/verify.yml/badge.svg)](https://github.com/ryanduguid/SirArthurFadden/actions/workflows/verify.yml) [![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A pipeline of small Python scripts that downloads every in-force principal
Commonwealth tax title from the Federal Register of Legislation and turns it
into a retrieval corpus: full-text markdown with provenance front matter, one
JSONL row per section, and a derived rates-and-thresholds index.

No dependencies beyond the standard library and `curl`. No API key: the
Register's API is free and unauthenticated.

## For practitioners

Run the pipeline and you get the current compilations of 946 tax titles as
plain markdown and per-section JSONL rows, each row carrying its register id,
compilation number, compilation date and source URL, so a quoted provision can
be traced back to the exact version it came from. You also get a derived index
of 4,149 rates and thresholds across 286 titles, each entry citing the Act and
section that sets it. It is a finding aid built from the Register's own
documents, not advice and not the authorised text.

**This repository holds the code, not the corpus.** The output is roughly a
gigabyte and includes material that should not be redistributed casually; see
[What this deliberately does not ship](#what-this-deliberately-does-not-ship).
Run the pipeline and you get your own copy from the primary source, which is
the point.

## What it produces

Last full run, 4 August 2026:

| | |
|---|---|
| Titles | 946: 175 Acts, 675 legislative instruments, 96 notifiable instruments |
| Retrieval rows | 21,784 |
| Words | 6,041,512 |
| EPUB downloaded | 37.5 MB |
| Rates index | 4,149 entries across 286 titles |
| Download failures | 0 |
| Parse failures | 0 |

That run predates the volume-gate fix in `extract.py` and the table-stack fix
after it, and so does the `manifest_md.json` shipped beside it, so the next full
run reports more rows and words:

- the volume gate, which silently dropped five volumes of F2025L00281;
- the table stack, which let a table nested inside a cell discard the rows its
  enclosing table had already parsed. Four titles carry nested tables
  (F2005B01198, F2005L00211, F2005L01901, F2026L00716) and between them
  recovered 84 text fragments; the other 942 titles parse identically;
- the pre-section row. Text ahead of a document's first section reached the
  markdown but had no row to hold it, so it never reached `sections.jsonl`,
  which is the file retrieval reads. 511 of the 946 titles gain one
  `Introductory material` row each, carrying material such as the long title,
  the assent note and the enacting words. No title loses retrievable text;
- the pre-body table gate, which dropped a table or figure sitting between two
  pre-body paragraphs while keeping the prose either side. This changes the
  markdown for one title.

See [BUILD.md](BUILD.md).

Layout under the corpus root:

```
epub/<register_id>.epub              the file exactly as the Register served it
markdown/<register_id>/<id>.md       full text, YAML front matter
markdown/<register_id>/sections.jsonl one row per section, ready for RAG
markdown/<register_id>/endnotes.md   amendment history, kept out of the sections
rates/rates.jsonl, rates/RATES.md    derived rates and thresholds index
sources.json, INDEX.md, README.md, LICENCE-NOTICE.md
```

Every JSONL row carries its own register id, collection, compilation number,
compilation date, section, source URL, licence and attribution, because rows
travel independently of the file they came from.

## Running the pipeline

The builder deliberately does **not** read `ATO_KB_ROOT` or `ATO_DIST`.
Environment-selected output roots made it possible for a poisoned process
environment to redirect downloads, deletes, or distribution output. In a source
checkout, generated corpus files live in the deterministic `./corpus/`
directory. To build at another location, place the scripts in that location's
`build/` directory; they then use the parent directory as the corpus root.

From an empty checkout, run the stages in order:

```bash
python discover.py      # list every in-force title matching the tax keywords -> titles_all.json, titles_principal.json
python versions.py      # dedup titles and resolve each one's current version date -> acts_resolved.json
python download.py      # fetch the current EPUB for each resolved title -> ./corpus/epub/*.epub, manifest_raw.json
python probe13.py       # only if download reports no_epub: probe version history -> probe13.json
python retry13.py       # fetch the latest published compilation for those titles -> retry13_patch.json; patches manifest_raw.json in place
python extract.py       # convert each EPUB to markdown and per-section JSONL -> ./corpus/markdown/**, manifest_md.json
python pii_scan.py      # flag disciplinary-register rows naming private individuals -> pii_flagged.json
python pii_scan2.py     # second pass at a lower threshold, plus emails, phones and TFNs
python finalize.py      # write the corpus-level index and licence files -> ./corpus/sources.json, INDEX.md, README.md, LICENCE-NOTICE.md
python rates.py         # derive the rates-and-thresholds index -> ./corpus/rates/rates.jsonl, RATES.md
python check_current.py # read-only staleness check against the Register
```

The PII scans run before `finalize.py` because the generated corpus README
reports the scan's totals; `finalize.py` refuses to run without
`pii_flagged.json` rather than print counts no scan produced.

## Exporting a change-review observation

`export_monitor_contract.py` projects a completed corpus `sources.json` and a
separately collected, structured observation-facts JSON file into the exact
v1 baseline and observation inputs for
[`tax-radar-au`](https://github.com/ryanduguid/tax-radar-au):

```bash
python export_monitor_contract.py corpus/sources.json observation-facts.json --out monitor-input
```

The facts file has schema version `au-tax-register-observation-facts.v1`. It
contains the observation timestamp, whether coverage is complete, and one
stateful result for each observed Register id. The exporter validates the
scope, collection, UTC timestamps, HTTPS evidence links and state-specific
fields, rejects duplicate JSON members and control characters, and refuses an
input whose resolved path is either output filename. Existing output names must
be ordinary files, never directories, links, junctions or other special paths.
Exactly one writer may publish to an output directory at a time; any existing
publisher lock fails closed. If it has no recovery artefacts, an operator may
remove `.monitor-contract.publish.lock` after confirming its owner is no longer
running. If rollback itself fails, the exporter retains that lock and every unrecovered
`.bak` file, so no later publisher proceeds. The operator must restore or
deliberately retire the old/new pair and its recovery artefacts before removing
the lock.

The exporter stages both files and restores the prior pair after an ordinary
write failure. `monitor-baseline.json` and `register-observation.json` are
replaced individually, not by a cross-file filesystem transaction. A process
or power loss between replacements can therefore leave an old/new pair and
lock or rollback files for recovery. A fully crash-atomic publication needs
versioned pair directories and an atomic generation pointer, which this v1
adapter does not create.

It does not call the Register, download a document, decide the legal effect of
a change, or update any workflow. `synthetic` remains the output mode because
the monitor's current v1 schema is a provenance-first review-queue contract;
the human technical review remains separate.

To produce a corpus you can pass on to someone else:

```bash
python dist.py          # build the redistributable subset -> ./corpus/dist/
python dist_verify.py   # check the built subset against its own claims, exits non-zero on failure
```

`corpus_paths.py`, `curl_fetch.py` and `pii_patterns.py` are shared modules the
stages import; they are not run directly.

The intermediate JSON files (`titles_all.json`, `acts_resolved.json`,
`manifest_raw.json`, `probe13.json`, `retry13_patch.json`) are build outputs
the pipeline regenerates; they are not committed or shipped. Each stage writes
its file beside the scripts, so any single stage re-runs without repeating the
earlier ones. `manifest_md.json` from the 4 August run stays committed because
the regression suite pins its pre-fix figures against BUILD.md.

`download.py` sleeps 10 seconds between titles, honouring the `Crawl-delay` in
https://www.legislation.gov.au/robots.txt, so a full download is about 2 hours
40 minutes. It skips any EPUB already present whose sidecar records the same
version. Discovery is about 40 minutes; the three local stages take about three
minutes combined.

## Why the code is shaped the way it is

[BUILD.md](BUILD.md) lists every trap the code exists to avoid. Each one cost a
real defect. The short version:

- **Unordered `$skip`/`$top` paging silently drops rows.** No `$orderby` meant
  142 of 813 titles vanished, including the Tax Agent Services Act 2009.
- **`curl` does not truncate `-o` on transport failure**, so a shared temp file
  re-reads the previous response and hands one Act another Act's compilation.
- **The download endpoint answers in two shapes**, raw EPUB bytes or a JSON
  envelope with the file base64 inside. Sniff the first byte.
- **HTTP errors and non-EPUB responses stop the download stage.** They are not
  evidence that a document is absent and must not be counted as `no_epub`.
  Only a validated staged download is moved to an `.epub` path.
- **The current version can have no document.** The Register records that an
  amendment commenced before publishing the compilation, so the URL 404s. That
  is not a broken download. Only an explicit null `registerId` on the current
  version is recorded as `no_epub`; the count changes as compilations are
  published.
- **Acts use two Word templates; instruments use dozens.** Deciding the template
  from the markup fails, because cosmetic classes look structural. Run the
  structural pass, and only when it finds nothing at all re-run with the
  bare-paragraph fallback, verified not to change a single Act row.
- **Never filter images by byte size.** Doing so deleted a GST decision
  flowchart and a maintenance-income formula.

## Accuracy and limits

Not the authorised text. Authorised versions are PDF only, stamped under
sections 15ZA and 15ZB of the *Legislation Act 2003*. Everything here derives
from the EPUB reading view.

Check the `collection` field before relying on a provision. An Act and a
regulation made under it are not interchangeable, and an instrument can be
disallowed or sunset while its enabling Act stands.

Selection uses title keywords (Tax, Excise, Superannuation, Customs Tariff,
Medicare Levy) applied to the Act, LegislativeInstrument and
NotifiableInstrument collections. A tax-relevant title without one of those
words in its name is absent. The Register's `contains(name,...)` also matches
former titles and substrings, which is how the Passenger Movement Charge Act
1978 arrived under its old name, the Departure Tax Act 1978, and how two dozen
`AD/ROTAX/...` airworthiness directives arrived because "Rotax" contains "tax".
`sources.json` records `keywords_in_name` rather than filtering them out, since
any rule strict enough to drop Rotax also drops the Departure Tax Act.

Some titles have no published compilation for the version now in force, so they
carry the last compilation the Register holds, marked `version_is_current:
false` in the front matter and on every row. There is no newer document to
re-download: the URL built from the in-force date answers 404.

## What this deliberately does not ship

The corpus itself. It is about a gigabyte and this code rebuilds it in an
afternoon, so shipping it would trade a lot of storage for very little. Two
parts of it should not be redistributed at all, and `dist.py` exists to separate
them out:

1. **The EPUBs embed the Commonwealth Coat of Arms**, which is excluded from the
   Register's CC BY 4.0 grant, along with any third-party material the Register
   has not cleared. Stripping the image would also destroy the only thing that
   makes shipping EPUBs worthwhile, which is that they are byte-identical to
   what the Register served. The markdown and JSONL carry no image data at all, because
   `extract.py` never emits any, so they need nothing removed.
2. **Twelve titles name private individuals.** The Tax Practitioners Board
   registers terminations and suspensions of tax and BAS agents as notifiable
   instruments: tables of named people with registration numbers and the
   provision breached, about 5,400 name mentions across 169 rows. Public on the
   Register as PDFs you read one at a time; shipping them as dataset rows makes
   them name-searchable at scale, which is a different act.

`pii_scan.py` finds the second category without being told where to look. It
tests every row in all 946 titles for personal names appearing alongside agent
registration numbers, because a bare name test flags the whole corpus:
legislation names Ministers, Commissioners and litigants constantly, and the
registration number is what separates a disciplinary register from a statute.
`pii_scan2.py` re-runs at a lower threshold and sweeps for emails, phone numbers
and eight- or nine-digit tax file numbers. On the source snapshot retrieved
2026-08-04, the JSONL scan finds ten occurrences of four unique organisational
contacts across three titles: two government email addresses and two government
landlines. They are approved by a title-bound SHA-256 fingerprint in
`pii_contact_allowlist.json`; the policy does not store the identifiers
themselves. An allowlist entry is reserved for a manually reviewed
organisational contact published in that primary instrument; personal contacts
and TFNs are never allowlisted. Pattern false positives are fixed in
`pii_patterns.py` with a regression test rather than approved as data. Every
exception is bound to the identifier kind, digest and Register id and must carry
a review reason.

The scan, `dist.py` preflight and `dist_verify.py` all fail on a new or moved
contact identifier, and their diagnostics contain only Register ids and
truncated fingerprints. The publication gates require each title's exact
declared Markdown/JSONL file inventory and inspect every file in it, covering
the human-readable Markdown and endnotes as well as `sections.jsonl`. A private
name beside a registration number in any representation fails, as does an
unreadable file, binary control data, a nested path or an unexpected file.

For that validated source snapshot, `dist.py` writes `dist/`: 934 titles, 21,728
rows, 6,068,848 words and 109,492,029 bytes, with a `REMOVED.md` listing every
exclusion and its Register link so the omission is visible and reversible.
`dist_verify.py` checks the result against its own claims and exits non-zero if
any check fails.

## Licence

The code is MIT. See [LICENSE](LICENSE).

Everything it downloads is Commonwealth legislation from the Federal Register of
Legislation, licensed
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), excluding the
Commonwealth Coat of Arms and any third-party material. `finalize.py` writes a
`LICENCE-NOTICE.md` into the corpus with the required attribution wording, which
differs for the unchanged EPUBs ("Sourced from") and the converted markdown
("Based on content from").

## Related

ATO rulings and guidance are not legislation and are not on the Register. The
`simplelex` datasets on HuggingFace cover them. See
[ATO-Australian-Tax-Rulings-and-Guidance](https://huggingface.co/datasets/simplelex/ATO-Australian-Tax-Rulings-and-Guidance).
Validated against the live ATO Legal Database on 4 August 2026: the document
text is close to verbatim, but `Date_of_Issue` is empty on all 5,677 public
rulings, `Title` holds the document type rather than the title, `Is_Repealed`
flags instruments that are in force, and its legislative instruments are as-made
text rather than current compilations. Good for retrieval over document text,
not for determining currency.
