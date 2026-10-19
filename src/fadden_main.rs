//! `fadden <stage>`: the corpus-builder stages ported to Rust.
fn main() {
    let Some(args) = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().ok())
        .collect::<Option<Vec<String>>>()
    else {
        eprintln!("fadden: every argument must be valid Unicode.");
        std::process::exit(2);
    };
    std::process::exit(tax_radar_au::fadden::cli::main(&args));
}
