"""Second PII pass: what the first one would have missed.

pii_scan.py needed three registration numbers AND three names in one row, which
is the shape of a disciplinary table. Two things that shape misses:

  - the same register split so finely that a row holds one person
  - contact details, which carry no registration number at all

So this drops the threshold to a single name-plus-number pairing, and sweeps
separately for emails, phone numbers and tax file numbers anywhere in the
corpus. A statute quoting "1 300 000 000" as a dollar figure is not a phone
number, so the phone pattern requires the conventional grouping.
"""
import collections, glob, json, os, re

ROOT = os.environ.get("ATO_KB_ROOT", r"C:\ato-kb")
KNOWN = set(json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                        "pii_flagged.json"), encoding="utf-8")
                     ) and [f["register_id"] for f in json.load(
    open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      "pii_flagged.json"), encoding="utf-8"))])

NAME = re.compile(r"\b[A-Z][a-z]{1,15},?\s+[A-Z][a-z]{1,15}(?:\s+[A-Z][a-z]{1,15})?\b")
REGNO = re.compile(r"\b\d{8}\b")
STATUTORY = re.compile(
    r"\b(Act|Regulation|Schedule|Division|Subdivision|Part|Chapter|Section|"
    r"Commissioner|Minister|Treasurer|Commonwealth|Australian|Australia|Board|"
    r"Tax|Taxation|Income|Superannuation|Court|Tribunal|Determination|Notice|"
    r"Instrument|Amendment|January|February|March|April|May|June|July|August|"
    r"September|October|November|December)\b")

EMAIL = re.compile(r"\b[\w.+-]+@[\w-]+\.[\w.]{2,}\b")
# Australian formats: 02 1234 5678, (02) 1234 5678, 0412 345 678, 1300 123 456.
PHONE = re.compile(r"(?<!\d)(?:\(0\d\)\s?\d{4}\s?\d{4}|0[2-8]\s\d{4}\s\d{4}|"
                   r"04\d{2}\s\d{3}\s\d{3}|1[38]00\s\d{3}\s\d{3})(?!\d)")
TFN = re.compile(r"\btax file number\s*:?\s*\d", re.I)


def main():
    weak, contacts = [], collections.Counter()
    ex = collections.defaultdict(list)
    for p in sorted(glob.glob(os.path.join(ROOT, "markdown", "*", "sections.jsonl"))):
        rid = os.path.basename(os.path.dirname(p))
        for l in open(p, encoding="utf-8"):
            if not l.strip():
                continue
            r = json.loads(l)
            t = r.get("text") or ""
            for label, pat in (("email", EMAIL), ("phone", PHONE), ("tfn", TFN)):
                for m in pat.findall(t):
                    contacts[label] += 1
                    if len(ex[label]) < 5:
                        ex[label].append((rid, str(m)[:44]))
            if rid in KNOWN:
                continue
            names = {m.group(0) for m in NAME.finditer(t)
                     if not STATUTORY.search(m.group(0))}
            regs = set(REGNO.findall(t))
            if names and regs:
                weak.append((rid, r.get("row_id"), len(names), len(regs)))

    print("rows outside the 12 known titles pairing a name with an 8-digit "
          "number: %d" % len(weak))
    for w in weak[:12]:
        print("   %-12s %-26s names=%d nums=%d" % w)
    print()
    print("contact details across the whole corpus:", dict(contacts) or "none")
    for k, v in ex.items():
        print("   %-6s %s" % (k, v[:3]))


if __name__ == "__main__":
    main()
