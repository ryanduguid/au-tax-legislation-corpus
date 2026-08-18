"""Build the redistributable subset of the corpus.

Two things in the full corpus should not travel in a published dataset:

  the EPUBs      they embed the Commonwealth Coat of Arms, which sits outside
                 the Register's CC BY 4.0 grant, and any third-party material
                 the Register has not cleared either
  12 titles      the Tax Practitioners Board registers its terminations as
                 notifiable instruments, so those carry roughly 5,400 name
                 mentions with agent registration numbers and the provision
                 breached

The markdown and JSONL carry no image bytes at all (extract.py never emitted
them), so nothing needs stripping there. The 12 titles are dropped whole rather
than redacted: once the name tables go, the only rows left are the Board
secretary's signature block and a figure placeholder, so a partial redaction
would preserve nothing and risk missing a name.

Everything dropped is listed in REMOVED.md with its Register link, so the
omission is visible and reversible from the primary source.
"""
import collections, json, os, re, shutil, uuid

from corpus_paths import (child, corpus_root, is_reparse_point, register_id,
                          reject_symlinks)
from dist_verify import (_expected_title_files,
                         _title_tree as _verifier_title_tree,
                         verify_distribution)
from pii_patterns import (load_contact_allowlist,
                          privacy_findings_in_file)

ROOT = corpus_root(__file__)
DIST = child(ROOT, "dist")
HERE = os.path.dirname(os.path.abspath(__file__))
CONTACT_ALLOWLIST = os.path.join(HERE, "pii_contact_allowlist.json")


def _validated_distribution_target(path):
    """Return an absolute, non-link directory target with a real parent."""
    target = os.path.abspath(os.fspath(path))
    parent, name = os.path.split(target)
    if not name or target == parent:
        raise ValueError("distribution target must be a named child directory")
    if not os.path.isdir(parent):
        raise ValueError("distribution target parent does not exist")
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    if os.path.lexists(target):
        if (os.path.islink(target) or is_junction(target)
                or is_reparse_point(target)):
            raise ValueError(
                "distribution target must not be a link or reparse point")
        if not os.path.isdir(target):
            raise ValueError("distribution target must be a directory")
    return target


def _validated_managed_sibling(target, candidate, kind):
    """Validate one private stage/backup path beside ``target``.

    These are the only computed paths the publisher may remove. Rechecking the
    parent and a narrow generated-name prefix immediately before each use keeps
    cleanup from expanding beyond the intended distribution directory.
    """
    target = _validated_distribution_target(target)
    if kind not in {"stage", "backup"}:
        raise ValueError("unknown managed distribution path kind")
    candidate = os.path.abspath(os.fspath(candidate))
    parent, target_name = os.path.split(target)
    candidate_parent, candidate_name = os.path.split(candidate)
    expected_prefix = ".%s.%s-" % (target_name, kind)
    suffix = (candidate_name[len(expected_prefix):]
              if candidate_name.startswith(expected_prefix) else "")
    if (os.path.normcase(candidate_parent) != os.path.normcase(parent)
            or not re.fullmatch(r"[0-9a-f]{32}", suffix)):
        raise ValueError("managed distribution path is not a validated sibling")
    if (os.path.normcase(os.path.realpath(candidate_parent)) !=
            os.path.normcase(os.path.realpath(parent))):
        raise ValueError("managed distribution path resolves outside its parent")
    return candidate


def _new_managed_sibling(target, kind):
    """Return a unique, absent stage or backup path beside ``target``."""
    target = _validated_distribution_target(target)
    parent, target_name = os.path.split(target)
    for _attempt in range(100):
        candidate = os.path.join(
            parent, ".%s.%s-%s" % (target_name, kind, uuid.uuid4().hex))
        candidate = _validated_managed_sibling(target, candidate, kind)
        if not os.path.lexists(candidate):
            return candidate
    raise RuntimeError("could not allocate a unique distribution %s path" % kind)


def _remove_managed_tree(target, candidate, kind):
    """Remove only a validated stage/backup directory, never a link or file."""
    candidate = _validated_managed_sibling(target, candidate, kind)
    if not os.path.lexists(candidate):
        return
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    if (os.path.islink(candidate) or is_junction(candidate)
            or is_reparse_point(candidate)
            or not os.path.isdir(candidate)):
        raise ValueError("refusing to remove a non-directory managed path")
    reject_symlinks(candidate)
    shutil.rmtree(candidate)


def _promote_distribution(staging, target):
    """Promote a validated stage without replacing a non-empty directory.

    Windows cannot atomically replace an existing non-empty directory. Move
    the old tree aside first, move the complete stage into place, and restore
    the old tree if that second rename fails.
    """
    target = _validated_distribution_target(target)
    staging = _validated_managed_sibling(target, staging, "stage")
    is_junction = getattr(os.path, "isjunction", lambda _path: False)
    if (not os.path.isdir(staging) or os.path.islink(staging)
            or is_junction(staging) or is_reparse_point(staging)):
        raise ValueError("distribution stage must be a real directory")

    backup = None
    if os.path.lexists(target):
        backup = _new_managed_sibling(target, "backup")
        os.rename(target, backup)

    try:
        # The destination is absent here, so this never relies on platform-
        # specific replacement behaviour for non-empty directories.
        os.rename(staging, target)
    except BaseException:
        if backup is not None:
            if os.path.lexists(target):
                raise RuntimeError(
                    "distribution promotion failed after creating its target; "
                    "automatic rollback is unsafe"
                )
            backup = _validated_managed_sibling(target, backup, "backup")
            if (not os.path.isdir(backup) or os.path.islink(backup)
                    or is_junction(backup) or is_reparse_point(backup)):
                raise RuntimeError("distribution backup is unsafe to restore")
            os.rename(backup, target)
            backup = None
        raise

    if backup is not None:
        _remove_managed_tree(target, backup, "backup")


def _title_tree(directory):
    """Return contained regular files and nested directories in a safe tree.

    One definition, in dist_verify.py: this file carried a copy that had
    already drifted on the exception type. The verifier reports a non-regular
    file as a named FAIL and raises ValueError; the preflight here has no
    verdict to report into, so the same condition must abort the build as the
    RuntimeError the other publication gates raise. reject_symlinks runs
    first so a link keeps refusing with the ValueError its callers pin.
    """
    reject_symlinks(directory)
    try:
        return _verifier_title_tree(directory)
    except ValueError as error:
        raise RuntimeError(str(error)) from error


def _title_privacy_findings(directory, rid, title, approved):
    """Return safe privacy findings from the exact declared title outputs."""
    private_files, matches = 0, set()
    try:
        files, nested_directories = _title_tree(directory)
        relative_files = {os.path.relpath(path, directory) for path in files}
        expected_files, metadata_ok = _expected_title_files(rid, title)
        if (nested_directories or relative_files != expected_files
                or not metadata_ok):
            raise RuntimeError(
                "listed title %s has an unexpected file inventory" % rid)
        for path in files:
            has_private_pair, unexpected = privacy_findings_in_file(
                path, rid, approved)
            private_files += int(has_private_pair)
            matches.update(unexpected)
    except (OSError, UnicodeError) as error:
        raise RuntimeError(
            "listed title %s contains an unreadable or non-text file" % rid
        ) from error
    return private_files, matches


def reject_unapproved_contacts(titles, approved):
    """Fail before replacing ``dist/`` on any unapproved privacy finding.

    Diagnostics include only the public Register id and a truncated digest.
    The matched identifier never reaches logs or the checked-in policy file.
    """
    total, examples, private_titles = 0, [], []
    for title in titles:
        rid = register_id(title["register_id"])
        directory = child(ROOT, "markdown", rid)
        if not os.path.isdir(directory):
            raise RuntimeError("listed title %s has no markdown directory" % rid)
        sections = child(directory, "sections.jsonl")
        if not os.path.isfile(sections):
            raise RuntimeError("listed title %s has no sections.jsonl" % rid)
        private_files, unexpected = _title_privacy_findings(
            directory, rid, title, approved)
        if private_files:
            private_titles.append(rid)
        for kind, digest, _ in sorted(unexpected):
            total += 1
            if len(examples) < 8:
                examples.append("%s:%s:%s" % (rid, kind, digest[:16]))
    if private_titles:
        raise RuntimeError(
            "private-person registration pairs in redistributed title files: "
            "%d (%s)" % (len(private_titles), ", ".join(private_titles[:8])))
    if total:
        raise RuntimeError(
            "unapproved contact identifiers: %d (%s)" %
            (total, ", ".join(examples))
        )


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


TABLE_BLOCK_LEAD = "**Table-shaped instruments are split on their tables.**"


def replace_readme_table_block_paragraph(text, count):
    """Rewrite the table_block paragraph finalize.py writes, for the subset.

    Every title dist.py removes is a table_block title, so the full corpus's
    count is wrong here and so is its worked example: it names the Tax
    Practitioners Board instruments that were just removed.

    Located by find() rather than a pattern, for the same reason as the sibling
    above: a regex ending `.*?\\n\\n` backtracks polynomially on a README that
    repeats the lead, and py/polynomial-redos is a rule this repo already
    fixed once in rates.py.
    """
    start = text.find(TABLE_BLOCK_LEAD)
    if start < 0:
        return text
    end = text.find("\n\n", start)
    end = len(text) if end < 0 else end + 2
    replacement = (
        "%s %d titles carry `granularity: table_block`: no headings anywhere, "
        "just a run of tables. Each table becomes a row, carrying the prose that "
        "preceded it, such as the sentence naming the enabling provision or the "
        "basin name. The Tax Practitioners Board instruments chunked this way "
        "are not in this distribution; see REMOVED.md.\n\n"
        % (TABLE_BLOCK_LEAD, count)
    )
    return text[:start] + replacement + text[end:]


def replace_readme_collection_counts(text, acts, instruments):
    """Rewrite every ``N Acts and N legislative and notifiable`` summary.

    README.md is source-controlled but still treated as input by the build.
    Scanning decimal runs directly keeps the replacement linear for malformed,
    arbitrarily long digit strings while preserving the former whole-phrase
    replacement behaviour.
    """
    marker = " Acts and "
    suffix = " legislative and notifiable"
    replacement = "%d Acts and %d legislative and notifiable" % (acts, instruments)
    parts, cursor = [], 0
    while True:
        marker_at = text.find(marker, cursor)
        if marker_at < 0:
            parts.append(text[cursor:])
            return "".join(parts)

        number_start = marker_at
        while number_start > cursor and text[number_start - 1].isdecimal():
            number_start -= 1
        second_start = marker_at + len(marker)
        second_end = second_start
        while second_end < len(text) and text[second_end].isdecimal():
            second_end += 1
        suffix_end = second_end + len(suffix)

        if (number_start < marker_at and second_start < second_end and
                text.startswith(suffix, second_end)):
            parts.append(text[cursor:number_start])
            parts.append(replacement)
            cursor = suffix_end
        else:
            # Retain the first marker character and move forward so every
            # candidate is examined once, even when malformed.
            parts.append(text[cursor:marker_at + 1])
            cursor = marker_at + 1


def _build_distribution(staging):
    approved_contacts = load_contact_allowlist(CONTACT_ALLOWLIST)
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

    # This gate runs against the source before any output is copied. A newly
    # introduced email, phone number or TFN therefore cannot rely on a
    # previously committed scan result.
    reject_unapproved_contacts(titles, approved_contacts)

    os.makedirs(child(staging, "markdown"))
    os.makedirs(child(staging, "rates"))

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
        copied = child(staging, "markdown", rid)
        # Preserve a link introduced after the source preflight as a link so
        # the copied-tree gate rejects it; following it here could materialise
        # bytes from outside the declared title directory.
        shutil.copytree(d, copied, symlinks=True)
        private_files, unexpected = _title_privacy_findings(
            copied, rid, title, approved_contacts)
        if private_files:
            raise RuntimeError(
                "copied title contains private-person registration details: %s"
                % rid)
        if unexpected:
            kind, digest, _ = sorted(unexpected)[0]
            raise RuntimeError(
                "copied title contains an unapproved contact identifier: "
                "%s:%s:%s" % (rid, kind, digest[:16])
            )
        copied_sections = child(staging, "markdown", rid, "sections.jsonl")
        with open(copied_sections, encoding="utf-8") as f:
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
    with open(child(staging, "rates", "rates.jsonl"), "w", encoding="utf-8") as f:
        for record in rate_records:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
    with open(child(staging, "rates", "RATES.md"), "w", encoding="utf-8") as f:
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
    with open(child(staging, "sources.json"), "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)

    shutil.copy(child(ROOT, "LICENCE-NOTICE.md"),
                child(staging, "LICENCE-NOTICE.md"))

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
    # file describing 934 and 21,596. The summary line has to be rebuilt, not
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
    with open(child(staging, "INDEX.md"), "w", encoding="utf-8") as f:
        f.write(txt)

    with open(child(ROOT, "README.md"), encoding="utf-8") as f:
        rd = f.read()
    # The README's headline wraps across three lines, so a single-line replace
    # misses it and leaves the file claiming 946 titles. Match the numbers
    # themselves, whitespace-tolerant.
    rd = re.sub(r"%s in-force principal titles" % len(src["titles"]),
                "%d in-force principal titles" % len(titles), rd)
    rd = replace_readme_collection_counts(
        rd, stats["Act"], len(titles) - stats["Act"])
    rd = re.sub(r"instruments\.\s+[\d,]+ retrieval rows, [\d,]+ words\.",
                "instruments. %s retrieval rows, %s words."
                % (f"{kept_rows:,}", f"{kept_words:,}"), rd)
    # Two full-corpus claims are false for this subset and cannot be fixed by
    # patching counts: the layout bullet promising epub/<register_id>.epub (no
    # EPUBs ship) and the limits paragraph describing the titles that name
    # people (every one of them is removed from dist).
    rd = re.sub(r"^- `epub/<register_id>\.epub`[^\n]*\n", "", rd,
                count=1, flags=re.M)
    # The lead is matched loosely on purpose: the new finalize.py writes a
    # numeric count ("**12 titles name people.**") but the README built by
    # the previous generation spells it out ("**Eleven titles name
    # people.**"), and the rewrite must strip both.
    rd = re.sub(
        r"\*\*[^*\n]{1,24} titles name people\.\*\*.*?\n\n",
        "**Titles naming people are not in this distribution.** The Tax "
        "Practitioners Board registers its terminations and suspensions as "
        "notifiable instruments; the %d titles carrying those name tables were "
        "removed before publication. See REMOVED.md for the list and their "
        "Register links.\n\n" % len(drop),
        rd, count=1, flags=re.S)
    # Every title removed from dist is a table_block title, so the full
    # corpus's count is wrong here and so is its worked example. The right
    # figure is already computed for sources.json.
    rd = replace_readme_table_block_paragraph(
        rd, counts["titles_table_block_chunk"])
    rd = ("> **This is the redistributable subset.** The EPUBs and %d titles that "
          "name private individuals are not included. See REMOVED.md for what was "
          "dropped and why, and run the pipeline yourself for the full corpus.\n\n"
          % len(drop) + rd)
    with open(child(staging, "README.md"), "w", encoding="utf-8") as f:
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
        "the second pass approved are %d unique organisational identifiers. "
        "Each is bound to a reviewed Register title in the hashed allowlist; "
        "a new or moved contact fails publication." % (
            f"{len(src['titles']):,}", len(drop),
            len({(kind, digest) for kind, digest, _rid in approved_contacts})),
    ]
    with open(child(staging, "REMOVED.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")

    size = sum(os.path.getsize(os.path.join(r, f))
               for r, _, fs in os.walk(staging) for f in fs)
    print("titles %d (dropped %d) | rows %s | words %s | rates %d (dropped %d) | %.1f MB"
          % (len(titles), len(drop), f"{kept_rows:,}", f"{kept_words:,}",
             len(rate_records), rdropped, size / 1e6))


def main():
    """Build, validate and publish ``dist/`` without exposing partial output."""
    target = _validated_distribution_target(DIST)
    staging = _new_managed_sibling(target, "stage")
    os.mkdir(staging)
    try:
        _build_distribution(staging)
        failures = verify_distribution(staging, CONTACT_ALLOWLIST)
        if failures:
            raise RuntimeError(
                "staged distribution failed validation: %s" % ", ".join(failures))
        _promote_distribution(staging, target)
    finally:
        # A successful promotion consumes the staging path. Every pre-publish
        # failure leaves it beside dist, where this exact-prefix guard permits
        # cleanup without broadening the deletion target.
        _remove_managed_tree(target, staging, "stage")


if __name__ == "__main__":
    main()
