# Contributing

This is a review queue. It surfaces potential changes to Australian tax sources with their provenance attached and hands every technical decision to a human reviewer. Contributions must not add anything that states a legal conclusion, infers a rule, or updates downstream content without a human in the loop.

## Design rules

- Provenance travels with the item: source, version and retrieval detail, or the item does not enter the queue.
- An incomplete scope stays visible as incomplete. Do not collapse an unknown into a default.
- Map exact source identifiers. Leave a near-match for the reviewer to judge.
- No network access at runtime, and no imports that would open one. The test suite walks the AST to enforce this, so keep it passing rather than working around it.

## Data boundary

Use synthetic fixtures. The `.gitignore` blocks `.env`, `.env.*`, `*.jsonl`, `*.epub`, `*.pdf` and `corpus/`. Keep out retrieved legislation, subscription content, and anything whose licence does not permit redistribution.

## Local verification

Python 3.10 or newer, with `uv` and a committed lock file.

```bash
uv sync --locked --extra dev --python 3.12
uv run --locked --extra dev --python 3.12 pytest
uv run --locked --extra dev --python 3.12 python -m build
```

CI runs the tests on Ubuntu with Python 3.10 through 3.13 and on Windows with 3.12, plus a packaging job and CodeQL.

## Pull requests

Name the review gate or classification rule your change affects and show the test that pins it. Watch timestamp handling: `datetime.fromisoformat` accepts a different grammar on 3.10 than on 3.11 and later, so test the boundary instead of assuming the interpreter agrees with your pattern.

For a potential security vulnerability, follow [SECURITY.md](SECURITY.md) rather than opening an issue.
