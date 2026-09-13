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
