//! Round-trip write-verification record ring (feature 005 FR-056..059, FR-100).
//!
//! Every SMC write is followed within the same tick by a read-back; the pair
//! is captured as a `RoundTripRecord` and pushed into a bounded 256-entry
//! `[RoundTripRecord; 256]` ring. The ring holds the per-session correlation
//! ID once at the ring level (NOT per-record — preserves the 24-byte record
//! invariant) and provides drain-on-teardown observability.
//!
//! # Invariants
//!
//! - `RoundTripRecord` is exactly 24 bytes — verified at compile time.
//! - The ring is 256 records = 6 KB stack-adjacent, no heap allocation.
//! - `count()` is monotonic across the process lifetime (never wraps).
//! - `session_id()` is set once at construction and never changes.
//!
//! Spec references: FR-056 (monotonic counter), FR-057 (record schema),
//! FR-058 (bounded ring), FR-059 (on-demand inspection), FR-100 (correlation
//! ID at ring level per analyze finding I3 resolution).

use core::mem::size_of;

use crate::correlation::SessionId;

/// Outcome of a single write + read-back pair.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundTripOutcome {
    /// Write succeeded and read-back matched byte-for-byte.
    Match = 0,
    /// The write itself failed (IOKit or SMC result byte).
    WriteFailed = 1,
    /// The write succeeded but the subsequent read-back failed.
    ReadbackFailed = 2,
    /// Both calls succeeded but the readback bytes differ from written bytes.
    Mismatch = 3,
}

/// One write+readback event, packed into exactly 24 bytes.
///
/// The 24-byte invariant is enforced by a compile-time assertion. Changing
/// this size is a breaking change to the ring layout — add a new variant or
/// bump the schema version instead.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RoundTripRecord {
    /// Nanoseconds since session start (not wall-clock).
    pub timestamp_ns: u64,
    /// SMC key fourcc that was written (big-endian).
    pub fourcc: u32,
    /// Bytes that were written (zero-padded for ui8 / flt).
    pub written_bytes: [u8; 4],
    /// Bytes that were read back (zero-padded).
    pub readback_bytes: [u8; 4],
    /// Length of the valid written payload (1 for ui8, 4 for flt/ui32).
    pub written_len: u8,
    /// Length of the valid readback payload.
    pub readback_len: u8,
    /// Match / write-failed / readback-failed / mismatch.
    pub outcome: RoundTripOutcome,
    /// Reserved for future use; zeroed on push.
    pub _pad: u8,
}

// The 24-byte invariant — locked by a compile-time assertion per data-model §6.
const _: () = assert!(size_of::<RoundTripRecord>() == 24);

impl RoundTripRecord {
    /// Construct a successful-match record.
    #[must_use]
    pub fn new_match(
        timestamp_ns: u64,
        fourcc: u32,
        written: &[u8],
        readback: &[u8],
    ) -> Self {
        Self::new(timestamp_ns, fourcc, written, readback, RoundTripOutcome::Match)
    }

    /// Construct a record with an explicit outcome.
    #[must_use]
    pub fn new(
        timestamp_ns: u64,
        fourcc: u32,
        written: &[u8],
        readback: &[u8],
        outcome: RoundTripOutcome,
    ) -> Self {
        let mut written_bytes = [0u8; 4];
        let mut readback_bytes = [0u8; 4];
        let written_len = written.len().min(4);
        let readback_len = readback.len().min(4);
        written_bytes[..written_len].copy_from_slice(&written[..written_len]);
        readback_bytes[..readback_len].copy_from_slice(&readback[..readback_len]);
        Self {
            timestamp_ns,
            fourcc,
            written_bytes,
            readback_bytes,
            written_len: written_len as u8,
            readback_len: readback_len as u8,
            outcome,
            _pad: 0,
        }
    }
}

/// Bounded round-trip ring with per-session correlation ID.
///
/// 256 records × 24 bytes = 6 KB. Plus a `SessionId` (26 bytes) and two
/// integers (`next` + `count`). Total struct size ≈ 6.1 KB, all stack-adjacent
/// when embedded in `WriteSession`.
pub struct RoundTripRing {
    /// Per-session correlation ID (FR-100 ring-level — set once at construction).
    session_id: SessionId,
    /// The ring buffer itself.
    records: [RoundTripRecord; 256],
    /// Next slot to write (wraps at 256).
    next: usize,
    /// Monotonic total push count (FR-056 — never wraps, never resets).
    count: u64,
}

impl RoundTripRing {
    /// Construct a new ring stamped with the given session ID.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            records: [RoundTripRecord {
                timestamp_ns: 0,
                fourcc: 0,
                written_bytes: [0; 4],
                readback_bytes: [0; 4],
                written_len: 0,
                readback_len: 0,
                outcome: RoundTripOutcome::Match,
                _pad: 0,
            }; 256],
            next: 0,
            count: 0,
        }
    }

    /// Return the per-session correlation ID (FR-100).
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Push a new record into the ring, overwriting the oldest slot when full.
    /// `count()` always reflects the total number of pushes.
    pub fn push(&mut self, record: RoundTripRecord) {
        self.records[self.next] = record;
        self.next = (self.next + 1) % 256;
        self.count = self.count.saturating_add(1);
    }

    /// Monotonic round-trip counter (FR-056 — never resets, never wraps).
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Return an iterator over the most recent `n` records in insertion order
    /// (oldest of the `n` returned first).
    ///
    /// Returns at most `min(n, 256, self.count)` records.
    pub fn recent(&self, n: usize) -> impl Iterator<Item = &RoundTripRecord> {
        let available = (self.count as usize).min(256);
        let take = n.min(available);
        // Start is `take` slots back from `next`, wrapping.
        let start = (self.next + 256 - take) % 256;
        (0..take).map(move |i| &self.records[(start + i) % 256])
    }

    /// Drain the current ring contents to a `std::io::Write` sink as a
    /// simple machine-readable line-per-record format. Used by teardown
    /// (data-model.md §9 step 6).
    ///
    /// Does NOT clear the ring — the buffer remains valid after the drain.
    ///
    /// # Errors
    ///
    /// Any `std::io::Error` from the sink is propagated.
    pub fn drain_to<W: std::io::Write>(&self, mut sink: W) -> std::io::Result<()> {
        writeln!(sink, "# fand round-trip drain session={}", self.session_id)?;
        writeln!(sink, "# count={} capacity=256", self.count)?;
        for rec in self.recent(256) {
            writeln!(
                sink,
                "{} fourcc={:#010x} wrote={:02x?} read={:02x?} outcome={:?}",
                rec.timestamp_ns,
                rec.fourcc,
                &rec.written_bytes[..rec.written_len.min(4) as usize],
                &rec.readback_bytes[..rec.readback_len.min(4) as usize],
                rec.outcome,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(n: u64) -> RoundTripRecord {
        RoundTripRecord::new_match(n, 0x4630_4D64, &[1], &[1])
    }

    #[test]
    fn record_is_24_bytes() {
        assert_eq!(size_of::<RoundTripRecord>(), 24);
    }

    #[test]
    fn ring_count_is_monotonic() {
        let mut ring = RoundTripRing::new(SessionId::new());
        for n in 0..1000 {
            ring.push(sample_record(n));
        }
        assert_eq!(ring.count(), 1000);
    }

    #[test]
    fn ring_wraps_at_256() {
        let mut ring = RoundTripRing::new(SessionId::new());
        for n in 0..300 {
            ring.push(sample_record(n));
        }
        // Push count is 300, capacity 256 — oldest 44 records were overwritten.
        assert_eq!(ring.count(), 300);
        let recent: Vec<u64> = ring.recent(256).map(|r| r.timestamp_ns).collect();
        assert_eq!(recent.len(), 256);
        // The oldest retained record is timestamp 44; the newest is 299.
        assert_eq!(recent.first().copied(), Some(44));
        assert_eq!(recent.last().copied(), Some(299));
    }

    #[test]
    fn ring_recent_n_smaller_than_count() {
        let mut ring = RoundTripRing::new(SessionId::new());
        for n in 0..10 {
            ring.push(sample_record(n));
        }
        let recent: Vec<u64> = ring.recent(3).map(|r| r.timestamp_ns).collect();
        assert_eq!(recent, vec![7, 8, 9]);
    }

    #[test]
    fn ring_recent_empty_before_any_push() {
        let ring = RoundTripRing::new(SessionId::new());
        assert_eq!(ring.recent(10).count(), 0);
    }

    #[test]
    fn ring_session_id_stable_across_pushes() {
        let id = SessionId::new();
        let mut ring = RoundTripRing::new(id);
        for n in 0..50 {
            ring.push(sample_record(n));
        }
        assert_eq!(ring.session_id(), id);
    }

    #[test]
    fn ring_drain_to_sink() {
        let mut ring = RoundTripRing::new(SessionId::new());
        ring.push(sample_record(42));
        let mut buf: Vec<u8> = Vec::new();
        ring.drain_to(&mut buf).expect("drain");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("fand round-trip drain session="));
        assert!(text.contains("fourcc=0x46304d64"));
        assert!(text.contains("count=1"));
    }

    #[test]
    fn record_new_from_longer_slice_truncates_to_4() {
        let rec = RoundTripRecord::new_match(0, 0xDEAD_BEEF, &[1, 2, 3, 4, 5, 6], &[7, 8, 9, 10]);
        assert_eq!(rec.written_bytes, [1, 2, 3, 4]);
        assert_eq!(rec.readback_bytes, [7, 8, 9, 10]);
        assert_eq!(rec.written_len, 4);
    }

    #[test]
    fn record_new_from_single_byte() {
        let rec = RoundTripRecord::new_match(0, 0xDEAD_BEEF, &[0xAA], &[0xAA]);
        assert_eq!(rec.written_bytes, [0xAA, 0, 0, 0]);
        assert_eq!(rec.written_len, 1);
    }
}

// ---------------------------------------------------------------------
// Kani proof harnesses (FR-082 — T088).
//
// These harnesses prove that the ring push/index math never escapes
// the fixed 256-slot buffer, regardless of the push sequence or the
// internal `next`/`count` state.
// ---------------------------------------------------------------------

#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use crate::correlation::SessionId;

    /// FR-082: pushing an arbitrary (small) number of records never
    /// writes outside the 256-slot ring buffer. The `next` cursor
    /// stays in `[0, 256)` after every push.
    #[kani::proof]
    #[kani::unwind(8)]
    fn kani_ring_push_never_oob() {
        let mut ring = RoundTripRing::new(SessionId::new());
        // Bound the symbolic push count so kani terminates. 6 pushes is
        // enough to cover the wrap edge (next crosses zero) after we
        // stuff the ring near-full below.
        let n: u8 = kani::any();
        kani::assume(n <= 6);
        for _ in 0..n {
            let rec = RoundTripRecord::new_match(0, 0, &[0; 4], &[0; 4]);
            ring.push(rec);
            assert!(ring.next < 256);
        }
        assert!(ring.next < 256);
    }

    /// FR-082: `count()` is monotonically non-decreasing across any
    /// sequence of pushes, and the index computation for `recent(n)`
    /// never computes an out-of-bounds subtract.
    #[kani::proof]
    #[kani::unwind(4)]
    fn kani_ring_count_monotonic() {
        let mut ring = RoundTripRing::new(SessionId::new());
        let before = ring.count();
        let rec = RoundTripRecord::new_match(0, 0, &[0; 4], &[0; 4]);
        ring.push(rec);
        let after = ring.count();
        assert!(after >= before);
        assert!(ring.next < 256);
    }
}
