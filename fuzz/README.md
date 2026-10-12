The Atheris target feeds fabricated text to the privacy patterns in
`fadden/pii_patterns.py`, which the corpus build runs over every extracted Act.
Every reported name and registration number must occur in the text, every
contact fingerprint must carry a known kind and a SHA-256 digest rather than
the identifier, and each pattern must finish on arbitrary input: one that
backtracks without bound shows up as a timeout. Unexpected exceptions and
failed invariants fail the run. The seed corpus holds a statutory passage and
fabricated contact details (the TFN is the ATO's published example).

On Linux with Python 3.14, from the repository root, run:

```sh
python -m pip install --require-hashes --only-binary :all: -r fuzz/requirements-fuzz.txt
python -m fuzz.privacy_patterns fuzz/corpus -max_total_time=60 -max_len=4096 -timeout=2 -rss_limit_mb=1024
```

The patterns use only the standard library, so the harness imports `fadden`
from the checkout. `.github/workflows/fuzzing.yml` runs the target after main
pushes, weekly and on demand, with read-only repository access and a
five-minute timeout; it is not a merge gate. `tests/corpus/test_fuzz_harness.py`
replays the corpus with unittest, so the release preflight runs it too.
Atheris does not support Windows, so coverage-guided fuzzing runs on Linux. A
successful run is bounded evidence, not proof that every input is handled.

After changing the pinned version, regenerate the hashes with:

```sh
uv pip compile fuzz/requirements-fuzz.in --python-version 3.14 --python-platform x86_64-unknown-linux-gnu --generate-hashes --output-file fuzz/requirements-fuzz.txt
```
