//! Wall-clock helpers shared across crates.
//!
//! Every caller needs the same semantics: seconds since the Unix epoch,
//! saturating to 0 if the clock is before the epoch (possible with mocked
//! or misconfigured system clocks).

use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix time in whole seconds.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Current Unix time in whole seconds, truncated to `u32` range.
///
/// Saturates at `u32::MAX` (year 2106); consensus timestamps are `u32`.
pub fn now_timestamp_u32() -> u32 {
    now_secs().min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_secs_is_monotonic_and_sane() {
        let a = now_secs();
        let b = now_secs();
        assert!(b >= a, "clock went backwards within the test");
        // Sanity: later than 2026-01-01 and before year 2100.
        assert!((1_767_225_600..4_102_444_800).contains(&a), "a = {a}");
    }

    #[test]
    fn now_timestamp_u32_fits_u32() {
        // Trivially true by the return type; the call must not panic.
        let _ = now_timestamp_u32();
    }
}
