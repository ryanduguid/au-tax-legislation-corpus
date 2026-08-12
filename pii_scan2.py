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

from corpus_paths import child, corpus_root, register_id
from pii_patterns import private_person_registration_details

ROOT = corpus_root(__file__)
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "pii_flagged.json"), encoding="utf-8") as source:
    KNOWN = {item["register_id"] for item in json.load(source)}

EMAIL = re.compile(r"\b[\w.+-]+@[\w-]+\.[\w.]{2,}\b")
# Australian formats: 02 1234 5678, (02) 1234 5678, 0412 345 678, 1300 123 456.
PHONE = re.compile(r"(?<!\d)(?:\(0\d\)\s?\d{4}\s?\d{4}|0[2-8]\s\d{4}\s\d{4}|"
                   r"04\d{2}\s\d{3}\s\d{3}|1[38]00\s\d{3}\s\d{3})(?!\d)")
TFN = re.compile(r"\btax file number\s*:?\s*\d", re.I)


def main():
    weak, contacts = [], collections.Counter()
    ex = collections.defaultdict(list)
    markdown_root = child(ROOT, "markdown")
    for candidate in sorted(glob.glob(os.path.join(markdown_root, "*", "sections.jsonl"))):
        rid = register_id(os.path.basename(os.path.dirname(candidate)))
        p = child(markdown_root, rid, "sections.jsonl")
        with open(p, encoding="utf-8") as source:
            for line in source:
                if not line.strip():
                    continue
                row = json.loads(line)
                text = row.get("text") or ""
                for label, pattern in (("email", EMAIL), ("phone", PHONE), ("tfn", TFN)):
                    for match in pattern.findall(text):
                        contacts[label] += 1
                        if len(ex[label]) < 5:
                            ex[label].append((rid, str(match)[:44]))
                if rid in KNOWN:
                    continue
                names, regs = private_person_registration_details(text)
                if names and regs:
                    weak.append((rid, row.get("row_id"), len(names), len(regs)))

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
