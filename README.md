# au-tax-legislation-corpus: trace a change to its source

[![tests](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/ci.yml/badge.svg)](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/ci.yml)
[![CodeQL](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/codeql.yml/badge.svg)](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/codeql.yml)
[![release](https://img.shields.io/github/v/release/ryanduguid/au-tax-legislation-corpus?color=5C2D91&labelColor=04001F)](https://github.com/ryanduguid/au-tax-legislation-corpus/releases/latest)
[![licence: MIT](https://img.shields.io/badge/licence-MIT-5C2D91.svg?labelColor=04001F)](LICENSE)
![python: 3.10+](https://img.shields.io/badge/python-3.10%2B-5C2D91.svg?labelColor=04001F)

Synthetic example. This is a finding aid derived from the Register's EPUB reading view, not authorised legislation, tax advice or a conclusion about legal effect.

**Input:** the fabricated [source index](tests/corpus/fixtures/publication/sample-sources.json) and [reviewed observation facts](tests/corpus/fixtures/publication/sample-observation-facts-v3.json).

From a source clone, install: `python -m pip install -e .`. That puts both `python -m fadden` and the `tax-radar-au` command on the path. The builder still reads and writes beside its own stage modules, so run its stages from the checkout.

```bash
python -m fadden export_monitor_contract -- tests/corpus/fixtures/publication/sample-sources.json tests/corpus/fixtures/publication/sample-observation-facts-v3.json --out ../synthetic-monitor-example
```

**Output:** `monitor-baseline.json` and `register-observation.json` in the named output directory. The synthetic title `C2099A00001` is recorded as `SUPERSEDED`; the pair carries the evidence and source identity for review.

The example can be read without running the command:

| Review field | Fabricated evidence |
| --- | --- |
| Source identity | Sample Consumption Tax Act 2099, `C2099A00001`, collection `Act` |
| Indexed version | Compilation 1, dated 1 July 2026, retrieved 1 August 2026 |
| Detected change | The supplied observation records `SUPERSEDED` and compilation 2, dated 5 August 2026 |
| Observation evidence | Checked 8 August 2026; evidence identifier `ev-c2099a00001-01`, a content digest and an `example.invalid` URL |
| Required review | Read the source, confirm the applicable version and dates, then decide whether any workpaper or workflow needs attention |

The names, dates and change above come from the linked fixtures. They describe
no real legislation and establish no legal effect.

**Human decision:** Inspect the cited source and decide whether the apparent change affects a workpaper or workflow. This offline example makes no Register request and establishes no real legislative change.

<details>
<summary>Build the corpus, inspect contracts and check limitations</summary>

## Two related systems

The corpus builder (`python -m fadden`) produces retrieval material from Commonwealth legislation. Its `rulings` stage derives paragraph JSONL from named ATO rulings and related documents under the ATO's reuse notice ([docs/rulings.md](docs/rulings.md)). The change-review queue (`tax-radar-au`) consumes a reviewed observation contract and raises items for a person to assess.

**Package lifecycle:** source-only. The builder is not published to PyPI; releases carry source archives, not a ready-made corpus or a builder wheel. The queue has its own package identity; this source install does not download legislation.

## Reference

- [Pipeline commands and operating boundaries](BUILD.md)
- [Synthetic review queue](RADAR.md)
- [Monitor export and recovery contract](docs/monitor-contract.md)
- [Live capture and publication evidence boundaries](docs/evidence-export.md)
- [Dated build evidence and known corrections](docs/build-evidence.md)
- [Architecture](docs/architecture.md)
- [Accuracy, scope and redistribution limits](docs/scope.md)
- [Compared with the Open Australian Legal Corpus](docs/comparison.md)
- [Release rules](RELEASING.md) and [contributor checks](AGENTS.md)

Live Register capture, synthetic observations and publication candidates have distinct contracts. A successful capture or export does not authorise publication.

## Source and licence

The primary source is the [Federal Register of Legislation](https://www.legislation.gov.au/). The repository contains code, not the corpus; generated output retains its own source and licence records.

Code: [MIT](LICENSE). Read the [redistribution limits](docs/scope.md#what-this-deliberately-does-not-ship) before handling generated material.

</details>
