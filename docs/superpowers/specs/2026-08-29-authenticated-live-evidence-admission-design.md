# Authenticated live evidence admission design

**Date:** 29 August 2026

**Stage:** 3B

**Status:** Approved amended design; implementation and hosted activation remain separate

**Producer base:** `feature/live-register-capture` at
`20c489450a279f032ed78a290bb0fb76602a5522`

**Publisher base:** local `main` at
`07fcdf82c924873daa18cc3217419f3cba8468fd`

## Purpose

Stage 3B opens one narrow, authenticated path from a complete live Federal
Register capture to a factual source-only development for the Australian Tax
Intelligence publisher. It carries enough evidence for the publisher to prove
the one compilation event it will publish without transporting or replaying all
946 title lookups.

The flow is:

```text
complete Stage 3A capture with run_status VERIFIED
  -> one evidence-bundle.v2 per SUPERSEDED title
  -> one attested immutable GitHub Release for the capture
  -> independent publisher provenance and evidence admission
  -> one atomic development.v2 record per admitted bundle
  -> existing static HTML, RSS and JSON Feed build
```

Credibility remains the controlling requirement. An incomplete run, a failed
lookup, an unsupported source state, missing provenance, inconsistent bytes or
an ambiguous fact produces no public record. A run with no `SUPERSEDED`
observations succeeds privately and creates no release.

This design follows the [Stage 3A live-capture contract](2026-08-29-live-register-capture-design.md).
It does not change that contract's five source states or weaken its immutable
capture graph.

## Scope

### Included

- a live-only, per-development `evidence-bundle.v2` contract;
- deterministic production of those bundles from a validated Stage 3A graph;
- one manually dispatched, self-contained producer workflow;
- GitHub immutable Release transport and artifact attestations;
- three distinct native GitHub provenance checks in the publisher;
- independent publisher recomputation of the one source fact being admitted;
- deterministic transformation to the existing `development.v2` canonical
  record;
- the bounded `development.v2` compatibility corrections exposed by live
  Stage 3A data;
- factual presentation of a newly registered compilation in HTML and feeds;
- Federal Register attribution for adapted metadata; and
- local tests, golden conformance bytes and activation gates.

### Excluded

- scheduled monitoring or automatic retries;
- automatic publisher downloads, commits, pull requests, merges or deployment;
- baseline advancement or correction of an accepted development;
- a database, queue, webhook, cross-repository service or runtime API;
- AI technical explainers, impact analysis, topics or practice-area inference;
- ATO, TPB, Treasury, court, tribunal, AASB, AUASB, ASIC or APRA sources;
- CPD, personalised advice or registered tax practitioner review;
- a real 946-title capture, live release, live import or public deployment during
  implementation; and
- any claim that GitHub provenance is a government signature.

Stage 3B is deliberately manual after the producer release. An operator
downloads a candidate, imports it locally, inspects the resulting source-only
page and builds the site. Every further automation boundary requires its own
design and approval.

## Trust statement

The Federal Register does not cryptographically sign each metadata response.
Stage 3A preserves the exact bytes received over authenticated HTTPS. Stage 3B
adds proof that a specific protected GitHub workflow in
`ryanduguid/au-tax-legislation-corpus`, running from `refs/heads/main` on a
GitHub-hosted runner, produced the candidate bytes and attached them to the
identified immutable release.

That proves workflow provenance and later byte integrity. It does not prove
that the Commonwealth authored the bytes, that the workflow was logically
correct, or that substantive law changed. The publisher therefore parses the
retained response again and independently proves only this bounded claim:

> The Federal Register response captured for this title reports a registered,
> in-force current compilation with a non-null compilation number, a different
> Register document identifier and a later compilation date than the captured
> baseline.

The publisher does not re-fetch the Register. It cannot independently establish
the completeness of all 946 lookups from a per-development bundle. It accepts
that run-level assertion only from the exact attested workflow, then verifies
the individual source event from the included raw response bytes.

## Architecture and ownership

### Producer

The producer continues to own:

- the fixed tax-legislation manifest and complete-scope capture;
- Federal Register request construction, pacing and raw response preservation;
- the five Stage 3A lifecycle states;
- validation of the complete baseline, capture and observation graph;
- selection of `SUPERSEDED` candidates only after the run is `VERIFIED`;
- byte-stable `evidence-bundle.v2` production; and
- the manual release workflow and its source attribution notes.

The producer never creates publisher content, decides practical impact or calls
an AI model.

### Publisher

The publisher treats the downloaded file, its JSON and all GitHub command
output as untrusted. It owns:

- snapshotting and hashing the exact downloaded bytes;
- immutable-release, release-membership and workflow-provenance checks;
- strict bundle, Stage 3A object and raw OData validation;
- recomputation of all candidate-level digests and relationships;
- deterministic `development.v2` transformation;
- fixed-root, atomic, idempotent admission with conflict refusal; and
- restrained public language and links.

The production boundary is intentionally deep: one live bundle path enters and
either one canonical development is atomically admitted or nothing changes.
Provenance orchestration, JSON parsing, OData interpretation, transformation
and filesystem staging remain behind that boundary. Live-v2 parsing,
provenance, transformation and admission form one dedicated module. The
unchanged synthetic-v1 parser and transformation remain a separate conformance
path; there is no shared production "import any bundle" mode switch. Shared
strict-JSON and scalar-validation primitives are permitted.

The production module exposes one fixed-root live import operation. Tests may
substitute process and filesystem adapters only through internal test seams;
no production command or programmatic export accepts an alternate content
root, skips provenance, enables synthetic input, selects a mode or permits an
overwrite or retry.

### Why a per-development envelope

A whole-capture bundle would force the publisher to transport and replay 946
unrelated lookups to publish one factual event. An ordinary Actions artifact
would expire. A custom receipt or checksum sidecar would duplicate facts that
the candidate, Stage 3A digests and GitHub attestations already bind.

One bounded candidate per `SUPERSEDED` title keeps the independent verification
surface small while the producer still refuses to emit anything from an
incomplete or blocked run. One immutable release groups every candidate from
the same verified observation without turning that group into a publisher
batch transaction.

## `evidence-bundle.v2`

### Envelope

The bundle is one ordinary UTF-8 JSON file no larger than 1 MiB. Duplicate
members, a byte-order mark, malformed UTF-8, unknown members and non-canonical
scalar forms fail. The exact top-level shape is:

```json
{
  "schema_version": "evidence-bundle.v2",
  "producer": {
    "name": "tax-radar-au",
    "version": "0.1.3"
  },
  "run": {
    "observed_at": "2026-08-29T00:00:00Z",
    "scope_id": "au-primary-tax-legislation.v4",
    "complete": true,
    "run_status": "VERIFIED",
    "baseline_sha256": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    "observation_sha256": "sha256:2222222222222222222222222222222222222222222222222222222222222222"
  },
  "baseline_title": {},
  "capture_result": {},
  "observation": {},
  "rights": {
    "mode": "metadata-only",
    "attribution": "Based on content from the Federal Register of Legislation at 2026-08-29. For the latest information on Australian Government legislation please go to https://www.legislation.gov.au. Changes: selected and reformatted Federal Register metadata into a bounded evidence bundle and factual source update; no legislation text is reproduced.",
    "licence_url": "https://creativecommons.org/licenses/by/4.0/"
  },
  "primary_response_base64": "e30K"
}
```

The example shows the envelope and is not a conformance fixture. The checked-in
golden fixture will contain complete nested objects and mutually valid hashes.
The producer emits fixed key order, two-space indentation, LF line endings and
one trailing newline, but the publisher's structural parser does not treat JSON
member order as meaning.

`schema_version` is exactly `evidence-bundle.v2`. There is no top-level `mode`:
this schema is live-only. There is no whole-capture digest because the publisher
does not receive the whole capture manifest. There is no bundle-declared digest
of its own bytes; the publisher calculates that digest from its exact snapshot.

### Producer and run receipt

`producer` has exactly `name` and `version`:

- `name` is exactly `tax-radar-au`;
- `version` is 1 to 100 ASCII characters and a strict Semantic Version 2.0.0
  string; and
- admission does not hard-code `0.1.3`, because workflow identity and protected
  source are the authentication boundary.

`run` has exactly:

- `observed_at`, copied from the Stage 3A observation;
- `scope_id`, exactly `au-primary-tax-legislation.v4`;
- `complete`, exactly `true`;
- `run_status`, exactly `VERIFIED`;
- `baseline_sha256`, the digest of the exact Stage 3A
  `monitor-baseline.json` bytes; and
- `observation_sha256`, the digest of the exact Stage 3A
  `register-observation.json` bytes.

Both digests use `sha256:` followed by 64 lowercase hexadecimal characters.
The release tag is
`live-evidence-v2-<observation_sha256-without-the-sha256-prefix>`.

### Rights

`rights` has exactly `mode`, `attribution` and `licence_url`:

- `mode` is exactly `metadata-only`;
- `licence_url` is exactly
  `https://creativecommons.org/licenses/by/4.0/`; and
- `attribution` is exactly the changed-content wording below, with
  `<capture-date>` replaced by the literal ISO calendar component of
  `observation.checked_at`.

```text
Based on content from the Federal Register of Legislation at <capture-date>.
For the latest information on Australian Government legislation please go to
https://www.legislation.gov.au. Changes: selected and reformatted Federal
Register metadata into a bounded evidence bundle and factual source update; no
legislation text is reproduced.
```

The producer derives this object rather than accepting caller wording. The
publisher independently reconstructs it from the validated observation and
requires exact equality before carrying the object into canonical source
rights. The object is part of the attested immutable asset, so mutable release
notes are not its sole durable attribution.

### Embedded Stage 3A objects

`baseline_title` is one exact Stage 3A baseline title. It has exactly:

- `register_id`;
- `name`;
- `collection`;
- `compilation_number`, which may be a non-empty string or `null`;
- `compilation_date`;
- `version_is_current`;
- nullable `current_version_start`;
- `retrieved`;
- `source_url`; and
- `register_page`.

`capture_result` is the exact matching Stage 3A result. It has exactly
`register_id`, `collection`, `checked_at`, `state`, `error_category` and
`requests`. Each request has exactly:

- `role`;
- `url`;
- `checked_at`;
- `http_status`;
- `transport_error_category`;
- `attempt_count`;
- `response_headers`;
- `response_length`;
- `response_sha256`; and
- `evidence_path`.

For an admissible candidate there is exactly one request, its role is
`current`, its status is 200, it has no transport error, and it declares the
retained primary response.

`observation` is the exact matching Stage 3A v4 observation item. It has
exactly:

- `register_id`;
- `collection`;
- `state`;
- `evidence_id`;
- `observed_compilation_number`;
- `observed_compilation_date`;
- `observed_register_document_id`;
- `current_version_start`;
- `evidence_url`;
- `checked_at`;
- `error_category`;
- `capture_result_sha256`;
- `primary_response_sha256`; and
- `primary_response_media_type`.

It must be a valid `SUPERSEDED` item: the three observed compilation fields are
non-null, `current_version_start` and `error_category` are null, and the primary
media type is exactly `application/json`.

The producer and publisher both reconstruct the capture-result bytes with the
Stage 3A canonical JSON rules before checking `capture_result_sha256`. Copying a
digest field without reproducing those bytes is insufficient.

### Primary response bytes

`primary_response_base64` is canonical RFC 4648 base64 using the standard
alphabet, required padding and no whitespace. Re-encoding the decoded bytes
must reproduce the string exactly. The decoded response is no larger than
256 KiB.

The decoded length and SHA-256 must equal the current request's
`response_length` and `response_sha256`. The content-addressed evidence path,
retained content type and the observation's primary-response fields must agree
with the same bytes. The producer does not parse and reserialise those bytes for
the envelope.

### Deterministic identity and filename

The candidate identity is derived, never supplied as a second assertion:

```text
development_id = dev-frl-<register_id-lower>-<current-document-id-lower>
bundle_id      = bundle-frl-<register_id-lower>-<current-document-id-lower>-r1
source_id      = frl-<current-document-id-lower>
```

The release asset filename is `<bundle_id>.json`. The filename is for stable
operator handling only. The publisher derives all identities from validated
content and never accepts a fact because its filename looks correct.

### Producer generation rules

The live exporter accepts one immutable Stage 3A capture directory and an
absent output directory. It revalidates the complete graph before creating an
output or missing output parent. It requires:

- v4 live observation and v1 live capture schemas;
- the fixed v4 scope;
- exact baseline, capture and observation cross-file digests;
- every expected Register identifier exactly once;
- `complete: true` and `run_status: VERIFIED`;
- no `LOOKUP_FAILED` result; and
- every referenced evidence file to match its declared length and digest.

After full-graph validation, `UNCHANGED`,
`CURRENT_NO_PUBLISHED_COMPILATION` and `NO_LONGER_IN_FORCE` produce no
candidate. Each `SUPERSEDED` item produces one v2 bundle. Any candidate-level
inconsistency or size breach fails the whole export; it must not silently omit
that candidate. Derived bundle identities and filenames must be unique across
the run; any collision also fails the whole export.

For each candidate the exporter derives the exact rights object from that
observation's `checked_at`. It does not accept rights wording, a capture date or
a licence URL from its caller. Different candidates in one run may therefore
carry different attribution dates.

The exporter captures each input file once and revalidates every final bundle.
Its official-output transaction is supported only on Windows. There it builds
in an owned private sibling, holds the output parent and staging identities,
and promotes the exact held staging directory with one identity-bound,
no-replace rename. Cleanup likewise targets only the held staging object and
its recorded children. On other platforms it fails closed before creating a
missing output parent or staging directory because the available unprivileged
directory operations cannot bind both final promotion and cleanup to that held
identity. It never falls back to pathname-only mutation and never overwrites,
repairs or deletes a prior output. On Windows, a verified run with zero
candidates produces an empty private candidate set and returns success so the
workflow can end without creating a release.

The producer version comes from the trimmed checked-in `VERSION` file. Export
fails unless it is strict SemVer and equals the package version in
`pyproject.toml`; this retains one release value without pinning the publisher
to a particular version.

`evidence-bundle.v1` remains byte-compatible and synthetic-only. It continues
to test parser and transformation conformance in its existing separate path,
but it is not accepted by any live-v2 parser, transformer or production import
operation and gains no live mode.

## Producer workflow

### Trigger and runner

Add `.github/workflows/publish-live-evidence.yml` as a self-contained workflow.
It has only `workflow_dispatch` and accepts no inputs. The single job:

- refuses to run unless `github.repository` is exactly
  `ryanduguid/au-tax-legislation-corpus` and `github.ref` is exactly
  `refs/heads/main`;
- runs only on the GitHub-hosted `windows-latest` runner;
- has a 120-minute timeout;
- uses concurrency group `publish-live-evidence-v2` with
  `cancel-in-progress: false`;
- checks out without persisted credentials;
- uses the checked-in `fadden/manifest_md.json`; and
- receives no API key or caller-selected manifest, scope, destination, tag or
  release name.

Every action reference uses a reviewed full commit SHA. The implementation may
reuse the repository's reviewed full-SHA pins for checkout and Python setup.
The `actions/attest` pin is reviewed before activation. Floating tags and
third-party release actions are not accepted.

All `run` steps use PowerShell (`pwsh`). Runner-private paths are constructed
with `Join-Path` from `$env:RUNNER_TEMP`, step outputs are appended through
`$env:GITHUB_OUTPUT`, and release assets are enumerated with `Get-ChildItem`
rather than relying on Bash syntax or shell wildcard expansion.

### Permissions

The job declares only:

```yaml
permissions:
  attestations: write
  contents: write
  id-token: write
```

The workflow sets the `GH_TOKEN` environment variable only on the two
release-command steps. Other shell steps do not receive that environment
variable; checkout and attestation use only the credentials their actions need
under the declared permissions. Checkout credentials are not persisted.

### Ordered operation

The workflow performs these steps in order:

1. Check out the exact `main` revision and set up the locked Python toolchain.
2. Run the complete 946-title Stage 3A capture under `$env:RUNNER_TEMP`.
3. Revalidate the graph and require `complete: true` and `VERIFIED`.
4. Generate all deterministic v2 candidates into one private directory.
5. If the directory contains no candidates, finish successfully without a tag,
   attestation or release.
6. Require no more than 1,000 candidates, then invoke `actions/attest` once over
   all candidate paths. One provenance statement contains each candidate as an
   independently verifiable subject. The lower limit comes from GitHub Releases;
   the action itself permits 1,024 subjects. The present 946-title scope fits
   both limits even if every title is superseded. Future scope growth fails
   closed rather than splitting one run across releases or attestations without
   a new design.
7. Create one draft release at the deterministic observation-hash tag, upload
   all candidate assets together and attach deterministic attribution notes.
   The release title is the tag and its target is the exact checked-out
   `GITHUB_SHA`, not a later resolution of the moving branch name.
8. Publish the draft with `latest: false`. Publishing under the repository's
   immutable-releases setting freezes the release and creates the GitHub release
   attestation used by `gh release verify`.

The workflow must never publish before every candidate is uploaded and the
artifact attestation succeeds. A pre-publication failure creates no public
release. GitHub may retain a private failed draft; the workflow reports it and
does not automatically delete it. Deleting or repairing that draft is an
explicit operator action.

A pre-existing tag, draft or release for the deterministic identity is a
bounded failure. The workflow does not modify it or treat a new run as a
replacement.

### Release notes and rights

The deterministic release notes state the capture start time, candidate count,
source-only nature and that every asset contains its own durable Federal
Register attribution dated to that response's retrieval:

```text
Capture started: <run.observed_at>
Candidates: <candidate-count>
Source-only evidence: no legislation text is reproduced. Each immutable asset
contains the Federal Register attribution for its own response retrieval date.
```

The workflow derives the actual timestamp and count. It does not accept
free-form release notes. Release notes are an aggregate human summary, not the
sole rights record and not a replacement for the exact object in every asset.
The workflow attaches no full corpus, full capture directory, checksum
manifest, SBOM, separate run receipt or custom asset index.

### Activation prerequisites

Before the workflow is enabled or manually run on GitHub:

1. Enable immutable releases for the producer repository. This affects future
   releases, so it must precede the first live evidence release.
2. Protect `main` with pull-request review, the existing required CI checks,
   force-push and deletion prevention, and no routine bypass.
3. Review every full action SHA, including `actions/attest`.
4. Merge the workflow through the protected branch.

The existing reusable software-release workflow remains separate. Reusing it
would make the reusable workflow the attestation signer and would break the
publisher's exact local-workflow identity policy.

## Publisher admission

### Production command

The only production command is:

```text
npm run import-bundle -- --bundle <file>
```

`content/developments` is fixed inside the repository. There is no production
`--content-root`, synthetic flag, provenance skip, offline-trust flag,
overwrite flag or retry flag. `evidence-bundle.v1` and every schema other than
v2 are rejected by this command.

The command and its one supported programmatic operation resolve the fixed
root internally. No exported production function accepts another root, a
mode, synthetic input, alternate process or filesystem operations, a
provenance verifier, offline trust, overwrite or retry behaviour. Internal
test seams are not a supported import API and cannot be reached through this
command.

The command requires an authenticated GitHub CLI at version 2.98.0 or later and
checks that the exact verification commands and flags are available. That floor
includes the 2026 security fixes affecting attestation and release
verification. An older or incompatible client, missing authentication, command
absence, rate limiting or network failure is an admission failure, not
permission to continue locally.

### Snapshot and provenance order

Before any canonical content change, the importer:

1. resolves the input and fixed content root and rejects links, junctions,
   reparse points, special files, aliases and unsafe ancestors;
2. creates only an importer-owned private staging directory under the fixed
   content root;
3. reads the bounded input once, writes an exact private snapshot, re-reads it
   and checks byte equality;
4. strictly parses enough of the snapshot to validate the v2 envelope and
   derive the observation-hash release tag;
5. invokes the three provenance checks below against that snapshot without a
   shell or string-built command; and
6. only after all three succeed, performs complete semantic admission and
   transformation.

The three checks remain distinct because they enforce different properties:

```text
gh release verify <tag> \
  --repo ryanduguid/au-tax-legislation-corpus

gh release verify-asset <tag> <snapshot-path> \
  --repo ryanduguid/au-tax-legislation-corpus

gh attestation verify <snapshot-path> \
  --repo ryanduguid/au-tax-legislation-corpus \
  --signer-workflow ryanduguid/au-tax-legislation-corpus/.github/workflows/publish-live-evidence.yml \
  --source-ref refs/heads/main \
  --predicate-type https://slsa.dev/provenance/v1 \
  --deny-self-hosted-runners
```

The first verifies the immutable release attestation, the second proves that
the exact snapshot is an asset of that release, and the third constrains the
artifact's producing repository, signer workflow, source ref, predicate and
runner class. No hand-written asset inventory or filename policy replaces any
of them.

Child-process output is bounded and mapped to stable error categories. Tokens,
arbitrary exception text and full command output do not enter canonical content
or public errors. The importer performs no automatic retry.

### Independent evidence checks

After provenance succeeds, the publisher validates the complete envelope and
recomputes the candidate fact:

- strict UTF-8 JSON, duplicate-member refusal, exact member sets and all size,
  text, identifier, date, timestamp, URL, digest and SemVer constraints;
- exact producer name, v4 scope, verified complete run receipt and release tag;
- baseline, result and observation identity, collection and timestamp
  agreement;
- exact official Register URLs for the title and canonical current-query URL;
- canonical capture-result bytes, digest and evidence identifier;
- canonical base64, decoded byte length, content digest, evidence path, media
  type and cross-object primary-response declarations;
- strict raw OData JSON with exactly the Stage 3A context and row fields;
- exactly one row for the same title, with `isCurrent: true`, status `InForce`,
  a valid non-null current document identifier and a non-empty compilation
  number;
- a current compilation date strictly later than the baseline date and a
  compilation number different from the nullable baseline number;
- raw current identifiers and the raw `start` compilation date equal every
  observation declaration;
- a valid non-null `registeredAt` timestamp, with its literal ISO calendar
  component retained as a separate registration date rather than compared with
  or substituted for the raw `start` compilation date;
- exact rights members, mode, licence URL and changed-content attribution,
  independently reconstructed from the validated `observation.checked_at`; and
- no state or language beyond `SUPERSEDED` and the bounded compilation fact.

Any mismatch abstains. The publisher does not query the Federal Register to
repair or enrich the bundle.

The `registeredAt` parser implements the Stage 3A timestamp grammar directly,
including up to seven fractional digits and an optional numeric offset. It
extracts the calendar component exactly as written and does not rely on
JavaScript `Date` to accept, truncate or timezone-normalise that source
representation. The registration date must not be later than the UTC calendar
date of `observation.checked_at`; an offset-less timestamp that would require a
timezone assumption abstains instead.

### Atomic admission and idempotency

The importer calculates `upstream.bundle_sha256` from the exact private
snapshot, builds one canonical record in owned staging, re-reads and validates
the exact bytes, removes the private snapshot, and confirms staging contains
only the canonical `development.json` before one directory rename.

If the final development directory is absent, the rename admits it. If it is an
ordinary directory containing exactly the same canonical bytes, import returns
`unchanged`. Any other existing path, extra entry or byte difference is a
conflict. Nothing is overwritten, merged, repaired or deleted.

Every failure before promotion removes only recognised importer-owned staging
and leaves `content/developments` byte-for-byte unchanged. Cleanup failure is
reported rather than hidden. Error messages are bounded, deterministic and
non-zero at the command line.

## Canonical `development.v2` transformation

`development.v2` remains the only canonical imported record; a v3 is not
introduced. The live mapping is deterministic:

| Canonical field | Source or fixed value |
|---|---|
| `schema_version` | `development.v2` |
| `development_id` | derived live identity |
| `mode` | `live` |
| `title` | `baseline_title.name` |
| `authority_status` | `in-force` |
| `evidence_status` | `verified` |
| `publication_status` | `source-only` |
| `published_at` | literal `registeredAt` calendar date at midnight UTC |
| `effective_at` | `null` |
| `topics` | empty array |
| `affected_practice_areas` | empty array |
| `explainer` | `null` |
| `revision.number` | `1` |
| `revision.updated_at` | `observation.checked_at` |
| `revision.change_note` | `Initial source-only record from authenticated Federal Register compilation evidence.` |
| `revision.replaces_bundle_id` | `null` |
| `upstream.bundle_id` | derived live bundle identity |
| `upstream.bundle_sha256` | publisher hash of exact v2 bytes |
| `upstream.generated_at` | `observation.checked_at` |
| `upstream.producer.name` | bundle producer name |
| `upstream.producer.version` | validated SemVer |
| `upstream.producer.baseline_sha256` | `run.baseline_sha256` |
| `upstream.producer.observation_facts_sha256` | `run.observation_sha256` |

`source_event` is exactly `compilation-superseded`. It carries the baseline
Register identifier and collection, the previous compilation number and date,
and the observed current compilation number, compilation date and Register
document identifier. The current compilation date remains the calendar date
from raw `start`; it is distinct from the registration-derived
`published_at`.

Live evidence exposes four bounded compatibility rules that the synthetic v1
projection could not establish:

- `source_event.previous_compilation.number` may be `null`; 47 checked-in titles
  have legitimate unnumbered legacy compilations;
- `title` and the matching source title allow up to 500 characters; the current
  manifest contains an official 240-character title, beyond the old synthetic
  bound of 200;
- live `source.evidence_id` must equal the Stage 3A grammar
  `frl:<register-id>:<32-lowercase-hex>`, including its colons; and
- the prescribed changed-content attribution allows up to 500 characters.

The current compilation number remains mandatory and non-empty. A null
previous number renders as “No compilation number recorded”. Other text and
identifier bounds remain unchanged. The live v2 validator applies these rules
directly instead of projecting the record through the synthetic v1 bundle
validator. This corrects the still-unlaunched live contract without changing
the v1 parser or introducing a new canonical schema version.

Generation and revision times use the per-title `checked_at`, not the run's
earlier `observed_at`. This preserves the canonical rule that generation cannot
predate source retrieval. The registration-derived `published_at` is a
date-level projection at midnight UTC and must not be later than the UTC
calendar date of `checked_at`; no timezone is invented to make an ambiguous
source timestamp pass.

The single source is:

| Source field | Value |
|---|---|
| `source_id` | derived current-document source identity |
| `publisher` | `Federal Register of Legislation` |
| `document_class` | `legislation` |
| `title` | baseline title name |
| `canonical_url` | exact official `baseline_title.register_page` |
| `published_at` | literal `registeredAt` calendar date at midnight UTC |
| `retrieved_at` | `observation.checked_at` |
| `evidence_id` | Stage 3A observation evidence identity |
| `content_sha256` | primary response digest |
| `content_kind` | `metadata-response` |
| `content_media_type` | `application/json` |
| `rights` | exact independently reconstructed bundle rights object |
| `evidence` | empty array |

The publisher independently reconstructs the exact bundle rights object from
the ISO calendar date of `observation.checked_at`, requires equality with the
attested object and carries that validated object into the canonical source.
It does not generate a second canonical-only wording.

The immutable evidence release URL is derived from the fixed producer
repository and `run.observation_sha256`; no second mutable URL field is needed
in the canonical record.

The v1 synthetic bundle and its golden transformation remain unchanged for
conformance. Synthetic parsing and transformation tests do not invoke the live
parser, provenance or filesystem-admission path. The live production command
and its supported programmatic operation receive no runtime `allowSynthetic`
escape hatch or generic mode switch.

## Public presentation

The headline is exactly:

```text
New compilation: <official title>
```

The detail view shows:

- the previous compilation number, or “No compilation number recorded”;
- the current compilation number;
- “Registered” with the literal calendar date derived from raw `registeredAt`;
- the distinct current compilation date derived from raw `start` as factual
  compilation detail;
- the existing `in-force`, `verified` and `source-only` status language;
- the official Federal Register title page as the primary action; and
- the immutable GitHub evidence release as a secondary evidence link.

The supporting sentence is exactly “The Federal Register reports a newer
registered current compilation for this title.” It must not say that
legislation was amended, commenced or substantively changed, and it must not
infer practitioner impact.

Empty topic and practice-area sections are omitted. No explainer shell is shown.
Raw response bytes remain in the release bundle, not the website. Opaque hashes
are available through the evidence release but are not displayed by default.

The homepage, development page, RSS and JSON Feed use the same source-only
identity, headline, registration-derived publication date and restrained
claim. The compilation date remains separate factual detail. The machine feeds
must not add a stronger summary than the HTML page.

## Workflow and import failures

The following all produce no new canonical content:

- a Stage 3A run that is blocked, incomplete or structurally inconsistent;
- any candidate-generation failure;
- zero candidates, which is a successful abstention and no release;
- an existing deterministic release identity;
- a failed release, release-asset or artifact-attestation check;
- a missing or incompatible `gh` CLI, authentication or network access;
- malformed, oversized, non-canonical or internally inconsistent bundle data;
- raw response bytes that do not independently prove the declared event;
- unsafe input, staging or content-root filesystem shapes;
- staged-byte changes, write failure, race or promotion failure; and
- an existing same-identity development that is not byte-identical.

There is no automatic retry. A later manual attempt starts from a deliberate
operator decision and still cannot overwrite either an immutable release or an
accepted development.

## Testing and acceptance

### Producer tests

The producer test surface covers:

- exact v2 envelope, nested Stage 3A shapes and unknown-member refusal;
- exact per-candidate rights derivation, attribution date, wording and licence;
- strict SemVer, UTC timestamp, identifier, URL, digest, size and base64 rules;
- reconstruction of capture-result bytes and response evidence relationships;
- complete-graph gating before candidate selection;
- every Stage 3A state, including zero candidates and mixed valid states;
- whole-export failure when any `SUPERSEDED` candidate is inconsistent;
- deterministic bundle identities, filenames, bytes and ordering;
- Windows absent-output, link, race, exact cleanup and identity-bound promotion
  behaviour, plus non-Windows refusal before output mutation;
- unchanged v1 bytes and existing Stage 3A contracts; and
- a byte-identical v2 golden fixture shared with the publisher.

Workflow policy tests inspect the checked-in YAML and require the exact manual
trigger, no inputs, repository/ref guard, `windows-latest`, PowerShell run
steps and Windows-safe path/output handling, 120-minute timeout, concurrency
policy, minimal permissions, full action SHAs, fixed manifest, attest-once
behaviour, zero-candidate exit, draft-before-publish ordering, deterministic
tag, aggregate rights summary and `latest: false`.

### Publisher tests

The publisher test surface covers:

- the exact three `gh` argument vectors and every individual command failure;
- bounded child-process output and no shell invocation;
- strict snapshot parsing, exact byte hashing and release-tag derivation;
- every v2 member, size, base64, digest and cross-object relation;
- independent strict OData parsing and source-event recomputation;
- valid differing `start` and `registeredAt` calendar dates, including a
  numeric-offset form, with no timezone conversion or extra source request;
- preservation of the raw `start` date as compilation detail and derivation of
  canonical and public publication dates from the literal `registeredAt`
  calendar component;
- missing, unknown or altered rights members, incorrect attribution dates,
  wording and licence URLs, and exact canonical rights carry-through;
- null previous compilation numbers and mandatory current numbers;
- exact canonical mapping, chronology, attribution and evidence-release link;
- restrained HTML, RSS and JSON Feed rendering;
- fixed content root and absence of production bypasses;
- isolation of synthetic-v1 conformance from live-v2 parsing, provenance and
  filesystem admission;
- every validation, process and filesystem failure leaving content unchanged;
- exact re-import as a no-op and every changed same-identity import as a
  conflict;
- existing v1 synthetic parser/transformation conformance; and
- byte equality with the producer's v2 golden fixture.

The final local gates retain every Stage 3A repository test, compile, Ruff,
mypy and Python 3.12 package-build check. The publisher must pass its defined
`npm run check` and `npm run smoke` commands, including deterministic HTML, RSS
and JSON Feed output from an isolated admitted fixture. The two repositories'
golden fixture bytes and SHA-256 must match exactly.

### Hosted checks requiring separate authority

Local implementation does not verify hosted settings or make a live source
claim. After separate approval, activation proceeds in this order:

1. Confirm immutable releases and protected-branch settings on GitHub.
2. Push and merge the reviewed implementation through required CI.
3. Manually run one full 946-title live capture workflow.
4. If and only if it emits a candidate, verify the immutable release and import
   one downloaded bundle locally.
5. Build and inspect every public representation of that development.
6. Authorise deployment separately.

No later step is implied by approval of an earlier one.

## Alternatives rejected

### Actions artifacts

Public Actions artifacts expire and therefore cannot remain the durable
evidence link behind a professional update.

### One whole-capture publisher bundle

It would move large unrelated evidence and make each admission depend on replay
of 946 titles. The producer's protected workflow is the run-completeness trust
boundary; the publisher verifies the one event it presents.

### Publisher re-fetch of the Federal Register

A later response can differ from the bytes that triggered the event, and a
second network dependency adds ambiguity rather than proving the original
observation.

### Reuse of the software-release workflow

The reusable workflow would become the signer identity. A dedicated local
workflow gives the publisher one exact, narrow provenance policy and does not
mix source evidence with package releases.

### Custom checksums, release manifests or receipt sidecars

The envelope, exact-byte publisher hash, Stage 3A digests and GitHub
attestations already cover those relationships. More sidecars add consistency
failure modes without another trust property.

### Automatic cross-repository publication

Automatic download, commit, pull request and deployment would combine several
new authority boundaries before the first live evidence path is observed. It
also needs a safe baseline-advancement and deduplication design.

### `development.v3`

The required canonical changes are a nullable previous compilation number and
correct timestamp derivation in an unlaunched live path. They do not justify a
parallel canonical record or renderer.

## Primary operational references

- [Federal Register data sharing and reuse](https://www.legislation.gov.au/help-and-resources/using-the-legislation-register/data-share-and-reuse)
- [Federal Register terms of use](https://www.legislation.gov.au/terms-of-use)
- [GitHub immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)
- [GitHub release asset limits](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases)
- [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
- [`actions/attest`](https://github.com/actions/attest)
- [`gh release verify`](https://cli.github.com/manual/gh_release_verify)
- [`gh release verify-asset`](https://cli.github.com/manual/gh_release_verify-asset)
- [`gh attestation verify`](https://cli.github.com/manual/gh_attestation_verify)
- [GitHub CLI 2.98.0](https://github.com/cli/cli/releases/tag/v2.98.0)

## Known limits and future work

- The signed workflow remains software controlled by repository maintainers;
  protected review reduces but does not remove that trust.
- Release immutability prevents in-place mutation but does not make GitHub an
  archival service. An authorised deletion or repository outage can remove the
  evidence link, although an immutable tag name cannot later be reused. A
  separately verified archival mirror can be designed after the first live
  path is proven.
- GitHub protects immutable release assets and tags, not the human-readable
  release title and notes. Every v2 asset therefore carries its exact durable
  rights object; release notes remain an aggregate human summary, and the
  publisher independently verifies and carries the same rights into its
  canonical record.
- GitHub availability and authenticated CLI access are required for admission.
- Official producer output publication is currently Windows-only. Supporting
  another platform requires an equally identity-bound promotion and cleanup
  design; pathname-only fallback remains prohibited.
- The checked-in corpus selection is keyword-based and is not all Australian
  tax law or accounting material.
- Stage 3B publishes only new Federal Register compilations. Other source
  families need their own evidence and rights designs.
- No accepted-record correction or revision path exists. Conflicts abstain.
- The baseline is not advanced automatically. Re-running against a stale
  baseline may create another release for the same current document, while the
  publisher correctly refuses to overwrite the existing development.
- For that reason this workflow must not be scheduled until baseline
  advancement and release-level deduplication are separately designed.
- Source-only publication is not a technical explainer. AI explainers remain a
  later, separately gated stage and must never backfill unsupported source
  claims.
