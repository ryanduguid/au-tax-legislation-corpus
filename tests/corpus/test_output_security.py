"""Contained paths, atomic writes and publication recovery regressions."""

from __future__ import annotations

import contextlib
import io
import json
import stat
import subprocess  # nosec B404 - isolated Python import regression
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from fadden import corpus_paths, download
from fadden import export_monitor_contract as contract
from fadden import export_publication_bundles as publisher

from tests.corpus import test_regressions as regressions

REPOSITORY = Path(__file__).resolve().parents[2]
STAGE = REPOSITORY / "fadden"
PUBLICATION = Path(__file__).parent / "fixtures" / "publication"


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
        for name in ("versions", "probe13", "check_current"):
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
        for name in ("versions", "probe13"):
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

    def test_rate_rows_must_belong_to_their_directory_even_without_rate_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            module = regressions.load_module("rate_identity", STAGE / "rates.py")
            module.ROOT, module.OUT = str(root), str(root / "rates")
            title = root / "markdown" / "C2099A00001"
            title.mkdir(parents=True)
            (root / "sources.json").write_text(json.dumps({"titles": [{"register_id": title.name}]}), encoding="utf-8")
            (title / "sections.jsonl").write_text(json.dumps({"register_id": "C2099A00002", "text": "No rate here."}) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "does not match"):
                module.main()
            self.assertEqual(list((root / "rates").iterdir()), [])

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
        for name in ("discover", "versions", "download", "probe13", "retry13", "extract",
                     "finalize", "rates", "check_current", "pii_scan", "pii_scan2", "dist"):
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
            for name in ("corpus_paths", "download", "http_fetch", "pii_patterns", "rates", "dist_verify"):
                (hostile / (name + ".py")).write_text('raise AssertionError("bare helper imported")\n', encoding="utf-8")
            script = ("import sys, importlib; sys.path[:0] = " + repr([str(REPOSITORY), str(hostile)]) +
                      "; before = list(sys.path); from fadden import STAGES; import fadden.__main__; "
                      "[importlib.import_module('fadden.' + name) for name in STAGES]; "
                      "assert sys.path == before; assert 'corpus_paths' not in sys.modules")
            # Fixed interpreter and test-owned script, with isolated import state.
            # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-audit.dangerous-subprocess-use-audit
            result = subprocess.run([sys.executable, "-I", "-c", script], cwd=temporary,  # nosec B603
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)

class PublicationRecoveryTests(unittest.TestCase):
    def test_interrupt_before_rollback_iteration_preserves_backups_and_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destinations = {name: root / (name + ".json") for name in ("baseline", "observation")}
            staged = {name: root / (name + ".tmp") for name in destinations}
            for name in destinations:
                destinations[name].write_bytes(("old " + name).encode())
                staged[name].write_bytes(("new " + name).encode())
            replace = contract.os.replace

            def fail_last_promotion(source, destination):
                if Path(source) == staged["observation"]:
                    raise OSError("promotion failed")
                replace(source, destination)

            with mock.patch.object(contract.os, "replace", side_effect=fail_last_promotion), \
                    mock.patch.object(contract, "list", side_effect=KeyboardInterrupt("recovery interrupted"), create=True):
                with self.assertRaises(KeyboardInterrupt):
                    contract._publish(staged, destinations)
            self.assertTrue((root / contract.PUBLISH_LOCK_FILENAME).exists())
            self.assertEqual({path.read_bytes() for path in root.glob(".*.bak")},
                             {b"old baseline", b"old observation"})

    def test_backup_collision_preserves_a_dangling_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destinations = {name: root / (name + ".json") for name in ("baseline", "observation")}
            staged = {name: root / (name + ".tmp") for name in destinations}
            for name in destinations:
                destinations[name].write_bytes(("old " + name).encode())
                staged[name].write_bytes(("new " + name).encode())
            collision = root / ".baseline.json.monitor-contract-fixed.bak"
            try:
                collision.symlink_to(root / "missing")
            except OSError as exc:
                self.skipTest(f"file symlinks unavailable: {exc}")
            with mock.patch.object(contract.uuid, "uuid4", return_value=SimpleNamespace(hex="fixed")):
                with self.assertRaisesRegex(contract.ContractError, "already exists"):
                    contract._publish(staged, destinations)
            self.assertTrue(collision.is_symlink())
            self.assertEqual(destinations["baseline"].read_bytes(), b"old baseline")
            self.assertFalse((root / contract.PUBLISH_LOCK_FILENAME).exists())

    def test_producer_fractional_timestamp_grammar_is_portable(self):
        for length in range(1, 7):
            timestamp = "2099-01-01T12:34:56." + "1" * length + "Z"
            with self.subTest(timestamp=timestamp):
                self.assertEqual(contract._utc_timestamp(timestamp, "checked_at"), timestamp)

    def test_monitor_staging_collision_preserves_unowned_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "baseline.json"
            collision = destination.with_name(".baseline.json.monitor-contract-fixed.tmp")
            collision.write_bytes(b"unowned")
            with mock.patch.object(contract.uuid, "uuid4", return_value=SimpleNamespace(hex="fixed")):
                with self.assertRaises(FileExistsError):
                    contract._write_staged(destination, {})
            self.assertEqual(collision.read_bytes(), b"unowned")

    def test_monitor_interruptions_after_each_rename_restore_the_old_pair(self):
        replace = contract.os.replace
        for interrupt_at in range(1, 5):
            with self.subTest(rename=interrupt_at), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                destinations = {name: root / (name + ".json") for name in ("baseline", "observation")}
                staged = {name: root / (name + ".tmp") for name in destinations}
                for name in destinations:
                    destinations[name].write_bytes(("old " + name).encode())
                    staged[name].write_bytes(("new " + name).encode())
                calls = 0
                interrupted = False

                def interrupt_after_replace(source, destination):
                    nonlocal calls, interrupted
                    replace(source, destination)
                    if not interrupted:
                        calls += 1
                        if calls == interrupt_at:
                            interrupted = True
                            raise KeyboardInterrupt("interrupted after a completed rename")

                with mock.patch.object(contract.os, "replace", side_effect=interrupt_after_replace):
                    with self.assertRaises(KeyboardInterrupt):
                        contract._publish(staged, destinations)
                for name in destinations:
                    self.assertEqual(destinations[name].read_bytes(), ("old " + name).encode())
                self.assertFalse((root / contract.PUBLISH_LOCK_FILENAME).exists())
                self.assertEqual(list(root.glob(".*.bak")), [])

    def test_empty_destination_race_is_refused_by_the_promotion_primitive(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "bundles"
            promote = publisher._promote_no_replace
            identity = []

            def competitor_appears(staging, destination):
                destination.mkdir()
                identity.append(destination.stat().st_ino)
                promote(staging, destination)

            with mock.patch.object(publisher, "_promote_no_replace", side_effect=competitor_appears):
                with self.assertRaisesRegex(publisher.PublicationBundleError, "could not be promoted"):
                    publisher.export_publication_bundles(PUBLICATION / "sample-sources.json",
                                                        PUBLICATION / "sample-observation-facts-v3.json", output)
            self.assertEqual(output.stat().st_ino, identity[0])
            self.assertEqual(list(output.iterdir()), [])
            self.assertEqual(list(output.parent.glob(".*.tmp")), [])

    def test_installed_producer_version_comes_from_distribution_metadata(self):
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(publisher, "VERSION_PATH", Path(temporary) / "absent"), \
                mock.patch.object(publisher, "version", return_value="0.1.7") as installed_version:
            self.assertEqual(publisher._producer_version(), "0.1.7")
        installed_version.assert_called_once_with("tax-radar-au")

    def test_publisher_url_rejects_other_hosts_ports_and_suffixes(self):
        path = "/C2099A00001/latest/text"
        for url in ("https://example.test" + path, "https://www.legislation.gov.au:444" + path,
                    "https://www.legislation.gov.au" + path + "?redirect=other",
                    "https://www.legislation.gov.au" + path + "#other"):
            with self.subTest(url=url), self.assertRaises(publisher.PublicationBundleError):
                publisher._publisher_https_url(url, "canonical URL", expected_path=path)
        self.assertEqual(publisher._publisher_https_url("https://www.legislation.gov.au" + path,
                         "canonical URL", expected_path=path), "https://www.legislation.gov.au" + path)

    def test_reparse_staging_is_not_recursively_removed(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            staging = parent / ".output.publication-bundles-test.tmp"
            staging.mkdir()
            details = SimpleNamespace(st_mode=stat.S_IFDIR,
                                      st_file_attributes=getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))
            with mock.patch.object(publisher.os, "lstat", return_value=details), \
                    mock.patch.object(publisher.shutil, "rmtree") as remove:
                with self.assertRaises(publisher.PublicationBundleError):
                    publisher._remove_owned_staging(staging, parent=parent, prefix=".output.publication-bundles-")
            remove.assert_not_called()

    def test_staging_collision_preserves_the_existing_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            staging = root / ".output.publication-bundles-fixed.tmp"
            staging.mkdir()
            marker = staging / "unrelated.txt"
            marker.write_text("keep", encoding="utf-8")
            with mock.patch.object(publisher.uuid, "uuid4", return_value=SimpleNamespace(hex="fixed")):
                with self.assertRaisesRegex(publisher.PublicationBundleError, "could not be written"):
                    publisher.export_publication_bundles(PUBLICATION / "sample-sources.json",
                        PUBLICATION / "sample-observation-facts-v3.json", root / "output")
            self.assertEqual(marker.read_text(encoding="utf-8"), "keep")

    def test_cleanup_failure_keeps_the_primary_failure(self):
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(publisher, "_write_bundle", side_effect=OSError("primary write failure")), \
                mock.patch.object(publisher, "_remove_owned_staging", side_effect=publisher.PublicationBundleError("cleanup failure")):
            with self.assertRaisesRegex(publisher.PublicationBundleError, "primary write failure") as caught:
                publisher.export_publication_bundles(PUBLICATION / "sample-sources.json",
                    PUBLICATION / "sample-observation-facts-v3.json", Path(temporary) / "output")
            self.assertIn("cleanup failure", str(caught.exception.__cause__))

    def test_owned_lock_cleanup_failure_is_reported(self):
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.object(contract, "_remove", side_effect=OSError("unlink failed")), \
                    self.assertRaisesRegex(contract.ContractError, "lock could not be removed"):
                with contract._OutputDirectoryLock(Path(temporary)):
                    pass


if __name__ == "__main__":
    unittest.main()
