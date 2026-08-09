# Commonwealth tax legislation: a corpus builder

Nine Python scripts that download every in-force principal Commonwealth tax
title from the Federal Register of Legislation and turn it into a retrieval
corpus: full-text markdown with provenance front matter, one JSONL row per
section, and a derived rates-and-thresholds index.

No dependencies beyond the standard library and `curl`. No API key — the
Register's API is free and unauthenticated.

**This repository holds the code, not the corpus.** The output is roughly a
gigabyte and includes material that should not be redistributed casually; see
[What this deliberately does not ship](#what-this-deliberately-does-not-ship).
Run the pipeline and you get your own copy from the primary source, which is
the point.

## What it produces

Last full run, 4 August 2026:

| | |
|---|---|
| Titles | 946 — 175 Acts, 675 legislative instruments, 96 notifiable instruments |
| Retrieval rows | 21,784 |
| Words | 6,041,512 |
| EPUB downloaded | 37.5 MB |
| Rates index | 4,149 entries across 286 titles |
| Download failures | 0 |
| Parse failures | 0 |

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

## Running it

The builder deliberately does **not** read `ATO_KB_ROOT` or `ATO_DIST`.
Environment-selected output roots made it possible for a poisoned process
environment to redirect downloads, deletes, or distribution output. In a source
checkout, generated corpus files live in the deterministic `./corpus/`
directory. To build at another location, place the scripts in that location's
`build/` directory; they then use the parent directory as the corpus root.

```bash
python discover.py      # -> titles_all.json, titles_principal.json
python versions.py      # -> acts_resolved.json
python download.py      # -> ./corpus/epub/*.epub, manifest_raw.json
python probe13.py       # only if download reports no_epub
python retry13.py       # -> retry13_patch.json; patches manifest_raw.json in place
python extract.py       # -> ./corpus/markdown/**, manifest_md.json
python finalize.py      # -> ./corpus/sources.json, INDEX.md, README.md, LICENCE-NOTICE.md
python rates.py         # -> ./corpus/rates/rates.jsonl, RATES.md
python check_current.py # read-only staleness check against the Register
```

To produce a corpus you can pass on to someone else:

```bash
python pii_scan.py      # -> pii_flagged.json, the titles that name people
python pii_scan2.py     # second pass at a lower threshold, plus contact details
python dist.py          # -> ./corpus/dist/, the redistributable subset
python dist_verify.py   # checks the built subset, exits non-zero if any check fails
```

The intermediate JSON files from the 4 August run are committed, so any single
stage re-runs without repeating the earlier ones.

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
- **The current version can have no document.** The Register records that an
  amendment commenced before publishing the compilation, so the URL 404s. That
  is not a broken download, and 13 titles are in that state.
- **Acts use two Word templates; instruments use dozens.** Deciding the template
  from the markup fails, because cosmetic classes look structural. Run the
  structural pass, and only when it finds nothing at all re-run with the
  bare-paragraph fallback — verified not to change a single Act row.
- **Never filter images by byte size.** Doing so deleted a GST decision
  flowchart and a maintenance-income formula.

## Accuracy and limits

Not the authorised text. Authorised versions are PDF only, stamped under
sections 15ZA and 15ZB of the *Legislation Act 2003*. Everything here derives
from the EPUB reading view.

Check the `collection` field before relying on a provision. An Act and a
regulation made under it are not interchangeable, and an instrument can be
disallowed or sunset while its enabling Act stands.

Selection is by title keyword — Tax, Excise, Superannuation, Customs Tariff,
Medicare Levy — applied to the Act, LegislativeInstrument and
NotifiableInstrument collections. A tax-relevant title without one of those
words in its name is absent. The Register's `contains(name,...)` also matches
former titles and substrings, which is how the Passenger Movement Charge Act
1978 arrived under its old name, the Departure Tax Act 1978, and how two dozen
`AD/ROTAX/...` airworthiness directives arrived because "Rotax" contains "tax".
`sources.json` records `keywords_in_name` rather than filtering them out, since
any rule strict enough to drop Rotax also drops the Departure Tax Act.

Thirteen titles carry a superseded compilation, marked `version_is_current:
false` in the front matter and on every row.

## What this deliberately does not ship

The corpus itself. It is about a gigabyte and this code rebuilds it in an
afternoon, so shipping it would trade a lot of storage for very little. Two
parts of it should not be redistributed at all, and `dist.py` exists to separate
them out:

1. **The EPUBs embed the Commonwealth Coat of Arms**, which is excluded from the
   Register's CC BY 4.0 grant, along with any third-party material the Register
   has not cleared. Stripping the image would also destroy the only thing that
   makes shipping EPUBs worthwhile, which is that they are byte-identical to
   what the Register served. The markdown and JSONL carry no image data at all —
   `extract.py` never emits any — so they need nothing removed.
2. **Twelve titles name private individuals.** The Tax Practitioners Board
   registers terminations and suspensions of tax and BAS agents as notifiable
   instruments: tables of named people with registration numbers and the
   provision breached, about 5,400 name mentions across 169 rows. Public on the
   Register as PDFs you read one at a time; shipping them as dataset rows makes
   them name-searchable at scale, which is a different act.

`pii_scan.py` finds the second category without being told where to look. It
tests every row in all 946 titles for personal names appearing alongside agent
registration numbers, because a bare name test flags the whole corpus —
legislation names Ministers, Commissioners and litigants constantly, and the
registration number is what separates a disciplinary register from a statute.
`pii_scan2.py` re-runs at a lower threshold and sweeps for emails, phone numbers
and tax file numbers; it found nothing outside those twelve, and the only
contact details anywhere are five organisational addresses.

`dist.py` then writes `dist/`: 934 titles, 21,596 rows, 5,722,156 words, 106 MB,
with a `REMOVED.md` listing every exclusion and its Register link so the
omission is visible and reversible. `dist_verify.py` checks the result against
its own claims and exits non-zero if any check fails.

## Licence

The code is MIT — see [LICENSE](LICENSE).

Everything it downloads is Commonwealth legislation from the Federal Register of
Legislation, licensed
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), excluding the
Commonwealth Coat of Arms and any third-party material. `finalize.py` writes a
`LICENCE-NOTICE.md` into the corpus with the required attribution wording, which
differs for the unchanged EPUBs ("Sourced from") and the converted markdown
("Based on content from").

## Related

ATO rulings and guidance are not legislation and are not on the Register. The
`simplelex` datasets on HuggingFace cover them — see
[ATO-Australian-Tax-Rulings-and-Guidance](https://huggingface.co/datasets/simplelex/ATO-Australian-Tax-Rulings-and-Guidance).
Validated against the live ATO Legal Database on 4 August 2026: the document
text is close to verbatim, but `Date_of_Issue` is empty on all 5,677 public
rulings, `Title` holds the document type rather than the title, `Is_Repealed`
flags instruments that are in force, and its legislative instruments are as-made
text rather than current compilations. Good for retrieval over document text,
not for determining currency.
