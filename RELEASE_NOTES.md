# v0.1.3

This is a release of the corpus builder, not a prebuilt legislation corpus.

First release cut from the repository's current history, restoring the full
verification story (`gh release verify` and `gh attestation verify`) that the
history rewrite broke for v0.1.1 and v0.1.2.

- `python -m fadden <stage>` works from a source checkout: the dispatcher puts
  the package directory on `sys.path` so the stages' flat imports resolve, and
  a forwarded retrieval date now reaches `extract`. A real-import test over
  every stage guards the entry point.
- BUILD.md documents the mandatory PII scan stages and no longer claims the
  source checkout ships the regenerated intermediates.
- RELEASING.md and the sibling references use the current repository names;
  the release workflow and its pinning test point at reachable
  release-policy history; `tools/build_release_archives.py` is synchronised
  with the shared script the release actually runs.

# v0.1.2

This is a release of the corpus builder, not a prebuilt legislation corpus.

Changes since `v0.1.1`:

- apply both privacy predicates to every declared title representation: Markdown, optional endnotes and JSONL;
- reject undeclared title files, nested directories, invalid UTF-8, binary control data, symbolic links, junctions and other Windows reparse points;
- make TFNs permanently non-allowlistable and reject allowlist reasons that contain a raw contact identifier;
- turn malformed JSON, rate records and manipulated manifest paths into named publication failures rather than tracebacks; and
- derive and verify README and distribution-index counts independently from the validated output.

The corrected builder passed 112 tests and two complete builds from the public
4 August 2026 source snapshot. Both builds produced the same validated 2,105-file
distribution: 934 redistributed titles, 21,728 rows and 6,068,848 body words.
Those figures describe that validation run, not a promise that the underlying
legislation remains current after the snapshot date.

No downloaded EPUB, extracted legislation, corpus output or client data is included. Rebuild from the Federal Register of Legislation under the controls in `BUILD.md` and review the resulting licence notice before redistribution.
