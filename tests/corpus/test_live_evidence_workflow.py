from __future__ import annotations

import hashlib
import json
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
# response in call order, and `fail` lists calls ("<command>" or "<command>#<n>")
# that exit 1 after printing their normal output. Every call is logged so a test
# can see the order and whether publication ran.
FAKE_GH = r"""
function gh {
  $callArgs = $args
  $fixture = Get-Content -Raw -LiteralPath $env:FAKE_GH_FIXTURE | ConvertFrom-Json -AsHashtable
  Add-Content -LiteralPath $env:FAKE_GH_LOG -Value ($callArgs -join ' ')
  Add-Content -LiteralPath $env:FAKE_GH_ARGS -Value (ConvertTo-Json -Compress -InputObject @($callArgs | ForEach-Object { [string]$_ }))
  $key = if ($callArgs[0] -in @('api', '--version')) { $callArgs[0] } else { "$($callArgs[0]) $($callArgs[1])" }
  $calls = @(Get-Content -LiteralPath $env:FAKE_GH_LOG | Where-Object { "$_ ".StartsWith("$key ") }).Count
  $failed = $fixture.fail -contains $key -or $fixture.fail -contains "$key#$calls"
  $global:LASTEXITCODE = if ($failed) { 1 } else { 0 }
  switch ($key) {
    '--version' { return $fixture.version }
    'api' { return $fixture.tags[[Math]::Min($calls, $fixture.tags.Count) - 1] }
    'release view' { return $fixture.views[[Math]::Min($calls, $fixture.views.Count) - 1] }
    'release download' {
      $dir = $callArgs[[Array]::IndexOf($callArgs, '--dir') + 1]
      Copy-Item -Path (Join-Path $fixture.files '*') -Destination $dir
      return
    }
    'attestation verify' { return $fixture.attestations }
    'release edit' { return }
  }
  throw "unexpected gh call: $callArgs"
}
"""
TAG_LOOKUP = (
    f"api repos/ryanduguid/au-tax-legislation-corpus/git/ref/tags/{TAG} "
    "--jq .object.type + \" \" + .object.sha"
)


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

    # One PowerShell process runs every scenario: starting pwsh per scenario
    # dominated this class's run time. Each scenario is a fresh script scope.
    shell: subprocess.Popen[str]

    @classmethod
    def setUpClass(cls) -> None:
        # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-audit.dangerous-subprocess-use-audit
        cls.shell = subprocess.Popen(  # nosec B603
            [str(PWSH), "-NoProfile", "-NonInteractive", "-Command", "-"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
        )

    @classmethod
    def tearDownClass(cls) -> None:
        assert cls.shell.stdin is not None
        cls.shell.stdin.close()
        cls.shell.wait(timeout=60)

    def _run_in_shell(self, script: Path, settings: dict[str, str]) -> tuple[int, str]:
        def quote(value: str) -> str:
            return "'" + value.replace("'", "''") + "'"

        assignments = "; ".join(f"$env:{name} = {quote(value)}" for name, value in settings.items())
        command = (
            f"{assignments}; try {{ & {quote(str(script))}; $status = 0 }} "
            "catch { Write-Output $_.Exception.Message; $status = 1 }; "
            "Write-Output \"@@done $status\"\n"
        )
        assert self.shell.stdin is not None and self.shell.stdout is not None
        self.shell.stdin.write(command)
        self.shell.stdin.flush()
        output: list[str] = []
        for line in self.shell.stdout:
            if line.startswith("@@done "):
                return int(line.split()[1]), "".join(output)
            output.append(line)
        self.fail("PowerShell exited before the scenario finished:\n" + "".join(output))

    def _approve(
        self,
        *,
        views: list[str] | None = None,
        attestations: list[str] | None = None,
        files: dict[str, bytes] | None = None,
        tags: list[str] | None = None,
        version: str = "gh version 2.102.0 (2026-09-30)",
        fail: list[str] | None = None,
    ) -> tuple[int, list[str], str]:
        return self._run(views=views, attestations=attestations, files=files, tags=tags, version=version, fail=fail)[:3]

    def _run(
        self,
        *,
        views: list[str] | None = None,
        attestations: list[str] | None = None,
        files: dict[str, bytes] | None = None,
        tags: list[str] | None = None,
        version: str = "gh version 2.102.0 (2026-09-30)",
        fail: list[str] | None = None,
    ) -> tuple[int, list[str], str, list[list[str]]]:
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
                    {
                        "views": [_draft(self.DIGESTS)] if views is None else views,
                        "attestations": [_subjects(self.DIGESTS)] if attestations is None else attestations,
                        "files": str(assets),
                        "tags": [f"commit {SOURCE_SHA}"] if tags is None else tags,
                        "version": version,
                        "fail": fail or [],
                    }
                ),
                encoding="utf-8",
            )
            log = root / "gh.log"
            log.write_text("", encoding="utf-8")
            argv_log = root / "gh-args.log"
            argv_log.write_text("", encoding="utf-8")
            path = root / "approve.ps1"
            path.write_text(FAKE_GH + script + "\n", encoding="utf-8")
            settings = {
                "RELEASE_TAG": TAG,
                "RUNNER_TEMP": str(root / "runner"),
                "FAKE_GH_FIXTURE": str(fixture),
                "FAKE_GH_LOG": str(log),
                "FAKE_GH_ARGS": str(argv_log),
            }
            status, output = self._run_in_shell(path, settings)
            calls = log.read_text(encoding="utf-8").splitlines()
            argv = [json.loads(line) for line in argv_log.read_text(encoding="utf-8").splitlines()]
            return status, calls, output, argv

    def _assert_refused(self, outcome: tuple[int, list[str], str], message: str = "") -> None:
        status, calls, output = outcome
        self.assertNotEqual(status, 0, output)
        self.assertIn(message, output)
        self.assertFalse([call for call in calls if call.startswith("release edit ")])

    def test_publishes_when_every_asset_matches_the_attestation(self) -> None:
        status, calls, output = self._approve()
        self.assertEqual(status, 0, output)
        words = [call.split() for call in calls]
        self.assertEqual(
            [w[0] if w[0] in ("api", "--version") else f"{w[0]} {w[1]}" for w in words],
            [
                "--version",
                "release view",
                "api",
                "release download",
                "attestation verify",
                "release view",
                "api",
                "release edit",
            ],
        )
        self.assertEqual(calls[2], TAG_LOOKUP)
        self.assertEqual(calls[6], TAG_LOOKUP)
        self.assertEqual(
            calls[1], f"release view {TAG} --repo ryanduguid/au-tax-legislation-corpus --json tagName,isDraft,assets,body"
        )
        self.assertRegex(
            calls[3], rf"^release download {TAG} --repo ryanduguid/au-tax-legislation-corpus --dir .+reviewed-live-evidence$"
        )
        self.assertRegex(calls[4], r"^attestation verify .+reviewed-live-evidence[\\/]a\.json ")
        self.assertTrue(
            calls[4].endswith(
                " --repo ryanduguid/au-tax-legislation-corpus"
                " --signer-workflow ryanduguid/au-tax-legislation-corpus/.github/workflows/publish-live-evidence.yml"
                " --source-ref refs/heads/main"
                f" --source-digest {SOURCE_SHA}"
                " --predicate-type https://slsa.dev/provenance/v1"
                " --deny-self-hosted-runners"
                " --format json --jq .[].verificationResult.statement.subject | tojson"
            ),
            calls[4],
        )
        self.assertEqual(
            calls[7], f"release edit {TAG} --repo ryanduguid/au-tax-legislation-corpus --draft=false --latest=false"
        )

    def test_each_gh_call_receives_its_exact_arguments(self) -> None:
        status, _, output, argv = self._run()
        self.assertEqual(status, 0, output)
        repo = ["--repo", "ryanduguid/au-tax-legislation-corpus"]
        view = ["release", "view", TAG, *repo, "--json", "tagName,isDraft,assets,body"]
        tag = [
            "api",
            f"repos/ryanduguid/au-tax-legislation-corpus/git/ref/tags/{TAG}",
            "--jq",
            '.object.type + " " + .object.sha',
        ]
        self.assertEqual(argv[0], ["--version"])
        self.assertEqual(argv[1], view)
        self.assertEqual(argv[2], tag)
        self.assertEqual(argv[3][:-1], ["release", "download", TAG, *repo, "--dir"])
        download_dir = argv[3][-1]
        self.assertTrue(download_dir.endswith("reviewed-live-evidence"), download_dir)
        self.assertEqual(
            argv[4],
            [
                "attestation",
                "verify",
                str(Path(download_dir) / "a.json"),
                *repo,
                "--signer-workflow",
                "ryanduguid/au-tax-legislation-corpus/.github/workflows/publish-live-evidence.yml",
                "--source-ref",
                "refs/heads/main",
                "--source-digest",
                SOURCE_SHA,
                "--predicate-type",
                "https://slsa.dev/provenance/v1",
                "--deny-self-hosted-runners",
                "--format",
                "json",
                "--jq",
                ".[].verificationResult.statement.subject | tojson",
            ],
        )
        self.assertEqual(argv[5], view)
        self.assertEqual(argv[6], tag)
        self.assertEqual(argv[7], ["release", "edit", TAG, *repo, "--draft=false", "--latest=false"])

    def test_publishes_a_single_asset_draft(self) -> None:
        files = {"a.json": self.FILES["a.json"]}
        digests = {"a.json": self.DIGESTS["a.json"]}
        status, _, output = self._approve(
            views=[_draft(digests)], attestations=[_subjects(digests)], files=files
        )
        self.assertEqual(status, 0, output)

    def test_any_verified_attestation_with_the_exact_subject_set_suffices(self) -> None:
        other = _subjects({"a.json": self.DIGESTS["a.json"]})
        status, _, output = self._approve(attestations=[other, _subjects(self.DIGESTS)])
        self.assertEqual(status, 0, output)

    def test_partial_attestations_are_not_combined(self) -> None:
        self._assert_refused(
            self._approve(
                attestations=[_subjects({name: digest}) for name, digest in self.DIGESTS.items()]
            ),
            "do not match the attested candidate set",
        )

    def test_empty_or_null_verification_output_stops_publication(self) -> None:
        for output in ([], ["[]"], ["null"]):
            with self.subTest(output=output):
                self._assert_refused(
                    self._approve(attestations=output), "do not match the attested candidate set"
                )
        with self.subTest(output="malformed"):
            self._assert_refused(self._approve(attestations=["{"]))

    def test_an_unattested_extra_asset_stops_publication(self) -> None:
        files = {**self.FILES, "c.json": b'{"c":3}\n'}
        digests = {name: _digest(data) for name, data in files.items()}
        self._assert_refused(
            self._approve(views=[_draft(digests)], files=files),
            "do not match the attested candidate set",
        )

    def test_an_attested_asset_missing_from_the_draft_stops_publication(self) -> None:
        self._assert_refused(
            self._approve(
                views=[_draft({"a.json": self.DIGESTS["a.json"]})],
                files={"a.json": self.FILES["a.json"]},
            ),
            "do not match the attested candidate set",
        )

    def test_changed_asset_bytes_stop_publication(self) -> None:
        attested = {**self.DIGESTS, "b.json": _digest(b"other")}
        self._assert_refused(
            self._approve(attestations=[_subjects(attested)]),
            "do not match the attested candidate set",
        )

    def test_a_case_only_rename_stops_publication(self) -> None:
        attested = {"a.json": self.DIGESTS["a.json"], "B.json": self.DIGESTS["b.json"]}
        self._assert_refused(
            self._approve(attestations=[_subjects(attested)]),
            "do not match the attested candidate set",
        )

    def test_a_repeated_subject_cannot_stand_in_for_another_asset(self) -> None:
        repeated = json.dumps(
            [{"name": "a.json", "digest": {"sha256": self.DIGESTS["a.json"]}}] * 2
        )
        self._assert_refused(
            self._approve(attestations=[repeated]), "do not match the attested candidate set"
        )

    def test_a_repeated_draft_asset_name_stops_publication(self) -> None:
        draft = json.loads(_draft(self.DIGESTS))
        draft["assets"][1]["name"] = "a.json"
        self._assert_refused(self._approve(views=[json.dumps(draft)]))

    def test_downloaded_bytes_must_match_the_draft_digest(self) -> None:
        reported = {**self.DIGESTS, "b.json": _digest(b"other")}
        self._assert_refused(
            self._approve(views=[_draft(reported)], attestations=[_subjects(reported)]),
            "downloaded draft assets do not match the draft",
        )

    def test_a_download_outside_the_asset_list_stops_publication(self) -> None:
        files = {"a.json": self.FILES["a.json"], "c.json": self.FILES["b.json"]}
        self._assert_refused(
            self._approve(files=files), "downloaded draft assets do not match the draft"
        )

    def test_a_draft_changed_after_verification_stops_publication(self) -> None:
        replaced = _draft({**self.DIGESTS, "b.json": _digest(b"other")})
        renamed = _draft({"a.json": self.DIGESTS["a.json"], "c.json": self.DIGESTS["b.json"]})
        extra = _draft({**self.DIGESTS, "c.json": _digest(b"c")}, candidates=3)
        published = json.loads(_draft(self.DIGESTS))
        published["isDraft"] = False
        undigested = json.loads(_draft(self.DIGESTS))
        del undigested["assets"][0]["digest"]
        for name, later, message in (
            ("digest", replaced, "draft changed during verification"),
            ("name", renamed, "draft changed during verification"),
            ("count", extra, "draft changed during verification"),
            ("published", json.dumps(published), "must be an existing draft"),
            ("digest removed", json.dumps(undigested), "incomplete candidate asset"),
        ):
            with self.subTest(change=name):
                self._assert_refused(
                    self._approve(views=[_draft(self.DIGESTS), later]), message
                )

    def test_a_tag_moved_after_verification_stops_publication(self) -> None:
        for later, message in (
            (f"commit {'2' * 40}", "draft tag moved during verification"),
            (f"tag {SOURCE_SHA}", "must point directly at a commit"),
        ):
            with self.subTest(later=later):
                self._assert_refused(
                    self._approve(tags=[f"commit {SOURCE_SHA}", later]), message
                )

    def test_the_tag_commit_is_the_required_source_digest(self) -> None:
        other = "f" * 40
        status, calls, output = self._approve(tags=[f"commit {other}"])
        self.assertEqual(status, 0, output)
        self.assertIn(f" --source-digest {other} ", calls[4])

    def test_a_tag_that_is_not_a_commit_stops_publication(self) -> None:
        self._assert_refused(
            self._approve(tags=[f"tag {SOURCE_SHA}"]), "must point directly at a commit"
        )

    def test_an_asset_without_a_digest_stops_publication(self) -> None:
        draft = json.loads(_draft(self.DIGESTS))
        del draft["assets"][1]["digest"]
        self._assert_refused(self._approve(views=[json.dumps(draft)]), "incomplete candidate asset")

    def test_every_failed_gh_call_stops_publication(self) -> None:
        for call, message in (
            ("--version", "version could not be read"),
            ("release view#1", "draft could not be read"),
            ("api#1", "draft tag could not be read"),
            ("release download", "could not be downloaded"),
            ("attestation verify", "no verified artifact attestation"),
            ("release view#2", "draft could not be read"),
            ("api#2", "draft tag could not be read"),
        ):
            with self.subTest(call=call):
                self._assert_refused(self._approve(fail=[call]), message)
        status, calls, output = self._approve(fail=["release edit"])
        self.assertNotEqual(status, 0, output)
        self.assertIn("Publication failed", output)
        self.assertTrue(calls[-1].startswith("release edit "))

    def test_an_unsupported_gh_version_stops_before_any_release_call(self) -> None:
        for version in ("gh version 2.101.0 (2026-09-01)", "gh version 2.99", "", "something else"):
            with self.subTest(version=version):
                outcome = self._approve(version=version)
                self._assert_refused(outcome, "2.102.0 or later is required")
                self.assertEqual(outcome[1], ["--version"])
        for version in ("gh version 2.102.0 (2026-09-30)", "gh version 2.110.3", "gh version 3.0.0 (2027-01-01)"):
            with self.subTest(version=version):
                status, _, output = self._approve(version=version)
                self.assertEqual(status, 0, output)


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
                ["--version"],
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
