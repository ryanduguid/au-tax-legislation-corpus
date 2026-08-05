"""Build the redistributable subset of the corpus.

Two things in the full corpus should not travel in a published dataset:

  the EPUBs      they embed the Commonwealth Coat of Arms, which sits outside
                 the Register's CC BY 4.0 grant, and any third-party material
                 the Register has not cleared either
  12 titles      the Tax Practitioners Board registers its terminations as
                 notifiable instruments, so those carry roughly 5,400 name
                 mentions with agent registration numbers and the provision
                 breached

The markdown and JSONL carry no image bytes at all — extract.py never emitted
them — so nothing needs stripping there. The 12 titles are dropped whole rather
than redacted: once the name tables go, the only rows left are the Board
secretary's signature block and a figure placeholder, so a partial redaction
would preserve nothing and risk missing a name.

Everything dropped is listed in REMOVED.md with its Register link, so the
omission is visible and reversible from the primary source.
"""
import collections, glob, json, os, re, shutil

ROOT = os.environ.get("ATO_KB_ROOT", r"C:\ato-kb")
DIST = os.path.join(ROOT, "dist")
HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    flagged = json.load(open(os.path.join(HERE, "pii_flagged.json"), encoding="utf-8"))
    drop = {f["register_id"]: f for f in flagged}
    src = json.load(open(os.path.join(ROOT, "sources.json"), encoding="utf-8"))

    if os.path.exists(DIST):
        shutil.rmtree(DIST)
    os.makedirs(os.path.join(DIST, "markdown"))
    os.makedirs(os.path.join(DIST, "rates"))

    kept_rows = kept_titles = kept_words = 0
    for d in sorted(glob.glob(os.path.join(ROOT, "markdown", "*"))):
        rid = os.path.basename(d)
        if rid in drop:
            continue
        shutil.copytree(d, os.path.join(DIST, "markdown", rid))
        kept_titles += 1
        for l in open(os.path.join(d, "sections.jsonl"), encoding="utf-8"):
            if l.strip():
                kept_rows += 1
                kept_words += len((json.loads(l).get("text") or "").split())

    # rates.jsonl derives from the same rows, so it inherits the exclusion.
    rin = os.path.join(ROOT, "rates", "rates.jsonl")
    rkept = rdropped = 0
    with open(os.path.join(DIST, "rates", "rates.jsonl"), "w", encoding="utf-8") as f:
        for l in open(rin, encoding="utf-8"):
            if not l.strip():
                continue
            if json.loads(l)["register_id"] in drop:
                rdropped += 1
                continue
            f.write(l)
            rkept += 1
    shutil.copy(os.path.join(ROOT, "rates", "RATES.md"),
                os.path.join(DIST, "rates", "RATES.md"))

    # sources.json, minus the dropped titles, with the count corrected.
    titles = [t for t in src["titles"] if t["register_id"] not in drop]
    out = dict(src, titles=titles)
    for k in ("titles_total", "acts", "instruments"):
        out.pop(k, None)
    out["titles_count"] = len(titles)
    out["excluded_titles"] = [
        {"register_id": r, "name": drop[r]["name"],
         "reason": "names private individuals; see REMOVED.md"} for r in sorted(drop)]
    json.dump(out, open(os.path.join(DIST, "sources.json"), "w", encoding="utf-8"),
              indent=1, ensure_ascii=False)

    shutil.copy(os.path.join(ROOT, "LICENCE-NOTICE.md"),
                os.path.join(DIST, "LICENCE-NOTICE.md"))

    # INDEX.md and README.md are generated against the full corpus, so copying
    # them ships a contents page linking to 12 titles that are not here and a
    # headline count that overstates what you have. Rewrite both against dist.
    stats = collections.Counter()
    rows_by_coll, words_by_coll = collections.Counter(), collections.Counter()
    for t in titles:
        c = t.get("collection") or "unknown"
        stats[c] += 1
        rows_by_coll[c] += t.get("jsonl_rows", 0)
        words_by_coll[c] += t.get("words", 0)
    label = [("Act", "Acts"), ("LegislativeInstrument", "Legislative instruments"),
             ("NotifiableInstrument", "Notifiable instruments")]

    idx = open(os.path.join(ROOT, "INDEX.md"), encoding="utf-8").read().split("\n")
    keep_lines, dropped_lines = [], 0
    for ln in idx:
        if any(r in ln for r in drop):
            dropped_lines += 1
            continue
        keep_lines.append(ln)
    txt = "\n".join(keep_lines)

    # Rewrite the counts from what dist actually holds. String-matching the
    # original numbers is what let "946 titles ... 21,784 rows" survive into a
    # file describing 934 and 21,596 — the summary line has to be rebuilt, not
    # patched.
    txt = re.sub(
        r"^[\d,]+ titles \([\d,]+ Acts, [\d,]+ instruments\), [\d,]+ retrieval rows, [\d,]+ words\.$",
        "%s titles (%d Acts, %d instruments), %s retrieval rows, %s words.\n\n"
        "%d further titles were removed before publication; see REMOVED.md."
        % (f"{len(titles):,}", stats["Act"], len(titles) - stats["Act"],
           f"{kept_rows:,}", f"{kept_words:,}", len(drop)),
        txt, count=1, flags=re.M)

    for key, lab in label:
        txt = re.sub(r"^\| %s \| [\d,]+ \| [\d,]+ \| [\d,]+ \|$" % re.escape(lab),
                     "| %s | %s | %s | %s |" % (lab, f"{stats[key]:,}",
                                                f"{rows_by_coll[key]:,}",
                                                f"{words_by_coll[key]:,}"),
                     txt, count=1, flags=re.M)
        txt = re.sub(r"^## %s \(\d+\)$" % re.escape(lab),
                     "## %s (%d)" % (lab, stats[key]), txt, count=1, flags=re.M)
    open(os.path.join(DIST, "INDEX.md"), "w", encoding="utf-8").write(txt)

    rd = open(os.path.join(ROOT, "README.md"), encoding="utf-8").read()
    # The README's headline wraps across three lines, so a single-line replace
    # misses it and leaves the file claiming 946 titles. Match the numbers
    # themselves, whitespace-tolerant.
    rd = re.sub(r"%s in-force principal titles" % len(src["titles"]),
                "%d in-force principal titles" % len(titles), rd)
    rd = re.sub(r"175 Acts and \d+ legislative and notifiable",
                "%d Acts and %d legislative and notifiable"
                % (stats["Act"], len(titles) - stats["Act"]), rd)
    rd = re.sub(r"instruments\.\s+[\d,]+ retrieval rows, [\d,]+ words\.",
                "instruments. %s retrieval rows, %s words."
                % (f"{kept_rows:,}", f"{kept_words:,}"), rd)
    rd = ("> **This is the redistributable subset.** The EPUBs and 12 titles that "
          "name private individuals are not included. See REMOVED.md for what was "
          "dropped and why, and run the pipeline yourself for the full corpus.\n\n"
          + rd)
    open(os.path.join(DIST, "README.md"), "w", encoding="utf-8").write(rd)
    print("INDEX.md: dropped %d lines referencing removed titles" % dropped_lines)

    lines = [
        "# What was removed from this distribution",
        "",
        "The full corpus this was built from covers %s titles. This distribution "
        "carries %s." % (f"{len(src['titles']):,}", f"{len(titles):,}"),
        "",
        "## The EPUBs",
        "",
        "Not included. The Register serves them with the Commonwealth Coat of Arms "
        "embedded, and the Coat of Arms is excluded from the CC BY 4.0 grant "
        "covering everything else. Stripping it would also destroy the only reason "
        "to ship them, which is that they are byte-identical to what "
        "legislation.gov.au served. Run `download.py` to fetch your own.",
        "",
        "The markdown and JSONL contain no image data of any kind. Where the source "
        "had a figure, the text carries a marker in its place: "
        "`[Commonwealth Coat of Arms omitted, not licensed under CC BY]` for the "
        "Coat of Arms, `[image not described in source: ...]` for everything else.",
        "",
        "## 12 titles naming private individuals",
        "",
        "The Tax Practitioners Board registers terminations and suspensions of tax "
        "and BAS agents as notifiable instruments. Each is a table of named people "
        "with their registration number and the provision they breached. That is "
        "public on the Register, where it is a PDF you read one at a time. Shipping "
        "it as rows in a dataset makes it name-searchable at scale, which is a "
        "different act, so these are omitted.",
        "",
        "Each remains available from the Register at "
        "`https://www.legislation.gov.au/<register_id>/latest/text`.",
        "",
        "| Register ID | Rows | Title |",
        "|---|---|---|",
    ]
    for r in sorted(drop, key=lambda x: -drop[x]["names_est"]):
        f = drop[r]
        lines.append("| [%s](https://www.legislation.gov.au/%s/latest/text) | %d | %s |"
                     % (r, r, f["rows_total"], f["name"].replace("|", "\\|")))
    lines += [
        "",
        "Detection did not rely on the titles being recognisable. Every row in all "
        "%s titles was tested for personal names appearing alongside agent "
        "registration numbers; these 12 were what came back, and a second pass at a "
        "lower threshold found nothing outside them. The only contact details "
        "anywhere in the corpus are five organisational addresses "
        "(two agency inboxes and three switchboard numbers, all published on "
        "the agencies' own sites), "
        "which are left in place." % f"{len(src['titles']):,}",
    ]
    open(os.path.join(DIST, "REMOVED.md"), "w", encoding="utf-8").write(
        "\n".join(lines) + "\n")

    size = sum(os.path.getsize(os.path.join(r, f))
               for r, _, fs in os.walk(DIST) for f in fs)
    print("titles %d (dropped %d) | rows %s | words %s | rates %d (dropped %d) | %.1f MB"
          % (kept_titles, len(drop), f"{kept_rows:,}", f"{kept_words:,}",
             rkept, rdropped, size / 1e6))


if __name__ == "__main__":
    main()
