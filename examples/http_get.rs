//! Parity probe for `fadden::http`'s network client against urllib.
//!
//! Usage: `http_get [--limit N] URL...`. For each URL, runs the attempt loop
//! `fetch_json` and the rulings fetch share (one redirect record, `TRIES`
//! attempts, stopping at the first 2xx; without the waits between attempts,
//! which the stage tests check) and prints one JSON line: each attempt's
//! outcome, `["ok", final URL, Content-Type, body length, body SHA-256]`,
//! `["status", code]` or `["error", exception name]`, or `["raise",
//! exception name]` for a `ValueError` that ends the loop. `--limit` reads at most
//! N bytes of a body, as the rulings stage does.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tax_radar_au::fadden::http::{self, Failure, Request};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut limit = None;
    if args.first().is_some_and(|a| a == "--limit") {
        limit = Some(args[1].parse::<usize>().expect("--limit N"));
        args.drain(..2);
    }
    let mut net = http::connect();
    for url in args {
        let mut request = Request::default();
        let mut outcomes = Vec::new();
        for _ in 1..=http::TRIES {
            let outcome: Value = match net.get(&url, limit, &mut request) {
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
                Err(Failure::Uncaught(_)) => json!(["raise", "ValueError"]),
            };
            let done = outcome[0] != "status" && outcome[0] != "error";
            outcomes.push(outcome);
            if done {
                break;
            }
        }
        println!("{}", json!([url, outcomes]));
    }
}
