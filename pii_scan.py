"""Find rows in the corpus that name private individuals.

The TPB registers its terminations as notifiable instruments, so the corpus
carries the names of tax agents it has disciplined. Those rows are public on the
Register but should not travel in a redistributable dataset.

This does not trust the earlier spot-finding. It sweeps all 946 titles for the
shape of the thing: a row carrying several personal names alongside an agent
registration number. Signatories, Ministers and case citations are named in
ordinary legislation constantly, so a bare name test would flag the whole
corpus; the registration number is what separates a disciplinary register from
a statute.
"""
import collections, glob, json, os, re

from corpus_paths import child, corpus_root, register_id

ROOT = corpus_root(__file__)

# "Smith, John" or "John Smith" — two or three capitalised words, no statutory
# vocabulary in them.
NAME = re.compile(r"\b[A-Z][a-z]{1,15},?\s+[A-Z][a-z]{1,15}(?:\s+[A-Z][a-z]{1,15})?\b")
# TPB registration numbers run 8 digits; ABNs 11.
REGNO = re.compile(r"\b\d{8}\b")
STATUTORY = re.compile(
    r"\b(Act|Regulation|Schedule|Division|Subdivision|Part|Chapter|Section|"
    r"Commissioner|Minister|Treasurer|Commonwealth|Australian|Australia|Board|"
    r"Tax|Taxation|Income|Superannuation|Court|Tribunal|Determination|Notice|"
    r"Instrument|Amendment|January|February|March|April|May|June|July|August|"
    r"September|October|November|December)\b")


def person_names(text):
    out = set()
    for m in NAME.finditer(text):
        s = m.group(0)
        if STATUTORY.search(s):
            continue
        out.add(s)
    return out


def main():
    src = json.load(open(child(ROOT, "sources.json"), encoding="utf-8"))
    byid = {t["register_id"]: t for t in src["titles"]}
    flagged = []

    markdown_root = child(ROOT, "markdown")
    for candidate in sorted(glob.glob(os.path.join(markdown_root, "*", "sections.jsonl"))):
        rid = register_id(os.path.basename(os.path.dirname(candidate)))
        p = child(markdown_root, rid, "sections.jsonl")
        rows = [json.loads(l) for l in open(p, encoding="utf-8") if l.strip()]
        hot = []
        for i, r in enumerate(rows):
            t = r.get("text") or ""
            regs = len(set(REGNO.findall(t)))
            names = person_names(t)
            # A disciplinary table has many of both. A signature block has one
            # name and no registration number.
            if regs >= 3 and len(names) >= 3:
                hot.append((i, r.get("row_id"), len(names), regs, len(t.split())))
        if hot:
            flagged.append({
                "register_id": rid,
                "name": byid.get(rid, {}).get("name", "?"),
                "collection": byid.get(rid, {}).get("collection"),
                "rows_total": len(rows),
                "rows_flagged": len(hot),
                "names_est": sum(h[2] for h in hot),
                "words": sum(h[4] for h in hot),
                "row_ids": [h[1] for h in hot],
            })

    flagged.sort(key=lambda x: -x["names_est"])
    print("titles carrying person-name rows: %d" % len(flagged))
    print("%-13s %-6s %-7s %-8s %s" % ("REGISTER_ID", "ROWS", "FLAG", "NAMES", "TITLE"))
    for f in flagged:
        print("  %-12s %5d %6d %7d  %s"
              % (f["register_id"], f["rows_total"], f["rows_flagged"],
                 f["names_est"], f["name"][:52]))
    print("\ntotal flagged rows: %d | est. distinct names: %d | words: %s"
          % (sum(f["rows_flagged"] for f in flagged),
             sum(f["names_est"] for f in flagged),
             f"{sum(f['words'] for f in flagged):,}"))
    json.dump(flagged, open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                         "pii_flagged.json"), "w", encoding="utf-8"),
              indent=1)


if __name__ == "__main__":
    main()
