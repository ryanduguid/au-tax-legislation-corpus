"""Verify the distribution before it goes anywhere.

Checks the claims the distribution makes about itself rather than trusting the
build script that made it: no personal names, no image bytes, every row parses,
every title listed in sources.json is actually present, and nothing links to a
title that was removed.
"""
import collections, json, os, sys

from corpus_paths import child, corpus_root, register_id
from pii_patterns import has_private_person_registration_pair

DIST = child(corpus_root(__file__), "dist")

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
    # register_id() refuses a name that is not a Federal Register identifier,
    # which is what keeps a crafted directory name out of every path built
    # below. Calling it straight inside a set comprehension also turned an
    # ordinary stray - a .DS_Store, a hand-copied notes.md, an editor's backup
    # directory - into a ValueError out of main(), so the operator got a
    # traceback instead of a verdict on a distribution about to be published.
    # Collect the refusals instead and report them as the failure they are.
    present, stray = set(), []
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    # glob("*") omits dotfiles, so the first version of this check claimed it
    # covered .DS_Store while silently passing one. scandir inventories every
    # top-level entry and lets the gate distinguish real title directories
    # from files, symlinks and Windows junctions.
    with os.scandir(markdown_root) as iterator:
        entries = sorted(iterator, key=lambda entry: entry.name)
    for entry in entries:
        name = entry.name
        try:
            rid = register_id(name)
        except ValueError:
            stray.append(name)
            continue
        if (entry.is_symlink() or is_junction(entry.path)
                or not entry.is_dir(follow_symlinks=False)):
            stray.append(name)
            continue
        present.add(rid)
    removed = {register_id(e["register_id"]) for e in src.get("excluded_titles", [])}

    check("every listed title has a directory", listed <= present,
          "missing %d" % len(listed - present))
    check("no directory beyond what is listed", present <= listed and not stray,
          "extra %d%s" % (len(present - listed),
                          (", not a title directory: " + ", ".join(stray[:5]))
                          if stray else ""))
    check("no removed title present", not (removed & present))

    rows = bad = section_rows = rid_mismatch = 0
    hot = collections.Counter()
    kind_counts = collections.Counter()
    rows_by_coll, words_by_coll = collections.Counter(), collections.Counter()
    unsafe_row_paths = []
    for rid in sorted(present):
        try:
            p = child(markdown_root, rid, "sections.jsonl")
        except ValueError:
            unsafe_row_paths.append(rid)
            continue
        if os.path.islink(p) or is_junction(p):
            unsafe_row_paths.append(rid)
            continue
        if not os.path.isfile(p):
            continue
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
                if has_private_person_registration_pair(t):
                    hot[rid] += 1
    check("every JSONL row parses", bad == 0, "%s rows, %d malformed" % (f"{rows:,}", bad))
    check("each JSONL row matches its title directory", rid_mismatch == 0,
          "%d mismatched register ids" % rid_mismatch)
    check("row files stay inside real title directories", not unsafe_row_paths,
          ", ".join(unsafe_row_paths[:5]))
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
    check("README does not promise EPUB files", "epub/<register_id>.epub" not in rd)
    check("README does not claim rows naming people",
          "titles name people" not in rd and "disciplined agents" not in rd)
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
