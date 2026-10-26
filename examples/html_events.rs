//! Differential probe for `htmlparser::parse` against CPython's `HTMLParser`.
//!
//! Each stdin line is a path to a UTF-8 document. Each stdout line is a JSON
//! array with the document's events for `convert_charrefs` true, then false:
//! `[handler, args..., line, offset]`, where `handler` is the `handle_*` name
//! and a tag's attributes are `[name, value or null]` pairs.
use std::io::{BufRead, Write};

use serde_json::{Value, json};
use tax_radar_au::htmlparser::{self, Event};

fn events(text: &str, convert_charrefs: bool) -> Value {
    htmlparser::parse(text, convert_charrefs)
        .into_iter()
        .map(|p| {
            let mut row = match p.event {
                Event::StartTag(tag, attrs) => vec![json!("starttag"), json!(tag), json!(attrs)],
                Event::StartEndTag(tag, attrs) => {
                    vec![json!("startendtag"), json!(tag), json!(attrs)]
                }
                Event::EndTag(tag) => vec![json!("endtag"), json!(tag)],
                Event::Data(data) => vec![json!("data"), json!(data)],
                Event::Comment(data) => vec![json!("comment"), json!(data)],
                Event::Decl(data) => vec![json!("decl"), json!(data)],
                Event::Pi(data) => vec![json!("pi"), json!(data)],
                Event::UnknownDecl(data) => vec![json!("unknown_decl"), json!(data)],
                Event::CharRef(name) => vec![json!("charref"), json!(name)],
                Event::EntityRef(name) => vec![json!("entityref"), json!(name)],
            };
            row.extend([json!(p.line), json!(p.offset)]);
            Value::Array(row)
        })
        .collect()
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let path = line.expect("stdin line");
        let text = std::fs::read_to_string(&path).expect("UTF-8 document");
        let answer = json!([events(&text, true), events(&text, false)]);
        // One answer per request: the caller sends the next path after reading it.
        writeln!(stdout, "{answer}").expect("stdout");
        stdout.flush().expect("stdout");
    }
}
