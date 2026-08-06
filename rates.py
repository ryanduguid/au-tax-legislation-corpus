"""Derive a rates-and-thresholds index from the corpus already on disk.

No network. Reads every sections.jsonl and pulls out the provisions that carry
an operative number: rate tables, percentage rates, dollar thresholds and the
indexation machinery that moves them.

Every record cites its Act, compilation and section so the number can be
checked against the provision. This is a finding aid, not a substitute for
reading the law.
"""
import json, os, re, glob, collections

from corpus_paths import child, corpus_root, register_id

ROOT = corpus_root(__file__)
OUT = child(ROOT, "rates")

MONEY = re.compile(r'\$\s?[\d,]+(?:\.\d+)?')
PCT = re.compile(r'(?<!\d)\d+(?:\.\d+)?\s?%')
# Bare multipliers: FBT gross-up 2.0802 / 1.8868, statutory fractions.
FACTOR = re.compile(r'(?<![\d.])\d\.\d{2,4}(?![\d])')
# "the rate is 30%", "at the rate of 47%", "is 0.5 of the amount"
RATE_PHRASE = re.compile(r'(rate[s]? (?:is|are|of)|percentage|factor|multiplied by|'
                         r'gross[\u2011-]up|indexation factor)', re.I)
THRESHOLD_PHRASE = re.compile(r'(threshold|limit|cap|maximum|minimum|exceeds|'
                              r'does not exceed|tax[\u2011-]free)', re.I)
INDEX_PHRASE = re.compile(r'(index(ed|ation)|CPI|consumer price index|AWOTE|'
                          r'average weekly ordinary time earnings)', re.I)
# Ownership and control tests are percentages, but not rates of tax. The
# continuity-of-ownership test, the 100%-subsidiary rule and the 75% trust
# voting interest test all carry a "%" and would otherwise swamp the rate
# bucket, where someone is looking for what a tax is charged at.
TEST_PHRASE = re.compile(r'((?<!\d)\d+% (stake|subsidiary|interest)|more than a? ?(?<!\d)\d+% stake|'
                         r'(voting|dividend|capital|ownership|control|equity) '
                         r'(interest|right|stake|power)s?|'
                         r'continuity of ownership|beneficial(ly)? (own|entitled)|'
                         r'wholly[‑-]owned|majority[‑-]owned)', re.I)
YEAR = re.compile(r'\b(19|20)\d{2}[\u2011-]\d{2}\b|\b(19|20)\d{2}\b')

# Topic buckets, matched against Act name + section heading.
TOPICS = [
    # Order matters: first match wins, so put the specific before the general.
    ("customs tariff schedules", r'customs tariff|tariff (classification|schedule|item)|'
                                 r'chapter \d+ .*(goods|articles)'),
    ("excise and fuel", r'excise|fuel tax credit|diesel|petroleum product'),
    ("income tax rates", r'income tax.*rate|rates act|marginal rate|tax[‑-]free threshold|'
                         r'resident taxpayer|non[‑-]resident.*rate'),
    ("medicare levy", r'medicare'),
    ("fringe benefits tax", r'fringe benefit|(?<![A-Za-z])FBT(?![A-Za-z])|gross[‑-]up'),
    ("superannuation", r'superannuation|super(annuation)? (guarantee|contribution|fund)|'
                       r'concessional cap|transfer balance|(?<![A-Za-z])SMSF(?![A-Za-z])'),
    ("gst and indirect", r'goods and services tax|(?<![A-Za-z])GST(?![A-Za-z])|wine equalisation|luxury car tax'),
    ("capital gains", r'capital gain|(?<![A-Za-z])CGT(?![A-Za-z])|cost base|discount capital'),
    ("capital allowances and depreciation", r'depreciat|capital allowance|decline in value|'
                                            r'effective life|car limit|low[‑-]value pool'),
    ("companies and franking", r'franking|imputation|dividend|company tax|corporate tax rate|'
                               r'base rate entity'),
    ("trusts and partnerships", r'(?<![A-Za-z])trust(?![A-Za-z])|trustee|partnership|beneficiary'),
    ("deductions and offsets", r'deduction|offset|rebate|substantiat|entertainment|'
                               r'gift|donation'),
    ("withholding and PAYG", r'withholding|(?<![A-Za-z])PAYG(?![A-Za-z])|tax file number|instal?ment'),
    ("family assistance", r'family assistance|family tax benefit|child care'),
    ("petroleum and mining", r'petroleum resource rent|minerals resource rent|mining'),
    ("R&D and incentives", r'research and development|(?<![A-Za-z])R&D(?![A-Za-z])|incentive|concession'),
    ("small business", r'small business|aggregated turnover'),
    ("administration and penalties", r'penalt|shortfall|general interest charge|(?<![A-Za-z])GIC(?![A-Za-z])|'
                                     r'(?<![A-Za-z])SIC(?![A-Za-z])|administrative|objection|assessment'),
    ("international", r'foreign|non[‑-]resident|transfer pricing|thin capitalisation|'
                      r'double tax|treaty'),
]


def topic_for(act, heading, body=""):
    # ITAA 1997 covers every topic, so the Act name alone tells you nothing.
    # Weight the heading and the provision text ahead of it.
    hay = "%s %s %s" % (heading or "", body[:300], act)
    for name, pat in TOPICS:
        if re.search(pat, hay, re.I):
            return name
    return "other"


def table_blocks(text):
    """Yield contiguous markdown tables as lists of lines."""
    cur = []
    for ln in text.split("\n"):
        if ln.startswith("|"):
            cur.append(ln)
        elif cur:
            yield cur
            cur = []
    if cur:
        yield cur


def is_rate_table(lines):
    body = " ".join(lines)
    return bool(MONEY.search(body) or PCT.search(body))


def sentences(text):
    plain = "\n".join(l for l in text.split("\n") if not l.startswith("|"))
    pieces, start, index = [], 0, 0
    while index < len(plain):
        char = plain[index]
        if char == "\n" and index + 1 < len(plain) and plain[index + 1] == "\n":
            pieces.append(plain[start:index])
            index += 2
            while index < len(plain) and plain[index] == "\n":
                index += 1
            start = index
            continue
        if char in ".;:":
            following = index + 1
            while following < len(plain) and plain[following].isspace():
                following += 1
            if following > index + 1 and following < len(plain) and (
                    plain[following] in "ABCDEFGHIJKLMNOPQRSTUVWXYZ(\u2022*"):
                pieces.append(plain[start:index + 1])
                start = following
                index = following
                continue
        index += 1
    pieces.append(plain[start:])
    for s in pieces:
        s = s.strip()
        if 15 < len(s) < 600:
            yield s


def main():
    os.makedirs(OUT, exist_ok=True)
    records = []

    markdown_root = child(ROOT, "markdown")
    for candidate in sorted(glob.glob(os.path.join(markdown_root, "*", "sections.jsonl"))):
        rid = register_id(os.path.basename(os.path.dirname(candidate)))
        p = child(markdown_root, rid, "sections.jsonl")
        for ln in open(p, encoding="utf-8"):
            if not ln.strip():
                continue
            r = json.loads(ln)
            text = r.get("text") or ""
            rid = register_id(r["register_id"])
            # Gross-up factors, indexation factors and statutory fractions are
            # bare decimals (2.0802, 1.8868, 0.5), not $ or %, so a filter on
            # currency and percent alone misses them entirely.
            if not (MONEY.search(text) or PCT.search(text) or
                    (FACTOR.search(text) and RATE_PHRASE.search(text))):
                continue
            base = {
                "register_id": rid, "act": r["act"],
                "collection": r.get("collection"),
                "compilation_number": r.get("compilation_number"),
                "compilation_date": r.get("compilation_date"),
                "section": r.get("section"), "heading": r.get("heading"),
                "register_page": r.get("register_page"),
                "topic": topic_for(r["act"], r.get("heading"), text),
            }

            for tbl in table_blocks(text):
                if len(tbl) >= 3 and is_rate_table(tbl):
                    records.append(dict(base, kind="table",
                                        years=sorted({m.group(0) for m in YEAR.finditer(" ".join(tbl))}),
                                        content="\n".join(tbl)))

            for s in sentences(text):
                has_money, has_pct = bool(MONEY.search(s)), bool(PCT.search(s))
                has_factor = bool(FACTOR.search(s)) and bool(RATE_PHRASE.search(s))
                if not (has_money or has_pct or has_factor):
                    continue
                # A percentage stands on its own. Requiring the word "rate"
                # alongside it dropped s 9-70 of the GST Act — "The amount of
                # GST on a taxable supply is 10% of the value of the taxable
                # supply" — which is the operative charging provision for the
                # whole tax. Dollar amounts still need a phrase, because
                # legislation is full of incidental sums.
                if not has_pct and not (RATE_PHRASE.search(s)
                                        or THRESHOLD_PHRASE.search(s)
                                        or INDEX_PHRASE.search(s)):
                    continue
                kind = ("indexation" if INDEX_PHRASE.search(s)
                        else "ownership test" if has_pct and TEST_PHRASE.search(s)
                        else "rate" if has_pct else
                        "factor" if has_factor and not has_money else "threshold")
                records.append(dict(base, kind=kind,
                                    amounts=sorted(set(MONEY.findall(s)) | set(PCT.findall(s))
                                                   | (set(FACTOR.findall(s)) if has_factor else set())),
                                    years=sorted({m.group(0) for m in YEAR.finditer(s)}),
                                    content=s))

    # Stable ids so a rebuild does not reshuffle references.
    records.sort(key=lambda r: (r["topic"], r["act"], str(r["section"] or ""), r["kind"]))
    for i, r in enumerate(records, 1):
        r["rate_id"] = "R%05d" % i

    # The source renders hyphens as U+2011 (non-breaking), so a plain search for
    # "gross-up" or "tax-free" finds nothing. Ship an ASCII-folded field.
    def fold(t):
        for ch in ("‑", "‐", "‒", "–", "—"):
            t = t.replace(ch, "-")
        return t.replace(" ", " ")

    for r in records:
        r["content_ascii"] = fold(r["content"])
        r["heading_ascii"] = fold(r["heading"]) if r.get("heading") else None

    with open(child(OUT, "rates.jsonl"), "w", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    by_topic = collections.Counter(r["topic"] for r in records)
    by_kind = collections.Counter(r["kind"] for r in records)
    by_coll = collections.Counter(r["collection"] or "unknown" for r in records)

    md = ["# Rates, thresholds and indexation",
          "",
          "Derived from the Acts and instruments in this corpus. Every entry cites its",
          "title, compilation and section. Check the provision before relying on a number:",
          "this is a finding aid, not advice, and not the authorised text.",
          "",
          "%s entries across %s titles." % (
              f"{len(records):,}", f"{len({r['register_id'] for r in records}):,}"),
          "",
          "A rate set by an Act and a rate set by a determination made under it are not",
          "the same kind of thing: an instrument can be disallowed or sunset while its",
          "enabling Act stands. The `collection` field on every row says which you have.",
          "",
          "Figures the ATO computes rather than Parliament enacts are absent by",
          "construction, because they appear in no legislation: the FBT gross-up factors,",
          "the Division 7A benchmark rate and the indexed superannuation caps among them.",
          "The provisions here give the formula; the ATO publishes the result.",
          "",
          "| Kind | Count |", "|---|---|"]
    for k, n in by_kind.most_common():
        md.append("| %s | %s |" % (k, f"{n:,}"))
    md += ["", "| Collection | Count |", "|---|---|"]
    for c, n in by_coll.most_common():
        md.append("| %s | %s |" % (c, f"{n:,}"))
    md += ["", "| Topic | Count |", "|---|---|"]
    for t, n in by_topic.most_common():
        md.append("| %s | %s |" % (t, f"{n:,}"))

    for topic, _ in by_topic.most_common():
        md += ["", "## " + topic, ""]
        rs = [r for r in records if r["topic"] == topic]
        tables = [r for r in rs if r["kind"] == "table"]
        if tables:
            md.append("### Rate tables")
            md.append("")
            for r in tables[:40]:
                md.append("**%s**%s %s — %s" % (
                    r["act"],
                    " _(instrument)_" if (r["collection"] or "Act") != "Act" else "",
                    ("s " + r["section"]) if r["section"] else "",
                    r["heading"] or ""))
                md.append("")
                md.append(r["content"])
                md.append("")
            if len(tables) > 40:
                md.append("_%d further tables in rates.jsonl._" % (len(tables) - 40))
                md.append("")
        others = [r for r in rs if r["kind"] != "table"]
        if others:
            md += ["### Provisions carrying a number", "",
                   "| Title | Made by | Section | Kind | Amounts | Provision |",
                   "|---|---|---|---|---|---|"]
            for r in others[:120]:
                md.append("| %s | %s | %s | %s | %s | %s |" % (
                    r["act"].replace("|", "\\|")[:48],
                    "Act" if (r["collection"] or "Act") == "Act" else "instrument",
                    r["section"] or "-", r["kind"],
                    ", ".join(r.get("amounts", []))[:40],
                    r["content"].replace("|", "\\|").replace("\n", " ")[:150]))
            if len(others) > 120:
                md.append("")
                md.append("_%d further provisions in rates.jsonl._" % (len(others) - 120))

    with open(child(OUT, "RATES.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(md) + "\n")

    print("entries: %d  titles: %d" % (
        len(records), len({r["register_id"] for r in records})))
    print("by collection:", dict(by_coll))
    print("by kind:", dict(by_kind))
    print("by topic:", dict(by_topic.most_common(8)))


if __name__ == "__main__":
    main()
