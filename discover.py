"""Stage 1: discover in-force principal tax Acts, and probe the 'latest' download alias."""
import json, time, urllib.parse, subprocess, sys, os

API = "https://api.prod.legislation.gov.au/v1"
# Corpus root. Override with ATO_KB_ROOT to run this somewhere other than the
# machine it was written on.
OUT = os.environ.get("ATO_KB_ROOT", r"C:\ato-kb")
SCRATCH = os.path.dirname(os.path.abspath(__file__))

KEYWORDS = ["Tax", "Excise", "Superannuation", "Customs Tariff", "Medicare Levy"]
# Acts alone leave out the operative detail: the ITAA, GST and TAA Regulations
# and the FBT rate determinations are all legislative instruments.
COLLECTIONS = ["Act", "LegislativeInstrument", "NotifiableInstrument"]


def curl_json(url, tries=3):
    """curl leaves the previous response in place when a transfer fails, so a
    shared temp file silently returns the PREVIOUS page as if it were this one.
    On the paging path that is exactly how 142 titles went missing."""
    dst = os.path.join(SCRATCH, "_tmp.json")
    for attempt in range(tries):
        if os.path.exists(dst):
            os.remove(dst)
        r = subprocess.run(["curl.exe", "-sL", "--max-time", "90", "-o", dst, url],
                           capture_output=True)
        if r.returncode == 0:
            try:
                with open(dst, encoding="utf-8") as f:
                    d = json.load(f)
                if "error" not in d:
                    return d
            except Exception:
                pass
        time.sleep(5)
    return None


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
        if not d:
            print("  FAILED page skip=%d for %r" % (skip, keyword))
            break
        v = d.get("value", [])
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
        p = subprocess.run(["curl.exe", "-sIL", "--max-time", "60", "-o", os.path.join(SCRATCH, "_h.txt"),
                            "-w", "%{http_code} %{content_type} %{size_download}", u],
                           capture_output=True, text=True)
        print("   %-32s -> %s" % (shape, p.stdout.strip()))
        time.sleep(10)


if __name__ == "__main__":
    main()
