# Live Federal Register Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fail-closed, evidence-preserving live capture command that observes every title in a supplied corpus manifest and emits an immutable baseline, capture graph and v4 monitor observation without enabling live publication.

**Architecture:** Build one deep module, `fadden.capture_register`, around the public `capture_register_run` interface. The module owns strict manifest projection, canonical Register queries, the production HTTPS adapter, state derivation, byte-for-byte evidence storage, graph validation and one-rename publication. Tests inject a small in-memory `RegisterSession`; the CLI only parses arguments and delegates.

**Tech Stack:** Python 3.10 standard library, `unittest`, existing `uv` lockfile, Ruff, mypy and Python build tooling.

**Spec:** [`docs/superpowers/specs/2026-08-29-live-register-capture-design.md`](../specs/2026-08-29-live-register-capture-design.md)

## Global Constraints

- Preserve the existing v1-v3 synthetic monitor contracts and `evidence-bundle.v1` byte-for-byte.
- Do not add a live flag, partial-scope mode, overwrite mode, scheduler, database, publisher integration, credentials or third-party dependency.
- All automated tests must use deterministic in-memory exchanges; no test may reach the network or sleep.
- Preserve an explicit null `compilationNumber` for the 47 legacy unnumbered titles in the checked-in manifest. The member remains mandatory; never invent a number.
- Retain only bounded HTTP 200 bodies. Never retain, log or serialise a non-200 body, arbitrary header, exception text or absolute evidence path.
- Perform all input and filesystem preflight before creating a missing output parent. Promote an official capture only with one rename into an absent destination.
- Make the implementation work in both package context (`python -m fadden`) and the repository's supported Python 3.10-3.13 range.
- Keep raw live captures under ignored operator-chosen output directories. Do not commit a real capture.

---

## File Map and Public Interfaces

- Create `fadden/capture_register.py`: the sole live-capture module. It defines `CaptureRegisterError`, immutable `RegisterExchange`, the `RegisterSession` protocol, the standard-library production session, `capture_register_run`, JSON and HTTP validation, state classification, graph validation and atomic directory promotion.
- Create `tests/corpus/test_live_register_capture.py`: public-interface tests with an in-memory session and deterministic exchange fixtures.
- Modify `fadden/__init__.py`: append `capture_register` to `STAGES`.
- Modify `fadden/__main__.py`: forward remainder arguments to `capture_register.main` using the same path as the two existing exporters.
- Modify `tests/corpus/test_corpus_cli.py`: pin the new stage roster and argument forwarding.
- Modify `BUILD.md` and `README.md`: document the live capture command, output contract, operational pacing, evidence limits and closed publication boundary.

The public Python seam is:

```python
def capture_register_run(
    manifest_path: str | Path,
    destination: str | Path,
    *,
    session: RegisterSession | None = None,
) -> dict[str, Path]:
    """Capture complete Register evidence and return the three contract paths."""
```

The returned mapping has exactly the keys `baseline`, `capture` and `observation`.

The module fixes these contract constants: `au-tax-register-capture.v1`,
`au-tax-register-observation.v4`, `au-primary-tax-legislation.v4`, `live`,
`https://api.prod.legislation.gov.au/v1/`, an 8 MiB manifest limit and a
256 KiB retained-response limit.

The injectable transport seam is:

```python
@dataclass(frozen=True)
class RegisterExchange:
    checked_at: str
    status: int | None
    headers: Mapping[str, str]
    body: bytes | None
    attempts: int
    error_category: str | None = None


class RegisterSession(Protocol):
    observed_at: str

    def get(self, url: str) -> RegisterExchange:
        """Return one bounded, already-retried exchange for a canonical URL."""
```

Each capture result has this exact structure; its `requests` array contains the current request and, only when required, the history request:

```json
{
  "register_id": "C2004A00467",
  "collection": "Act",
  "checked_at": "2026-08-29T00:00:01Z",
  "state": "UNCHANGED",
  "error_category": null,
  "requests": [
    {
      "role": "current",
      "url": "https://api.prod.legislation.gov.au/v1/versions?%24top=1&%24filter=titleId%20eq%20%27C2004A00467%27%20and%20isCurrent%20eq%20true&%24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt",
      "checked_at": "2026-08-29T00:00:01Z",
      "http_status": 200,
      "transport_error_category": null,
      "attempt_count": 1,
      "response_headers": {
        "content-type": "application/json; odata.metadata=minimal; odata.streaming=true; charset=utf-8",
        "odata-version": "4.0"
      },
      "response_length": 322,
      "response_sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "evidence_path": "evidence/sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"
    }
  ]
}
```

The repeated `a` value is a concrete schema example. Production and test objects must match `sha256:[0-9a-f]{64}` exactly and derive it from the actual response bytes.

## Task 1: Establish the single-title happy-path vertical slice

**Files:**

- Create: `tests/corpus/test_live_register_capture.py`
- Create: `fadden/capture_register.py`

- [ ] Add deterministic test fixtures for one rich manifest row, one exact current-version body and an in-memory session.

```python
CURRENT_URL = (
    "https://api.prod.legislation.gov.au/v1/versions?%24top=1&"
    "%24filter=titleId%20eq%20%27C2004A00467%27%20and%20isCurrent%20eq%20true&"
    "%24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt"
)


class MemorySession:
    observed_at = "2026-08-29T00:00:00Z"

    def __init__(self, exchanges: dict[str, list[RegisterExchange]]) -> None:
        self.exchanges = {url: list(items) for url, items in exchanges.items()}
        self.urls: list[str] = []

    def get(self, url: str) -> RegisterExchange:
        self.urls.append(url)
        return self.exchanges[url].pop(0)
```

- [ ] Write `test_single_unchanged_title_writes_complete_immutable_graph` through `capture_register_run`. Assert the exact request URL, exact returned path mapping, projected baseline fields, `complete: true`, `run_status: VERIFIED`, `UNCHANGED`, exact raw evidence bytes and all declared hashes. Assert the capture top-level key set is exactly `schema_version`, `mode`, `observed_at`, `source_api`, `baseline_sha256`, `expected_register_ids`, `complete`, `results`; assert the observation top-level key set is exactly `schema_version`, `mode`, `observed_at`, `scope_id`, `baseline_sha256`, `capture_sha256`, `expected_register_ids`, `complete`, `run_status`, `observations`.
- [ ] Run the focused test and confirm it fails because `fadden.capture_register` does not exist.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture.LiveRegisterCaptureTests.test_single_unchanged_title_writes_complete_immutable_graph -v`

Expected: `ERROR` with `ModuleNotFoundError: No module named 'fadden.capture_register'`.

- [ ] Implement the smallest complete vertical slice: constants, error type, dataclass/protocol, deterministic JSON serialisation, canonical current URL, one-row OData validation, `UNCHANGED` derivation, evidence hashing, v1 capture, v4 observation and staged directory promotion.

Use fixed serialisation everywhere:

```python
def _json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def _sha256_id(content: bytes) -> str:
    return f"sha256:{hashlib.sha256(content).hexdigest()}"
```

- [ ] Re-run the focused test and confirm it passes.
- [ ] Commit the vertical slice.

```bash
git add fadden/capture_register.py tests/corpus/test_live_register_capture.py
git commit -m "feat: capture unchanged Register evidence"
```

## Task 2: Complete strict manifest projection and preflight

**Files:**

- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `fadden/capture_register.py`

- [ ] Add a multi-title projection test proving sort order, per-title retrieval dates, the latest top-level retrieval date, ignored rich fields, default `version_is_current: true`, and required `current_version_start` for an explicitly non-current baseline title.
- [ ] Add a legacy unnumbered-title test proving an explicit null `compilationNumber` is preserved and can match the same null and date in a registered current row; reject a missing member.
- [ ] Add a table-driven rejection test covering an empty array, non-array top level, over-8-MiB input, malformed UTF-8, duplicate members at the top and nested levels, duplicate IDs, invalid collections, non-canonical dates, control characters and malformed Register IDs.
- [ ] Add exact source-URL cases. Accept only:

```text
https://www.legislation.gov.au/<same-id>/<same-versionStart>/<same-versionStart>/text/original/epub
```

Reject credentials, ports, fragments, query strings, alternate hosts, encoded separators, ID mismatch and date mismatch.
- [ ] Add filesystem preflight tests proving a rejected manifest creates neither destination nor its missing parent.
- [ ] Run the manifest tests and confirm the new cases fail.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture.LiveRegisterCaptureManifestTests -v`

Expected: failures for unimplemented strict input validation.

- [ ] Implement one-snapshot ordinary-file loading with `os.lstat`, a byte limit before decode, strict UTF-8, duplicate-member rejection and exact rich-manifest projection. Reuse `corpus_paths.register_id` for the ID allowlist; do not duplicate its regular expression.
- [ ] Define the baseline shape as the existing monitor v1 source shape with fixed constants:

```python
{
    "corpus": "Commonwealth tax statutes and legislative instruments",
    "retrieved": max(title["retrieved"] for title in titles),
    "source": "Federal Register of Legislation",
    "source_api": "https://api.prod.legislation.gov.au/v1/",
    "titles": projected_titles,
}
```

Each projected title uses the existing monitor keys `register_id`, `name`,
`collection`, `compilation_number`, `compilation_date`, `version_is_current`,
`current_version_start`, `retrieved`, `source_url` and `register_page`.
`register_page` is fixed to
`https://www.legislation.gov.au/<register-id>/latest/text`.

- [ ] Re-run the manifest tests, then the original happy-path test.
- [ ] Commit strict projection.

```bash
git add fadden/capture_register.py tests/corpus/test_live_register_capture.py
git commit -m "feat: validate live capture manifests"
```

## Task 3: Implement the complete state machine and response validation

**Files:**

- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `fadden/capture_register.py`

- [ ] Add public-interface tests for all five states. Pin these valid interpretations:

| Current response | History response | State |
|---|---|---|
| Same registered compilation and date | Not requested | `UNCHANGED` |
| Different registered compilation with later date | Not requested | `SUPERSEDED` |
| In-force current row with `registerId: null` | Not requested | `CURRENT_NO_PUBLISHED_COMPILATION` |
| Empty current `value` | Same-title historical row | `NO_LONGER_IN_FORCE` |
| Insufficient or inconsistent evidence | Requested only after empty current | `LOOKUP_FAILED` |

- [ ] Pin the canonical history query:

```text
https://api.prod.legislation.gov.au/v1/versions?%24top=1&%24filter=titleId%20eq%20%27C2004A00467%27&%24orderby=start%20desc&%24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt
```

- [ ] Add table-driven `LOOKUP_FAILED` cases for transport failure, non-200 status, oversized body, wrong media type, malformed UTF-8, duplicate JSON members, wrong envelope, unknown envelope members, missing or extra row fields, two rows, wrong title, non-boolean `isCurrent`, wrong current status, malformed date/time, empty registered compilation number, malformed document ID, equal-date mismatch and backwards chronology. Also reject malformed exchange timestamps, non-positive attempt counts, unsafe header values and contradictory status/body/error combinations before they enter output.
- [ ] Assert every failure category is a fixed bounded token from an allowlist such as `TRANSPORT_ERROR`, `HTTP_STATUS`, `RESPONSE_TOO_LARGE`, `UNSUPPORTED_MEDIA_TYPE`, `INVALID_JSON`, `INVALID_ODATA_SHAPE`, `IDENTITY_MISMATCH` and `INCONSISTENT_CHRONOLOGY`.
- [ ] Run the state tests and confirm the unimplemented cases fail.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture.LiveRegisterCaptureStateTests -v`

Expected: failures for missing fallback queries and closed-state classification.

- [ ] Implement exact OData envelope validation. Require exactly `@odata.context` and `value`; require the context to equal `https://api.prod.legislation.gov.au/v1/$metadata#Versions(titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)`; require each row to contain exactly the seven selected members. Parse `start` as a valid ISO timestamp and reduce it to its canonical date. Permit `registeredAt` to be null only when `registerId` is null; otherwise require a valid ISO timestamp. Permit a registered row's `compilationNumber` to be literal null only so an explicit null baseline can match the same date; do not treat a later null-numbered version as a guessed supersession.
- [ ] Implement the state matrix without fall-through guesses. A current row must be `isCurrent is True` and `status == "InForce"`; a history row used for cessation must have the same title, `isCurrent is False` and a status in the official `InForce`, `Ceased`, `Repealed`, `NeverEffective` enum. Any other combination becomes `LOOKUP_FAILED`.
- [ ] Derive v2-compatible observation fields and nullability for each state. Use the official Register version page for a registered document evidence URL and the title page for a no-document or no-longer-in-force state.
- [ ] Set `complete` from exact attempted-ID coverage. Set `run_status` to `VERIFIED` only when complete and no observation is `LOOKUP_FAILED`.
- [ ] Re-run the state tests and the complete live-capture test module.
- [ ] Commit the state machine.

```bash
git add fadden/capture_register.py tests/corpus/test_live_register_capture.py
git commit -m "feat: classify complete Register observations"
```

## Task 4: Add the production HTTPS session without weakening the seam

**Files:**

- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `fadden/capture_register.py`

- [ ] Add tests that call `capture_register_run` with `session=None` while patching the standard-library opener, monotonic clock, sleep and UTC clock. The fake opener must never perform a network request.
- [ ] Prove the production adapter sends the repository user agent, uses a 90-second timeout, refuses 30x redirects, caps the body at 256 KiB plus one sentinel byte, normalises only `Date`, `Content-Type`, `OData-Version` and `X-Frl-Version`, and maps exceptions to bounded categories without serialising their text.
- [ ] Prove at most three attempts, at least six seconds between retry attempts and at least 1.5 seconds between separate Register requests. Assert the tests record requested sleeps but consume no wall-clock delay.
- [ ] Run the production-session cases and confirm they fail before implementation.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture.LiveRegisterProductionSessionTests -v`

Expected: failures because the default session and retry/pacing policy are incomplete.

- [ ] Implement a no-redirect `urllib.request.HTTPRedirectHandler`, HTTPS-only host checks and one bounded read. Treat a redirect as `HTTP_STATUS`; never follow its `Location` value.
- [ ] Retry only bounded transport failures and HTTP 408, 429 and 5xx responses. Return the final status, attempts and allowed headers; discard every non-200 body immediately.
- [ ] Capture `observed_at` at session construction and `checked_at` after each completed exchange using explicit UTC `Z` timestamps.
- [ ] Re-run production-session tests and the whole capture test module.
- [ ] Commit the production adapter.

```bash
git add fadden/capture_register.py tests/corpus/test_live_register_capture.py
git commit -m "feat: add paced Register HTTPS session"
```

## Task 5: Make the evidence graph self-validating and deterministic

**Files:**

- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `fadden/capture_register.py`

- [ ] Add tests for two titles sharing identical 200 bytes. Assert one content-addressed evidence file, two declarations of the same digest and no body reserialisation.
- [ ] Add a full-scope continuation test in which the first title fails and later titles still run. Assert every expected ID occurs exactly once, `complete: true` and `run_status: BLOCKED`.
- [ ] Add deterministic-repeat tests using the same manifest, exchanges and timestamps in two destinations. Assert byte equality for all four JSON/evidence roles, allowing only directory names to differ outside file contents.
- [ ] Add cross-file tamper tests by patching the final validation seam after staged writes. Corrupt the baseline, capture manifest, observation, an evidence file, a declared digest and a relative evidence path one at a time; assert no official destination is promoted.
- [ ] Prove a bounded malformed HTTP 200 body is retained byte-for-byte even though its title becomes `LOOKUP_FAILED`. Prove a non-200 body supplied by an injected session is discarded and has no digest, evidence path or byte fragment in any output.
- [ ] Assert `capture_result_sha256` hashes the deterministic capture-result object, `evidence_id` contains the Register ID plus the first 32 result-digest hex characters, `primary_response_sha256` binds only the current request body, and `capture_sha256` binds the final `register-capture.json` bytes.
- [ ] Run the graph tests and confirm missing validation cases fail.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture.LiveRegisterEvidenceGraphTests -v`

Expected: failures for deduplication, digest binding or final graph revalidation not yet implemented.

- [ ] Centralise generated-byte and digest construction. Write evidence first, then capture bytes, then observation bytes. Re-read every staged file as bytes and validate the exact three root JSON filenames, an `evidence` directory containing only declared `sha256-<hex>.json` files, relative containment, declarations, hashes, expected ID coverage and schemas before promotion.
- [ ] Normalise the validated primary response media type to `application/json` in v4 while retaining the selected raw `Content-Type` header value in the capture result.
- [ ] Ensure the capture manifest never declares its own digest and JSON never contains an absolute local path.
- [ ] Re-run graph tests and all capture tests.
- [ ] Commit graph validation.

```bash
git add fadden/capture_register.py tests/corpus/test_live_register_capture.py
git commit -m "feat: validate immutable Register evidence graph"
```

## Task 6: Harden destination ownership and atomic promotion

**Files:**

- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `fadden/capture_register.py`

- [ ] Add destination tests modelled on `test_publication_bundle_export.py`: existing destination file/directory/link, unsafe output leaf, special manifest, linked manifest, linked or reparse ancestor, input/output alias, more than one missing parent and a raced destination appearing before promotion.
- [ ] Add handled write, revalidation and `os.rename` failure tests. Assert the destination and any pre-existing paths remain untouched and only the module-owned staging directory is removed.
- [ ] On platforms that can create symbolic links or junctions, exercise real filesystem objects; otherwise skip only the unsupported case. Mock `st_file_attributes` to cover the Windows reparse-point branch everywhere.
- [ ] Run filesystem tests and confirm the new cases fail.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_live_register_capture.LiveRegisterCaptureFilesystemTests -v`

Expected: failures for unimplemented ownership or race checks.

- [ ] Implement absolute-path normalisation without resolving through a prospective destination. Validate every existing ancestor with `lstat`; reject links, junctions, reparse points and special files. Permit creation of only one absent, safe-named immediate parent after input validation.
- [ ] Name staging `.<destination-name>.register-capture-<uuid>.tmp`, create it as a private sibling, and only remove it when its parent and prefix prove module ownership. Re-check the absent destination and ordinary parent immediately before one `os.rename`.
- [ ] Re-run filesystem tests and all capture tests.
- [ ] Commit filesystem hardening.

```bash
git add fadden/capture_register.py tests/corpus/test_live_register_capture.py
git commit -m "fix: harden live capture publication boundary"
```

## Task 7: Wire the CLI, document operations and preserve publication closure

**Files:**

- Modify: `fadden/__init__.py`
- Modify: `fadden/__main__.py`
- Modify: `tests/corpus/test_corpus_cli.py`
- Modify: `tests/corpus/test_live_register_capture.py`
- Modify: `BUILD.md`
- Modify: `README.md`

- [ ] Extend the CLI tests so `capture_register` imports in package context and receives `manifest.json --out capture-dir` as a forwarded list.
- [ ] Add direct `capture_register.main` tests for required arguments, successful path reporting and parser exit code 2 with a bounded error message. Assert no exception text or response body reaches stderr.
- [ ] Add a regression asserting `export_publication_bundles` still rejects a live v4 observation and the existing sample v3 bundle bytes remain unchanged.
- [ ] Run the CLI and publication-boundary tests and confirm they fail before wiring.

Run: `uv run --locked --extra dev python -m unittest tests.corpus.test_corpus_cli tests.corpus.test_live_register_capture.LiveRegisterCaptureCliTests tests.corpus.test_publication_bundle_export -v`

Expected: CLI roster/forwarding failures; existing publication tests remain green.

- [ ] Append `capture_register` to `STAGES` and include it in the argument-forwarding branch in `fadden.__main__.main`.
- [ ] Implement the module CLI:

```python
def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Capture complete live Federal Register metadata evidence."
    )
    parser.add_argument("manifest", type=Path, help="rich corpus manifest JSON")
    parser.add_argument("--out", required=True, type=Path, help="new immutable capture directory")
    args = parser.parse_args(argv)
    try:
        paths = capture_register_run(args.manifest, args.out)
    except CaptureRegisterError as exc:
        parser.error(str(exc))
    for role in ("baseline", "capture", "observation"):
        print(f"{role}: {paths[role]}")
    return 0
```

- [ ] Document the command, four-file layout, roughly 24-minute minimum pacing for the current 946-title manifest, retry cost, raw-evidence handling, non-signed-response trust limit and the explicit Stage 3B publication gate. State that valid captures may be `BLOCKED` and must be inspected.
- [ ] Re-run CLI, capture, publication-export and monitor-contract tests.
- [ ] Commit CLI and documentation.

```bash
git add fadden/__init__.py fadden/__main__.py fadden/capture_register.py tests/corpus/test_corpus_cli.py tests/corpus/test_live_register_capture.py BUILD.md README.md
git commit -m "docs: expose live Register capture command"
```

## Task 8: Run compatibility and release gates

**Files:**

- Modify only if a gate reveals a Stage 3A defect. Do not repair unrelated baseline failures.

- [ ] Confirm no raw capture or staging directory exists in the working tree.

Run: `git status --short && git ls-files | rg "(^|/)(register-capture|register-observation|monitor-baseline)\.json$|/evidence/sha256-"`

Expected: only intended source, test and documentation changes; no live output match.

- [ ] Run the repository's corpus unittest gate.

Run: `uv run --locked --extra dev python -m unittest discover -s tests/corpus -t . -v`

Expected: all corpus tests pass.

- [ ] Run the full pytest gate with an explicit safe temporary base because the host has a stale external pytest reparse path.

Run on this Windows host:

```powershell
$capturePytestTemp = Join-Path ([System.IO.Path]::GetTempPath()) ("tax-radar-stage3a-" + [guid]::NewGuid().ToString("N"))
uv run --locked --extra dev pytest tests --basetemp $capturePytestTemp
```

Expected: 360 existing tests plus the new Stage 3A tests pass, with the final total determined by collection rather than hard-coded into acceptance.

- [ ] Run compilation, focused lint, repository lint and type checks.

```bash
uv run --locked --extra dev python -m compileall -q .
uv run --locked --extra dev ruff check fadden/capture_register.py fadden/__main__.py fadden/__init__.py tests/corpus/test_live_register_capture.py tests/corpus/test_corpus_cli.py
uv run --locked --extra dev ruff check tax_radar_au tests
uv run --locked --extra dev mypy tax_radar_au
```

Expected: all commands exit 0.

- [ ] Build the package from the committed tree.

Run: `uv run --locked --extra dev --python 3.12 python -m build`

Expected: wheel and source distribution build successfully.

- [ ] Run whitespace, status and history checks.

```bash
git diff --check
git status --short
git log --oneline --decorate -10
```

Expected: no whitespace errors; only deliberately uncommitted acceptance-ledger evidence, if the ledger workflow keeps it local.

- [ ] Run the approved acceptance ledger and record exit codes and material output in its evidence section.
- [ ] Do not run the 946-title live capture as an automated gate. Offer its exact command as the next explicit observation step after the code and review are complete.
- [ ] Perform a final diff review against the approved Stage 3A specification, then commit any documentation-only verification record separately.
