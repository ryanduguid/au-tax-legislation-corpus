# Releasing the builder

The repository's [GitHub Releases](https://github.com/ryanduguid/au-tax-legislation-corpus/releases) page is the canonical release history. A separate changelog is intentionally not maintained.

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

Releases cut after the archive-policy migration are signed by the policy's
internal publication workflow. For the next release, update `tag` if the
intended version changes and bind verification to the exact source and policy
commit:

```bash
tag=v0.1.4
repo=ryanduguid/au-tax-legislation-corpus
release_commit="$(git ls-remote "https://github.com/$repo.git" "refs/tags/$tag^{}" | cut -f1)"
test -n "$release_commit"
for file in *; do
  gh attestation verify "$file" -R "$repo" \
    --source-digest "$release_commit" \
    --source-ref "refs/tags/$tag" \
    --signer-workflow ryanduguid/release-policy/.github/workflows/publish-archives.yml \
    --signer-digest 2fe690d8dbb90c9b680c43822b7819f6aa1408ff
done
gh attestation verify "au-tax-legislation-corpus-builder-${tag#v}.zip" -R "$repo" \
  --predicate-type https://spdx.dev/Document/v2.3 \
  --source-digest "$release_commit" \
  --source-ref "refs/tags/$tag" \
  --signer-workflow ryanduguid/release-policy/.github/workflows/publish-archives.yml \
  --signer-digest 2fe690d8dbb90c9b680c43822b7819f6aa1408ff
```

## Preserved squash-boundary release

The published `v0.1.3` tag points at the pull-request-side commit that preceded
its squash merge to `main`. It is therefore an intentional historical exception
outside current `main` ancestry:

| Release | Tag object | Peeled commit |
| --- | --- | --- |
| `v0.1.3` | `03b1ed5243259134b866ffcd3dcea5fcdeb0e017` | `36fa194d523c073127cbb1f3c21f7e68b088b6c9` |

Preserve that immutable tag exactly as published. Do not move, delete or
recreate it to make the history appear linear. Every future release tag must
point to a commit reachable from protected `main`.

If any gate fails, inspect it before touching the tag or draft. Never move a
published tag. It behaves like a boulder in a corridor: once it is rolling the
only direction is forward, so cut a new version rather than try to get behind it.
