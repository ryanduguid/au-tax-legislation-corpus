"""Fetch a JSON document from the Register API, one definition for every stage.

This used to shell out to curl and read the response back from a temp file.
curl does not truncate -o on transport failure, so a temp file that survived
between requests re-read the previous response and handed it back as if it
were this one: on the discovery paging path that is how 142 titles went
missing, and on the version lookups it silently gave one Act another Act's
record. The guard was a fresh file per attempt, and every stage had to supply
its own path so that two stages sharing a checkout never shared a file.

Reading the response off the socket removes the file, and the whole class of
failure with it. There is nothing on disk for one attempt to inherit from
another, so there is no path to thread through the callers either.

Downloads still use curl: see download.py, where --retry earns its keep on a
600-second EPUB stream.
"""
import http.client
import json
import time
import urllib.error
import urllib.request

# Per-socket-operation, not the whole transfer: curl --max-time 90 capped the
# transfer end to end, so a response that dribbles can now outlast 90 seconds
# where curl would have cut it. These are small JSON pages off one API, and a
# stalled socket still trips the timeout, so the looser bound is acceptable.
TIMEOUT = 90
UA = "au-tax-legislation-corpus (+https://github.com/ryanduguid/au-tax-legislation-corpus)"


def fetch_json(url, tries=3, delay=6):
    """Return the decoded JSON at ``url``, or None after ``tries`` failures.

    A non-2xx status raises inside urlopen and is retried like any other
    transport failure. curl reported those as success and left the error body
    to be parsed, which is why the caller-side ``"error" not in d`` check
    below still exists: the API also returns 200 with an error document.
    """
    request = urllib.request.Request(url, headers={"User-Agent": UA})
    for _ in range(tries):
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                document = json.load(response)
            if "error" not in document:
                return document
        except (urllib.error.URLError, http.client.HTTPException, OSError,
                UnicodeDecodeError, json.JSONDecodeError):
            pass
        time.sleep(delay)
    return None
