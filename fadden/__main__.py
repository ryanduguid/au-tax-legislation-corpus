"""Dispatch documented pipeline stages by name."""

from __future__ import annotations

import argparse
import datetime
import importlib
import sys
from typing import Sequence

from fadden import STAGES


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
        elif args.stage == "export_monitor_contract":
            result = func(forwarded)
        else:
            result = func()
    finally:
        sys.argv = saved
    return int(result or 0)


if __name__ == "__main__":
    raise SystemExit(main())
