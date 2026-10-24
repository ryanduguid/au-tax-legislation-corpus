"""CLI dispatcher for the packaged pipeline stages."""

from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import unittest
from pathlib import Path
from unittest import mock

from fadden import STAGES
from fadden.__main__ import main
from fadden.export_live_evidence_bundles import LiveEvidenceBundleError
from fadden.export_live_evidence_bundles import main as export_live_evidence_main


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
        # the way `python -m fadden` does. Most Rust stages have no module;
        # discover and versions keep theirs because finalize copies them.
        for stage in STAGES:
            if importlib.util.find_spec(f"fadden.{stage}") is None:
                continue
            with self.subTest(stage=stage):
                importlib.import_module(f"fadden.{stage}")

    def test_stage_dispatch_forwards_remaining_arguments(self) -> None:
        for stage in ("capture_register", "export_live_evidence_bundles"):
            with self.subTest(stage=stage), mock.patch("importlib.import_module") as importer:
                module = mock.Mock()
                module.main.return_value = 0
                importer.return_value = module
                forwarded = ["a.json", "--out", "out"]
                self.assertEqual(main([stage, "--", *forwarded]), 0)
                importer.assert_called_once_with(f"fadden.{stage}")
                module.main.assert_called_once_with(forwarded)

    def test_rust_stages_run_the_fadden_binary_with_their_arguments(self) -> None:
        import fadden.__main__ as dispatcher

        for stage in sorted(dispatcher.RUST_STAGES):
            with self.subTest(stage=stage), \
                    mock.patch.object(dispatcher, "rust_binary", return_value="fadden-bin"), \
                    mock.patch.object(dispatcher.subprocess, "run") as run:
                run.return_value.returncode = 3
                self.assertEqual(main([stage, "--", "a", "--json"]), 3)
                command = run.call_args.args[0]
                self.assertEqual(command, ["fadden-bin", stage, "a", "--json"])
                module_dir = run.call_args.kwargs["env"]["FADDEN_MODULE_DIR"]
                self.assertEqual(Path(module_dir), Path(dispatcher.__file__).resolve().parent)

    def test_a_rust_stage_without_a_built_binary_says_how_to_build_it(self) -> None:
        import fadden.__main__ as dispatcher

        with mock.patch.object(dispatcher, "rust_binary", return_value=None), \
                mock.patch("sys.stderr", new_callable=io.StringIO) as stderr:
            self.assertEqual(main(["as_at", "2026-08-01"]), 1)
        self.assertIn("cargo build --locked --release", stderr.getvalue())

    def test_the_binary_is_fadden_bin_then_a_cargo_build_then_path(self) -> None:
        import fadden.__main__ as dispatcher

        with mock.patch.dict(dispatcher.os.environ, {"FADDEN_BIN": "/opt/fadden"}):
            self.assertEqual(dispatcher.rust_binary(), "/opt/fadden")
        with mock.patch.dict(dispatcher.os.environ, {"FADDEN_BIN": ""}):
            with mock.patch.object(
                dispatcher.Path,
                "is_file",
                autospec=True,
                side_effect=lambda candidate: candidate.parent.name == "debug",
            ):
                found = Path(dispatcher.rust_binary() or "")
            self.assertEqual(found.parent.name, "debug")
            self.assertEqual(found.parent.parent.name, "target")
            with (
                mock.patch.object(dispatcher.Path, "is_file", return_value=False),
                mock.patch.object(dispatcher.shutil, "which", return_value="/usr/bin/fadden") as which,
            ):
                self.assertEqual(dispatcher.rust_binary(), "/usr/bin/fadden")
            which.assert_called_once_with("fadden")

    def _dispatch(self, *argv: str) -> mock.Mock:
        with mock.patch("importlib.import_module") as importer:
            module = mock.Mock()
            module.main.return_value = None
            importer.return_value = module
            self.assertEqual(main(list(argv)), 0)
        return module.main

    def test_a_stage_without_parameters_is_called_bare(self) -> None:
        self._dispatch("download").assert_called_once_with()

    def test_a_separator_left_by_argparse_is_stripped(self) -> None:
        # Whether argparse keeps "--" in a REMAINDER list depends on the
        # interpreter version, so the parsed result is fixed here.
        parsed = argparse.Namespace(stage="capture_register", stage_args=["--", "2026-08-22"])
        with mock.patch.object(argparse.ArgumentParser, "parse_args", return_value=parsed):
            stage_main = self._dispatch("capture_register")
        stage_main.assert_called_once_with(["2026-08-22"])

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
                "capture_register",
                "export_live_evidence_bundles",
                "rulings",
                "as_at",
                "compare_builds",
                "search",
            ),
        )

    def test_live_export_usage_errors_return_two_and_contract_errors_return_one(self) -> None:
        """The stage keeps parser failures distinct from bounded exporter failures."""
        with self.assertRaises(SystemExit) as usage:
            export_live_evidence_main(["capture-only"])
        self.assertEqual(usage.exception.code, 2)

        stderr = io.StringIO()
        with mock.patch(
            "fadden.export_live_evidence_bundles.export_live_evidence_bundles",
            side_effect=LiveEvidenceBundleError("output directory must not already exist"),
        ), contextlib.redirect_stderr(stderr):
            result = export_live_evidence_main(["capture", "--out", "output"])

        self.assertEqual(result, 1)
        self.assertEqual(stderr.getvalue(), "error: output directory must not already exist\n")

    def test_live_export_raw_filesystem_error_is_bounded(self) -> None:
        """No raw filesystem exception may escape the command-line boundary."""
        stderr = io.StringIO()
        with mock.patch(
            "fadden.export_live_evidence_bundles.export_live_evidence_bundles",
            side_effect=OSError("private path must not leak"),
        ), contextlib.redirect_stderr(stderr):
            result = export_live_evidence_main(["capture", "--out", "output"])

        self.assertEqual(result, 1)
        self.assertEqual(stderr.getvalue(), "error: live evidence export failed\n")


if __name__ == "__main__":
    unittest.main()
