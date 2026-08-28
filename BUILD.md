# Rebuilding this corpus

In a source checkout, run `python -m fadden <stage>` from the repository root.
Each stage resolves paths from its own module file under `fadden/`, so the
intermediate JSON files beside those modules are both the inputs and the
outputs of the pipeline. Derived corpus files are written below `./corpus/`.

The builder does not accept an environment-selected output root. To build at a
different location, copy the stage modules into that location's `build/`
directory as shown below; the parent of `build/` is the deterministic corpus
root. `finalize.py` still publishes that flat `build/` layout into a completed
corpus so operators can keep running `python discover.py` there.

```bash
# source checkout
python -m fadden discover      # -> fadden/titles_all.json, fadden/titles_principal.json
python -m fadden versions      # -> fadden/acts_resolved.json
python -m fadden download      # -> ./corpus/epub/*.epub, fadden/manifest_raw.json
python -m fadden probe13       # -> fadden/probe13.json      (only if download reports no_epub)
python -m fadden retry13       # -> fadden/retry13_patch.json; patches manifest_raw.json in place
python -m fadden extract       # -> ./corpus/markdown/**, fadden/manifest_md.json
python -m fadden pii_scan      # -> fadden/pii_flagged.json   (mandatory; finalize refuses without it)
python -m fadden pii_scan2     # -> refines fadden/pii_flagged.json
python -m fadden finalize      # -> ./corpus/sources.json, INDEX.md, README.md, LICENCE-NOTICE.md
python -m fadden rates         # -> ./corpus/rates/rates.jsonl, RATES.md
python -m fadden capture_register -- fadden/manifest_md.json --out build/register-capture-20260829

# deployed corpus (flat scripts copied into build/)
cd C:\ato-kb\build
python discover.py
```

## Exporting monitor inputs

After `finalize.py` has written a completed `sources.json`, a separately
collected fabricated or reviewed observation-facts document can be projected
by `export_monitor_contract.py` for `tax-radar-au` (formerly `tax-radar-au`) without
giving that project access to this build tree:

```bash
python -m fadden export_monitor_contract -- corpus/sources.json observation-facts.json --out monitor-input
```

The facts document may use `au-tax-register-observation-facts.v1`, v2 or v3. The command
writes the deterministic, exact monitor inputs `monitor-baseline.json` and
`register-observation.json` with duplicate-member, control-character and
resolved input/output-collision checks. Existing output names must be ordinary
files, never directories, links, junctions or other special paths. Exactly one writer
can publish to a given output directory at a time. Any existing lock
fails closed; if it has no recovery artefacts, an operator may remove
`.monitor-contract.publish.lock` after confirming its owner is no longer running.
If rollback itself fails,
the exporter retains that lock and every unrecovered `.bak` file, so no later publisher
proceeds. The operator must restore or deliberately retire the old/new pair and its
recovery artefacts before removing the lock. It never queries the Register:
`check_current.py` remains the read-only Register lookup stage, and the adapter
only validates and projects facts a caller has already collected.

The two names are replaced individually after staging and backup. An ordinary
promotion failure restores the prior pair, but a process or power loss between
the replacements can leave an old/new pair and lock or rollback evidence for
recovery. This is not a cross-file crash-atomic publication; that would require
versioned outputs plus an atomic generation pointer.

`complete: true` requires every title in `sources.json` exactly once. A partial
observation is allowed only with `complete: false`, so the monitor blocks rather
than inferring that unobserved titles are unchanged. The output is a review
queue input, not an authorised-text claim, legal conclusion or workflow change.

## Capturing live Register evidence

`capture_register.py` observes every title in a rich corpus manifest directly
against the Federal Register and preserves the exact bounded HTTP 200 response
bytes used for each decision:

```bash
python -m fadden capture_register -- fadden/manifest_md.json --out build/register-capture-20260829
```

The destination must not exist. Its immediate parent may be one absent,
safe-named directory whose parent already exists. The command validates the
manifest before filesystem creation, queries titles in Register-id order,
builds under a private sibling, re-reads every output and digest, then promotes
the four-part graph with one directory rename:

```text
register-capture-20260829/
  monitor-baseline.json
  register-capture.json
  register-observation.json
  evidence/sha256-<digest>.json
```

The current 946-title manifest requires at least 946 requests. The enforced
1.5-second gap makes a no-retry run about 24 minutes; an empty current-version
result adds one history request, and retryable failures add six-second waits.
Run incremental operational work outside the Register's preferred 08:00-20:00
Australian window. The API needs no key. The adapter refuses redirects, uses a
90-second socket timeout and makes at most three attempts.

The manifest's 47 legacy unnumbered compilations carry an explicit null
`compilationNumber`. Capture preserves the null and verifies it against the
same date and null in the current Register row; it never substitutes a display
dash or synthetic number into the evidence contract.

`VERIFIED` means every manifest identifier was attempted exactly once and no
title ended in `LOOKUP_FAILED`. A complete capture containing source or
consistency failures is still retained, with `run_status: BLOCKED`, so the
failure evidence is auditable. `CURRENT_NO_PUBLISHED_COMPILATION` and
`NO_LONGER_IN_FORCE` are valid observations but never publication candidates.

This is local source capture, not publication authority. Register responses
are authenticated in transit by HTTPS but are not individually government
signed. Do not commit raw captures. Stage 3B must transport the retained bytes
with authenticated producer provenance, prescribed Register attribution and
publisher-side revalidation before any live development can be admitted.

## Exporting publication evidence bundles

The publisher contract currently has one synthetic conformance path:

```bash
python -m fadden export_publication_bundles -- tests/corpus/fixtures/publication/sample-sources.json tests/corpus/fixtures/publication/sample-observation-facts-v3.json --out build/publication-bundles
```

V3 content evidence records the SHA-256 of the exact artificial payload used to
support each observation, plus its content kind and media type. It does not
reuse the observation-file digest as if that were source evidence. The export
requires complete scope; ignores `UNCHANGED`; and permits only a matching,
newer `SUPERSEDED` compilation. Every other changed, blocked or inconsistent
state aborts the complete run before output publication.

The destination itself must not exist. Its immediate parent may be an ordinary
directory or one absent, safe-named directory whose parent is ordinary; that
one parent is created only after input validation. Both inputs and every
existing parent must be ordinary filesystem objects, not links, junctions or
special files. The command captures both inputs once, builds all JSON files
under a private sibling and renames that complete directory into place. A
failure removes only that private staging directory. The bundle is metadata-only
and contains no source extract, impact assessment or explainer.

The publication adapter and its fixtures still accept only `mode: synthetic`
v3 input. The live capture emits v4 and is deliberately incompatible; passing
either contract test does not make a development ready for professional
publication.

In a source checkout the intermediates are regenerated, not shipped: the
.gitignore keeps titles_all.json, acts_resolved.json, manifest_raw.json and the
probe patches out of the tree, and only manifest_md.json and the title lists
travel with the repository. The deployed corpus at the build root carries the
full set that produced the current corpus, and there any single stage can be
re-run without repeating the earlier ones.

**`manifest_md.json` and the published corpus predate the volume-gate fix
below.** They were produced by the extractor as it stood before it, so
F2025L00281 is still recorded as `sections=20, words=18583` - the lossy parse,
not what `extract.py` now produces for it. Re-running `extract.py` changes that
entry, and `finalize.py` reads the manifest, so the corpus README's title, row
and word totals move with it. In the deployed corpus, re-running `finalize.py` on its own reproduces
today's published figures, because it is fed the pre-fix manifest. Those
intermediates are still re-runnable; they are simply not a description of what
this code does now.

The same is true of the table stack added after that run. A table nested inside
a cell used to discard the rows its enclosing table had already parsed. Four
titles carry nested tables (F2005B01198, F2005L00211, F2005L01901, F2026L00716)
and between them recovered 84 text fragments; the other 942 parse identically,
so re-running `extract.py` moves those four titles' figures as well.

Two more changes move the figures further. Text ahead of a document's first
section had no row to hold it, so it reached the markdown and never
`sections.jsonl`; opening a row for it adds one `Introductory material` row to
511 of the 946 titles, and no title loses retrievable text. A table or figure
between two pre-body paragraphs was dropped while the prose either side was
kept; admitting it changes the markdown for one title.

## Timing

`download.py` sleeps 10 seconds between titles, honouring the Crawl-delay in
https://www.legislation.gov.au/robots.txt. All 946 titles take about 2 hours
40 minutes. It skips any EPUB already present whose sidecar `.meta.json`
records the same version, so re-runs cost only the new titles.

`discover.py` and `versions.py` hit the API roughly 1,100 times at 1.5 to 3
second intervals, about 40 minutes combined. `extract.py`, `finalize.py` and
`rates.py` are local: about 3 minutes for all three over 946 titles.

`check_current.py` is 946 API calls at 1.5 seconds, so allow half an hour. It
prints a progress line every 25 titles.

## Traps this code exists to avoid

- **`curl` does not truncate `-o` on transport failure.** A shared temp file
  silently re-reads the previous response. On the paging path that dropped 142
  titles; on version lookup it gave one Act another Act's compilation. Every
  `fetch_json` reads each response off the socket, so no attempt can
  inherit a file from the one before it.
- **Unordered `$skip`/`$top` paging drops rows.** `$orderby=id` is mandatory,
  and `discover.py` raises if paged ids are not unique.
- **`$top` is capped at 100.** Above that the API returns 400.
- **Any filter containing `isPrincipal` returns 400.** It is applied in Python.
- **The download endpoint answers in two shapes**: raw EPUB bytes, or a JSON
  envelope with the file base64 in a `bytes` field. Sniff the first byte.
- **An HTTP or invalid-content response is not evidence that an EPUB is
  absent.** The download stage stops on transport errors, HTTP errors and
  non-EPUB bodies instead of recording them as `no_epub`. Responses are staged
  in `.part`; only a validated archive replaces the `.epub`. The failure log
  records bounded status and content-type metadata, never the response body.
  Both `download.py` and `retry13.py` restore every changed EPUB and sidecar to
  the graph referenced by the prior manifest; retry audit, sidecar and manifest
  writes are staged and atomically replaced. A process or power loss while that
  run-level journal is open can still leave changed files and `.rollback`
  evidence beside the prior manifest. Fully crash-atomic cross-file publication
  would require versioned filenames plus a separately committed pointer.
- **`/latest/` is not a download alias.** It returns the SPA shell as
  `text/html`. Resolve the version date first.
- **The current version can have no document.** `versions?$filter=isCurrent eq
  true` happily returns a version whose `registerId` is null: the Register knows
  an amendment commenced but has not published the compilation. Every document
  path built from that date answers 404, which reads as a broken download rather
  than a missing document. Titles can enter or leave this state as compilations
  are published. `retry13.py` falls back to the
  most recent version that does have a `registerId` and marks the result
  `version_is_current: false`, in the front matter, on every JSONL row, and in
  `sources.json`. Substituting an older compilation silently would misreport the
  corpus as current. `check_current.py` reports these in their own bucket: it
  used to file them under "SUPERSEDED, re-download these", which sends you at a
  URL that answers 404. An explicit null `registerId` from this API response is
  the only condition the download stage records as `no_epub`.
- **Acts use two EPUB templates.** Modern Acts use `ActHead1`-`ActHead5`; Acts
  predating it carry no structural headings and mark sections only with a
  `CharSectno` span. Some use `<h1>`-`<h6>` instead of `<p>`.
- **Instruments use dozens.** Three worth mapping by name, because together they
  cover most of the volume:
  - `LI-Heading1`/`LI-Heading2`: the standard modern instrument template.
    Heading1 is the Part, Heading2 the section. `LI-Heading3` and below are
    sub-headings *inside* a section and are deliberately left unmapped, or they
    split the section they belong to.
  - `IASBSectionTitle1`-`4`: accounting standards registered as instruments
    (AASB 112 Income Taxes). Unnumbered headings. Mapping them turned one
    29,000-word blob into 62 rows.
  - No classes at all, headings as bare paragraphs (`3 Definition`).
- **Do not decide the template from the markup.** Two attempts failed: cosmetic
  Word classes (`Header`, `ListParagraph`, `Normal`) look structural, and the
  one-off families run past 25. `extract.py` runs the structural pass, and only
  when that yields no section at all re-runs with `force_bare=True`. A document
  that parsed keeps its result untouched, so this cannot corrupt an Act.
  Verified across all 175, which come out row-for-row identical.
- **Accept the fallback only if it found headings.** A single "Introductory
  material" row holding the whole document is the same blob under a worse label;
  let the `whole_act` path own that case and say so.
- **Let the document supply its own heading vocabulary.** APRA prudential
  standards head their sections with classless, unnumbered paragraphs
  ("Authority", "Application") that no rule can pick out of body text, but they
  list every one in their own table of contents. The fallback collects the
  `TOC\d` entries and treats a later exact match as a heading. `SKIP_CLASS`
  already drops the contents page, so the only hits are the real headings.
  Took CPS 226 from one blob to 30 rows holding 98% of its text.
- **An unstyled heading is still a heading.** Most APRA determinations carry no
  contents page either. They mark headings by omission: the heading is unstyled
  and the body under it is styled. That asymmetry is the whole signal, and it
  rejects the signature block for free: "Clare Gibney" is followed by
  "Executive Director", unstyled as well, while "Authority" is followed by
  `BodyText1`. Guard on length (<= 80) and no trailing `.;,:` so the making
  words do not qualify. Do NOT test the following block against
  `COSMETIC_CLASS`: it contains `BodyText`, which prefix-matches `BodyText1`,
  the very class that proves the paragraph is styled body. That one mistake left
  the rule finding 2 headings where it should have found 17.
- **Bare heading detection, three ways it goes wrong.** Numbering is `1` or
  `1.`; headings run past 100 characters and end on a digit (`... for the
  purposes of Division 75`) or `?`, so neither length nor closing character
  separates heading from body. What works: no period or semicolon anywhere
  inside and no trailing comma. And `1 October 2016` on its own line parses as
  section 1 under any rule loose enough to accept the real headings, so month
  names are excluded explicitly.
- **Some documents are tables, not sections.** The Tax Practitioners Board
  publishes its terminations as three paragraphs and twenty-four tables, with no
  heading anywhere for either pass to find, so 15,000 words landed in one row.
  The table is the document's own unit. Two things make the split safe. Count
  word mass, not lines, when deciding the document is table-shaped: those
  instruments repeat their lead-in sentence above every table, so 36 prose lines
  against 32 tables reads as narrative when it is the same sentence 32 times.
  And keep the prose in document order rather than hoisting the first line as a
  shared lead. Excise By-law No. 127 prescribes petroleum fields one table per
  basin, and hoisting stranded "C. PERTH BASIN" and dropped the operative
  paragraphs entirely. `table_split` asserts afterwards that every segmented
  line appears in some chunk and raises if one does not; that check is what
  caught it, and it now runs on every build. It covers the table-split path
  only. `to_markdown` has no equivalent whole-document assertion, which is how
  a heading-less EPUB volume was able to lose its whole body silently. See the
  volume gate below.
- **The pre-body gate is per volume, not per document.** A multi-volume EPUB
  repeats its compilation cover page at the head of every volume, so `seen_body`
  resets at each volume boundary, but the decision that the document *has*
  structural headings was taken once, across all volumes. A volume that carries
  none (F2025L00281 keeps Schedule 1 behind `ScheduleHeading`/`P1` markup) then
  never opened its gate, and its tables, images and paragraphs were dropped with
  no placeholder, no counter and no parse failure: 92% of that instrument, and
  `manifest_md.json` recorded `sections=20, words=18583` with no error field.
  Three things hold it shut now. Decide the gate from the volume's own blocks.
  Open it, for a heading-less volume, at that volume's contents page, or at the
  first paragraph carrying a body class if one comes first, never at the
  volume's own first block, which is the compilation cover page that every
  markdown file and every JSONL row states was omitted. `Header` and `Footer`
  are skipped classes but not boundaries: Word repeats the running header above
  the cover page. A volume showing neither boundary keeps the gate shut and is
  dropped as it was before, which is the older documented loss in preference to
  publishing a cover page under a notice saying it was removed; all 11
  heading-less volumes across the 946 EPUBs open at a contents page. And leave
  the bare-text endnote trigger armed only once a mapped heading has been seen.
  The cover page lists "Endnotes" as one of the volumes, so arming it at the
  top of a heading-less volume routes that volume's whole body into
  `endnotes.md`, which is the same loss wearing a different hat. Recovered text
  also opens a row of its own, or it is retrieved under the section number of
  whatever was still open when the previous volume ended.
- **Numbering gaps in ActHead instruments are genuine**, not parser misses.
  Compilation removes repealed sections, so 21-29 simply do not appear.
- **`contains(name,...)` matches more than the current name.** It found the
  Passenger Movement Charge Act 1978 under its former title, the Departure Tax
  Act 1978. It also matches substrings, which is how two dozen `AD/ROTAX/...`
  airworthiness directives arrived: "Rotax" contains "tax". Do not filter these
  out on a name regex, because any rule strict enough to drop Rotax also drops the
  Departure Tax Act. `sources.json` records `keywords_in_name` instead.
- **Section numbers render as `40 <U+2011> 1`**, a non-breaking hyphen padded
  with non-breaking spaces. A regex expecting a plain adjacent hyphen matches
  nothing.
- **Table cells wrap their content in `<p>`.** A parser that flushes its buffer
  on `<p>` open reads every cell as empty.
- **Never filter images by byte size.** Doing so deleted a GST decision
  flowchart and a maintenance-income formula. Gate on pixel dimensions and
  always emit a placeholder when discarding.
