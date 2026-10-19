"""Dispatch documented pipeline stages by name."""

from __future__ import annotations

import argparse
import datetime
import importlib
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Sequence

from fadden import STAGES

# Stages ported to the Rust `fadden` binary (Cargo.toml). They keep their
# command lines; this dispatcher runs the binary for them.
RUST_STAGES = frozenset({"as_at", "compare_builds", "search"})


def rust_binary() -> str | None:
    """FADDEN_BIN, else a Cargo release or debug build in this checkout, else PATH."""
    named = os.environ.get("FADDEN_BIN")
    if named:
        return named
    suffix = ".exe" if os.name == "nt" else ""
    root = Path(__file__).resolve().parent.parent
    for build in ("release", "debug"):
        candidate = root / "target" / build / f"fadden{suffix}"
        if candidate.is_file():
            return str(candidate)
    return shutil.which("fadden")


def run_rust_stage(stage: str, forwarded: list[str]) -> int:
    binary = rust_binary()
    if binary is None:
        print(f"error: the {stage} stage is the Rust fadden binary; build it with "
              "cargo build --locked --release", file=sys.stderr)
        return 1
    # The stage's defaults, such as as_at's manifest, sit beside this module.
    env = dict(os.environ, FADDEN_MODULE_DIR=os.path.dirname(os.path.abspath(__file__)))
    # Maintainer CLI; the binary is this checkout's build or the operator's choice.
    return subprocess.run([binary, stage, *forwarded], env=env, check=False).returncode  # NOSONAR


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden",
        description="Run one corpus-builder stage. Names match the historical scripts.",
    )
    parser.add_argument("stage", choices=STAGES)
    parser.add_argument(
        "stage_args",
        nargs=argparse.REMAINDER,
        help="Arguments forwarded to the stage (use -- to stop option parsing).",
    )
    args = parser.parse_args(argv)
    forwarded = list(args.stage_args)
    if forwarded and forwarded[0] == "--":
        forwarded = forwarded[1:]
    if args.stage in RUST_STAGES:
        return run_rust_stage(args.stage, forwarded)

    module = importlib.import_module(f"fadden.{args.stage}")
    func = getattr(module, "main")
    saved = sys.argv
    sys.argv = [args.stage, *forwarded]
    try:
        if args.stage == "finalize":
            result = func(datetime.date.today().isoformat())
        elif args.stage in {
            "export_monitor_contract",
            "export_publication_bundles",
            "capture_register",
            "export_live_evidence_bundles",
            "rulings",
        }:
            result = func(forwarded)
        elif args.stage == "extract" and forwarded:
            # extract.main takes the retrieval date as a parameter and only
            # reads sys.argv under its own __main__ guard, which never runs
            # here; without this branch a forwarded date was silently dropped.
            result = func(forwarded[0])
        else:
            result = func()
    finally:
        sys.argv = saved
    return int(result or 0)


if __name__ == "__main__":
    raise SystemExit(main())
