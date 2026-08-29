# Live Federal Register capture design

**Date:** 29 August 2026

**Stage:** 3A

**Status:** Approved

**Branch base:** `feature/publication-evidence-bundle` at `093f1167956b7ad0e1df857763ca0ec08f253455`

## Purpose

Stage 3A adds a live, evidence-preserving observation path for the Commonwealth
tax legislation already represented by this repository. It answers one narrow
question: what does the Federal Register of Legislation currently report for
every title in the checked-in corpus manifest, and what exact bytes support that
answer?

This stage produces an immutable local capture and a derived monitor
observation. It does not publish a development, send an artefact to another
repository, call an AI model, schedule itself, push a branch or deploy a site.
The existing live publication path remains closed until Stage 3B can transport
the raw evidence with authenticated provenance and the publisher can admit it
independently.

## Source and rights position

The Federal Register is the authorised whole-of-government website for
Commonwealth legislation. Its documented public interface is an OpenAPI 3.0.1
REST interface at `https://api.prod.legislation.gov.au/v1/`. It is free, needs
no key, returns JSON and documents, and is expressly subject to change and
periods of reduced availability.

The Register permits programmatic reuse and asks automated users to respect its
crawl guidance. The production adapter will retain the repository's existing
minimum 1.5-second delay between Register interface requests. A future
scheduler must also follow the Register's preference for incremental work
outside 08:00 to 20:00 Australian time. Stage 3A does not add a scheduler.

The capture stores only metadata responses, not legislation text. Raw response
bytes remain evidence and are not committed by this change. Before any of
those bytes or their adaptations become public, Stage 3B must apply the
Register's prescribed attribution, link to the relevant Register page and link
to CC BY 4.0. Third-party material remains out of scope.

Primary references:

- <https://www.legislation.gov.au/help-and-resources/using-the-legislation-register/data-share-and-reuse>
- <https://api.prod.legislation.gov.au/swagger/index.html>
- <https://www.legislation.gov.au/terms-of-use>

## Decision

Add one deep module, `fadden.capture_register`, behind one production
interface:

```python
capture_register_run(
    manifest_path: str | Path,
    destination: str | Path,
    *,
    session: RegisterSession | None = None,
) -> dict[str, Path]
```

The function validates and projects the corpus manifest, queries the Register,
preserves exact successful response bytes, derives all title states, validates
the finished graph and atomically promotes one absent destination directory.
Callers receive paths for the baseline, capture manifest and derived
observation. They do not orchestrate requests, parse OData, classify states,
name evidence files or perform publication.

`RegisterSession` is the narrow seam for the true external dependency. The
production adapter owns HTTPS, retries, UTC timestamps and pacing. Tests use an
in-memory adapter returning controlled exchanges. The adapter is injectable
only as a keyword argument; request construction, validation and state
derivation remain inside the module.

The command-line interface is:

```text
python -m fadden capture_register -- <manifest.json> --out <absent-directory>
```

There is no `--live`, `--publish`, synthetic override, partial-scope flag or
overwrite flag. Running the command is unambiguously a live capture, and a
successful run is complete relative to the supplied manifest.

## Input projection

The command reads one ordinary, non-linked JSON file of at most 8 MiB. JSON is
decoded as strict UTF-8 and duplicate members are rejected at every depth. The
top-level value must be a non-empty array.

Each title must provide the existing corpus fields needed to establish the
baseline:

- `id`
- `name`
- `collection`
- `versionStart`
- `compilationNumber`, whose member is required but whose value may be a
  non-empty string or the literal `null` used by legacy unnumbered Register
  compilations
- `retrieved`
- `sourceUrl`
- optional `version_is_current`, which defaults to `true` as `finalize.py`
  already does
- `current_version_start` whenever `version_is_current` is `false`

Unrelated rich-manifest fields are ignored. Required values are validated
before the destination or a missing destination parent is created. Register
identifiers use the repository's existing Register identifier validator.
Collections remain limited to `Act`, `LegislativeInstrument` and
`NotifiableInstrument`. Identifiers must be unique. Dates must be canonical ISO
calendar dates. Source URLs must be exact HTTPS Register document URLs for the
same title and compilation date.

The module projects the rows into the existing monitor baseline shape. The
baseline `retrieved` date is the latest per-title retrieval date, while every
title retains its own retrieval date. The corpus name and source interface are
fixed constants. Stage 3A permits the projected `compilation_number` to be null
only when the mandatory rich-manifest member is explicitly null; the existing
synthetic v1-v3 monitor and publication contracts are unchanged. Baseline
titles are sorted by Register identifier and
collection, then serialised deterministically. The exact baseline bytes are
hashed and bound into the observation.

The checked-in `fadden/manifest_md.json` currently contains 946 titles: 175
Acts, 675 legislative instruments and 96 notifiable instruments. Forty-seven
of those titles have an explicit null `compilationNumber`; the Register API
uses that literal null representation for legacy unnumbered compilations. The
projection preserves the manifest fact rather than fabricating a number, then
requires the live row to corroborate it. These figures describe the current
input, not hard-coded acceptance thresholds. Future properly curated additions
must be captured automatically.

## Register requests

For each baseline title, in sorted order, the module constructs a canonical
current-version query against the exact host
`api.prod.legislation.gov.au`. It selects only the fields needed to prove the
state:

- `titleId`
- `start`
- `compilationNumber`
- `registerId`
- `isCurrent`
- `status`
- `registeredAt`

The current query uses `$top=1` and a filter binding the exact title identifier
to `isCurrent eq true`. If it returns no version, the module performs one
canonical history query for the same title, ordered by `start desc`, to
distinguish a title that is no longer in force from a failed or inconsistent
lookup.

The production adapter:

- uses the standard-library TLS trust store;
- refuses redirects rather than following a response to another host;
- uses the repository's identifying user agent;
- applies a 90-second per-socket timeout;
- makes at most three attempts;
- waits at least six seconds between retries and at least 1.5 seconds between
  Register requests; and
- returns a bounded exchange record without logging a response body.

A successful response must be HTTP 200, no larger than 256 KiB and have an
`application/json` media type. Its body must be strict UTF-8 JSON with no
duplicate members. OData envelope and selected version fields are validated
exactly. An unexpected row count, title identifier, status, current flag,
field type, date, compilation identity or chronology is not coerced.

## Immutable evidence layout

The caller supplies an absent destination. A successful capture contains
exactly:

```text
<destination>/
  monitor-baseline.json
  register-capture.json
  register-observation.json
  evidence/
    sha256-<64 lowercase hex>.json
```

Every retained HTTP 200 response is written byte-for-byte under its SHA-256
name. Equal bodies share one file. They are never parsed and reserialised for
storage. `register-capture.json` does not declare its own digest. The derived
observation declares the SHA-256 of the final capture-manifest bytes and the
baseline bytes, avoiding a self-referential hash.

`register-capture.json` has the exact top-level members `schema_version`,
`mode`, `observed_at`, `source_api`, `baseline_sha256`,
`expected_register_ids`, `complete` and `results`. Its schema is
`au-tax-register-capture.v1`, its mode is `live`, and its two digest values use
the existing `sha256:<64 lowercase hex>` representation. It contains no
publication status and makes no claim that its transport is authenticated
beyond the production HTTPS exchange.

Each capture result records:

- Register identifier and collection;
- checked-at timestamp;
- exact request role and URL;
- final HTTP status or bounded transport error category;
- attempt count;
- selected `Date`, `Content-Type`, `OData-Version` and `X-Frl-Version` headers
  when present;
- response length, SHA-256 and relative evidence path for a retained 200
  response; and
- the derived source state and any bounded consistency error category.

Non-200 bodies are not retained or logged. A malformed bounded 200 JSON body is
retained because its exact bytes explain why the observation failed closed.
No absolute local path, socket exception text, response body or arbitrary
server header enters a JSON output or console error.

All JSON generated by the module uses UTF-8, two-space indentation, fixed key
order and one trailing newline. Arrays whose order is not semantically supplied
by the source are sorted deterministically.

## State derivation

Every baseline title produces exactly one observation state:

| Evidence | State | Required interpretation |
|---|---|---|
| One registered current version with the same compilation number and date as the baseline, including matching explicit null numbers | `UNCHANGED` | The checked-in compilation still matches the Register. |
| One registered current version with a different compilation number and a strictly later compilation date | `SUPERSEDED` | A newer published compilation exists and may become a Stage 3B candidate. |
| One current version whose `registerId` is null | `CURRENT_NO_PUBLISHED_COMPILATION` | The Register reports a current version but no document is published. Never substitute an older document or create a candidate. |
| No current version and at least one same-title historical version, or one current version whose `status` is not `InForce` | `NO_LONGER_IN_FORCE` | The title has ceased or been repealed. The Register reports repeal either way: as an empty current result or on the current row itself. Stage 3A records it but does not create a public development. |
| Transport, status, media, JSON, identity, shape or chronology failure; no current or historical version; or any state outside the matrix | `LOOKUP_FAILED` | Evidence is insufficient or inconsistent. |

A current row must report `isCurrent: true`. Its `status` must be one of the
Register's four values; anything else is a shape failure. A current row whose
status is not `InForce` states that the title is no longer in force and is
recorded as `NO_LONGER_IN_FORCE`, never as a candidate, whatever else the row
carries. Live capture of 946 titles on 30 August 2026 observed four repealed
instruments reported this way, each with `isCurrent: true`, `status: Repealed`
and a null document identifier. A registered
current row must have a non-empty compilation number or the Register's literal
null for an unnumbered legacy compilation, plus a canonical start date and
Register document identifier. A null baseline and null current number can only
be `UNCHANGED` on the same date; a later current version remains fail-closed
unless it supplies a non-empty superseding number. A superseding compilation
must not move backwards or sideways in time. Any unexplained mismatch is
`LOOKUP_FAILED` with a fixed error category, not a guessed state.

`complete` means every expected Register identifier was attempted and appears
exactly once. `run_status` is `VERIFIED` only when the run is complete and no
title is `LOOKUP_FAILED`; otherwise it is `BLOCKED`. Valid
`CURRENT_NO_PUBLISHED_COMPILATION` and `NO_LONGER_IN_FORCE` states do not make
an otherwise complete capture untrustworthy, but they never generate a
superseded-compilation candidate.

The projected output uses a new
`au-tax-register-observation.v4` schema with `mode: live`. Its exact top-level
members are `schema_version`, `mode`, `observed_at`, `scope_id`,
`baseline_sha256`, `capture_sha256`, `expected_register_ids`, `complete`,
`run_status` and `observations`. The scope is
`au-primary-tax-legislation.v4`.

Each v4 observation retains the v2 state fields: `register_id`, `collection`,
`state`, `evidence_id`, `observed_compilation_number`,
`observed_compilation_date`, `observed_register_document_id`,
`current_version_start`, `evidence_url`, `checked_at` and `error_category`. It
adds `capture_result_sha256`, which binds the deterministic capture-result
object, plus nullable `primary_response_sha256` and
`primary_response_media_type` fields. The primary response is the current
query's retained HTTP 200 body. Those two fields are null only when no such
body exists. `evidence_id` is derived from the Register identifier and the
first 32 hexadecimal characters of the capture-result digest; the full digest
remains authoritative. This v4 shape does not pretend that a transport failure
has source-content bytes.

Versions 1 through 3 remain byte-compatible and keep their existing synthetic
behaviour. The v4 projection is new rather than making v3's mandatory scalar
content evidence nullable after the fact.

## Failure and publication behaviour

Manifest or filesystem preflight failure creates no destination and no missing
destination parent. Per-title source failures are evidence, so the module
continues through the remaining scope and promotes a complete audit capture
with `run_status: BLOCKED`. A process interruption before promotion leaves no
official capture; the private staging directory is removed on handled failure.

The destination, its existing ancestors and the manifest must be ordinary
filesystem objects, not symbolic links, junctions, reparse points or special
files. The destination must not exist. At most one absent, safe-named immediate
parent may be created after input validation. Inputs, staging and destination
must not alias one another. The module writes under a private sibling,
revalidates every generated file and digest, then performs one directory rename.
It removes only staging that it created and recognises. It never overwrites,
repairs or deletes a prior capture.

Stage 3A does not extend `evidence-bundle.v1` to live mode and does not change
the publisher. The existing exporter continues to accept only synthetic v3
observations. This deliberate incompatibility prevents a live observation
digest from being mistaken for independently admitted evidence. Stage 3B must
define the evidence-carrying bundle, authenticated transport, prescribed
attribution and publisher-side revalidation before any live import is possible.

## Testing

All automated tests use an in-memory Register session. The ordinary test suite
makes no network request, sleeps for no wall-clock interval and writes only to
temporary directories. Tests exercise the module through
`capture_register_run`; private parsing helpers are implementation details.

The test matrix must cover:

- exact rich-manifest projection, canonical ordering and baseline digest;
- all five source states and the verified/blocked run decision;
- current-version and history-query URL construction;
- a current version with no registered document;
- strict UTF-8, duplicate JSON members, size, media type and OData shape;
- explicit legacy null compilation numbers without invented source facts;
- wrong-title, wrong-status, duplicate-row and backwards-chronology responses;
- transport and non-200 error categorisation without body or path disclosure;
- response-byte hashing, content-addressed names, deduplication and final
  cross-file digest revalidation;
- full-scope continuation after a title failure;
- ordinary-file, containment, link, junction, reparse-point and alias checks;
- staged-write, revalidation and promotion failures with prior paths preserved;
- command-line argument handling and exit status;
- unchanged v1, v2, v3, impact-queue and synthetic publication-bundle golden
  contracts; and
- deterministic repeated output from identical exchanges and timestamps.

The implementation is complete only when repository-defined unittests, pytest,
Ruff, mypy and package build pass, the new acceptance ledger passes, and the
working tree contains no raw live capture. A real 946-title capture is an
explicit post-implementation observation run, not a test prerequisite and not
publication authority.

## Alternatives rejected

### Retrofit `check_current.py`

This is shorter but preserves neither response bytes nor a run graph, exposes
classification through console prose and cannot support independent downstream
admission. It remains useful as a human-readable diagnostic but is not the
credibility seam.

### Generate a live publication bundle directly

This would let the same process fetch, interpret and declare its own evidence
verified while sending only a digest downstream. Authentication would prove who
made the assertion, not that the publisher received and checked the bytes. Live
publication remains closed until Stage 3B.

### Add a database, queue or hosted scheduler now

None is required to prove live source capture. An immutable directory and Git
history are sufficient at this stage. Hosting and scheduling would expand the
operational and credential surface before the evidence contract is proven.

## Known trust limit

The Register does not cryptographically sign each metadata response. Stage 3A
preserves what the production environment received over authenticated HTTPS and
makes later alteration detectable; it does not create a government signature.
Stage 3B can authenticate the producing workflow and let the publisher
revalidate the evidence, but it must describe this limit honestly rather than
claim mathematical proof of source authorship.
