# Compared with the Open Australian Legal Corpus

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
