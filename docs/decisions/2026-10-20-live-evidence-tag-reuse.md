# Decision note: reusing an existing live-evidence tag or draft

Status: accepted, 20 October 2026: option A. The workflow now fails closed and
the tests pin that behaviour. Line references below describe the code before
that change.

## Conflict

[`publish-live-evidence.yml`](../../.github/workflows/publish-live-evidence.yml)
lines 178-199, added in #118 (16 September 2026), reused an existing
`live-evidence-v2-*` tag when it pointed at `GITHUB_SHA`, and reused an existing
draft for that tag. Only a tag on another commit or a published release failed.

The [admission design](../superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md)
says the opposite:

- line 485: a pre-existing ref fails and is never moved or replaced;
- lines 503-505: the workflow never deletes, moves, repairs or reuses any tag,
  draft, release or attestation;
- lines 512-517: a later run with the same identity fails at ref creation, and a
  pre-existing tag, draft or release is a bounded failure.

[`test_live_evidence_workflow.py`](../../tests/corpus/test_live_evidence_workflow.py)
(about lines 366-373 and 442-444) pinned the workflow's reuse branches, so the
tests also disagreed with the design.

## Findings

1. **With the current identity, the reuse path almost never runs.** The tag is
   the SHA-256 of `register-observation.json`, which contains `observed_at` to
   the second (`fadden/capture_register.py` lines 150-155 and 1371). Every run,
   including a job re-run, captures again, so a retry normally gets a new tag
   and does not collide with an earlier failed attempt. The path runs only if
   identical observation bytes recur, for example after a future change that reuses a
   cached capture or drops `observed_at` from the identity.
2. **When it runs, it does not recover a partial upload.** The retry uploads
   every candidate again without `--clobber`. `gh release upload` refuses an
   existing asset name without `--clobber` (gh source behaviour; not exercised
   here), so the retry fails at the first batch that holds an already uploaded
   asset. It can recover only when no candidate asset was uploaded, including
   after creation of an empty draft.
3. **Nothing compares bytes, only counts.** The publish workflow checks the
   candidate file count before attestation (line 126) and before upload
   (line 168). It never compares an existing draft's assets with this run's
   attested files. `approve-live-evidence.yml` then publishes, which freezes the
   release, after checking only that the asset count equals `Candidates: N`, and
   that each asset is `.json`, non-empty and `uploaded`. A draft with the right
   count but different bytes passes approval.
4. **A byte check is available.** Attestation subjects carry SHA-256 digests,
   and the GitHub REST API reports a `digest` for each release asset (confirmed
   on this repository's `v0.3.0` assets, 20 October 2026).

## Options

### A. Fail closed, as designed

Delete the reuse branches: create the ref with the unconditional POST, then
`gh release create`, so any existing tag or draft fails. Change the tests to pin
that behaviour. The design needs no amendment.

- Small diff; restores conformance with the reviewed design.
- Retries still work because each run has a new tag.
- A failed attempt may leave an orphan tag and possibly a private draft for an
  operator to handle by hand, as the design already states; the workflow never
  reuses them.

### B. Amend the design to allow guarded reuse

Keep reuse, and amend the design (lines 485, 503-517) to require at least:

- the existing tag points at `GITHUB_SHA` (already checked);
- the release is still a draft (already checked);
- each candidate's SHA-256 is recorded before attestation and rechecked before
  upload, rather than its count alone;
- the remote asset list is read with digests; any asset absent from the local
  set or with a different digest fails the run;
- only missing assets are uploaded, still without `--clobber`.

This adds code, tests and a design change to the publication integrity
boundary for a path with no current trigger (finding 1). It needs a Calvino
review before implementation.

### Independent of A or B: digest check at approval

Before `gh release edit --draft=false`, the approval workflow should check each
asset's digest against the attestation, either by downloading each asset and
running `gh attestation verify`, or by matching every asset `digest` to an
attestation subject. This closes finding 3 for every draft, whether or not it
was reused.

## Recommendation

Choose **A**. Reuse contradicts the reviewed design, its stated benefit does not
hold for an identity that changes on every capture, and where it does run it
fails on partial uploads and checks no bytes. Make the approval digest check a
separate change, because count-only approval is the larger integrity gap and
remains under either option. Revisit B only if the release identity ever stops
changing between runs.
