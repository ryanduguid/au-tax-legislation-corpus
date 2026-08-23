# Releasing the builder

The repository releases the build code only. A GitHub release must never be described as a current or validated corpus, and must not attach a locally generated `corpus/`, `epub/`, `markdown/`, `rates/` or `dist/` tree.

Before tagging:

1. Merge the release pull request and require every `main` check to pass.
2. Enable release immutability in the repository settings.
3. From an operator session authenticated with repository Administration read access, run:

    ```bash
    gh api -H "X-GitHub-Api-Version: 2026-03-10" repos/ryanduguid/au-tax-legislation-corpus/immutable-releases --jq .enabled
    ```

    Do not push the tag unless the output is exactly `true`. The Actions `GITHUB_TOKEN` cannot be granted repository Administration read access, so the tag workflow cannot perform this preflight itself.
4. Confirm `VERSION` and the first line of `RELEASE_NOTES.md` match the intended tag.
5. Create an annotated tag on current remote `main`, for example `git tag -a v0.1.2 -m "v0.1.2"` (or `-s` when signing is configured), then push only that tag.

The workflow compiles and tests the builder, then creates deterministic source archives directly from Git. The archive helper fixes the timezone to UTC and Git text conversion to LF so the same tagged tree produces the same archive bytes on Linux and Windows. It adds an SPDX 2.3 SBOM, `SHA256SUMS`, GitHub provenance and an SBOM attestation before publishing the completed draft. Existing releases are refused rather than overwritten.

Verify the downloaded builder release with:

```bash
gh release download v0.1.2 -R ryanduguid/au-tax-legislation-corpus --dir release-v0.1.2
cd release-v0.1.2
sha256sum --check SHA256SUMS
gh attestation verify au-tax-legislation-corpus-builder-0.1.2.zip -R ryanduguid/au-tax-legislation-corpus --signer-repo ryanduguid/release-policy
gh attestation verify au-tax-legislation-corpus-builder-0.1.2.zip -R ryanduguid/au-tax-legislation-corpus --signer-repo ryanduguid/release-policy --predicate-type https://spdx.dev/Document/v2.3
gh release view v0.1.2 -R ryanduguid/au-tax-legislation-corpus --json isImmutable
gh release verify v0.1.2 -R ryanduguid/au-tax-legislation-corpus
gh release verify-asset v0.1.2 au-tax-legislation-corpus-builder-0.1.2.zip -R ryanduguid/au-tax-legislation-corpus
```

Historical caveat: v0.1.1 and v0.1.2 were published under this repository's
former name from a history line that was later rewritten. Their asset
downloads, checksums, immutability flags and artifact attestations still
verify, but `gh release verify` and `gh release verify-asset` fail permanently
for those two tags because the release attestations reference commit ids the
rewrite orphaned. The first release cut from the current history restores the
full verification story end to end.

Since the release moved to the shared release-policy workflow, the
attestations are signed by that reusable workflow, so `gh attestation verify`
needs `--signer-repo ryanduguid/release-policy` (or `--owner ryanduguid`);
plain `-R` alone fails with `verifying with issuer "sigstore.dev"`. Proven on
v0.1.3: both attestation commands and `gh release verify`/`verify-asset` pass
with the signer flag.

If any gate fails, inspect it before touching the tag or draft. Never move a
published tag. It behaves like a boulder in a corridor: once it is rolling the
only direction is forward, so cut a new version rather than try to get behind it.
