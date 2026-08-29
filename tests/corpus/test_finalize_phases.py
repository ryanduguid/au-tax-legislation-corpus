"""Characterisation contracts for the corpus finalisation phases."""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[2]
STAGE = REPO / "fadden"
SUPPORT = (
    "discover.py",
    "versions.py",
    "download.py",
    "extract.py",
    "finalize.py",
    "check_current.py",
    "corpus_paths.py",
    "http_fetch.py",
)
# These are SHA-256 digests of newline-normalised text captured from the exact
# CS-21 parent before any phase interface existed.
OUTPUT_HASHES = {
    "INDEX.md": "4a312fb1aab3b2f0c2499bc82e6c90ff34324a7843c4a4566f4cbddbc50b46ab",
    "LICENCE-NOTICE.md": "736c818cd50e48d1e3052ffd58e9fd238c803a9949c617e9973a85f6753ba915",
    "README.md": "1fa345c021dc674dd7b4cefa9d00b507bb435e6ac0ab6b668a15511949d1a60b",
    "sources.json": "44fb7fea960c7aaf98ce2abc9d642eaaae08872cfc062418950e271921349a0b",
}
STDOUT = (
    "titles=2 (acts=1 instruments=1) rows=3 words=33 epub=0.0 MB "
    "missing=1 whole_act=0\n"
    "by collection: {'Act': 1, 'LegislativeInstrument': 1}\n"
)


def load_finalize(name: str):
    module_dir = str(STAGE)
    sys.path.insert(0, module_dir)
    try:
        spec = importlib.util.spec_from_file_location(name, STAGE / "finalize.py")
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.remove(module_dir)


def canonical_hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def write_fixture(scratch: Path, root: Path) -> None:
    act = {
        "id": "C2026A00001",
        "name": "Zulu Tax Act | Example",
        "collection": "Act",
        "versionStart": "2026-07-01",
        "compilationNumber": "7",
        "epub": "C2026A00001.epub",
        "bytes": 1000,
        "sourceUrl": "https://example.test/C2026A00001",
        "markdown": "C2026A00001/C2026A00001.md",
        "retrieved": "2026-08-01",
        "sections": 2,
        "granularity": "section",
        "words": 11,
        "endnotes": True,
        "long_title": "An Act used only as a fabricated test fixture",
    }
    instrument = {
        "id": "F2026L00002",
        "name": "Alpha Superannuation Instrument",
        "collection": "LegislativeInstrument",
        "versionStart": "2026-06-01",
        "compilationNumber": "2",
        "epub": "F2026L00002.epub",
        "bytes": 2000,
        "sourceUrl": "https://example.test/F2026L00002",
        "markdown": "F2026L00002/F2026L00002.md",
        "retrieved": "2026-08-02",
        "sections": 1,
        "granularity": "table_block",
        "words": 22,
        "endnotes": False,
        "long_title": None,
        "version_is_current": False,
        "current_version_start": "2026-08-03",
    }
    missing = {
        "id": "F2026N00003",
        "name": "Missing Medicare Levy Notice",
        "collection": "NotifiableInstrument",
        "versionStart": "2026-05-01",
        "epub": None,
        "bytes": 0,
        "sourceUrl": "https://example.test/F2026N00003",
        "reason": "current_version_has_no_document",
    }
    (scratch / "manifest_md.json").write_text(
        json.dumps([act, instrument]), encoding="utf-8"
    )
    (scratch / "manifest_raw.json").write_text(
        json.dumps([act, instrument, missing]), encoding="utf-8"
    )
    (scratch / "pii_flagged.json").write_text(
        json.dumps([{"names_est": 123}, {"names_est": 78}]), encoding="utf-8"
    )
    for name in SUPPORT:
        (scratch / name).write_bytes(("support:" + name + "\n").encode("utf-8"))

    rows = {
        "C2026A00001": [
            {"section": "1", "kind": "section", "text": "one"},
            {"section": None, "kind": "container", "text": "two"},
        ],
        "F2026L00002": [
            {"section": None, "kind": "introductory", "text": "three"},
        ],
    }
    for register_id, records in rows.items():
        folder = root / "markdown" / register_id
        folder.mkdir(parents=True)
        payload = "".join(json.dumps(record) + "\n" for record in records)
        (folder / "sections.jsonl").write_text(payload, encoding="utf-8")


class FinalizePhaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.base = Path(self.temporary.name)
        self.scratch = self.base / "build"
        self.root = self.base / "corpus"
        self.scratch.mkdir()
        self.root.mkdir()
        write_fixture(self.scratch, self.root)
        self.finalize = load_finalize(f"finalize_phases_{id(self)}")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def inventory(self):
        raw, markdown = self.finalize.load_retrieval_inventory(self.scratch)
        return self.finalize.assemble_corpus_inventory(raw, markdown, self.root)

    def test_retrieval_inventory_and_assembly_are_direct_exact_phases(self) -> None:
        raw, markdown = self.finalize.load_retrieval_inventory(self.scratch)
        self.assertEqual([entry["id"] for entry in raw], [
            "C2026A00001", "F2026L00002", "F2026N00003",
        ])
        self.assertEqual([entry["id"] for entry in markdown], [
            "C2026A00001", "F2026L00002",
        ])

        inventory = self.finalize.assemble_corpus_inventory(raw, markdown, self.root)
        self.assertEqual([entry["id"] for entry in inventory.missing], ["F2026N00003"])
        self.assertEqual([entry["id"] for entry in inventory.titles], [
            "F2026L00002", "C2026A00001",
        ])
        self.assertEqual([entry["_rows"] for entry in inventory.titles], [1, 2])
        self.assertEqual([entry["_sec_rows"] for entry in inventory.titles], [0, 1])
        self.assertEqual(inventory.rows_by_kind, {
            "introductory": 1, "section": 1, "container": 1,
        })
        self.assertEqual(inventory.by_collection, {
            "LegislativeInstrument": {"titles": 1, "rows": 1, "words": 22},
            "Act": {"titles": 1, "rows": 2, "words": 11},
        })
        self.assertEqual(
            (inventory.total_rows, inventory.section_rows, inventory.total_words,
             inventory.epub_bytes, inventory.acts, inventory.instruments),
            (3, 1, 33, 3000, 1, 1),
        )

    def test_publication_documents_preserve_exact_canonical_bytes(self) -> None:
        inventory = self.inventory()
        documents = {
            "sources.json": json.dumps(
                self.finalize.build_sources_document(inventory, "2026-08-04"),
                indent=1,
                ensure_ascii=False,
            ),
            "INDEX.md": self.finalize.build_index_document(inventory, "2026-08-04"),
            "LICENCE-NOTICE.md": self.finalize.build_licence_notice("2026-08-04"),
            "README.md": self.finalize.build_readme_document(
                inventory, "2026-08-04", pii_titles=2, pii_names=200
            ),
        }
        self.assertEqual(
            {name: canonical_hash(text) for name, text in documents.items()},
            OUTPUT_HASHES,
        )

    def test_finish_publication_phase_preserves_readme_scripts_and_report(self) -> None:
        inventory = self.inventory()
        self.finalize.SCRATCH = str(self.scratch)
        self.finalize.ROOT = str(self.root)
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            self.finalize.finish_corpus_publication(inventory, "2026-08-04")

        readme = (self.root / "README.md").read_text(encoding="utf-8")
        self.assertEqual(canonical_hash(readme), OUTPUT_HASHES["README.md"])
        self.assertEqual(stdout.getvalue(), STDOUT)
        for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
            self.assertEqual((self.root / name).read_bytes(), (self.scratch / name).read_bytes())
        for name in SUPPORT:
            self.assertEqual(
                (self.root / "build" / name).read_bytes(),
                (self.scratch / name).read_bytes(),
            )

    def test_main_keeps_output_stdout_and_support_script_bytes_exact(self) -> None:
        self.finalize.SCRATCH = str(self.scratch)
        self.finalize.ROOT = str(self.root)
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            self.finalize.main("2026-08-04")

        actual = {
            name: canonical_hash((self.root / name).read_text(encoding="utf-8"))
            for name in OUTPUT_HASHES
        }
        self.assertEqual(actual, OUTPUT_HASHES)
        self.assertEqual(stdout.getvalue(), STDOUT)
        for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
            self.assertEqual((self.root / name).read_bytes(), (self.scratch / name).read_bytes())
        for name in SUPPORT:
            self.assertEqual(
                (self.root / "build" / name).read_bytes(),
                (self.scratch / name).read_bytes(),
            )


if __name__ == "__main__":
    unittest.main()
