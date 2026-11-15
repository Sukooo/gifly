//! Wall-clock time as the library stores it: seconds since 1970, or milliseconds where a name
//! ends in `_ms`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    since_1970().as_secs() as i64
}

pub fn now_ms() -> i64 {
    since_1970().as_millis() as i64
}

/// Zero if the system clock is set before 1970.
fn since_1970() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}
