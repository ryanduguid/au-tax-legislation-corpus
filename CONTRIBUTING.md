# Contributing

The code here builds a corpus rather than carrying one. The pipeline downloads in-force Commonwealth tax legislation, extracts it, and records where each document came from. Expect review comments about provenance and licensing before you get any about speed.

## Data boundary

- Keep built artefacts out of git. The `.gitignore` blocks `epub/`, `markdown/`, `rates/`, `external/`, `corpus/`, `sources.json`, `INDEX.md`, `LICENCE-NOTICE.md` and the download logs. Commit the builder and leave its output on disk.
- Do not commit material whose licence forbids redistribution, and leave the licence notice the pipeline generates in place.
- The PII scan exists because source documents carry names and identifiers. Keep its findings out of commit messages and pull request bodies.

## Extraction rules worth preserving

- A document is a set of volumes. Decide per volume. A decision made once per document and applied per volume discards content without raising anything.
- Manifest writes are atomic. Both writers of `manifest_raw.json` replace the file rather than truncating it.
- A title with no compilation is not superseded. Say what you know.

## Local verification

Python 3.14 or newer, standard library only.

```bash
python -m compileall -q .
python -m unittest discover -s tests/corpus -t . -v
```

Discovery is scoped to `tests/corpus`. CI runs the Python suite as `uv run --locked --extra dev pytest` and the radar's Rust tests as `cargo test --locked` in `.github/workflows/ci.yml`.

The suite carries regression tests tied to specific past defects. Do not relax one to make a change pass. Each assertion records something that went wrong here.

The monitor-contract suite includes a compatibility assertion against
`tax-radar-au` (formerly `au_tax_change_impact_monitor`), the Rust binary in this
repository. It runs the binary named by `TAX_RADAR_AU_BIN`, or else a release or
debug build under `target/`; for example, in PowerShell:

```powershell
cargo build --locked --release
python -m unittest tests.corpus.test_monitor_contract.MonitorContractTests.test_generated_pair_is_accepted_by_the_radar_binary_when_built -v
```

Without a built binary, that one cross-system assertion skips; the producer-side
contract tests still run.

## Pull requests

Add or update tests with every change to behaviour: new functionality needs a test that fails without it, and a fixed defect needs a regression test. CI holds both packages to their coverage floor.

State which stage of the pipeline your change touches and what the output looks like before and after. If the change alters counts recorded in a manifest, say which titles moved and why.

For a potential security vulnerability, follow [SECURITY.md](SECURITY.md) rather than opening an issue.

Contributions are accepted under FSL-1.1-ALv2; see [LICENSE](LICENSE) and [NOTICE](NOTICE).
