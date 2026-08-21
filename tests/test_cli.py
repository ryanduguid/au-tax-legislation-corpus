"""CLI dispatcher for the packaged pipeline stages."""

from __future__ import annotations

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

    def test_stage_dispatch_forwards_remaining_arguments(self) -> None:
        with mock.patch("importlib.import_module") as importer:
            module = mock.Mock()
            module.main.return_value = 0
            importer.return_value = module
            self.assertEqual(main(["export_monitor_contract", "--", "a.json", "b.json", "--out", "out"]), 0)
            importer.assert_called_once_with("fadden.export_monitor_contract")
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
            ),
        )


if __name__ == "__main__":
    unittest.main()
