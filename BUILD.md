# Rebuilding this corpus

Run from inside this `build/` directory. Each script resolves its working
directory to its own folder, so the intermediate JSON files beside it are both
the inputs and the outputs of the pipeline.

For a direct source checkout, run the same commands from the checkout root;
derived corpus files are then written below `./corpus/`. The builder does not
accept an environment-selected output root. To build at a different location,
copy the builder scripts into that location's `build/` directory as shown
below; the parent of `build/` is the deterministic corpus root.

```bash
cd C:\ato-kb\build
python discover.py      # -> titles_all.json, titles_principal.json
python versions.py      # -> acts_resolved.json
python download.py      # -> ../epub/*.epub, manifest_raw.json
python probe13.py       # -> probe13.json      (only if download reports no_epub)
python retry13.py       # -> retry13_patch.json, then patch manifest_raw.json
python extract.py       # -> ../markdown/**, manifest_md.json
python finalize.py      # -> ../sources.json, ../INDEX.md, ../README.md, ../LICENCE-NOTICE.md
python rates.py         # -> ../rates/rates.jsonl, ../rates/RATES.md
```

The intermediate files shipped here are the ones that produced the current
corpus, so any single stage can be re-run without repeating the earlier ones.

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
  `curl_json` deletes its temp file per attempt and checks the return code.
- **Unordered `$skip`/`$top` paging drops rows.** `$orderby=id` is mandatory,
  and `discover.py` raises if paged ids are not unique.
- **`$top` is capped at 100.** Above that the API returns 400.
- **Any filter containing `isPrincipal` returns 400.** It is applied in Python.
- **The download endpoint answers in two shapes**: raw EPUB bytes, or a JSON
  envelope with the file base64 in a `bytes` field. Sniff the first byte.
- **`/latest/` is not a download alias.** It returns the SPA shell as
  `text/html`. Resolve the version date first.
- **The current version can have no document.** `versions?$filter=isCurrent eq
  true` happily returns a version whose `registerId` is null: the Register knows
  an amendment commenced but has not published the compilation. Every document
  path built from that date answers 404, which reads as a broken download rather
  than a missing document. Thirteen titles were in this state, the Family Law
  (Superannuation) Regulations 2025 among them. `retry13.py` falls back to the
  most recent version that does have a `registerId` and marks the result
  `version_is_current: false` — in the front matter, on every JSONL row, and in
  `sources.json`. Substituting an older compilation silently would misreport the
  corpus as current. `check_current.py` reports these in their own bucket: it
  used to file them under "SUPERSEDED, re-download these", which sends you at a
  URL that answers 404. The tell is a current version with a null `registerId`.
- **Acts use two EPUB templates.** Modern Acts use `ActHead1`-`ActHead5`; Acts
  predating it carry no structural headings and mark sections only with a
  `CharSectno` span. Some use `<h1>`-`<h6>` instead of `<p>`.
- **Instruments use dozens.** Three worth mapping by name, because together they
  cover most of the volume:
  - `LI-Heading1`/`LI-Heading2` — the standard modern instrument template.
    Heading1 is the Part, Heading2 the section. `LI-Heading3` and below are
    sub-headings *inside* a section and are deliberately left unmapped, or they
    split the section they belong to.
  - `IASBSectionTitle1`-`4` — accounting standards registered as instruments
    (AASB 112 Income Taxes). Unnumbered headings. Mapping them turned one
    29,000-word blob into 62 rows.
  - No classes at all, headings as bare paragraphs (`3 Definition`).
- **Do not decide the template from the markup.** Two attempts failed: cosmetic
  Word classes (`Header`, `ListParagraph`, `Normal`) look structural, and the
  one-off families run past 25. `extract.py` runs the structural pass, and only
  when that yields no section at all re-runs with `force_bare=True`. A document
  that parsed keeps its result untouched, so this cannot corrupt an Act —
  verified across all 175, which come out row-for-row identical.
- **Accept the fallback only if it found headings.** A single "Introductory
  material" row holding the whole document is the same blob under a worse label;
  let the `whole_act` path own that case and say so.
- **Let the document supply its own heading vocabulary.** APRA prudential
  standards head their sections with classless, unnumbered paragraphs
  ("Authority", "Application") that no rule can pick out of body text — but they
  list every one in their own table of contents. The fallback collects the
  `TOC\d` entries and treats a later exact match as a heading. `SKIP_CLASS`
  already drops the contents page, so the only hits are the real headings.
  Took CPS 226 from one blob to 30 rows holding 98% of its text.
- **An unstyled heading is still a heading.** Most APRA determinations carry no
  contents page either. They mark headings by omission: the heading is unstyled
  and the body under it is styled. That asymmetry is the whole signal, and it
  rejects the signature block for free — "Clare Gibney" is followed by
  "Executive Director", unstyled as well, while "Authority" is followed by
  `BodyText1`. Guard on length (<= 80) and no trailing `.;,:` so the making
  words do not qualify. Do NOT test the following block against
  `COSMETIC_CLASS`: it contains `BodyText`, which prefix-matches `BodyText1` —
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
  shared lead — Excise By-law No. 127 prescribes petroleum fields one table per
  basin, and hoisting stranded "C. PERTH BASIN" and dropped the operative
  paragraphs entirely. Assert afterwards that every body line appears in some
  chunk; that check is what caught it.
- **Numbering gaps in ActHead instruments are genuine**, not parser misses.
  Compilation removes repealed sections, so 21-29 simply do not appear.
- **`contains(name,...)` matches more than the current name.** It found the
  Passenger Movement Charge Act 1978 under its former title, the Departure Tax
  Act 1978. It also matches substrings, which is how two dozen `AD/ROTAX/...`
  airworthiness directives arrived: "Rotax" contains "tax". Do not filter these
  out on a name regex — any rule strict enough to drop Rotax also drops the
  Departure Tax Act. `sources.json` records `keywords_in_name` instead.
- **Section numbers render as `40 <U+2011> 1`**, a non-breaking hyphen padded
  with non-breaking spaces. A regex expecting a plain adjacent hyphen matches
  nothing.
- **Table cells wrap their content in `<p>`.** A parser that flushes its buffer
  on `<p>` open reads every cell as empty.
- **Never filter images by byte size.** Doing so deleted a GST decision
  flowchart and a maintenance-income formula. Gate on pixel dimensions and
  always emit a placeholder when discarding.
