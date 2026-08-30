from __future__ import annotations

from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[2]

EXPECTED_POLICY = """\
# Agent instructions

This repository combines two deliberately separated systems: a corpus builder and a
synthetic change-review radar. Treat corpus outputs, synthetic monitor inputs, live
Register captures and publication candidates as different contracts.

- Synthetic monitor contracts project fabricated or separately reviewed observation
  facts and never query the Register. Live Register capture queries the official API,
  retains its bounded response bytes and emits a deliberately incompatible live contract.
- Never commit raw live captures, retained Register responses or generated corpus,
  evidence, distribution, demo or build output.
- Treat official capture and export destinations as immutable: use an absent destination,
  never overwrite, repair or delete an earlier output, and preserve the exact source bytes,
  content digests and cross-file provenance that support each result.
- The supported official live-evidence export boundary is Windows only. On every other
  platform it must fail closed before creating an output parent or staging directory.
- Live capture and local export are not publication authority. A human must explicitly
  authorise every tag, release, public development and deployment; `VERIFIED` evidence and
  passing checks do not authorise publication.
- The corpus is a finding aid derived from the EPUB reading view, not authorised legal
  text, tax advice or a conclusion about the practical effect of a change.
"""

EXPECTED_MAP = """\
- [README.md](README.md) explains the two halves, corpus limits and high-level operation.
- [BUILD.md](BUILD.md) owns pipeline, monitor-export, live-capture and live-export details.
- [RADAR.md](RADAR.md) owns the synthetic review-queue contract and human-review boundary.
- [RELEASING.md](RELEASING.md) owns builder release preflight and immutable-release rules.
- The approved [Stage 3A capture design](docs/superpowers/specs/2026-08-29-live-register-capture-design.md)
  and [Stage 3B evidence design](docs/superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md)
  own the trust, provenance and publication boundaries. Keep operational detail there
  instead of duplicating it here.
"""

EXPECTED_PLATFORM_NOTE = """\
`verify.yml` runs compilation and the corpus unittest suite on Ubuntu. `ci.yml` runs the
full locked pytest suite on Ubuntu for Python 3.10-3.13, but its Windows 3.12 matrix runs
only `tests/radar`. The workflow documents pre-existing full-corpus Windows failures around
8.3 short paths, junctions and reparse points; do not describe a local Windows full-suite
failure in those paths as a radar regression without proving it.
"""

WHEEL_SMOKE_COMMANDS = [
    'uv run --locked --extra dev --python 3.12 python -m build',
    "$wheelDir = (Resolve-Path dist).Path",
    (
        "$smokeDir = Join-Path ([System.IO.Path]::GetTempPath()) "
        "(\"tax-radar-wheel-smoke-\" + [guid]::NewGuid().ToString(\"N\"))"
    ),
    'python -m venv "$smokeDir\\venv"',
    (
        '& "$smokeDir\\venv\\Scripts\\python.exe" -m pip install '
        '--no-index --find-links $wheelDir tax-radar-au'
    ),
    "Push-Location $smokeDir",
    (
        '$baseline = & "$smokeDir\\venv\\Scripts\\python.exe" -c '
        '"from tax_radar_au.util import sample_path; '
        "print(sample_path('baseline', 'sample-sources.json'))\""
    ),
    (
        '$observation = & "$smokeDir\\venv\\Scripts\\python.exe" -c '
        '"from tax_radar_au.util import sample_path; '
        "print(sample_path('observations', 'sample-register-observation.json'))\""
    ),
    (
        '$mapping = & "$smokeDir\\venv\\Scripts\\python.exe" -c '
        '"from tax_radar_au.util import sample_path; '
        "print(sample_path('mappings', 'sample-source-skill-map.json'))\""
    ),
    (
        '$decision = & "$smokeDir\\venv\\Scripts\\python.exe" -c '
        '"from tax_radar_au.util import sample_path; '
        "print(sample_path('decisions', 'sample-technical-review.json'))\""
    ),
    (
        '& "$smokeDir\\venv\\Scripts\\tax-radar-au.exe" compare '
        '--baseline $baseline --observation $observation --map $mapping --out demo'
    ),
    (
        '& "$smokeDir\\venv\\Scripts\\tax-radar-au.exe" validate-review '
        '--queue demo/impact-queue.json --decision $decision --out demo/validation.json'
    ),
    "Pop-Location",
]


def _normalise(document: str) -> str:
    return re.sub(r"\s+", " ", document).strip()


def _section(document: str, heading: str) -> str:
    match = re.search(
        rf"^## {re.escape(heading)}\s*$\n(.*?)(?=^## |\Z)",
        document,
        flags=re.MULTILINE | re.DOTALL,
    )
    assert match is not None, f"missing ## {heading} section"
    return match.group(1)


def _fenced_commands(section: str) -> list[str]:
    blocks = re.findall(r"```(?:bash|powershell)\n(.*?)```", section, flags=re.DOTALL)
    return [line for block in blocks for line in block.splitlines() if line]


def _without_fenced_commands(section: str) -> str:
    return re.sub(r"```(?:bash|powershell)\n.*?```", "", section, flags=re.DOTALL)


def _workflow_commands() -> list[str]:
    commands: list[str] = []
    for name in ("verify.yml", "ci.yml"):
        workflow = (ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")
        for command in re.findall(
            r"^\s+(?:-\s+)?run:\s*(?!\|\s*$)(\S.*)$",
            workflow,
            flags=re.MULTILINE,
        ):
            if "${{ matrix.tests }}" in command:
                commands.extend(
                    command.replace("${{ matrix.tests }}", tests)
                    for tests in ("tests", "tests/radar")
                )
            else:
                commands.append(command)
    return list(dict.fromkeys(commands))


def test_agents_preserves_exact_corpus_and_publication_boundaries() -> None:
    guidance = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
    policy, separator, _remainder = guidance.partition("## Repository map")

    assert separator
    assert _normalise(policy) == _normalise(EXPECTED_POLICY)


def test_agents_links_to_the_existing_authoritative_documents() -> None:
    guidance = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
    repository_map = _section(guidance, "Repository map")

    assert _normalise(repository_map) == _normalise(EXPECTED_MAP)
    for path in (
        "README.md",
        "BUILD.md",
        "RADAR.md",
        "RELEASING.md",
        "docs/superpowers/specs/2026-08-29-live-register-capture-design.md",
        "docs/superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md",
    ):
        assert (ROOT / path).is_file()


def test_agents_tracks_ci_commands_and_platform_scope() -> None:
    guidance = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
    ci = _section(guidance, "CI gates")

    assert _fenced_commands(ci) == _workflow_commands()
    assert _normalise(_without_fenced_commands(ci)) == _normalise(
        EXPECTED_PLATFORM_NOTE
    )


def test_agents_requires_locked_build_and_installed_wheel_smoke() -> None:
    guidance = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
    build = _section(guidance, "Package build and installed-wheel smoke")

    assert _fenced_commands(build) == WHEEL_SMOKE_COMMANDS
    assert _normalise(_without_fenced_commands(build)) == _normalise(
        """\
        Run the locked build, then install and exercise that wheel from a fresh temporary
        directory outside the checkout. This is a package preflight, not a publication step:
        """
    )


def test_claude_imports_the_shared_guidance_exactly() -> None:
    assert (ROOT / "CLAUDE.md").read_text(encoding="utf-8") == "@AGENTS.md\n"
