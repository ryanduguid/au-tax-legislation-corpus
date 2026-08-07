"""Report which titles in this knowledge base have been superseded on the Register.

Usage:  python check_current.py [Act|LegislativeInstrument|NotifiableInstrument]

Reads sources.json, asks the Federal Register API for each title's current
compilation, and prints any that have moved on. Read-only, no downloads.

Legislative instruments sunset under Part 4 of the Legislation Act 2003, so a
title can stop being in force without its compilation changing. When no current
version comes back, this asks again without the isCurrent filter: versions but
none current means the title has fallen out of force, nothing at all means the
lookup failed. Reporting both as one number hid a repeal behind a network error.
"""
import json, os, subprocess, sys, time, urllib.parse

from corpus_paths import child, corpus_root

API = "https://api.prod.legislation.gov.au/v1"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
# Finalize publishes this script at the completed corpus root. In a source
# checkout, output lives under the deterministic ``corpus/`` directory instead.
ROOT = SCRIPT_DIR if os.path.isfile(os.path.join(SCRIPT_DIR, "sources.json")) else corpus_root(__file__)
DELAY = 1.5


def curl_json(url, tries=3):
    """curl does not truncate -o on transport failure, so a stale file from the
    previous request would be re-read and silently reported as a fresh answer."""
    dst = child(ROOT, "_check_tmp.json")
    for _ in range(tries):
        if os.path.exists(dst):
            os.remove(dst)
        p = subprocess.run(["curl.exe", "-sL", "--max-time", "90", "-o", dst, url],
                           capture_output=True)
        if p.returncode == 0:
            try:
                with open(dst, encoding="utf-8") as f:
                    d = json.load(f)
                if "error" not in d:
                    return d
            except Exception:
                pass
        time.sleep(6)
    return None


def main():
    with open(child(ROOT, "sources.json"), encoding="utf-8") as f:
        src = json.load(f)

    # sources.json before instruments were added called this key "acts".
    acts = src.get("titles") or src["acts"]
    want = sys.argv[1] if len(sys.argv) > 1 else None
    if want:
        acts = [a for a in acts if (a.get("collection") or "") == want]
        if not acts:
            print("No titles in collection %r." % want)
            return
    print("Built %s. Checking %d titles against the Register.%s\n"
          % (src["retrieved"], len(acts),
             ("  Roughly %d minutes." % max(1, round(len(acts) * DELAY / 60)))))
    stale, unchanged, errors, gone, nodoc = [], 0, [], [], []

    def versions(flt, top=1):
        d = curl_json("%s/versions?$top=%d&$filter=%s"
                      "&$select=titleId,start,compilationNumber,registerId"
                      % (API, top, urllib.parse.quote(flt)))
        v = (d or {}).get("value") or []
        return v if d is not None else None

    for i, a in enumerate(acts, 1):
        v = versions("titleId eq '%s' and isCurrent eq true" % a["register_id"])
        # Guard against a response for the wrong title being read as this one's.
        if v and v[0].get("titleId") not in (None, a["register_id"]):
            v = []
        if not v:
            # Distinguish sunset or repealed from a failed lookup.
            any_v = versions("titleId eq '%s'" % a["register_id"])
            time.sleep(DELAY)
            if any_v is None:
                errors.append(a)
            elif any_v:
                gone.append(a)
            else:
                errors.append(a)
        else:
            now_c = v[0].get("compilationNumber")
            now_d = v[0]["start"][:10]
            if str(now_c) != str(a["compilation_number"]) or now_d != a["compilation_date"]:
                # A current version with no registerId has no document behind
                # it: the Register knows the amendment commenced but has not
                # published the compilation. Telling anyone to re-download it
                # sends them at a URL that answers 404. These are the titles
                # retry13.py already resolved to the last published version.
                if v[0].get("registerId") is None:
                    nodoc.append((a, now_d))
                else:
                    stale.append((a, now_c, now_d))
            else:
                unchanged += 1
        if i % 25 == 0:
            # Redirected to a file, Python block-buffers stdout, so a 30-minute
            # run over the whole corpus shows nothing at all until it exits.
            print("  checked %d/%d" % (i, len(acts)), flush=True)
        time.sleep(DELAY)

    print("\nunchanged: %d   superseded: %d   no compilation published: %d   "
          "no longer in force: %d   lookup failed: %d"
          % (unchanged, len(stale), len(nodoc), len(gone), len(errors)))
    if nodoc:
        print("\nNO COMPILATION PUBLISHED for the version now in force. The text "
              "here is the last one the Register holds, already marked "
              "version_is_current: false. Re-downloading returns 404 until the "
              "Register publishes the compilation:")
        for a, dt in nodoc:
            print("  %-12s holds %s, in force since %s  %s"
                  % (a["register_id"], a["compilation_date"], dt, a["name"][:44]))
    if stale:
        print("\nSUPERSEDED, re-download these:")
        for a, c, dt in stale:
            print("  %-12s comp %s (%s) -> comp %s (%s)  %s"
                  % (a["register_id"], a["compilation_number"], a["compilation_date"],
                     c, dt, a["name"][:55]))
    if gone:
        print("\nNO LONGER IN FORCE (repealed, or sunset under Part 4 of the "
              "Legislation Act 2003). Drop these:")
        for a in gone:
            print("  %-12s %-22s %s"
                  % (a["register_id"], a.get("collection") or "-", a["name"][:50]))
    if errors:
        print("\nLOOKUP FAILED:")
        for a in errors:
            print("  %-12s %s" % (a["register_id"], a["name"][:60]))
    tmp = child(ROOT, "_check_tmp.json")
    if os.path.exists(tmp):
        os.remove(tmp)


if __name__ == "__main__":
    main()
