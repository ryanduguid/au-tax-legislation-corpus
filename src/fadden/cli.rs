//! `fadden <stage> [arguments]`: run one ported corpus stage.
use std::io::Write;

use super::{
    Run, as_at, capture_register, compare_builds, dist, dist_verify, live_evidence,
    monitor_contract, pii_scan, probe13, publication_bundles, rates, rulings, search,
};
use crate::argparse::Exit;

/// The stages this binary runs; the others remain in the Python package.
/// `capture_register` and `export_live_evidence_bundles` are here for
/// verification only: `python -m fadden` still runs their Python stages,
/// which the publication workflow uses.
pub const STAGES: [&str; 14] = [
    "as_at",
    "capture_register",
    "compare_builds",
    "dist",
    "dist_verify",
    "export_live_evidence_bundles",
    "export_monitor_contract",
    "export_publication_bundles",
    "pii_scan",
    "pii_scan2",
    "probe13",
    "rates",
    "rulings",
    "search",
];

const USAGE: &str = "usage: fadden {as_at,capture_register,compare_builds,dist,dist_verify,export_live_evidence_bundles,export_monitor_contract,export_publication_bundles,pii_scan,pii_scan2,probe13,rates,rulings,search} [arguments ...]\n";

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
        Some((stage, rest)) if stage == "capture_register" => {
            capture_register::main(rest, &mut run)
        }
        Some((stage, rest)) if stage == "compare_builds" => compare_builds::main(rest, &mut run),
        Some((stage, rest)) if stage == "rates" => rates::main(rest, &mut run),
        Some((stage, rest)) if stage == "rulings" => rulings::main(rest, &mut run),
        Some((stage, rest)) if stage == "search" => search::main(rest, &mut run),
        Some((stage, rest)) if stage == "pii_scan" => pii_scan::main(rest, &mut run),
        Some((stage, rest)) if stage == "pii_scan2" => pii_scan::main2(rest, &mut run),
        Some((stage, rest)) if stage == "probe13" => probe13::main(rest, &mut run),
        Some((stage, rest)) if stage == "dist_verify" => dist_verify::main(rest, &mut run),
        Some((stage, rest)) if stage == "dist" => dist::main(rest, &mut run),
        Some((stage, rest)) if stage == "export_live_evidence_bundles" => {
            live_evidence::main(rest, &mut run)
        }
        Some((stage, rest)) if stage == "export_monitor_contract" => {
            monitor_contract::main(rest, &mut run)
        }
        Some((stage, rest)) if stage == "export_publication_bundles" => {
            publication_bundles::main(rest, &mut run)
        }
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
