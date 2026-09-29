"""Classify the build's titles against a date, and check a quotation against a row.

A compilation date is the start date of that compiled text version. It is not
necessarily the commencement or application date of every provision or
amendment the compilation contains. A compilation also does not show
application, saving or transitional provisions elsewhere, modification by
another law, or an amendment with retrospective effect registered after the
build was retrieved: the Legislation Act 2003 does not require a new
compilation for one. So this stage answers a narrower question than "what was
the law on DATE". For each title it says whether the compilation this build
captured corresponds to DATE according to the build's own metadata:

- ``captured``: the compilation started on or before DATE, and DATE is before
  ``current_version_start`` for a title whose in-force version had no
  compilation, or on or before the retrieval date for a current title.
- ``before_compilation``: DATE is before the compilation started. The text in
  force on DATE may differ, or the provision may not have existed. Read the
  Register's earlier compilations.
- ``superseded``: DATE is on or after ``current_version_start``. A later version
  applies, and the Register had published no compilation for it.
- ``after_retrieval``: DATE is after the retrieval date. A later compilation may
  exist. ``check_current`` finds versions the Register now lists; it cannot
  show that nothing retrospective was registered later.

``captured`` is not a finding that the text was the law on DATE.

``quote_in_text`` is for a consumer that cites a row. It accepts a quotation
only when it appears word for word in the row text.

usage: python -m fadden as_at YYYY-MM-DD [--manifest PATH] [--json]
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import sys
import unicodedata
from typing import Sequence

CAPTURED = "captured"
BEFORE_COMPILATION = "before_compilation"
SUPERSEDED = "superseded"
AFTER_RETRIEVAL = "after_retrieval"
STATUSES = (CAPTURED, BEFORE_COMPILATION, SUPERSEDED, AFTER_RETRIEVAL)

MANIFEST = os.path.join(os.path.dirname(os.path.abspath(__file__)), "manifest_md.json")

ACTIONS = {
    BEFORE_COMPILATION: "read the Register's earlier compilations",
    SUPERSEDED: "a later version applies; read the Register",
    AFTER_RETRIEVAL: "run check_current and read the Register",
}


def _date(title: dict, field: str) -> datetime.date:
    value = title.get(field)
    try:
        parsed = datetime.date.fromisoformat(value) if isinstance(value, str) else None
    except ValueError:
        parsed = None
    if parsed is None or parsed.isoformat() != value:
        raise ValueError(f"{title.get('id', '?')}: {field} is not a YYYY-MM-DD date: {value!r}")
    return parsed


def window(title: dict) -> tuple[datetime.date, datetime.date | None, datetime.date]:
    """Return (compilation start, superseded from or None, retrieved) for one title.

    Refuses a manifest entry whose dates cannot all be true at once.
    """
    start = _date(title, "versionStart")
    retrieved = _date(title, "retrieved")
    if start > retrieved:
        raise ValueError(f"{title.get('id', '?')}: versionStart is after retrieved")
    current = title.get("version_is_current", True)
    if not isinstance(current, bool):
        # A string "false" would otherwise read as current and hide the successor.
        raise ValueError(
            f"{title.get('id', '?')}: version_is_current must be true or false, not {current!r}"
        )
    if current is False:
        superseded = _date(title, "current_version_start")
        if not start < superseded <= retrieved:
            raise ValueError(
                f"{title.get('id', '?')}: current_version_start must fall after "
                "versionStart and on or before retrieved"
            )
        return start, superseded, retrieved
    return start, None, retrieved


def classify(title: dict, on: datetime.date) -> str:
    """Say how the compilation captured for ``title`` relates to the date ``on``."""
    start, superseded, retrieved = window(title)
    if on < start:
        return BEFORE_COMPILATION
    if superseded is not None:
        return SUPERSEDED if on >= superseded else CAPTURED
    return AFTER_RETRIEVAL if on > retrieved else CAPTURED


def _literal(text: str) -> str:
    # NFKC turns compatibility forms, such as a non-breaking space, into their
    # plain equivalents, and turns the non-breaking hyphen the Register uses in
    # section numbers (40‑1) into U+2010. A quotation typed with a keyboard
    # hyphen has to match that, so U+2010 is read as "-" too. Nothing else is
    # folded: no case, whitespace or punctuation changes.
    return unicodedata.normalize("NFKC", text).replace("‐", "-")


def quote_in_text(quote: str, text: str) -> bool:
    """Whether ``quote`` appears word for word in ``text``.

    A match shows that the quotation is faithful to the row. It does not show
    that the row states the law for the question or the date. A blank quotation
    is refused rather than matching every text.
    """
    if not quote.strip():
        raise ValueError("a blank quotation matches every text")
    return _literal(quote) in _literal(text)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden as_at",
        description="Classify each title's captured compilation against one date.",
    )
    parser.add_argument("date", help="the date to test, as YYYY-MM-DD")
    parser.add_argument("--manifest", default=MANIFEST, help="manifest_md.json to read")
    parser.add_argument("--json", action="store_true", help="print one JSON object per title")
    args = parser.parse_args(argv)
    try:
        on = datetime.date.fromisoformat(args.date)
    except ValueError:
        parser.error(f"date must be YYYY-MM-DD, not {args.date!r}")
    try:
        with open(args.manifest, encoding="utf-8") as f:
            titles = json.load(f)
        rows = [(title, classify(title, on)) for title in titles]
    except (OSError, ValueError, TypeError, AttributeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if args.json:
        for title, status in rows:
            start, superseded, retrieved = window(title)
            print(json.dumps({
                "register_id": title.get("id"),
                "name": title.get("name"),
                "status": status,
                "compilation_date": start.isoformat(),
                "superseded_from": superseded.isoformat() if superseded else None,
                "retrieved": retrieved.isoformat(),
            }, ensure_ascii=False))
        return 0

    counts = {status: sum(1 for _, s in rows if s == status) for status in STATUSES}
    print(f"{len(rows)} titles against {on.isoformat()}:")
    for status in STATUSES:
        action = f" ({ACTIONS[status]})" if status in ACTIONS and counts[status] else ""
        print(f"  {status}: {counts[status]}{action}")
    print("captured means the build holds the compilation for that date; it is not a "
          "finding that the text was the law then.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
