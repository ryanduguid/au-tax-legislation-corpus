//! Differential probe for `pyzlib::Decompress` against CPython's zlib.
//!
//! Each stdin line is one decompressor: calls separated by `;`, each
//! `<hex input>,<max_length>`. Each stdout line answers with one
//! `<hex output>/<hex unconsumed_tail>/<hex unused_data>/<eof 0|1>` per call,
//! separated by `;`, ending at `E` for a call that raised `zlib.error`.
use std::io::{BufRead, Write};
use tax_radar_au::pyzlib::Decompress;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex input"))
        .collect()
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line.expect("stdin line");
        let mut d = Decompress::default();
        let mut answers = Vec::new();
        for call in line.split(';').filter(|c| !c.is_empty()) {
            let (data, limit) = call.split_once(',').expect("call");
            match d.decompress(&unhex(data), limit.parse().expect("limit")) {
                Ok(out) => answers.push(format!(
                    "{}/{}/{}/{}",
                    hex(&out),
                    hex(&d.unconsumed_tail),
                    hex(&d.unused_data),
                    u8::from(d.eof)
                )),
                Err(_) => {
                    answers.push("E".to_owned());
                    break;
                }
            }
        }
        writeln!(stdout, "{}", answers.join(";")).expect("stdout");
    }
}
