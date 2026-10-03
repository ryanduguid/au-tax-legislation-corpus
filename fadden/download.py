"""Stage 3: download the current EPUB for each resolved Act.

Honours the Register's robots.txt Crawl-delay of 10 seconds.

The download endpoint answers in one of 2 shapes and does not tell you which
in advance:
  * raw EPUB bytes (content-type application/epub+zip), or
  * a JSON envelope carrying the file base64-encoded in a "bytes" field, plus
    useful metadata (registerId, fileName, sizeInBytes, isAuthorised).
Large raw transfers also drop mid-stream, so those get a resumed retry.
"""
import base64
import binascii
import contextlib
import datetime
import functools
import json
import os
import re
import shutil
import struct
import subprocess
import time
import zipfile
import zlib
from typing import TYPE_CHECKING

if TYPE_CHECKING or __package__:
    from .corpus_paths import child, corpus_root, iso_date, register_id, require_builder_layout
    from .corpus_paths import write_json_atomic as _write_json_atomic
else:
    from corpus_paths import child, corpus_root, iso_date, register_id, require_builder_layout
    from corpus_paths import write_json_atomic as _write_json_atomic


SCRATCH = os.path.dirname(os.path.abspath(__file__))
ROOT = corpus_root(__file__)
EPUB_DIR = child(ROOT, "epub")
CRAWL_DELAY = 10
MAX_EPUB_BYTES = 128 * 1024 * 1024
MAX_DOWNLOAD_BYTES = 192 * 1024 * 1024
MAX_EPUB_MEMBERS = 4096
MAX_EPUB_MEMBER_BYTES = 64 * 1024 * 1024
MAX_EPUB_UNCOMPRESSED_BYTES = 256 * 1024 * 1024
MAX_EPUB_DIRECTORY_BYTES = 8 * 1024 * 1024


class DownloadError(RuntimeError):
    """The Register did not return a usable document."""


def diagnostic(value, limit=80):
    """Collapse and bound untrusted response metadata for logs."""
    text = "".join(c if c.isprintable() else " " for c in str(value))
    return " ".join(text.split())[:limit] or "?"


def write_json_atomic(path, value, **kwargs):
    """Compatibility entry point for the shared mutable-file writer."""
    _write_json_atomic(path, value, **kwargs)


def snapshot_paths(*paths):
    """Back up a group of paths for whole-run rollback."""
    snapshot = []
    for path in paths:
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
    """Restore every path changed since the prior manifest was read.

    A title new to this run keeps its EPUB and sidecar when both were written:
    the EPUB passed validation before it replaced its .part file, and the
    sidecar stamps its version, so the next run's cache check reuses the pair.
    Discarding them cost a first build every fetch before the failing title.
    """
    for snapshot in reversed(snapshots):
        new_pair_written = all(
            not existed and os.path.exists(path) for path, _backup, existed in snapshot)
        for path, backup, existed in reversed(snapshot):
            if existed:
                if not os.path.exists(backup):
                    raise RuntimeError("missing rollback file: %s" % backup)
                os.replace(backup, path)
            elif os.path.exists(path) and not new_pair_written:
                os.remove(path)
        for path, backup, _existed in snapshot:
            for transient in (backup, path + ".part"):
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


def _check_zip_directory(source):
    """Bound the raw directory before ZipFile allocates member objects."""
    source.seek(0, os.SEEK_END)
    size = source.tell()
    source.seek(0)
    if size > MAX_EPUB_BYTES:
        raise DownloadError("EPUB exceeds the compressed input budget")
    # Bound central-directory parsing too: ZipFile allocates member objects
    # before infolist can enforce the member budget.
    tail_start = max(0, size - 65_557)
    source.seek(tail_start)
    tail = source.read(65_557)
    marker = tail.rfind(b"PK\x05\x06")
    if marker < 0 or len(tail) - marker < 22:
        raise DownloadError("EPUB has no complete ZIP directory record")
    (_, disk, directory_disk, disk_members, members, directory_bytes,
     offset, comment_bytes) = struct.unpack("<4s4H2LH", tail[marker:marker + 22])
    directory_end = tail_start + marker
    if directory_end >= 20:
        source.seek(directory_end - 20)
        if source.read(4) == b"PK\x06\x07":
            raise DownloadError("EPUB uses an unsupported ZIP64 directory")
    if (disk or directory_disk or disk_members != members
            or marker + 22 + comment_bytes != len(tail)
            or members > MAX_EPUB_MEMBERS
            or directory_bytes > MAX_EPUB_DIRECTORY_BYTES
            or offset == 0xffffffff or offset + directory_bytes != directory_end):
        raise DownloadError("EPUB exceeds the supported ZIP directory budget")
    source.seek(offset)
    count = 0
    while source.tell() < directory_end:
        header = source.read(46)
        if len(header) != 46 or header[:4] != b"PK\x01\x02":
            raise DownloadError("EPUB has an invalid ZIP directory member")
        count += 1
        if count > MAX_EPUB_MEMBERS:
            raise DownloadError("EPUB exceeds the member count budget")
        lengths = struct.unpack_from("<3H", header, 28)
        next_member = source.tell() + sum(lengths)
        if next_member > directory_end:
            raise DownloadError("EPUB has an invalid ZIP directory extent")
        source.seek(next_member)
    if count != members:
        raise DownloadError("EPUB ZIP directory member count is inconsistent")


def _admit_epub_members(archive):
    """Reject unsupported metadata before any member is decoded."""
    members = archive.infolist()
    if len({member.filename for member in members}) != len(members):
        raise DownloadError("EPUB contains duplicate ZIP member names")
    if len(members) > MAX_EPUB_MEMBERS:
        raise DownloadError("EPUB exceeds the member count budget")
    if any(member.file_size > MAX_EPUB_MEMBER_BYTES for member in members):
        raise DownloadError("EPUB exceeds the uncompressed member budget")
    if sum(member.file_size for member in members) > MAX_EPUB_UNCOMPRESSED_BYTES:
        raise DownloadError("EPUB exceeds the total uncompressed budget")
    if any(member.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
           or member.flag_bits & 1 for member in members):
        raise DownloadError("EPUB uses unsupported compression or encryption")
    offsets = sorted(member.header_offset for member in members)
    if len(set(offsets)) != len(offsets):
        raise DownloadError("EPUB members share a local header")
    setattr(archive, "_epub_member_ends", dict(zip(offsets, offsets[1:] + [archive.start_dir])))


@contextlib.contextmanager
def open_epub(path):
    """Admit a bounded archive and retain ownership of its input lifetime."""
    owned = not hasattr(path, "read")
    source = open(path, "rb") if owned else path
    position = None
    try:
        if not owned:
            position = source.tell()
        _check_zip_directory(source)
        source.seek(0)
        with zipfile.ZipFile(source) as archive:
            _admit_epub_members(archive)
            yield archive
    finally:
        if owned:
            source.close()
        elif position is not None:
            source.seek(position)


def _position_epub_member(archive, info):
    """Validate local data extent and position the admitted stream for decoding."""
    source = archive.fp
    if (source is None or not 0 <= info.file_size <= MAX_EPUB_MEMBER_BYTES
            or not 0 <= info.compress_size <= MAX_EPUB_BYTES
            or info.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
            or info.flag_bits & 1 or info.header_offset < 0):
        raise DownloadError("EPUB member is outside the supported byte budget")
    source.seek(info.header_offset)
    header = source.read(30)
    if len(header) != 30 or header[:4] != b"PK\x03\x04":
        raise DownloadError("EPUB member has an invalid local header")
    flags, method = struct.unpack_from("<2H", header, 6)
    name_bytes, extra_bytes = struct.unpack_from("<2H", header, 26)
    try:
        name = source.read(name_bytes).decode("utf-8" if flags & 0x800 else "cp437")
    except UnicodeError as exc:
        raise DownloadError("EPUB member has an invalid filename") from exc
    source.seek(extra_bytes, os.SEEK_CUR)
    if (name != info.orig_filename or flags != info.flag_bits or method != info.compress_type
            or source.tell() + info.compress_size > archive._epub_member_ends[info.header_offset]):
        raise DownloadError("EPUB member metadata does not match its local data")
    if method == zipfile.ZIP_STORED and info.compress_size != info.file_size:
        raise DownloadError("EPUB stored member has inconsistent lengths")
    return source, method


def read_epub_member(archive, member, *, retain=True):
    """Bound inflation itself and verify the complete member before using its bytes."""
    info = archive.getinfo(member) if isinstance(member, str) else member
    source, method = _position_epub_member(archive, info)
    inflater = zlib.decompressobj(-zlib.MAX_WBITS) if method == zipfile.ZIP_DEFLATED else None
    remaining = info.compress_size
    total = crc = 0
    content = bytearray()
    try:
        while remaining:
            block = source.read(min(65_536, remaining))
            if not block:
                raise DownloadError("EPUB member data is truncated")
            remaining -= len(block)
            while True:
                limit = min(65_536, info.file_size + 1 - total)
                piece = (inflater.decompress(block, limit)
                         if inflater is not None else block)
                total += len(piece)
                if total > info.file_size:
                    raise DownloadError("EPUB member exceeds its declared uncompressed length")
                crc = zlib.crc32(piece, crc)
                if retain:
                    content.extend(piece)
                if inflater is not None and inflater.unused_data:
                    raise DownloadError("EPUB member has trailing compressed data")
                if inflater is None:
                    break
                block = inflater.unconsumed_tail
                # A full output buffer can leave a match pending after input ends.
                if not block and (len(piece) < limit or inflater.eof):
                    break
    except zlib.error as exc:
        raise DownloadError("EPUB member has invalid deflate data") from exc
    if total != info.file_size or crc != info.CRC or (inflater is not None and not inflater.eof):
        raise DownloadError("EPUB member length, checksum or deflate endpoint is invalid")
    return bytes(content)


def valid_zip(path):
    try:
        with open_epub(path) as archive:
            for member in archive.infolist():
                read_epub_member(archive, member, retain=False)
            return True
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
        if os.path.getsize(path) > MAX_DOWNLOAD_BYTES:
            return {"_invalid": True}
        with open(path, encoding="utf-8") as f:
            d = json.load(f)
    except Exception:
        return {"_invalid": True}
    if not isinstance(d, dict):
        return {"_invalid": True}
    b64 = d.get("bytes")
    if not isinstance(b64, str) or not b64:
        return {"_invalid": True}
    if len(b64) > 4 * ((MAX_EPUB_BYTES + 2) // 3):
        return {"_invalid": True}
    try:
        decoded = base64.b64decode(b64, validate=True)
    except (ValueError, TypeError, binascii.Error):
        return {"_invalid": True}
    if len(decoded) > MAX_EPUB_BYTES:
        return {"_invalid": True}
    with open(path, "wb") as f:
        f.write(decoded)
    return {k: d.get(k) for k in
            ("registerId", "fileName", "sizeInBytes", "isAuthorised",
             "compilationNumber", "mimeType", "extension")}


@functools.lru_cache(maxsize=1)
def _require_bounded_curl():
    try:
        # Trusted operator PATH selects the local prerequisite; fixed argv, no shell.
        executable = shutil.which("curl")
        if executable is None:
            raise OSError("curl executable is unavailable")
        # Fixed argv invokes the absolute prerequisite selected from the trusted operator PATH.
        # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-audit.dangerous-subprocess-use-audit
        result = subprocess.run([os.path.abspath(executable), "--version"], capture_output=True, text=True)  # nosec B603
    except OSError as exc:
        raise DownloadError("curl 8.4.0 or newer is required for bounded downloads") from exc
    match = re.match(r"curl ([0-9]+)\.([0-9]+)\.([0-9]+)", result.stdout or "")
    if result.returncode or not match or tuple(map(int, match.groups())) < (8, 4, 0):
        raise DownloadError("curl 8.4.0 or later is required for bounded downloads")


def fetch(url, dst, tries=3):
    """Return download metadata, or raise if the response is unusable."""
    _require_bounded_curl()
    part = dst + ".part"
    if os.path.exists(part):
        os.remove(part)
    code = ctype = ""
    problem = "no response"
    for attempt in range(tries):
        meta = None
        args = ["curl", "-sL", "--max-time", "600", "--retry", "2",
                "--retry-all-errors", "--retry-delay", "5",
                "--proto", "=https", "--proto-redir", "=https",
                "-w", "%{http_code}|%{content_type}", "-o", part]
        # Resume only makes sense for a truncated raw transfer.
        resume = attempt > 0 and os.path.exists(part) and sniff(part) == b"P"
        remaining = MAX_DOWNLOAD_BYTES - (os.path.getsize(part) if resume else 0)
        if remaining <= 0:
            problem = "download exceeds the response budget"
            break
        args += ["--max-filesize", str(remaining)]
        if resume:
            args[1:1] = ["-C", "-"]
        p = subprocess.run(args + [url], capture_output=True, text=True)
        out = (p.stdout or "").strip().split("|")
        code = diagnostic(out[0] if out else "?", 10)
        ctype = diagnostic(out[1] if len(out) > 1 else "?")

        if p.returncode:
            problem = "curl exit %s" % p.returncode
        elif not code.isdigit() or not 200 <= int(code) < 300:
            problem = "HTTP %s" % code
        elif os.path.exists(part) and os.path.getsize(part) > MAX_DOWNLOAD_BYTES:
            problem = "download exceeds the response budget"
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
    require_builder_layout(__file__)
    os.makedirs(EPUB_DIR, exist_ok=True)
    with open(os.path.join(SCRATCH, "acts_resolved.json"), encoding="utf-8") as f:
        acts = json.load(f)

    with open(os.path.join(SCRATCH, "download_log.txt"), "a", encoding="utf-8") as log:
        manifest, ok_n, fail_n, total_bytes = [], 0, 0, 0
        snapshots = []
        try:
            for i, a in enumerate(acts, 1):
                rid, d = register_id(a["id"]), iso_date(a["versionStart"])
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
                            compilationRegisterId=prev.get("compilationRegisterId"),
                            isAuthorised=prev.get("isAuthorised"),
                            fetched_at=prev.get("fetched_at")))
                        ok_n += 1
                        total_bytes += sz
                        continue
                if no_document:
                    # Stale prior-version files are deliberately left unreferenced:
                    # deleting them here would break the previous manifest if this
                    # run failed before publishing its replacement.
                    if os.path.exists(dst + ".part"):
                        os.remove(dst + ".part")

                rec = dict(a, sourceUrl=url)
                if no_document:
                    fail_n += 1
                    sz = 0
                    rec.update(epub=None, bytes=0, status="no_epub",
                               reason="current_version_has_no_document")
                    label = "NO_EPUB"
                else:
                    snapshots.append(snapshot_paths(dst, side))
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
                    rec.update(epub=os.path.basename(dst), bytes=sz, status="ok",
                               fetched_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
                    write_json_atomic(side, {
                        "versionStart": d,
                        "compilationNumber": a.get("compilationNumber"),
                        "compilationRegisterId": rec.get("compilationRegisterId"),
                        "bytes": sz,
                        "isAuthorised": rec.get("isAuthorised"),
                        "fetched_at": rec["fetched_at"],
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
