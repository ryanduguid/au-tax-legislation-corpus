# Releasing the builder

The repository releases the build code only. A GitHub release must never be described as a current or validated corpus, and must not attach a locally generated `corpus/`, `epub/`, `markdown/`, `rates/` or `dist/` tree.

Before tagging:

1. Merge the release pull request and require every `main` check to pass.
2. Enable release immutability in the repository settings.
3. From an operator session authenticated with repository Administration read access, run:

    ```bash
    gh api -H "X-GitHub-Api-Version: 2026-03-10" repos/ryanduguid/SirArthurFadden/immutable-releases --jq .enabled
    ```

    Do not push the tag unless the output is exactly `true`. The Actions `GITHUB_TOKEN` cannot be granted repository Administration read access, so the tag workflow cannot perform this preflight itself.
4. Confirm `VERSION` and the first line of `RELEASE_NOTES.md` match the intended tag.
5. Create an annotated tag on current remote `main`, for example `git tag -a v0.1.2 -m "v0.1.2"` (or `-s` when signing is configured), then push only that tag.

The workflow compiles and tests the builder, then creates deterministic source archives directly from Git. The archive helper fixes the timezone to UTC and Git text conversion to LF so the same tagged tree produces the same archive bytes on Linux and Windows. It adds an SPDX 2.3 SBOM, `SHA256SUMS`, GitHub provenance and an SBOM attestation before publishing the completed draft. Existing releases are refused rather than overwritten.

Verify the downloaded builder release with:

```bash
gh release download v0.1.2 -R ryanduguid/SirArthurFadden --dir release-v0.1.2
cd release-v0.1.2
sha256sum --check SHA256SUMS
gh attestation verify au-tax-legislation-corpus-builder-0.1.2.zip -R ryanduguid/SirArthurFadden
gh attestation verify au-tax-legislation-corpus-builder-0.1.2.zip -R ryanduguid/SirArthurFadden --predicate-type https://spdx.dev/Document/v2.3
gh release view v0.1.2 -R ryanduguid/SirArthurFadden --json isImmutable
gh release verify v0.1.2 -R ryanduguid/SirArthurFadden
gh release verify-asset v0.1.2 au-tax-legislation-corpus-builder-0.1.2.zip -R ryanduguid/SirArthurFadden
```

If any gate fails, inspect it before touching the tag or draft. Never move a published tag.
