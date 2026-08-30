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

Python 3.10 or newer, standard library only.

```bash
python -m compileall -q .
python -m unittest discover -s tests/corpus -t . -v
```

That is the command `.github/workflows/verify.yml` runs. Discovery is scoped to `tests/corpus` because the radar half under `tests/radar` imports pytest, which is not in the standard library. `ci.yml` runs both halves as `uv run --locked --extra dev pytest`.

The suite carries regression tests tied to specific past defects. Do not relax one to make a change pass. Each assertion records something that went wrong here.

The monitor-contract suite includes an optional compatibility assertion against
`tax-radar-au` (formerly `au_tax_change_impact_monitor`). It first uses the audit workspace's fixed sibling
checkout when that checkout exists. In an ordinary clone, install the monitor
package or put its checkout root on Python's existing import path before launching
the test; for example, in PowerShell:

```powershell
$env:PYTHONPATH = 'C:\path\to\tax-radar-au'
python -m unittest tests.corpus.test_monitor_contract.MonitorContractTests.test_generated_pair_is_accepted_by_a_local_monitor_when_available -v
```

Without either source, that one cross-project assertion skips; the producer-side
contract tests still run.

## Pull requests

State which stage of the pipeline your change touches and what the output looks like before and after. If the change alters counts recorded in a manifest, say which titles moved and why.

For a potential security vulnerability, follow [SECURITY.md](SECURITY.md) rather than opening an issue.
