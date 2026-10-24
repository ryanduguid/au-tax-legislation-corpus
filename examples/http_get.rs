//! Parity probe for `fadden::http`'s network client against urllib.
//!
//! For each URL argument, runs the attempt loop `fetch_json` and the rulings
//! fetch share (one redirect record, `TRIES` attempts, stopping at the first
//! 2xx) and prints one JSON line: each attempt's outcome, `["ok", final URL,
//! Content-Type, body length, body SHA-256]`, `["status", code]` or
//! `["error", exception name]`.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::http::{self, Failure, Redirects};

fn main() {
    let mut net = http::connect();
    for url in std::env::args().skip(1) {
        let mut redirects = Redirects::default();
        let mut outcomes = Vec::new();
        for number in 1..=http::TRIES {
            http::attempt(net.as_mut(), number);
            let outcome: Value = match net.get(&url, None, &mut redirects) {
                Ok(r) => json!([
                    "ok",
                    r.url,
                    r.content_type,
                    r.body.len(),
                    Sha256::digest(&r.body)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                ]),
                Err(Failure::Status(code)) => json!(["status", code]),
                Err(Failure::Transport(name)) => json!(["error", name]),
            };
            let done = outcome[0] == "ok";
            outcomes.push(outcome);
            if done {
                break;
            }
        }
        println!("{}", json!([url, outcomes]));
    }
}
