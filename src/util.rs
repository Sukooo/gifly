//! Small helpers more than one module needs.

use std::{
    fs, io,
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError},
    thread,
    time::Duration,
};

/// FNV-1a: a fast, stable hash for naming files after a link or their contents. Not for
/// anything an attacker could aim at, since collisions are easy to make on purpose.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Locks `m`, carrying on if another thread panicked while holding it. Only for locks that
/// guard something a panic can't leave half-changed, like a plain list.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes `bytes` aside and renames them into place, so a half-written file is never read as
/// `path`. The temporary name includes the thread, as two threads may write the same file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let part = path.with_extension(format!("{:?}.part", thread::current().id()));
    fs::write(&part, bytes).and_then(|()| fs::rename(&part, path)).inspect_err(|_| {
        let _ = fs::remove_file(&part);
    })
}

/// Calls `f` up to `tries` times, `wait` apart, until it gives something.
pub fn retry<T>(tries: u32, wait: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    (0..tries).find_map(|i| {
        if i > 0 {
            thread::sleep(wait);
        }
        f()
    })
}

/// Checks `done` up to `tries` times, `wait` apart, until it's true.
pub fn wait_until(tries: u32, wait: Duration, mut done: impl FnMut() -> bool) -> bool {
    retry(tries, wait, || done().then_some(())).is_some()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn retry_stops_at_the_first_success_without_waiting_before_it() {
        let mut calls = 0;
        let start = Instant::now();
        let got = super::retry(5, Duration::from_secs(1), || {
            calls += 1;
            Some(calls)
        });
        assert_eq!((got, calls), (Some(1), 1));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn retry_makes_exactly_the_tries_it_was_given() {
        let mut calls = 0;
        assert!(!super::wait_until(3, Duration::ZERO, || {
            calls += 1;
            false
        }));
        assert_eq!(calls, 3);
        assert_eq!(super::retry(0, Duration::ZERO, || Some(())), None);
    }

    #[test]
    fn fnv1a_matches_the_reference_values() {
        // From the FNV reference test suite.
        assert_eq!(super::fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(super::fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(super::fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
