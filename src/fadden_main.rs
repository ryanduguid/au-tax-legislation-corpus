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
    // Nested JSON drives recursive walks as deep as Python's recursion limit,
    // past what the main thread's stack holds on Windows (1 MiB). The size is
    // a reservation; pages are committed only as the stack grows. Named `main`,
    // so a panic message reads as it did on the main thread.
    let stage = std::thread::Builder::new()
        .name("main".into())
        .stack_size(64 << 20)
        .spawn(move || tax_radar_au::fadden::cli::main(&args))
        .expect("the stage thread starts");
    // A panic has printed its message on the stage thread; exit as Rust's runtime would.
    std::process::exit(stage.join().unwrap_or(101));
}
