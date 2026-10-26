//! Bounded reads, measured with a counting allocator: ported from the two
//! tracemalloc cases in tests/radar/test_exposure.py.
mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use tax_radar_au::radar::{exposure, util};
use tempfile::tempdir;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(now, Ordering::SeqCst);
        // SAFETY: the caller upholds GlobalAlloc's contract for this layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
        // SAFETY: ptr was allocated by System with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The peak memory in use while `body` runs, above what was in use before.
fn peak_during(body: impl FnOnce()) -> usize {
    let base = CURRENT.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    body();
    PEAK.load(Ordering::SeqCst) - base
}

// One test, so no other test thread allocates while a peak is measured.
#[test]
fn oversized_inputs_are_refused_without_reading_them() {
    let dir = tempdir().unwrap();
    let queue = compare_paths(
        &sample("baseline", "sample-sources.json"),
        &sample("observations", "sample-register-observation.json"),
        &sample("mappings", "sample-workpaper-map.json"),
    )
    .unwrap();
    let queue_path = write_queue(&queue, &dir.path().join("queue"));
    let profiles = sample("profiles", "sample-client-profiles.json");

    // An oversized Markdown companion is refused from its reported size.
    let companion = queue_path.with_extension("md");
    let original = std::fs::read(&companion).unwrap();
    std::fs::write(&companion, vec![b' '; 16_000_000]).unwrap();
    let mut outcome = None;
    let peak = peak_during(|| outcome = Some(exposure::exposure(&py(&queue_path), &py(&profiles))));
    refused(outcome.unwrap(), "Queue Markdown does not match");
    assert!(peak < 4_000_000, "peak {peak}");
    std::fs::write(&companion, original).unwrap();

    // A file larger than its reported size is read only to the limit.
    let large = dir.path().join("large.json");
    std::fs::write(&large, vec![b' '; 16_000_000]).unwrap();
    util::set_size_hook(|path, size| if path.name() == "large.json" { 0 } else { size });
    let mut outcome = None;
    let peak = peak_during(|| outcome = Some(exposure::exposure(&py(&queue_path), &py(&large))));
    util::clear_hooks();
    refused(outcome.unwrap(), "exceeds 4000000 bytes");
    // The profiles limit is 4,000,000 bytes; one byte past it is read, not 16 MB.
    assert!(peak < 4_000_000 + 1_000_000, "peak {peak}");
}
