//! Fuzz the privacy patterns the PII scans, the distribution builder and its
//! verifier run over every title. A pattern that backtracks without bound
//! shows up as a timeout; a broken invariant aborts the run.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(problem) = tax_radar_au::fadden::pii::check_one_input(data) {
        panic!("{problem}");
    }
});
