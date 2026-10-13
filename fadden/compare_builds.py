"""List the sections that changed between two builds of the corpus.

``check_current`` and the radar say that a title's compilation changed; this
stage says where. It reads two corpus roots, each holding
``markdown/<register_id>/sections.jsonl``, and compares every title present in
both. Rows are matched on their container and section label, because a row_id
carries an ordinal that moves when a section is inserted. A label repeated in
one container is matched in order of appearance, and a row without a section
label by its heading.

For each title it reports the sections added, removed and changed, and whether
a change is to the text, the heading or both. Titles present in only one build
are listed. The comparison covers the derived reading view, not the authorised
text: read both compilations on the Register before relying on a change, and
treat none of it as a finding about an amendment's legal effect.

usage: python -m fadden compare_builds OLD_ROOT NEW_ROOT [--title REGISTER_ID] [--json]
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from typing import TYPE_CHECKING, Any, Sequence

if TYPE_CHECKING or __package__:
    from .corpus_paths import child, register_id
else:
    from corpus_paths import child, register_id

Key = tuple[str, str, int]


def titles(root: str) -> set[str]:
    """Return the register ids under *root* that hold a sections.jsonl."""
    markdown = child(root, "markdown")
    found = set()
    for name in os.listdir(markdown):
        try:
            rid = register_id(name)
        except ValueError:
            continue
        if os.path.isfile(child(markdown, rid, "sections.jsonl")):
            found.add(rid)
    return found


def load_rows(root: str, rid: str) -> list[dict[str, Any]]:
    path = child(root, "markdown", register_id(rid), "sections.jsonl")
    rows = []
    with open(path, encoding="utf-8") as source:
        for number, line in enumerate(source, 1):
            if not line.strip():
                continue
            row = json.loads(line)
            if not isinstance(row, dict) or not isinstance(row.get("text"), str):
                raise ValueError(f"{path}:{number} is not a sections.jsonl row")
            rows.append(row)
    return rows


def keyed(rows: list[dict[str, Any]]) -> dict[Key, dict[str, Any]]:
    seen: dict[tuple[str, str], int] = {}
    result: dict[Key, dict[str, Any]] = {}
    for row in rows:
        label = f"s {row['section']}" if row.get("section") else f"heading {row.get('heading') or ''}"
        base = (row.get("container") or "", label)
        seen[base] = seen.get(base, 0) + 1
        result[(*base, seen[base])] = row
    return result


def _place(row: dict[str, Any]) -> dict[str, Any]:
    return {"container": row.get("container"), "section": row.get("section"),
            "heading": row.get("heading")}


def compare_title(old_rows: list[dict[str, Any]], new_rows: list[dict[str, Any]]) -> dict[str, Any]:
    old, new = keyed(old_rows), keyed(new_rows)
    changed = []
    for key, row in new.items():
        if key not in old:
            continue
        text = old[key]["text"] != row["text"]
        heading = old[key].get("heading") != row.get("heading")
        if text or heading:
            change = "text and heading" if text and heading else "text" if text else "heading"
            changed.append({**_place(row), "change": change})
    return {
        "old_compilation": _compilation(old_rows),
        "new_compilation": _compilation(new_rows),
        "added": [_place(new[key]) for key in new if key not in old],
        "removed": [_place(old[key]) for key in old if key not in new],
        "changed": changed,
    }


def _compilation(rows: list[dict[str, Any]]) -> dict[str, Any] | None:
    if not rows:
        return None
    return {"number": rows[0].get("compilation_number"), "date": rows[0].get("compilation_date")}


def _describe(place: dict[str, Any]) -> str:
    parts = [place["container"]] if place.get("container") else []
    parts.append(f"s {place['section']}" if place.get("section") else str(place.get("heading")))
    return ", ".join(parts)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden compare_builds",
        description="List the sections added, removed and changed between two corpus builds.",
    )
    parser.add_argument("old_root", help="corpus root of the earlier build")
    parser.add_argument("new_root", help="corpus root of the later build")
    parser.add_argument("--title", help="compare only this register id")
    parser.add_argument("--json", action="store_true", help="print one JSON object per title")
    args = parser.parse_args(argv)
    try:
        old_titles, new_titles = titles(args.old_root), titles(args.new_root)
        if args.title:
            rid = register_id(args.title)
            if rid not in old_titles or rid not in new_titles:
                raise ValueError(f"{rid} is not in both builds")
            old_titles, new_titles = {rid}, {rid}
        reports = {rid: compare_title(load_rows(args.old_root, rid), load_rows(args.new_root, rid))
                   for rid in sorted(old_titles & new_titles)}
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if args.json:
        for rid, report in reports.items():
            print(json.dumps({"register_id": rid, **report}, ensure_ascii=False))
        for rid in sorted(old_titles - new_titles):
            print(json.dumps({"register_id": rid, "only_in": "old"}))
        for rid in sorted(new_titles - old_titles):
            print(json.dumps({"register_id": rid, "only_in": "new"}))
        return 0

    differing = 0
    for rid, report in reports.items():
        if not (report["added"] or report["removed"] or report["changed"]):
            continue
        differing += 1
        old, new = report["old_compilation"] or {}, report["new_compilation"] or {}
        print(f"{rid}: compilation {old.get('number')} ({old.get('date')}) to "
              f"{new.get('number')} ({new.get('date')})")
        for place in report["changed"]:
            print(f"  changed ({place['change']}): {_describe(place)}")
        for place in report["added"]:
            print(f"  added: {_describe(place)}")
        for place in report["removed"]:
            print(f"  removed: {_describe(place)}")
    print(f"{len(reports)} titles compared, {differing} with section changes; "
          f"{len(old_titles - new_titles)} only in the old build, "
          f"{len(new_titles - old_titles)} only in the new build.")
    print("This compares the derived reading view; read both compilations on the Register "
          "before relying on a change.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
