//! Python `re` patterns run as `fancy_regex` patterns with Python's semantics.
//!
//! The scripts' patterns are written in Python syntax. [`re`] translates one
//! with its flags, once, and caches it as `re.compile` does. Python's `\s`,
//! `\w`, `\d` and `\b` follow `str.isspace`, `str.isalnum` and `str.isdecimal`,
//! a non-multiline `$` also matches before a final newline, and `re.IGNORECASE`
//! lets an ASCII letter match the non-ASCII letters Python pairs with it.
use crate::py;
use fancy_regex::{Captures, Regex, RegexBuilder, RegexInput};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::BuildHasherDefault;
use std::sync::{Arc, LazyLock, Mutex};

pub const I: u8 = 1;
pub const S: u8 = 2;
pub const M: u8 = 4;

/// A compiled pattern; matching never fails for lack of backtracking budget.
///
/// A pattern using `\b` or `\B` also gets a copy with the regex crate's own boundary,
/// which lets the fast engine run. Its word characters differ from Python's only on
/// the WORD_DIFF code points, so it gives the same matches on text without them.
pub struct Re {
    exact: Regex,
    native: Option<Regex>,
    /// The same patterns compiled by the regex crate itself, where they need no
    /// lookaround or backreference. `fancy_regex` hands such a pattern to the
    /// regex crate anyway, but its own search loop around it is many times
    /// slower, so matching without groups goes to these directly.
    plain_exact: Option<regex::Regex>,
    plain_native: Option<regex::Regex>,
}

fn build(pattern: &str, translated: &str) -> Regex {
    RegexBuilder::new(translated)
        .backtrack_limit(usize::MAX)
        .build()
        .unwrap_or_else(|e| panic!("pattern {pattern:?} as {translated:?}: {e}"))
}

/// FxHash: the lookup runs on every call (the HTML parser asks per token), and
/// the keys are the program's own patterns, so SipHash's flood resistance buys
/// nothing here.
#[derive(Default)]
struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(word))
                .wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Per flags, by pattern: a hit borrows the pattern instead of copying it.
type Cache = Mutex<HashMap<u8, HashMap<String, Arc<Re>, BuildHasherDefault<FxHasher>>>>;

/// `re.compile(pattern, flags)`, cached.
pub fn re(pattern: &str, flags: u8) -> Arc<Re> {
    static CACHE: LazyLock<Cache> = LazyLock::new(Default::default);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let by_pattern = cache.entry(flags).or_default();
    if let Some(compiled) = by_pattern.get(pattern) {
        return compiled.clone();
    }
    let translated = translate(pattern, flags);
    let native = translate_with(pattern, flags, true);
    let plain = |p: &str| regex::Regex::new(p).ok();
    let compiled = Arc::new(Re {
        plain_native: (native != translated).then(|| plain(&native)).flatten(),
        plain_exact: plain(&translated),
        native: (native != translated).then(|| build(pattern, &native)),
        exact: build(pattern, &translated),
    });
    by_pattern.insert(pattern.to_owned(), compiled.clone());
    compiled
}

/// `re.fullmatch(pattern, text, flags)`: the pattern anchored at both ends, so alternatives
/// and quantifiers backtrack until the whole text matches.
pub fn fullmatch(pattern: &str, flags: u8, text: &str) -> bool {
    re(&format!(r"\A(?:{pattern})\Z"), flags).is_match(text)
}

/// `re.escape(text)`.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() && !c.is_ascii_alphanumeric() && c != '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Re {
    /// Whether `text` lets the regex crate's own `\b` stand in for Python's.
    fn native_fits(&self, text: &str) -> bool {
        self.native.is_some() && (text.is_ascii() || !text.chars().any(py::word_diff))
    }

    fn engine(&self, text: &str) -> &Regex {
        match &self.native {
            Some(native) if self.native_fits(text) => native,
            _ => &self.exact,
        }
    }

    /// The regex crate's compilation of the engine `text` gets, if it has one.
    fn plain(&self, text: &str) -> Option<&regex::Regex> {
        if self.native_fits(text) {
            self.plain_native.as_ref()
        } else {
            self.plain_exact.as_ref()
        }
    }

    pub fn search<'t>(&self, text: &'t str) -> Option<Caps<'t>> {
        self.search_at(text, 0)
    }

    /// `pattern.search(text, pos)` with `pos` as a byte offset.
    pub fn search_at<'t>(&self, text: &'t str, pos: usize) -> Option<Caps<'t>> {
        if let Some(plain) = self.plain(text) {
            return plain.captures_at(text, pos).map(Caps::Plain);
        }
        self.engine(text)
            .captures_from_pos(text, pos)
            .expect("unbounded backtracking")
            .map(Caps::Fancy)
    }

    /// `pattern.match(text, pos)`: a match that starts at `pos`. An anchored search,
    /// rather than an unanchored one checked afterwards, stops at `pos` instead of
    /// scanning the rest of the text whenever nothing matches there.
    pub fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<Caps<'t>> {
        self.engine(text)
            .captures_input(RegexInput::new(text).from_pos(pos).anchored(true))
            .expect("unbounded backtracking")
            .map(Caps::Fancy)
    }

    pub fn is_match(&self, text: &str) -> bool {
        if let Some(plain) = self.plain(text) {
            return plain.is_match(text);
        }
        self.engine(text)
            .is_match(text)
            .expect("unbounded backtracking")
    }

    pub fn captures_iter<'r, 't>(&'r self, text: &'t str) -> Box<dyn Iterator<Item = Caps<'t>> + 'r>
    where
        't: 'r,
    {
        match self.plain(text) {
            Some(plain) => Box::new(plain.captures_iter(text).map(Caps::Plain)),
            None => Box::new(
                self.engine(text)
                    .captures_iter(text)
                    .map(|c| Caps::Fancy(c.expect("unbounded backtracking"))),
            ),
        }
    }

    /// Whether the regex crate and `fancy_regex` find the same groups at the
    /// same places in `text`, for the compilation `text` gets: the fast path
    /// must not change a match. For the fuzz target.
    #[doc(hidden)]
    pub fn engines_agree(&self, text: &str) -> bool {
        let Some(plain) = self.plain(text) else {
            return true;
        };
        let spans = |c: &Caps<'_>| -> Vec<Option<(usize, usize)>> {
            (0..c.len())
                .map(|i| c.get(i).map(|m| (m.start(), m.end())))
                .collect()
        };
        let fast: Vec<_> = plain
            .captures_iter(text)
            .map(|c| spans(&Caps::Plain(c)))
            .collect();
        let slow: Vec<_> = self
            .engine(text)
            .captures_iter(text)
            .map(|c| spans(&Caps::Fancy(c.expect("unbounded backtracking"))))
            .collect();
        fast == slow
    }

    /// `pattern.finditer(text)` as the text of each whole match. Without
    /// groups to fill, a pattern with no lookaround runs on the regex crate.
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> Box<dyn Iterator<Item = &'t str> + 'r>
    where
        't: 'r,
    {
        match self.plain(text) {
            Some(plain) => Box::new(plain.find_iter(text).map(|m| m.as_str())),
            None => Box::new(
                self.engine(text)
                    .find_iter(text)
                    .map(|m| m.expect("unbounded backtracking").as_str()),
            ),
        }
    }

    /// `pattern.findall(text)` for a pattern with at most one group.
    pub fn findall(&self, text: &str) -> Vec<String> {
        self.captures_iter(text)
            .map(|c| {
                c.get(usize::from(c.len() > 1))
                    .map_or(String::new(), |m| m.as_str().to_owned())
            })
            .collect()
    }

    /// `pattern.findall(text)` for a pattern with several groups: each group's text, or
    /// an empty string for a group that did not take part, as Python returns them.
    pub fn findall_groups(&self, text: &str) -> Vec<Vec<String>> {
        self.captures_iter(text)
            .map(|c| {
                (1..c.len())
                    .map(|g| c.get(g).map_or(String::new(), |m| m.as_str().to_owned()))
                    .collect()
            })
            .collect()
    }

    /// `pattern.split(text)` for a pattern without groups.
    pub fn split<'t>(&self, text: &'t str) -> Vec<&'t str> {
        let mut pieces = Vec::new();
        let mut last = 0;
        for c in self.captures_iter(text) {
            let m = c.get(0).expect("group 0");
            pieces.push(&text[last..m.start()]);
            last = m.end();
        }
        pieces.push(&text[last..]);
        pieces
    }

    /// `pattern.sub(replacement, text)` with a literal replacement.
    pub fn sub(&self, replacement: &str, text: &str) -> String {
        self.sub_with(text, |_| replacement.to_owned())
    }

    /// `pattern.sub(function, text)`.
    pub fn sub_with(&self, text: &str, mut f: impl FnMut(&Caps<'_>) -> String) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for c in self.captures_iter(text) {
            let m = c.get(0).expect("group 0");
            out.push_str(&text[last..m.start()]);
            out.push_str(&f(&c));
            last = m.end();
        }
        out.push_str(&text[last..]);
        out
    }
}

/// One match's groups, from whichever engine ran the pattern.
pub enum Caps<'t> {
    Plain(regex::Captures<'t>),
    Fancy(Captures<'t, str>),
}

impl std::ops::Index<usize> for Caps<'_> {
    type Output = str;

    /// Group `i`'s text; panics where it did not take part, as both engines do.
    fn index(&self, i: usize) -> &str {
        match self {
            Caps::Plain(c) => &c[i],
            Caps::Fancy(c) => &c[i],
        }
    }
}

/// One group's text and where it starts in the searched text.
#[derive(Clone, Copy, Debug)]
pub struct Span<'t> {
    text: &'t str,
    start: usize,
}

impl<'t> Span<'t> {
    pub fn start(&self) -> usize {
        self.start
    }

    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }

    pub fn as_str(&self) -> &'t str {
        self.text
    }
}

impl<'t> Caps<'t> {
    /// Group `i`, or None where it did not take part.
    pub fn get(&self, i: usize) -> Option<Span<'t>> {
        match self {
            Caps::Plain(c) => c.get(i).map(|m| Span {
                text: m.as_str(),
                start: m.start(),
            }),
            Caps::Fancy(c) => c.get(i).map(|m| Span {
                text: m.as_str(),
                start: m.start(),
            }),
        }
    }

    /// The number of groups, group 0 included.
    pub fn len(&self) -> usize {
        match self {
            Caps::Plain(c) => c.len(),
            Caps::Fancy(c) => c.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Group `name`, or None where it did not take part.
    pub fn name(&self, name: &str) -> Option<Span<'t>> {
        match self {
            Caps::Plain(c) => c.name(name).map(|m| Span {
                text: m.as_str(),
                start: m.start(),
            }),
            Caps::Fancy(c) => c.name(name).map(|m| Span {
                text: m.as_str(),
                start: m.start(),
            }),
        }
    }
}

fn hex_escape(out: &mut String, c: char) {
    let _ = write!(out, "\\x{{{:X}}}", c as u32);
}

/// What one literal character matches, as class members (no brackets).
fn members(out: &mut String, c: char, flags: u8) {
    if flags & I != 0 && c.is_ascii_alphabetic() {
        let letter = py::ignorecase_letter(c);
        out.push_str(&letter[1..letter.len() - 1]);
    } else if flags & I != 0 && c.is_alphabetic() {
        // shortcut: a non-ASCII letter matches its own simple lower and upper case, which
        // agrees with Python's lower()-based matching for the letters these patterns hold.
        for variant in [c]
            .into_iter()
            .chain(c.to_lowercase())
            .chain(c.to_uppercase())
        {
            hex_escape(out, variant);
        }
    } else {
        hex_escape(out, c);
    }
}

fn literal(out: &mut String, c: char, flags: u8) {
    if flags & I != 0 && c.is_alphabetic() {
        out.push('[');
        members(out, c, flags);
        out.push(']');
    } else {
        hex_escape(out, c);
    }
}

fn class_body(class: &str) -> &str {
    &class[1..class.len() - 1]
}

struct Parser<'p> {
    chars: Vec<char>,
    i: usize,
    pattern: &'p str,
    native: bool,
}

/// One escape or character, as Python reads it inside or outside a class.
enum Atom {
    Char(char),
    Class(&'static str, bool),
    Other(String),
}

impl Parser<'_> {
    fn next(&mut self) -> char {
        let c = *self
            .chars
            .get(self.i)
            .unwrap_or_else(|| panic!("truncated {:?}", self.pattern));
        self.i += 1;
        c
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.i + offset).copied()
    }

    fn hex(&mut self, digits: usize) -> char {
        let text: String = (0..digits).map(|_| self.next()).collect();
        char::from_u32(u32::from_str_radix(&text, 16).expect("hex escape")).expect("code point")
    }

    fn atom(&mut self, in_class: bool) -> Atom {
        let c = self.next();
        if c != '\\' {
            return Atom::Char(c);
        }
        let e = self.next();
        let class =
            |table: &'static LazyLock<String>, negated| Atom::Class(table.as_str(), negated);
        match e {
            's' => class(&py::S, false),
            'w' => class(&py::W, false),
            'd' => class(&py::D, false),
            'S' => class(&py::S, true),
            'W' => class(&py::W, true),
            'D' => class(&py::D, true),
            'x' => Atom::Char(self.hex(2)),
            'u' => Atom::Char(self.hex(4)),
            'U' => Atom::Char(self.hex(8)),
            'n' => Atom::Char('\n'),
            't' => Atom::Char('\t'),
            'r' => Atom::Char('\r'),
            'f' => Atom::Char('\x0c'),
            'v' => Atom::Char('\x0b'),
            'a' => Atom::Char('\x07'),
            '0' => Atom::Char('\0'),
            'b' if in_class => Atom::Char('\x08'),
            'b' if self.native => Atom::Other("\\b".into()),
            'B' if self.native => Atom::Other("\\B".into()),
            'b' => Atom::Other(py::word_boundary()),
            'B' => {
                let w = &*py::W;
                Atom::Other(format!("(?:(?<={w})(?={w})|(?<!{w})(?!{w}))"))
            }
            'A' => Atom::Other("\\A".into()),
            'Z' | 'z' => Atom::Other("\\z".into()),
            '1'..='9' => Atom::Other(format!("\\{e}")),
            _ if e.is_ascii_alphanumeric() => {
                panic!("unsupported escape \\{e} in {:?}", self.pattern)
            }
            _ => Atom::Char(e),
        }
    }

    fn class(&mut self, out: &mut String, flags: u8) {
        out.push('[');
        if self.peek(0) == Some('^') {
            self.i += 1;
            out.push('^');
        }
        let mut first = true;
        loop {
            if self.peek(0) == Some(']') && !first {
                self.i += 1;
                break;
            }
            first = false;
            match self.atom(true) {
                Atom::Char(lo)
                    if self.peek(0) == Some('-') && self.peek(1).is_some_and(|c| c != ']') =>
                {
                    self.i += 1;
                    let Atom::Char(hi) = self.atom(true) else {
                        panic!("bad range in {:?}", self.pattern)
                    };
                    hex_escape(out, lo);
                    out.push('-');
                    hex_escape(out, hi);
                    if flags & I != 0 {
                        for cp in (lo as u32)..=(hi as u32).min(0x7f) {
                            let c = char::from_u32(cp).unwrap();
                            if c.is_ascii_alphabetic() {
                                members(out, c, flags);
                            }
                        }
                    }
                }
                Atom::Char(c) => members(out, c, flags),
                Atom::Class(table, false) => out.push_str(class_body(table)),
                Atom::Class(table, true) => {
                    // Rust classes nest, so a negated Python class becomes a nested one.
                    let _ = write!(out, "[^{}]", class_body(table));
                }
                Atom::Other(_) => panic!("unsupported class escape in {:?}", self.pattern),
            }
        }
        out.push(']');
    }

    /// Open a group and return the flags inside it: `(?i:...)` and `(?-i:...)`
    /// scope flags to the group, as Python allows.
    fn group(&mut self, out: &mut String, flags: u8) -> u8 {
        if self.peek(0) == Some('?') {
            let mut j = 1;
            let mut on = 0u8;
            let mut off = 0u8;
            let mut negative = false;
            while let Some(c) = self.peek(j) {
                let bit = match c {
                    'i' => I,
                    'm' => M,
                    's' => S,
                    '-' if !negative => {
                        negative = true;
                        j += 1;
                        continue;
                    }
                    _ => break,
                };
                if negative {
                    off |= bit
                } else {
                    on |= bit
                }
                j += 1;
            }
            if j > 1 && self.peek(j) == Some(':') {
                self.i += j + 1;
                out.push_str("(?:");
                return (flags | on) & !off;
            }
        }
        self.plain_group(out, flags);
        flags
    }

    fn plain_group(&mut self, out: &mut String, flags: u8) {
        // (?P=name) is a named backreference, not a group.
        if self.peek(0) == Some('?') && self.peek(1) == Some('P') && self.peek(2) == Some('=') {
            self.i += 3;
            let mut name = String::new();
            loop {
                let c = self.next();
                if c == ')' {
                    break;
                }
                name.push(c);
            }
            if flags & I != 0 {
                let _ = write!(out, "(?i:\\k<{name}>)");
            } else {
                let _ = write!(out, "\\k<{name}>");
            }
            return;
        }
        out.push('(');
        if self.peek(0) != Some('?') {
            return;
        }
        self.i += 1;
        out.push('?');
        match self.next() {
            c @ (':' | '=' | '!') => out.push(c),
            '<' => {
                let kind = self.next();
                assert!(matches!(kind, '=' | '!'), "group in {:?}", self.pattern);
                out.push('<');
                out.push(kind);
            }
            'P' => {
                assert_eq!(
                    self.next(),
                    '<',
                    "named backreference in {:?}",
                    self.pattern
                );
                out.push_str("P<");
                loop {
                    let c = self.next();
                    out.push(c);
                    if c == '>' {
                        break;
                    }
                }
            }
            other => panic!("unsupported group (?{other} in {:?}", self.pattern),
        }
    }

    /// A `{m,n}` quantifier, or a literal brace as Python reads a malformed one.
    fn brace(&mut self, out: &mut String, flags: u8) {
        let start = self.i;
        let mut text = String::new();
        while let Some(c) = self.peek(0) {
            if c.is_ascii_digit() || c == ',' {
                text.push(c);
                self.i += 1;
            } else {
                break;
            }
        }
        let valid = self.peek(0) == Some('}') && !text.is_empty() && text.matches(',').count() <= 1;
        if valid {
            self.i += 1;
            let text = if text.starts_with(',') {
                format!("0{text}")
            } else {
                text
            };
            let _ = write!(out, "{{{text}}}");
        } else {
            self.i = start;
            literal(out, '{', flags);
        }
    }
}

/// Python pattern syntax and flags to a `fancy_regex` pattern.
pub fn translate(pattern: &str, flags: u8) -> String {
    translate_with(pattern, flags, false)
}

/// `translate`, with `\b` and `\B` left to the regex crate when `native` is set.
fn translate_with(pattern: &str, flags: u8, native: bool) -> String {
    // Global inline flags such as (?m) or (?si) may only open a Python pattern.
    let mut flags = flags;
    let mut pattern = pattern;
    if let Some(rest) = pattern.strip_prefix("(?")
        && let Some(end) = rest.find(')')
        && end > 0
        && rest[..end].chars().all(|c| "ims".contains(c))
    {
        for c in rest[..end].chars() {
            flags |= match c {
                'i' => I,
                'm' => M,
                _ => S,
            };
        }
        pattern = &rest[end + 1..];
    }
    let mut p = Parser {
        chars: pattern.chars().collect(),
        i: 0,
        pattern,
        native,
    };
    let mut out = String::with_capacity(pattern.len() * 2);
    let mut scopes: Vec<u8> = Vec::new();
    while let Some(c) = p.peek(0) {
        match c {
            '[' => {
                p.i += 1;
                p.class(&mut out, flags);
            }
            '(' => {
                p.i += 1;
                scopes.push(flags);
                flags = p.group(&mut out, flags);
            }
            '{' => {
                p.i += 1;
                p.brace(&mut out, flags);
            }
            ')' => {
                p.i += 1;
                out.push(c);
                flags = scopes.pop().unwrap_or(flags);
            }
            '|' | '*' | '+' | '?' => {
                p.i += 1;
                out.push(c);
            }
            '.' => {
                p.i += 1;
                out.push_str(if flags & S != 0 { "(?s:.)" } else { "." });
            }
            '^' => {
                p.i += 1;
                out.push_str(if flags & M != 0 { "(?m:^)" } else { "^" });
            }
            '$' => {
                p.i += 1;
                out.push_str(if flags & M != 0 {
                    "(?m:$)"
                } else {
                    "(?=\\n?\\z)"
                });
            }
            _ => match p.atom(false) {
                Atom::Char(c) => literal(&mut out, c, flags),
                Atom::Class(table, false) => out.push_str(table),
                Atom::Class(table, true) => {
                    let _ = write!(out, "[^{}]", class_body(table));
                }
                // Under IGNORECASE a backreference matches the group's text in any case.
                Atom::Other(text)
                    if flags & I != 0 && text.as_bytes().get(1).is_some_and(u8::is_ascii_digit) =>
                {
                    let _ = write!(out, "(?i:{text})");
                }
                Atom::Other(text) => out.push_str(&text),
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_at_is_anchored_and_sees_behind_pos() {
        // pattern.match(text, pos): no match at pos is None even when one follows.
        assert!(re("b+", 0).match_at("abba", 0).is_none());
        assert_eq!(&re("b+", 0).match_at("abba", 1).expect("match")[0], "bb");
        // Lookbehind and \b look at the text before pos, as Python's do.
        assert!(re("(?<=a)b", 0).match_at("ab", 1).is_some());
        assert!(re("(?<=c)b", 0).match_at("ab", 1).is_none());
        assert!(re(r"\bb", 0).match_at("ab", 1).is_none());
        assert!(re(r"\bb", 0).match_at("a b", 2).is_some());
        // An alternation keeps Python's first-alternative choice at pos.
        assert_eq!(&re("a|ab", 0).match_at("xab", 1).expect("match")[0], "a");
        assert_eq!(&re("(x)?y", 0).match_at("zy", 1).expect("match")[0], "y");
    }

    /// `(whole match, span, groups)` for `match_at`, as CPython 3.15 reports it.
    type Found<'a> = Option<(&'a str, (usize, usize), Vec<Option<&'a str>>)>;

    fn found<'t>(pattern: &str, text: &'t str, pos: usize) -> Found<'t> {
        re(pattern, 0).match_at(text, pos).map(|m| {
            let whole = m.get(0).expect("group 0");
            let groups = (1..m.len()).map(|i| m.get(i).map(|g| g.as_str())).collect();
            (whole.as_str(), (whole.start(), whole.end()), groups)
        })
    }

    #[test]
    fn match_at_agrees_with_cpython_at_nonzero_positions() {
        // Each expectation is CPython 3.15's re.compile(pattern).match(text, pos).
        assert_eq!(found("(?i)a", "xA", 1), Some(("A", (1, 2), vec![])));
        assert_eq!(found("(?i:a)b", "xAb", 1), Some(("Ab", (1, 3), vec![])));
        assert_eq!(found("(?i:a)b", "xAB", 1), None);
        assert_eq!(found("(?m)^a", "x\na", 2), Some(("a", (2, 3), vec![])));
        assert_eq!(found("(?m)^a", "xa", 1), None);
        assert_eq!(found("^a", "xa", 1), None);
        assert_eq!(found(r"\Ab", "ab", 1), None);
        assert_eq!(found("a$", "xa", 1), Some(("a", (1, 2), vec![])));
        assert_eq!(found("a|ab", "xab", 1), Some(("a", (1, 2), vec![])));
        assert_eq!(found("a*", "xb", 1), Some(("", (1, 1), vec![])));
        assert_eq!(found("a*", "xaab", 1), Some(("aa", (1, 3), vec![])));
        assert_eq!(found("", "abc", 3), Some(("", (3, 3), vec![])));
        assert_eq!(found("(?<=a)b", "ab", 1), Some(("b", (1, 2), vec![])));
        assert_eq!(found(r"\bb", "ab", 1), None);
        assert_eq!(found(r"\bb", "a b", 2), Some(("b", (2, 3), vec![])));
        // An optional group that took no part is None, not "" (fancy-regex 0.19.0's defect).
        assert_eq!(found(r"(\w+)?", "", 0), Some(("", (0, 0), vec![None])));
        assert_eq!(found(r"(\w+)?", "x-", 1), Some(("", (1, 1), vec![None])));
        assert_eq!(found("(a)|b", "xb", 1), Some(("b", (1, 2), vec![None])));
    }

    #[test]
    fn attribute_captures_match_cpython() {
        // The HTML parser's pattern at the attribute after "<a ".
        let groups = |text| {
            found(crate::htmlparser::ATTRFIND_TOLERANT, text, 3)
                .map(|(whole, _, groups)| (whole, groups))
        };
        assert_eq!(groups("<a b>"), Some(("b", vec![Some("b"), None, None])));
        assert_eq!(
            groups("<a b=>"),
            Some(("b=", vec![Some("b"), Some("="), Some("")]))
        );
        assert_eq!(
            groups(r#"<a b="">"#),
            Some((r#"b="""#, vec![Some("b"), Some(r#"="""#), Some(r#""""#)]))
        );
        assert_eq!(
            groups("<a b=''>"),
            Some(("b=''", vec![Some("b"), Some("=''"), Some("''")]))
        );
        assert_eq!(
            groups("<a b=c d>"),
            Some(("b=c ", vec![Some("b"), Some("=c"), Some("c")]))
        );
        assert_eq!(
            groups(r#"<a b = "x y">"#),
            Some((
                r#"b = "x y""#,
                vec![Some("b"), Some(r#" = "x y""#), Some(r#""x y""#)]
            ))
        );
        assert_eq!(
            groups("<a b=/>"),
            Some(("b=/", vec![Some("b"), Some("=/"), Some("/")]))
        );
    }

    #[test]
    fn python_classes_boundaries_and_anchors() {
        assert!(re(r"a\sb", 0).is_match("a\x1cb"));
        assert!(!re(r"a\sb", 0).is_match("a\u{200b}b"));
        assert!(re(r"<tr\b", 0).is_match("<tr\u{301}"));
        assert!(re(r"^x$", 0).is_match("x\n"));
        assert!(!re(r"^x$", 0).is_match("x\n\n"));
        assert!(re(r"^b$", M).is_match("a\nb\nc"));
        assert!(re(r"a.b", S).is_match("a\nb"));
        assert!(!re(r"a.b", 0).is_match("a\nb"));
        assert!(re(r"[\s/]x", 0).is_match("\x1fx"));
        assert!(re(r"[^\S]", 0).is_match("\x1c"));
        assert_eq!(
            re(r"\Z", 0).search("ab\n").unwrap().get(0).unwrap().start(),
            3
        );
        assert!(re(r"a{2}", 0).is_match("aa") && re(r"a{", 0).is_match("a{"));
        assert!(re(r"(?P<x>a)(?=b)", 0).is_match("ab"));
    }

    #[test]
    fn ignorecase_pairs_ascii_letters_with_python_extras() {
        assert!(re("script", I).is_match("SCR\u{130}PT"));
        assert!(re("^[a-z]+$", I).is_match("\u{17f}\u{212a}AB"));
        assert!(re("^[^a-z]$", I).is_match("1"));
        assert!(!re("^[^a-z]$", I).is_match("\u{212a}"));
        assert!(!re("k", 0).is_match("K"));
        assert!(re(r"(?P<name>[a-z][\w:-]*)", I).is_match("Ab"));
        assert!(re(r"<(script)>.*?</\1>", I | S).is_match("<SCRIPT>x</script>"));
        assert!(!re(r"<(script)>.*?</\1>", S).is_match("<SCRIPT>x</script>"));
    }

    #[test]
    fn escape_matches_re_escape() {
        assert_eq!(escape("a.b-c_d/é"), "a\\.b\\-c_d\\/é");
        assert!(re(&format!("^{}$", escape("a.b(c)")), 0).is_match("a.b(c)"));
    }
}
