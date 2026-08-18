"""Stage 1: discover in-force principal tax Acts, and probe the 'latest' download alias."""
import json, time, urllib.parse, subprocess, os

from curl_fetch import curl_json as _curl_json

API = "https://api.prod.legislation.gov.au/v1"
SCRATCH = os.path.dirname(os.path.abspath(__file__))

KEYWORDS = ["Tax", "Excise", "Superannuation", "Customs Tariff", "Medicare Levy"]
# Acts alone leave out the operative detail: the ITAA, GST and TAA Regulations
# and the FBT rate determinations are all legislative instruments.
COLLECTIONS = ["Act", "LegislativeInstrument", "NotifiableInstrument"]


def curl_json(url, tries=3):
    """This stage's temp file and retry pace; the trap is in curl_fetch.py."""
    return _curl_json(url, os.path.join(SCRATCH, "_tmp.json"), tries, delay=5)


def page_titles(keyword, collection="Act"):
    """Page every in-force title in `collection` whose name contains keyword."""
    f = ("collection eq '%s' and isInForce eq true and contains(name,'%s')"
         % (collection, keyword))
    sel = "id,name,isPrincipal,isInForce,collection,year,number,status"
    rows, skip = [], 0
    while True:
        url = "%s/titles?$top=100&$skip=%d&$orderby=id&$filter=%s&$select=%s" % (
            API, skip, urllib.parse.quote(f), sel)
        d = curl_json(url)
        # A partial title list is worse than no title list: downstream stages
        # treat titles_all.json as authoritative and would build a plausible,
        # silently incomplete corpus.  Do not turn a failed page into a clean
        # end-of-results signal.
        if not isinstance(d, dict) or not isinstance(d.get("value"), list):
            raise RuntimeError("title discovery failed at skip=%d for %r; "
                               "refusing to write an incomplete title list"
                               % (skip, keyword))
        v = d["value"]
        rows += v
        if len({r["id"] for r in rows}) != len(rows):
            raise SystemExit("duplicate ids while paging %r: unordered paging" % keyword)
        if len(v) < 100:
            break
        skip += 100
        time.sleep(2)
    return rows


def main():
    seen, all_rows = set(), []
    for coll in COLLECTIONS:
        for kw in KEYWORDS:
            r = page_titles(kw, coll)
            fresh = []
            for x in r:
                if x["id"] in seen:
                    continue
                seen.add(x["id"])
                fresh.append(x)
            all_rows += fresh
            print("%-22s %-16s matched %4d, new %4d" % (coll, kw, len(r), len(fresh)))
            time.sleep(3)

    principal = [x for x in all_rows if x.get("isPrincipal")]
    print("\ntotal distinct in-force Act titles: %d" % len(all_rows))
    print("of which principal (client-side filter): %d" % len(principal))

    with open(os.path.join(SCRATCH, "titles_all.json"), "w", encoding="utf-8") as f:
        json.dump(all_rows, f, indent=1)
    with open(os.path.join(SCRATCH, "titles_principal.json"), "w", encoding="utf-8") as f:
        json.dump(principal, f, indent=1)

    print("\nsample principal Acts:")
    for x in sorted(principal, key=lambda r: r["name"])[:15]:
        print("   %-12s %s" % (x["id"], x["name"][:80]))

    # Probe whether the /latest/ download alias works, which would remove
    # the need for a per-Act version lookup.
    print("\n--- probing download URL aliases on ITAA 1997 (C2004A05138)")
    for shape in ["latest/text/original/epub",
                  "latest/downloads/epub",
                  "latest/epub"]:
        u = "https://www.legislation.gov.au/C2004A05138/" + shape
        p = subprocess.run(["curl", "-sIL", "--max-time", "60", "-o", os.path.join(SCRATCH, "_h.txt"),
                            "-w", "%{http_code} %{content_type} %{size_download}", u],
                           capture_output=True, text=True)
        print("   %-32s -> %s" % (shape, p.stdout.strip()))
        time.sleep(10)


if __name__ == "__main__":
    main()
