//! Times `htmlparser::parse` (convert_charrefs true, as extract runs it) over
//! the UTF-8 documents named one per stdin line; prints seconds and events.
use std::io::BufRead;
use std::time::Instant;

use tax_radar_au::htmlparser;

fn main() {
    let texts: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map(|line| std::fs::read_to_string(line.expect("stdin line")).expect("UTF-8 document"))
        .collect();
    let started = Instant::now();
    let events: usize = texts.iter().map(|t| htmlparser::parse(t, true).len()).sum();
    println!("{:.2} s, {events} events", started.elapsed().as_secs_f64());
}
