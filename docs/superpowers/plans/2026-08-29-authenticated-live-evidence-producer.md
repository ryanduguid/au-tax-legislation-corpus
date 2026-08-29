# Authenticated Live Evidence Producer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Export one deterministic, rights-bearing `evidence-bundle.v2` asset for every valid `SUPERSEDED` title in a complete Stage 3A capture and publish the candidate set only through the approved manual attested-release workflow.

**Architecture:** One deep `fadden.export_live_evidence_bundles` module snapshots an immutable Stage 3A capture once, invokes the existing complete-graph validator on that owned snapshot, derives candidates and performs one identity-bound output promotion on Windows. It refuses official output mutation on other platforms. A dedicated GitHub-hosted Windows workflow consumes only the checked-in manifest and publishes the deterministic candidates after one multi-subject attestation. Synthetic v1 export remains byte-compatible and separate.

**Tech Stack:** Python 3.10+, standard library, Windows native filesystem handles, `unittest`, `pytest`, Ruff, mypy, GitHub Actions on `windows-latest`, PowerShell, GitHub CLI and `actions/attest` v4 pinned to `1e69f48acb82d1966a394da916b4c1698aa569d6`.

**Spec:** `docs/superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md` at or after commit `b5f4d23`.

## Global Constraints

- Work only on `feature/live-register-capture`; do not merge, push, create a release, run the 946-title live capture or activate hosted settings.
- Preserve the intentional untracked `GATES.md`; never stage, edit or delete it.
- `evidence-bundle.v1` bytes and existing synthetic behaviour must remain unchanged.
- `evidence-bundle.v2` is live-only, exact-member JSON, at most 1 MiB, with decoded raw response bytes at most 256 KiB.
- Every candidate carries an exact rights object derived from its own `observation.checked_at`; callers cannot provide rights wording or dates.
- `start` remains the compilation date. `registeredAt` remains independent and is not required to share that date.
- The exporter performs no network request and makes no public claim.
- Official output publication is Windows-only. Other platforms fail before creating a missing output parent or staging directory; no pathname-only fallback is permitted.
- The workflow has only `workflow_dispatch`, no inputs, runs on `windows-latest` with `pwsh`, has no floating action tags and uses no third-party release action.
- All filesystem failure paths fail closed, clean only exporter-owned staging and never overwrite an existing output.
- Use Australian English in prose and preserve exact code identifiers and official names.

---

### Task 1: Establish the v2 contract, rights object and golden bytes

**Files:**
- Create: `fadden/export_live_evidence_bundles.py`
- Create: `tests/corpus/test_live_evidence_bundle_export.py`
- Create: `tests/corpus/fixtures/live-evidence/evidence-bundle.v2.json`
- Modify: `tests/corpus/test_publication_bundle_export.py`

**Interfaces:**
- Produces: `LiveEvidenceExport(release_tag: str, candidates: tuple[Path, ...])`.
- Produces: `export_live_evidence_bundles(capture_dir: str | Path, output_dir: str | Path) -> LiveEvidenceExport`.
- Produces: `main(argv: Sequence[str] | None = None) -> int`.
- Keeps all bundle construction, validation, JSON, rights and identity helpers private.

- [ ] **Step 1: Write failing contract and golden tests**

Create `LiveEvidenceBundleContractTests` using one minimal owned Stage 3A fixture with baseline date `2026-08-05`, raw `start` `2026-08-18T00:00:00`, raw `registeredAt` `2026-08-27T17:31:41.1234567+10:00`, `checked_at` `2026-08-28T00:00:00Z`, compilation `20` and current document `F2026C00838`. Assert:

```python
result = export_live_evidence_bundles(capture, output)
self.assertEqual(
    result.release_tag,
    f"live-evidence-v2-{observation_digest.removeprefix('sha256:')}",
)
self.assertEqual(
    tuple(path.name for path in result.candidates),
    ("bundle-frl-f2022l00347-f2026c00838-r1.json",),
)
self.assertEqual(
    json.loads(result.candidates[0].read_text(encoding="utf-8"))["rights"],
    {
        "mode": "metadata-only",
        "attribution": (
            "Based on content from the Federal Register of Legislation at 2026-08-28. "
            "For the latest information on Australian Government legislation please go "
            "to https://www.legislation.gov.au. Changes: selected and reformatted Federal "
            "Register metadata into a bounded evidence bundle and factual source update; "
            "no legislation text is reproduced."
        ),
        "licence_url": "https://creativecommons.org/licenses/by/4.0/",
    },
)
```

Also assert exact key sets, two-space JSON, LF endings, one trailing newline, canonical base64 and byte equality with the checked-in v2 fixture. Add a regression which hashes the existing v1 fixture before and after export and asserts equality.

- [ ] **Step 2: Run the focussed tests and verify RED**

Run:

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_evidence_bundle_export.LiveEvidenceBundleContractTests -v
```

Expected: import failure because `fadden.export_live_evidence_bundles` does not exist.

- [ ] **Step 3: Implement the minimum deterministic contract**

Add this public shell and keep supporting helpers private:

```python
@dataclass(frozen=True)
class LiveEvidenceExport:
    release_tag: str
    candidates: tuple[Path, ...]


class LiveEvidenceBundleError(ValueError):
    pass
```

Implement the exact `export_live_evidence_bundles(capture_dir, output_dir)`
signature from the Interfaces block. Use
`json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n"`,
strict RFC 4648 base64, lowercase `sha256:` identifiers and the exact rights
template from the specification. Derive identities from validated
title/current-document identifiers only. The CLI eventually emits exactly
`candidate_count=<decimal>` and `release_tag=<tag>` on stdout; do not emit
paths, input data or response bytes.

- [ ] **Step 4: Make the focussed contract tests GREEN**

Run the Step 2 command. Expected: all `LiveEvidenceBundleContractTests` pass.

- [ ] **Step 5: Commit Task 1**

```powershell
git add fadden/export_live_evidence_bundles.py tests/corpus/test_live_evidence_bundle_export.py tests/corpus/fixtures/live-evidence/evidence-bundle.v2.json tests/corpus/test_publication_bundle_export.py
git diff --cached --check
git commit -m "feat: define live evidence bundle contract"
```

### Task 2: Validate the complete capture graph and derive candidates

**Files:**
- Modify: `fadden/capture_register.py`
- Modify: `fadden/export_live_evidence_bundles.py`
- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `tests/corpus/test_live_evidence_bundle_export.py`

**Interfaces:**
- Produces: `validate_capture_graph(capture_dir: str | Path) -> None` as a narrow Stage 3A graph-validation wrapper.
- Consumes: exact Stage 3A baseline, result, observation and retained evidence bytes from the exporter-owned snapshot.

- [ ] **Step 1: Add failing full-graph and state-selection tests**

Add table-driven cases proving:

- `complete: false`, non-`VERIFIED`, any `LOOKUP_FAILED`, missing/duplicate title, digest mismatch, unsafe evidence path or undeclared file fails the entire export;
- `UNCHANGED`, `CURRENT_NO_PUBLISHED_COMPILATION` and `NO_LONGER_IN_FORCE` create no asset;
- one inconsistent `SUPERSEDED` item prevents all otherwise-valid candidates;
- duplicate derived identities fail;
- null previous compilation number is accepted, but current number is non-empty;
- differing `start` and `registeredAt` dates are accepted;
- malformed `registeredAt` is rejected without an extra request;
- raw identifiers, `start`, response length, digest, media type, evidence path and reconstructed `capture_result_sha256` must agree.

Name the key regression `test_registered_at_is_independent_from_compilation_start` and assert `2026-08-18` remains in `observation.observed_compilation_date` while the raw response retains registration date `2026-08-27`.

- [ ] **Step 2: Run the graph tests and verify RED**

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_evidence_bundle_export.LiveEvidenceGraphTests -v
```

Expected: failures for missing complete-graph validation and candidate-state handling.

- [ ] **Step 3: Expose the narrow Stage 3A validator and implement snapshot validation**

In `capture_register.py`, add only:

```python
def validate_capture_graph(capture_dir: str | Path) -> None:
    _validate_staged_graph(Path(capture_dir).resolve(strict=True))
```

Keep `capture_register_run()` behaviour unchanged and add a regression proving the wrapper accepts a generated graph and rejects a mutated one. In the exporter, copy each recognised capture file once into an exporter-owned private snapshot, rejecting links, junctions, reparse points, aliases, special files and identity changes. Run `validate_capture_graph()` against that owned snapshot, then derive candidates only from the validated snapshot.

- [ ] **Step 4: Implement exact raw-row and rights validation**

Parse the raw OData bytes with duplicate-member rejection. Require exactly the Stage 3A context and row fields, one in-force current row, a non-null current document identifier and non-empty current compilation number. Validate `registeredAt` with the Stage 3A timestamp grammar but never compare its calendar component with `start`. Derive the rights date only from `observation.checked_at`.

- [ ] **Step 5: Run focussed Stage 3A and exporter tests GREEN**

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture tests.corpus.test_live_evidence_bundle_export -v
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit Task 2**

```powershell
git add fadden/capture_register.py fadden/export_live_evidence_bundles.py tests/corpus/test_live_register_capture.py tests/corpus/test_live_evidence_bundle_export.py
git diff --cached --check
git commit -m "feat: export verified live evidence candidates"
```

### Task 3: Complete atomic filesystem, version and CLI behaviour

**Files:**
- Modify: `fadden/export_live_evidence_bundles.py`
- Modify: `fadden/__init__.py`
- Modify: `fadden/__main__.py`
- Modify: `tests/corpus/test_live_evidence_bundle_export.py`
- Modify: `tests/corpus/test_corpus_cli.py`
- Modify: `README.md`
- Modify: `BUILD.md`

**Interfaces:**
- Consumes: `LiveEvidenceExport` from Tasks 1–2.
- Produces CLI: `python -m fadden export_live_evidence_bundles -- <capture-dir> --out <absent-output-dir>`.

- [ ] **Step 1: Add failing transaction, version and dispatcher tests**

Cover Windows absent output, one safe missing parent, input/output collision, existing output, linked ancestors, race before promotion, write/re-read/revalidation/rename failures, cleanup failure and exact owned-staging cleanup. Assert zero candidates still promotes one empty output directory and returns success on Windows. Assert other platforms fail before creating an output parent or staging directory. Test strict `VERSION` SemVer and equality with `pyproject.toml` project version. Pin the complete stage roster and exact argv forwarding.

- [ ] **Step 2: Run the focussed tests and verify RED**

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_evidence_bundle_export.LiveEvidenceFilesystemTests tests.corpus.test_corpus_cli -v
```

Expected: failures for incomplete transaction and missing dispatcher registration.

- [ ] **Step 3: Implement the transaction and CLI**

On Windows, use a private sibling name beginning `.<output-name>.live-evidence-`, mode `0o700`, exclusive file creation mode `0o600`, exact re-read validation, held output-parent and staging handles, and one native no-replace rename of the exact held staging object. Cleanup opens and disposes only the recorded children through the held directory identity, then disposes that exact staging directory. On other platforms, fail after read-only preflight but before creating a missing parent or staging directory. Add `export_live_evidence_bundles` to `STAGES`; forward everything after the literal `--` exactly as `capture_register` does. CLI usage errors return 2; contract/filesystem errors return 1 with bounded messages.

- [ ] **Step 4: Update local documentation**

Document the exact exporter command, live-only v2 boundary, independent registration/compilation dates, embedded rights, no-network behaviour and the fact that local export does not create a release or public development.

- [ ] **Step 5: Run focussed tests GREEN and commit**

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_evidence_bundle_export tests.corpus.test_corpus_cli -v
git add fadden/export_live_evidence_bundles.py fadden/__init__.py fadden/__main__.py tests/corpus/test_live_evidence_bundle_export.py tests/corpus/test_corpus_cli.py README.md BUILD.md
git diff --cached --check
git commit -m "feat: add atomic live evidence export command"
```

### Task 4: Add the manual attested immutable-release workflow

**Files:**
- Create: `.github/workflows/publish-live-evidence.yml`
- Create: `tests/corpus/test_live_evidence_workflow.py`

**Interfaces:**
- Consumes CLI summary keys `candidate_count` and `release_tag`.
- Produces no release when `candidate_count` is `0`; otherwise one draft-upload-publish release transaction.

- [ ] **Step 1: Add failing workflow policy tests**

Assert exact `workflow_dispatch:`, no inputs, repository/ref guard, `windows-latest`, `defaults.run.shell: pwsh`, 120-minute timeout, concurrency `publish-live-evidence-v2`, `cancel-in-progress: false`, exact permissions, fixed manifest, `uv==0.12.0`, 1,000-candidate bound, zero-candidate gates, one `actions/attest` invocation, draft before upload before publish, exact checked-out SHA target, `latest: false`, full action SHAs and no reusable/third-party release action. Assert runner-private paths use `Join-Path` from `$env:RUNNER_TEMP`, outputs append through `$env:GITHUB_OUTPUT`, assets are enumerated with `Get-ChildItem`, and `GH_TOKEN` appears only on the two `gh release` command steps.

- [ ] **Step 2: Run policy tests and verify RED**

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_evidence_workflow -v
```

Expected: missing workflow failure.

- [ ] **Step 3: Implement the workflow**

Use these resolved full-SHA action identities exactly; source review remains an
activation prerequisite:

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
  with:
    persist-credentials: false
- uses: actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97 # v7.0.0
- uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4
  if: steps.export.outputs.has_candidates == 'true'
  with:
    subject-path: ${{ runner.temp }}/live-evidence/*.json
```

Set the workflow's default `run` shell to `pwsh`. Run the fixed manifest capture and exporter below paths built with `Join-Path $env:RUNNER_TEMP`. Validate the CLI's two exact summary lines before appending `has_candidates`, `candidate_count` and `release_tag` through `$env:GITHUB_OUTPUT`. Enumerate candidate files with `Get-ChildItem -LiteralPath $candidateDir -Filter '*.json' -File`, sort them by name and pass their full-path array to `gh release upload` without `--clobber`; do not rely on shell glob expansion. Create a draft release targeting `$env:GITHUB_SHA`, upload all assets, then publish with `--latest=false`. Derive deterministic aggregate notes from the validated timestamp/count and state that every immutable asset contains its own attribution.

- [ ] **Step 4: Run policy tests GREEN and commit**

```powershell
uv run --locked --extra dev python -m unittest tests.corpus.test_live_evidence_workflow -v
git add .github/workflows/publish-live-evidence.yml tests/corpus/test_live_evidence_workflow.py
git diff --cached --check
git commit -m "ci: add attested live evidence release workflow"
```

### Task 5: Run producer acceptance and freeze the cross-repository fixture

**Files:**
- Modify only if a gate exposes a defect in Task 1–4 files.

**Interfaces:**
- Produces the exact v2 fixture bytes and SHA-256 consumed by the publisher plan.

- [ ] **Step 1: Run all repository-defined local gates**

```powershell
uv run --locked --extra dev python -m compileall -q .
uv run --locked --extra dev python -m unittest discover -s tests -v
$stage3bPytest = Join-Path ([System.IO.Path]::GetTempPath()) ("tax-radar-stage3b-" + [guid]::NewGuid().ToString("N"))
uv run --locked --extra dev pytest tests --basetemp $stage3bPytest
uv run --locked --extra dev ruff check tax_radar_au fadden tests
uv run --locked --extra dev mypy tax_radar_au
uv run --locked --extra dev --python 3.12 python -m build
git diff --check
```

Expected: every command exits zero. Do not run the live capture command.

- [ ] **Step 2: Record exact fixture evidence**

```powershell
$fixture = 'tests/corpus/fixtures/live-evidence/evidence-bundle.v2.json'
$hash = (Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash.ToLowerInvariant()
"$((Get-Item -LiteralPath $fixture).Length) bytes sha256:$hash"
```

Record the exact output in the task handoff. The publisher must copy, not regenerate, these bytes.

- [ ] **Step 3: Commit any gate-driven fixes separately**

Stage only the files changed for the verified defect, run `git diff --cached --check`, rerun the failed focussed test and commit with a message naming the defect. Leave `GATES.md` untracked.
