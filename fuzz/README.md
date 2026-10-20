Four cargo-fuzz targets check invariants of the code that reads untrusted input:

- `privacy_patterns` feeds fabricated text to the privacy patterns in
  `fadden::pii` (`src/fadden/pii.rs`), which the PII scans, the distribution
  builder and its verifier run over every title. Every reported name and
  registration number must occur in the text, every contact fingerprint must
  carry a known kind and a SHA-256 digest rather than the identifier, and each
  pattern must finish on arbitrary input: one that backtracks without bound
  shows up as a timeout. Its seed corpus, `fuzz/corpus`, holds a statutory
  passage and fabricated contact details (the TFN is the ATO's published
  example).
- `pattern_engines` checks that a privacy pattern run on the regex crate, the
  fast path for a pattern without lookaround, finds the same groups at the same
  places as `fancy_regex`.
- `json_decode` feeds arbitrary bytes to the JSON decoder (`src/pyjson.rs`),
  which reads corpus files and Register responses: no input may panic it, and a
  decoded value written as `json.dumps` writes it must decode to the same JSON.
- `python_sort` checks the port of CPython's `list.sort` (`src/pysort.rs`): the
  stable sort under a total order, and every item kept under one that is not.

The last three take their seeds from `fuzz/seeds/<target>`. Failed invariants
abort the run.

cargo-fuzz needs a nightly toolchain. On Linux, from the repository root, run:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked --version 0.13.2
cargo +nightly fuzz run privacy_patterns fuzz/corpus -- -max_total_time=60 -max_len=4096 -timeout=2 -rss_limit_mb=1024
```

For the other targets, copy `fuzz/seeds/<target>` to a scratch directory and
pass that instead of `fuzz/corpus`. libFuzzer adds each new interesting input
to the corpus directory it is given; delete those hash-named files rather than
committing them. On Windows with the MSVC toolchain the same commands work once
the directory holding `clang_rt.asan_dynamic-x86_64.dll` (the Visual Studio
`bin\Hostx64\x64` tools directory) is on `PATH`; without it a target exits
with `STATUS_DLL_NOT_FOUND`.

The targets live in their own Cargo workspace here, so the repository's stable
builds never compile them. `.github/workflows/fuzzing.yml` runs each for 60
seconds after main pushes, weekly and on demand, with read-only repository
access and a 15-minute timeout; it is not a merge gate. The invariant checks
live in the library, and `tests/fadden_pii.rs` and `tests/fuzz_seeds.rs` replay
every seed through them on stable, so ordinary CI catches a broken check. A
successful run is bounded evidence, not proof that every input is handled.
