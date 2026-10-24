//! CPython 3.15's `html.parser.HTMLParser` (identical in 3.14.8), as a list of events.
//!
//! [`parse`] runs one `feed(text)` and then `close()`, which is how every site
//! script drives its parser, and returns what the handler methods would have
//! received, in order, each with the `getpos()` value the handler would see.
//! Data chunking follows the original: `feed` parses up to a possibly cut
//! character reference and `close` handles the rest as a separate chunk.
// The parser keeps CPython's index arithmetic line for line, so prefix tests stay explicit.
#![allow(clippy::manual_strip)]
use crate::html;
use crate::pyre::{self, re};

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    StartTag(String, Vec<(String, Option<String>)>),
    StartEndTag(String, Vec<(String, Option<String>)>),
    EndTag(String),
    Data(String),
    Comment(String),
    Decl(String),
    Pi(String),
    UnknownDecl(String),
    CharRef(String),
    EntityRef(String),
}

/// One event and the `(lineno, offset)` that `getpos()` returns during its handler.
#[derive(Debug, Clone, PartialEq)]
pub struct Positioned {
    pub event: Event,
    pub line: usize,
    pub offset: usize,
}

const CDATA_CONTENT_ELEMENTS: [&str; 6] =
    ["script", "style", "xmp", "iframe", "noembed", "noframes"];
const RCDATA_CONTENT_ELEMENTS: [&str; 2] = ["textarea", "title"];

const TAGFIND_TOLERANT: &str = r"([a-zA-Z][^\t\n\r\f />]*)(?:[\t\n\r\f ]|/(?!>))*";
pub(crate) const ATTRFIND_TOLERANT: &str = concat!(
    r#"((?<=['"\t\n\r\f /])[^\t\n\r\f />][^\t\n\r\f /=>]*)"#,
    r#"([\t\n\r\f ]*=[\t\n\r\f ]*('[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\f ]*))?"#,
    r"(?:[\t\n\r\f ]|/(?!>))*",
);
const LOCATETAGEND: &str = concat!(
    r"[a-zA-Z][^\t\n\r\f />]*",
    r"[\t\n\r\f /]*",
    r#"(?:(?<=['"\t\n\r\f /])[^\t\n\r\f />][^\t\n\r\f /=>]*"#,
    r#"(?:[\t\n\r\f ]*=[\t\n\r\f ]*(?:'[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\f ]*))?"#,
    r"[\t\n\r\f /]*)*>?",
);

/// `html.parser._unescape_attrvalue`.
fn unescape_attrvalue(value: &str) -> String {
    re(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*)[;=]?", 0).sub_with(value, |m| {
        let reference = &m[0];
        if reference.starts_with("&#")
            || (!reference.ends_with('=') && html::is_html5_entity(&reference[1..]))
        {
            html::unescape(reference)
        } else {
            reference.to_owned()
        }
    })
}

struct Parser {
    rawdata: String,
    convert_charrefs: bool,
    cdata_elem: Option<String>,
    escapable: bool,
    line: usize,
    offset: usize,
    events: Vec<Positioned>,
}

/// Byte offset of the character `count` characters before the end of `text[from..]`, or `from`.
fn chars_from_end(text: &str, from: usize, count: usize) -> usize {
    text[from..]
        .char_indices()
        .rev()
        .nth(count.saturating_sub(1))
        .map_or(from, |(i, _)| from + i)
}

impl Parser {
    fn emit(&mut self, event: Event) {
        self.events.push(Positioned {
            event,
            line: self.line,
            offset: self.offset,
        });
    }

    fn updatepos(&mut self, i: usize, j: usize) -> usize {
        if i >= j {
            return j;
        }
        let span = &self.rawdata[i..j];
        let lines = span.matches('\n').count();
        if lines > 0 {
            self.line += lines;
            let last = span.rfind('\n').expect("a newline");
            self.offset = span[last + 1..].chars().count();
        } else {
            self.offset += span.chars().count();
        }
        j
    }

    fn data(&mut self, text: &str) {
        let text = if self.convert_charrefs && self.escapable {
            html::unescape(text)
        } else {
            text.to_owned()
        };
        self.emit(Event::Data(text));
    }

    /// Where `interesting` next matches at or after `i`.
    fn interesting(&self, i: usize) -> Option<usize> {
        match self.cdata_elem.as_deref() {
            None => self.rawdata[i..].find(['&', '<']).map(|j| i + j),
            // re.compile(r'\z') matches at the end.
            Some("plaintext") => Some(self.rawdata.len()),
            Some(elem) => {
                // re.IGNORECASE | re.ASCII: only ASCII letters fold.
                let name: String = elem
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphabetic() {
                            format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
                        } else {
                            pyre::escape(&c.to_string())
                        }
                    })
                    .collect();
                let amp = if self.escapable && !self.convert_charrefs {
                    "&|"
                } else {
                    ""
                };
                re(&format!(r"{amp}</{name}(?=[\t\n\r\f />])"), 0)
                    .search_at(&self.rawdata, i)
                    .map(|m| m.get(0).expect("group 0").start())
            }
        }
    }

    fn set_cdata_mode(&mut self, elem: &str, escapable: bool) {
        self.cdata_elem = Some(elem.to_lowercase());
        self.escapable = escapable;
    }

    fn clear_cdata_mode(&mut self) {
        self.cdata_elem = None;
        self.escapable = true;
    }

    fn goahead(&mut self, end: bool) {
        let mut i = 0;
        let n = self.rawdata.len();
        while i < n {
            let j;
            if self.convert_charrefs && self.cdata_elem.is_none() {
                match self.rawdata[i..].find('<') {
                    Some(at) => j = i + at,
                    None => {
                        // A character reference may be cut at the end of the buffer.
                        let from = chars_from_end(&self.rawdata, i, 34);
                        if let Some(amp) = self.rawdata[from..].rfind('&').map(|a| from + a)
                            && !self.rawdata[amp..].contains(['\t', '\n', '\r', '\x0c', ' ', ';'])
                        {
                            break;
                        }
                        j = n;
                    }
                }
            } else {
                match self.interesting(i) {
                    Some(at) => j = at,
                    None => {
                        if self.cdata_elem.is_some() {
                            break;
                        }
                        j = n;
                    }
                }
            }
            if i < j {
                let text = self.rawdata[i..j].to_owned();
                self.data(&text);
            }
            i = self.updatepos(i, j);
            if i == n {
                break;
            }
            let rest = &self.rawdata[i..];
            if rest.starts_with('<') {
                let starttag = re("<[a-zA-Z]", 0).match_at(&self.rawdata, i).is_some();
                let mut k: isize = if starttag {
                    self.parse_starttag(i)
                } else if rest.starts_with("</") {
                    self.parse_endtag(i)
                } else if rest.starts_with("<!--") {
                    self.parse_comment(i)
                } else if rest.starts_with("<?") {
                    self.parse_pi(i)
                } else if rest.starts_with("<!") {
                    self.parse_html_declaration(i)
                } else if i + 1 < n || end {
                    self.emit(Event::Data("<".into()));
                    (i + 1) as isize
                } else {
                    break;
                };
                if k < 0 {
                    if !end {
                        break;
                    }
                    let rest = self.rawdata[i..].to_owned();
                    if starttag {
                    } else if rest.starts_with("</") {
                        if i + 2 == n {
                            self.emit(Event::Data("</".into()));
                        } else if re("</[a-zA-Z]", 0).match_at(&rest, 0).is_some() {
                        } else {
                            self.emit(Event::Comment(rest[2..].to_owned()));
                        }
                    } else if rest.starts_with("<!--") {
                        let mut body = &rest[4..];
                        for suffix in ["--!", "--", "-"] {
                            if let Some(stripped) = body.strip_suffix(suffix) {
                                body = stripped;
                                break;
                            }
                        }
                        self.emit(Event::Comment(body.to_owned()));
                    } else if rest.starts_with("<![CDATA[") {
                        self.emit(Event::UnknownDecl(rest[3..].to_owned()));
                    } else if prefix_lower_is(&rest, "<!doctype") {
                        self.emit(Event::Decl(rest[2..].to_owned()));
                    } else if rest.starts_with("<!") {
                        self.emit(Event::Comment(rest[2..].to_owned()));
                    } else if rest.starts_with("<?") {
                        self.emit(Event::Pi(rest[2..].to_owned()));
                    } else {
                        unreachable!("we should not get here!");
                    }
                    k = n as isize;
                }
                i = self.updatepos(i, k as usize);
            } else if rest.starts_with("&#") {
                let found = re("&#(?:[0-9]+|[xX][0-9a-fA-F]+)[^0-9a-fA-F]", 0)
                    .match_at(&self.rawdata, i)
                    .map(|m| reference(&m, 2));
                if let Some((name, k)) = found {
                    self.emit(Event::CharRef(name));
                    i = self.updatepos(i, k);
                    continue;
                }
                if re("&#(?:[0-9]|[xX][0-9a-fA-F])", 0)
                    .match_at(&self.rawdata, i)
                    .is_some()
                {
                    if end {
                        self.emit(Event::CharRef(self.rawdata[i + 2..].to_owned()));
                        i = self.updatepos(i, n);
                    }
                    break;
                } else if self.rawdata[i..].chars().count() > 3 {
                    self.emit(Event::Data("&#".into()));
                    i = self.updatepos(i, i + 2);
                } else {
                    break;
                }
            } else if rest.starts_with('&') {
                let found = re("&([a-zA-Z][-.a-zA-Z0-9]*)[^a-zA-Z0-9]", 0)
                    .match_at(&self.rawdata, i)
                    .map(|m| reference(&m, 1));
                if let Some((name, k)) = found {
                    self.emit(Event::EntityRef(name));
                    i = self.updatepos(i, k);
                    continue;
                }
                if re("&[a-zA-Z#]", 0).match_at(&self.rawdata, i).is_some() {
                    if end {
                        self.emit(Event::EntityRef(self.rawdata[i + 1..].to_owned()));
                        i = self.updatepos(i, n);
                    }
                    break;
                } else if i + 1 < n {
                    self.emit(Event::Data("&".into()));
                    i = self.updatepos(i, i + 1);
                } else {
                    break;
                }
            } else {
                unreachable!("interesting.search() lied");
            }
        }
        if end && i < n {
            let text = self.rawdata[i..n].to_owned();
            self.data(&text);
            i = self.updatepos(i, n);
        }
        self.rawdata.drain(..i);
    }

    fn parse_html_declaration(&mut self, i: usize) -> isize {
        let rest = &self.rawdata[i..];
        if rest.starts_with("<!--") {
            self.parse_comment(i)
        } else if rest.starts_with("<![CDATA[") {
            match rest[9..].find("]]>") {
                None => -1,
                Some(j) => {
                    let body = rest[3..9 + j].to_owned();
                    self.emit(Event::UnknownDecl(body));
                    (i + 9 + j + 3) as isize
                }
            }
        } else if prefix_lower_is(rest, "<!doctype") {
            let start = byte_after_chars(rest, 9);
            match rest[start..].find('>') {
                None => -1,
                Some(gt) => {
                    let body = rest[2..start + gt].to_owned();
                    self.emit(Event::Decl(body));
                    (i + start + gt + 1) as isize
                }
            }
        } else {
            self.parse_bogus_comment(i)
        }
    }

    fn parse_comment(&mut self, i: usize) -> isize {
        // An empty comment is abruptly closed by the first ">" or "->".
        let m = re("-?>", 0)
            .match_at(&self.rawdata, i + 4)
            .or_else(|| re("--!?>", 0).search_at(&self.rawdata, i + 4));
        let Some(m) = m else { return -1 };
        let whole = m.get(0).expect("group 0");
        let (start, end) = (whole.start(), whole.end());
        let body = self.rawdata[i + 4..start].to_owned();
        self.emit(Event::Comment(body));
        end as isize
    }

    fn parse_bogus_comment(&mut self, i: usize) -> isize {
        match self.rawdata[i + 2..].find('>') {
            None => -1,
            Some(pos) => {
                let body = self.rawdata[i + 2..i + 2 + pos].to_owned();
                self.emit(Event::Comment(body));
                (i + 2 + pos + 1) as isize
            }
        }
    }

    fn parse_pi(&mut self, i: usize) -> isize {
        match self.rawdata[i + 2..].find('>') {
            None => -1,
            Some(pos) => {
                let body = self.rawdata[i + 2..i + 2 + pos].to_owned();
                self.emit(Event::Pi(body));
                (i + 2 + pos + 1) as isize
            }
        }
    }

    fn parse_starttag(&mut self, i: usize) -> isize {
        let endpos = self.check_for_whole_start_tag(i);
        if endpos < 0 {
            return endpos;
        }
        // Moved out, not cloned: a copy per start tag made parsing quadratic.
        // Nothing below reads self.rawdata.
        let rawdata = std::mem::take(&mut self.rawdata);
        let result = self.parse_starttag_at(&rawdata, i, endpos as usize);
        self.rawdata = rawdata;
        result
    }

    fn parse_starttag_at(&mut self, rawdata: &str, i: usize, endpos: usize) -> isize {
        let m = re(TAGFIND_TOLERANT, 0)
            .match_at(rawdata, i + 1)
            .expect("unexpected call to parse_starttag()");
        let mut k = m.get(0).expect("group 0").end();
        let tag = m[1].to_lowercase();
        let mut attrs = Vec::new();
        while k < endpos {
            let Some(m) = re(ATTRFIND_TOLERANT, 0).match_at(rawdata, k) else {
                break;
            };
            let name = m[1].to_lowercase();
            let value = match (m.get(2), m.get(3)) {
                (None, _) => None,
                (Some(_), value) => {
                    let mut value = value.map_or("", |v| v.as_str());
                    let quoted = value.len() >= 2
                        && ((value.starts_with('\'') && value.ends_with('\''))
                            || (value.starts_with('"') && value.ends_with('"')));
                    // Python's value[:1] == "'" == value[-1:] also holds for a lone quote.
                    if quoted || value == "'" || value == "\"" {
                        value = if value.len() >= 2 {
                            &value[1..value.len() - 1]
                        } else {
                            ""
                        };
                    }
                    Some(if value.is_empty() {
                        String::new()
                    } else {
                        unescape_attrvalue(value)
                    })
                }
            };
            attrs.push((name, value));
            k = m.get(0).expect("group 0").end();
        }
        let end = crate::py::strip(&rawdata[k..endpos]);
        if end != ">" && end != "/>" {
            self.emit(Event::Data(rawdata[i..endpos].to_owned()));
            return endpos as isize;
        }
        if end.ends_with("/>") {
            self.emit(Event::StartEndTag(tag, attrs));
        } else {
            self.emit(Event::StartTag(tag.clone(), attrs));
            if CDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) || tag == "plaintext" {
                self.set_cdata_mode(&tag, false);
            } else if RCDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) {
                self.set_cdata_mode(&tag, true);
            }
        }
        endpos as isize
    }

    fn check_for_whole_start_tag(&self, i: usize) -> isize {
        let m = re(LOCATETAGEND, 0)
            .match_at(&self.rawdata, i + 1)
            .expect("a tag");
        let j = m.get(0).expect("group 0").end();
        if self.rawdata.as_bytes()[j - 1] != b'>' {
            return -1;
        }
        j as isize
    }

    fn parse_endtag(&mut self, i: usize) -> isize {
        if !self.rawdata[i + 2..].contains('>') {
            return -1;
        }
        if re("</[a-zA-Z]", 0).match_at(&self.rawdata, i).is_none() {
            if self.rawdata[i + 2..].starts_with('>') {
                return (i + 3) as isize;
            }
            return self.parse_bogus_comment(i);
        }
        let m = re(LOCATETAGEND, 0)
            .match_at(&self.rawdata, i + 2)
            .expect("a tag");
        let j = m.get(0).expect("group 0").end();
        if self.rawdata.as_bytes()[j - 1] != b'>' {
            return -1;
        }
        let m = re(TAGFIND_TOLERANT, 0)
            .match_at(&self.rawdata, i + 2)
            .expect("a tag");
        let tag = m[1].to_lowercase();
        self.emit(Event::EndTag(tag));
        self.clear_cdata_mode();
        j as isize
    }
}

/// A reference's name (the match after `skip` bytes, less its final character) and where
/// parsing resumes: after a `;`, or at the final character otherwise.
fn reference(m: &crate::pyre::Caps<'_>, skip: usize) -> (String, usize) {
    let whole = m.get(0).expect("group 0");
    let text = whole.as_str();
    let last = text.chars().next_back().expect("a character");
    let name = text[skip..text.len() - last.len_utf8()].to_owned();
    let resume = if last == ';' {
        whole.end()
    } else {
        whole.end() - last.len_utf8()
    };
    (name, resume)
}

fn byte_after_chars(text: &str, count: usize) -> usize {
    text.char_indices()
        .nth(count)
        .map_or(text.len(), |(i, _)| i)
}

/// `text[:len(prefix)].lower() == prefix`, counting characters as Python does.
fn prefix_lower_is(text: &str, prefix: &str) -> bool {
    text[..byte_after_chars(text, prefix.chars().count())].to_lowercase() == prefix
}

/// `parser.feed(text); parser.close()`, returning the handler calls in order.
pub fn parse(text: &str, convert_charrefs: bool) -> Vec<Positioned> {
    let mut parser = Parser {
        rawdata: String::new(),
        convert_charrefs,
        cdata_elem: None,
        escapable: true,
        line: 1,
        offset: 0,
        events: Vec::new(),
    };
    if !text.is_empty() {
        parser.rawdata.push_str(text);
        parser.goahead(false);
    }
    parser.goahead(true);
    parser.events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &str) -> Vec<Event> {
        parse(text, true).into_iter().map(|p| p.event).collect()
    }

    #[test]
    fn tags_attributes_and_text() {
        assert_eq!(
            events("<P Class='a &amp; b' hidden data-x=y>Hi &amp; bye<br/></p>"),
            vec![
                Event::StartTag(
                    "p".into(),
                    vec![
                        ("class".into(), Some("a & b".into())),
                        ("hidden".into(), None),
                        ("data-x".into(), Some("y".into()))
                    ]
                ),
                Event::Data("Hi & bye".into()),
                Event::StartEndTag("br".into(), vec![]),
                Event::EndTag("p".into()),
            ]
        );
    }

    #[test]
    fn script_content_is_raw_until_its_end_tag() {
        assert_eq!(
            events("<script>if (a < b) { x = '</b>'; }</SCRIPT >tail"),
            vec![
                Event::StartTag("script".into(), vec![]),
                Event::Data("if (a < b) { x = '</b>'; }".into()),
                Event::EndTag("script".into()),
                Event::Data("tail".into()),
            ]
        );
    }

    #[test]
    fn a_cut_reference_at_the_end_is_a_separate_chunk() {
        assert_eq!(
            events("a<b>x &amp"),
            vec![
                Event::Data("a".into()),
                Event::StartTag("b".into(), vec![]),
                Event::Data("x &".into()),
            ]
        );
    }

    #[test]
    fn comments_declarations_and_unclosed_tags() {
        assert_eq!(
            events("<!DOCTYPE html><!-- c --><!--><!x><a href='x"),
            vec![
                Event::Decl("DOCTYPE html".into()),
                Event::Comment(" c ".into()),
                Event::Comment(String::new()),
                Event::Comment("x".into()),
            ]
        );
    }

    #[test]
    fn positions_count_lines_and_characters() {
        let parsed = parse("é\n<a>\n  <b>", false);
        let starts: Vec<_> = parsed.iter().map(|p| (p.line, p.offset)).collect();
        assert_eq!(starts, vec![(1, 0), (2, 0), (2, 3), (3, 2)]);
    }
}
