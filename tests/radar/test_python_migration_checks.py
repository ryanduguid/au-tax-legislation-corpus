"""Keep the retained interpreter jobs, coverage producer and release names aligned."""
from __future__ import annotations

import re
import unittest
from pathlib import Path

from tests.radar.test_repository_guidance import _workflow_matrix_values

ROOT = Path(__file__).resolve().parents[2]


class PythonMigrationChecks(unittest.TestCase):
    def _ci(self) -> str:
        return (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

    def test_retained_matrix_includes_the_supported_windows_subset(self) -> None:
        ci = self._ci()
        self.assertEqual(_workflow_matrix_values(ci, "python"), ["3.14", "3.15"])
        self.assertRegex(ci, r'os: windows-latest\s+python: "3\.14"')
        self.assertIn("test_live_evidence_publication_paths.py", ci)

    def test_coverage_and_artifact_production_select_the_surviving_leg(self) -> None:
        ci = self._ci()
        names = (
            "run tests with whole-package branch coverage",
            "write whole-package coverage for Codecov",
            "keep the coverage report for the Codecov upload",
            "hold whole-package coverage at the configured floor",
        )
        for name in names:
            with self.subTest(step=name):
                marker = "      - name: " + name + "\n"
                self.assertEqual(ci.count(marker), 1)
                body = ci.split(marker, 1)[1]
                body = re.split(r"(?m)^      - ", body, maxsplit=1)[0]
                self.assertIn("if: matrix.os == 'ubuntu-latest' && matrix.python == '3.14'", body)
        self.assertIn("matrix.python != '3.14'", ci)
        self.assertIn("UV_PYTHON: ${{ matrix.python }}", ci)

    def test_release_selectors_match_every_retained_test_and_package_job(self) -> None:
        release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        selected = re.findall(r"(?m)^        \.github/workflows/ci.yml: (.+)$", release)
        # The policy matches the shortened name reported by GitHub, not the full matrix value.
        windows = "test (windows-latest, 3.14, tests/radar tests/corpus/test_live_evidence_bundle_export.py tests/co..."
        self.assertEqual(selected, [
            "lint", "package", "test (ubuntu-latest, 3.14, tests)",
            "test (ubuntu-latest, 3.15, tests)", windows,
            "tests-gates",
        ])
