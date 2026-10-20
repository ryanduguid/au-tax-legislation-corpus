"""The live evidence export leaves the synthetic publication fixture unchanged.

The publication exporter is the Rust fadden binary; its contract tests are
tests/fadden_publication_bundles.rs.
"""

from __future__ import annotations

import hashlib
import os
import tempfile
import unittest
from pathlib import Path

FIXTURES = Path(__file__).parent / "fixtures" / "publication"


class PublicationBundleExportTests(unittest.TestCase):
    @unittest.skipUnless(
        os.name == "nt",
        "identity-bound live evidence publication is supported only on Windows",
    )
    def test_live_v2_export_does_not_change_the_synthetic_v1_fixture(self):
        """A live-contract change must not alter synthetic-v1 conformance bytes."""
        fixture = FIXTURES / "evidence-bundle.v1.json"
        before = hashlib.sha256(fixture.read_bytes()).hexdigest()

        from fadden.export_live_evidence_bundles import export_live_evidence_bundles

        from tests.corpus.test_live_evidence_bundle_export import (
            LiveEvidenceBundleContractTests,
        )

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = LiveEvidenceBundleContractTests()._write_capture(root)
            export_live_evidence_bundles(capture, root / "output")

        after = hashlib.sha256(fixture.read_bytes()).hexdigest()
        self.assertEqual(after, before)


if __name__ == "__main__":
    unittest.main()
