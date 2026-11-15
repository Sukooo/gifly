//! Counters for "Copy diagnostics": what loading GIFs has fetched this session, and how.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub static REQUESTS: AtomicU64 = AtomicU64::new(0);
pub static DOWNLOADED: AtomicU64 = AtomicU64::new(0);
/// Files (downloads and thumbnails) read from the disk cache.
pub static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
/// Files that had to be downloaded.
pub static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
pub static RENEW_REQUESTS: AtomicU64 = AtomicU64::new(0);
pub static RENEWED: AtomicU64 = AtomicU64::new(0);
/// Links in renewal requests that failed outright.
pub static RENEW_FAILED: AtomicU64 = AtomicU64::new(0);
/// Frames drawn while the picker was open, and the CPU time they took.
pub static FRAMES: AtomicU64 = AtomicU64::new(0);
pub static FRAME_US: AtomicU64 = AtomicU64::new(0);
pub static SLOWEST_US: AtomicU64 = AtomicU64::new(0);
/// Frames that took longer than a 60Hz screen shows one.
pub static SLOW_FRAMES: AtomicU64 = AtomicU64::new(0);

pub fn add(counter: &AtomicU64, n: usize) {
    counter.fetch_add(n as u64, Relaxed);
}

/// Counts a drawn frame that took `secs` of CPU time.
pub fn frame(secs: f32) {
    let us = (secs * 1e6) as u64;
    FRAMES.fetch_add(1, Relaxed);
    FRAME_US.fetch_add(us, Relaxed);
    SLOWEST_US.fetch_max(us, Relaxed);
    if us > 16_667 {
        SLOW_FRAMES.fetch_add(1, Relaxed);
    }
}

pub fn summary() -> String {
    let get = |c: &AtomicU64| c.load(Relaxed);
    let frames = get(&FRAMES);
    format!(
        "frames: {frames} drawn, {:.2} ms average, {:.1} ms slowest, {} slow\n\
         network: {} requests, {} MB; cache: {} hits, {} misses; renewals: {} requests, {} links renewed, {} failed",
        get(&FRAME_US) as f64 / frames.max(1) as f64 / 1000.0,
        get(&SLOWEST_US) as f64 / 1000.0,
        get(&SLOW_FRAMES),
        get(&REQUESTS),
        get(&DOWNLOADED) >> 20,
        get(&CACHE_HITS),
        get(&CACHE_MISSES),
        get(&RENEW_REQUESTS),
        get(&RENEWED),
        get(&RENEW_FAILED),
    )
}
