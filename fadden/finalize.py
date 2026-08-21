"""Stage 5: write sources.json, INDEX.md, README.md, LICENCE-NOTICE.md and the
staleness checker."""
import json, os, re, shutil, datetime

from corpus_paths import child, corpus_root, register_id

SCRATCH = os.path.dirname(os.path.abspath(__file__))
ROOT = corpus_root(__file__)

KEYWORDS = ["Tax", "Excise", "Superannuation", "Customs Tariff", "Medicare Levy"]
# The Register's contains(name,...) matches more than the current display name:
# it caught the Passenger Movement Charge Act 1978 on its former title, Departure
# Tax Act 1978. That is worth having, so nothing is excluded on the name alone.
# This only records which keywords are visible in the name now, so a reader can
# filter the handful that came in on a substring (AD/ROTAX/... is an
# airworthiness directive that matched "tax" inside "Rotax").
def keywords_in_name(name):
    return [k for k in KEYWORDS
            if re.search(r'(?<![A-Za-z])' + re.escape(k), name, re.I)]


LICENCE = "CC BY 4.0"
LICENCE_URL = "https://creativecommons.org/licenses/by/4.0/"
REGISTER = "https://www.legislation.gov.au"


def pii_summary():
    """Totals for the README paragraph on titles naming people.

    Derived from pii_flagged.json rather than hardcoded in prose, which is how
    the corpus README came to claim eleven titles and 3,750 names against a
    committed scan showing twelve and 5,404. The documented pipeline runs
    pii_scan.py and pii_scan2.py before this stage, so the scan output must
    exist here. A missing or unreadable file means the stages ran out of
    order; refuse to write a README carrying unverified counts.
    """
    path = os.path.join(SCRATCH, "pii_flagged.json")
    try:
        with open(path, encoding="utf-8") as f:
            flagged = json.load(f)
        titles = len(flagged)
        names = sum(int(t.get("names_est") or 0) for t in flagged)
    except (OSError, ValueError, TypeError) as error:
        raise RuntimeError(
            "pii_flagged.json is missing or unreadable (%s); run pii_scan.py "
            "and pii_scan2.py before finalize.py so the README's PII counts "
            "come from a real scan" % error)
    # The paragraph says "about", so round the mention count to the nearest 50.
    return titles, int(round(names / 50.0)) * 50

# The Register requires different wording depending on whether the content was
# changed. The EPUBs are byte-identical to what it served; everything derived
# from them is not.
ATTR_UNCHANGED = ("Sourced from the Federal Register of Legislation at %s. "
                  "For the latest information on Australian Government law "
                  "please go to " + REGISTER)
ATTR_CHANGED = ("Based on content from the Federal Register of Legislation at %s. "
                "For the latest information on Australian Government legislation "
                "please go to " + REGISTER + ". Changes: converted from EPUB to "
                "markdown, contents pages removed, endnotes separated, images replaced "
                "by their descriptive alt text, compilation cover pages and running "
                "headers omitted.")


def main(retrieved):
    with open(os.path.join(SCRATCH, "manifest_md.json"), encoding="utf-8") as f:
        md = json.load(f)
    with open(os.path.join(SCRATCH, "manifest_raw.json"), encoding="utf-8") as f:
        raw = json.load(f)

    missing = [a for a in raw if not a.get("epub")]
    ok = sorted([a for a in md if a.get("markdown")], key=lambda a: a["name"])

    # Count the rows actually written, not the ones parsed, and break the
    # null-section rows down by kind. Lumping them together as "containers"
    # overstated the structural ones.
    tot_kind = {}
    for a in ok:
        p = child(ROOT, "markdown", register_id(a["id"]), "sections.jsonl")
        n = sec = 0
        with open(p, encoding="utf-8") as source:
            for line in source:
                if not line.strip():
                    continue
                n += 1
                row = json.loads(line)
                if row.get("section"):
                    sec += 1
                kind = row.get("kind") or ("section" if row.get("section") else "unnumbered")
                tot_kind[kind] = tot_kind.get(kind, 0) + 1
        a["_rows"], a["_sec_rows"] = n, sec

    tot_rows = sum(a["_rows"] for a in ok)
    tot_sec_rows = sum(a["_sec_rows"] for a in ok)
    tot_words = sum(a.get("words", 0) for a in ok)
    tot_bytes = sum(a.get("bytes", 0) for a in ok)

    # An Act and a regulation made under it carry very different authority, so
    # every count is broken out by collection rather than reported as one total.
    by_coll = {}
    for a in ok:
        c = a.get("collection") or "unknown"
        d = by_coll.setdefault(c, {"titles": 0, "rows": 0, "words": 0})
        d["titles"] += 1
        d["rows"] += a["_rows"]
        d["words"] += a.get("words", 0)
    n_act = by_coll.get("Act", {}).get("titles", 0)
    n_inst = len(ok) - n_act

    sources = {
        "corpus": "Commonwealth tax statutes and legislative instruments",
        "retrieved": retrieved,
        "source": "Federal Register of Legislation",
        "source_api": "https://api.prod.legislation.gov.au/v1/",
        "licence": LICENCE,
        "licence_url": LICENCE_URL,
        "attribution_epub": ATTR_UNCHANGED % retrieved,
        "attribution_markdown": ATTR_CHANGED % retrieved,
        "licence_note": (
            "Excludes the Commonwealth Coat of Arms and any third-party material. "
            "The EPUBs under epub/ embed the Coat of Arms as an image; it is NOT "
            "licensed under CC BY and must not be reused. The markdown omits it. "
            "Authorised versions are PDF only, stamped under sections 15ZA and "
            "15ZB of the Legislation Act 2003; the EPUB reading view used here is "
            "NOT the authorised text."),
        "selection": (
            "In-force principal titles in the Act, LegislativeInstrument and "
            "NotifiableInstrument collections whose name contains Tax, Excise, "
            "Superannuation, Customs Tariff or Medicare Levy. isPrincipal could not "
            "be applied server-side (the API returns 400 for any filter containing "
            "it), so it was applied client-side. Selection is by title keyword, so a "
            "tax-relevant title without one of those words in its name is absent, and "
            "some collected titles (notably Commonwealth employee superannuation "
            "schemes) are not tax law. Check the collection field before relying on a "
            "provision: an Act and a regulation made under it are not interchangeable."),
        "counts": {
            "titles": len(ok),
            "acts": n_act,
            "instruments": n_inst,
            "by_collection": by_coll,
            "titles_without_epub": len(missing),
            "jsonl_rows": tot_rows,
            "words_body_only": tot_words,
            "epub_bytes": tot_bytes,
            "rows_with_section_id": tot_sec_rows,
            "rows_container": tot_kind.get("container", 0),
            "rows_unnumbered": tot_kind.get("unnumbered", 0),
            "rows_introductory": tot_kind.get("introductory", 0),
            "titles_no_keyword_in_current_name": len(
                [a for a in ok if not keywords_in_name(a["name"])]),
            "titles_whole_act_chunk": len([a for a in ok if a.get("granularity") == "whole_act"]),
            "titles_table_block_chunk": len(
                [a for a in ok if a.get("granularity") == "table_block"]),
            "titles_with_endnotes": len([a for a in ok if a.get("endnotes")]),
            "titles_not_current_version": len(
                [a for a in ok if a.get("version_is_current") is False]),
        },
        "titles_not_current_version": [
            {"register_id": a["id"], "name": a["name"],
             "collection": a.get("collection"),
             "text_is_version_from": a.get("versionStart"),
             "compilation_number": a.get("compilationNumber"),
             "superseded_from": a.get("current_version_start"),
             "reason": "the in-force version has no published compilation on the Register"}
            for a in ok if a.get("version_is_current") is False
        ],
        "titles": [
            {
                "register_id": a["id"],
                "name": a["name"],
                "collection": a.get("collection"),
                "keywords_in_name": keywords_in_name(a["name"]),
                "long_title": a.get("long_title"),
                "compilation_number": a.get("compilationNumber"),
                "compilation_date": a.get("versionStart"),
                # False for the handful of titles whose in-force version has no
                # published document, so the text here is the last compilation
                # the Register actually holds. current_version_start is the
                # commencement that text does not yet reflect.
                "version_is_current": a.get("version_is_current", True),
                "current_version_start": a.get("current_version_start"),
                "retrieved": a.get("retrieved"),
                "jsonl_rows": a["_rows"],
                "granularity": a.get("granularity"),
                "words": a.get("words"),
                "epub_bytes": a.get("bytes"),
                "epub": "epub/%s" % a["epub"],
                "markdown": "markdown/%s" % a["markdown"],
                "sections_jsonl": "markdown/%s/sections.jsonl" % a["id"],
                "endnotes": ("markdown/%s/endnotes.md" % a["id"]) if a.get("endnotes") else None,
                "source_url": a.get("sourceUrl"),
                "register_page": "%s/%s/latest/text" % (REGISTER, a["id"]),
            } for a in ok
        ],
        "titles_without_epub": [
            # download.py stops on HTTP and content errors rather than record
            # them, so a missing title carries only its 'reason': the explicit
            # no-document case. The httpCode/contentType keys read here before
            # were never written by any downloader this repository has held.
            {"register_id": a["id"], "name": a["name"],
             "collection": a.get("collection"),
             "reason": a.get("reason")}
            for a in missing
        ],
    }
    with open(child(ROOT, "sources.json"), "w", encoding="utf-8") as f:
        json.dump(sources, f, indent=1, ensure_ascii=False)

    COLL_LABEL = [("Act", "Acts"),
                  ("LegislativeInstrument", "Legislative instruments"),
                  ("NotifiableInstrument", "Notifiable instruments")]
    idx = ["# Commonwealth tax legislation: index", "",
           "Retrieved %s from the Federal Register of Legislation." % retrieved,
           "Licence [%s](%s). Attribution and limits: see README.md." % (LICENCE, LICENCE_URL),
           "",
           "%d titles (%d Acts, %d instruments), %s retrieval rows, %s words." % (
               len(ok), n_act, n_inst, f"{tot_rows:,}", f"{tot_words:,}"),
           "",
           "| Collection | Titles | Rows | Words |", "|---|---|---|---|"]
    for key, label in COLL_LABEL:
        d = by_coll.get(key)
        if d:
            idx.append("| %s | %s | %s | %s |" % (
                label, f"{d['titles']:,}", f"{d['rows']:,}", f"{d['words']:,}"))

    for key, label in COLL_LABEL:
        group = [a for a in ok if (a.get("collection") or "unknown") == key]
        if not group:
            continue
        idx += ["", "## %s (%d)" % (label, len(group)), "",
                "| Title | Register ID | Comp. | Comp. date | Rows | Words |",
                "|---|---|---|---|---|---|"]
        for a in group:
            idx.append("| [%s](markdown/%s) | %s | %s | %s | %s | %s |" % (
                a["name"].replace("|", "\\|"), a["markdown"], a["id"],
                a.get("compilationNumber") or "-", a.get("versionStart") or "-",
                f"{a['_rows']:,}", f"{a.get('words', 0):,}"))
    leftover = [a for a in ok if (a.get("collection") or "unknown")
                not in {k for k, _ in COLL_LABEL}]
    if leftover:
        idx += ["", "## Other collections (%d)" % len(leftover), "",
                "| Title | Collection | Register ID | Rows |", "|---|---|---|---|"]
        for a in leftover:
            idx.append("| [%s](markdown/%s) | %s | %s | %s |" % (
                a["name"].replace("|", "\\|"), a["markdown"],
                a.get("collection") or "-", a["id"], f"{a['_rows']:,}"))
    if missing:
        idx += ["", "## Titles with no EPUB available", "",
                "| Title | Collection | Register ID | Reason |",
                "|---|---|---|---|"]
        for a in missing:
            idx.append("| %s | %s | %s | %s |" % (
                a["name"], a.get("collection") or "-", a["id"],
                a.get("reason") or "-"))
    with open(child(ROOT, "INDEX.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(idx) + "\n")

    with open(child(ROOT, "LICENCE-NOTICE.md"), "w", encoding="utf-8") as f:
        f.write("""# Licence notice

Commonwealth legislation from the Federal Register of Legislation, licensed
[Creative Commons Attribution 4.0 International](%s).

## Attribution for the raw EPUBs under `epub/`

> %s

## Attribution for the markdown, JSONL and index (changed content)

> %s

## Not covered by CC BY

The Commonwealth Coat of Arms and any third-party copyright material are
excluded from the licence. The EPUBs under `epub/` embed the Coat of Arms as an
image. Do not reuse it. The markdown omits all images and keeps only their
descriptive alt text.

## Not the authorised text

Authorised versions are PDF only, stamped under sections 15ZA and 15ZB of the
Legislation Act 2003. Everything here derives from the EPUB reading view.
""" % (LICENCE_URL, ATTR_UNCHANGED % retrieved, ATTR_CHANGED % retrieved))

    readme = """# Commonwealth tax legislation knowledge base

Built {retrieved} from the Federal Register of Legislation.

## What this is

{n} in-force principal titles covering tax, excise, superannuation, customs
tariff and Medicare levy: {n_act} Acts and {n_inst} legislative and notifiable
instruments. {rows} retrieval rows, {words} words.

Each title is stored as:

- `epub/<register_id>.epub`: the file exactly as the Register served it
- `markdown/<register_id>/<register_id>.md`: full text with YAML frontmatter
- `markdown/<register_id>/sections.jsonl`: one row per section, ready for RAG
- `markdown/<register_id>/endnotes.md`: amendment history, kept out of the sections

Plus a derived index:

- `rates/rates.jsonl`: every provision carrying a rate, threshold, factor or
  indexation rule, bucketed by topic and cited back to its section
- `rates/RATES.md`: the same, readable

`sources.json` records the register id, collection, compilation number and
compilation date for every title.

**Check the `collection` field before relying on a provision.** An Act and a
regulation made under it are not interchangeable, and an instrument can be
disallowed or sunset while its enabling Act stands.

## Checking staleness

    python check_current.py
    python check_current.py Act                    # one collection only

It asks the Register API for each title's current compilation and reports four
things separately: superseded compilations, titles whose in-force version has no
published compilation, titles no longer in force, and lookups that failed.
Read-only, about {mins} minutes for everything.

No run's counts are repeated here: it is a separate command, run after this
README is written, and its report goes to stdout. Its four buckets are not
interchangeable. The {notcurrent} titles listed in `sources.json` under
`titles_not_current_version` belong to the second bucket, not the first: their
in-force version has no published compilation, so there is no newer document to
re-download and the path built from that date answers 404.

## Licence and attribution

Licensed [{lic}]({lic_url}). Full statement in LICENCE-NOTICE.md. Every JSONL row
carries its own attribution, because rows travel independently of this file.

The markdown is CHANGED content (converted from EPUB, contents pages removed,
endnotes separated), so it uses the Register's "Based on content from" wording.
The EPUBs are unchanged and use "Sourced from".

The Commonwealth Coat of Arms is not covered by CC BY. It is embedded in the
EPUBs; the markdown omits all images and keeps only their alt text.

## Limits

**Not the authorised text.** Authorised versions are PDF only, stamped under
sections 15ZA and 15ZB of the Legislation Act 2003. Fine for research and
retrieval, not for citation in formal advice.

**Legislation only.** No ATO rulings, determinations or practice statements, no
case law, no explanatory memoranda. Figures the ATO computes rather than
Parliament enacts are therefore absent: the FBT gross-up factors, the Division
7A benchmark rate and the indexed superannuation caps among them. The Acts and
instruments give you the formula; the ATO publishes the result.

**Selection is by title keyword.** A tax-relevant title without Tax, Excise,
Superannuation, Customs Tariff or Medicare Levy in its name is absent: the
Customs Act 1901 and Charities Act 2013 among them.

**Some collected titles are not tax law.** Three ways they got in:

- The Commonwealth employee superannuation schemes match "Superannuation".
- The Register matches more than the current name. That is mostly a benefit:
  the Passenger Movement Charge Act 1978 came in on its former title, the
  Departure Tax Act 1978. `keywords_in_name` in `sources.json` is empty for
  these, so they can be reviewed rather than guessed at.
- A substring match pulled in roughly 35 documents that have nothing to do with
  tax: about 24 aviation airworthiness directives, because "Rotax" and "Britax"
  contain "tax", and around 10 social security instruments about taxi licence
  buybacks, because "taxi" starts with it. Every one is named `AD/...` or
  `Social Security (Exempt Lump Sum ...)`. Filter on `keywords_in_name` plus the
  name to drop them; nothing was deleted, because the same rule strict enough to
  catch Rotax also throws away the Departure Tax Act.

**Sunsetting.** Legislative instruments expire under Part 4 of the Legislation
Act 2003 unless remade. An instrument in force on the build date may have sunset
since. `check_current.py` catches a changed compilation, not a repeal.

**Tables** are converted to markdown tables. Cells spanning several columns are
padded so later cells keep their position, except where a table's spans vary row
to row, in which case the unpadded grid is kept because padding scatters those
values. Images inside a cell stay in that cell.

**Some instruments have no structure to find.** {whole} of them are stored as a
single `whole_act` row rather than split by section, because the document
genuinely has no sections and is short enough that one row is the right chunk.
No text is lost: retrieval returns the whole instrument.

**Table-shaped instruments are split on their tables.** {tabled} titles carry
`granularity: table_block`: no headings anywhere, just a run of tables. The Tax
Practitioners Board publishes its terminations this way, and Excise By-law No.
127 prescribes petroleum fields one table per basin. Each table becomes a row,
carrying the prose that preceded it: the sentence naming the enabling
provision, or the basin name. The largest of these was a single 14,928-word row
before the split.

**{pii_titles} titles name people.** The Tax Practitioners Board registers its
terminations and suspensions as notifiable instruments, so the corpus carries
about {pii_names} name mentions of disciplined agents with their registration
numbers and the provision they fell foul of. That text is public on the
Register, but splitting it into rows
makes it searchable by name in a way the source document is not. Exclude
`granularity: table_block` rows whose title starts with "Termination and
Suspension" or "TPB Termination" if that is not wanted.

**{notcurrent} titles are not the current text.** The Register can record that a
version commenced without publishing a compilation for it, and every document
path built from that date answers 404. Those titles carry the last compilation
that exists, marked `version_is_current: false` in the front matter and on every
JSONL row, with `superseded_from` giving the commencement the text does not
reflect. `sources.json` lists them under `titles_not_current_version`.

**Rows are sections or containers.** A row with a `section` id is a section. A
row with `section: null` is a container: text sitting directly under a Chapter,
Part, Division or Schedule heading rather than under a numbered section. Its
`heading` names the container. Schedules in particular hold large bodies of text
that belong to no section number.

**Per-Act retrieval dates vary.** Each Act's `retrieved` field and attribution
carry the date that Act's EPUB was actually downloaded, not the build date.

## Parser notes

Section numbers render as `40 <U+2011> 1`: digits joined by a non-breaking hyphen
padded with non-breaking spaces. A regex expecting a plain adjacent hyphen
matches nothing.

Table cells wrap content in `<p>`, so cell text must accumulate in a buffer that
survives a nested paragraph.
"""
    pii_titles, pii_names = pii_summary()
    with open(child(ROOT, "README.md"), "w", encoding="utf-8") as f:
        f.write(readme.format(
            retrieved=retrieved, n=len(ok), n_act=n_act, n_inst=n_inst,
            rows=f"{tot_rows:,}",
            words=f"{tot_words:,}", lic=LICENCE, lic_url=LICENCE_URL,
            mins=max(1, round(len(ok) * 1.5 / 60)),
            pii_titles=pii_titles, pii_names=f"{pii_names:,}",
            whole=len([a for a in ok if a.get("granularity") == "whole_act"]),
            tabled=len([a for a in ok if a.get("granularity") == "table_block"]),
            notcurrent=len([a for a in ok if a.get("version_is_current") is False])))

    # finalize.py also ships inside build/, so a re-run from there would copy a
    # file onto itself and raise SameFileError after the published documents
    # have already been rewritten.
    def copy_if_different(src_p, dst_p):
        if os.path.abspath(src_p) != os.path.abspath(dst_p):
            shutil.copy(src_p, dst_p)

    build_dir = child(ROOT, "build")
    os.makedirs(build_dir, exist_ok=True)
    for name in ("check_current.py", "corpus_paths.py", "curl_fetch.py"):
        copy_if_different(os.path.join(SCRATCH, name), child(ROOT, name))
    for name in ("discover.py", "versions.py", "download.py", "extract.py",
                 "finalize.py", "check_current.py", "corpus_paths.py",
                 "curl_fetch.py"):
        copy_if_different(os.path.join(SCRATCH, name),
                          child(build_dir, name))

    print("titles=%d (acts=%d instruments=%d) rows=%d words=%d epub=%.1f MB "
          "missing=%d whole_act=%d"
          % (len(ok), n_act, n_inst, tot_rows, tot_words, tot_bytes / 1e6,
             len(missing),
             len([a for a in ok if a.get("granularity") == "whole_act"])))
    print("by collection:", {k: v["titles"] for k, v in sorted(by_coll.items())})


if __name__ == "__main__":
    main(datetime.date.today().isoformat())
