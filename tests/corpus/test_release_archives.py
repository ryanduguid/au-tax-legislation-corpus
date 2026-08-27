from __future__ import annotations

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
POLICY_COMMIT = "8b4de1ed339f1358b5f3e850b63412d8717d01da"


class ReleaseArchiveTests(unittest.TestCase):
    def test_release_archive_construction_has_one_owner(self) -> None:
        self.assertFalse((ROOT / "tools" / "build_release_archives.py").exists())

    def test_release_workflow_uses_the_shared_archive_policy(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8",
        )
        self.assertIn(
            "ryanduguid/release-policy/.github/workflows/release-archive.yml@"
            + POLICY_COMMIT,
            workflow,
        )
        self.assertIn("artifact-stem: au-tax-legislation-corpus-builder", workflow)
        self.assertNotIn("build_release_archives.py", workflow)
        self.assertNotIn("\n          git archive ", workflow)


if __name__ == "__main__":
    unittest.main()
