#![no_main]
// The privacy patterns run on the regex crate where they need no lookaround:
// that fast path must find exactly what fancy_regex finds.
use libfuzzer_sys::fuzz_target;
use tax_radar_au::fadden::pii::check_engines;

fuzz_target!(|data: &[u8]| {
    if let Err(problem) = check_engines(data) {
        panic!("{problem}");
    }
});
