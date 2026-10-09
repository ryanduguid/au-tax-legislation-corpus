# Corpus and research tool comparisons

The [Open Australian Legal Corpus](https://huggingface.co/datasets/isaacus/open-australian-legal-corpus)
(Isaacus, CC BY 4.0) is the best-known open dataset of Australian legislation.
Use it when you need a ready-made, multi-jurisdiction dataset of whole
documents. Use this builder when you need Commonwealth tax law cut into
sections, with a compilation and in-force status on every row, and a way to
notice when the law changes.

Facts about the Open Australian Legal Corpus below are from its dataset card,
version 7.1.0, read on 24 September 2026.

| | Open Australian Legal Corpus | This builder |
| --- | --- | --- |
| What you get | A published dataset, loadable directly | Code; you build the corpus yourself (`python -m fadden`) |
| Coverage | 9 sources covering the Commonwealth, 5 states and Norfolk Island, including court decisions; 4,818 Commonwealth Acts and 27,538 Commonwealth instruments from the Federal Register | Commonwealth Acts and instruments whose titles match the tax keywords in [scope](scope.md) (946 titles in the dated build), plus named ATO rulings through the `rulings` stage |
| Unit | One record per document, with its whole text | Legislation: one row per section, with `row_id`, `section`, `heading` and `container`; `granularity` is `table_block` or `whole_act` where a title cannot be split into sections. Rulings: one row per numbered paragraph, with its own fields ([rulings](rulings.md)) |
| Version | Latest known version of each document; the card gives the collection date as 10 March 2025 | Legislation: the in-force compilation at build time, or the last one the Register published where the in-force version has none (`version_is_current: false`); `compilation_number`, `compilation_date` and `version_is_current` are on every row. Rulings: `fetched_on` only; no compilation status |
| Change tracking | New dataset versions | `check_current` compares the build with the Register; the `tax-radar-au` queue raises changes for review |
| Personal data | Not compared; see its card | 12 titles naming private individuals are excluded from distribution; organisational contacts need a reviewed allowlist entry ([scope](scope.md#what-this-deliberately-does-not-ship)) |
| Provenance | `url`, `when_scraped` and `version_id` per document | Legislation: Register page and source URL per row, a `sources.json` inventory, and live-capture evidence bundles with content digests ([evidence export](evidence-export.md)). Rulings: `source_url`, `source_sha256` and `licence_basis` per row |

Neither is the authorised text of the law. Check the Register's authorised PDF
before relying on a provision.

## Why there is no export in its format

Its records hold one document's whole text. Converting legislation rows into
that shape would drop the section, compilation and in-force fields that are the
reason to use this builder, and it would open a second distribution route that
also has to carry the personal-data exclusions above. If you need both, load
them separately.

## Related Australian legal-data projects

These projects overlap part of the same ground. The facts below are from each
project's README and GitHub metadata, read on 29 September 2026; none of them
was installed or run for this comparison.

The AsAt row was checked against its source at
[`34bfd63`](https://github.com/Aldiharley/asat/tree/34bfd63a220c3ba55ba82d27188870fb8c586543)
on 5 October 2026. Its legislation fixtures use the parent Act's commencement
for `coverage_from` because they lack provision commencement dates; the separate
application interval is not modelled. Retrieval for an income year therefore
does not establish that a provision's text applied in that year.

| Project | What it is | Where it differs from this builder |
| --- | --- | --- |
| [gunba/ato-mcp](https://github.com/gunba/ato-mcp) (MIT, v0.16.2) | A local MCP server over a pre-built download of about 158,000 ATO legal database documents (about 467,000 chunks), with hybrid lexical and vector search, statutory definition lookup and live fetch for documents it does not carry | It serves the ATO's own publication of legislation, cases, rulings and guidance as a ready-made search index. This builder produces Register compilations cut into sections with compilation status, and it tracks change. Use it for ATO guidance retrieval; use this builder when a row must name its compilation |
| [Aldiharley/asat](https://github.com/Aldiharley/asat) (AGPL-3.0) | Australian tax-law retrieval with temporal filters and quotation checks against retrieved source text. Its legislation coverage can extend back to the parent Act's commencement, which may include years before the quoted provision applied | It answers questions; this builder produces material. Its fixtures do not establish provision commencement or application dates. This builder's [`as_at` stage](scope.md#dates-and-past-income-years) reports compilation windows and their limits. Neither establishes historical provision applicability. `quote_in_text` follows AsAt's quotation-checking approach with NFKC normalisation; a matching quotation alone establishes neither the correct authority nor its relevance |
| [matematicsolutions/au-eli-mcp](https://github.com/matematicsolutions/au-eli-mcp) (Apache-2.0, v0.3.3) | An MCP server that searches Commonwealth Acts on the Register by title and fetches current consolidated text, with a citation and a coverage statement on every response | Acts only and fetched live, so there is no local corpus, no instruments, no sections and no change review. Its `au_coverage` tool, which declares what it does not cover, is a good pattern for any consumer of this corpus |
| [cchew/lex-au](https://github.com/cchew/lex-au) (MIT, v0.10.2) and [cchew/lex-au-graph](https://github.com/cchew/lex-au-graph) (MIT, v0.13.1) | lex-au converts Register DOCX into Akoma Ntoso XML with section-level identifiers and publishes 3,076 Acts and 2 regulations as a CC BY 4.0 dataset; lex-au-graph builds a cross-reference graph over it for definition resolution | Every Commonwealth Act rather than a tax selection, in a legal XML standard, and the graph follows defined terms across Acts, which flat section rows cannot do. It does not carry this builder's tax instruments, ATO rulings or change-review queue |
| [Danielkgr/legislation-monitor](https://github.com/Danielkgr/legislation-monitor) (MIT) | A local web app that stores Commonwealth and Victorian Acts, re-fetches them on request and shows a side-by-side diff when the text changes, mapped to the Parts and sections it touches | It adds Victorian legislation. It works on text hashes, where `check_current` and `tax-radar-au` work on Register compilation identity and keep review evidence; `compare_builds` lists the changed sections between two builds of this corpus |
| [russellbrenner/jurisd](https://github.com/russellbrenner/jurisd) (Apache-2.0, 0.5.0 at [`a8d86f0`](https://github.com/russellbrenner/jurisd/tree/a8d86f0)) | A local-first AU and NZ legal research workbench and MCP server: offline legislation and decision modules with a citation graph, live AustLII retrieval and AGLC4 citation formatting | It answers research questions across every jurisdiction, including case law. This builder produces a provenance-bound Commonwealth tax corpus and holds no decisions |

## Commercial research tools

The vendor pages below were read on 9 October 2026. They describe advertised
capabilities; neither product was used in a trial or tested against this corpus.

| Product and primary source | Advertised capability | Comparison with this builder |
| --- | --- | --- |
| [CCH iKnowConnect](https://www.wolterskluwer.com/en-au/solutions/cch-iknowconnect) | Research across legislation, cases, editorial commentary and practical tools; source-linked AI answers and daily or weekly news alerts | It provides a research service with editorial material. This builder supplies Commonwealth tax source rows and a change-review queue. Its provenance fields support source review, but do not supply CCH's commentary or establish the practical effect of a change. |
| [Lawg](https://lawg.ai/) | Structured tax answers with references to legislation, ATO rulings and case law, including passage-level case citations | It answers research questions across sources. This builder holds no case law and generates no answer. A consumer can use its section and paragraph rows to check quotations, subject to the recorded coverage and date limits. |

### Cases for evaluating a research consumer

Run these cases with fabricated questions and retained public source text.
Record the source identifier, compilation or retrieval date, exact passage,
question date and coverage limitation beside each result. These are proposed
evaluation cases; no commercial product has passed them in this comparison.

1. A quotation is absent from the cited row. The consumer must reject the
   quotation even if the source title and citation look plausible.
2. A quotation matches a row, but the question concerns a date outside its
   compilation window. Preserve the quotation result and the date limitation
   separately. `fadden.as_at.quote_in_text` and `as_at` already provide these
   distinct checks; neither establishes historical provision applicability.
3. The question needs a court decision, state legislation or editorial
   commentary. Declare that material outside this builder's coverage instead
   of presenting a Commonwealth section as a complete answer.
4. An ATO ruling paragraph is cited. Show its source URL and retrieval evidence;
   do not assign it legislation compilation status.
5. A change appears in `compare_builds` or the radar queue. Link the changed
   source and retain human review of its practical effect before updating
   downstream advice or workflows.
