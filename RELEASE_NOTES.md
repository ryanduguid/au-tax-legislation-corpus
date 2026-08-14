# v0.1.1

This is a release of the corpus builder, not a prebuilt legislation corpus.

Changes since `v0.1.0`:

- prevent nested tables, pre-section text, pre-body tables and heading-less volumes from being silently lost;
- make downloads, manifests and validated distribution publication fail closed and stage outputs atomically;
- strengthen source, hidden-entry, PII and unapproved-contact checks while keeping scan logs redacted; and
- add workflow-built builder archives, SHA-256 checksums, an SPDX SBOM and GitHub build attestations.

No downloaded EPUB, extracted legislation, corpus output or client data is included. Rebuild from the Federal Register of Legislation under the controls in `BUILD.md` and review the resulting licence notice before redistribution.
