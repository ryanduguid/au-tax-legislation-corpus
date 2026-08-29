# au-tax-legislation-corpus

```
+----------------------------------------------------------------------+
|                      au-tax-legislation-corpus                       |
+----------------------------------------------------------------------+
|        Provenance-rich corpus of Commonwealth tax legislation        |
+----------------------------------+-----------------------------------+
| DR  what it gives you            | CR  what it needs                 |
+----------------------------------+-----------------------------------+
| 946 tax titles as markdown       | curl and internet access          |
| JSONL rows with provenance       | Federal Register API access       |
| rates and thresholds index       | -                                 |
+----------------------------------+-----------------------------------+
```

### Commonwealth tax legislation: a corpus builder

[![Verify](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/verify.yml/badge.svg)](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/verify.yml) [![License: MIT](https://img.shields.io/badge/License-MIT-4F485E.svg?labelColor=04001F)](LICENSE)

A packaged pipeline (`python -m fadden <stage>`) that downloads every in-force principal
Commonwealth tax title from the Federal Register of Legislation and turns it
into a retrieval corpus: full-text markdown with provenance front matter, one
JSONL row per section, and a derived rates-and-thresholds index.

No dependencies beyond the standard library and `curl`. No API key: the
Register's API is free and unauthenticated.

## Two halves

This repository holds a producer and the consumer written against it.

- **The corpus builder**, described below and in [BUILD.md](BUILD.md). Standard
  library and `curl` only.
- **The change-review queue**, described in [RADAR.md](RADAR.md). Installs as
  `tax-radar-au` and reads the builder's reviewed observation output.

They arrived here as separate repositories with a covenant between them.
`fadden/export_monitor_contract.py` sets out what the producer owes, a
baseline source index sets out what the consumer will accept, and
`ContractError` sets out what happens when either side breaks the terms.
Nothing enforced any of it across the repository boundary. Now
`tests/corpus/test_monitor_contract.py` does, on every run, instead of only
on a machine that happened to have both clones.

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

From an empty checkout, run the stages in order. Stage names are unchanged;
the dispatcher lives at `python -m fadden <stage>`. Intermediate JSON files
are written beside the stage modules under `fadden/`.

```bash
python -m fadden discover      # list every in-force title matching the tax keywords -> fadden/titles_all.json, fadden/titles_principal.json
python -m fadden versions      # dedup titles and resolve each one's current version date -> fadden/acts_resolved.json
python -m fadden download      # fetch the current EPUB for each resolved title -> ./corpus/epub/*.epub, fadden/manifest_raw.json
python -m fadden probe13       # only if download reports no_epub: probe version history -> fadden/probe13.json
python -m fadden retry13       # fetch the latest published compilation for those titles -> fadden/retry13_patch.json; patches manifest_raw.json in place
python -m fadden extract       # convert each EPUB to markdown and per-section JSONL -> ./corpus/markdown/**, fadden/manifest_md.json
python -m fadden pii_scan      # flag disciplinary-register rows naming private individuals -> fadden/pii_flagged.json
python -m fadden pii_scan2     # second pass at a lower threshold, plus emails, phones and TFNs
python -m fadden finalize      # write the corpus-level index and licence files -> ./corpus/sources.json, INDEX.md, README.md, LICENCE-NOTICE.md
python -m fadden rates         # derive the rates-and-thresholds index -> ./corpus/rates/rates.jsonl, RATES.md
python -m fadden check_current # read-only staleness check against the Register
python -m fadden capture_register -- fadden/manifest_md.json --out build/register-capture-20260829
python -m fadden export_live_evidence_bundles -- build/register-capture-20260829 --out build/live-evidence-20260829
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
python -m fadden export_monitor_contract -- corpus/sources.json observation-facts.json --out monitor-input
```

The facts file may use `au-tax-register-observation-facts.v1`, v2 or v3. It
contains the observation timestamp, whether coverage is complete, and one
stateful result for each observed Register id. V3 also binds every observation
to the SHA-256 of the exact source payload used, its content kind and media
type. The exporter validates the
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
a change, or update any workflow. `synthetic` remains this exporter's output
mode; live source capture is a separate, deliberately incompatible contract.

## Capturing live Register evidence

The live capture command checks every title in a rich corpus manifest and
retains the exact bounded Federal Register metadata response bytes behind its
decision:

```bash
python -m fadden capture_register -- fadden/manifest_md.json --out build/register-capture-20260829
```

It writes one new immutable directory containing `monitor-baseline.json`,
`register-capture.json`, `register-observation.json` and content-addressed JSON
under `evidence/`. Existing outputs are never overwritten. Inputs and existing
ancestors must be ordinary filesystem objects; all generated bytes and
cross-file hashes are revalidated before one directory rename.

The checked-in manifest currently contains 946 titles, so the enforced
1.5-second request spacing makes a no-retry run about 24 minutes. Empty current
lookups add an ordered history query, and retryable failures wait at least six
seconds. The standard-library HTTPS adapter refuses redirects, uses a 90-second
socket timeout, attempts a request at most three times and needs no API key.
Operational schedulers remain out of scope; if one is added later, incremental
work should run outside the Register's preferred 08:00-20:00 Australian window.

Forty-seven legacy titles have an explicit null compilation number in the
checked-in manifest, and the Register API uses that representation for
unnumbered compilations. The capture preserves the manifest fact instead of
inventing a number. It accepts an unchanged result only when the live null and
compilation date both match; an ambiguous later null-numbered version fails
closed as `LOOKUP_FAILED`.

Every title receives one of `UNCHANGED`, `SUPERSEDED`,
`CURRENT_NO_PUBLISHED_COMPILATION`, `NO_LONGER_IN_FORCE` or `LOOKUP_FAILED`.
Source failures do not shrink the audit scope: the command continues, retains a
complete graph and marks it `BLOCKED`. A `VERIFIED` result means complete scope
with no lookup failures; it is not a legal conclusion or publication decision.

Do not commit raw capture directories. HTTPS authenticates the endpoint in
transit, but the Register does not sign each response. Stage 3B must carry the
retained source bytes with authenticated producer provenance, the Register's
prescribed attribution and independent publisher-side validation before a live
development can enter the public site.

## Exporting live publication evidence

After a complete Stage 3A capture is locally validated as `VERIFIED`, export
its live-only v2 candidate bundles with:

```bash
python -m fadden export_live_evidence_bundles -- build/register-capture-20260829 --out build/live-evidence-20260829
```

The destination must be absent. The exporter snapshots and validates the
capture, then writes a private sibling directory and promotes it once without
overwriting a competing destination. A verified capture with no `SUPERSEDED`
titles succeeds by producing one empty directory. It performs no network
request or live capture.

Each `evidence-bundle.v2` is metadata-only and embeds the exact retained
response bytes, prescribed Federal Register attribution and CC BY 4.0 rights.
It keeps the independently observed compilation date and the raw registration
date distinct; neither is a claim that the legislation was amended or changed
in practical effect. This local producer operation creates neither a GitHub
release nor public development. Release attestation and publisher-side
admission are separate, manually authorised boundaries.

## Exporting publication evidence bundles

The publication adapter proves the next contract using checked-in synthetic
inputs:

```bash
python -m fadden export_publication_bundles -- tests/corpus/fixtures/publication/sample-sources.json tests/corpus/fixtures/publication/sample-observation-facts-v3.json --out build/publication-bundles
```

It accepts only a complete synthetic v3 observation. `UNCHANGED` titles emit
nothing; every changed title must be a supported `SUPERSEDED` compilation with
a later publication date, matching collection and complete content evidence.
Any blocked, unsupported, incomplete or inconsistent title fails the whole
export. The resulting `evidence-bundle.v1` contains source metadata and exact
input/content digests, but no source extract, practical implication or AI
explanation.

The output path must be absent. Its immediate parent may be an ordinary
directory or one absent, safe-named directory whose parent is ordinary; the
exporter creates that one parent only after all inputs validate. Both inputs
must be ordinary files outside the output. The command builds every bundle in a
private sibling directory and promotes the complete directory with one rename;
a failed write or promotion removes only exporter-owned staging and leaves
existing paths untouched.

The fixture's `content_sha256` identifies the checked-in artificial source
payload bytes. It is not a hash of the observation file. This exporter cannot
establish current professional coverage and emits only `mode: synthetic`; it
rejects the live observer's v4 output.

To produce a corpus you can pass on to someone else:

```bash
python -m fadden dist          # build the redistributable subset -> ./corpus/dist/
python -m fadden dist_verify   # check the built subset against its own claims, exits non-zero on failure
```

`fadden/corpus_paths.py`, `fadden/http_fetch.py` and `fadden/pii_patterns.py` are shared modules the
stages import; they are not run directly. `finalize.py` still copies the
discover-through-finalize loop plus `check_current.py` and the shared modules
into a completed corpus `build/` directory as flat scripts so a deployed
corpus keeps the historical `python discover.py` layout; the probe, PII, rates,
dist and export stages run from the source checkout.

The intermediate JSON files (`titles_all.json`, `acts_resolved.json`,
`manifest_raw.json`, `probe13.json`, `retry13_patch.json`) are build outputs
the pipeline regenerates; they are not committed or shipped. Each stage writes
its file beside the stage modules, so any single stage re-runs without repeating the
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
  re-read the previous response and handed one Act another Act's compilation.
  The JSON stages read the response off the socket now, so they carry no temp
  file to inherit; `download.py` still uses `curl`, to a per-title path.
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
