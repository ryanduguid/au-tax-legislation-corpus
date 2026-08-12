import json, os, subprocess, time, urllib.parse

API = "https://api.prod.legislation.gov.au/v1"
SCRATCH = os.path.dirname(os.path.abspath(__file__))


def curl_json(url, tries=3):
    dst = os.path.join(SCRATCH, "_p13.json")
    for _ in range(tries):
        if os.path.exists(dst): os.remove(dst)
        p = subprocess.run(["curl","-sL","--max-time","90","-o",dst,url], capture_output=True)
        if p.returncode == 0:
            try:
                with open(dst, encoding="utf-8") as source:
                    d = json.load(source)
                if "error" not in d: return d
            except Exception: pass
        time.sleep(6)
    return None


def main():
    with open(os.path.join(SCRATCH, "manifest_raw.json"), encoding="utf-8") as source:
        manifest = json.load(source)
    missing = [item for item in manifest if not item.get("epub")]
    output = []
    for item in missing:
        rid = item["id"]
        response = curl_json(
            "%s/versions?$top=60&$orderby=start&$filter=%s&$select=titleId,start,end,isCurrent,compilationNumber,registerId"
            % (API, urllib.parse.quote("titleId eq '%s'" % rid))
        )
        versions = (response or {}).get("value") or []
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
