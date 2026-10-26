//! `tax-radar-au`: the synthetic change-review queue's command line.
use std::io::Write;

fn main() {
    let Some(args) = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().ok())
        .collect::<Option<Vec<String>>>()
    else {
        // Python accepted such an argument and failed later on its path; a
        // lossy conversion here would name a different file, so refuse it.
        let message = "tax-radar-au: blocked: every argument must be valid Unicode.\n";
        let text = if cfg!(windows) {
            message.replace('\n', "\r\n")
        } else {
            message.into()
        };
        let _ = std::io::stderr().write_all(text.as_bytes());
        std::process::exit(2);
    };
    std::process::exit(tax_radar_au::radar::cli::main(&args));
}
