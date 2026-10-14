"""Build and query a local search index over a finished corpus.

``build`` reads a corpus root that ``finalize`` completed: ``sources.json`` and
each listed title's ``markdown/<register_id>/sections.jsonl``. It checks every
row against the title's ``sources.json`` entry before anything is written, so
the index never lends a row provenance its file does not carry:

- ``markdown/`` must hold no title that ``sources.json`` does not list;
- each row must name its own title, carry the ``row_id`` its ordinal implies,
  carry the title's compilation number and date, and be marked
  ``authorised: false``;
- each file must hold exactly the ``jsonl_rows`` that ``sources.json`` records.

The index is one SQLite file holding a full-text table (FTS5, standard library
``sqlite3``), the SHA-256 of every ``sections.jsonl`` it read, the defined terms
found in definitions rows, and the section references found in row text. The
destination must not exist; the file is built beside it and linked into place,
which refuses an existing name.

``query`` ranks rows by BM25 with the heading weighted above the text, after
any row whose section number equals a query word. Each whitespace-separated
query word must appear; a hyphenated word such as ``8-1`` must appear as
adjacent tokens. Text is NFKC-normalised before indexing, so the Register's
non-breaking hyphen and typographic ligatures match plain input.

``define`` looks a term up among paragraphs of rows headed Definitions,
Definition, Dictionary or Interpretation that open ``<term> means``, ``<term>
has the meaning given by``, ``<term> has the same meaning as``, ``<term>
includes``, ``<term> does not include`` or ``<term>: ...``. A term defined any
other way, or in a row with another heading, is not found; an empty result is
not a finding that the term has its ordinary meaning.

``refs`` lists the sections, Divisions and Subdivisions a row cites, and the
rows that cite its section, Division or Subdivision. A citation is ``section``,
``subsection``, ``paragraph`` or ``subparagraph`` followed by a section number,
``Division`` followed by a Division number, or ``Subdivision`` followed by a
numbered Subdivision such as ``20-A``; a list joined by commas, ``and`` or
``or`` gives one reference per item. With no ``of`` or ``in`` after it, or
``of this ...``, it resolves to the citing title. When ``of`` or ``in`` names
another instrument, it resolves only to a title whose name matches exactly; an
abbreviation such as ``ITAA 1997``, ``the Act`` or an instrument outside the
index stays unresolved, with the cited name kept. A row's Division and
Subdivision come from ``structure``.

All of it is a finding aid over the EPUB reading view, not the authorised text.

usage:
  python -m fadden search build CORPUS_ROOT --out INDEX
  python -m fadden search query INDEX WORDS... [--title REGISTER_ID] [--limit N] [--json]
  python -m fadden search define INDEX TERM... [--title REGISTER_ID] [--json]
  python -m fadden search refs INDEX ROW_ID [--json]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
import unicodedata
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, Any, Iterator, Sequence

if TYPE_CHECKING or __package__:
    from .corpus_paths import child, refuse_unlisted_titles, register_id
else:
    from corpus_paths import child, refuse_unlisted_titles, register_id

SCHEMA = "2"
NOTICE = ("Finding aid derived from the Register's EPUB reading view, not authorised "
          "legislation or tax advice. Read the authorised compilation on the Register "
          "before relying on a row.")

DEFINITIONS_HEADING = re.compile(r"(?i)\b(?:definitions?|dictionary|interpretation)\b")
# "In this Act:", "In this Division, unless the contrary intention appears, ..."
LEAD_IN = re.compile(
    r"In (?:this|these) [^:,\n]{1,60}(?:, (?:unless|except) [^:,\n]{1,60})?[,:]\s+")
VERB = re.compile(
    r"(?P<term>[^\n]{1,150}?)\s+(?:means|has the meaning given by|has the same meaning as"
    r"|has the meaning|includes|does not include)\b")
COLON = re.compile(r"(?P<term>[^:\n(]{1,80}):\s+\S")
QUALIFIER = re.compile(r",\s+(?:in relation to|in respect of|for the purposes of)\b.*\Z", re.S)
# A continuation ("but does not include ...") or a note is not a defined term.
NOT_A_TERM = re.compile(r"(?i)(?:note|example|however|for example|in this|but|and|or|other than)\b")
MAX_TERM_WORDS = 15

_SECTION = r"\d+[A-Z]{0,4}(?:[-\u2010\u2011]\d+[A-Z]{0,4})?"
# An instrument named after "of" or "in": a title ("... Act 1997", "...
# Regulations"), an abbreviation with a year ("ITAA 1997") or "the Act".
_YEAR = r"(?:18|19|20)\d{2}\b"
_CITED = (r"(?:the\s+)?\*?([A-Z][A-Za-z]*(?:\s+[A-Z][A-Za-z]*){0,3}\s+" + _YEAR
          + r"|(?=[A-Z])[^.;:,\n]{0,150}?\b(?:Act|Regulations?|Rules)\b(?:\s+" + _YEAR + ")?)")
# A section, a Division ("40", "83A") or a numbered Subdivision ("20-A"), with
# any subsection and paragraph brackets after it.
_LABEL = r"\d+[A-Z]{0,4}(?:[-\u2010\u2011](?:\d+[A-Z]{0,4}|[A-Z]{1,3}\b))?(?!\.\d)(?:\([0-9A-Za-z]{1,6}\))*"
_LIST_SEPARATOR = r"(?:,?\s+(?:and|or)\s+|,\s+)"
CITATION = re.compile(
    r"\b(?P<keyword>(?i:(?:sub)?(?:section|paragraph|subparagraph)|subdivision|division))s?\s+"
    r"(?P<labels>" + _LABEL + "(?:" + _LIST_SEPARATOR + _LABEL + r")*)"
    r"(?:\s+(?:of|in)\s+(?:Schedule\s+\d+[A-Z]?\s+to\s+" + _CITED
    + r"|this\s+\w+|Schedule\s+\d+[A-Z]?\b|" + _CITED + r"))?")
LIST_SEPARATOR = re.compile(_LIST_SEPARATOR)
SECTION = re.compile(_SECTION)
_SUBDIVISION = r"\d+[A-Z]{0,4}[-\u2010\u2011][A-Z]{1,3}\b"
SUBDIVISION = re.compile(_SUBDIVISION)
MARKDOWN_HEADING = re.compile(r"^#{1,6} (.+)$", re.M)
# A heading that opens a level of structure. "Division 6.1" opens a Division
# this index does not label, so it clears the Division rather than naming 6.
LEVEL = re.compile(r"(Chapter|Part|Schedule|Division|Subdivision)\b(?:\s+("
                   + _SUBDIVISION + "|" + _SECTION + r")(?![.\w]))?")
HYPHENS = str.maketrans({"\u2010": "-", "\u2011": "-"})


class SearchIndexError(ValueError):
    """The corpus or the index failed a check; nothing was written."""


def normalise(text: str) -> str:
    """Fold compatibility forms (ligatures, the non-breaking hyphen) for matching."""
    return unicodedata.normalize("NFKC", text)


def term_key(term: str) -> str:
    """Lookup key for a defined term: folded, unstarred, single-spaced, plain hyphens."""
    folded = normalise(term).translate(HYPHENS).replace("*", "").casefold()
    return " ".join(folded.split())


def section_key(section: str) -> str:
    return normalise(section).translate(HYPHENS).replace(" ", "")


def _definition_start(paragraph: str) -> str | None:
    paragraph = LEAD_IN.sub("", paragraph, count=1)
    match = VERB.match(paragraph) or COLON.match(paragraph)
    if not match:
        return None
    term = QUALIFIER.sub("", match["term"]).replace("*", "").strip(" \t\"'\u2018\u2019\u201c\u201d")
    if not term or NOT_A_TERM.match(term) or len(term.split()) > MAX_TERM_WORDS:
        return None
    return term


def definitions(text: str) -> list[tuple[str, str]]:
    """Split a definitions row into (term, definition text) pairs.

    A definition runs from its opening paragraph through following list items,
    notes and plain continuation paragraphs, and ends at the next definition or
    at a numbered subsection such as ``(2)``.
    """
    found: list[tuple[str, list[str]]] = []
    current: list[str] | None = None
    for paragraph in text.split("\n\n"):
        paragraph = paragraph.strip()
        if not paragraph:
            continue
        if paragraph.startswith(("- ", ">")):
            if current is not None:
                current.append(paragraph)
            continue
        if paragraph.startswith("("):
            current = None
            continue
        term = _definition_start(paragraph)
        if term is not None:
            current = [paragraph]
            found.append((term, current))
        elif current is not None:
            current.append(paragraph)
    return [(term, "\n\n".join(body)) for term, body in found]


def citations(text: str) -> Iterator[tuple[str, str, str | None, str]]:
    """Yield (kind, key, cited act or None, citation text) for each reference.

    *kind* is ``section``, ``division`` or ``subdivision``. A list ("sections
    8-1, 8-5 and 8-10") yields one reference per item, all naming the
    instrument after the last item. The list ends at an item numbered unlike
    the first (hyphenated or not). A range ("sections 8-1 to 8-10") yields
    only its first item. A Subdivision named by letter alone ("Subdivision B")
    is not read: the same letter recurs in many Divisions of one title.
    """
    for match in CITATION.finditer(text):
        keyword = match["keyword"].casefold()
        kind = keyword if keyword in ("division", "subdivision") else "section"
        act = next((name for name in match.groups()[2:] if name), None)
        act = " ".join(act.split()) if act else None
        citation = " ".join(match.group(0).split())
        hyphenated = None
        for label in LIST_SEPARATOR.split(match["labels"]):
            key = section_key(label.split("(", 1)[0])
            # "section 8-1 and 2 other provisions": a later item numbered unlike
            # the first is prose, so the list ends before it.
            if hyphenated is None:
                hyphenated = "-" in key
            elif hyphenated != ("-" in key):
                break
            if SUBDIVISION.fullmatch(key):
                fits = kind == "subdivision" and "(" not in label
            else:
                fits = kind == "section" or (kind == "division" and not re.search("[-(]", key + label))
            if fits:
                yield kind, key, act, citation


def structure(rows: list[dict[str, Any]],
              markdown: str) -> Iterator[tuple[str | None, str | None]]:
    """Yield the (Division, Subdivision) each row of one title sits in.

    A row's ``container`` names only its innermost heading, so the title's own
    Markdown supplies the hierarchy. Its headings are walked in order: a
    ``Chapter``, ``Part`` or ``Schedule`` heading clears both levels, a
    ``Division`` heading sets the Division and clears the Subdivision, and a
    ``Subdivision`` heading sets a numbered Subdivision such as ``20-A`` (a
    lettered one clears it, since letters recur across Divisions). Each row
    takes the place of the next heading equal to its own; a row with no such
    heading, such as ``Introductory material``, gets neither.
    """
    places = []
    division = subdivision = None
    for text in MARKDOWN_HEADING.findall(markdown):
        heading = " ".join(normalise(text).split())
        found = LEVEL.match(heading)
        if found:
            level, label = found[1], section_key(found[2]) if found[2] else None
            if level == "Division":
                division, subdivision = label, None
            elif level == "Subdivision":
                subdivision = label if label and SUBDIVISION.fullmatch(label) else None
            else:
                division = subdivision = None
        places.append((heading, division, subdivision))
    position = 0
    for row in rows:
        heading = " ".join(normalise(row.get("heading") or "").split())
        for index in range(position, len(places)):
            if places[index][0] == heading:
                position = index + 1
                yield places[index][1], places[index][2]
                break
        else:
            yield None, None


def _read_bytes(root: str, rid: str, name: str) -> bytes:
    try:
        return Path(child(root, "markdown", rid, name)).read_bytes()
    except OSError as error:
        raise SearchIndexError(f"{rid}: cannot read {name} ({error.strerror})") from None


def _read_title(root: str, title: dict[str, Any]) -> dict[str, Any]:
    rid = register_id(title.get("register_id"))
    data = _read_bytes(root, rid, "sections.jsonl")
    markdown = _read_bytes(root, rid, rid + ".md")
    try:
        lines = data.decode("utf-8").splitlines()
        markdown_text = markdown.decode("utf-8")
    except UnicodeDecodeError:
        raise SearchIndexError(f"{rid}: sections.jsonl or {rid}.md is not UTF-8") from None
    rows: list[dict[str, Any]] = []
    for number, line in enumerate(lines, 1):
        if not line.strip():
            continue
        where = f"{rid}/sections.jsonl:{number}"
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            raise SearchIndexError(f"{where} is not JSON") from None
        if not isinstance(row, dict) or not isinstance(row.get("text"), str):
            raise SearchIndexError(f"{where} is not a sections.jsonl row")
        ordinal = len(rows) + 1
        expected = "%s:%04d:%s" % (rid, ordinal, row.get("section") or "-")
        if row.get("register_id") != rid:
            raise SearchIndexError(f"{where} names another title")
        if row.get("row_id") != expected:
            raise SearchIndexError(f"{where} has row_id {row.get('row_id')!r}, expected {expected!r}")
        for field in ("compilation_number", "compilation_date"):
            if row.get(field) != title.get(field):
                raise SearchIndexError(f"{where} {field} differs from sources.json")
        if row.get("authorised") is not False:
            raise SearchIndexError(f"{where} is not marked authorised: false")
        rows.append(row)
    if len(rows) != title.get("jsonl_rows"):
        raise SearchIndexError(
            f"{rid}: sections.jsonl holds {len(rows)} rows; sources.json records "
            f"{title.get('jsonl_rows')}")
    return {"title": title, "rid": rid, "rows": rows, "markdown": markdown_text,
            "sections_sha256": hashlib.sha256(data).hexdigest(),
            "markdown_sha256": hashlib.sha256(markdown).hexdigest()}


_SCHEMA_SQL = """
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE titles (
  register_id TEXT PRIMARY KEY, name TEXT NOT NULL, collection TEXT,
  compilation_number TEXT, compilation_date TEXT, version_is_current INTEGER NOT NULL,
  retrieved TEXT, register_page TEXT, source_url TEXT, rows INTEGER NOT NULL,
  sections_sha256 TEXT NOT NULL, markdown_sha256 TEXT NOT NULL);
CREATE TABLE rows (
  id INTEGER PRIMARY KEY, row_id TEXT NOT NULL UNIQUE,
  register_id TEXT NOT NULL REFERENCES titles, section TEXT, section_key TEXT,
  division_key TEXT, subdivision_key TEXT, heading TEXT, container TEXT, text TEXT NOT NULL);
CREATE INDEX rows_section ON rows (register_id, section_key);
CREATE INDEX rows_division ON rows (register_id, division_key);
CREATE INDEX rows_subdivision ON rows (register_id, subdivision_key);
CREATE VIRTUAL TABLE rows_fts USING fts5 (
  heading, text, content='rows', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2');
CREATE TABLE definitions (
  term TEXT NOT NULL, term_key TEXT NOT NULL, row_id TEXT NOT NULL REFERENCES rows (row_id),
  position INTEGER NOT NULL, text TEXT NOT NULL);
CREATE INDEX definitions_key ON definitions (term_key);
CREATE TABLE refs (
  from_row_id TEXT NOT NULL REFERENCES rows (row_id), kind TEXT NOT NULL,
  to_register_id TEXT, to_key TEXT NOT NULL, cited_act TEXT, citation TEXT NOT NULL);
CREATE INDEX refs_to ON refs (to_register_id, kind, to_key);
CREATE INDEX refs_from ON refs (from_row_id);
"""


def build(root: str, out: str) -> dict[str, int]:
    """Check the corpus at *root* and write a new index file at *out*."""
    destination = Path(out)
    if os.path.lexists(destination):
        raise SearchIndexError("the index destination must not already exist")
    if not destination.parent.is_dir():
        raise SearchIndexError("the index destination's parent directory does not exist")
    try:
        sources_bytes = Path(child(root, "sources.json")).read_bytes()
        sources = json.loads(sources_bytes)
        listed = sources["titles"]
        if not isinstance(listed, list) or not all(isinstance(t, dict) for t in listed):
            raise TypeError
    except OSError as error:
        raise SearchIndexError(f"cannot read sources.json ({error.strerror})") from None
    except (ValueError, KeyError, TypeError):
        raise SearchIndexError("sources.json is not a finished corpus inventory") from None
    try:
        refuse_unlisted_titles(root, {register_id(t.get("register_id")) for t in listed})
    except RuntimeError as error:
        raise SearchIndexError(str(error)) from None
    read = [_read_title(root, title) for title in listed]
    names: dict[str, str | None] = {}
    for entry in read:
        # Two titles sharing a name leave a citation of that name unresolved.
        name = entry["title"]["name"]
        names[name] = None if name in names else entry["rid"]

    temporary = destination.with_name(f".{destination.name}.{uuid.uuid4().hex}.tmp")
    counts = {"titles": len(read), "rows": 0, "definitions": 0, "refs": 0}
    connection = sqlite3.connect(temporary)
    try:
        with connection:
            connection.executescript(_SCHEMA_SQL)
            for entry in read:
                title, rid, rows = entry["title"], entry["rid"], entry["rows"]
                connection.execute(
                    "INSERT INTO titles VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                    (rid, title["name"], title.get("collection"), title.get("compilation_number"),
                     title.get("compilation_date"), int(title.get("version_is_current", True)),
                     title.get("retrieved"), title.get("register_page"), title.get("source_url"),
                     len(rows), entry["sections_sha256"], entry["markdown_sha256"]))
                for row, place in zip(rows, structure(rows, entry["markdown"])):
                    _insert_row(connection, rid, row, place, names, counts)
            connection.executemany("INSERT INTO meta VALUES (?, ?)", [
                ("schema", SCHEMA),
                ("corpus_retrieved", str(sources.get("retrieved"))),
                ("sources_sha256", hashlib.sha256(sources_bytes).hexdigest()),
                *((f"{name}_count", str(value)) for name, value in counts.items()),
            ])
        connection.close()
        os.link(temporary, destination)
    except BaseException:
        connection.close()
        temporary.unlink(missing_ok=True)
        raise
    temporary.unlink()
    return counts


def _insert_row(connection: sqlite3.Connection, rid: str, row: dict[str, Any],
                place: tuple[str | None, str | None], names: dict[str, str | None],
                counts: dict[str, int]) -> None:
    section = row.get("section")
    cursor = connection.execute(
        "INSERT INTO rows (row_id, register_id, section, section_key, division_key,"
        " subdivision_key, heading, container, text) VALUES (?,?,?,?,?,?,?,?,?)",
        (row["row_id"], rid, section, section_key(section) if section else None, *place,
         row.get("heading"), row.get("container"), row["text"]))
    # The full-text table reads its snippets from rows (the Register's text) but
    # indexes the NFKC form, so both spellings of a ligature or hyphen match.
    connection.execute("INSERT INTO rows_fts (rowid, heading, text) VALUES (?,?,?)",
                       (cursor.lastrowid, normalise(row.get("heading") or ""),
                        normalise(row["text"])))
    counts["rows"] += 1
    if DEFINITIONS_HEADING.search(row.get("heading") or ""):
        for position, (term, body) in enumerate(definitions(row["text"]), 1):
            connection.execute("INSERT INTO definitions VALUES (?,?,?,?,?)",
                               (term, term_key(term), row["row_id"], position, body))
            counts["definitions"] += 1
    for kind, cited, act, text in citations(row["text"]):
        target = rid if act is None else names.get(act)
        connection.execute("INSERT INTO refs VALUES (?,?,?,?,?,?)",
                           (row["row_id"], kind, target, cited, act, text))
        counts["refs"] += 1


def open_index(path: str) -> sqlite3.Connection:
    """Open an index read-only and check its schema."""
    if not Path(path).is_file():
        raise SearchIndexError("the index file does not exist")
    connection = sqlite3.connect(Path(path).resolve().as_uri() + "?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        found = connection.execute("SELECT value FROM meta WHERE key = 'schema'").fetchone()
    except sqlite3.DatabaseError:
        connection.close()
        raise SearchIndexError("the file is not a search index") from None
    if found is None or found["value"] != SCHEMA:
        connection.close()
        raise SearchIndexError("the index was built by a different schema version")
    return connection


def match_expression(words: Sequence[str]) -> str:
    """Quote each query word as an FTS5 phrase so no input is read as syntax."""
    phrases = []
    for word in words:
        tokens = re.findall(r"\w+", normalise(word))
        if tokens:
            phrases.append('"%s"' % " ".join(tokens))
    if not phrases:
        raise SearchIndexError("the query has no searchable words")
    return " ".join(phrases)


_PROVENANCE = ("t.name AS act, t.collection, t.compilation_number, t.compilation_date, "
               "t.version_is_current, t.retrieved, t.register_page, t.source_url, "
               "t.sections_sha256")


def _hit(row: sqlite3.Row) -> dict[str, Any]:
    hit = dict(row)
    hit["version_is_current"] = bool(hit["version_is_current"])
    hit["authorised"] = False
    return hit


def query(connection: sqlite3.Connection, words: Sequence[str], *, title: str | None = None,
          limit: int = 10) -> list[dict[str, Any]]:
    sql = ("SELECT r.row_id, r.register_id, r.section, r.heading, r.container, r.text, "
           f"{_PROVENANCE}, snippet(rows_fts, 1, '[', ']', '...', 16) AS snippet "
           "FROM rows_fts JOIN rows r ON r.id = rows_fts.rowid "
           "JOIN titles t ON t.register_id = r.register_id WHERE rows_fts MATCH ?")
    params: list[Any] = [match_expression(words)]
    if title:
        sql += " AND r.register_id = ?"
        params.append(register_id(title))
    sections = sorted({section_key(word) for word in words if SECTION.fullmatch(normalise(word))})
    sql += " ORDER BY r.section_key IN (%s) DESC, bm25(rows_fts, 4.0, 1.0), r.id LIMIT ?" % (
        ",".join("?" * len(sections)))
    params += [*sections, limit]
    return [_hit(row) for row in connection.execute(sql, params)]


def define(connection: sqlite3.Connection, term: str, *,
           title: str | None = None) -> list[dict[str, Any]]:
    """Exact matches for *term*; failing those, terms that begin with it."""
    key = term_key(term)
    if not key:
        raise SearchIndexError("the term is empty")
    sql = ("SELECT d.term, d.text AS definition, r.row_id, r.register_id, r.section, r.heading, "
           f"{_PROVENANCE} FROM definitions d JOIN rows r ON r.row_id = d.row_id "
           "JOIN titles t ON t.register_id = r.register_id WHERE {} ")
    params: list[Any] = []
    if title:
        sql += "AND r.register_id = ? "
        params.append(register_id(title))
    sql += "ORDER BY t.version_is_current DESC, t.name, r.id, d.position"
    exact = connection.execute(sql.format("d.term_key = ?"), [key, *params]).fetchall()
    if exact:
        return [_hit(row) for row in exact]
    prefix = key.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + " %"
    return [_hit(row) for row in connection.execute(
        sql.format("d.term_key LIKE ? ESCAPE '\\'"), [prefix, *params])]


_TARGET_COLUMN = {"section": "section_key", "division": "division_key",
                  "subdivision": "subdivision_key"}


def refs(connection: sqlite3.Connection, row_id: str) -> dict[str, Any]:
    """What *row_id* cites, and rows that cite its section, Division or Subdivision."""
    row = connection.execute("SELECT * FROM rows WHERE row_id = ?", (row_id,)).fetchone()
    if row is None:
        raise SearchIndexError(f"{row_id} is not in the index")
    outbound = []
    for ref in connection.execute(
            "SELECT * FROM refs WHERE from_row_id = ? ORDER BY rowid", (row_id,)):
        targets = [] if ref["to_register_id"] is None else [
            dict(target) for target in connection.execute(
                "SELECT row_id, heading, container FROM rows WHERE register_id = ? AND "
                f"{_TARGET_COLUMN[ref['kind']]} = ? ORDER BY id",
                (ref["to_register_id"], ref["to_key"]))]
        outbound.append({"kind": ref["kind"], "key": ref["to_key"],
                         "citation": ref["citation"], "cited_act": ref["cited_act"],
                         "register_id": ref["to_register_id"], "rows": targets})
    inbound = [dict(source) for source in connection.execute(
        "SELECT DISTINCT r.row_id, r.heading, f.kind, f.citation FROM refs f "
        "JOIN rows r ON r.row_id = f.from_row_id WHERE f.to_register_id = ? AND r.row_id != ? "
        "AND ((f.kind = 'section' AND f.to_key = ?) OR (f.kind = 'division' AND f.to_key = ?) "
        "OR (f.kind = 'subdivision' AND f.to_key = ?)) ORDER BY r.id",
        (row["register_id"], row_id, row["section_key"], row["division_key"],
         row["subdivision_key"]))]
    return {"row_id": row_id, "register_id": row["register_id"], "section": row["section"],
            "division": row["division_key"], "subdivision": row["subdivision_key"],
            "heading": row["heading"], "cites": outbound, "cited_by": inbound}


def _source_line(connection: sqlite3.Connection) -> str:
    meta = dict(connection.execute("SELECT key, value FROM meta").fetchall())
    return (f"Index of the corpus retrieved {meta.get('corpus_retrieved')}, sources.json "
            f"sha256 {meta.get('sources_sha256', '')[:16]}. {NOTICE}")


def _describe(hit: dict[str, Any]) -> str:
    state = "current" if hit["version_is_current"] else "not current"
    return (f"{hit['row_id']}, compilation {hit['compilation_number']} "
            f"({hit['compilation_date']}), {state}, retrieved {hit['retrieved']}\n"
            f"   {hit['register_page']}")


def _print_json(items: Sequence[dict[str, Any]]) -> None:
    for item in items:
        print(json.dumps(item, ensure_ascii=False))


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden search",
        description="Build and query a local full-text, definitions and reference index.")
    commands = parser.add_subparsers(dest="command", required=True)
    build_parser = commands.add_parser("build", help="index a finished corpus root")
    build_parser.add_argument("corpus_root")
    build_parser.add_argument("--out", required=True, help="new index file; must not exist")
    query_parser = commands.add_parser("query", help="rank rows that contain every word")
    query_parser.add_argument("index")
    query_parser.add_argument("words", nargs="+")
    query_parser.add_argument("--title", help="search only this register id")
    query_parser.add_argument("--limit", type=int, default=10, choices=range(1, 101),
                              metavar="1-100")
    query_parser.add_argument("--json", action="store_true", help="print one object per hit")
    define_parser = commands.add_parser("define", help="look up a defined term")
    define_parser.add_argument("index")
    define_parser.add_argument("term", nargs="+")
    define_parser.add_argument("--title", help="look only in this register id")
    define_parser.add_argument("--json", action="store_true")
    refs_parser = commands.add_parser("refs", help="sections a row cites and rows citing it")
    refs_parser.add_argument("index")
    refs_parser.add_argument("row_id")
    refs_parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    try:
        if args.command == "build":
            counts = build(args.corpus_root, args.out)
            print("indexed {titles} titles, {rows} rows, {definitions} definitions and "
                  "{refs} section references".format(**counts))
            return 0
        connection = open_index(args.index)
        try:
            return _run(connection, args)
        finally:
            connection.close()
    except (SearchIndexError, ValueError, OSError, sqlite3.Error) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


def _run(connection: sqlite3.Connection, args: argparse.Namespace) -> int:
    if args.command == "query":
        hits = query(connection, args.words, title=args.title, limit=args.limit)
        if args.json:
            _print_json(hits)
            return 0
        for number, hit in enumerate(hits, 1):
            print(f"{number}. {hit['act']}: {hit['heading']}")
            print(f"   {_describe(hit)}")
            print(f"   {' '.join(hit['snippet'].split())}")
        print(f"{len(hits)} rows. {_source_line(connection)}")
        return 0
    if args.command == "define":
        found = define(connection, " ".join(args.term), title=args.title)
        if args.json:
            _print_json(found)
            return 0
        for item in found:
            print(f"{item['term']} ({item['act']}, {item['heading']})")
            print(f"   {_describe(item)}")
            print("   " + item["definition"].replace("\n\n", "\n   "))
        if not found:
            print("No definition paragraph found. The term may be defined another way or "
                  "in a row this index does not read as definitions; this is not a finding "
                  "that it has its ordinary meaning.")
        print(_source_line(connection))
        return 0
    report = refs(connection, args.row_id)
    if args.json:
        _print_json([report])
        return 0
    print(f"{report['row_id']}: {report['heading']}")
    for ref in report["cites"]:
        shown = [target["row_id"] for target in ref["rows"][:5]]
        if len(ref["rows"]) > 5:
            shown.append(f"and {len(ref['rows']) - 5} more")
        where = ", ".join(shown) or (
            "not in this index" if ref["register_id"] is None else f"no row for that {ref['kind']}")
        print(f"  cites {ref['kind']} {ref['key']} ({ref['citation']}): {where}")
    for source in report["cited_by"]:
        print(f"  cited by {source['row_id']} ({source['heading']}): {source['citation']}")
    print(_source_line(connection))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
