"""Second PII pass: what the first one would have missed.

pii_scan.py needed three registration numbers AND three names in one row, which
is the shape of a disciplinary table. Two things that shape misses:

  - the same register split so finely that a row holds one person
  - contact details, which carry no registration number at all

So this drops the threshold to a single name-plus-number pairing, and sweeps
separately for emails, phone numbers and tax file numbers anywhere in the
corpus. A statute quoting "1 300 000 000" as a dollar figure is not a phone
number, so the phone pattern pins its digit counts and prefixes even though
it now accepts unspaced, +61 and 13/1300/1800 forms.
"""
import collections
import glob
import json
import os
import sys

from corpus_paths import child, corpus_root, register_id
from pii_patterns import (contact_fingerprints, load_contact_allowlist,
                          private_person_registration_details)

ROOT = corpus_root(__file__)
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "pii_flagged.json"), encoding="utf-8") as source:
    KNOWN = {item["register_id"] for item in json.load(source)}

ALLOWLIST = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         "pii_contact_allowlist.json")


def main():
    approved = load_contact_allowlist(ALLOWLIST)
    weak, contacts, unapproved = [], collections.Counter(), collections.Counter()
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
                for label, digest in contact_fingerprints(text):
                    contacts[label] += 1
                    key = (label, digest, rid)
                    if key not in approved:
                        unapproved[label] += 1
                        if len(ex[label]) < 5:
                            # Scanner output commonly lands in CI and audit logs.  Keep
                            # enough information to find and compare a match without
                            # copying the contact identifier into those logs.
                            ex[label].append((rid, row.get("row_id"), digest[:16]))
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
    print("unapproved contact details:", dict(unapproved) or "none")
    for k, v in ex.items():
        print("   %-6s register_id, row_id, sha256[:16]: %s" % (k, v[:3]))
    return 1 if weak or unapproved else 0


if __name__ == "__main__":
    sys.exit(main())
