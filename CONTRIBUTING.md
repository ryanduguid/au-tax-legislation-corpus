# Contributing

This repository builds a corpus; it does not ship one. The pipeline downloads in-force Commonwealth tax legislation, extracts it, and records where every document came from. Provenance and licensing discipline matter more here than throughput.

## Data boundary

- Built artifacts stay out of git. The `.gitignore` blocks `epub/`, `markdown/`, `rates/`, `external/`, `corpus/`, `sources.json`, `INDEX.md`, `LICENCE-NOTICE.md` and the download logs. Commit the builder, not its output.
- Do not commit material whose licence does not permit redistribution, and do not remove the licence notice the pipeline generates.
- The PII scan exists because source documents contain names and identifiers. Keep its findings out of commit messages and pull request bodies.

## Extraction rules worth preserving

- A document is a set of volumes. Any decision made once per document but applied per volume will silently discard content. This repository has already lost the body of an entire title that way. Decide per volume.
- Manifest writes are atomic. Both writers of `manifest_raw.json` replace the file rather than truncating it.
- A title with no compilation is not superseded. Say what is actually known.

## Local verification

Python 3.10 or newer, standard library only.

```bash
python -m compileall -q .
python -m unittest discover -s tests -v
```

The suite includes regression tests tied to specific past defects. Do not relax one to make a change pass; the assertion is the record of what went wrong.

## Pull requests

State which stage of the pipeline changes and what the output looks like before and after. If a change alters counts recorded in a manifest, say which titles moved and why.

For a potential security vulnerability, follow [SECURITY.md](SECURITY.md) rather than opening an issue.
