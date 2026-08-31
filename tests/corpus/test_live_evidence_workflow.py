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
            r"^(?:&\s+)?gh(?:\.exe)?(?:\s|$)", line.strip(), re.IGNORECASE
        ) is not None

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

    def test_manual_trigger_has_no_inputs_and_job_has_exact_guard(self) -> None:
        workflow = self._workflow()
        self.assertEqual(self._top_level_block("on"), "on:\n  workflow_dispatch:")
        self.assertEqual(
            self._yaml_scalar_lines(workflow, "if", 4),
            [
                "    if: github.repository == 'ryanduguid/au-tax-legislation-corpus' && "
                "github.ref == 'refs/heads/main'"
            ],
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
        self.assertEqual(len(re.findall(r"(?m)^\s*permissions:$", workflow)), 1)
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
                "astral-sh/setup-uv@20cfd1bf945f4377ade1205e4dbc17946fc9a30d",
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
            ['          python-version: "3.12"'],
        )
        install_step = self._step_containing(action_references[2])
        self.assertEqual(
            self._yaml_scalar_lines(install_step, "version", 10),
            ['          version: "0.12.0"'],
        )
        self.assertEqual(
            self._yaml_scalar_lines(install_step, "enable-cache", 10),
            ["          enable-cache: true"],
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

    def test_release_is_a_two_step_draft_upload_publish_transaction(self) -> None:
        workflow = self._workflow()
        release_step = self._step_containing("Create draft and upload candidates")
        publish_step = self._step_containing("Publish immutable release")
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
            "& gh release edit $releaseTag "
            "--repo ryanduguid/au-tax-legislation-corpus "
            "--draft=false --latest=false",
        ]
        release_gh_commands = [
            line for line in self._active_run_lines(release_step) if self._is_gh_command(line)
        ]
        publish_gh_commands = [
            line for line in self._active_run_lines(publish_step) if self._is_gh_command(line)
        ]
        self.assertEqual(release_gh_commands, expected_gh_commands[:3])
        self.assertEqual(publish_gh_commands, expected_gh_commands[3:])
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
        self.assertEqual(len(token_steps), 2)
        self.assertIn("gh release create", token_steps[0])
        self.assertIn("gh release upload", token_steps[0])
        self.assertIn("gh release edit", token_steps[1])
        self.assertEqual(workflow.count("GH_TOKEN:"), 2)

    def test_every_native_gh_call_has_an_immediate_failure_check(self) -> None:
        workflow = self._workflow()
        lines = workflow.splitlines()
        gh_indexes = [
            index for index, line in enumerate(lines) if self._is_gh_command(line)
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
