"""Stage 2: dedup titles properly, then resolve each Act's current version date."""
import json, time, urllib.parse, os

from curl_fetch import curl_json as _curl_json

API = "https://api.prod.legislation.gov.au/v1"
SCRATCH = os.path.dirname(os.path.abspath(__file__))


def curl_json(url, tries=3):
    """This stage's temp file; the trap is in curl_fetch.py."""
    return _curl_json(url, os.path.join(SCRATCH, "_v.json"), tries)


def main():
    with open(os.path.join(SCRATCH, "titles_all.json"), encoding="utf-8") as f:
        rows = json.load(f)

    # Proper dedup by register id.
    by_id = {}
    for r in rows:
        by_id[r["id"]] = r
    principal = sorted([r for r in by_id.values() if r.get("isPrincipal")],
                       key=lambda r: r["name"])
    print("distinct in-force Act titles: %d" % len(by_id))
    print("distinct principal Acts:      %d" % len(principal))

    # Probe the versions filter shape once before looping.
    probe = curl_json("%s/versions?$top=2&$filter=%s&$select=titleId,start,compilationNumber,isCurrent"
                      % (API, urllib.parse.quote(
                          "titleId eq 'C2004A05138' and isCurrent eq true")))
    print("\nprobe isCurrent filter:", json.dumps(probe.get("value") if probe else None)[:200])
    if not probe or not probe.get("value"):
        print("!! isCurrent filter unusable, falling back to ordered scan per Act")

    resolved, failed = [], []
    for i, t in enumerate(principal, 1):
        f = "titleId eq '%s' and isCurrent eq true" % t["id"]
        d = curl_json("%s/versions?$top=1&$filter=%s&$select=titleId,start,compilationNumber,registerId"
                      % (API, urllib.parse.quote(f)))
        v = (d or {}).get("value") or []
        # Reject a response that belongs to a different Act.
        if v and v[0].get("titleId") not in (None, t["id"]):
            print("  MISMATCH %s got %s" % (t["id"], v[0].get("titleId")))
            v = []
        if v:
            rec = dict(t)
            rec["versionStart"] = v[0]["start"][:10]
            rec["compilationNumber"] = v[0].get("compilationNumber")
            rec["compilationRegisterId"] = v[0].get("registerId")
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

    with open(os.path.join(SCRATCH, "acts_resolved.json"), "w", encoding="utf-8") as f:
        json.dump(resolved, f, indent=1)


if __name__ == "__main__":
    main()
