"""Verify the distribution before it goes anywhere.

Checks the claims the distribution makes about itself rather than trusting the
build script that made it: no personal names, no image bytes, every row parses,
every title listed in sources.json is actually present, and nothing links to a
title that was removed.
"""
import collections, glob, json, os, re, sys

from corpus_paths import child, corpus_root, register_id

DIST = child(corpus_root(__file__), "dist")
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
    # Keep this verifier reusable from a regression test process as well as the
    # command line; a previous failing call must not poison a later call.
    fails.clear()

    with open(child(DIST, "sources.json"), encoding="utf-8") as f:
        src = json.load(f)
    titles = src["titles"]
    by_id = {register_id(t["register_id"]): t for t in titles}
    listed = set(by_id)
    markdown_root = child(DIST, "markdown")
    present = {register_id(os.path.basename(d)) for d in glob.glob(os.path.join(markdown_root, "*"))}
    removed = {register_id(e["register_id"]) for e in src.get("excluded_titles", [])}

    check("every listed title has a directory", listed <= present,
          "missing %d" % len(listed - present))
    check("no directory beyond what is listed", present <= listed,
          "extra %d" % len(present - listed))
    check("no removed title present", not (removed & present))

    rows = bad = section_rows = rid_mismatch = 0
    hot = collections.Counter()
    kind_counts = collections.Counter()
    rows_by_coll, words_by_coll = collections.Counter(), collections.Counter()
    for candidate in glob.glob(os.path.join(markdown_root, "*", "sections.jsonl")):
        rid = register_id(os.path.basename(os.path.dirname(candidate)))
        p = child(markdown_root, rid, "sections.jsonl")
        with open(p, encoding="utf-8") as f:
            for l in f:
                if not l.strip():
                    continue
                rows += 1
                try:
                    r = json.loads(l)
                except Exception:
                    bad += 1
                    continue
                if r.get("register_id") != rid:
                    rid_mismatch += 1
                t = r.get("text") or ""
                coll = r.get("collection") or by_id.get(rid, {}).get("collection") or "unknown"
                rows_by_coll[coll] += 1
                words_by_coll[coll] += len(t.split())
                kind = r.get("kind") or ("section" if r.get("section") else "unnumbered")
                kind_counts[kind] += 1
                if r.get("section"):
                    section_rows += 1
                names = {m.group(0) for m in NAME.finditer(t) if not STATUTORY.search(m.group(0))}
                if len(names) >= 3 and len(set(REGNO.findall(t))) >= 3:
                    hot[rid] += 1
    check("every JSONL row parses", bad == 0, "%s rows, %d malformed" % (f"{rows:,}", bad))
    check("each JSONL row matches its title directory", rid_mismatch == 0,
          "%d mismatched register ids" % rid_mismatch)
    check("no row names private individuals", not hot, str(dict(hot))[:60])

    counts = src.get("counts") or {}
    expected_by_coll = {
        coll: {"titles": sum(1 for t in titles if (t.get("collection") or "unknown") == coll),
               "rows": rows_by_coll[coll], "words": words_by_coll[coll]}
        for coll in sorted({t.get("collection") or "unknown" for t in titles})
    }
    check("sources count: titles", counts.get("titles") == len(titles),
          "%r vs %d" % (counts.get("titles"), len(titles)))
    check("sources count: rows", counts.get("jsonl_rows") == rows,
          "%r vs %d" % (counts.get("jsonl_rows"), rows))
    check("sources count: words", counts.get("words_body_only") == sum(words_by_coll.values()),
          "%r vs %d" % (counts.get("words_body_only"), sum(words_by_coll.values())))
    check("sources count: section rows", counts.get("rows_with_section_id") == section_rows,
          "%r vs %d" % (counts.get("rows_with_section_id"), section_rows))
    check("sources count: tracked row kinds",
          counts.get("rows_container") == kind_counts["container"]
          and counts.get("rows_unnumbered") == kind_counts["unnumbered"]
          and counts.get("rows_introductory") == kind_counts["introductory"],
          str(dict(kind_counts)))
    check("sources collection counts", counts.get("by_collection") == expected_by_coll,
          str(counts.get("by_collection"))[:80])
    check("sources declares no distributed EPUB bytes", counts.get("epub_bytes") == 0,
          str(counts.get("epub_bytes")))
    check("title metadata does not point to excluded EPUBs",
          all(t.get("epub") is None and t.get("epub_included") is False for t in titles))

    exts = collections.Counter(os.path.splitext(f)[1].lower()
                               for r, _, fs in os.walk(DIST) for f in fs)
    check("no image files", not any(e in exts for e in
                                    (".png", ".jpg", ".jpeg", ".gif", ".svg", ".epub")),
          str(dict(exts)))

    with open(child(DIST, "INDEX.md"), encoding="utf-8") as f:
        idx = f.read()
    check("INDEX links no removed title", not any(r in idx for r in removed))
    check("INDEX headline matches actual rows", f"{rows:,}" in idx)
    with open(child(DIST, "README.md"), encoding="utf-8") as f:
        rd = f.read()
    check("README states the real title count", "%d in-force principal" % len(titles) in rd)
    with open(child(DIST, "REMOVED.md"), encoding="utf-8") as f:
        removed_md = f.read()
    check("REMOVED.md lists every exclusion", all(r in removed_md for r in removed))

    rt, rates_bad = [], 0
    with open(child(DIST, "rates", "rates.jsonl"), encoding="utf-8") as f:
        for l in f:
            if not l.strip():
                continue
            try:
                rt.append(json.loads(l))
            except Exception:
                rates_bad += 1
    check("every rates JSONL row parses", rates_bad == 0, "%d malformed" % rates_bad)
    check("no rates entry cites a removed title",
           not [r for r in rt if r["register_id"] in removed])
    check("every rates entry cites a present title",
           not [r for r in rt if r["register_id"] not in present])
    with open(child(DIST, "rates", "RATES.md"), encoding="utf-8") as f:
        rates_md = f.read()
    rate_headline = "%s entries across %s titles." % (
        f"{len(rt):,}", f"{len({r.get('register_id') for r in rt if r.get('register_id')}):,}")
    check("RATES headline matches filtered JSONL", rate_headline in rates_md)
    check("RATES contains no removed-title name",
          not any(e.get("name") and e["name"] in rates_md
                  for e in src.get("excluded_titles", [])))

    size = sum(os.path.getsize(os.path.join(r, f))
               for r, _, fs in os.walk(DIST) for f in fs)
    print("\n%s titles | %s rows | %.1f MB | %d removed"
          % (f"{len(titles):,}", f"{rows:,}", size / 1e6, len(removed)))
    print("RESULT:", "all checks passed" if not fails else "FAILED: %s" % fails)
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
