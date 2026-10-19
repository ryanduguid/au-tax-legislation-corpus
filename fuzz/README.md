The cargo-fuzz target `privacy_patterns` feeds fabricated text to the privacy
patterns in `fadden::pii` (`src/fadden/pii.rs`), which the PII scans, the
distribution builder and its verifier run over every title. Every reported name
and registration number must occur in the text, every contact fingerprint must
carry a known kind and a SHA-256 digest rather than the identifier, and each
pattern must finish on arbitrary input: one that backtracks without bound shows
up as a timeout. Failed invariants abort the run. The seed corpus holds a
statutory passage and fabricated contact details (the TFN is the ATO's
published example).

cargo-fuzz needs a nightly toolchain. On Linux, from the repository root, run:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked --version 0.13.2
cargo +nightly fuzz run privacy_patterns fuzz/corpus -- -max_total_time=60 -max_len=4096 -timeout=2 -rss_limit_mb=1024
```

The target lives in its own Cargo workspace here, so the repository's stable
builds never compile it. `.github/workflows/fuzzing.yml` runs it after main
pushes, weekly and on demand, with read-only repository access and a 15-minute
timeout; it is not a merge gate. `tests/fadden_pii.rs` replays the seed corpus
through the same invariant check on stable, so ordinary CI catches a broken
harness. A successful run is bounded evidence, not proof that every input is
handled.
