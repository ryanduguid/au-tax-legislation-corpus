"""Verify the distribution before it goes anywhere.

Checks the claims the distribution makes about itself rather than trusting the
build script that made it: no personal names, no image bytes, every row parses,
every title listed in sources.json is actually present, and nothing links to a
title that was removed.
"""
import collections, json, os, sys

from corpus_paths import child, corpus_root, register_id, reject_symlinks
from pii_patterns import (has_private_person_registration_pair,
                          load_contact_allowlist,
                          unapproved_contact_fingerprints_in_file)

DIST = child(corpus_root(__file__), "dist")
HERE = os.path.dirname(os.path.abspath(__file__))
CONTACT_ALLOWLIST = os.path.join(HERE, "pii_contact_allowlist.json")


def _entry_kind(entry, is_junction):
    """Classify an untrusted directory entry without following links."""
    try:
        if entry.is_symlink():
            return "symlink"
        if is_junction(entry.path):
            return "junction"
        if entry.is_dir(follow_symlinks=False):
            return "directory"
        if entry.is_file(follow_symlinks=False):
            return "file"
    except OSError:
        return "unreadable"
    return "other"


def _title_files(directory):
    """Yield every contained regular file in a link-free title tree."""
    reject_symlinks(directory)
    for current, directories, files in os.walk(directory, followlinks=False):
        directories.sort()
        for name in sorted(files):
            candidate = os.path.join(current, name)
            relative = os.path.relpath(candidate, directory)
            contained = child(directory, relative)
            if not os.path.isfile(contained):
                raise ValueError("title tree contains a non-regular file")
            yield contained


def verify_distribution(distribution=None, contact_allowlist=None):
    """Validate one distribution tree and return the failed check labels.

    The command-line wrapper converts this result to an exit status. Keeping
    the validation itself free of ``sys.exit`` lets ``dist.py`` validate a
    staging tree before it becomes the published ``dist/`` directory.
    """
    dist_root = os.fspath(distribution) if distribution is not None else DIST
    policy = (os.fspath(contact_allowlist) if contact_allowlist is not None
              else CONTACT_ALLOWLIST)
    fails = []

    def check(label, ok, detail=""):
        print("  %-52s %s%s" % (label, "PASS" if ok else "FAIL",
                                ("  " + detail) if detail else ""))
        if not ok:
            fails.append(label)

    approved_contacts = load_contact_allowlist(policy)

    with open(child(dist_root, "sources.json"), encoding="utf-8") as f:
        src = json.load(f)
    titles = src["titles"]
    by_id = {register_id(t["register_id"]): t for t in titles}
    listed = set(by_id)
    markdown_root = child(dist_root, "markdown")
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
        kind = _entry_kind(entry, is_junction)
        try:
            rid = register_id(name)
        except ValueError:
            stray.append("%s [%s]" % (name, kind))
            continue
        if kind != "directory":
            stray.append("%s [%s]" % (name, kind))
            continue
        present.add(rid)
    removed = {register_id(e["register_id"]) for e in src.get("excluded_titles", [])}

    check("every listed title has a directory", listed <= present,
          "missing %d" % len(listed - present))
    unexpected = (["%s [directory]" % rid for rid in sorted(present - listed)]
                  + stray)
    check("no directory beyond what is listed", not unexpected,
          "unexpected %d%s" % (
              len(unexpected),
              (": " + ", ".join(unexpected[:5])) if unexpected else ""))
    check("no removed title present", not (removed & present))

    rows = bad = section_rows = rid_mismatch = 0
    hot = collections.Counter()
    contact_hot = collections.Counter()
    kind_counts = collections.Counter()
    rows_by_coll, words_by_coll = collections.Counter(), collections.Counter()
    unsafe_row_paths = set()
    unreadable_title_files = set()
    for rid in sorted(present):
        title_directory = os.path.join(markdown_root, rid)
        try:
            title_files = list(_title_files(title_directory))
            p = child(markdown_root, rid, "sections.jsonl")
        except (OSError, ValueError):
            unsafe_row_paths.add(rid)
            continue
        for title_file in title_files:
            try:
                unexpected_contacts = unapproved_contact_fingerprints_in_file(
                    title_file, rid, approved_contacts)
            except (OSError, UnicodeError):
                unreadable_title_files.add(rid)
                continue
            for kind, digest, _ in unexpected_contacts:
                contact_hot[(rid, kind, digest[:16])] += 1
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
    check("title files stay inside real link-free directories", not unsafe_row_paths,
          ", ".join(sorted(unsafe_row_paths)[:5]))
    check("all distributed title files are UTF-8 text", not unreadable_title_files,
          ", ".join(sorted(unreadable_title_files)[:5]))
    check("no row names private individuals", not hot, str(dict(hot))[:60])
    check("no unapproved contact identifiers", not contact_hot,
          str(dict(contact_hot))[:120])

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
                               for r, _, fs in os.walk(dist_root) for f in fs)
    check("no image files", not any(e in exts for e in
                                    (".png", ".jpg", ".jpeg", ".gif", ".svg", ".epub")),
          str(dict(exts)))

    with open(child(dist_root, "INDEX.md"), encoding="utf-8") as f:
        idx = f.read()
    check("INDEX links no removed title", not any(r in idx for r in removed))
    check("INDEX headline matches actual rows", f"{rows:,}" in idx)
    with open(child(dist_root, "README.md"), encoding="utf-8") as f:
        rd = f.read()
    check("README states the real title count", "%d in-force principal" % len(titles) in rd)
    check("README does not promise EPUB files", "epub/<register_id>.epub" not in rd)
    check("README does not claim rows naming people",
          "titles name people" not in rd and "disciplined agents" not in rd)
    with open(child(dist_root, "REMOVED.md"), encoding="utf-8") as f:
        removed_md = f.read()
    check("REMOVED.md lists every exclusion", all(r in removed_md for r in removed))

    rt, rates_bad = [], 0
    with open(child(dist_root, "rates", "rates.jsonl"), encoding="utf-8") as f:
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
    with open(child(dist_root, "rates", "RATES.md"), encoding="utf-8") as f:
        rates_md = f.read()
    rate_headline = "%s entries across %s titles." % (
        f"{len(rt):,}", f"{len({r.get('register_id') for r in rt if r.get('register_id')}):,}")
    check("RATES headline matches filtered JSONL", rate_headline in rates_md)
    check("RATES contains no removed-title name",
          not any(e.get("name") and e["name"] in rates_md
                  for e in src.get("excluded_titles", [])))

    size = sum(os.path.getsize(os.path.join(r, f))
               for r, _, fs in os.walk(dist_root) for f in fs)
    print("\n%s titles | %s rows | %.1f MB | %d removed"
          % (f"{len(titles):,}", f"{rows:,}", size / 1e6, len(removed)))
    print("RESULT:", "all checks passed" if not fails else "FAILED: %s" % fails)
    return fails


def main():
    sys.exit(1 if verify_distribution() else 0)


if __name__ == "__main__":
    main()
