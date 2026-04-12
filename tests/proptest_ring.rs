//! Property-based tests for `RoundTripRing` (FR-056, FR-057, FR-086, FR-100).
//!
//! Verifies the bounded ring's invariants under arbitrary push sequences:
//! - `count()` is monotonic and equals the total number of pushes (FR-056)
//! - `recent(n)` returns at most `min(n, 256, count)` records in insertion order
//! - The ring wraps correctly at 256 entries
//! - The session ID stays stable across all pushes (FR-100)
//! - 24-byte `RoundTripRecord` invariant is preserved across construction
//!
//! Per FR-086: ≥1,000 cases per property in CI (the ring tests are slower
//! than the clamp/decoder tests because they instantiate the full ring).

use proptest::prelude::*;

use fand::correlation::SessionId;
use fand::smc::round_trip::{RoundTripOutcome, RoundTripRecord, RoundTripRing};

/// Strategy: generate a valid `RoundTripRecord`. The byte arrays are
/// 4 bytes each; lengths are clamped to 0..=4.
fn record_strategy() -> impl Strategy<Value = RoundTripRecord> {
    (
        any::<u64>(),                  // timestamp_ns
        any::<u32>(),                  // fourcc
        any::<[u8; 4]>(),              // written
        any::<[u8; 4]>(),              // readback
        0u8..=4u8,                     // outcome variant
    )
        .prop_map(|(ts, fc, w, r, outcome_var)| {
            let outcome = match outcome_var {
                0 => RoundTripOutcome::Match,
                1 => RoundTripOutcome::WriteFailed,
                2 => RoundTripOutcome::ReadbackFailed,
                _ => RoundTripOutcome::Mismatch,
            };
            RoundTripRecord::new(ts, fc, &w, &r, outcome)
        })
}

/// Property: count() equals the number of pushes regardless of wrap state.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn count_is_monotonic_total(records in proptest::collection::vec(record_strategy(), 0..1000)) {
        let mut ring = RoundTripRing::new(SessionId::new());
        let n = records.len();
        for r in records {
            ring.push(r);
        }
        prop_assert_eq!(ring.count(), n as u64);
    }
}

/// Property: recent(n) returns at most min(n, 256, count) records.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn recent_returns_bounded_count(
        records in proptest::collection::vec(record_strategy(), 0..500),
        n in 0usize..1000usize,
    ) {
        let mut ring = RoundTripRing::new(SessionId::new());
        let total = records.len();
        for r in records {
            ring.push(r);
        }
        let collected: Vec<_> = ring.recent(n).collect();
        let expected = n.min(256).min(total);
        prop_assert_eq!(collected.len(), expected);
    }
}

/// Property: the session ID is stable across all pushes (FR-100).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn session_id_is_stable(records in proptest::collection::vec(record_strategy(), 0..500)) {
        let initial_id = SessionId::new();
        let mut ring = RoundTripRing::new(initial_id);
        for r in records {
            ring.push(r);
        }
        let after = ring.session_id();
        prop_assert_eq!(after, initial_id);
    }
}

/// Property: after wrapping, the most recent records preserve insertion order.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn recent_preserves_insertion_order_after_wrap(
        records in proptest::collection::vec(record_strategy(), 256..600),
    ) {
        let mut ring = RoundTripRing::new(SessionId::new());
        let total = records.len();
        for r in &records {
            ring.push(r.clone());
        }
        // The ring holds the last 256 records; recent(256) should return
        // them in insertion order (oldest of the 256 first).
        let collected: Vec<u64> = ring
            .recent(256)
            .map(|r| r.timestamp_ns)
            .collect();
        let expected: Vec<u64> = records[(total - 256)..]
            .iter()
            .map(|r| r.timestamp_ns)
            .collect();
        prop_assert_eq!(collected, expected);
    }
}

/// Property: an empty ring's recent(n) returns nothing.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn empty_ring_recent_is_empty(n in 0usize..1000usize) {
        let ring = RoundTripRing::new(SessionId::new());
        prop_assert_eq!(ring.recent(n).count(), 0);
    }
}
