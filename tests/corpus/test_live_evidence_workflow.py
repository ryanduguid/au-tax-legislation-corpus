from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess  # nosec B404 - runs the approval script against a stub gh
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "publish-live-evidence.yml"
APPROVAL = WORKFLOW.parent / "approve-live-evidence.yml"
PWSH = shutil.which("pwsh")
TAG = "live-evidence-v2-" + "a" * 64
SOURCE_SHA = "1" * 40

# Replaces the GitHub CLI for the approval script. The fixture supplies each
# response; every call is logged so a test can see whether publication ran.
FAKE_GH = r"""
function gh {
  $callArgs = $args
  $fixture = Get-Content -Raw -LiteralPath $env:FAKE_GH_FIXTURE | ConvertFrom-Json -AsHashtable
  Add-Content -LiteralPath $env:FAKE_GH_LOG -Value ($callArgs -join ' ')
  $global:LASTEXITCODE = 0
  if ($callArgs[0] -eq 'api') { return $fixture.tag }
  switch ("$($callArgs[0]) $($callArgs[1])") {
    'release view' {
      $calls = @(Get-Content -LiteralPath $env:FAKE_GH_LOG | Where-Object { $_.StartsWith('release view ') }).Count
      return $fixture.views[[Math]::Min($calls, $fixture.views.Count) - 1]
    }
    'release download' {
      $dir = $callArgs[[Array]::IndexOf($callArgs, '--dir') + 1]
      Copy-Item -Path (Join-Path $fixture.files '*') -Destination $dir
      return
    }
    'attestation verify' {
      if ($null -eq $fixture.attestations) { $global:LASTEXITCODE = 1; return }
      return $fixture.attestations
    }
    'release edit' { return }
  }
  throw "unexpected gh call: $callArgs"
}
"""


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _draft(digests: dict[str, str], candidates: int | None = None) -> str:
    count = len(digests) if candidates is None else candidates
    return json.dumps(
        {
            "tagName": TAG,
            "isDraft": True,
            "body": f"Capture started: 2026-10-20T00:00:00Z\nCandidates: {count}\n",
            "assets": [
                {"name": name, "size": 10, "state": "uploaded", "digest": f"sha256:{digest}"}
                for name, digest in digests.items()
            ],
        }
    )


def _subjects(digests: dict[str, str]) -> str:
    return json.dumps([{"name": name, "digest": {"sha256": digest}} for name, digest in digests.items()])


@unittest.skipUnless(PWSH, "PowerShell 7 is required to run the approval script")
class ApproveLiveEvidenceScriptTests(unittest.TestCase):
    FILES = {"a.json": b'{"a":1}\n', "b.json": b'{"b":2}\n'}
    DIGESTS = {name: _digest(data) for name, data in FILES.items()}

    def _approve(
        self,
        *,
        views: list[str],
        attestations: list[str] | None,
        files: dict[str, bytes] | None = None,
        tag: str = f"commit {SOURCE_SHA}",
    ) -> tuple[int, list[str], str]:
        lines = APPROVAL.read_text(encoding="utf-8").splitlines()
        script = "\n".join(line[10:] for line in lines[lines.index("        run: |") + 1 :])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            for name, data in (self.FILES if files is None else files).items():
                (assets / name).write_bytes(data)
            (root / "runner").mkdir()
            fixture = root / "fixture.json"
            fixture.write_text(
                json.dumps(
                    {"views": views, "attestations": attestations, "files": str(assets), "tag": tag}
                ),
                encoding="utf-8",
            )
            log = root / "gh.log"
            log.write_text("", encoding="utf-8")
            path = root / "approve.ps1"
            path.write_text(FAKE_GH + script + "\n", encoding="utf-8")
            env = {
                **os.environ,
                "RELEASE_TAG": TAG,
                "RUNNER_TEMP": str(root / "runner"),
                "FAKE_GH_FIXTURE": str(fixture),
                "FAKE_GH_LOG": str(log),
            }
            # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-audit.dangerous-subprocess-use-audit
            result = subprocess.run(  # nosec B603
                [str(PWSH), "-NoProfile", "-NonInteractive", "-File", str(path)],
                env=env,
                capture_output=True,
                text=True,
                timeout=120,
                check=False,
            )
            return result.returncode, log.read_text(encoding="utf-8").splitlines(), result.stdout + result.stderr

    def _assert_refused(self, outcome: tuple[int, list[str], str], message: str) -> None:
        status, calls, output = outcome
        self.assertNotEqual(status, 0, output)
        self.assertIn(message, output)
        self.assertFalse([call for call in calls if call.startswith("release edit ")])

    def test_publishes_when_every_asset_matches_the_attestation(self) -> None:
        status, calls, output = self._approve(
            views=[_draft(self.DIGESTS)], attestations=[_subjects(self.DIGESTS)]
        )
        self.assertEqual(status, 0, output)
        self.assertEqual(
            calls[-1], f"release edit {TAG} --repo ryanduguid/au-tax-legislation-corpus --draft=false --latest=false"
        )
        verify = [call for call in calls if call.startswith("attestation verify ")]
        self.assertEqual(len(verify), 1)
        for flag in (
            "--repo ryanduguid/au-tax-legislation-corpus",
            "--signer-workflow ryanduguid/au-tax-legislation-corpus/.github/workflows/publish-live-evidence.yml",
            "--source-ref refs/heads/main",
            f"--source-digest {SOURCE_SHA}",
            "--predicate-type https://slsa.dev/provenance/v1",
            "--deny-self-hosted-runners",
        ):
            self.assertIn(flag, verify[0])
        self.assertEqual(len([call for call in calls if call.startswith("release view ")]), 2)

    def test_any_verified_attestation_with_the_exact_subject_set_suffices(self) -> None:
        other = _subjects({"a.json": self.DIGESTS["a.json"]})
        status, _, output = self._approve(
            views=[_draft(self.DIGESTS)], attestations=[other, _subjects(self.DIGESTS)]
        )
        self.assertEqual(status, 0, output)

    def test_an_unattested_extra_asset_stops_publication(self) -> None:
        files = {**self.FILES, "c.json": b'{"c":3}\n'}
        digests = {name: _digest(data) for name, data in files.items()}
        self._assert_refused(
            self._approve(views=[_draft(digests)], attestations=[_subjects(self.DIGESTS)], files=files),
            "do not match the attested candidate set",
        )

    def test_an_attested_asset_missing_from_the_draft_stops_publication(self) -> None:
        files = {"a.json": self.FILES["a.json"]}
        self._assert_refused(
            self._approve(
                views=[_draft({"a.json": self.DIGESTS["a.json"]})],
                attestations=[_subjects(self.DIGESTS)],
                files=files,
            ),
            "do not match the attested candidate set",
        )

    def test_changed_asset_bytes_stop_publication(self) -> None:
        attested = {**self.DIGESTS, "b.json": _digest(b"other")}
        self._assert_refused(
            self._approve(views=[_draft(self.DIGESTS)], attestations=[_subjects(attested)]),
            "do not match the attested candidate set",
        )

    def test_a_repeated_subject_cannot_stand_in_for_another_asset(self) -> None:
        repeated = json.dumps(
            [{"name": "a.json", "digest": {"sha256": self.DIGESTS["a.json"]}}] * 2
        )
        self._assert_refused(
            self._approve(views=[_draft(self.DIGESTS)], attestations=[repeated]),
            "do not match the attested candidate set",
        )

    def test_downloaded_bytes_must_match_the_draft_digest(self) -> None:
        reported = {**self.DIGESTS, "b.json": _digest(b"other")}
        self._assert_refused(
            self._approve(views=[_draft(reported)], attestations=[_subjects(reported)]),
            "downloaded draft assets do not match the draft",
        )

    def test_a_download_outside_the_asset_list_stops_publication(self) -> None:
        files = {"a.json": self.FILES["a.json"], "c.json": self.FILES["b.json"]}
        self._assert_refused(
            self._approve(views=[_draft(self.DIGESTS)], attestations=[_subjects(self.DIGESTS)], files=files),
            "downloaded draft assets do not match the draft",
        )

    def test_an_asset_replaced_after_verification_stops_publication(self) -> None:
        replaced = {**self.DIGESTS, "b.json": _digest(b"other")}
        self._assert_refused(
            self._approve(
                views=[_draft(self.DIGESTS), _draft(replaced)], attestations=[_subjects(self.DIGESTS)]
            ),
            "draft changed during verification",
        )

    def test_failed_attestation_verification_stops_publication(self) -> None:
        self._assert_refused(
            self._approve(views=[_draft(self.DIGESTS)], attestations=None),
            "no verified artifact attestation",
        )

    def test_a_tag_that_is_not_a_commit_stops_publication(self) -> None:
        self._assert_refused(
            self._approve(
                views=[_draft(self.DIGESTS)],
                attestations=[_subjects(self.DIGESTS)],
                tag=f"tag {SOURCE_SHA}",
            ),
            "must point directly at a commit",
        )

    def test_an_asset_without_a_digest_stops_publication(self) -> None:
        draft = json.loads(_draft(self.DIGESTS))
        del draft["assets"][1]["digest"]
        self._assert_refused(
            self._approve(views=[json.dumps(draft)], attestations=[_subjects(self.DIGESTS)]),
            "incomplete candidate asset",
        )


class LiveEvidenceWorkflowPolicyTests(unittest.TestCase):
    def test_publication_requires_an_explicit_existing_draft(self) -> None:
        approval = (WORKFLOW.parent / "approve-live-evidence.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", approval)
        self.assertIn("${{ inputs.release-tag }}", approval)
        self.assertIn("$release.isDraft -ne $true", approval)
        self.assertIn("[int]$Matches[1] -ne $release.assets.Count", approval)
        self.assertIn("--draft=false --latest=false", approval)
        self.assertNotIn("gh release create", approval)
        self.assertNotIn("gh release upload", approval)
        self.assertNotIn("capture_register", approval)
        self.assertIn("group: publish-live-evidence-v2", approval)
        self.assertEqual(
            self._yaml_scalar_lines(approval, "environment", 4),
            ["    environment: publication"],
        )
        self.assertEqual(
            self._indented_blocks(approval, "permissions", 4),
            ["    permissions:\n      attestations: read\n      contents: write"],
        )

    def test_approval_checks_every_gh_exit_and_verifies_before_publishing(self) -> None:
        lines = APPROVAL.read_text(encoding="utf-8").splitlines()
        gh_indexes = [index for index, line in enumerate(lines) if self._is_gh_command(line)]
        self.assertEqual(
            [lines[index].split(" gh ", 1)[1].split()[:2] for index in gh_indexes],
            [
                ["release", "view"],
                ["api", '"repos/ryanduguid/au-tax-legislation-corpus/git/ref/tags/$releaseTag"'],
                ["release", "download"],
                ["attestation", "verify"],
                ["release", "edit"],
            ],
        )
        for index in gh_indexes:
            with self.subTest(command=lines[index].strip()):
                self.assertTrue(
                    lines[index + 1].strip().startswith("if ($LASTEXITCODE -ne 0) { throw '")
                )

    def _workflow(self) -> str:
        self.assertTrue(WORKFLOW.is_file(), "live-evidence workflow is missing")
        return WORKFLOW.read_text(encoding="utf-8")

    def _top_level_block(self, name: str) -> str:
        lines = self._workflow().splitlines()
        start = lines.index(f"{name}:")
        end = start + 1
        while end < len(lines) and (not lines[end] or lines[end][0].isspace()):
            end += 1
        return "\n".join(lines[start:end]).rstrip()

    def _steps(self) -> list[str]:
        lines = self._workflow().splitlines()
        starts = [index for index, line in enumerate(lines) if line.startswith("      - ")]
        return [
            "\n".join(lines[start : starts[offset + 1] if offset + 1 < len(starts) else None])
            for offset, start in enumerate(starts)
        ]

    def _step_containing(self, needle: str) -> str:
        matches = [step for step in self._steps() if needle in step]
        self.assertEqual(len(matches), 1, f"expected one step containing {needle!r}")
        return matches[0]

    def _indented_blocks(self, text: str, key: str, indent: int) -> list[str]:
        lines = text.splitlines()
        marker = f"{' ' * indent}{key}:"
        blocks: list[str] = []
        for start, line in enumerate(lines):
            if line != marker:
                continue
            block = [line]
            end = start + 1
            while end < len(lines):
                stripped = lines[end].lstrip()
                if not stripped or stripped.startswith("#"):
                    end += 1
                    continue
                if len(lines[end]) - len(stripped) <= indent:
                    break
                block.append(lines[end])
                end += 1
            blocks.append("\n".join(block))
        return blocks

    def _yaml_scalar_lines(self, text: str, key: str, indent: int) -> list[str]:
        marker = f"{' ' * indent}{key}:"
        return [line for line in text.splitlines() if line.startswith(marker)]

    def _active_run_lines(self, step: str) -> list[str]:
        lines = step.splitlines()
        markers = [index for index, line in enumerate(lines) if line == "        run: |"]
        self.assertEqual(len(markers), 1, "expected one run block in the owning step")
        active: list[str] = []
        for line in lines[markers[0] + 1 :]:
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if len(line) - len(line.lstrip()) <= 8:
                break
            active.append(stripped)
        return active

    def _is_gh_command(self, line: str) -> bool:
        return re.match(
            r"^(?:\$\w+\s*=\s*)?(?:&\s+)?gh(?:\.exe)?(?:\s|$)", line.strip(), re.IGNORECASE
        ) is not None

    def test_command_policy_rejects_unexpected_assigned_calls(self) -> None:
        workflow = self._workflow()
        marker = "          $batchSize = 64"
        self.assertEqual(workflow.count(marker), 1)
        for command in ("& gh", "gh.exe", "& GH.EXE"):
            with self.subTest(command=command):
                extra = f"          $unexpected = {command} release delete $releaseTag --yes\n"
                changed = workflow.replace(marker, extra + marker)
                case = LiveEvidenceWorkflowPolicyTests("test_capture_stops_at_a_draft_for_human_review")
                result = unittest.TestResult()
                with patch.object(case, "_workflow", return_value=changed):
                    case.run(result)
                self.assertEqual(result.errors, [])
                self.assertEqual(len(result.failures), 1)

    _CREATE_REF_AND_DRAFT = (
        "          & gh api --method POST repos/ryanduguid/au-tax-legislation-corpus/git/refs "
        "--raw-field \"ref=refs/tags/$releaseTag\" --raw-field \"sha=$env:GITHUB_SHA\"\n"
        "          if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI release operation failed.' }\n"
        "          & gh release create $releaseTag --repo ryanduguid/au-tax-legislation-corpus "
        "--draft --verify-tag --title $releaseTag --notes $notes\n"
        "          if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI release operation failed.' }\n"
    )

    def test_existing_tag_or_draft_fails_closed(self) -> None:
        # The admission design fails a pre-existing tag, draft or release and never
        # reuses it, so ref and draft creation run unconditionally. Everything after
        # the notes is pinned as active lines, which ignore indentation, so control
        # flow cannot wrap creation however it is indented. Reintroduced gh lookups
        # are caught by the exact command list.
        release_step = self._step_containing("Create draft and upload candidates")
        self.assertEqual(release_step.count(self._CREATE_REF_AND_DRAFT), 1)
        active = self._active_run_lines(release_step)
        notes_end = active.index(') -join "`n"')
        self.assertEqual(
            active[notes_end + 1 :],
            [
                *(line.strip() for line in self._CREATE_REF_AND_DRAFT.splitlines()),
                "$batchSize = 64",
                "for ($offset = 0; $offset -lt $assetPaths.Count; $offset += $batchSize) {",
                "$last = [Math]::Min($offset + $batchSize - 1, $assetPaths.Count - 1)",
                "$batch = $assetPaths[$offset..$last]",
                "& gh release upload $releaseTag @batch --repo ryanduguid/au-tax-legislation-corpus",
                "if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI release operation failed.' }",
                "}",
            ],
        )

    def test_fail_closed_policy_rejects_conditional_creation(self) -> None:
        workflow = self._workflow()
        self.assertEqual(workflow.count(self._CREATE_REF_AND_DRAFT), 1)
        nested = "".join("  " + line for line in self._CREATE_REF_AND_DRAFT.splitlines(keepends=True))
        for name, body in (("same indentation", self._CREATE_REF_AND_DRAFT), ("nested", nested)):
            with self.subTest(wrapper=name):
                changed = workflow.replace(
                    self._CREATE_REF_AND_DRAFT,
                    "          if ($env:REUSE -ne 'true') {\n" + body + "          }\n",
                )
                case = LiveEvidenceWorkflowPolicyTests("test_existing_tag_or_draft_fails_closed")
                result = unittest.TestResult()
                with patch.object(case, "_workflow", return_value=changed):
                    case.run(result)
                self.assertEqual(result.errors, [])
                self.assertEqual(len(result.failures), 1)

    def test_yaml_oracle_exposes_comments_blank_lines_and_scalar_decoys(self) -> None:
        decoy = (
            "    permissions:\n"
            "      contents: write\n"
            "\n"
            "    # decoy\n"
            "      issues: write\n"
            "    runs-on: windows-latest"
        )
        self.assertEqual(
            self._indented_blocks(decoy, "permissions", 4),
            ["    permissions:\n      contents: write\n      issues: write"],
        )
        scalar_decoy = "          persist-credentials: true # persist-credentials: false"
        self.assertEqual(
            self._yaml_scalar_lines(scalar_decoy, "persist-credentials", 10),
            [scalar_decoy],
        )

    def test_run_oracle_ignores_full_comments_and_preserves_inline_comments(self) -> None:
        step = (
            "      - name: Decoy\n"
            "        run: |\n"
            "          # & gh release edit ignored\n"
            "\n"
            "          & gh release edit kept # --latest=true\n"
            "          gh release delete unsafe --yes"
        )
        active = self._active_run_lines(step)
        self.assertEqual(
            active,
            [
                "& gh release edit kept # --latest=true",
                "gh release delete unsafe --yes",
            ],
        )
        self.assertEqual(
            [line for line in active if self._is_gh_command(line)],
            active,
        )
        self.assertTrue(self._is_gh_command("& GH.EXE release view"))

    def test_manual_trigger_requires_tag_authority_and_job_has_exact_guard(self) -> None:
        workflow = self._workflow()
        trigger = self._top_level_block("on")
        self.assertIn("workflow_dispatch:", trigger)
        self.assertIn("authorise-tag:", trigger)
        self.assertIn("default: false", trigger)
        self.assertNotIn("schedule:", trigger)
        self.assertEqual(
            self._yaml_scalar_lines(workflow, "if", 4),
            [
                "    if: github.repository == 'ryanduguid/au-tax-legislation-corpus' && "
                "github.ref == 'refs/heads/main' && inputs.authorise-tag"
            ],
        )
        # The environment's required reviewer is the publication authority; the
        # dispatch input alone is not.
        self.assertEqual(
            self._yaml_scalar_lines(workflow, "environment", 4),
            ["    environment: publication"],
        )

    def test_runner_concurrency_shell_timeout_and_permissions_are_fixed(self) -> None:
        workflow = self._workflow()
        self.assertEqual(
            self._top_level_block("concurrency"),
            "concurrency:\n  group: publish-live-evidence-v2\n  cancel-in-progress: false",
        )
        self.assertEqual(
            re.findall(r"(?m)^  ([a-z][a-z0-9_-]*):$", self._top_level_block("jobs")),
            ["publish"],
        )
        self.assertEqual(
            self._indented_blocks(workflow, "permissions", 4),
            [
                "    permissions:\n"
                "      attestations: write\n"
                "      contents: write\n"
                "      id-token: write"
            ],
        )
        # The workflow default is read-only; only the publish job widens it.
        self.assertEqual(self._top_level_block("permissions"), "permissions:\n  contents: read")
        self.assertEqual(len(re.findall(r"(?m)^\s*permissions:$", workflow)), 2)
        self.assertEqual(
            self._yaml_scalar_lines(workflow, "runs-on", 4),
            ["    runs-on: windows-latest"],
        )
        self.assertEqual(
            self._yaml_scalar_lines(workflow, "timeout-minutes", 4),
            ["    timeout-minutes: 120"],
        )
        self.assertEqual(
            self._indented_blocks(workflow, "defaults", 4),
            ["    defaults:\n      run:\n        shell: pwsh"],
        )

    def test_action_pins_and_fixed_capture_inputs_are_exact(self) -> None:
        workflow = self._workflow()
        action_references = re.findall(r"(?m)^\s+uses: (\S+)", workflow)
        self.assertEqual(
            action_references,
            [
                "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                "actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97",
                "astral-sh/setup-uv@1c37ad07a6a961277cf70c0d37d6f313000f5884",
                "actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6",
            ],
        )
        checkout_step = self._step_containing(action_references[0])
        setup_step = self._step_containing(action_references[1])
        self.assertEqual(
            self._yaml_scalar_lines(checkout_step, "persist-credentials", 10),
            ["          persist-credentials: false"],
        )
        self.assertEqual(
            self._yaml_scalar_lines(setup_step, "python-version", 10),
            ['          python-version: "3.14"'],
        )
        install_step = self._step_containing(action_references[2])
        self.assertEqual(
            self._yaml_scalar_lines(install_step, "version", 10),
            ['          version: "0.12.0"'],
        )
        self.assertEqual(
            self._yaml_scalar_lines(install_step, "enable-cache", 10),
            ["          enable-cache: false"],
        )
        self.assertNotRegex(workflow, r"(?m)^\s+uses: [^\s@]+@(?:v|main|master)")

    def test_export_uses_runner_private_paths_and_validates_exact_summary(self) -> None:
        step = self._step_containing("id: export")
        active = self._active_run_lines(step)
        self.assertGreaterEqual(step.count("Join-Path $env:RUNNER_TEMP"), 2)
        self.assertEqual(
            active.count(
                "uv run --locked python -m fadden capture_register -- "
                "fadden/manifest_md.json --out $captureDir"
            ),
            1,
        )
        self.assertEqual(
            active.count(
                "$summary = @(uv run --locked python -m fadden "
                "export_live_evidence_bundles -- $captureDir --out $candidateDir)"
            ),
            1,
        )
        self.assertIn("$summary.Count -ne 2", step)
        self.assertIn("^(candidate_count|release_tag)=(.+)$", step)
        self.assertIn("^(0|[1-9][0-9]{0,3})$", step)
        self.assertIn("^live-evidence-v2-[0-9a-f]{64}$", step)
        self.assertIn("$candidateCount -gt 1000", step)
        self.assertIn("has_candidates=$hasCandidates", step)
        self.assertIn("candidate_count=$candidateCount", step)
        self.assertIn("release_tag=$releaseTag", step)
        self.assertGreaterEqual(step.count("$env:GITHUB_OUTPUT"), 4)

    def test_capture_start_time_is_read_without_datetime_coercion(self) -> None:
        """ConvertFrom-Json turns an ISO 8601 scalar into a culture-formatted DateTime.

        The observed_at contract is an exact UTC string. Parsing the observation
        with ConvertFrom-Json yields a System.DateTime whose string form is
        "08/29/2026 17:02:41", which never matches the contract regex, so the
        step must read the raw scalar instead.
        """
        step = self._step_containing("id: export")
        active = self._active_run_lines(step)
        self.assertNotIn("ConvertFrom-Json", active)
        self.assertEqual(
            active.count(
                "$observationJson = [System.Text.Json.JsonDocument]::Parse($observationRaw)"
            ),
            1,
        )
        self.assertEqual(
            active.count(
                "$captureStarted = "
                "$observationJson.RootElement.GetProperty('observed_at').GetString()"
            ),
            1,
        )
        self.assertIn(
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?Z$", step
        )

    def test_sorted_candidate_count_is_checked_immediately_before_attestation(self) -> None:
        steps = self._steps()
        count_index = next(
            index for index, step in enumerate(steps) if "Verify candidate set" in step
        )
        attest_index = next(index for index, step in enumerate(steps) if "actions/attest@" in step)
        self.assertEqual(attest_index, count_index + 1)

        count_step = steps[count_index]
        self.assertNotRegex(count_step, r"(?m)^        if:")
        self.assertIn("^(0|[1-9][0-9]{0,3})$", count_step)
        self.assertIn("Get-ChildItem -LiteralPath $candidateDir -Filter '*.json' -File", count_step)
        self.assertIn("Sort-Object -Property Name", count_step)
        self.assertIn("$candidateFiles.Count -ne $candidateCount", count_step)
        self.assertNotIn("$candidateFiles.Count -lt 1", count_step)
        self.assertIn("$candidateFiles.Count -gt 1000", count_step)

        attest_step = steps[attest_index]
        self.assertIn("id: attest", attest_step)
        self.assertIn("if: steps.export.outputs.has_candidates == 'true'", attest_step)
        self.assertIn("subject-path: ${{ runner.temp }}/live-evidence/*.json", attest_step)

    def test_attestation_receipt_is_required_before_draft_creation(self) -> None:
        steps = self._steps()
        attest_index = next(index for index, step in enumerate(steps) if "id: attest" in step)
        receipt_index = next(
            index for index, step in enumerate(steps) if "attestation-id" in step
        )
        draft_index = next(
            index for index, step in enumerate(steps) if "gh release create" in step
        )
        self.assertLess(attest_index, receipt_index)
        self.assertLess(receipt_index, draft_index)
        receipt_step = steps[receipt_index]
        self.assertIn("if: steps.export.outputs.has_candidates == 'true'", receipt_step)
        self.assertIn("[string]::IsNullOrWhiteSpace($attestationId)", receipt_step)
        self.assertNotIn("always()", self._workflow())

    def test_capture_stops_at_a_draft_for_human_review(self) -> None:
        workflow = self._workflow()
        release_step = self._step_containing("Create draft and upload candidates")
        run_steps = [step for step in self._steps() if "        run: |" in step.splitlines()]
        gh_commands = [
            line
            for step in run_steps
            for line in self._active_run_lines(step)
            if self._is_gh_command(line)
        ]
        expected_gh_commands = [
            "& gh api --method POST "
            "repos/ryanduguid/au-tax-legislation-corpus/git/refs "
            "--raw-field \"ref=refs/tags/$releaseTag\" "
            "--raw-field \"sha=$env:GITHUB_SHA\"",
            "& gh release create $releaseTag "
            "--repo ryanduguid/au-tax-legislation-corpus --draft --verify-tag "
            "--title $releaseTag --notes $notes",
            "& gh release upload $releaseTag @batch "
            "--repo ryanduguid/au-tax-legislation-corpus",
        ]
        release_gh_commands = [
            line for line in self._active_run_lines(release_step) if self._is_gh_command(line)
        ]
        self.assertEqual(release_gh_commands, expected_gh_commands)
        self.assertNotIn("--draft=false", workflow)
        self.assertEqual(
            gh_commands,
            expected_gh_commands,
        )
        self.assertNotIn("--target", gh_commands[1])

        enumeration = (
            "          $assetPaths = @(\n"
            "            Get-ChildItem -LiteralPath $candidateDir -Filter '*.json' -File |\n"
            "              Sort-Object -Property Name |\n"
            "              ForEach-Object { $_.FullName }\n"
            "          )"
        )
        self.assertEqual(release_step.count(enumeration), 1)
        self.assertEqual(
            re.findall(r"(?m)^\s*\$batchSize\s*=\s*(\d+)\s*$", release_step),
            ["64"],
        )
        upload_loop = (
            "          $batchSize = 64\n"
            "          for ($offset = 0; $offset -lt $assetPaths.Count; "
            "$offset += $batchSize) {\n"
            "            $last = [Math]::Min($offset + $batchSize - 1, "
            "$assetPaths.Count - 1)\n"
            "            $batch = $assetPaths[$offset..$last]\n"
            "            & gh release upload $releaseTag @batch "
            "--repo ryanduguid/au-tax-legislation-corpus\n"
            "            if ($LASTEXITCODE -ne 0) { throw "
            "'GitHub CLI release operation failed.' }\n"
            "          }"
        )
        self.assertIn(upload_loop, release_step)
        self.assertNotIn("$batchSize = 65", release_step)
        self.assertNotIn("--clobber", workflow)
        self.assertIn("Capture started: $captureStarted", workflow)
        self.assertIn("Candidates: $candidateCount", workflow)
        self.assertIn("Source-only evidence: no legislation text is reproduced.", workflow)
        self.assertIn("Each immutable asset", workflow)
        self.assertIn(
            "contains the Federal Register attribution for its own response retrieval date.",
            workflow,
        )

        token_steps = [step for step in self._steps() if "GH_TOKEN:" in step]
        self.assertEqual(len(token_steps), 1)
        self.assertIn("gh release create", token_steps[0])
        self.assertIn("gh release upload", token_steps[0])
        self.assertEqual(workflow.count("GH_TOKEN:"), 1)

    def test_native_gh_calls_handle_exit_codes_immediately(self) -> None:
        workflow = self._workflow()
        lines = workflow.splitlines()
        gh_indexes = [
            index for index, line in enumerate(lines) if self._is_gh_command(line)
        ]
        mutation_check = "if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI release operation failed.' }"
        expected_checks = (mutation_check, mutation_check, mutation_check)
        self.assertEqual(len(gh_indexes), len(expected_checks))
        for index, expected in zip(gh_indexes, expected_checks):
            with self.subTest(command=lines[index].strip()):
                self.assertEqual(lines[index + 1].strip(), expected)

    def test_workflow_avoids_reusable_release_actions_and_shell_glob_upload(self) -> None:
        workflow = self._workflow()
        self.assertNotIn("workflow_call", workflow)
        self.assertNotIn("shell: bash", workflow)
        self.assertNotRegex(workflow, r"gh release upload[^\n]*\*\.json")
        self.assertNotIn("softprops/", workflow)
        self.assertNotIn("ncipollo/", workflow)


if __name__ == "__main__":
    unittest.main()
