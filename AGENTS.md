# Agent instructions

This repository combines 2 deliberately separated systems: a corpus builder and a
synthetic change-review radar. Treat corpus outputs, synthetic monitor inputs, live
Register captures and publication candidates as different contracts.

- Synthetic monitor contracts project fabricated or separately reviewed observation
  facts and never query the Register. Live Register capture queries the official API,
  retains its bounded response bytes and emits a deliberately incompatible live contract.
- Never commit raw live captures, retained Register responses or generated corpus,
  evidence, distribution, demo or build output.
- Treat official capture and export destinations as immutable: use an absent destination,
  never overwrite, repair or delete an earlier output, and preserve the exact source bytes,
  content digests and cross-file provenance that support each result.
- The supported official live-evidence export boundary is Windows only. On every other
  platform it must fail closed before creating an output parent or staging directory.
- Live capture and local export are not publication authority. A human must explicitly
  authorise every tag, release, public development and deployment; `VERIFIED` evidence and
  passing checks do not authorise publication.
- The corpus is a finding aid derived from the EPUB reading view, not authorised legal
  text, tax advice or a conclusion about the practical effect of a change.

## Repository map

- [README.md](README.md) explains the 2 halves, corpus limits and high-level operation.
- [BUILD.md](BUILD.md) owns pipeline, monitor-export, live-capture and live-export details.
- [RADAR.md](RADAR.md) owns the synthetic review-queue contract and human-review boundary.
- [RELEASING.md](RELEASING.md) owns builder release preflight and immutable-release rules.
- The approved [Stage 3A capture design](docs/superpowers/specs/2026-08-29-live-register-capture-design.md)
  and [Stage 3B evidence design](docs/superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md)
  own the trust, provenance and publication boundaries. Keep operational detail there
  instead of duplicating it here.

## CI gates

`ci.yml` runs the full locked pytest suite on Ubuntu for Python 3.14 and 3.15, then the
package build and the linters. Its Windows 3.14 matrix runs
`tests/corpus/test_live_evidence_bundle_export.py` and
`tests/corpus/test_live_evidence_publication_paths.py` under branch coverage so the
supported live-evidence export boundary is exercised. Codecov merges that report with the
Ubuntu 3.14 report. Only the Ubuntu report is held to the `fail_under` floor, because the
Windows leg runs a subset. Other corpus modules remain excluded because
of pre-existing Windows failures around 8.3 short paths, junctions and reparse points;
do not describe a local Windows full-suite failure in those paths as a radar regression
without proving it.

The radar is the Rust `tax-radar-au` binary. The `rust` job runs its formatting, Clippy,
tests and release build on Ubuntu and Windows, and the Ubuntu test legs build it for the
cross-system contract test in `tests/corpus/test_monitor_contract.py`.

`tests/fadden_golden.rs` replays `tests/golden/`: fabricated cases each run through the
Python stages the `fadden` binary replaced, on Windows and Linux, with what Python printed
and wrote. A failure there is a departure from the Python behaviour, not a stale fixture;
regenerate fixtures only with the port's differential harness against the base revision.

The fenced list records the unique single-line commands. The multiline binary-smoke gate
is reproduced exactly in the next section.
```bash
rustup toolchain install && cargo build --locked --release
uv run --locked --extra dev pytest tests
uv run --locked --extra dev pytest tests/corpus/test_live_evidence_bundle_export.py tests/corpus/test_live_evidence_publication_paths.py
uv run --locked --extra dev coverage run --branch --source=fadden -m pytest tests
uv run --locked --extra dev coverage xml -o coverage-codecov.xml
uv run --locked --extra dev coverage report
uv run --locked --extra dev coverage run --branch --source=fadden -m pytest tests/corpus/test_live_evidence_bundle_export.py tests/corpus/test_live_evidence_publication_paths.py
uv run --locked --extra dev coverage xml --fail-under=0 -o coverage-windows.xml
uv run --locked --extra dev --python 3.14 python -m build
python -m venv /tmp/venv && /tmp/venv/bin/pip install --no-index dist/tax_radar_au-*.whl && cd /tmp && /tmp/venv/bin/python -c "import fadden.source_snapshot"
rustup toolchain install
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo test --locked --features fake-http --test fadden_golden
cargo build --locked --release
cargo install cargo-audit --locked --version 0.22.2 && cargo audit
uv run --locked --extra dev ruff check .
uv run --locked --extra dev mypy
uv audit --locked --no-config --python-version 3.14
```

## Release binary smoke

Build the release binary, then run it on the shipped samples from a fresh temporary
directory outside the checkout. This is a binary preflight, not a publication step:

```bash
bin="$PWD/target/release/tax-radar-au"
samples="$PWD/samples"
demo="$(mktemp -d)"
cd "$demo"
"$bin" compare --baseline "$samples/baseline/sample-sources.json" --observation "$samples/observations/sample-register-observation.json" --map "$samples/mappings/sample-source-skill-map.json" --out demo
"$bin" validate-review --queue demo/impact-queue.json --decision "$samples/decisions/sample-technical-review.json" --out demo/validation.json
```
