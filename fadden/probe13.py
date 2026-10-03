"""Probe the Register's version history for every title download.py recorded
as no_epub, and write probe13.json so retry13.py can recover the ones that
have an older published compilation."""
import json
import os
import time
import urllib.parse
from typing import TYPE_CHECKING

if TYPE_CHECKING or __package__:
    from .corpus_paths import (
        compilation_id,
        register_id,
        require_builder_layout,
        version_date,
        version_rows,
        write_json_atomic,
    )
    from .http_fetch import fetch_json
else:
    from corpus_paths import (
        compilation_id,
        register_id,
        require_builder_layout,
        version_date,
        version_rows,
        write_json_atomic,
    )
    from http_fetch import fetch_json


API = "https://api.prod.legislation.gov.au/v1"
SCRATCH = os.path.dirname(os.path.abspath(__file__))


def main():
    require_builder_layout(__file__)
    with open(os.path.join(SCRATCH, "manifest_raw.json"), encoding="utf-8") as source:
        manifest = json.load(source)
    missing = [item for item in manifest if not item.get("epub")]
    output = []
    for item in missing:
        rid = register_id(item["id"])
        # Descending by start: $top caps the page, so ascending order would
        # return the 60 OLDEST versions and a long-history title would resolve
        # to a decades-old compilation.
        response = fetch_json(
            "%s/versions?$top=60&$orderby=start%%20desc&$filter=%s&$select=titleId,start,end,isCurrent,compilationNumber,registerId"
            % (API, urllib.parse.quote("titleId eq '%s'" % rid))
        )
        if response is None:
            # retry13.py treats an entry without versions as unrecoverable, so
            # swallowing an API failure here would silently drop the title.
            raise RuntimeError(
                "version lookup failed for %s after retries; refusing to "
                "write probe13.json with that title missing" % rid)
        versions = version_rows(response, rid)
        for version in versions:
            version_date(version.get("start"))
            compilation_id(version)
        with_document = [version for version in versions if version.get("registerId")]
        current = [version for version in versions if version.get("isCurrent")]
        latest = max(with_document, key=lambda version: version["start"]) if with_document else None
        output.append({
            "id": rid,
            "name": item["name"],
            "n_versions": len(versions),
            "current_has_doc": bool(current and current[0].get("registerId")),
            "latest_doc": latest,
        })
        print("%-12s vers=%-3d cur_doc=%-5s latest=%s %s" % (
            rid, len(versions), bool(current and current[0].get("registerId")),
            (latest or {}).get("registerId"), (latest or {}).get("start", "")[:10]))
        time.sleep(1.5)
    write_json_atomic(os.path.join(SCRATCH, "probe13.json"), output, indent=1)


if __name__ == "__main__":
    main()
