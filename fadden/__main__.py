"""Dispatch documented pipeline stages by name."""

from __future__ import annotations

import argparse
import datetime
import importlib
import os
import sys
from typing import Sequence

import fadden
from fadden import STAGES

# Stage modules import their shared helpers by bare name (corpus_paths,
# http_fetch, download) so the same files run unchanged in the flat deployed
# build/ layout. Under `python -m fadden` those names are not importable until
# the package directory itself is on sys.path.
_PACKAGE_DIR = os.path.dirname(os.path.abspath(fadden.__file__))
if _PACKAGE_DIR not in sys.path:
    sys.path.insert(0, _PACKAGE_DIR)


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
