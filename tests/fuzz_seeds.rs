//! The fuzz targets' seed inputs, replayed through the same invariant checks
//! on stable Rust, so ordinary CI catches a broken check. fuzz/README.md
//! explains the targets; `privacy_patterns`'s seeds replay in fadden_pii.rs.
use std::path::Path;

fn seeds(target: &str) -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("seeds")
        .join(target);
    let found: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert!(!found.is_empty(), "{target} has no seeds");
    found
}

#[test]
fn json_decode_seeds_round_trip() {
    for seed in seeds("json_decode") {
        tax_radar_au::pyjson::check_round_trip(&seed).unwrap();
    }
}

#[test]
fn python_sort_seeds_sort() {
    for seed in seeds("python_sort") {
        tax_radar_au::pysort::check_one_input(&seed).unwrap();
    }
}

#[test]
fn pattern_engines_seeds_agree() {
    for seed in seeds("pattern_engines") {
        tax_radar_au::fadden::pii::check_engines(&seed).unwrap();
    }
}
