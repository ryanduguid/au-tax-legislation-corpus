from __future__ import annotations

import ast
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

EXPECTED_POLICY = """\
# Agent instructions

This repository combines 2 deliberately separated systems: a corpus builder and a
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
- [README.md](README.md) explains the 2 halves, corpus limits and high-level operation.
- [BUILD.md](BUILD.md) owns pipeline, monitor-export, live-capture and live-export details.
- [RADAR.md](RADAR.md) owns the synthetic review-queue contract and human-review boundary.
- [RELEASING.md](RELEASING.md) owns builder release preflight and immutable-release rules.
- The approved [Stage 3A capture design](docs/superpowers/specs/2026-08-29-live-register-capture-design.md)
  and [Stage 3B evidence design](docs/superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md)
  own the trust, provenance and publication boundaries. Keep operational detail there
  instead of duplicating it here.
"""

EXPECTED_PLATFORM_NOTE = """\
`ci.yml` runs the full locked pytest suite on Ubuntu for Python 3.14 and 3.15, then the
package build and the linters. Its Windows 3.14 matrix runs
`tests/corpus/test_live_evidence_bundle_export.py` and
`tests/corpus/test_live_evidence_publication_paths.py` under branch coverage so the
supported live-evidence export boundary is exercised. Codecov merges that report with the
Ubuntu 3.14 report. Only the Ubuntu report is held to the `fail_under` floor, because the
Windows leg runs a subset. Other corpus modules remain excluded because
of pre-existing Windows failures around 8.3 short paths, junctions and reparse points;
do not describe a local Windows full-suite failure in those paths as a radar regression
without proving it.

The radar is the Rust `tax-radar-au` binary. The `rust` job runs its formatting, Clippy,
tests and release build on Ubuntu and Windows, and the Ubuntu test legs build it for the
cross-system contract test in `tests/corpus/test_monitor_contract.py`.

`tests/fadden_golden.rs` replays `tests/golden/`: fabricated cases each run through the
Python stages the `fadden` binary replaced, on Windows and Linux, with what Python printed
and wrote. A failure there is a departure from the Python behaviour, not a stale fixture;
regenerate fixtures only with the port's differential harness against the base revision.

The fenced list records the unique single-line commands. The multiline binary-smoke gate
is reproduced exactly in the next section.
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
    # The trailing aggregate job checks job results, not the repository, so its
    # scripts are not contributor commands.
    workflow = re.split(r"(?m)^  [\w-]+-gates:$", workflow, maxsplit=1)[0]
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
            # A space before # starts a YAML comment in a plain scalar, such as
            # a NOSONAR justification; it is not part of the command.
            gates.append((False, re.sub(r"\s+#.*$", "", value)))
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
            for name in ("ci.yml",)
        )
    )


def _workflow_multiline_gates() -> list[str]:
    return [
        command
        for name in ("ci.yml",)
        for multiline, command in _workflow_run_gates(
            (ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")
        )
        if multiline
    ]


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
  - run: uv run --locked pytest tests  # NOSONAR: justification
  - run: |
      npm ci
      npm test
  - run: >-
      cargo fmt --check &&
      cargo test
"""

    assert _workflow_run_gates(workflow) == [
        (False, "pwsh -File scripts/check-policy.ps1"),
        (False, "uv run --locked pytest tests"),
        (True, "npm ci\nnpm test"),
        (True, "cargo fmt --check && cargo test"),
    ]


def test_workflow_commands_derive_every_matrix_test_target() -> None:
    ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    ci = ci.replace('tests: ["tests"]', 'tests: ["tests", "tests/integration"]', 1)

    assert "uv run --locked --extra dev pytest tests/integration" in (
        _workflow_commands_from((ci,))
    )


def test_agents_reproduces_the_release_binary_smoke() -> None:
    guidance = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
    smoke = _section(guidance, "Release binary smoke")

    [gate] = _workflow_multiline_gates()
    assert _fenced_commands(smoke) == gate.splitlines()
    assert _normalise(_without_fenced_commands(smoke)) == _normalise(
        """\
        Build the release binary, then run it on the shipped samples from a fresh temporary
        directory outside the checkout. This is a binary preflight, not a publication step:
        """
    )


def test_claude_imports_the_shared_guidance_exactly() -> None:
    assert (ROOT / "CLAUDE.md").read_text(encoding="utf-8") == "@AGENTS.md\n"
