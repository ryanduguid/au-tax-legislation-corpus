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
YEAR = re.compile(r'\b(19|20)\d{2}[\u2011-]\d{2}\b|\b(19|20)\d{2}\b')


def _consume_digits(text, index, allow_commas=False):
    """Return the end of one forward-only decimal token.

    ``re`` is excellent for the bounded patterns below, but amounts and
    percentages arrive from long, externally supplied legislative text.  These
    scanners deliberately advance their cursor only forward, avoiding a
    backtracking path for an unterminated run of digits.
    """
    start = index
    while index < len(text):
        char = text[index]
        if char.isdecimal():
            index += 1
        elif (allow_commas and char == "," and index > start and
              index + 1 < len(text) and text[index + 1].isdecimal()):
            index += 1
        else:
            break
    return index


def money_values(text):
    """Return well-formed dollar amounts without regex backtracking."""
    values, index = [], 0
    while index < len(text):
        if text[index] != "$":
            index += 1
            continue
        start = index
        index += 1
        if index < len(text) and text[index].isspace():
            index += 1
        number_start = index
        index = _consume_digits(text, index, allow_commas=True)
        if index == number_start:
            continue
        if index < len(text) and text[index] == ".":
            fraction_start = index + 1
            fraction_end = _consume_digits(text, fraction_start)
            if fraction_end > fraction_start:
                index = fraction_end
        values.append(text[start:index])
    return values


def percentage_matches(text):
    r"""Return ``(value, start, end)`` triples for decimal percentages.

    The accepted forms remain the ones used by the rate index: ``10%`` and
    ``12.5 %``.  ``str.isdecimal`` has the same Unicode-decimal intent as the
    former ``\d`` pattern.
    """
    values, index = [], 0
    while index < len(text):
        if not text[index].isdecimal() or (index and text[index - 1].isdecimal()):
            index += 1
            continue
        start = index
        end = _consume_digits(text, index)
        if end < len(text) and text[end] == ".":
            fraction_start = end + 1
            fraction_end = _consume_digits(text, fraction_start)
            if fraction_end > fraction_start:
                end = fraction_end
        if end < len(text) and text[end].isspace():
            end += 1
        if end < len(text) and text[end] == "%":
            values.append((text[start:end + 1], start, end + 1))
            index = end + 1
        else:
            index = end
    return values


def percentage_values(text):
    return [value for value, _start, _end in percentage_matches(text)]


OWNERSHIP_PHRASES = (
    "continuity of ownership",
    "beneficial own", "beneficial entitled",
    "beneficially own", "beneficially entitled",
    "wholly-owned", "wholly‑owned",
    "majority-owned", "majority‑owned",
)


def _starts_with_ownership_word(text, index):
    for word in ("stake", "subsidiary", "interest"):
        end = index + len(word)
        if text.startswith(word, index) and (end == len(text) or not text[end].isalpha()):
            return True
    return False


def is_ownership_test(text):
    """Identify ownership/control tests without an unbounded numeric regex."""
    lower = text.casefold()
    if any(phrase in lower for phrase in OWNERSHIP_PHRASES):
        return True
    if any("%s %s" % (left, right) in lower
           for left in ("voting", "dividend", "capital", "ownership", "control", "equity")
           for right in ("interest", "right", "stake", "power")):
        return True
    for _value, _start, end in percentage_matches(text):
        # The previous expression recognised ``10% stake`` style tests.  Keep
        # that delimiter rule while allowing the scanner to run in linear time.
        if end < len(text) and text[end] == " " and _starts_with_ownership_word(lower, end + 1):
            return True
    return False

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
    return bool(money_values(body) or percentage_values(body))


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
        with open(p, encoding="utf-8") as source:
            for line in source:
                if not line.strip():
                    continue
                row = json.loads(line)
                text = row.get("text") or ""
                rid = register_id(row["register_id"])
                # Gross-up factors, indexation factors and statutory fractions are
                # bare decimals (2.0802, 1.8868, 0.5), not $ or %, so a filter on
                # currency and percent alone misses them entirely.
                if not (money_values(text) or percentage_values(text) or
                        (FACTOR.search(text) and RATE_PHRASE.search(text))):
                    continue
                base = {
                    "register_id": rid, "act": row["act"],
                    "collection": row.get("collection"),
                    "compilation_number": row.get("compilation_number"),
                    "compilation_date": row.get("compilation_date"),
                    "section": row.get("section"), "heading": row.get("heading"),
                    "register_page": row.get("register_page"),
                    "topic": topic_for(row["act"], row.get("heading"), text),
                }

                for table in table_blocks(text):
                    if len(table) >= 3 and is_rate_table(table):
                        records.append(dict(base, kind="table",
                                            years=sorted({m.group(0) for m in YEAR.finditer(" ".join(table))}),
                                            content="\n".join(table)))

                for sentence in sentences(text):
                    has_money = bool(money_values(sentence))
                    has_pct = bool(percentage_values(sentence))
                    has_factor = bool(FACTOR.search(sentence)) and bool(RATE_PHRASE.search(sentence))
                    if not (has_money or has_pct or has_factor):
                        continue
                    # A percentage stands on its own. Requiring the word "rate"
                    # alongside it dropped s 9-70 of the GST Act — "The amount of
                    # GST on a taxable supply is 10% of the value of the taxable
                    # supply" — which is the operative charging provision for the
                    # whole tax. Dollar amounts still need a phrase, because
                    # legislation is full of incidental sums.
                    if not has_pct and not (RATE_PHRASE.search(sentence)
                                            or THRESHOLD_PHRASE.search(sentence)
                                            or INDEX_PHRASE.search(sentence)):
                        continue
                    kind = ("indexation" if INDEX_PHRASE.search(sentence)
                            else "ownership test" if has_pct and is_ownership_test(sentence)
                            else "rate" if has_pct else
                            "factor" if has_factor and not has_money else "threshold")
                    records.append(dict(base, kind=kind,
                                        amounts=sorted(set(money_values(sentence)) | set(percentage_values(sentence))
                                                       | (set(FACTOR.findall(sentence)) if has_factor else set())),
                                        years=sorted({m.group(0) for m in YEAR.finditer(sentence)}),
                                        content=sentence))

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
