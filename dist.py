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
import collections, json, os, re, shutil

from corpus_paths import child, corpus_root, register_id, reject_symlinks

ROOT = corpus_root(__file__)
DIST = child(ROOT, "dist")
HERE = os.path.dirname(os.path.abspath(__file__))


def render_rates_markdown(records):
    """Render the readable rate index from the records being distributed.

    `rates.jsonl` is deliberately filtered below.  Its human-readable companion
    must be derived from exactly the same records; copying the full-corpus
    RATES.md would retain stale totals and could retain content from a title
    removed for privacy.
    """
    by_topic = collections.Counter(r.get("topic") or "other" for r in records)
    by_kind = collections.Counter(r.get("kind") or "unknown" for r in records)
    by_coll = collections.Counter(r.get("collection") or "unknown" for r in records)

    md = ["# Rates, thresholds and indexation", "",
          "> **Redistributable subset.** Entries for titles excluded from this "
          "distribution are not included.", "",
          "Derived from the Acts and instruments in this distribution. Every entry "
          "cites its title, compilation and section. Check the provision before "
          "relying on a number: this is a finding aid, not advice, and not the "
          "authorised text.", "",
          "%s entries across %s titles." % (
              f"{len(records):,}",
              f"{len({r.get('register_id') for r in records if r.get('register_id')}):,}"),
          "",
          "| Kind | Count |", "|---|---|"]
    for kind, n in by_kind.most_common():
        md.append("| %s | %s |" % (kind, f"{n:,}"))
    md += ["", "| Collection | Count |", "|---|---|"]
    for coll, n in by_coll.most_common():
        md.append("| %s | %s |" % (coll, f"{n:,}"))
    md += ["", "| Topic | Count |", "|---|---|"]
    for topic, n in by_topic.most_common():
        md.append("| %s | %s |" % (topic, f"{n:,}"))

    for topic, _ in by_topic.most_common():
        md += ["", "## " + topic, ""]
        topic_records = [r for r in records if (r.get("topic") or "other") == topic]
        tables = [r for r in topic_records if r.get("kind") == "table"]
        if tables:
            md += ["### Rate tables", ""]
            for r in tables[:40]:
                md.append("**%s**%s %s — %s" % (
                    r.get("act", ""),
                    " _(instrument)_" if (r.get("collection") or "Act") != "Act" else "",
                    ("s " + str(r["section"])) if r.get("section") else "",
                    r.get("heading") or ""))
                md += ["", r.get("content") or "", ""]
            if len(tables) > 40:
                md += ["_%d further tables in rates.jsonl._" % (len(tables) - 40), ""]

        others = [r for r in topic_records if r.get("kind") != "table"]
        if others:
            md += ["### Provisions carrying a number", "",
                   "| Title | Made by | Section | Kind | Amounts | Provision |",
                   "|---|---|---|---|---|---|"]
            for r in others[:120]:
                amounts = ", ".join(r.get("amounts", []))[:40]
                md.append("| %s | %s | %s | %s | %s | %s |" % (
                    (r.get("act") or "").replace("|", "\\|")[:48],
                    "Act" if (r.get("collection") or "Act") == "Act" else "instrument",
                    r.get("section") or "-", r.get("kind") or "unknown", amounts,
                    (r.get("content") or "").replace("|", "\\|").replace("\n", " ")[:150]))
            if len(others) > 120:
                md += ["", "_%d further provisions in rates.jsonl._" % (len(others) - 120)]

    return "\n".join(md) + "\n"


def main():
    with open(os.path.join(HERE, "pii_flagged.json"), encoding="utf-8") as f:
        flagged = json.load(f)
    drop = {register_id(f["register_id"]): f for f in flagged}
    with open(child(ROOT, "sources.json"), encoding="utf-8") as f:
        src = json.load(f)

    # Build only the titles declared by sources.json.  The prior glob copied any
    # stray directory under markdown/, then relied on verification to notice it.
    # It also preserved EPUB paths that do not exist in the redistributable tree.
    titles = []
    for title in src["titles"]:
        rid = register_id(title["register_id"])
        if rid in drop:
            continue
        title = dict(title)
        title["register_id"] = rid
        title["epub"] = None
        title["epub_included"] = False
        titles.append(title)

    if os.path.exists(DIST):
        shutil.rmtree(DIST)
    os.makedirs(child(DIST, "markdown"))
    os.makedirs(child(DIST, "rates"))

    kept_rows = kept_words = section_rows = 0
    kind_counts = collections.Counter()
    stats = collections.Counter((t.get("collection") or "unknown") for t in titles)
    rows_by_coll, words_by_coll = collections.Counter(), collections.Counter()
    for title in titles:
        rid = register_id(title["register_id"])
        d = child(ROOT, "markdown", rid)
        if not os.path.isdir(d):
            raise RuntimeError("listed title %s has no markdown directory" % rid)
        sections = child(d, "sections.jsonl")
        if not os.path.isfile(sections):
            raise RuntimeError("listed title %s has no sections.jsonl" % rid)
        reject_symlinks(d)
        shutil.copytree(d, child(DIST, "markdown", rid))
        with open(sections, encoding="utf-8") as f:
            for l in f:
                if not l.strip():
                    continue
                row = json.loads(l)
                text = row.get("text") or ""
                coll = row.get("collection") or title.get("collection") or "unknown"
                kept_rows += 1
                kept_words += len(text.split())
                rows_by_coll[coll] += 1
                words_by_coll[coll] += len(text.split())
                kind = row.get("kind") or ("section" if row.get("section") else "unnumbered")
                kind_counts[kind] += 1
                if row.get("section"):
                    section_rows += 1

    # The JSONL and its human-readable companion are both generated from this
    # filtered record set.  Copying the source RATES.md would republish full
    # corpus counts and, if a removed title carried an entry, its content.
    rin = child(ROOT, "rates", "rates.jsonl")
    rate_records, rdropped = [], 0
    with open(rin, encoding="utf-8") as f:
        for l in f:
            if not l.strip():
                continue
            record = json.loads(l)
            if record["register_id"] in drop:
                rdropped += 1
                continue
            rate_records.append(record)
    with open(child(DIST, "rates", "rates.jsonl"), "w", encoding="utf-8") as f:
        for record in rate_records:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
    with open(child(DIST, "rates", "RATES.md"), "w", encoding="utf-8") as f:
        f.write(render_rates_markdown(rate_records))

    # sources.json, minus the dropped titles, with every nested count rebuilt
    # from the files actually shipped.  The old code popped nonexistent
    # top-level keys and left sources["counts"] describing the full corpus.
    unavailable = [a for a in src.get("titles_without_epub", [])
                   if a.get("register_id") not in drop]
    counts = {
        "titles": len(titles),
        "acts": stats["Act"],
        "instruments": len(titles) - stats["Act"],
        "by_collection": {
            coll: {"titles": stats[coll], "rows": rows_by_coll[coll],
                   "words": words_by_coll[coll]}
            for coll in sorted(stats)
        },
        "titles_without_epub": len(unavailable),
        "jsonl_rows": kept_rows,
        "words_body_only": kept_words,
        "epub_bytes": 0,
        "rows_with_section_id": section_rows,
        "rows_container": kind_counts["container"],
        "rows_unnumbered": kind_counts["unnumbered"],
        "rows_introductory": kind_counts["introductory"],
        "titles_no_keyword_in_current_name": len(
            [t for t in titles if not t.get("keywords_in_name")]),
        "titles_whole_act_chunk": len(
            [t for t in titles if t.get("granularity") == "whole_act"]),
        "titles_table_block_chunk": len(
            [t for t in titles if t.get("granularity") == "table_block"]),
        "titles_with_endnotes": len([t for t in titles if t.get("endnotes")]),
        "titles_not_current_version": len(
            [t for t in titles if t.get("version_is_current") is False]),
    }
    out = dict(src)
    out["titles"] = titles
    out["counts"] = counts
    out["titles_count"] = len(titles)
    out["titles_without_epub"] = unavailable
    out["titles_not_current_version"] = [
        t for t in src.get("titles_not_current_version", [])
        if t.get("register_id") not in drop
    ]
    out["excluded_titles"] = [
        {"register_id": r, "name": drop[r]["name"],
         "reason": "names private individuals; see REMOVED.md"} for r in sorted(drop)]
    with open(child(DIST, "sources.json"), "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)

    shutil.copy(child(ROOT, "LICENCE-NOTICE.md"),
                child(DIST, "LICENCE-NOTICE.md"))

    # INDEX.md and README.md are generated against the full corpus, so copying
    # them ships a contents page linking to removed titles and a headline count
    # that overstates what you have. Rewrite both against dist.
    label = [("Act", "Acts"), ("LegislativeInstrument", "Legislative instruments"),
             ("NotifiableInstrument", "Notifiable instruments")]

    with open(child(ROOT, "INDEX.md"), encoding="utf-8") as f:
        index_text = f.read()
    idx = index_text.split("\n")
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
    with open(child(DIST, "INDEX.md"), "w", encoding="utf-8") as f:
        f.write(txt)

    with open(child(ROOT, "README.md"), encoding="utf-8") as f:
        rd = f.read()
    # The README's headline wraps across three lines, so a single-line replace
    # misses it and leaves the file claiming 946 titles. Match the numbers
    # themselves, whitespace-tolerant.
    rd = re.sub(r"%s in-force principal titles" % len(src["titles"]),
                "%d in-force principal titles" % len(titles), rd)
    rd = re.sub(r"(?<!\d)\d+ Acts and \d+ legislative and notifiable",
                 "%d Acts and %d legislative and notifiable"
                 % (stats["Act"], len(titles) - stats["Act"]), rd)
    rd = re.sub(r"instruments\.\s+[\d,]+ retrieval rows, [\d,]+ words\.",
                "instruments. %s retrieval rows, %s words."
                % (f"{kept_rows:,}", f"{kept_words:,}"), rd)
    rd = ("> **This is the redistributable subset.** The EPUBs and %d titles that "
          "name private individuals are not included. See REMOVED.md for what was "
          "dropped and why, and run the pipeline yourself for the full corpus.\n\n"
          % len(drop) + rd)
    with open(child(DIST, "README.md"), "w", encoding="utf-8") as f:
        f.write(rd)
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
        "## %d titles naming private individuals" % len(drop),
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
        "registration numbers; these %d were what came back, and a second pass at a "
        "lower threshold found nothing outside them. The only contact details "
        "anywhere in the corpus are five organisational addresses "
        "(two agency inboxes and three switchboard numbers, all published on "
        "the agencies' own sites), "
        "which are left in place." % (f"{len(src['titles']):,}", len(drop)),
    ]
    with open(child(DIST, "REMOVED.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")

    size = sum(os.path.getsize(os.path.join(r, f))
               for r, _, fs in os.walk(DIST) for f in fs)
    print("titles %d (dropped %d) | rows %s | words %s | rates %d (dropped %d) | %.1f MB"
          % (len(titles), len(drop), f"{kept_rows:,}", f"{kept_words:,}",
             len(rate_records), rdropped, size / 1e6))


if __name__ == "__main__":
    main()
