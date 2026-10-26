#![no_main]
// The port of CPython's list.sort: the stable sort under a total order, and
// every item kept under one that is not.
use libfuzzer_sys::fuzz_target;
use tax_radar_au::pysort::check_one_input;

fuzz_target!(|data: &[u8]| {
    if let Err(problem) = check_one_input(data) {
        panic!("{problem}");
    }
});
