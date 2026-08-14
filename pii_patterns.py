"""PII detection shared by the scans, distribution builder and verifier.

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
import hashlib
import json
import re

# "Smith, John", "SMITH, John" or "John Smith" - two or three name tokens.
# U+2019 is the curly apostrophe the Register's EPUB text actually uses.
_TOKEN = r"[A-Z][A-Za-z'’-]{1,20}"
NAME = re.compile(r"\b%s,?\s+%s(?:\s+%s)?\b" % (_TOKEN, _TOKEN, _TOKEN))
# TPB registration numbers run 8 digits; ABNs 11.
REGNO = re.compile(r"\b\d{8}\b")
# Contact shapes are matched once here so the diagnostic scan and both
# publication gates cannot disagree.  TFNs require eight or nine digits after
# the label; the former one-digit pattern mistook section references such as
# ``tax file number 7/...`` for TFNs.
EMAIL = re.compile(r"\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b")
PHONE = re.compile(
    r"(?<!\d)(?:\(0\d\)\s?\d{4}\s?\d{4}|0[2-8]\s\d{4}\s\d{4}|"
    r"04\d{2}\s\d{3}\s\d{3}|1[38]00\s\d{3}\s\d{3})(?!\d)"
)
TFN = re.compile(
    r"\btax file number\s*:?\s*(\d(?:[ -]?\d){7,8})(?![ -]?\d)", re.I
)
CONTACT_PATTERNS = (("email", EMAIL), ("phone", PHONE), ("tfn", TFN))
_REGISTER_ID = re.compile(r"[A-Z]\d{4}[A-Z]\d{5}\Z")
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
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


def private_person_registration_details(text):
    """Names and 8-digit registration numbers that make a row private-person data."""
    return person_names(text), set(REGNO.findall(text))


def has_private_person_registration_pair(text):
    names, registration_numbers = private_person_registration_details(text)
    return bool(names and registration_numbers)


def _contact_value(kind, match):
    value = match.group(1) if kind == "tfn" else match.group(0)
    if kind == "email":
        return value.strip().casefold()
    return "".join(c for c in value if c.isdigit())


def contact_fingerprints(text):
    """Yield ``(kind, sha256)`` pairs without returning the matched identifier."""
    for kind, pattern in CONTACT_PATTERNS:
        for match in pattern.finditer(text):
            normalised = _contact_value(kind, match)
            yield kind, hashlib.sha256(normalised.encode("utf-8")).hexdigest()


def load_contact_allowlist(path):
    """Load and strictly validate the hashed organisational-contact policy."""
    with open(path, encoding="utf-8") as source:
        document = json.load(source)
    if not isinstance(document, dict) or set(document) != {"schema_version", "entries"}:
        raise ValueError("invalid contact allowlist document")
    if document["schema_version"] != 1 or not isinstance(document["entries"], list):
        raise ValueError("unsupported contact allowlist schema")

    approved = set()
    required = {"kind", "sha256", "register_id", "reason"}
    for entry in document["entries"]:
        if not isinstance(entry, dict) or set(entry) != required:
            raise ValueError("invalid contact allowlist entry")
        kind = entry["kind"]
        digest = entry["sha256"]
        rid = entry["register_id"]
        reason = entry["reason"]
        # A tax file number is never an organisational-contact exception.  It
        # must remain a hard publication failure even if a future policy edit
        # has the right hash and title shape.
        if kind not in {"email", "phone"}:
            raise ValueError("invalid contact allowlist kind")
        if not isinstance(digest, str) or not _SHA256.fullmatch(digest):
            raise ValueError("invalid contact allowlist digest")
        if not isinstance(rid, str) or not _REGISTER_ID.fullmatch(rid):
            raise ValueError("invalid contact allowlist register id")
        if not isinstance(reason, str) or not reason.strip():
            raise ValueError("invalid contact allowlist reason")
        if any(contact_fingerprints(reason)):
            raise ValueError("contact allowlist reason contains a raw identifier")
        key = (kind, digest, rid)
        if key in approved:
            raise ValueError("duplicate contact allowlist entry")
        approved.add(key)
    return approved


def unapproved_contact_fingerprints(text, register_id, approved):
    """Return safe fingerprints for contacts not approved for this title."""
    return {
        (kind, digest, register_id)
        for kind, digest in contact_fingerprints(text)
        if kind == "tfn" or (kind, digest, register_id) not in approved
    }


def unapproved_contact_fingerprints_in_file(path, register_id, approved):
    """Return safe fingerprints from one redistributed UTF-8 text file.

    A title ships both human-readable Markdown and machine-readable JSONL.
    Checking only the JSONL leaves the other published representation outside
    the privacy gate, even though a future extraction change could make those
    representations differ.  Callers deliberately let decoding and I/O errors
    fail closed rather than treating an unreadable file as contact-free.
    """
    _private_pair, unexpected = privacy_findings_in_file(
        path, register_id, approved)
    return unexpected


def privacy_findings_in_file(path, register_id, approved):
    """Return both privacy predicates for one validated UTF-8 text file.

    Strict UTF-8 decoding rejects ordinary binary files.  Some binary payloads
    contain only decodable bytes, so C0/C1 controls that cannot occur in the
    generated Markdown or JSONL are rejected as well.  Tabs, line endings and
    form feeds remain valid source text.
    """
    with open(path, encoding="utf-8") as source:
        text = source.read()
    if any(((ord(character) < 32 and character not in "\t\n\r\f")
            or 127 <= ord(character) <= 159)
           for character in text):
        raise UnicodeError("redistributed file contains binary control bytes")
    return (has_private_person_registration_pair(text),
            unapproved_contact_fingerprints(text, register_id, approved))
