import json, os, subprocess, time, urllib.parse
API = "https://api.prod.legislation.gov.au/v1"
def curl_json(url, tries=3):
    dst = "_p13.json"
    for _ in range(tries):
        if os.path.exists(dst): os.remove(dst)
        p = subprocess.run(["curl","-sL","--max-time","90","-o",dst,url], capture_output=True)
        if p.returncode == 0:
            try:
                d = json.load(open(dst, encoding="utf-8"))
                if "error" not in d: return d
            except Exception: pass
        time.sleep(6)
    return None

m = json.load(open("manifest_raw.json", encoding="utf-8"))
bad = [a for a in m if not a.get("epub")]
out = []
for a in bad:
    rid = a["id"]
    d = curl_json("%s/versions?$top=60&$orderby=start&$filter=%s&$select=titleId,start,end,isCurrent,compilationNumber,registerId"
                  % (API, urllib.parse.quote("titleId eq '%s'" % rid)))
    vs = (d or {}).get("value") or []
    withdoc = [v for v in vs if v.get("registerId")]
    cur = [v for v in vs if v.get("isCurrent")]
    latest = max(withdoc, key=lambda v: v["start"]) if withdoc else None
    out.append({"id": rid, "name": a["name"], "n_versions": len(vs),
                "current_has_doc": bool(cur and cur[0].get("registerId")),
                "latest_doc": latest})
    print("%-12s vers=%-3d cur_doc=%-5s latest=%s %s" % (
        rid, len(vs), bool(cur and cur[0].get("registerId")),
        (latest or {}).get("registerId"), (latest or {}).get("start","")[:10]))
    time.sleep(1.5)
json.dump(out, open("probe13.json","w",encoding="utf-8"), indent=1)
