//! `textwrap.wrap(text, width)` with the defaults argparse uses: whitespace
//! collapsed, words split after hyphens, and words longer than a line broken.

use crate::pytext;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `[^\d\W]` in Python's regex syntax: a word character that is not a digit.
fn is_letter(c: char) -> bool {
    is_word(c) && pytext::decimal_value(c as u32).is_none()
}

/// `TextWrapper.wordsep_re.split`: runs of spaces, and words split after a
/// hyphen that joins two letters.
/// shortcut: the em-dash rules are left out; the parsers' fixed help texts
/// never put "--" between words.
fn chunks(text: &[char]) -> Vec<String> {
    let letter = |i: Option<usize>| i.and_then(|i| text.get(i)).is_some_and(|&c| is_letter(c));
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..text.len() {
        let boundary = if text[i] == ' ' {
            text.get(i + 1) != Some(&' ')
        } else if text.get(i + 1) == Some(&' ') {
            true
        } else if text[i] == '-' {
            // (?<=lt{2}-) or (?<=lt-lt-), then (?=lt -? lt).
            let behind = (letter(i.checked_sub(2)) && letter(i.checked_sub(1)))
                || (letter(i.checked_sub(3))
                    && text.get(i.wrapping_sub(2)) == Some(&'-')
                    && letter(i.checked_sub(1)));
            let ahead = letter(Some(i + 1))
                && (letter(Some(i + 2)) || (text.get(i + 2) == Some(&'-') && letter(Some(i + 3))));
            behind && ahead
        } else {
            false
        };
        if boundary || i + 1 == text.len() {
            out.push(text[start..=i].iter().collect());
            start = i + 1;
        }
    }
    out
}

/// `TextWrapper._handle_long_word`: break the next chunk to fill the line,
/// after its last hyphen when one fits.
fn break_long_word(chunks: &mut [String], line: &mut Vec<String>, space_left: usize) {
    let chunk: Vec<char> = chunks.last().expect("a chunk to break").chars().collect();
    let mut end = space_left;
    if chunk.len() > space_left
        && let Some(hyphen) = chunk[..space_left].iter().rposition(|&c| c == '-')
        && hyphen > 0
        && chunk[..hyphen].iter().any(|&c| c != '-')
    {
        end = hyphen + 1;
    }
    line.push(chunk[..end].iter().collect());
    *chunks.last_mut().expect("checked above") = chunk[end..].iter().collect();
}

/// `textwrap.wrap(text, width)`; `width` is at least 1 wherever argparse calls it.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let normalised = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chunks = chunks(&normalised.chars().collect::<Vec<_>>());
    chunks.reverse();
    let length = |s: &String| s.chars().count();
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut line: Vec<String> = Vec::new();
        let mut used = 0;
        if !lines.is_empty() && chunks.last().is_some_and(|c| c.trim().is_empty()) {
            chunks.pop();
        }
        while let Some(next) = chunks.last() {
            if used + length(next) > width {
                break;
            }
            used += length(next);
            line.push(chunks.pop().expect("checked above"));
        }
        if chunks.last().is_some_and(|c| length(c) > width) {
            // _handle_long_word (3.15) breaks only when the line has room left. No
            // room means the line already holds text, so the word waits for the next.
            let space_left = if width < 1 { 1 } else { width - used };
            if space_left > 0 {
                break_long_word(&mut chunks, &mut line, space_left);
            }
        }
        if line.last().is_some_and(|c| c.trim().is_empty()) {
            line.pop();
        }
        if !line.is_empty() {
            lines.push(line.concat());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_like_python_textwrap() {
        // textwrap.wrap("validate a human acknowledgement/escalation record", 24)
        assert_eq!(
            wrap("validate a human acknowledgement/escalation record", 24),
            ["validate a human acknowl", "edgement/escalation", "record"]
        );
        // textwrap.wrap("Run a fixed-policy, synthetic Xero review boundary.", 11)
        assert_eq!(
            wrap("Run a fixed-policy, synthetic Xero review boundary.", 11),
            [
                "Run a",
                "fixed-",
                "policy,",
                "synthetic",
                "Xero review",
                "boundary."
            ]
        );
        assert_eq!(
            wrap("show this help message and exit", 80),
            ["show this help message and exit"]
        );
        assert_eq!(
            wrap(
                "decision JSON under the bundled samples/ data or the working directory's build/",
                11
            ),
            [
                "decision",
                "JSON under",
                "the bundled",
                "samples/",
                "data or the",
                "working",
                "directory's",
                "build/"
            ]
        );
        assert_eq!(wrap("abcdefghijkl mnop", 11), ["abcdefghijk", "l mnop"]);
        assert_eq!(
            wrap("validate a human acknowledgement/escalation record", 17),
            ["validate a human", "acknowledgement/e", "scalation record"]
        );
        assert_eq!(
            wrap("a-b-c-d well-known re-enter", 7),
            ["a-b-c-d", "well-", "known", "re-", "enter"]
        );
    }
}
