"""Tests for the producer-side AU tax monitor-contract export.

The monitor owns its own strict schemas and evidence digests.  These tests
therefore prove that the corpus projects an independently serialised pair that
the existing monitor can consume unchanged; this repository never imports the
monitor at runtime. The exporter is the Rust fadden binary; its own tests are
tests/fadden_monitor_contract.rs.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from fadden.__main__ import rust_binary


def sources_document() -> dict:
    return {
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": "2026-08-20",
        "source": "Federal Register of Legislation",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "licence": "CC BY 4.0",
        "titles": [
            {
                "register_id": "F2099L00001",
                "name": "Sample Instrument",
                "collection": "LegislativeInstrument",
                "compilation_number": "1",
                "compilation_date": "2099-07-01",
                "version_is_current": True,
                "current_version_start": None,
                "retrieved": "2026-08-20",
                "source_url": "https://example.test/F2099L00001/1",
                "register_page": "https://example.test/F2099L00001/latest/text",
                "words": 42,
            },
            {
                "register_id": "C2099A00001",
                "name": "Sample Act",
                "collection": "Act",
                "compilation_number": "2",
                "compilation_date": "2099-08-01",
                "version_is_current": True,
                "current_version_start": None,
                "retrieved": "2026-08-20",
                "source_url": "https://example.test/C2099A00001/2",
                "register_page": "https://example.test/C2099A00001/latest/text",
                "sections_jsonl": "markdown/C2099A00001/sections.jsonl",
            },
        ],
    }


def observation_facts(*, complete: bool = True) -> dict:
    return {
        "schema_version": "au-tax-register-observation-facts.v1",
        "observed_at": "2026-08-21T10:00:00Z",
        "complete": complete,
        "observations": [
            {
                "register_id": "F2099L00001",
                "collection": "LegislativeInstrument",
                "state": "UNCHANGED",
                "observed_compilation_number": None,
                "observed_compilation_date": None,
                "observed_register_document_id": None,
                "current_version_start": None,
                "evidence_url": "https://example.test/F2099L00001/latest/text",
                "checked_at": "2026-08-21T10:00:01Z",
                "error_category": None,
            },
            {
                "register_id": "C2099A00001",
                "collection": "Act",
                "state": "SUPERSEDED",
                "observed_compilation_number": "3",
                "observed_compilation_date": "2099-09-01",
                "observed_register_document_id": "C2099C00003",
                "current_version_start": None,
                "evidence_url": "https://example.test/C2099A00001/latest/text",
                "checked_at": "2026-08-21T10:00:02Z",
                "error_category": None,
            },
        ],
    }


def _radar_binary() -> Path | None:
    """Return the Rust tax-radar-au binary, or None when it has not been built.

    TAX_RADAR_AU_BIN names it, relative to the repository root when not
    absolute; otherwise a Cargo release or debug build in target/ is used.
    """
    root = Path(__file__).resolve().parents[2]
    named = os.environ.get("TAX_RADAR_AU_BIN")
    suffix = ".exe" if os.name == "nt" else ""
    candidates = (
        [root / named]
        if named
        else [root / "target" / build / f"tax-radar-au{suffix}" for build in ("release", "debug")]
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate
        if named and candidate.with_name(candidate.name + suffix).is_file():
            return candidate.with_name(candidate.name + suffix)
    return None


class MonitorContractTests(unittest.TestCase):
    def test_build_docs_state_the_adapter_is_review_only_and_never_contacts_the_register(self):
        root = Path(__file__).resolve().parents[2]
        readme = (root / "docs" / "monitor-contract.md").read_text(encoding="utf-8")
        build = (root / "BUILD.md").read_text(encoding="utf-8")

        self.assertIn("src/fadden/monitor_contract.rs", readme)
        self.assertIn("au-tax-register-observation-facts.v1", readme)
        self.assertIn("does not call the Register", readme)
        self.assertIn("src/fadden/monitor_contract.rs", build)
        self.assertIn("monitor-baseline.json", build)
        self.assertIn("register-observation.json", build)
        self.assertIn("replaced individually", readme)
        self.assertIn("one writer", readme)
        self.assertIn("power loss", readme)
        self.assertIn("operator", readme)
        self.assertIn("replaced individually", build)
        self.assertIn("one writer", build)
        self.assertIn("power loss", build)
        self.assertIn("operator", build)
        self.assertIn("rollback itself fails", readme)
        self.assertIn("rollback itself fails", build)
        self.assertIn(".bak", readme)
        self.assertIn(".bak", build)
        for document in (readme, build):
            self.assertIn("30 seconds", document)
            self.assertIn("known consistent", document)
            self.assertIn("regenerate", document)
            self.assertIn("do not prove consistency", document)

    def test_generated_pair_is_accepted_by_the_radar_binary_when_built(self):
        binary = _radar_binary()
        exporter = rust_binary()
        if binary is None or exporter is None:
            self.skipTest("the Rust binaries are not built; run cargo build --release")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = root / "sources.json"
            facts = root / "facts.json"
            mapping = root / "map.json"
            sources.write_text(json.dumps(sources_document()), encoding="utf-8")
            facts.write_text(json.dumps(observation_facts()), encoding="utf-8")
            mapping.write_text(
                json.dumps(
                    {
                        "schema_version": "au-tax-source-skill-map.v1",
                        "mapping_version": "test.1",
                        "entries": [],
                    }
                ),
                encoding="utf-8",
            )
            subprocess.run(  # noqa: S603 - this checkout's build
                [exporter, "export_monitor_contract", str(sources), str(facts),
                 "--out", str(root / "out")],
                check=True,
                capture_output=True,
            )
            subprocess.run(  # noqa: S603 - this checkout's build
                [
                    str(binary), "compare",
                    "--baseline", str(root / "out" / "monitor-baseline.json"),
                    "--observation", str(root / "out" / "register-observation.json"),
                    "--map", str(mapping),
                    "--out", str(root / "queue"),
                ],
                check=True,
                capture_output=True,
            )
            queue = json.loads((root / "queue" / "impact-queue.json").read_text(encoding="utf-8"))

        self.assertEqual(queue["run_status"], "REVIEW_REQUIRED")
        self.assertEqual(queue["items"][0]["change_kind"], "SUPERSEDED")


if __name__ == "__main__":
    unittest.main()
