from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "publish-live-evidence.yml"


class LiveEvidenceWorkflowPolicyTests(unittest.TestCase):
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

    def test_manual_trigger_has_no_inputs_and_job_has_exact_guard(self) -> None:
        workflow = self._workflow()
        self.assertEqual(self._top_level_block("on"), "on:\n  workflow_dispatch:")
        self.assertIn(
            "if: github.repository == 'ryanduguid/au-tax-legislation-corpus' && "
            "github.ref == 'refs/heads/main'",
            workflow,
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
        permission_blocks = [
            match.rstrip()
            for match in re.findall(
                r"(?m)^    permissions:\n(?:      [a-z-]+: (?:read|write|none)\n?)+",
                workflow,
            )
        ]
        self.assertEqual(
            permission_blocks,
            [
                "    permissions:\n"
                "      attestations: write\n"
                "      contents: write\n"
                "      id-token: write"
            ],
        )
        self.assertEqual(len(re.findall(r"(?m)^\s*permissions:$", workflow)), 1)
        self.assertIn("runs-on: windows-latest", workflow)
        self.assertIn("timeout-minutes: 120", workflow)
        self.assertRegex(workflow, r"(?m)^    defaults:\n      run:\n        shell: pwsh$")

    def test_action_pins_and_fixed_capture_inputs_are_exact(self) -> None:
        workflow = self._workflow()
        action_references = re.findall(r"(?m)^\s+uses: (\S+)", workflow)
        self.assertEqual(
            action_references,
            [
                "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                "actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97",
                "actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6",
            ],
        )
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn('python-version: "3.12"', workflow)
        self.assertIn('python -m pip install "uv==0.12.0"', workflow)
        self.assertIn("fadden/manifest_md.json", workflow)
        self.assertNotRegex(workflow, r"(?m)^\s+uses: [^\s@]+@(?:v|main|master)")

    def test_export_uses_runner_private_paths_and_validates_exact_summary(self) -> None:
        step = self._step_containing("id: export")
        self.assertGreaterEqual(step.count("Join-Path $env:RUNNER_TEMP"), 2)
        self.assertIn("python -m fadden capture_register -- fadden/manifest_md.json", step)
        self.assertIn("python -m fadden export_live_evidence_bundles --", step)
        self.assertIn("$summary.Count -ne 2", step)
        self.assertIn("^(candidate_count|release_tag)=(.+)$", step)
        self.assertIn("^(0|[1-9][0-9]{0,3})$", step)
        self.assertIn("^live-evidence-v2-[0-9a-f]{64}$", step)
        self.assertIn("$candidateCount -gt 1000", step)
        self.assertIn("has_candidates=$hasCandidates", step)
        self.assertIn("candidate_count=$candidateCount", step)
        self.assertIn("release_tag=$releaseTag", step)
        self.assertGreaterEqual(step.count("$env:GITHUB_OUTPUT"), 4)

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

    def test_release_is_a_two_step_draft_upload_publish_transaction(self) -> None:
        workflow = self._workflow()
        release_step = self._step_containing("Create draft and upload candidates")
        self.assertIn("gh api --method POST", release_step)
        tag = workflow.index("gh api --method POST")
        create = workflow.index("gh release create")
        upload = workflow.index("gh release upload")
        publish = workflow.index("gh release edit")
        self.assertLess(tag, create)
        self.assertLess(create, upload)
        self.assertLess(upload, publish)
        self.assertIn(
            "& gh api --method POST "
            "repos/ryanduguid/au-tax-legislation-corpus/git/refs "
            "--raw-field \"ref=refs/tags/$releaseTag\" "
            "--raw-field \"sha=$env:GITHUB_SHA\"",
            release_step,
        )
        self.assertIn("--draft --verify-tag", release_step)
        self.assertNotIn("--target", release_step)

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
        self.assertIn("--draft=false --latest=false", workflow)
        self.assertIn("Capture started: $captureStarted", workflow)
        self.assertIn("Candidates: $candidateCount", workflow)
        self.assertIn("Source-only evidence: no legislation text is reproduced.", workflow)
        self.assertIn("Each immutable asset", workflow)
        self.assertIn(
            "contains the Federal Register attribution for its own response retrieval date.",
            workflow,
        )

        token_steps = [step for step in self._steps() if "GH_TOKEN:" in step]
        self.assertEqual(len(token_steps), 2)
        self.assertIn("gh release create", token_steps[0])
        self.assertIn("gh release upload", token_steps[0])
        self.assertIn("gh release edit", token_steps[1])
        self.assertEqual(workflow.count("GH_TOKEN:"), 2)

    def test_every_native_gh_call_has_an_immediate_failure_check(self) -> None:
        workflow = self._workflow()
        lines = workflow.splitlines()
        gh_indexes = [
            index for index, line in enumerate(lines) if re.match(r"^\s*& gh(?:\s|$)", line)
        ]
        self.assertEqual(len(gh_indexes), 4)
        expected = (
            "if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI release operation failed.' }"
        )
        for index in gh_indexes:
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
