"""Probe the Register's version history for every title download.py recorded
as no_epub, and write probe13.json so retry13.py can recover the ones that
have an older published compilation."""
import json, os, time, urllib.parse

from curl_fetch import curl_json as _curl_json

API = "https://api.prod.legislation.gov.au/v1"
SCRATCH = os.path.dirname(os.path.abspath(__file__))


def curl_json(url, tries=3):
    return _curl_json(url, os.path.join(SCRATCH, "_p13.json"), tries)


def main():
    with open(os.path.join(SCRATCH, "manifest_raw.json"), encoding="utf-8") as source:
        manifest = json.load(source)
    missing = [item for item in manifest if not item.get("epub")]
    output = []
    for item in missing:
        rid = item["id"]
        # Descending by start: $top caps the page, so ascending order would
        # return the 60 OLDEST versions and a long-history title would resolve
        # to a decades-old compilation.
        response = curl_json(
            "%s/versions?$top=60&$orderby=start%%20desc&$filter=%s&$select=titleId,start,end,isCurrent,compilationNumber,registerId"
            % (API, urllib.parse.quote("titleId eq '%s'" % rid))
        )
        if response is None:
            # retry13.py treats an entry without versions as unrecoverable, so
            # swallowing an API failure here would silently drop the title.
            raise RuntimeError(
                "version lookup failed for %s after retries; refusing to "
                "write probe13.json with that title missing" % rid)
        versions = response.get("value") or []
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
    with open(os.path.join(SCRATCH, "probe13.json"), "w", encoding="utf-8") as destination:
        json.dump(output, destination, indent=1)


if __name__ == "__main__":
    main()
