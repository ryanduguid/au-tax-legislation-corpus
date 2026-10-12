"""Replay the fuzz seed corpus so a broken harness fails ordinary CI."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from fuzz import privacy_patterns  # noqa: E402


class FuzzHarnessTests(unittest.TestCase):
    def test_seed_corpus(self) -> None:
        seeds = sorted((ROOT / "fuzz" / "corpus").iterdir())
        self.assertGreater(len(seeds), 0)
        for seed in seeds:
            with self.subTest(seed=seed.name):
                privacy_patterns.test_one_input(seed.read_bytes())

    def test_a_fingerprint_carrying_the_identifier_is_caught(self) -> None:
        leaked = iter([("email", "jane@example.test")])
        patterns = privacy_patterns.pii_patterns
        with mock.patch.object(patterns, "contact_fingerprints", return_value=leaked):
            with self.assertRaisesRegex(AssertionError, "malformed fingerprint"):
                privacy_patterns.test_one_input(b"")

    def test_a_match_outside_the_text_is_caught(self) -> None:
        patterns = privacy_patterns.pii_patterns
        with mock.patch.object(
            patterns, "private_person_registration_details", return_value=({"Jo"}, set())
        ):
            with self.assertRaisesRegex(AssertionError, "not in the text"):
                privacy_patterns.test_one_input(b"")


if __name__ == "__main__":
    unittest.main()
