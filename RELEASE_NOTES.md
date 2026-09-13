# v0.1.6

This candidate contains the corpus builder source. It excludes generated legislation and live evidence.

- Display the full queue digest and reject Markdown companions that differ from the JSON rendering.
- Record per-title EPUB metadata failures before refusing manifest publication.
- Validate the required PII summary before replacing publication artefacts.
- This release contains builder source only, without a corpus or live evidence.

# v0.1.5

This is a release of the corpus builder, not a prebuilt legislation corpus.

The `v0.1.4` tag was pushed on 7 September 2026 and its release preflight
failed before anything was published, so that tag carries no GitHub release.
A pushed tag is never moved, so `v0.1.5` is the same tree plus the fix.

Changes since `v0.1.3`:

- The release-policy preflight runs unittest discovery over the whole `tests`
  tree on a runner without pytest, and since the tax-radar-au merge that tree
  holds a pytest-only radar suite. `tests/radar/__init__.py` now withholds
  that package from unittest discovery, and a corpus test re-runs the
  preflight discovery with pytest hidden so it stays that way.
- The release workflow calls the shared release-policy archive workflow,
  pinned to a commit reachable from that repository's `main`
  (`99a6314`), after 2 repoints that followed release-policy's history
  rewrite. Dependabot no longer proposes bumps to that pin; the pin moves
  only through a reviewed change. `setup-uv` is pinned by commit, the
  evidence publisher runs without a cache, and every workflow carries a
  concurrency group and a job timeout.
- A repealed row that the Register still lists as current is recorded as no
  longer in force; `C2004A00982` is re-resolved against current Register
  metadata; `observed_at` is read without PowerShell datetime coercion; the
  distribution figures and stale documentation are corrected, raw HTML in
  radar Markdown is escaped, and the affected test paths are repaired.
- Documentation states the source-only package lifecycle, names GitHub
  Releases as the canonical release history and says what a release carries,
  adds cross-runtime contributor guidance for the corpus, and leads with a
  synthetic change-review export.

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

- apply both privacy predicates to every declared title representation: Markdown, optional endnotes and JSONL
- reject undeclared title files, nested directories, invalid UTF-8, binary control data, symbolic links, junctions and other Windows reparse points
- make TFNs permanently non-allowlistable and reject allowlist reasons that contain a raw contact identifier
- turn malformed JSON, rate records and manipulated manifest paths into named publication failures rather than tracebacks
- derive and verify README and distribution-index counts independently from the validated output.

The corrected builder passed 112 tests and 2 complete builds from the public
4 August 2026 source snapshot. Both builds produced the same validated 2,105-file
distribution: 934 redistributed titles and 21,596 rows, that snapshot's 946
titles and 21,784 rows less the 12 titles `pii_flagged.json` names. Those 12
titles hold 188 rows between them, of which 169 are recorded as naming private
individuals. The build drops a flagged title whole rather than row by row, so
all 188 of its rows leave the distribution, not only the 169.
Those figures describe that validation run, not a promise that the underlying
legislation remains current after the snapshot date.

No downloaded EPUB, extracted legislation, corpus output or client data is included. Rebuild from the Federal Register of Legislation under the controls in `BUILD.md` and review the resulting licence notice before redistribution.
