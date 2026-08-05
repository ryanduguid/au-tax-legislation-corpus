"""Verify the distribution before it goes anywhere.

Checks the claims the distribution makes about itself rather than trusting the
build script that made it: no personal names, no image bytes, every row parses,
every title listed in sources.json is actually present, and nothing links to a
title that was removed.
"""
import collections, glob, json, os, re, sys

DIST = os.environ.get("ATO_DIST", r"C:\ato-kb\dist")
NAME = re.compile(r"\b[A-Z][a-z]{1,15},?\s+[A-Z][a-z]{1,15}(?:\s+[A-Z][a-z]{1,15})?\b")
REGNO = re.compile(r"\b\d{8}\b")
STATUTORY = re.compile(
    r"\b(Act|Regulation|Schedule|Division|Subdivision|Part|Chapter|Section|"
    r"Commissioner|Minister|Treasurer|Commonwealth|Australian|Australia|Board|"
    r"Tax|Taxation|Income|Superannuation|Court|Tribunal|Determination|Notice|"
    r"Instrument|Amendment|January|February|March|April|May|June|July|August|"
    r"September|October|November|December)\b")

fails = []


def check(label, ok, detail=""):
    print("  %-52s %s%s" % (label, "PASS" if ok else "FAIL",
                            ("  " + detail) if detail else ""))
    if not ok:
        fails.append(label)


def main():
    src = json.load(open(os.path.join(DIST, "sources.json"), encoding="utf-8"))
    titles = src["titles"]
    listed = {t["register_id"] for t in titles}
    present = {os.path.basename(d) for d in glob.glob(os.path.join(DIST, "markdown", "*"))}
    removed = {e["register_id"] for e in src.get("excluded_titles", [])}

    check("every listed title has a directory", listed <= present,
          "missing %d" % len(listed - present))
    check("no directory beyond what is listed", present <= listed,
          "extra %d" % len(present - listed))
    check("no removed title present", not (removed & present))

    rows = bad = 0
    hot = collections.Counter()
    for p in glob.glob(os.path.join(DIST, "markdown", "*", "sections.jsonl")):
        rid = os.path.basename(os.path.dirname(p))
        for l in open(p, encoding="utf-8"):
            if not l.strip():
                continue
            rows += 1
            try:
                r = json.loads(l)
            except Exception:
                bad += 1
                continue
            t = r.get("text") or ""
            names = {m.group(0) for m in NAME.finditer(t) if not STATUTORY.search(m.group(0))}
            if len(names) >= 3 and len(set(REGNO.findall(t))) >= 3:
                hot[rid] += 1
    check("every JSONL row parses", bad == 0, "%s rows, %d malformed" % (f"{rows:,}", bad))
    check("no row names private individuals", not hot, str(dict(hot))[:60])

    exts = collections.Counter(os.path.splitext(f)[1].lower()
                               for r, _, fs in os.walk(DIST) for f in fs)
    check("no image files", not any(e in exts for e in
                                    (".png", ".jpg", ".jpeg", ".gif", ".svg", ".epub")),
          str(dict(exts)))

    idx = open(os.path.join(DIST, "INDEX.md"), encoding="utf-8").read()
    check("INDEX links no removed title", not any(r in idx for r in removed))
    check("INDEX headline matches actual rows", f"{rows:,}" in idx)
    rd = open(os.path.join(DIST, "README.md"), encoding="utf-8").read()
    check("README states the real title count", "%d in-force principal" % len(titles) in rd)
    check("REMOVED.md lists every exclusion",
          all(r in open(os.path.join(DIST, "REMOVED.md"), encoding="utf-8").read()
              for r in removed))

    rt = [json.loads(l) for l in open(os.path.join(DIST, "rates", "rates.jsonl"),
                                      encoding="utf-8") if l.strip()]
    check("no rates entry cites a removed title",
          not [r for r in rt if r["register_id"] in removed])
    check("every rates entry cites a present title",
          not [r for r in rt if r["register_id"] not in present])

    size = sum(os.path.getsize(os.path.join(r, f))
               for r, _, fs in os.walk(DIST) for f in fs)
    print("\n%s titles | %s rows | %.1f MB | %d removed"
          % (f"{len(titles):,}", f"{rows:,}", size / 1e6, len(removed)))
    print("RESULT:", "all checks passed" if not fails else "FAILED: %s" % fails)
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
