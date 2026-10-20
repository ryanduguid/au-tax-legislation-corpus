"""Contained paths, atomic writes and publication recovery regressions."""

from __future__ import annotations

import contextlib
import io
import json
import subprocess  # nosec B404 - isolated Python import regression
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fadden import corpus_paths, download

from tests.corpus import test_regressions as regressions

REPOSITORY = Path(__file__).resolve().parents[2]
STAGE = REPOSITORY / "fadden"


class PathAndWriteTests(unittest.TestCase):
    def test_full_fraction_grammar_is_portable_and_flat_fadden_roots_work(self):
        for fraction in ("", *("." + "1" * length for length in range(1, 8))):
            for offset in ("", "Z", "+10:00", "-00:59"):
                value = "2099-01-01T12:34:56" + fraction + offset
                with self.subTest(timestamp=value):
                    self.assertEqual(corpus_paths.version_date(value), "2099-01-01")
        with tempfile.TemporaryDirectory() as temporary:
            flat = Path(temporary) / "fadden"
            flat.mkdir()
            (flat / "sources.json").write_text("{}", encoding="utf-8")
            corpus_paths.require_builder_layout(flat / "check_current.py")

    def test_unpublished_title_preserves_an_unowned_fixed_temporary_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            epub = root / "epub"
            epub.mkdir()
            foreign = epub / "C2099A00001.epub.meta.json.tmp"
            foreign.write_bytes(b"unowned")
            (root / "acts_resolved.json").write_text(json.dumps([{
                "id": "C2099A00001", "name": "Synthetic unpublished title", "versionStart": "2099-01-01",
                "compilationRegisterId": None,
            }]), encoding="utf-8")
            with mock.patch.object(download, "SCRATCH", str(root)), \
                    mock.patch.object(download, "EPUB_DIR", str(epub)), \
                    mock.patch.object(download, "fetch") as fetch, contextlib.redirect_stdout(io.StringIO()):
                download.main()
            self.assertEqual(foreign.read_bytes(), b"unowned")
            fetch.assert_not_called()

    def test_network_stages_reject_filter_injection_before_requesting(self):
        for name in ("versions", "check_current"):
            with self.subTest(stage=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                module = regressions.load_module("injection_" + name, STAGE / (name + ".py"))
                injected = "C2099A00001' or isCurrent eq true"
                if name == "versions":
                    module.SCRATCH = str(root)
                    (root / "titles_all.json").write_text(json.dumps([
                        {"id": injected, "name": "Synthetic", "isPrincipal": True}
                    ]), encoding="utf-8")
                elif name == "probe13":
                    module.SCRATCH = str(root)
                    (root / "manifest_raw.json").write_text(json.dumps([
                        {"id": injected, "name": "Synthetic", "epub": None}
                    ]), encoding="utf-8")
                else:
                    module.ROOT = str(root)
                    (root / "sources.json").write_text(json.dumps({
                        "retrieved": "2099-01-01", "titles": [{"register_id": injected}]
                    }), encoding="utf-8")
                with mock.patch.object(module, "fetch_json") as fetch, \
                        mock.patch.object(sys, "argv", [name]), contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(ValueError):
                        module.main()
                fetch.assert_not_called()

    def test_malformed_version_rows_cannot_replace_resolved_or_probe_manifests(self):
        for name in ("versions",):
            for response in ({"value": 1}, {"value": ["wrong shape"]},
                             {"value": [{"titleId": "C2099A00002"}]},
                             {"value": [{"titleId": "C2099A00001", "start": "2099-02-30", "registerId": None}]},
                             {"value": [{"titleId": "C2099A00001", "start": "2099-01-01"}]}):
                with self.subTest(stage=name, response=response), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    module = regressions.load_module("bad_rows_" + name, STAGE / (name + ".py"))
                    module.SCRATCH = str(root)
                    input_name = "titles_all.json" if name == "versions" else "manifest_raw.json"
                    output_name = "acts_resolved.json" if name == "versions" else "probe13.json"
                    (root / input_name).write_text(json.dumps([{
                        "id": "C2099A00001", "name": "Synthetic", "isPrincipal": True, "epub": None
                    }]), encoding="utf-8")
                    output = root / output_name
                    output.write_text("previous manifest", encoding="utf-8")
                    with mock.patch.object(module, "fetch_json", return_value=response), \
                            mock.patch.object(module.time, "sleep"), contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaises((ValueError, RuntimeError)):
                            module.main()
                    self.assertEqual(output.read_text(encoding="utf-8"), "previous manifest")

    def test_bad_download_dates_never_reach_fetch_or_change_existing_documents(self):
        for name in ("download", "retry13"):
            with self.subTest(stage=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                module = regressions.load_module("date_url_" + name, STAGE / (name + ".py"))
                module.SCRATCH = str(root)
                module.EPUB_DIR = str(root / "epub")
                item = {"id": "C2099A00001", "name": "Synthetic", "epub": None,
                        "versionStart": "2099-01-01/../../unexpected", "compilationRegisterId": "C2099C00001"}
                (root / ("acts_resolved.json" if name == "download" else "manifest_raw.json")).write_text(
                    json.dumps([item]), encoding="utf-8")
                if name == "retry13":
                    (root / "probe13.json").write_text(json.dumps([{
                        "id": item["id"], "latest_doc": {"start": "2099-01-01/../../unexpected", "registerId": "C2099C00001"}
                    }]), encoding="utf-8")
                owner = module if name == "download" else module.dl
                with mock.patch.object(owner, "fetch") as fetch, contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaises(ValueError):
                        module.main()
                fetch.assert_not_called()
                self.assertEqual(list((root / "epub").glob("*")), [])

    def test_identifiers_and_complete_dates_are_checked(self):
        for value in ("C2099A00001' or true", "C２０９９A00001", "../C2099A00001", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus_paths.register_id(value)
        for value in ("2099-02-30", "2099-01-01/../../", "2099-01-01T99:00:00Z",
                      "2099-01-01T00:00:00+00:99", "2099-01-01junk", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus_paths.version_date(value)
        self.assertEqual(corpus_paths.version_date("2099-01-01T00:00:00.1234567Z"), "2099-01-01")

    def test_atomic_writer_preserves_prior_bytes_and_unowned_temp(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manifest.json"
            path.write_text("previous\n", encoding="utf-8")
            foreign = Path(str(path) + ".tmp")
            foreign.write_text("foreign staging\n", encoding="utf-8")
            with self.assertRaises(KeyboardInterrupt):
                with corpus_paths.atomic_text_writer(path) as target:
                    target.write("partial replacement")
                    raise KeyboardInterrupt
            self.assertEqual(path.read_text(encoding="utf-8"), "previous\n")
            self.assertEqual(foreign.read_text(encoding="utf-8"), "foreign staging\n")
            self.assertEqual(set(path.parent.iterdir()), {path, foreign})
            with corpus_paths.atomic_text_writer(path) as target:
                target.write("complete replacement\n")
            self.assertEqual(path.read_text(encoding="utf-8"), "complete replacement\n")

    def test_installed_locations_refuse_legacy_stages_before_side_effects(self):
        for name in ("discover", "versions", "download", "retry13", "extract",
                     "finalize", "check_current"):
            module = regressions.load_module("installed_guard_" + name, STAGE / (name + ".py"))
            with self.subTest(stage=name), tempfile.TemporaryDirectory() as temporary, \
                    mock.patch.object(module, "__file__", str(Path(temporary) / "fadden" / (name + ".py"))):
                with self.assertRaisesRegex(RuntimeError, "source checkout"):
                    if name == "finalize":
                        module.main("2026-10-01")
                    else:
                        module.main()
                self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_clean_package_imports_ignore_bare_helper_modules(self):
        with tempfile.TemporaryDirectory() as temporary:
            hostile = Path(temporary)
            for name in ("corpus_paths", "download", "http_fetch"):
                (hostile / (name + ".py")).write_text('raise AssertionError("bare helper imported")\n', encoding="utf-8")
            script = ("import sys, importlib; sys.path[:0] = " + repr([str(REPOSITORY), str(hostile)]) +
                      "; before = list(sys.path); from fadden import STAGES; import fadden.__main__; "
                      "[importlib.import_module('fadden.' + name) for name in STAGES if name not in fadden.__main__.RUST_STAGES]; "
                      "assert sys.path == before; assert 'corpus_paths' not in sys.modules")
            # Fixed interpreter and test-owned script, with isolated import state.
            # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-audit.dangerous-subprocess-use-audit
            result = subprocess.run([sys.executable, "-I", "-c", script], cwd=temporary,  # nosec B603
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)

if __name__ == "__main__":
    unittest.main()
