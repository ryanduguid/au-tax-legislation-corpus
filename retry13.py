"""Recover the titles whose current version carries no published document.

The Register can record that a version commenced without yet publishing a
compilation for it. `versions?$filter=isCurrent eq true` still returns that
version, but every document path built from its date answers 404, because the
document does not exist. Some titles can be in this state at any given build.

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

    patches, snapshots = [], []
    try:
        for i, a in enumerate(todo, 1):
            rid = register_id(a["id"])
            v = probe[rid]["latest_doc"]
            d = v["start"][:10]
            dst = child(EPUB_DIR, "%s.epub" % rid)
            side = child(EPUB_DIR, "%s.epub.meta.json" % rid)
            url = "https://www.legislation.gov.au/%s/%s/%s/text/original/epub" % (rid, d, d)

            snapshots.append(dl.snapshot_pair(dst, side))
            _ok, _code, _ctype, sz, meta = dl.fetch(url, dst)
            rec = {"id": rid, "sourceUrl": url, "versionStart": d,
                   "compilationNumber": v.get("compilationNumber"),
                   "compilationRegisterId": (meta or {}).get("registerId") or v.get("registerId"),
                   "version_is_current": False,
                   "current_version_start": (a.get("versionStart") or "")[:10],
                   "current_version_has_document": False}
            if meta:
                rec["isAuthorised"] = meta.get("isAuthorised")
            rec.update(epub=os.path.basename(dst), bytes=sz,
                       status="ok_superseded_version")
            dl.write_json_atomic(side, {
                "versionStart": d,
                "compilationNumber": rec["compilationNumber"],
                "compilationRegisterId": rec["compilationRegisterId"],
                "bytes": sz,
            })
            patches.append(rec)
            print("%2d/%2d %-12s %-22s %9d  %s" % (
                i, len(todo), rid, "OK", sz, a["name"][:50]), flush=True)
            time.sleep(dl.CRAWL_DELAY)

        recovered = {p["id"]: p for p in patches}
        merged = 0
        for i, a in enumerate(manifest):
            p = recovered.get(a["id"])
            if p:
                manifest[i] = dict(a, **p)
                merged += 1

        # Publish the audit record and manifest only after every recovery and
        # sidecar is ready. The shared journal restores the entire prior file
        # graph if either final write fails.
        patch_target = os.path.join(SCRATCH, "retry13_patch.json")
        manifest_target = os.path.join(SCRATCH, "manifest_raw.json")
        snapshots.append(dl.snapshot_paths(patch_target, manifest_target))
        dl.write_json_atomic(patch_target, patches, indent=1)
        if recovered:
            dl.write_json_atomic(manifest_target, manifest, indent=1)
    except BaseException:
        try:
            dl.rollback_snapshots(snapshots)
        except BaseException as rollback_error:
            raise RuntimeError(
                "retry failed and rollback was incomplete") from rollback_error
        raise
    else:
        leftovers = dl.discard_snapshots(snapshots)
        if leftovers:
            print("WARNING retained %d rollback file(s) after commit"
                  % len(leftovers))
        print("\nrecovered %d / %d" % (len(patches), len(todo)))
        if recovered:
            print("patched %d entr%s in manifest_raw.json"
                  % (merged, "y" if merged == 1 else "ies"))


if __name__ == "__main__":
    main()
