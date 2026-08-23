"""Fetch a JSON document with curl, one fresh temp file per attempt.

curl does not truncate -o on transport failure, so a temp file that survives
between requests re-reads the previous response and hands it back as if it
were this one. On the discovery paging path that is exactly how 142 titles
went missing, and on the version lookups it silently gave one Act another
Act's record. Four stages carried their own copy of the same loop; this is
the one definition. Each caller passes its own temp path, so two stages
sharing a checkout never share a response file.
"""
import json, os, subprocess, time


def curl_json(url, dst, tries=3, delay=6):
    """Return the decoded JSON at ``url``, or None after ``tries`` failures."""
    for _ in range(tries):
        if os.path.exists(dst):
            os.remove(dst)
        p = subprocess.run(["curl", "-sL", "--max-time", "90", "-o", dst, url],
                           capture_output=True)
        if p.returncode == 0:
            try:
                with open(dst, encoding="utf-8") as f:
                    d = json.load(f)
                if "error" not in d:
                    return d
            except (OSError, UnicodeDecodeError, json.JSONDecodeError):
                pass
        time.sleep(delay)
    return None
