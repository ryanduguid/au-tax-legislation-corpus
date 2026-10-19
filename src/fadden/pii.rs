//! The parts of `fadden/pii_patterns.py` the rulings stage uses: whether a
//! text holds a personal name beside a registration number, and which kinds of
//! contact detail (email, phone, tax file number) it holds. The patterns are
//! the Python module's, unchanged; the Python scans and gates still use it.
use crate::py;
use crate::pyre;

const TOKEN: &str = "[A-Z][A-Za-z'\u{2019}-]{1,20}";
const REGNO: &str = r"\b\d{8}\b";
const EMAIL: &str = r"\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b";
const PHONE: &str = concat!(
    r"(?<!\d)(?<!\d\.)(?:",
    r"\(0\d\)[\s-]?\d{4}[\s-]?\d{4}",
    r"|0[2-8][\s-]?\d{4}[\s-]?\d{4}",
    r"|04\d{2}[\s-]?\d{3}[\s-]?\d{3}",
    r"|\+61[\s-]?(?:\(0\)[\s-]?)?\d(?:[\s-]?\d){8}",
    r"|\+61[\s-]?\(0?\d\)[\s-]?\d(?:[\s-]?\d){7}",
    r"|1[38]00[\s-]?\d{3}[\s-]?\d{3}",
    r"|13[\s-]?\d{2}[\s-]?\d{2}",
    r")(?!\d)",
);
const TFN: &str =
    "\\b(?:tax file number|TFN)\\b\\s*(?:[.:\u{2013}-]\\s*)?(\\d(?:[\\s-]?\\d){7,8})(?![\\s-]?\\d)";
const TFN_BARE: &str = r"(?<![\d$])(?<![\d$][\s-])(\d{3}([\s-]?)\d{3}\2\d{3})(?![\s-]?\d)";
const STATUTORY: &str = concat!(
    r"\b(Act|Regulation|Schedule|Division|Subdivision|Part|Chapter|Section|",
    r"Commissioner|Minister|Treasurer|Commonwealth|Australian|Australia|Board|",
    r"Tax|Taxation|Income|Superannuation|Court|Tribunal|Determination|Notice|",
    r"Instrument|Amendment|January|February|March|April|May|June|July|August|",
    r"September|October|November|December)\b",
);
/// Weighted mod-11 check-digit sum every issued nine-digit TFN satisfies.
const TFN_WEIGHTS: [u32; 9] = [1, 4, 3, 7, 5, 8, 6, 9, 10];

fn name_pattern() -> String {
    format!(r"\b{TOKEN},?\s+{TOKEN}(?:\s+{TOKEN})?\b")
}

/// `is_statutory(candidate)`: a NAME match that is legislative vocabulary.
fn is_statutory(candidate: &str) -> bool {
    if pyre::re(STATUTORY, 0).is_match(candidate) {
        return true;
    }
    let all_caps = candidate
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|t| t.len() > 1 && t.chars().all(|c| c.is_ascii_uppercase()));
    all_caps && pyre::re(STATUTORY, pyre::I).is_match(candidate)
}

/// `has_private_person_registration_pair(text)`.
pub fn has_private_person_registration_pair(text: &str) -> bool {
    let names = pyre::re(&name_pattern(), 0)
        .captures_iter(text)
        .any(|c| !is_statutory(c.get(0).map_or("", |m| m.as_str())));
    names && pyre::re(REGNO, 0).is_match(text)
}

fn plausible_tfn(value: &str) -> bool {
    let digits: Vec<u32> = value.chars().filter_map(py::decimal_value).collect();
    digits.len() == 9
        && digits
            .iter()
            .zip(TFN_WEIGHTS)
            .map(|(d, w)| d * w)
            .sum::<u32>()
            % 11
            == 0
}

/// The kinds `contact_fingerprints(text)` yields, in its order, without the
/// digests: a bare nine-digit run counts only outside a labelled TFN and with
/// a valid check digit.
pub fn contact_kinds(text: &str) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    let labelled: Vec<(usize, usize)> = pyre::re(TFN, pyre::I)
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| (m.start(), m.end())))
        .collect();
    for (kind, pattern, flags) in [
        ("email", EMAIL, 0),
        ("phone", PHONE, 0),
        ("tfn", TFN, pyre::I),
    ] {
        if pyre::re(pattern, flags).is_match(text) {
            kinds.push(kind);
        }
    }
    let bare = pyre::re(TFN_BARE, 0).captures_iter(text).any(|c| {
        let m = c.get(1).expect("group 1 always takes part");
        !labelled
            .iter()
            .any(|&(s, e)| s <= m.start() && m.end() <= e)
            && plausible_tfn(m.as_str())
    });
    if bare {
        kinds.push("tfn");
    }
    kinds
}
