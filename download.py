"""Stage 3: download the current EPUB for each resolved Act.

Honours the Register's robots.txt Crawl-delay of 10 seconds.

The download endpoint answers in one of two shapes and does not tell you which
in advance:
  * raw EPUB bytes (content-type application/epub+zip), or
  * a JSON envelope carrying the file base64-encoded in a "bytes" field, plus
    useful metadata (registerId, fileName, sizeInBytes, isAuthorised).
Large raw transfers also drop mid-stream, so those get a resumed retry.
"""
import base64, binascii, json, os, shutil, subprocess, time, zipfile

from corpus_paths import child, corpus_root, register_id

SCRATCH = os.path.dirname(os.path.abspath(__file__))
ROOT = corpus_root(__file__)
EPUB_DIR = child(ROOT, "epub")
CRAWL_DELAY = 10


class DownloadError(RuntimeError):
    """The Register did not return a usable document."""


def diagnostic(value, limit=80):
    """Collapse and bound untrusted response metadata for logs."""
    text = "".join(c if c.isprintable() else " " for c in str(value))
    return " ".join(text.split())[:limit] or "?"


def write_json_atomic(path, value, **kwargs):
    """Write JSON beside its target, then atomically replace the target."""
    tmp = path + ".tmp"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(value, f, **kwargs)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise


def snapshot_pair(dst, side):
    """Back up one EPUB/sidecar pair for whole-run rollback."""
    snapshot = []
    for path in (dst, side):
        backup = path + ".rollback"
        if os.path.exists(backup):
            raise RuntimeError("unfinished rollback file: %s" % backup)
        snapshot.append((path, backup, os.path.exists(path)))
    try:
        for path, backup, existed in snapshot:
            if existed:
                shutil.copy2(path, backup)
    except BaseException:
        for _path, backup, _existed in snapshot:
            if os.path.exists(backup):
                os.remove(backup)
        raise
    return snapshot


def rollback_snapshots(snapshots):
    """Restore every pair changed since the prior manifest was read."""
    for snapshot in reversed(snapshots):
        for path, backup, existed in reversed(snapshot):
            if existed:
                if not os.path.exists(backup):
                    raise RuntimeError("missing rollback file: %s" % backup)
                os.replace(backup, path)
            elif os.path.exists(path):
                os.remove(path)
        for path, backup, _existed in snapshot:
            for transient in (backup, path + ".part", path + ".tmp"):
                if os.path.exists(transient):
                    os.remove(transient)


def discard_snapshots(snapshots):
    """Remove rollback copies after the new manifest commits."""
    leftovers = []
    for snapshot in snapshots:
        for _path, backup, _existed in snapshot:
            if os.path.exists(backup):
                try:
                    os.remove(backup)
                except OSError:
                    leftovers.append(backup)
    return leftovers


def valid_zip(path):
    try:
        with zipfile.ZipFile(path) as archive:
            return archive.testzip() is None
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
        return {"_invalid": True}
    if not isinstance(d, dict):
        return {"_invalid": True}
    b64 = d.get("bytes")
    if not isinstance(b64, str) or not b64:
        return {"_invalid": True}
    try:
        decoded = base64.b64decode(b64, validate=True)
    except (ValueError, TypeError, binascii.Error):
        return {"_invalid": True}
    with open(path, "wb") as f:
        f.write(decoded)
    return {k: d.get(k) for k in
            ("registerId", "fileName", "sizeInBytes", "isAuthorised",
             "compilationNumber", "mimeType", "extension")}


def fetch(url, dst, tries=3):
    """Return download metadata, or raise if the response is unusable."""
    part = dst + ".part"
    if os.path.exists(part):
        os.remove(part)
    code = ctype = ""
    meta = None
    problem = "no response"
    for attempt in range(tries):
        args = ["curl", "-sL", "--max-time", "600", "--retry", "2",
                "--retry-all-errors", "--retry-delay", "5",
                "-w", "%{http_code}|%{content_type}", "-o", part]
        # Resume only makes sense for a truncated raw transfer.
        if attempt > 0 and os.path.exists(part) and sniff(part) == b"P":
            args[1:1] = ["-C", "-"]
        p = subprocess.run(args + [url], capture_output=True, text=True)
        out = (p.stdout or "").strip().split("|")
        code = diagnostic(out[0] if out else "?", 10)
        ctype = diagnostic(out[1] if len(out) > 1 else "?")

        if p.returncode:
            problem = "curl exit %s" % p.returncode
        elif not code.isdigit() or not 200 <= int(code) < 300:
            problem = "HTTP %s" % code
        elif os.path.exists(part):
            invalid_envelope = False
            if sniff(part) == b"{":
                m = decode_envelope(part)
                if m and m.get("_invalid"):
                    invalid_envelope = True
                elif m:
                    meta = m
            if not invalid_envelope and valid_zip(part):
                size = os.path.getsize(part)
                os.replace(part, dst)
                return True, code, ctype, size, meta
            problem = ("invalid JSON envelope"
                       if invalid_envelope else "invalid EPUB response")
        else:
            problem = "missing response body"
        if attempt + 1 < tries:
            time.sleep(5)
    size = os.path.getsize(part) if os.path.exists(part) else 0
    if os.path.exists(part):
        os.remove(part)
    raise DownloadError("%s after %d attempt%s (content-type %s, %d bytes)" % (
        problem, tries, "" if tries == 1 else "s", ctype, size))


def main():
    os.makedirs(EPUB_DIR, exist_ok=True)
    with open(os.path.join(SCRATCH, "acts_resolved.json"), encoding="utf-8") as f:
        acts = json.load(f)

    with open(os.path.join(SCRATCH, "download_log.txt"), "a", encoding="utf-8") as log:
        manifest, ok_n, fail_n, total_bytes = [], 0, 0, 0
        snapshots = []
        try:
            for i, a in enumerate(acts, 1):
                rid, d = register_id(a["id"]), a["versionStart"]
                dst = child(EPUB_DIR, "%s.epub" % rid)
                url = "https://www.legislation.gov.au/%s/%s/%s/text/original/epub" % (rid, d, d)
                if "compilationRegisterId" not in a:
                    raise ValueError("%s: missing compilationRegisterId" % rid)
                no_document = a["compilationRegisterId"] is None

                # A cached file is only valid for the version it was fetched under.
                # Without this check a re-run stamps the newly resolved compilation
                # number onto stale bytes.
                side = child(EPUB_DIR, "%s.epub.meta.json" % rid)
                if (not no_document and os.path.exists(dst) and valid_zip(dst)
                        and os.path.exists(side)):
                    try:
                        with open(side, encoding="utf-8") as f:
                            prev = json.load(f)
                    except Exception:
                        prev = {}
                    if prev.get("versionStart") == d:
                        sz = os.path.getsize(dst)
                        manifest.append(dict(
                            a, epub=os.path.basename(dst), bytes=sz,
                            status="cached", sourceUrl=url,
                            compilationRegisterId=prev.get("compilationRegisterId")))
                        ok_n += 1
                        total_bytes += sz
                        continue
                if no_document:
                    # Stale prior-version files are deliberately left unreferenced:
                    # deleting them here would break the previous manifest if this
                    # run failed before publishing its replacement.
                    for p in (dst + ".part", side + ".tmp"):
                        if os.path.exists(p):
                            os.remove(p)

                rec = dict(a, sourceUrl=url)
                if no_document:
                    fail_n += 1
                    sz = 0
                    rec.update(epub=None, bytes=0, status="no_epub",
                               reason="current_version_has_no_document")
                    label = "NO_EPUB"
                else:
                    snapshots.append(snapshot_pair(dst, side))
                    try:
                        _ok, _code, _ctype, sz, meta = fetch(url, dst)
                    except DownloadError as error:
                        line = "%3d/%3d %-12s ERROR    %s" % (
                            i, len(acts), rid, error)
                        print(line, flush=True)
                        log.write(line + "\n")
                        log.flush()
                        raise
                    if meta:
                        rec["compilationRegisterId"] = meta.get("registerId")
                        rec["isAuthorised"] = meta.get("isAuthorised")
                    ok_n += 1
                    total_bytes += sz
                    rec.update(epub=os.path.basename(dst), bytes=sz, status="ok")
                    write_json_atomic(side, {
                        "versionStart": d,
                        "compilationNumber": a.get("compilationNumber"),
                        "compilationRegisterId": rec.get("compilationRegisterId"),
                        "bytes": sz,
                    })
                    label = "OK"
                manifest.append(rec)

                line = "%3d/%3d %-12s %-8s %9d  %s" % (
                    i, len(acts), rid, label, sz, a["name"][:58])
                print(line, flush=True)
                log.write(line + "\n")
                log.flush()
                time.sleep(CRAWL_DELAY)

            # manifest_raw.json is the sole record of this crawl: every sourceUrl,
            # compilationRegisterId and isAuthorised value. Replace it only after
            # all downloads and sidecars are ready; retained snapshots restore the
            # entire prior file graph if this final write fails.
            target = os.path.join(SCRATCH, "manifest_raw.json")
            write_json_atomic(target, manifest, indent=1)
        except BaseException:
            try:
                rollback_snapshots(snapshots)
            except BaseException as rollback_error:
                raise RuntimeError(
                    "download failed and rollback was incomplete") from rollback_error
            log.write("ROLLBACK restored %d changed title(s)\n" % len(snapshots))
            log.flush()
            raise
        else:
            leftovers = discard_snapshots(snapshots)
            if leftovers:
                log.write("WARNING retained %d rollback file(s) after commit\n"
                          % len(leftovers))
            s = "\nDONE ok=%d no_epub=%d total=%.1f MB" % (
                ok_n, fail_n, total_bytes / 1e6)
            print(s)
            log.write(s + "\n")


if __name__ == "__main__":
    main()
