from __future__ import annotations

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
POLICY_REF = "ryanduguid/release-policy/.github/workflows/release-archive.yml@"


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


if __name__ == "__main__":
    unittest.main()
