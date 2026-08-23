"""Guards for repository facts that are otherwise only true by habit.

Each assertion here fails when the exact line it names is removed, so a
packaging, documentation or CI regression cannot land with a green run.
"""

from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

BLOCKED_CHANGE_KINDS = (
    "INCOMPLETE_SCOPE",
    "MISSING_OBSERVATION",
    "LOOKUP_FAILED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "BASELINE_NOT_CURRENT",
)


def test_build_output_directories_are_ignored() -> None:
    # The README tells the reader to run `python -m build`, which fills dist/.
    # Unignored, a following `git add -A` commits a wheel and a tarball into a
    # public repository, where they cannot be removed without a history rewrite.
    entries = {line.strip() for line in (ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()}

    assert {"build/", "dist/"} <= entries


def test_readme_names_every_blocked_change_kind() -> None:
    # compare() blocks for five distinct reasons. A reader who meets
    # BASELINE_NOT_CURRENT or MISSING_OBSERVATION on a stale baseline needs the
    # README to say what the label and the non-zero exit mean.
    readme = (ROOT / "README.md").read_text(encoding="utf-8")

    for change_kind in BLOCKED_CHANGE_KINDS:
        assert change_kind in readme, f"README does not document {change_kind}"


def test_readme_describes_both_rejection_shapes_not_one() -> None:
    # argparse rejects a malformed command line itself, exit 2 with a usage
    # block and no "blocked:" line, so the README must not promise that prefix
    # for every rejected input. tests/test_cli.py pins the behaviour; this pins
    # the sentence that describes it.
    readme = (ROOT / "README.md").read_text(encoding="utf-8")

    assert "Every rejected input also exits 2" not in readme
    assert "rejected by argparse" in readme


def test_readme_states_that_an_unwritable_stdout_keeps_the_run_exit_status() -> None:
    # The exit-code table above says 0 or 2 and nothing else. A stdout that
    # cannot be written is the one way that promise used to break - the
    # interpreter's exit-time flush replaced the status with 120 - so the
    # README has to say what happens now instead. tests/test_cli.py pins the
    # behaviour; this pins the sentence.
    readme = (ROOT / "README.md").read_text(encoding="utf-8")

    assert "the exit status is still the one the run decided" in readme


def test_readme_states_the_pinned_timestamp_grammar() -> None:
    # The grammar is narrower than datetime.fromisoformat's on every supported
    # interpreter, so a user with a stored artefact needs it written down.
    readme = (ROOT / "README.md").read_text(encoding="utf-8")

    assert "YYYY-MM-DDThh:mm:ss[.ffffff][Z|+hh:mm|-hh:mm]" in readme
    assert "or a single space allowed in place of `T`" in readme


def test_ci_keeps_the_pytest_summary_line_visible() -> None:
    # pyproject already sets addopts = "-q". A second -q on the CI command line
    # makes it -qq, which drops the pass/fail count: a test file that stops
    # being collected then leaves every matrix leg green and identical.
    workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")

    assert 'addopts = "-q"' in (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert "pytest -q" not in workflow
    assert "pytest -qq" not in workflow
