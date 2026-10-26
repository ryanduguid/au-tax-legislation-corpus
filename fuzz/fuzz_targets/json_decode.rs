#![no_main]
// The JSON decoder reads corpus files and Register responses: no input may
// panic it, and what it decodes must survive a round trip through json.dumps.
use libfuzzer_sys::fuzz_target;
use tax_radar_au::pyjson::check_round_trip;

fuzz_target!(|data: &[u8]| {
    if let Err(problem) = check_round_trip(data) {
        panic!("{problem}");
    }
});
