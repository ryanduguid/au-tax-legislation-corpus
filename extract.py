"""Stage 4: EPUB -> markdown + per-section JSONL.

The Register's EPUBs are Word-generated XHTML with stable class names:
  ActHead1..ActHead5  Chapter / Part / Division / Subdivision / Section
  CharSectno          the digits of a section number
  subsection, paragraph, notetext, Tabletext ...
  TOC*, TofSects*     table of contents, skipped
  ENote*, TableOfActs*, TableOfAmend  endnotes apparatus, split out

Two templates exist. Acts predating ActHead (e.g. the Superannuation
(Distribution of Surplus) Act 1974) carry no structural headings at all and
mark sections only with a CharSectno span.

Section numbers render as `40 <U+2011> 1`: digits joined by a non-breaking
hyphen padded with non-breaking spaces, so any regex must allow whitespace
around the dash.

Table cells wrap their content in <p>, so cell text must be accumulated in a
buffer that survives a nested paragraph.
"""
import datetime, html, json, os, re, sys, zipfile
from html.parser import HTMLParser

from corpus_paths import child, corpus_root, register_id

SKIP_CLASS = re.compile(r'^(TOC\d|TofSects|Contents|Header|Footer)', re.I)
# Endnotes apparatus. The trigger must be narrow: CompiledActNo and UpdateDate
# also appear in the FRONT matter, so keying on them flips the whole Act into
# endnotes at line one.
ENDNOTE_CLASS = re.compile(r'^(ENote|TableOfActs|TableOfAmend|LI-Endnote)', re.I)
# Bare-text triggers are dangerous: a stray "Endnotes" paragraph in front
# matter routes an entire Act into its endnotes file. Only fire these once the
# body has started.
ENDNOTE_TEXT = re.compile(r'^(endnotes|about the endnotes|notes to the .+ act \d{4})\s*$', re.I)
HEAD_LEVEL = {"ActHead1": 2, "ActHead2": 2, "ActHead3": 3,
              "ActHead4": 4, "ActHead5": 5,
              # The standard modern legislative-instrument template. Heading1 is
              # the Part, Heading2 the section ("4 Definitions"). Heading3 and
              # below are sub-headings inside a section: left unmapped on purpose
              # so they stay body text instead of splitting the section they
              # belong to.
              "LI-Heading1": 2, "LI-Heading2": 5}
# Accounting standards registered as legislative instruments (AASB 112 Income
# Taxes and friends) use the IASB template. Their headings carry no number, so
# they land as unnumbered rows: still far better than one 29,000-word blob.
# Matched by prefix because the suffix encodes indentation, not depth
# (SectionTitle2Ind and SectionTitle2NonInd are the same level).
IASB_HEAD = re.compile(r'^(?:IASB|AASB|Conv)SectionTitle([1-4])', re.I)
# Section numbers run to eight trailing letters (8AAZLGA) and the dashed form
# can carry an alphabetic prefix. Capping at four silently nulled 98 real ids.
SECNO = re.compile(r'(\d{1,3}[A-Z]{0,4})\s*\u2011\s*(\d{1,4}[A-Z]{0,8})')
PLAIN_SECNO = re.compile(r'^\s*(\d{1,4}[A-Z]{0,8})(?=[.\s\u00a0\u2011]|$)')
FRONT_CLASS = re.compile(r'^(LongT|Preamble)', re.I)
# Body classes that appear BEFORE the first ActHead in some Acts. Without these
# the Customs Tariff Act's whole USER'S GUIDE is discarded. Unclassed
# compilation cover text is deliberately excluded.
PREBODY_CLASS = re.compile(r'^(subsection|paragraph|notetext|Preamble|SOText|noteToPara)', re.I)
# Paragraph-level body classes get a list marker so (a), (b), (i) keep their
# shape in markdown.
PARA_CLASS = re.compile(r'^(paragraph|paragraphsub|parabullet|LI-BodyTextSubpara|'
                        r'LI-BodyTextParaa|LI-Sectionparaa)', re.I)

# A third EPUB template. Many legislative instruments are built from a plain
# Word file with no class attributes and no CharSectno span, so neither of the
# other two detectors fires and the whole instrument collapses into one chunk.
# Headings there are bare paragraphs: "3 Definition", "5 Prescribed courses".
# Must end on a letter or bracket, otherwise "1 October 2016" reads as a
# heading. Only ever used when the document has no classed markup at all.
# Some number their headings "1." and some "1", so the period is optional. The
# heading itself may not contain one, which is what keeps a numbered sentence
# ("3. This determination applies to...") out.
# Instrument headings run long ("10. When must the valuation under methods 1 to
# 3 be made?") and some end on a digit ("... for the purposes of Division 75"),
# so neither length nor the closing character can be used to exclude body text.
# What does the work: no period or semicolon anywhere inside, and no trailing
# comma. A list item almost always ends on "." or ";".
BARE_SECHEAD = re.compile(r'^\s*(\d{1,3}[A-Z]{0,3})\.?\s+([A-Z][^.;]{2,140}[^\s.;,:])\s*$')
# Without this "1 October 2016" on a line of its own parses as section 1.
BARE_DATEISH = re.compile(
    r'^\s*\d{1,3}\.?\s+(January|February|March|April|May|June|July|August|'
    r'September|October|November|December)\b', re.I)
# Word carries these through from the authoring template. They say nothing about
# structure, so they must not be read as evidence that a document is marked up.
COSMETIC_CLASS = re.compile(
    r'^(Header|Footer|ListParagraph|Normal|MsoNormal|BodyText|Default|TOC\d)', re.I)
BARE_CONTAINER = re.compile(
    r'^\s*(?:Schedule|Part|Division|Chapter)\s+\d{1,3}[A-Z]{0,3}\b.{0,90}$')
HTAGS = {'h1', 'h2', 'h3', 'h4', 'h5', 'h6'}
# The Coat of Arms occupies a small fixed footprint. Keying on byte size instead
# deleted a GST decision flowchart and a maintenance-income formula outright.
COAT_MAX_PX = 200
COAT_MIN_BYTES = 5000
COAT_MIN_RATIO = 1.15
COAT_MAX_RATIO = 1.60

LICENCE = "CC BY 4.0"
LICENCE_URL = "https://creativecommons.org/licenses/by/4.0/"


def attribution(retrieved):
    """The Register requires 'Based on content from' wording for CHANGED
    material. The markdown is converted and has its contents page removed, so
    it is changed; only the raw EPUBs may use the 'Sourced from' wording."""
    return ("Based on content from the Federal Register of Legislation at %s. "
            "For the latest information on Australian Government legislation "
            "please go to https://www.legislation.gov.au. Changes: converted "
            "from EPUB to markdown, contents pages removed, endnotes separated, "
            "images replaced by their descriptive alt text, compilation cover pages "
            "and running headers omitted."
            % retrieved)


class Doc(HTMLParser):
    def __init__(self, img_sizes=None):
        super().__init__(convert_charrefs=True)
        self.img_sizes = img_sizes or {}
        self._colspan = 1
        self.blocks = []
        self._cls = None
        self._buf = []
        self._in_p = False
        self._sectno = False
        self._in_td = False
        self._cell = []
        self._row = []
        self._row_raw = []
        self._table = []
        self._table_raw = []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        # Some Acts mark headings with h1-h6 rather than p.ActHeadN. Treating
        # them as paragraphs lets the same class and CharSectno logic apply.
        if tag == "p" or tag == "li" or tag in HTAGS:
            self._flush()
            self._in_p = True
            self._cls = a.get("class", "")
            self._sectno = False
            self._buf = []
        elif tag == "span" and "CharSectno" in a.get("class", ""):
            self._sectno = True
        elif tag == "td":
            self._in_td = True
            self._cell = []
            self._buf = []
            try:
                self._colspan = max(1, int(a.get("colspan", 1)))
            except ValueError:
                self._colspan = 1
        elif tag == "table":
            self._table = []
            self._table_raw = []
        elif tag == "br":
            self._buf.append(" ")
        elif tag == "img":
            alt = (a.get("alt") or "").strip()
            src = os.path.basename(a.get("src") or "")

            def px(name):
                try:
                    return int(re.sub(r"[^\d]", "", a.get(name) or "0") or 0)
                except ValueError:
                    return 0

            # Identifying the Coat of Arms on any single signal has now failed
            # twice: a byte floor alone deleted a decision flowchart, pixel
            # bounds alone deleted 19 statutory formulas. Every genuine Arms
            # blob is small AND heavy AND close to 1.3:1; the formulas fail at
            # least one of those. Require all three.
            w, h, b = px("width"), px("height"), self.img_sizes.get(src, 0)
            ratio = (w / h) if h else 0
            is_arms = ("coat of arms" in alt.lower()
                       or (not alt and 0 < w < COAT_MAX_PX and 0 < h < COAT_MAX_PX
                           and b >= COAT_MIN_BYTES and COAT_MIN_RATIO < ratio < COAT_MAX_RATIO))
            # Nothing is ever discarded silently. A dropped image leaves a mark.
            text = ("[Commonwealth Coat of Arms omitted, not licensed under CC BY]"
                    if is_arms else
                    alt or "[image not described in source: %s]" % src)
            # An image inside a cell belongs to that cell, not stacked above the
            # table with its row association lost.
            if self._in_td:
                self._cell.append("> Figure: " + text)
            else:
                self.blocks.append({"k": "img", "src": src, "alt": text})

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)

    def handle_endtag(self, tag):
        if (tag == "p" or tag == "li" or tag in HTAGS) and self._in_p:
            self._flush()
        elif tag == "td" and self._in_td:
            self._flush()
            cell = " ".join(x for x in self._cell if x).strip()
            self._row_raw.append(cell)
            self._row.append(cell)
            # A spanned cell occupies several logical columns; without the
            # filler every later cell in the row shifts left.
            self._row.extend([""] * (self._colspan - 1))
            self._colspan = 1
            self._cell = []
            self._in_td = False
        elif tag == "tr":
            self._table.append(list(self._row))
            self._table_raw.append(list(self._row_raw))
            self._row = []
            self._row_raw = []
        elif tag == "table":
            exp = [r for r in self._table if any(c.strip() for c in r)]
            raw = [r for r in self._table_raw if any(c.strip() for c in r)]
            # Colspan expansion fixes alignment when the grid is regular, but on
            # tables whose spans vary row to row it scatters values across
            # columns. Keep whichever produces the more consistent grid.
            def spread(rows):
                return len({len(r) for r in rows}) if rows else 0
            rows = exp if spread(exp) <= spread(raw) else raw
            if rows:
                self.blocks.append({"k": "table", "rows": rows})
            self._table = []
            self._table_raw = []

    def handle_data(self, data):
        self._buf.append(data)

    def _text(self):
        return re.sub(r"[ \t]+", " ", "".join(self._buf).replace("\xa0", " ")).strip()

    def _flush(self):
        if self._in_p or self._in_td:
            t = self._text()
            if t:
                if self._in_td:
                    self._cell.append(t)
                else:
                    self.blocks.append({"k": "p", "cls": self._cls or "",
                                        "text": t, "sectno": self._sectno})
        self._in_p = False
        self._sectno = False
        self._buf = []


def epub_blocks(path):
    z = zipfile.ZipFile(path)
    names = [n for n in z.namelist() if n.lower().endswith((".html", ".xhtml"))]

    def key(n):
        m = re.search(r"document_(\d+)", n)
        return (int(m.group(1)) if m else 0, n)

    sizes = {os.path.basename(i.filename): i.file_size for i in z.infolist()}
    out = []
    for n in sorted(names, key=key):
        d = Doc(sizes)
        d.feed(z.read(n).decode("utf-8", "replace"))
        d._flush()
        out.append({"k": "file"})      # volume boundary
        out += d.blocks
    return out


def md_table(rows):
    w = max(len(r) for r in rows)
    rows = [[c.replace("|", "\\|") for c in r] + [""] * (w - len(r)) for r in rows]
    out = ["| " + " | ".join(rows[0]) + " |",
           "|" + "|".join(["---"] * w) + "|"]
    for r in rows[1:]:
        out.append("| " + " | ".join(r) + " |")
    return out


def table_split(body, name):
    """Chunk a document that is a run of tables rather than a run of sections.

    The Tax Practitioners Board publishes its terminations as a notifiable
    instrument shaped like this: one sentence naming the enabling provision,
    then twenty-odd tables of agent names, then a signature. There is no
    heading anywhere, so both the structural pass and the bare-paragraph
    fallback correctly find nothing, and the whole 15,000-word document lands
    in a single row that no retriever can use.

    The table is the document's own unit of meaning, and each one repeats its
    header row, so splitting on table boundaries loses nothing.

    Whatever prose precedes a table travels with it, in document order. That
    matters twice over. On the TPB instruments it carries the sentence naming
    the enabling provision, without which a table of names and dates says
    nothing. On Excise By-law No. 127, which prescribes petroleum fields one
    table per basin, it carries the basin name — and the operative paragraphs
    ahead of the first table become their own chunk instead of being dropped.
    Discarding the interleaved prose was a real loss of text, not a tidier
    result.

    Returns [] unless the document really is table-shaped, so this cannot
    reshape an instrument that merely contains a rate table.
    """
    # Segment the body into prose runs and tables, preserving order.
    segs, cur, pros = [], [], []
    for ln in body.split("\n"):
        if ln.startswith("|"):
            if pros:
                segs.append(("p", pros))
                pros = []
            cur.append(ln)
            continue
        if cur:
            segs.append(("t", cur))
            cur = []
        s = ln.strip()
        if s and not s.startswith("#"):
            pros.append(s)
    if cur:
        segs.append(("t", cur))
    if pros:
        segs.append(("p", pros))

    tables = [s for k, s in segs if k == "t" and len(s) >= 3]
    if len(tables) < 3:
        return []
    # Count words, not lines. Counting lines fails on the TPB instruments that
    # repeat their lead-in sentence above every table: 36 prose lines against
    # 32 tables reads as a narrative document, when the prose is the same
    # sentence thirty-two times. By word mass the tables win nine to one.
    t_words = sum(len(l.split()) for t in tables for l in t)
    p_words = sum(len(s.split()) for k, seg in segs if k == "p" for s in seg)
    if t_words < 2 * p_words:
        return []

    out, pending, n = [], [], 0
    total = len(tables)

    def emit(heading, kind, text):
        out.append({"section": None, "heading": heading, "kind": kind,
                    "container": None, "text": [text]})

    for kind, seg in segs:
        if kind == "p":
            pending.extend(seg)
            continue
        if len(seg) < 3:
            pending.extend(seg)
            continue
        n += 1
        # A short trailing prose line above a table is its caption ("C. PERTH
        # BASIN"); a long one is body text that happens to sit there.
        cap = pending[-1] if pending and len(pending[-1]) <= 80 else None
        # A substantial prose run before a table is the instrument's operative
        # text, not a caption. Give it its own row rather than burying it in
        # the first table's chunk, where nothing would ever retrieve it.
        rest = pending[:-1] if cap else pending
        if sum(len(s.split()) for s in rest) > 120:
            emit("%s — opening provisions" % name, "text_block", "\n".join(rest).strip())
            pending = [cap] if cap else []
        head = "%s — %s" % (name, cap) if cap else "%s — table %d of %d" % (name, n, total)
        emit(head, "table_block", "\n".join(pending + seg).strip())
        pending = []
    if pending:
        emit("%s — closing provisions" % name, "text_block", "\n".join(pending).strip())
    return out


def to_markdown(blocks, meta, force_bare=False):
    lines, sections, endnotes = [], [], []
    long_title = None
    long_title_is_longt = False
    container = None
    cur = None
    in_endnotes = False

    def _head_lvl(b):
        base = (b["cls"].split() or [""])[0]
        m = IASB_HEAD.match(base)
        return HEAD_LEVEL.get(base) or (2 if m and m.group(1) == "1" else 5 if m else None)

    # Counts the IASB template too, so its publisher boilerplate is held out of
    # the body by the same gate that holds out an Act's compilation cover page.
    has_acthead = any(b["k"] == "p" and _head_lvl(b) for b in blocks)
    # In bare mode there is no structural heading to mark the start of the body,
    # so the pre-body gate would discard the whole document.
    seen_body = not has_acthead or force_bare
    # Bare mode is never chosen from the markup, because guessing which classes
    # count as structure was wrong twice: cosmetic Word classes look structural,
    # and one-off template families run to dozens. The caller runs the normal
    # pass, and only if that yields no section at all does it re-run with
    # force_bare. A document that parsed keeps its result untouched.
    bare_mode = force_bare

    # APRA prudential standards and their kin put headings in classless
    # paragraphs with no number ("Authority", "Application"), which the numbered
    # heuristic below cannot see. They do list those headings in their own table
    # of contents, so the document supplies its own heading vocabulary. Exact
    # match on a short line, and the TOC itself is skipped by SKIP_CLASS, so the
    # only hits are the real headings in the body.
    toc_titles = set()
    if bare_mode:
        for b in blocks:
            if b["k"] != "p":
                continue
            if not re.match(r'^TOC\d', (b["cls"].split() or [""])[0], re.I):
                continue
            t = re.sub(r'\s*\.{2,}\s*\d*$', '', " ".join(b["text"].split())).strip()
            if 2 < len(t) < 120:
                toc_titles.add(t.lower())

    # APRA prudential standards without a contents page still mark their
    # headings, just by omission: the heading is unstyled and the body under it
    # carries a class. That asymmetry is the whole signal. It also rejects the
    # signature block for free, because "Clare Gibney" is followed by "Executive
    # Director", which is unstyled too.
    classless_heads = set()
    if bare_mode:
        paras = [i for i, b in enumerate(blocks) if b["k"] == "p" and b["text"].strip()]
        for pos, i in enumerate(paras[:-1]):
            b = blocks[i]
            if b["cls"].strip():
                continue
            t = b["text"].strip()
            if not (2 < len(t) <= 80) or t[-1] in ".;,:":
                continue
            nxt = blocks[paras[pos + 1]]
            base_n = (nxt["cls"].split() or [""])[0]
            # Any style at all counts. COSMETIC_CLASS must not be used here: it
            # matches BodyText, which prefix-matches BodyText1 — the very class
            # that marks the styled body under an unstyled heading.
            if base_n and not SKIP_CLASS.match(base_n):
                classless_heads.add(i)

    def emit(s):
        if in_endnotes:
            endnotes.append(s)
            return
        lines.append(s)
        if cur is not None:
            cur["text"].append(s)

    for bi, b in enumerate(blocks):
        k = b["k"]
        if k == "file":
            # Front matter repeats at the head of every volume; without this
            # reset it fuses into whichever section was open at the seam.
            # in_endnotes must reset too, or one stray trigger in volume 1
            # swallows every remaining volume.
            seen_body = not has_acthead or force_bare
            in_endnotes = False
            container = None
            continue

        if k in ("table", "img"):
            # Same gate as paragraphs. Without it a trailing amendment-history
            # table lands in whatever section was last open.
            if not seen_body and not in_endnotes:
                continue
            if k == "img":
                emit("> Figure: %s" % b["alt"])
            else:
                for ln in md_table(b["rows"]):
                    emit(ln)
                emit("")
            continue

        cls, text, sectno = b["cls"], b["text"], b["sectno"]
        base = cls.split()[0] if cls else ""

        # Skip the contents page FIRST. It carries a TOC entry whose text is
        # literally "Endnotes", which would otherwise trip the endnote trigger
        # before the body has even started.
        if SKIP_CLASS.match(cls):
            continue
        if not in_endnotes and (ENDNOTE_CLASS.match(base)
                                or (seen_body and ENDNOTE_TEXT.match(text))):
            in_endnotes = True
            seen_body = True
            cur = None
        if FRONT_CLASS.match(base) and (long_title is None or base.lower().startswith('longt')):
            if long_title is None or not long_title_is_longt:
                long_title = text
                long_title_is_longt = base.lower().startswith('longt')

        lvl = HEAD_LEVEL.get(base)
        if lvl is None:
            m_iasb = IASB_HEAD.match(base)
            if m_iasb:
                # Level 1 opens a container (Objective, Scope); deeper titles are
                # headings within it.
                lvl = 2 if m_iasb.group(1) == "1" else 5
        if lvl is None and sectno and not has_acthead and len(text) < 200:
            lvl = 5
        if lvl and not in_endnotes:
            seen_body = True
            lines.append("")
            lines.append("#" * lvl + " " + text)
            if lvl == 5:
                m = SECNO.match(text.strip())
                if m:
                    sid = "%s-%s" % (m.group(1), m.group(2))
                else:
                    p = PLAIN_SECNO.match(text)
                    sid = p.group(1) if p else None
                # A level-5 heading with no parseable number (e.g. "Notes.",
                # "Table I") is not a section and must not be counted as one.
                cur = {"section": sid, "heading": text,
                       "kind": "section" if sid else "unnumbered",
                       "container": container, "text": []}
                sections.append(cur)
            else:
                # A Chapter/Part/Division/Schedule heading closes the previous
                # section. Without this, Schedule bodies keep appending to the
                # last section and inherit a section number that does not own
                # them. Open a container row so the text is still retrievable,
                # labelled by its own heading.
                container = text
                cur = {"section": None, "heading": text, "kind": "container",
                       "container": container, "text": []}
                sections.append(cur)
            continue

        if bare_mode and not in_endnotes and not SKIP_CLASS.match(cls):
            if " ".join(text.split()).lower() in toc_titles or bi in classless_heads:
                lines.append("")
                lines.append("## " + text)
                container = text
                cur = {"section": None, "heading": text, "kind": "container",
                       "container": container, "text": []}
                sections.append(cur)
                continue
            m = None if BARE_DATEISH.match(text) else BARE_SECHEAD.match(text)
            if m:
                lines.append("")
                lines.append("##### " + text)
                cur = {"section": m.group(1), "heading": text, "kind": "section",
                       "container": container, "text": []}
                sections.append(cur)
                continue
            if BARE_CONTAINER.match(text):
                lines.append("")
                lines.append("## " + text)
                container = text
                cur = {"section": None, "heading": text, "kind": "container",
                       "container": container, "text": []}
                sections.append(cur)
                continue
            # The making words and the signature block sit ahead of section 1.
            # Without a row to hold them they reach the markdown but never the
            # JSONL, which is the file retrieval actually reads.
            if cur is None and text.strip():
                cur = {"section": None, "heading": "Introductory material",
                       "kind": "introductory", "container": None, "text": []}
                sections.append(cur)

        # Some Acts put real body text before the first ActHead (the Customs
        # Tariff Act's USER'S GUIDE). Emit it when it carries a body class;
        # unclassed compilation cover text stays out.
        if not seen_body and not in_endnotes:
            if not PREBODY_CLASS.match(base):
                continue
            if cur is None:
                cur = {"section": None, "heading": "Introductory material",
                       "kind": "introductory", "container": None, "text": []}
                sections.append(cur)
        prefix = ""
        if PARA_CLASS.match(base):
            prefix = "- "
        elif base == "notetext":
            prefix = "> "
        emit("")
        emit(prefix + text)

    attr = attribution(meta["retrieved"])
    fm = ["---",
          "register_id: %s" % meta["id"],
          "title: %s" % json.dumps(meta["name"]),
          "long_title: %s" % json.dumps(long_title or ""),
          # Act / LegislativeInstrument / NotifiableInstrument. Without it a
          # regulation reads as though Parliament enacted it.
          "collection: %s" % (meta.get("collection") or "null"),
          "compilation_number: %s" % (meta.get("compilationNumber") or "null"),
          "compilation_date: %s" % meta.get("versionStart"),
          # The Register can record that a version commenced without publishing
          # a compilation for it. Thirteen titles are in that state, so the text
          # below is the last compilation that exists, not the law in force.
          # Say so on the face of the document rather than only in sources.json.
          "version_is_current: %s" % ("false" if meta.get("version_is_current") is False
                                      else "true"),
          "superseded_from: %s" % (meta.get("current_version_start") or "null"),
          "source_url: %s" % meta.get("sourceUrl", ""),
          "register_page: https://www.legislation.gov.au/%s/latest/text" % meta["id"],
          "retrieved: %s" % meta["retrieved"],
          "licence: %s" % LICENCE,
          "licence_url: %s" % LICENCE_URL,
          "authorised: false",
          "attribution: %s" % json.dumps(attr),
          "---", ""]
    head = "# " + meta["name"] + "\n"
    if long_title:
        head += "\n> " + long_title + "\n"
    body = head + "\n".join(lines)
    body = re.sub(r"\n{3,}", "\n\n", body).strip()
    return "\n".join(fm) + body + "\n", sections, "\n".join(endnotes).strip(), long_title


def main(retrieved):
    scratch = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(scratch, "manifest_raw.json"), encoding="utf-8") as f:
        manifest = json.load(f)

    root = corpus_root(__file__)
    md_root = child(root, "markdown")
    os.makedirs(md_root, exist_ok=True)
    out_manifest = []
    failures = []

    for i, a in enumerate([m for m in manifest if m.get("epub")], 1):
        try:
            rid = register_id(a["id"])
            expected_epub = "%s.epub" % rid
            if a.get("epub") != expected_epub:
                raise ValueError("unexpected EPUB filename")
            src = child(root, "epub", expected_epub)
        except (KeyError, ValueError) as e:
            failures.append((str(a.get("id", "?")), str(e)))
            continue
        a = dict(a, id=rid)
        # Most EPUBs were fetched on an earlier day and served from cache, so
        # the build date would misstate when this Act was actually obtained.
        fetched = datetime.date.fromtimestamp(os.path.getmtime(src)).isoformat()
        attr = attribution(fetched)
        meta = dict(a, retrieved=fetched)
        try:
            blocks = epub_blocks(src)
            md, sections, endnotes, long_title = to_markdown(blocks, meta)
            # Instruments come from dozens of one-off Word templates. When none
            # of the structural detectors found a single section, fall back to
            # reading numbered paragraphs as headings rather than shipping the
            # whole instrument as one undifferentiated chunk.
            if not any(any(t.strip() for t in s["text"]) for s in sections):
                b_md, b_sec, b_en, b_lt = to_markdown(blocks, meta, force_bare=True)
                # Accept only if it actually found headings. A single
                # "Introductory material" row holding the whole document is the
                # same undifferentiated blob under a more misleading label, so
                # let the whole_act path own that case and say so.
                if any(s["kind"] in ("section", "container")
                       and any(t.strip() for t in s["text"]) for s in b_sec):
                    md, sections, endnotes, long_title = b_md, b_sec, b_en, b_lt
        except Exception as e:
            print("  FAIL %s %s" % (a["id"], e), flush=True)
            out_manifest.append(dict(a, markdown=None, sections=0, error=str(e)))
            failures.append((a["id"], str(e)))
            continue

        d = child(md_root, a["id"])
        os.makedirs(d, exist_ok=True)
        with open(child(d, a["id"] + ".md"), "w", encoding="utf-8") as f:
            f.write(md)
        if endnotes:
            # endnotes.md travels independently of the Act file, so it needs
            # its own provenance block.
            en_fm = "\n".join([
                "---",
                "register_id: %s" % a["id"],
                "title: %s" % json.dumps("Endnotes: " + a["name"]),
                "collection: %s" % (a.get("collection") or "null"),
                "compilation_number: %s" % (a.get("compilationNumber") or "null"),
                "compilation_date: %s" % a.get("versionStart"),
                "source_url: %s" % a.get("sourceUrl", ""),
                "register_page: https://www.legislation.gov.au/%s/latest/text" % a["id"],
                "retrieved: %s" % fetched,
                "licence: %s" % LICENCE,
                "licence_url: %s" % LICENCE_URL,
                "authorised: false",
                "attribution: %s" % json.dumps(attr),
                "---", ""])
            with open(child(d, "endnotes.md"), "w", encoding="utf-8") as f:
                f.write(en_fm + "# Endnotes: %s\n\n%s\n" % (a["name"], endnotes))

        body = md.split("\n---\n", 1)[-1] if md.startswith("---") else md
        emitted = [s for s in sections if any(x.strip() for x in s["text"])]
        whole = tabled = False
        if not emitted:
            emitted = table_split(body, a["name"])
            tabled = bool(emitted)
        if not emitted:
            whole = True
            emitted = [{"section": None, "heading": a["name"],
                        "kind": "whole_act", "container": None,
                        "text": [body.strip()]}]

        # Schedules restart numbering at 1, so a bare section id is not unique
        # within an Act. row_id is; `section` stays the human-facing label.
        with open(child(d, "sections.jsonl"), "w", encoding="utf-8") as f:
            for ordinal, s in enumerate(emitted, 1):
                f.write(json.dumps({
                    "register_id": a["id"], "act": a["name"],
                    "collection": a.get("collection"),
                    "compilation_number": a.get("compilationNumber"),
                    "compilation_date": a.get("versionStart"),
                    "version_is_current": a.get("version_is_current", True),
                    "row_id": "%s:%04d:%s" % (a["id"], ordinal, s["section"] or "-"),
                    "section": s["section"], "heading": s["heading"],
                    "container": s.get("container"),
                    "kind": s.get("kind") or ("section" if s.get("section") else "unnumbered"),
                    "granularity": ("whole_act" if whole else
                                    "table_block" if tabled else "section"),
                    "text": "\n".join(s["text"]).strip(),
                    "source_url": a.get("sourceUrl", ""),
                    "register_page": "https://www.legislation.gov.au/%s/latest/text" % a["id"],
                    "licence": LICENCE, "licence_url": LICENCE_URL,
                    "authorised": False, "attribution": attr,
                }, ensure_ascii=False) + "\n")

        words = len(body.split())
        out_manifest.append(dict(a, markdown="%s/%s.md" % (a["id"], a["id"]), retrieved=fetched,
                                 sections=len(emitted), granularity=
                                 ("whole_act" if whole else
                                  "table_block" if tabled else "section"),
                                 long_title=long_title, words=words,
                                 endnotes=bool(endnotes)))
        print("%3d %-12s sections=%-5d words=%-9d %s" % (
            i, a["id"], len(emitted), words, a["name"][:52]), flush=True)

    # Consumers use manifest_md.json as the signal that the markdown tree is
    # complete.  Never publish a manifest that makes a partial parse look like
    # a successful corpus build.  Earlier title directories may remain for
    # inspection, but the stage exits non-zero and leaves the prior manifest
    # untouched.
    if failures:
        ids = ", ".join(aid for aid, _ in failures[:10])
        more = "" if len(failures) <= 10 else " (and %d more)" % (len(failures) - 10)
        raise RuntimeError("extraction incomplete for %d title(s): %s%s; "
                           "refusing to write manifest_md.json"
                           % (len(failures), ids, more))

    with open(os.path.join(scratch, "manifest_md.json"), "w", encoding="utf-8") as f:
        json.dump(out_manifest, f, indent=1)
    print("\nEXTRACT DONE: %d acts" % len(out_manifest))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "2026-08-03")
