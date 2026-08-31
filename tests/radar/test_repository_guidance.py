from __future__ import annotations

from pathlib import Path
from pathlib import PurePosixPath
import ast
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

The fenced list records the unique single-line commands. The multiline package-smoke gate
is explained and matched semantically below without duplicating its shell body.
"""

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


def _workflow_run_gates(workflow: str) -> list[tuple[bool, str]]:
    lines = workflow.splitlines()
    gates: list[tuple[bool, str]] = []
    index = 0
    while index < len(lines):
        match = re.match(r"^\s*(?:-\s+)?run:\s*(.*?)\s*$", lines[index])
        if match is None:
            index += 1
            continue

        value = match.group(1)
        block = re.fullmatch(r"(?P<style>[|>])[+-]?", value)
        if block is None:
            gates.append((False, value))
            index += 1
            continue

        key_indent = lines[index].index("run:")
        cursor = index + 1
        while cursor < len(lines) and not lines[cursor].strip():
            cursor += 1
        if cursor == len(lines):
            gates.append((True, ""))
            break
        content_indent = len(lines[cursor]) - len(lines[cursor].lstrip())
        if content_indent <= key_indent:
            gates.append((True, ""))
            index = cursor
            continue

        body: list[str] = []
        while cursor < len(lines):
            line = lines[cursor]
            indent = len(line) - len(line.lstrip())
            if line.strip() and indent < content_indent:
                break
            if line.strip():
                body.append(line[content_indent:].rstrip())
            cursor += 1
        separator = "\n" if block.group("style") == "|" else " "
        gates.append((True, separator.join(body)))
        index = cursor
    return gates


def _workflow_matrix_values(workflow: str, key: str) -> list[str]:
    lines = workflow.splitlines()
    values: list[str] = []
    matrix_indent: int | None = None
    for line in lines:
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if stripped == "matrix:":
            matrix_indent = indent
            continue
        if matrix_indent is None:
            continue
        if stripped and indent <= matrix_indent:
            matrix_indent = None
            continue
        match = re.match(rf"^\s+{re.escape(key)}:\s*(\S.*)$", line)
        if match is None:
            continue
        raw = match.group(1).split(" #", 1)[0].strip()
        parsed = ast.literal_eval(raw) if raw[:1] in "[\"'" else raw
        values.extend(parsed if isinstance(parsed, list) else [parsed])
    return list(dict.fromkeys(values))


def _workflow_commands_from(workflows: tuple[str, ...]) -> list[str]:
    commands: list[str] = []
    for workflow in workflows:
        test_targets = _workflow_matrix_values(workflow, "tests")
        for multiline, command in _workflow_run_gates(workflow):
            if multiline:
                continue
            if "${{ matrix.tests }}" in command:
                commands.extend(
                    command.replace("${{ matrix.tests }}", tests)
                    for tests in test_targets
                )
            else:
                commands.append(command)
    return list(dict.fromkeys(commands))


def _workflow_commands() -> list[str]:
    return _workflow_commands_from(
        tuple(
            (ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")
            for name in ("verify.yml", "ci.yml")
        )
    )


def _workflow_multiline_gates() -> list[str]:
    return [
        command
        for name in ("verify.yml", "ci.yml")
        for multiline, command in _workflow_run_gates(
            (ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")
        )
        if multiline
    ]


def _workflow_run_commands(name: str) -> list[str]:
    workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(
        encoding="utf-8"
    )
    lines = workflow.splitlines()
    markers = [index for index, line in enumerate(lines) if line == f"      - name: {name}"]
    assert len(markers) == 1, f"expected one {name!r} workflow step"
    start = markers[0]
    end = next(
        (index for index in range(start + 1, len(lines)) if lines[index].startswith("      - ")),
        len(lines),
    )
    step = lines[start:end]
    run_markers = [index for index, line in enumerate(step) if line == "        run: |"]
    assert len(run_markers) == 1, "expected one multiline run block"

    active: list[str] = []
    for line in step[run_markers[0] + 1 :]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= 8:
            break
        active.append(stripped)

    commands: list[str] = []
    continued = ""
    for line in active:
        if line.endswith("\\"):
            continued += line[:-1].rstrip() + " "
        else:
            commands.append((continued + line).strip())
            continued = ""
    assert not continued, "workflow smoke command has an unterminated continuation"
    return commands


def _fullmatch(pattern: str, value: str, label: str) -> re.Match[str]:
    match = re.fullmatch(pattern, value)
    assert match is not None, f"{label} does not match the package smoke contract: {value!r}"
    return match


def _workflow_smoke_contract() -> tuple[tuple[str, ...], ...]:
    commands = _workflow_run_commands(
        "install the wheel and run the demo from outside the checkout"
    )
    assert len(commands) == 6, "package smoke step must contain six active commands"
    venv = _fullmatch(r"python -m venv (?P<path>/\S+)", commands[0], "workflow venv")
    assert venv["path"] == "/tmp/venv"
    venv_path = PurePosixPath(venv["path"])
    assert commands[2] == "cd /tmp"
    resolve = _fullmatch(
        r'resolve\(\) \{ (?P<python>\S+) -c "from tax_radar_au\.util import sample_path; '
        r"print\(sample_path\('\$1', '\$2'\)\)\"; \}",
        commands[3],
        "sample resolver",
    )
    assert resolve["python"] == str(venv_path / "bin" / "python")
    substitutions = {
        str(venv_path / "bin" / "pip"): "<python> -m pip",
        str(venv_path / "bin" / "tax-radar-au"): "<cli>",
    }
    normalised = [f"python -m venv <venv>", commands[1], "outside-checkout", *commands[4:]]
    for old, new in substitutions.items():
        normalised = [command.replace(old, new) for command in normalised]
    return tuple(
        tuple(
            re.sub(r'"\$\(resolve ([^ )]+) ([^ )]+)\)"', r"<sample:\1/\2>", command).split()
        )
        for command in normalised
    )


def _guidance_smoke_contract(commands: list[str]) -> tuple[tuple[str, ...], ...]:
    package_build = next(
        command for command in _workflow_commands() if command.endswith("python -m build")
    )
    assert commands[0] == package_build
    assert commands[1] == "$wheelDir = (Resolve-Path dist).Path"
    _fullmatch(
        r'\$smokeDir = Join-Path \(\[System\.IO\.Path\]::GetTempPath\(\)\) '
        r'\("tax-radar-wheel-smoke-" \+ \[guid\]::NewGuid\(\)\.ToString\("N"\)\)',
        commands[2],
        "outside-checkout temporary directory",
    )
    assert commands[3] == 'python -m venv "$smokeDir\\venv"'
    assert commands[5] == "Push-Location $smokeDir"

    samples: dict[str, str] = {}
    for command in commands[6:10]:
        match = _fullmatch(
            r'\$(?P<variable>[a-z]+) = & "\$smokeDir\\venv\\Scripts\\python\.exe" -c '
            r'"from tax_radar_au\.util import sample_path; '
            r"print\(sample_path\('(?P<kind>[^']+)', '(?P<file>[^']+)'\)\)\"",
            command,
            "installed sample resolver",
        )
        samples[f"${match['variable']}"] = f"<sample:{match['kind']}/{match['file']}>"
    assert commands[12] == "Pop-Location"
    normalised = [
        "python -m venv <venv>",
        commands[4]
        .replace('& "$smokeDir\\venv\\Scripts\\python.exe"', "<python>")
        .replace("$wheelDir", "dist"),
        "outside-checkout",
        *commands[10:12],
    ]
    for old, new in {
        '& "$smokeDir\\venv\\Scripts\\tax-radar-au.exe"': "<cli>",
        **samples,
    }.items():
        normalised = [command.replace(old, new) for command in normalised]
    return tuple(tuple(command.split()) for command in normalised)


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


def test_workflow_parser_detects_new_non_python_and_multiline_run_gates() -> None:
    workflow = """\
steps:
  - run: pwsh -File scripts/check-policy.ps1
  - run: |
      npm ci
      npm test
  - run: >-
      cargo fmt --check &&
      cargo test
"""

    assert _workflow_run_gates(workflow) == [
        (False, "pwsh -File scripts/check-policy.ps1"),
        (True, "npm ci\nnpm test"),
        (True, "cargo fmt --check && cargo test"),
    ]


def test_workflow_commands_derive_every_matrix_test_target() -> None:
    verify = (ROOT / ".github" / "workflows" / "verify.yml").read_text(
        encoding="utf-8"
    )
    ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    ci = ci.replace('tests: ["tests"]', 'tests: ["tests", "tests/integration"]', 1)

    assert "uv run --locked --extra dev pytest tests/integration" in (
        _workflow_commands_from((verify, ci))
    )


def test_agents_requires_locked_build_and_installed_wheel_smoke() -> None:
    guidance = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
    build = _section(guidance, "Package build and installed-wheel smoke")

    assert len(_workflow_multiline_gates()) == 1
    assert _guidance_smoke_contract(_fenced_commands(build)) == _workflow_smoke_contract()
    assert _normalise(_without_fenced_commands(build)) == _normalise(
        """\
        Run the locked build, then install and exercise that wheel from a fresh temporary
        directory outside the checkout. This is a package preflight, not a publication step:
        """
    )


def test_claude_imports_the_shared_guidance_exactly() -> None:
    assert (ROOT / "CLAUDE.md").read_text(encoding="utf-8") == "@AGENTS.md\n"
