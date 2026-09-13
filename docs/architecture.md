## Why the code is shaped the way it is

[BUILD.md](../BUILD.md) lists every trap the code exists to avoid. Each one cost a
real defect. The short version:

- **Unordered `$skip`/`$top` paging silently drops rows.** No `$orderby` meant
  142 of 813 titles vanished, including the *Tax Agent Services Act 2009*.
- **`curl` does not truncate `-o` on transport failure**, so a shared temp file
  re-read the previous response and handed one Act another Act's compilation.
  The JSON stages read the response off the socket now, so they carry no temp
  file to inherit; `download.py` still uses `curl`, to a per-title path.
- **The download endpoint answers in 2 shapes**, raw EPUB bytes or a JSON
  envelope with the file base64 inside. Sniff the first byte.
- **HTTP errors and non-EPUB responses stop the download stage.** They are not
  evidence that a document is absent and must not be counted as `no_epub`.
  Only a validated staged download is moved to an `.epub` path.
- **The current version can have no document.** The Register records that an
  amendment commenced before publishing the compilation, so the URL 404s. That
  is not a broken download. Only an explicit null `registerId` on the current
  version is recorded as `no_epub`; the count changes as compilations are
  published.
- **Acts use 2 Word templates; instruments use dozens.** Deciding the template
  from the markup fails, because cosmetic classes look structural. Run the
  structural pass, and only when it finds nothing at all re-run with the
  bare-paragraph fallback, verified not to change a single Act row.
- **Never filter images by byte size.** Doing so deleted a GST decision
  flowchart and a maintenance-income formula.
- **The same-named helpers in the evidence stages are not duplicates.**
  `capture_register.py`, `export_live_evidence_bundles.py`,
  `export_publication_bundles.py` and `export_monitor_contract.py` share
  14 private helper names, and every one of them has drifted. None of the
  pairs is byte-identical, so none belongs in `corpus_paths.py`. Some carry a
  different rule, not just a different error class: the live-evidence
  `_json_bytes` sets `allow_nan=False`, its `_write_new` creates at mode 0o600
  through `os.open` rather than `open(path, "xb")`, and its
  `_same_regular_file_identity` also rejects a file with more than one link.
  `_required_text` has 3 incompatible definitions: capture strips before
  validating and rejects control characters, the live-evidence version rejects
  untrimmed input but allows control characters, and the publication version
  rejects both. Lifting any of them silently rewrites one caller's contract.
  Diff a pair before assuming the repetition is accidental.
