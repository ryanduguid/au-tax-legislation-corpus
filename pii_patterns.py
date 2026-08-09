"""Person-name detection shared by the PII scans and the distribution verifier.

Both scans and dist_verify.py hunt the same shape: several personal names in a
row alongside agent registration numbers. The patterns live here once so the
gates cannot drift apart.

A name token accepts an uppercase run ("SMITH"), an internal capital
("McDonald") and apostrophes or hyphens ("O'Brien", "Baker-Finch"); the old
Capitalised-lowercase pair missed all of these. The statutory word list then
removes legislative vocabulary. It lists capitalised forms, so an all-caps
candidate ("TAXATION OFFICE") would sail past a case-sensitive check; those
candidates are checked case-insensitively as well.
"""
import re

# "Smith, John", "SMITH, John" or "John Smith" - two or three name tokens.
# U+2019 is the curly apostrophe the Register's EPUB text actually uses.
_TOKEN = r"[A-Z][A-Za-z'’-]{1,20}"
NAME = re.compile(r"\b%s,?\s+%s(?:\s+%s)?\b" % (_TOKEN, _TOKEN, _TOKEN))
# TPB registration numbers run 8 digits; ABNs 11.
REGNO = re.compile(r"\b\d{8}\b")
_STATUTORY_WORDS = (
    r"\b(Act|Regulation|Schedule|Division|Subdivision|Part|Chapter|Section|"
    r"Commissioner|Minister|Treasurer|Commonwealth|Australian|Australia|Board|"
    r"Tax|Taxation|Income|Superannuation|Court|Tribunal|Determination|Notice|"
    r"Instrument|Amendment|January|February|March|April|May|June|July|August|"
    r"September|October|November|December)\b")
STATUTORY = re.compile(_STATUTORY_WORDS)
_STATUTORY_CI = re.compile(_STATUTORY_WORDS, re.I)


def is_statutory(candidate):
    """True when a NAME match is legislative vocabulary, not a person."""
    if STATUTORY.search(candidate):
        return True
    # An all-caps token never matches the capitalised word list above, so
    # check those candidates against it case-insensitively as well.
    if any(len(t) > 1 and t.isupper()
           for t in re.split(r"[^A-Za-z]+", candidate)):
        return bool(_STATUTORY_CI.search(candidate))
    return False


def person_names(text):
    """The set of NAME matches in *text* that survive the statutory filter."""
    return {m.group(0) for m in NAME.finditer(text)
            if not is_statutory(m.group(0))}
