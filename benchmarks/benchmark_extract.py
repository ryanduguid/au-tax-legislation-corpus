"""Measurement-only benchmark for representative corpus extraction."""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

import pyperf


REPOSITORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPOSITORY))
sys.path.insert(0, str(REPOSITORY / "fadden"))

from fadden.extract import to_markdown  # noqa: E402


SECTION_COUNT = 2_000
EXPECTED_TABLE_COUNT = 100
TABLE_HEADER = "| Band | Rate | Amount |"
EXPECTED_RENDERED_SHA256 = "00b054b04d41da8a3a279a8549f715f5ce20882e6e9328a6066488baa6a825aa"
META = {
    "id": "C2099A00001",
    "name": "Fabricated Tax Benchmark Act 2099",
    "retrieved": "2026-08-28",
    "versionStart": "2099-01-01",
    "collection": "Act",
}


def representative_blocks() -> list[dict]:
    """Two fabricated volumes with sections, prose and periodic tables."""
    blocks: list[dict] = []
    per_volume = SECTION_COUNT // 2
    for volume in range(2):
        blocks.append({"k": "file"})
        for offset in range(per_volume):
            number = volume * per_volume + offset + 1
            blocks.append(
                {
                    "k": "p",
                    "cls": "ActHead5",
                    "sectno": True,
                    "text": f"{number} Fabricated benchmark section",
                }
            )
            blocks.append(
                {
                    "k": "p",
                    "cls": "MsoNormal",
                    "sectno": False,
                    "text": (
                        "A fabricated provision records an invented amount of $1,234.56 "
                        "and a 12.5% rate for repeatable extraction measurement."
                    ),
                }
            )
            if number % 20 == 0:
                blocks.append(
                    {
                        "k": "table",
                        "rows": [
                            ["Band", "Rate", "Amount"],
                            ["A", "12.5%", "$1,234.56"],
                            ["B", "7.5%", "$987.65"],
                        ],
                    }
                )
    return blocks


BLOCKS = representative_blocks()


def render_fixture() -> tuple[str, list[dict], str, str | None]:
    return to_markdown(BLOCKS, META)


def main() -> None:
    rendered, sections, _endnotes, _long_title = render_fixture()
    section_rows = [section for section in sections if section["kind"] == "section"]
    rendered_sha256 = hashlib.sha256(rendered.encode("utf-8")).hexdigest()
    if (
        len(section_rows) != SECTION_COUNT
        or "Fabricated benchmark section" not in rendered
        or rendered.count(TABLE_HEADER) != EXPECTED_TABLE_COUNT
        or rendered_sha256 != EXPECTED_RENDERED_SHA256
    ):
        raise RuntimeError("benchmark fixture did not exercise the expected extraction path")

    runner = pyperf.Runner()
    runner.metadata["fixture"] = "fabricated-two-volume-2000-section"
    runner.metadata["section_count"] = SECTION_COUNT
    runner.bench_func("corpus.to_markdown.2000_sections", render_fixture)


if __name__ == "__main__":
    main()
