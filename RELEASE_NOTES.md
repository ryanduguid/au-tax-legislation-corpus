# Unreleased

- Breaking: the review queue is a standalone Rust binary, `tax-radar-au` 1.0.0, built with `cargo build --locked --release`. Its commands, options, exit statuses, stdout and stderr lines, output file bytes (CRLF on Windows), digests and argparse usage and error text match the Python radar at 43b5e0c. The `tax_radar_au` import package, its `sample_path` helper and the wheel's console script are removed; the samples moved to `samples/`. The Python wheel carries the corpus builder only, and `fadden.source_snapshot` holds the snapshot loader the publication export used from the radar.
- Documented differences from the Python radar:
  - stdout and stderr are always UTF-8. Python on a non-UTF-8 Windows code page printed an unencodable `--out` path backslash-escaped with a note on stderr; Python 3.15's UTF-8 mode behaves like the binary.
  - `rulings` fetches with its own HTTP client. It follows up to 10 redirects, honours the proxy environment variables and checks certificates against the operating system's store, as urllib does, but it does not read the Windows Internet Settings proxy, it sends its own `Accept` and connection headers, and its 90-second limit applies to each phase of a request rather than to each socket read. Where a transport failure is reported as `fetch failed (<name>)`, the name is `URLError`, `TimeoutError` or `IncompleteRead` by the phase that failed, which can differ from the exception urllib would have raised. A final URL after redirects is recorded as the client normalises it.
  - `dist_verify` still exits 2 when the checks could not run, but prints only the traceback's last line where Python printed it whole.
  - `dist_verify` lists unapproved contact findings in its FAIL detail sorted by title and digest; Python listed them in set order, which follows string hashes and varied from run to run. Where two values that cannot be ordered meet in a sort (a non-string collection beside string ones), the TypeError can name the two types in the other order, as Python's own runs could.
  - An argument that is not valid Unicode (possible on Windows) is refused with `tax-radar-au: blocked: every argument must be valid Unicode.` and exit 2, where Python accepted it and failed later on the path.
  - Resolving an output path whose existing component cannot be opened (access denied, sharing violation) keeps that component's letter case as given; Python asked Windows for its stored name.
- The `as_at`, `compare_builds`, `dist`, `dist_verify`, `pii_scan`, `pii_scan2`, `probe13`, `rates`, `rulings` and `search` corpus stages are the Rust `fadden` binary. The privacy patterns they share are `src/fadden/pii.rs`; `fadden/pii_patterns.py` is removed, and the fuzzing workflow runs a cargo-fuzz target on nightly Rust instead of the Atheris one. `python -m fadden` runs it with the same arguments, and it keeps their command lines, output bytes and exit statuses. Documented differences:
  - An exception the Python stage did not catch printed a traceback; the binary prints only its last line, with the same exit status 1.
  - A search index records the bundled SQLite's version (3.53.2) in its file header, so the file differs from Python's in those 4 header bytes only.
  - An argument that is not valid Unicode (possible on Windows) is refused with `fadden: every argument must be valid Unicode.` and exit 2, as the radar does.
  - A file or directory name that is not valid Unicode (bytes that are not UTF-8 on Linux, a lone surrogate on Windows) in a tree a stage lists stops the stage with `ValueError: file name is not valid Unicode: '<path>'`, exit 1, or exit 2 from `dist_verify`. Python carried such a name through as a surrogate-escaped string.
  - JSON nested more than 1,000 levels deep raises `RecursionError: maximum recursion depth exceeded while decoding a JSON array from a unicode string` (or `object`). CPython 3.15 raises it only where its C stack runs low, above 13,000 levels on Windows, and words the message by the stack used.
  - Writing JSON nested more than 900 containers deep (`dist`'s `sources.json`, `pii_scan`'s `pii_flagged.json`, `probe13.json`) raises `RecursionError: maximum recursion depth exceeded`. Python's encoder stops at its recursion limit, about 995 containers in a bare script and fewer under a deeper call stack. `pii_scan` leaves `pii_flagged.json` empty where Python left the part written before it stopped.
  - Sorts follow CPython 3.15. Where 64 or more items include values that do not order (or NaNs), CPython 3.14 forms its runs differently and can meet a different pair of types, or leave NaNs elsewhere, as 3.14 and 3.15 differ from each other.

# v0.3.0

This candidate contains the corpus builder source. It excludes generated legislation and live evidence.

- Breaking: the builder and radar require Python 3.14 or later; 0.2.1 remains the last release for Python 3.11 to 3.13 (#166).
- Licence: Apache-2.0 replaces MIT for the code, with a `NOTICE` file whose attribution anyone redistributing the code or a derivative must keep (#179). Releases before 0.3.0 stay under MIT.
- A `compare_builds` stage lists the sections added, removed and changed between two local builds, title by title, and the titles present in only one of them (#170).
- Stage 2 resolves up to eight principal titles in each paced request, keeps record order, and falls back to single-title requests when batching is unavailable. A malformed response or unresolved title leaves `acts_resolved.json` unchanged (#175).
- EPUB tables keep source-cell columns under `rowspan`, overlapping row groups are refused, and the parser checks expanded, buffered and padded allocations against its limits (#178).
- The dot-leader pattern in title extraction no longer backtracks (#192).
- The release also carries its signed provenance bundle, `au-tax-legislation-corpus-builder-0.3.0.intoto.jsonl` (#191).
- This release contains builder source only, without a corpus or live evidence.

# v0.2.1

This candidate contains the corpus builder source. It excludes generated legislation and live evidence.

- No v0.2.0 release exists. The v0.2.0 tag (5 October 2026) stopped at the release preflight because the release workflow listed its Python 3.11 check twice; it now lists it once, and a test refuses a repeated check. 0.2.1 carries every change below.
- Breaking: the builder and radar require Python 3.11 or later; 0.1.7 remains the last release for Python 3.10 (#162).
- An `as_at` stage checks a compilation's point in time, each row carries its compilation window, and a literal quotation check compares quoted text with the source (#154).
- Archive and table extraction are bounded, Register identities and timestamps are validated, and existing corpus and radar outputs survive a failed staging or promotion (#159).
- The radar keeps its input files when an output path names one of them, directly or through a link (#149), bounds its companion reads and rejects control characters in profile labels (#150), and reports a non-string observation schema version as blocked input with exit 2 (#155).
- The extractor treats a valueless `class` attribute as empty instead of stopping (#161), and the rulings fetch closes HTTP error responses before retrying (#160).
- CodeQL runs from a repository workflow so a release can require it (#152).
- This release contains builder source only, without a corpus or live evidence.

# v0.1.7

This candidate contains the corpus builder source. It excludes generated legislation and live evidence.

- A rulings stage collects ATO Legal Database documents into a collection of their own (#139).
- Rate rows carry their provenance, discovery reads the Register's status before anything else, and the guides and labels are corrected (#114).
- A rebuild refuses titles the list no longer names instead of carrying them forward, the PII scan no longer takes decimal rate factors for phone numbers, five licence-restricted standards stay out of distributions, and a failed first build rolls back without discarding complete new downloads (#144).
- Corpus content is preserved and evidence publication requires review (#132); draft publication is retry-safe and title lists are written atomically (#118); a failed check is reported apart from a verifier that could not run (#119).
- Live-evidence tagging and publication run in a protected environment (#143), and a builder release now requires the mandatory `main` checks to have passed for its commit (#131).
- The radar gains an exposure command for pseudonymous client profiles (#145).
- This release contains builder source only, without a corpus or live evidence.

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
