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
        # No fallback exists: an earlier revision advertised an "ordered scan
        # per Act" here that was never implemented. If the isCurrent filter is
        # unusable, every per-Act lookup below comes back empty, each title
        # lands in `failed`, and the stage raises instead of writing a partial
        # acts_resolved.json. The probe result is a diagnostic, not a switch.
        print("!! isCurrent filter unusable; every per-Act lookup will fail "
              "and this stage will refuse to write acts_resolved.json")

    resolved, failed = [], []
    for i, t in enumerate(principal, 1):
        rid = register_id(t["id"])
        f = "titleId eq '%s' and isCurrent eq true" % rid
        d = fetch_json("%s/versions?$top=1&$filter=%s&$select=titleId,start,compilationNumber,registerId"
                       % (API, urllib.parse.quote(f)))
        try:
            v = version_rows(d, rid)
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
        time.sleep(1.5)

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
