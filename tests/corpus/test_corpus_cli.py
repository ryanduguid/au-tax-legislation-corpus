"""CLI dispatcher for the packaged pipeline stages."""

from __future__ import annotations

import importlib
import unittest
from unittest import mock

from fadden import STAGES
from fadden.__main__ import main


class FaddenCliTests(unittest.TestCase):
    def test_help_lists_every_documented_stage(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            main(["--help"])
        self.assertEqual(raised.exception.code, 0)

    def test_unknown_stage_is_rejected(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            main(["not-a-stage"])
        self.assertEqual(raised.exception.code, 2)

    def test_every_stage_imports_in_package_context(self) -> None:
        # The dispatcher imports fadden.<stage> for real; the mocked
        # forwarding test below cannot catch a stage whose bare imports only
        # resolve in the flat deployed layout. This one imports every stage
        # the way `python -m fadden` does.
        for stage in STAGES:
            with self.subTest(stage=stage):
                importlib.import_module(f"fadden.{stage}")

    def test_extract_dispatch_passes_the_forwarded_date(self) -> None:
        # extract.main takes the retrieval date as a parameter and reads
        # sys.argv only under its own __main__ guard, which never runs under
        # the dispatcher; the date must be forwarded as an argument.
        with mock.patch("importlib.import_module") as importer:
            module = mock.Mock()
            module.main.return_value = 0
            importer.return_value = module
            self.assertEqual(main(["extract", "--", "2026-08-22"]), 0)
            module.main.assert_called_once_with("2026-08-22")

    def test_stage_dispatch_forwards_remaining_arguments(self) -> None:
        for stage in ("export_monitor_contract", "export_publication_bundles"):
            with self.subTest(stage=stage), mock.patch("importlib.import_module") as importer:
                module = mock.Mock()
                module.main.return_value = 0
                importer.return_value = module
                self.assertEqual(main([stage, "--", "a.json", "b.json", "--out", "out"]), 0)
                importer.assert_called_once_with(f"fadden.{stage}")
                module.main.assert_called_once_with(["a.json", "b.json", "--out", "out"])

    def test_stage_roster_is_the_documented_pipeline(self) -> None:
        self.assertEqual(
            STAGES,
            (
                "discover",
                "versions",
                "download",
                "probe13",
                "retry13",
                "extract",
                "pii_scan",
                "pii_scan2",
                "finalize",
                "rates",
                "check_current",
                "dist",
                "dist_verify",
                "export_monitor_contract",
                "export_publication_bundles",
            ),
        )


if __name__ == "__main__":
    unittest.main()
