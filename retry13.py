"""Recover the titles whose current version carries no published document.

The Register can record that a version commenced without yet publishing a
compilation for it. `versions?$filter=isCurrent eq true` still returns that
version, but every document path built from its date answers 404, because the
document does not exist. Thirteen titles landed here.

The recovery is to fall back to the most recent version that does have a
`registerId`, and to say so in the manifest: `version_is_current` goes false and
`current_version_start` records the commencement the text does not yet reflect.
Silently substituting an older compilation would misreport the corpus as
current.
"""
import json, os, time

import download as dl
from corpus_paths import child, corpus_root, register_id

SCRATCH = os.path.dirname(os.path.abspath(__file__))
ROOT = corpus_root(__file__)
EPUB_DIR = child(ROOT, "epub")


def main():
    with open(os.path.join(SCRATCH, "probe13.json"), encoding="utf-8") as source:
        probe = {p["id"]: p for p in json.load(source)}
    with open(os.path.join(SCRATCH, "manifest_raw.json"), encoding="utf-8") as source:
        manifest = json.load(source)
    todo = [a for a in manifest if not a.get("epub") and probe.get(a["id"], {}).get("latest_doc")]
    print("retrying %d titles" % len(todo), flush=True)

    patches = []
    for i, a in enumerate(todo, 1):
        rid = register_id(a["id"])
        v = probe[rid]["latest_doc"]
        d = v["start"][:10]
        dst = child(EPUB_DIR, "%s.epub" % rid)
        url = "https://www.legislation.gov.au/%s/%s/%s/text/original/epub" % (rid, d, d)

        ok, code, ctype, sz, meta = dl.fetch(url, dst)
        rec = {"id": rid, "sourceUrl": url, "versionStart": d,
               "compilationNumber": v.get("compilationNumber"),
               "compilationRegisterId": (meta or {}).get("registerId") or v.get("registerId"),
               "version_is_current": False,
               "current_version_start": (a.get("versionStart") or "")[:10],
               "current_version_has_document": False}
        if meta:
            rec["isAuthorised"] = meta.get("isAuthorised")
        if ok:
            rec.update(epub=os.path.basename(dst), bytes=sz, status="ok_superseded_version")
            with open(child(EPUB_DIR, "%s.epub.meta.json" % rid), "w", encoding="utf-8") as f:
                json.dump({"versionStart": d,
                           "compilationNumber": rec["compilationNumber"],
                           "compilationRegisterId": rec["compilationRegisterId"],
                           "bytes": sz}, f)
        else:
            if os.path.exists(dst):
                os.remove(dst)
            rec.update(epub=None, bytes=0, status="no_epub",
                       httpCode=code, contentType=ctype)
        patches.append(rec)
        print("%2d/%2d %-12s %-22s %9d  %s" % (
            i, len(todo), rid, "OK" if ok else "FAIL " + str(code), sz, a["name"][:50]),
            flush=True)
        time.sleep(dl.CRAWL_DELAY)

    with open(os.path.join(SCRATCH, "retry13_patch.json"), "w", encoding="utf-8") as f:
        json.dump(patches, f, indent=1)
    print("\nrecovered %d / %d" % (sum(1 for p in patches if p["epub"]), len(patches)))

    # Apply the recoveries to manifest_raw.json here, so the documented
    # pipeline needs no manual patching step. retry13_patch.json above stays
    # as the audit record of what this run changed. Only an entry whose id
    # matches a patch record with an epub is rewritten; a failed retry leaves
    # the original no_epub record in place.
    recovered = {p["id"]: p for p in patches if p.get("epub")}
    if recovered:
        merged = 0
        for i, a in enumerate(manifest):
            p = recovered.get(a["id"])
            if p:
                manifest[i] = dict(a, **p)
                merged += 1
        # This is the only write in the pipeline that replaces an existing
        # artefact rather than creating one, and manifest_raw.json is the sole
        # record of a 2h40m crawl: every sourceUrl, compilationRegisterId and
        # isAuthorised value. Truncating it in place means a Ctrl-C, a full disk
        # or an exception mid-dump leaves a half-written file, extract.py fails
        # on json.load, and the whole download stage has to be re-run. Write a
        # sibling temp file and rename it over the target instead; os.replace is
        # atomic on the same filesystem, on Windows as well as POSIX.
        target = os.path.join(SCRATCH, "manifest_raw.json")
        tmp = target + ".tmp"
        try:
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(manifest, f, indent=1)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, target)
        except BaseException:
            if os.path.exists(tmp):
                os.remove(tmp)
            raise
        print("patched %d entr%s in manifest_raw.json"
              % (merged, "y" if merged == 1 else "ies"))


if __name__ == "__main__":
    main()
