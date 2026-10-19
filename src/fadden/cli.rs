//! `fadden <stage> [arguments]`: run one ported corpus stage.
use std::io::Write;

use super::{Run, as_at, compare_builds};
use crate::argparse::Exit;

/// The stages this binary runs; the others remain in the Python package.
pub const STAGES: [&str; 2] = ["as_at", "compare_builds"];

const USAGE: &str = "usage: fadden {as_at,compare_builds} [arguments ...]\n";

/// Text-mode output: newlines become the platform's line separator.
fn text_mode(bytes: &[u8]) -> Vec<u8> {
    if cfg!(windows) {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            if b == b'\n' {
                out.push(b'\r');
            }
            out.push(b);
        }
        out
    } else {
        bytes.to_vec()
    }
}

/// Run the command line; the result is the exit status.
pub fn main(argv: &[String]) -> i32 {
    let mut run = Run::default();
    let outcome = match argv.split_first() {
        Some((stage, rest)) if stage == "as_at" => as_at::main(rest, &mut run),
        Some((stage, rest)) if stage == "compare_builds" => compare_builds::main(rest, &mut run),
        Some((flag, _)) if flag == "-h" || flag == "--help" => Err(Exit::Help(format!(
            "{USAGE}\nRun one ported corpus-builder stage; `python -m fadden` forwards these stages here.\n"
        ))),
        _ => Err(Exit::Error(format!(
            "{USAGE}fadden: error: choose a stage: {}\n",
            STAGES.join(", ")
        ))),
    };
    let code = match outcome {
        Ok(code) => code,
        Err(Exit::Help(text)) => {
            run.stdout.extend_from_slice(text.as_bytes());
            0
        }
        Err(Exit::Error(text)) => {
            run.stderr.extend_from_slice(text.as_bytes());
            2
        }
    };
    let _ = std::io::stdout().write_all(&text_mode(&run.stdout));
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().write_all(&text_mode(&run.stderr));
    code
}
