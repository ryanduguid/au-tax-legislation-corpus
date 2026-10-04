from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
POLICY_REF = "ryanduguid/release-policy/.github/workflows/release-archive.yml@"
# Set in the simulated preflight run below so the simulation does not nest.
PREFLIGHT_SIMULATION = "AU_TAX_RELEASE_PREFLIGHT_SIMULATION"


class ReleaseArchiveTests(unittest.TestCase):
    def test_release_archive_construction_has_one_owner(self) -> None:
        self.assertFalse((ROOT / "tools" / "build_release_archives.py").exists())

    def test_release_workflow_uses_the_shared_archive_policy(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8",
        )
        self.assertIn(POLICY_REF, workflow)
        # The policy must be pinned to an immutable 40-hex commit, never a
        # branch or a tag. Which commit it names moves with every policy bump
        # and is reviewed in that bump's own diff, so it is not frozen here.
        pin = workflow.split(POLICY_REF, 1)[1].split()[0]
        self.assertEqual(len(pin), 40, pin)
        self.assertTrue(set(pin) <= set("0123456789abcdef"), pin)
        self.assertIn("artifact-stem: au-tax-legislation-corpus-builder", workflow)
        self.assertNotIn("build_release_archives.py", workflow)
        self.assertNotIn("\n          git archive ", workflow)

    def test_release_requires_each_check_once(self) -> None:
        # The release policy refuses a check listed twice, but only once a tag
        # runs it: the v0.2.0 tag stopped there after the floor change renamed
        # the 3.10 leg to 3.11, which was already listed.
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8",
        )
        checks = []
        for line in workflow.split("required-checks: |\n", 1)[1].splitlines():
            if not line.startswith("        .github/workflows/"):
                break
            checks.append(line.strip())
        self.assertTrue(checks)
        self.assertEqual(len(checks), len(set(checks)), checks)

    def test_release_preflight_discovery_runs_without_pytest(self) -> None:
        """The policy's consumer-tests job runs `python -B -m unittest discover
        -s tests` with nothing installed beyond requirements-test.txt, which
        this repository does not ship. The radar suite is pytest-only, so
        tests/radar/__init__.py withholds that package from unittest
        discovery; the v0.1.4 preflight failed on `import pytest` before that
        hook existed. This re-runs the same discovery with pytest hidden
        behind a stub that refuses to import, the way the release runner
        sees it."""
        if os.environ.get(PREFLIGHT_SIMULATION) == "1":
            self.skipTest("already inside the simulated release preflight")
        with tempfile.TemporaryDirectory() as stub_dir:
            (Path(stub_dir) / "pytest.py").write_text(
                "raise ModuleNotFoundError(\"No module named 'pytest'\")\n",
                encoding="utf-8",
            )
            env = dict(os.environ)
            env[PREFLIGHT_SIMULATION] = "1"
            env["PYTHONPATH"] = os.pathsep.join(
                [stub_dir] + [p for p in [env.get("PYTHONPATH", "")] if p]
            )
            result = subprocess.run(
                [sys.executable, "-B", "-m", "unittest", "discover", "-s", "tests"],
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
                timeout=900,
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stderr[-4000:])
        self.assertNotIn("_FailedTest", result.stderr)
        self.assertNotIn("No module named 'pytest'", result.stderr)
        self.assertRegex(result.stderr, r"\nRan [1-9]\d* tests? in ")


if __name__ == "__main__":
    unittest.main()
