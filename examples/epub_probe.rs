//! Differential probe for `fadden::epub` against `fadden/download.py`.
//!
//! Each stdin line is an archive path. Each stdout line is `E<hex error>` when
//! `open_epub` fails, or one `<hex name>,<file_size>,<result>` per member in
//! `namelist()` order, separated by `;`, where the result is the member's
//! bytes in hex or `E<hex error>`. An error is `<class>: <str(exc)>`. With
//! EPUB_PROBE_DIGEST set, member bytes are `H<sha256 hex>` instead, for
//! members too large to print.
use std::io::{BufRead, Write};

use sha2::{Digest, Sha256};
use tax_radar_au::fadden::Exc;
use tax_radar_au::fadden::epub;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn error(exc: &Exc) -> String {
    let class = exc.class().rsplit('.').next().unwrap_or_default();
    format!("E{}", hex(format!("{class}: {exc}").as_bytes()))
}

fn main() {
    let digest = std::env::var_os("EPUB_PROBE_DIGEST").is_some();
    let stdin = std::io::stdin();
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let path = line.expect("stdin line");
        let answer = match epub::open(&path) {
            Err(exc) => error(&exc),
            Ok(archive) => archive
                .members
                .iter()
                .map(|m| {
                    let result = match archive.read(&m.filename) {
                        Ok(bytes) if digest => format!("H{}", hex(&Sha256::digest(&bytes))),
                        Ok(bytes) => hex(&bytes),
                        Err(exc) => error(&exc),
                    };
                    format!("{},{},{result}", hex(m.filename.as_bytes()), m.file_size)
                })
                .collect::<Vec<_>>()
                .join(";"),
        };
        writeln!(stdout, "{answer}").expect("stdout");
    }
}
