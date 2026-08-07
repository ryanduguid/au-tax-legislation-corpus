"""Stage 3: download the current EPUB for each resolved Act.

Honours the Register's robots.txt Crawl-delay of 10 seconds.

The download endpoint answers in one of two shapes and does not tell you which
in advance:
  * raw EPUB bytes (content-type application/epub+zip), or
  * a JSON envelope carrying the file base64-encoded in a "bytes" field, plus
    useful metadata (registerId, fileName, sizeInBytes, isAuthorised).
Large raw transfers also drop mid-stream, so those get a resumed retry.
"""
import base64, json, os, subprocess, time, zipfile

from corpus_paths import child, corpus_root, register_id

SCRATCH = os.path.dirname(os.path.abspath(__file__))
ROOT = corpus_root(__file__)
EPUB_DIR = child(ROOT, "epub")
CRAWL_DELAY = 10


def valid_zip(path):
    try:
        return zipfile.ZipFile(path).testzip() is None
    except Exception:
        return False


def sniff(path):
    try:
        with open(path, "rb") as f:
            return f.read(1)
    except Exception:
        return b""


def decode_envelope(path):
    """If the file is the JSON envelope, replace it with the decoded EPUB.

    Returns the envelope metadata dict, or None if it was not an envelope.
    """
    try:
        with open(path, encoding="utf-8") as f:
            d = json.load(f)
    except Exception:
        return None
    if not isinstance(d, dict):
        return None
    b64 = d.get("bytes")
    if not b64:
        return {"_no_bytes": True, **{k: d.get(k) for k in
                                      ("registerId", "extension", "mimeType", "sizeInBytes")}}
    with open(path, "wb") as f:
        f.write(base64.b64decode(b64))
    return {k: d.get(k) for k in
            ("registerId", "fileName", "sizeInBytes", "isAuthorised",
             "compilationNumber", "mimeType", "extension")}


def fetch(url, dst, tries=3):
    """Returns (ok, http_code, content_type, bytes, envelope_meta)."""
    code = ctype = ""
    meta = None
    for attempt in range(tries):
        args = ["curl.exe", "-sL", "--max-time", "600", "--retry", "2",
                "--retry-all-errors", "--retry-delay", "5",
                "-w", "%{http_code}|%{content_type}", "-o", dst]
        # Resume only makes sense for a truncated raw transfer.
        if attempt > 0 and os.path.exists(dst) and sniff(dst) == b"P":
            args[1:1] = ["-C", "-"]
        p = subprocess.run(args + [url], capture_output=True, text=True)
        out = (p.stdout or "").strip().split("|")
        code = out[0] if out else "?"
        ctype = out[1] if len(out) > 1 else "?"

        if os.path.exists(dst):
            if sniff(dst) == b"{":
                m = decode_envelope(dst)
                if m and not m.get("_no_bytes"):
                    meta = m
                elif m and m.get("_no_bytes"):
                    return False, code, "json/no-bytes", 0, m
            if valid_zip(dst):
                return True, code, ctype, os.path.getsize(dst), meta
        time.sleep(5)
    return False, code, ctype, (os.path.getsize(dst) if os.path.exists(dst) else 0), meta


def main():
    os.makedirs(EPUB_DIR, exist_ok=True)
    with open(os.path.join(SCRATCH, "acts_resolved.json"), encoding="utf-8") as f:
        acts = json.load(f)

    log = open(os.path.join(SCRATCH, "download_log.txt"), "a", encoding="utf-8")
    manifest, ok_n, fail_n, total_bytes = [], 0, 0, 0

    for i, a in enumerate(acts, 1):
        rid, d = register_id(a["id"]), a["versionStart"]
        dst = child(EPUB_DIR, "%s.epub" % rid)
        url = "https://www.legislation.gov.au/%s/%s/%s/text/original/epub" % (rid, d, d)

        # A cached file is only valid for the version it was fetched under.
        # Without this check a re-run stamps the newly resolved compilation
        # number onto stale bytes.
        side = child(EPUB_DIR, "%s.epub.meta.json" % rid)
        if os.path.exists(dst) and valid_zip(dst) and os.path.exists(side):
            try:
                with open(side, encoding="utf-8") as f:
                    prev = json.load(f)
            except Exception:
                prev = {}
            if prev.get("versionStart") == d:
                sz = os.path.getsize(dst)
                manifest.append(dict(a, epub=os.path.basename(dst), bytes=sz,
                                     status="cached", sourceUrl=url,
                                     compilationRegisterId=prev.get("compilationRegisterId")))
                ok_n += 1
                total_bytes += sz
                continue
        for p in (dst, side):
            if os.path.exists(p):
                os.remove(p)

        ok, code, ctype, sz, meta = fetch(url, dst)
        rec = dict(a, sourceUrl=url)
        if meta:
            rec["compilationRegisterId"] = meta.get("registerId")
            rec["isAuthorised"] = meta.get("isAuthorised")
        if ok:
            ok_n += 1
            total_bytes += sz
            rec.update(epub=os.path.basename(dst), bytes=sz, status="ok")
            with open(side, "w", encoding="utf-8") as f:
                json.dump({"versionStart": d,
                           "compilationNumber": a.get("compilationNumber"),
                           "compilationRegisterId": rec.get("compilationRegisterId"),
                           "bytes": sz}, f)
        else:
            fail_n += 1
            if os.path.exists(dst):
                os.remove(dst)
            rec.update(epub=None, bytes=0, status="no_epub",
                       httpCode=code, contentType=ctype)
        manifest.append(rec)

        line = "%3d/%3d %-12s %-8s %9d  %s" % (
            i, len(acts), rid, "OK" if ok else "NO_EPUB", sz, a["name"][:58])
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()
        time.sleep(CRAWL_DELAY)

    with open(os.path.join(SCRATCH, "manifest_raw.json"), "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=1)

    s = "\nDONE ok=%d no_epub=%d total=%.1f MB" % (ok_n, fail_n, total_bytes / 1e6)
    print(s)
    log.write(s + "\n")
    log.close()


if __name__ == "__main__":
    main()
