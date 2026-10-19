//! `tax-radar-au`: compare, validate-review and exposure.
use std::io::Write;

use super::{Error, exposure, monitor, persist};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{Json, Layout, dumps};
use crate::pyos::{OsError, Style};
use crate::pypath::PyPath;

const PROG: &str = "tax-radar-au";

fn build_parser() -> Parser {
    let required = |flag: &str| Action::option(flag).required();
    Parser::new(PROG)
        .description("Create a synthetic provenance-first tax change-review queue.")
        .subcommands("command")
        .sub(
            "compare",
            Some("compare a baseline index, observation, and exact source map"),
            |p| {
                p.arg(required("--baseline"))
                    .arg(required("--observation"))
                    .arg(required("--map"))
                    .arg(required("--out"))
            },
        )
        .sub(
            "validate-review",
            Some("validate a human technical-review decision"),
            |p| {
                p.arg(required("--queue"))
                    .arg(required("--decision"))
                    .arg(Action::option("--out").help("optional path for the validation JSON"))
            },
        )
        .sub(
            "exposure",
            Some("list pseudonymous client profiles whose workflows an intact queue names"),
            |p| {
                p.arg(required("--queue"))
                    .arg(required("--profiles"))
                    .arg(required("--out"))
            },
        )
}

/// Text-mode output: newlines become the platform's line separator.
fn text(s: &str) -> Vec<u8> {
    if cfg!(windows) {
        s.replace('\n', "\r\n").into_bytes()
    } else {
        s.as_bytes().to_vec()
    }
}

fn eprint(s: &str) {
    let _ = std::io::stderr().write_all(&text(s));
}

fn blocked(message: &str) -> i32 {
    eprint(&format!("{PROG}: blocked: {message}\n"));
    2
}

/// Write to standard output, reporting a write failure as Python's stdout did.
///
/// Rust's `Stdout` treats `EBADF` as success, but Python reported it, for
/// example when stdout is a file opened only for reading. A descriptor 1 that
/// is not open at all left Python with no `sys.stdout`, which prints nothing.
#[cfg(unix)]
fn write_stdout(bytes: &[u8]) -> std::io::Result<()> {
    use std::os::fd::FromRawFd;
    // SAFETY: F_GETFD only queries descriptor 1.
    if unsafe { libc::fcntl(1, libc::F_GETFD) } == -1 {
        return Ok(());
    }
    // SAFETY: descriptor 1 is open; ManuallyDrop keeps this File from closing it.
    let mut stdout = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
    stdout.write_all(bytes)
}

#[cfg(not(unix))]
fn write_stdout(bytes: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes).and_then(|()| stdout.flush())
}

/// Print the run summary and settle stdout, returning `code` either way.
///
/// The summary is a convenience; the output files are already on disk, so a
/// stdout that cannot be written changes nothing but a note on stderr.
fn report(lines: &[String], code: i32) -> i32 {
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    if let Err(error) = write_stdout(&text(&body)) {
        let exc = OsError::new(&error, None, Style::Errno);
        eprint(&format!(
            "{PROG}: note: the run summary could not be written to stdout ({exc}); \
             the exit status still reflects the run and the files on disk are complete.\n"
        ));
    }
    code
}

fn path(args: &crate::argparse::Namespace, key: &str) -> PyPath {
    PyPath::new(&args.str(key).unwrap_or_default())
}

fn summary_paths(lines: &mut Vec<String>, paths: &(PyPath, PyPath)) {
    lines.push(format!("  json: {}", paths.0));
    lines.push(format!("  markdown: {}", paths.1));
}

fn status_code(status: &Json) -> i32 {
    if status.as_str() == Some("BLOCKED") {
        2
    } else {
        0
    }
}

/// Run the command line; the result is the exit status.
pub fn main(argv: &[String]) -> i32 {
    let args = match build_parser().parse(argv) {
        Ok(args) => args,
        Err(Exit::Help(help)) => {
            let _ = std::io::stdout().write_all(&text(&help));
            return 0;
        }
        Err(Exit::Error(message)) => {
            eprint(&message);
            return 2;
        }
    };
    match run(&args) {
        Ok(code) => code,
        Err(Error::Monitor { message, .. }) => blocked(&message),
        Err(Error::Os(error)) => blocked(&error.text),
    }
}

fn run(args: &crate::argparse::Namespace) -> super::Result<i32> {
    let write_failed = |what: &str, error: OsError| {
        Ok(blocked(&format!(
            "the {what} could not be written: {error}"
        )))
    };
    match args.str("command").as_deref() {
        Some("compare") => {
            let (baseline, observation, map, out) = (
                path(args, "baseline"),
                path(args, "observation"),
                path(args, "map"),
                path(args, "out"),
            );
            let queue = monitor::compare(&baseline, &observation, &map)?;
            let written = persist::output_paths(&out, "impact-queue").and_then(|(json, md)| {
                persist::refuse_input_overwrite(&[json, md], &[baseline, observation, map])?;
                monitor::write_queue(&queue, &out)
            });
            let paths = match written {
                Ok(paths) => paths,
                Err(Error::Os(error)) => return write_failed("output directory", error),
                Err(other) => return Err(other),
            };
            let count = match queue.at("items") {
                Json::Array(items) => items.len(),
                _ => 0,
            };
            let mut lines = vec![format!(
                "{PROG}: {}; {count} item(s)",
                monitor::py_format(queue.at("run_status"))
            )];
            summary_paths(&mut lines, &paths);
            Ok(report(&lines, status_code(queue.at("run_status"))))
        }
        Some("exposure") => {
            let (queue, profiles, out) = (
                path(args, "queue"),
                path(args, "profiles"),
                path(args, "out"),
            );
            let result = exposure::exposure(&queue, &profiles)?;
            let paths = match exposure::write_exposure(&result, &out, &queue, &profiles) {
                Ok(paths) => paths,
                Err(Error::Os(error)) => return write_failed("output directory", error),
                Err(other) => return Err(other),
            };
            let items = match result.at("items") {
                Json::Array(items) => items.as_slice(),
                _ => &[],
            };
            let matched = items
                .iter()
                .filter(|item| item.at("matches").truthy())
                .count();
            let unmatched = match result.at("profiles_without_candidate_match") {
                Json::Array(ids) => ids.len(),
                _ => 0,
            };
            let run_status = result.at("queue").at("run_status");
            let mut lines = vec![format!(
                "{PROG}: exposure for a {} queue; {matched} item(s) with a profile match, \
                 {unmatched} profile(s) without one",
                monitor::py_format(run_status)
            )];
            summary_paths(&mut lines, &paths);
            Ok(report(&lines, status_code(run_status)))
        }
        _ => {
            let (queue, decision) = (path(args, "queue"), path(args, "decision"));
            let validation = monitor::validate_review(&queue, &decision)?;
            if let Some(out) = args.str("out") {
                let companion = queue.with_suffix(".md").map_err(Error::monitor)?;
                let written = persist::write_receipt(
                    &format!("{}\n", dumps(&validation, Layout::Indented)),
                    &PyPath::new(&out),
                    &[queue, companion, decision],
                );
                match written {
                    Ok(_) => {}
                    Err(Error::Os(error)) => return write_failed("validation output file", error),
                    Err(other) => return Err(other),
                }
            }
            Ok(report(
                &[format!(
                    "{PROG}: {}; {} decision(s)",
                    monitor::py_format(validation.at("status")),
                    monitor::py_format(validation.at("decision_count"))
                )],
                0,
            ))
        }
    }
}
