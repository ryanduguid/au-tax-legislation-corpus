"""Stage 2: dedup titles properly, then resolve each Act's current version date."""
import json
import os
import time
import urllib.parse
from typing import TYPE_CHECKING

if TYPE_CHECKING or __package__:
    from .corpus_paths import (
        compilation_id,
        register_id,
        require_builder_layout,
        version_date,
        version_rows,
        write_json_atomic,
    )
    from .http_fetch import fetch_json
else:
    from corpus_paths import (
        compilation_id,
        register_id,
        require_builder_layout,
        version_date,
        version_rows,
        write_json_atomic,
    )
    from http_fetch import fetch_json


API = "https://api.prod.legislation.gov.au/v1"
SCRATCH = os.path.dirname(os.path.abspath(__file__))
VERSION_BATCH_SIZE = 8
RESOLUTION_DELAY = 1.5


def _versions_url(title_ids):
    condition = " or ".join("titleId eq '%s'" % rid for rid in title_ids)
    if len(title_ids) > 1:
        condition = "(%s)" % condition
    condition += " and isCurrent eq true"
    return "%s/versions?$top=%d&$filter=%s&$select=titleId,start,compilationNumber,registerId" % (
        API, len(title_ids) + 1 if len(title_ids) > 1 else 1, urllib.parse.quote(condition))


def _batch_rows(payload, title_ids):
    """Validate every row; return None when a valid group needs singleton recovery."""
    error = "invalid version batch; refusing to write acts_resolved.json"
    if (not isinstance(payload, dict) or not isinstance(payload.get("value"), list)
            or "@odata.nextLink" in payload or "@nextLink" in payload
            or len(payload["value"]) > len(title_ids) + 1):
        raise RuntimeError(error)
    by_id, duplicate = {}, False
    for row in payload["value"]:
        if not isinstance(row, dict):
            raise RuntimeError(error)
        rid = row.get("titleId")
        if not isinstance(rid, str) or rid not in title_ids:
            raise RuntimeError(error)
        try:
            version_rows({"value": [row]}, rid)
            version_date(row.get("start"))
            compilation_id(row)
        except ValueError as cause:
            raise RuntimeError(error) from cause
        duplicate |= rid in by_id
        by_id[rid] = row
    return None if duplicate or len(by_id) != len(title_ids) else by_id


def main():
    require_builder_layout(__file__)
    with open(os.path.join(SCRATCH, "titles_all.json"), encoding="utf-8") as f:
        rows = json.load(f)

    # Proper dedup by register id.
    by_id = {}
    for r in rows:
        by_id[register_id(r["id"])] = r
    principal = sorted([r for r in by_id.values() if r.get("isPrincipal")],
                       key=lambda r: r["name"])
    # titles_all.json holds Acts, legislative instruments and notifiable
    # instruments, so both totals count titles rather than Acts. Each row's
    # own `collection` field says which kind it is.
    by_collection = {}
    for r in by_id.values():
        key = r.get("collection") or "unknown"
        by_collection[key] = by_collection.get(key, 0) + 1
    print("distinct in-force titles: %d" % len(by_id))
    print("distinct principal titles:   %d" % len(principal))
    print("by collection:", ", ".join(
        "%s %d" % (name, count) for name, count in sorted(by_collection.items())))

    # Probe the versions filter shape once before looping.
    probe = fetch_json("%s/versions?$top=2&$filter=%s&$select=titleId,start,compilationNumber,isCurrent"
                       % (API, urllib.parse.quote(
                           "titleId eq 'C2004A05138' and isCurrent eq true")))
    print("\nprobe isCurrent filter:", json.dumps(probe.get("value") if probe else None)[:200])
    if not probe or not probe.get("value"):
        # The probe is diagnostic; resolution failures below prevent a partial manifest.
        print("!! isCurrent filter probe returned no rows; "
              "current-version resolution will still be attempted")

    resolved, failed = [], []
    index, batching = 0, True
    while index < len(principal):
        group = principal[index:index + (VERSION_BATCH_SIZE if batching else 1)]
        title_ids = [register_id(t["id"]) for t in group]
        d = fetch_json(_versions_url(title_ids))
        time.sleep(RESOLUTION_DELAY)
        if len(group) > 1 and d is None:
            # No decoded rows to admit; use the established singleton path for this run.
            print("!! version batch unavailable; switching to single-title lookups")
            batching = False
            continue
        rows_by_id = _batch_rows(d, title_ids) if len(group) > 1 else {}
        if rows_by_id is None:
            print("!! ambiguous version batch; retrying %d title(s) individually" % len(group))
        for i, t in enumerate(group, index + 1):
            rid = t["id"]
            if rows_by_id is None:
                payload = fetch_json(_versions_url([rid]))
                time.sleep(RESOLUTION_DELAY)
            else:
                payload = {"value": [rows_by_id[rid]]} if len(group) > 1 else d
            try:
                v = version_rows(payload, rid)
                if v:
                    start = version_date(v[0].get("start"))
                    document_id = compilation_id(v[0])
            except ValueError:
                v = []
            if v:
                rec = dict(t)
                rec["versionStart"] = start
                rec["compilationNumber"] = v[0].get("compilationNumber")
                rec["compilationRegisterId"] = document_id
                resolved.append(rec)
            else:
                failed.append(t)
            if i % 25 == 0:
                print("  resolved %d/%d (failed %d)" % (i, len(principal), len(failed)))
        index += len(group)

    print("\nresolved: %d   failed: %d" % (len(resolved), len(failed)))
    for t in failed[:10]:
        print("   FAILED %s %s" % (t["id"], t["name"][:70]))

    # The next stage treats acts_resolved.json as a complete work list.  A
    # network/API miss must therefore fail the stage instead of publishing a
    # valid-looking partial manifest and letting the corpus quietly shrink.
    if failed:
        raise RuntimeError("version resolution incomplete for %d title(s); "
                           "refusing to write acts_resolved.json"
                           % len(failed))

    write_json_atomic(os.path.join(SCRATCH, "acts_resolved.json"), resolved, indent=1)


if __name__ == "__main__":
    main()
