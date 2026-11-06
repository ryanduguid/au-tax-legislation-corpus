## What it produces

The 8 September 2026 extraction checks covered the original 946-file snapshot
and a refreshed 951-title inventory. The original files were retrieved on
3 and 4 August. Reusing them measures the parser changes against the same
snapshot; it does not establish September source currency. All copied EPUBs
and sidecars were checked against their original SHA-256 hashes.

| Measure | Published baseline | Original snapshot reparsed | Refreshed inventory |
|---|---:|---:|---:|
| Titles | 946 | 946 | 951 |
| Retrieval rows | 21,784 | 21,916 | 21,958 |
| Words | 6,041,512 | 6,377,327 | 6,387,381 |
| EPUB bytes | 37,495,335 | 37,495,335 | 37,717,652 |
| Rates-index entries | 4,149 | 4,159 | 4,161 |
| Titles in the rates index | 286 | 286 | 287 |
| Parse failures | 0 | 0 | 0 |

The reparse used the current `extract.py` and `rates.py` in a separate deployed
`build/` directory. It did not overwrite the original cache or replace the
historical `fadden/manifest_md.json` in this repository. The collection counts
remain 175 Acts, 675 legislative instruments and 96 notifiable instruments.

For the refreshed inventory, discovery and version resolution completed for
951 titles without lookup failures. Downloads reused 922 matching cached files
and retrieved 17 new or updated documents. Twelve current versions had no
published document; `probe13` and `retry13` recovered earlier documents and
marked them `version_is_current: false`. None remained missing after recovery.
The refreshed collection contains 175 Acts, 677 legislative instruments and
99 notifiable instruments. Its totals reflect source changes as well as parser
corrections, so they are not a measure of the parser changes alone.

In both runs, the first privacy scan found 169 flagged rows across 12 titles. The second scan
exited with failure: 2,043 phone-shaped matches and 4 TFN-shaped matches had
no allowlist entry. These are scanner matches, not confirmed identifiers. There
were no additional name-and-registration-number rows outside the 12 flagged
titles. Finalisation and redistribution verification were not run after that
failure; this is extraction evidence, not a verified distribution. No corpus
text or contact identifiers are included in this evidence update.

A fresh-directory run also exposed a circular dependency in the documented
pipeline: `pii_scan.py` read `sources.json`, which the later `finalize.py` stage
creates. The scan now reads the extraction manifest, so it can run before
finalisation. The regression test exercises that order without a `sources.json`.

Historical full run, 4 August 2026:

| | |
|---|---|
| Titles | 946: 175 Acts, 675 legislative instruments, 96 notifiable instruments |
| Retrieval rows | 21,784 |
| Words | 6,041,512 |
| EPUB downloaded | 37.5 MB |
| Rates index | 4,149 entries across 286 titles |
| Download failures | 0 |
| Parse failures | 0 |

That historical run predates the volume-gate fix in `extract.py` and the
table-stack fix after it, and so does the `manifest_md.json` shipped beside it.
The 8 September reparse above includes these corrections:

- the volume gate, which silently dropped 5 volumes of F2025L00281
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

See [BUILD.md](../BUILD.md).

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

## One-command preset build

One run of `python -m fadden quick` on 6 November 2026 took an empty folder to a `search_tax_legislation` hit in 116 seconds, 90 of them `download`'s 10-second Crawl-delay after each of the 9 Acts. It ran on a Windows 11 desktop PC (Intel Core i7-12700K, 64 GB of memory) whilst other builds were running. Network speed and the Register's load will change the figure, and one run is not a benchmark.

The folder held 9 titles, 9,439 rows, 3,678 definitions and 32,463 references, from 9.1 MB of EPUBs, in 82 MB. All 9 Acts had a published document for their current version, so `probe13` and `retry13` did not run. With `AUS_ACCOUNTING_CORPUS_ROOT` set to that folder, `search_tax_legislation` in the published `aus-accounting-mcp` 0.7.1 returned `C2004A05138:0035:8-1`, section 8-1 of the ITAA 1997, first for 'general deductions', 1.03 seconds after the script started, including the server's import.

## Retrieval baseline on real law

Measured on 5 November 2026 over `tests/fixtures/retrieval_real_law.tsv`: 46 queries
written before any run, 32 in base form and 14 with inflected word forms, each naming
the section it should reach. An agent wrote them from the Acts' published section
numbers; no person has graded them. Every expected section is in the index below, on
one row, and its Register heading was read against its question. A hit is that section
of that title in the first 1, 5 or 10 results.

| Ranker | Queries | Hit at 1 | Hit at 5 | Hit at 10 | Mean reciprocal rank | No results |
|---|---:|---:|---:|---:|---:|---:|
| `fadden search query`, base | 32 | 8 | 21 | 24 | 0.400 | 0 |
| `fadden search query`, inflected | 14 | 1 | 1 | 1 | 0.071 | 1 |
| MCP tier ranker, base | 32 | 11 | 17 | 22 | 0.422 | 0 |
| MCP tier ranker, inflected | 14 | 1 | 3 | 3 | 0.121 | 0 |

The MCP tier ranker is `search_sections` in `apps/aus-accounting-mcp/aus_accounting_mcp/corpus.py`
of australian-accounting at `6462edc`. Rerun the first 2 rows with
`FADDEN_REFERENCE_INDEX=<index> cargo test --locked --test fadden_search reference_questions_on_a_real_index -- --ignored --nocapture`.
These counts describe this question set over these 6 titles, not retrieval over the
whole corpus or legal correctness. The inflected rows show that neither ranker matches
another form of a word: the MCP ranker's substring match reaches some forms, and only
that way.

The index holds 6 titles, 8,096 rows and 28,438 references, built with `fadden search
build` from `sources.json` SHA-256 `43823bbd8b9ab8ab7128e2a4fcfd8b3b628ef4e1198550a8dbd54920422bfd29`:

| Register ID | Title | Compilation | Date | EPUB SHA-256 |
|---|---|---:|---|---|
| C2004A05138 | *Income Tax Assessment Act 1997* | 268 | 1 October 2026 | `05b7d33fbbfba411672c0c9341d282d14a3f9517bcfc2e6d347c349f47ea92ba` |
| C1936A00027 | *Income Tax Assessment Act 1936* | 192 | 1 July 2026 | `6b9686b4bdc77cd2c10816dcb086767d557b2a84fb3b80080ce2be3067fedb41` |
| C1953A00001 | *Taxation Administration Act 1953* | 227 | 1 October 2026 | `7d80b65a08cdc28f63fa81634e46ad4c0572d45693bc86c4ddb7d06e1aa5894c` |
| C2004A00446 | *A New Tax System (Goods and Services Tax) Act 1999* | 96 | 1 January 2026 | `a7e144d6139becf287ec49b86dbc9f7d07eb2de69a72b1e7ab3ad07deca52cb5` |
| C2004A03280 | *Fringe Benefits Tax Assessment Act 1986* | 97 | 1 July 2026 | `21ee9133acc6dbc48393dfb4ea334324d348c1b6f886d84e347c984dd899f57c` |
| F2025L01097 | *Income Tax Assessment (Effective Life of Depreciating Assets) Determination 2025* | 0 | 15 September 2025 | `aecb8c2f898f20a1556929c231564ebc18f53ebbc317395c5880fadab2c357ba` |


## Word-form search reference set

Measured on 7 November 2026 with schema 4 and the retained 9-title preset corpus:
9,439 rows, sources.json SHA-256 `74a42916984bec87b7c27a96d7566423a75c493b1fcdd10f992fdd1ffcdf1b1c`.
This is a different corpus from the 6-title baseline above; in particular the
ITAA 1936 compilation is 193, dated 1 October 2026. Compare exact and stemmed
results within this run, rather than attributing differences from that earlier
run to stemming. Both modes read the same original rows.

The fixture now has 100 distinct query texts: the original 32 base and 14
inflected questions, 44 additional heading-derived questions, 4 before-compilation
cases, 4 after-retrieval cases and 2 missing-source cases. The original 46 query,
target and class fields are unchanged. Additions were fixed before either mode
was scored. An agent wrote the questions from retained Register headings and
capture windows; no person has graded them and no result was used to tune them.
The fixture SHA-256 is `3ffa31e7da518ef24d34916b006a2b3b5529537ead91c0baf6f78a8031c9f6d0`.

| Mode and class | Retrievable queries | Hit at 1 | Hit at 5 | Hit at 10 | MRR at 10 | Retrieval failures | No results |
|---|---:|---:|---:|---:|---:|---:|---:|
| Exact, original base | 32 | 8 | 21 | 23 | 0.395 | 9 | 0 |
| Stemmed, original base | 32 | 6 | 15 | 23 | 0.303 | 9 | 0 |
| Exact, original inflected | 14 | 1 | 1 | 1 | 0.071 | 13 | 1 |
| Stemmed, original inflected | 14 | 2 | 7 | 9 | 0.262 | 5 | 0 |
| Exact, additional | 44 | 18 | 30 | 35 | 0.532 | 9 | 1 |
| Stemmed, additional | 44 | 18 | 31 | 34 | 0.526 | 10 | 0 |

A retrieval failure means that the expected section is held but absent from the
first 10 results; zero returned rows are a subset of those failures. All 90
undated target sections are held. The 2 missing-source cases name sections 4 and
6 of F2025L01097, whose metadata is retained in the earlier index but which is
absent from this 9-title corpus. They are reported separately and excluded from
the retrieval denominators. Neither mode returned text for any of the 8 dated
refusal cases, and every expected status was reported. A dated false match is
returned text for one of those unsupported windows, not an ungraded distractor
in an ordinary ranked result. Amendment and version cases test capture windows;
they do not establish historical amendment text or provision applicability.

Stemming raises inflected top-five hits from 1 to 7 of 14 here, but lowers the
original base-form top-five count from 21 to 15 of 32. It stays opt-in. Porter can
also conflate unrelated words, as the fabricated universe/university regression
shows. These are ranks on this question set, not general recall, legal
correctness or evidence that a quotation matches. Definitions and quotation
checks remain exact.

Reproduce the paired measurement by rebuilding these retained corpus inputs to
a new index and running the ignored `reference_questions_on_a_real_index` test
above with `FADDEN_REFERENCE_INDEX` naming it. The test prints each case, its
source digest and separate retrieval, missing-source and dated false-match
counts. Read the printed digest before comparing runs. Original-text files
underlying this measurement are pinned below; the table contains metadata, not
redistributed legislation.

| Register ID | Compilation | Compilation date | sections.jsonl SHA-256 |
|---|---:|---|---|
| C2004A00446 | 96 | 2026-01-01 | `0f1c71f075344d0b8d1418135822b6f7968cb92f500efad4d0c370bdcffdf085` |
| C2004A03280 | 97 | 2026-07-01 | `cf26fd7e4fd916b8622ce889abf7e2b2d88c2d10300286205cc5766fa028d9c3` |
| C2004A05128 | 104 | 2026-10-01 | `335abe56087580f7f7fa79ead1a1a60deafbe5fcb25806d723a84de175fb513e` |
| C1936A00027 | 193 | 2026-10-01 | `6561aaa9d938fe911280f9a0d116698b0e6f20970efeae2da1baad24edbf4cb3` |
| C2004A05138 | 268 | 2026-10-01 | `acd7506caa921fefac498904b4ccb0a2e4ab3511b587a3795edf46cfc8d27099` |
| C2004A03348 | 66 | 2026-07-01 | `98de6510d50fb62dfd5499d0e2c2b25b546259b3a92190a55279c20101c940f3` |
| C2004A04402 | 78 | 2026-07-01 | `d06b03b8d14604365a8f6f89a823b26df9849456ae9ffc5412ad1e605e69b44c` |
| C2004A04633 | 131 | 2026-08-10 | `fd91d6ab97c0990ee4646a22d99401bcbfc584ba4fda0b34e3a86ac494bc9efa` |
| C1953A00001 | 227 | 2026-10-01 | `3a69ca9cbc7943a8a6a08add19019326551cb26a6eb431b32d2a99b3deb79e6b` |

## How often a run of words identifies one row

`cite-check` calls a quotation `distinct` when no other row holds it and it has at least
10 words. Measured on 6 November 2026 over the index above (6 titles, 8,096 rows,
2,172,677 words after `normalised` folding): every run of each length, at every position
in every row, counted as in one row if no other row holds those words.

| Words | Runs | In one row only | Share |
|---:|---:|---:|---:|
| 3 | 2,156,485 | 591,720 | 27.44% |
| 4 | 2,148,390 | 943,943 | 43.94% |
| 5 | 2,140,295 | 1,205,742 | 56.34% |
| 6 | 2,132,201 | 1,386,995 | 65.05% |
| 8 | 2,116,029 | 1,586,805 | 74.99% |
| 9 | 2,107,945 | 1,642,628 | 77.93% |
| 10 | 2,099,864 | 1,682,371 | 80.12% |
| 12 | 2,083,730 | 1,734,997 | 83.26% |
| 16 | 2,051,616 | 1,788,740 | 87.19% |
| 20 | 2,019,835 | 1,810,667 | 89.64% |
| 30 | 1,942,508 | 1,809,023 | 93.13% |
| 40 | 1,868,763 | 1,774,463 | 94.95% |

The floor is 10 because that is the first length at which at least 4 in 5 runs were in one
row. That threshold is a judgement. The curve has no sharp step, and no length measured
reached 95%, because these Acts repeat long passages, so a floor cannot certify a
quotation by length alone. `other_rows` is the direct count for each quotation, and the
floor only stops a short passage being called `distinct` because this build lacks the
titles that repeat it.

This describes these 6 titles, not the corpus. Adding rows cannot make an existing run
more unique, but they add runs of their own, so the share on a wider build can rise or
fall. Rerun with
`FADDEN_REFERENCE_INDEX=<index> cargo test --locked --test fadden_search distinct_floor_on_a_real_index -- --ignored --nocapture`.
