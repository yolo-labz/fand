//! Diagnostic unlock session (FR-001 through FR-005, FR-002 watchdog).
//!
//! Wraps the `Ftst` diagnostic unlock with:
//! - round-trip verification on acquire and release (FR-001, FR-004, FR-005)
//! - a userspace watchdog timer that forces teardown if the main tick stalls
//!   for more than 4 seconds without a successful round-trip (FR-002)
//! - an `AtomicBool` idempotency guard shared with the signal and panic paths
//!   (FR-023, FR-090 three-connection model)
//!
//! # Three-connection model (I1 resolution)
//!
//! The session operates on the main thread's `SmcConnection` (connection #1).
//! The signal thread and panic hook each own their OWN `SmcConnection`
//! (#2 and #3) for their own emergency release paths — no mutex, no
//! cross-thread sharing of a single connection.

#![allow(clippy::missing_errors_doc)]
#![allow(unsafe_code)] // GCD timer FFI is declared in src/launchd/gcd.rs; no raw unsafe here

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::correlation::SessionId;

/// FR-002 watchdog deadline. Chosen at 4 s to leave a full 1 s of slack under
/// the 5 s teardown budget (SC-005).
pub const WATCHDOG_DEADLINE: Duration = Duration::from_secs(4);

/// Shared state for a diagnostic unlock session.
///
/// Held by the `DiagnosticUnlockSessionGuard` on the main thread while active.
/// The `release_in_progress` AtomicBool is cloned into an `Arc` and shared with
/// the signal thread / panic hook so they can observe idempotency without
/// needing a reference to the session itself.
#[derive(Debug)]
pub struct DiagnosticUnlockSession {
    /// Per-session correlation ID (FR-100 — one per ring, stamped into every error).
    session_id: SessionId,
    /// Instant at which the session was acquired (for watchdog elapsed calculation).
    acquired_at: Instant,
    /// Wall-clock nanos of the most recent successful round-trip (FR-002 heartbeat).
    /// Wrapped in `Arc` so the watchdog poller thread can read it without
    /// needing a reference to the parent session struct.
    last_heartbeat_ns: Arc<AtomicU64>,
    /// Shared with signal thread + panic hook via Arc clone (FR-023, FR-090).
    /// First compare_exchange(false, true) wins the teardown race.
    release_in_progress: Arc<AtomicBool>,
    /// Whether this session has already been released (one-shot).
    released: bool,
    /// FR-002 watchdog poller thread handle. The thread is spawned in
    /// `start_watchdog()` and joined on drop / explicit release. It polls
    /// `watchdog_fired()` every `WATCHDOG_POLL_INTERVAL` and on fire raises
    /// SIGTERM to the current process so the existing signal-thread teardown
    /// path runs idempotently.
    watchdog_thread: Option<JoinHandle<()>>,
    /// Signals the watchdog thread to exit cleanly when the session is released.
    watchdog_stop: Arc<AtomicBool>,
}

/// How often the watchdog poller thread checks the heartbeat. 250 ms gives
/// at most 250 ms of slack on top of the 4-second deadline → worst-case
/// teardown latency of 4.25 s, well under the 5-second SC-005 budget.
pub const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(250);

impl DiagnosticUnlockSession {
    /// Construct a new session. Does NOT issue any SMC writes — the caller
    /// owns the write + round-trip verify sequence and calls `heartbeat()` on
    /// success.
    ///
    /// The watchdog thread is NOT started until `start_watchdog()` is called.
    /// This lets the caller control the watchdog lifecycle independently of
    /// the session state machine (useful for tests that want to verify the
    /// state machine without spawning a real thread).
    #[must_use]
    pub fn new(session_id: SessionId, release_in_progress: Arc<AtomicBool>) -> Self {
        let now = Self::wall_clock_ns();
        Self {
            session_id,
            acquired_at: Instant::now(),
            last_heartbeat_ns: Arc::new(AtomicU64::new(now)),
            release_in_progress,
            released: false,
            watchdog_thread: None,
            watchdog_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the FR-002 watchdog poller thread. Spawns a thread that polls
    /// `watchdog_fired()` every `WATCHDOG_POLL_INTERVAL` and, on fire, raises
    /// SIGTERM to the current process via `libc::kill(getpid(), SIGTERM)`.
    /// The signal thread (RD-03) catches SIGTERM and runs the existing
    /// teardown path on its own owned `SmcConnection` (#2 in the three-
    /// connection model). The watchdog itself does NOT touch the SMC.
    ///
    /// Idempotent: starting the watchdog twice is a no-op.
    pub fn start_watchdog(&mut self) {
        if self.watchdog_thread.is_some() {
            return;
        }
        let stop = self.watchdog_stop.clone();
        let release_flag = self.release_in_progress.clone();
        let last_heartbeat = self.last_heartbeat_ns.clone();
        let handle = std::thread::Builder::new()
            .name("fand-watchdog".into())
            .spawn(move || {
                Self::watchdog_loop(stop, release_flag, last_heartbeat);
            })
            .expect("spawn watchdog thread");
        self.watchdog_thread = Some(handle);
    }

    fn watchdog_loop(
        stop: Arc<AtomicBool>,
        release_flag: Arc<AtomicBool>,
        last_heartbeat: Arc<AtomicU64>,
    ) {
        loop {
            std::thread::sleep(WATCHDOG_POLL_INTERVAL);
            if stop.load(Ordering::Acquire) {
                return;
            }
            // Already in teardown — nothing for the watchdog to do.
            if release_flag.load(Ordering::Acquire) {
                return;
            }
            let now = Self::wall_clock_ns();
            let last = last_heartbeat.load(Ordering::Acquire);
            let elapsed_ms = now.saturating_sub(last) / 1_000_000;
            if elapsed_ms >= WATCHDOG_DEADLINE.as_millis() as u64 {
                // FR-002 watchdog fired. Raise SIGTERM to ourselves so the
                // signal thread's existing teardown path runs idempotently
                // on its own owned SmcConnection. We do NOT call SMC writes
                // here because the watchdog thread doesn't own a connection.
                #[allow(unsafe_code)]
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGTERM);
                }
                // The signal thread will exit the process within 500 ms.
                // We park here forever — when the process dies, this thread
                // dies too.
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
        }
    }

    /// Stop the watchdog thread cleanly (called from teardown / drop).
    pub fn stop_watchdog(&mut self) {
        self.watchdog_stop.store(true, Ordering::Release);
        if let Some(handle) = self.watchdog_thread.take() {
            // Don't join — the watchdog might be asleep in its 250ms tick.
            // Detach instead; the stop flag will cause it to exit on its
            // next iteration.
            drop(handle);
        }
    }

    /// Correlation ID stamped on this session.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Record a successful round-trip — rearms the watchdog (FR-002).
    pub fn heartbeat(&self) {
        let now = Self::wall_clock_ns();
        self.last_heartbeat_ns.store(now, Ordering::Release);
    }

    /// Elapsed milliseconds since the last successful round-trip.
    #[must_use]
    pub fn ms_since_last_heartbeat(&self) -> u64 {
        let now = Self::wall_clock_ns();
        let last = self.last_heartbeat_ns.load(Ordering::Acquire);
        now.saturating_sub(last) / 1_000_000
    }

    /// True when the watchdog has fired (FR-002): more than 4 s have elapsed
    /// since the last successful round-trip. Callers MUST observe this on
    /// every tick and trigger teardown immediately when it returns true.
    #[must_use]
    pub fn watchdog_fired(&self) -> bool {
        self.ms_since_last_heartbeat() >= WATCHDOG_DEADLINE.as_millis() as u64
    }

    /// Elapsed time since the session was acquired.
    #[must_use]
    pub fn acquired_elapsed(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    /// Atomically mark the session as released. Returns true if THIS caller
    /// won the race; false if someone else (signal thread, panic hook, watchdog)
    /// already released. Used by the teardown checklist idempotency guard.
    pub fn try_begin_release(&mut self) -> bool {
        if self.released {
            return false;
        }
        let won = self
            .release_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            self.released = true;
        }
        won
    }

    /// Return a clone of the shared release-in-progress Arc for the signal
    /// thread + panic hook.
    #[must_use]
    pub fn release_in_progress_handle(&self) -> Arc<AtomicBool> {
        self.release_in_progress.clone()
    }

    fn wall_clock_ns() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn fresh_session() -> DiagnosticUnlockSession {
        DiagnosticUnlockSession::new(SessionId::new(), Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn new_session_not_released() {
        let session = fresh_session();
        assert!(!session.released);
    }

    #[test]
    fn heartbeat_updates_elapsed() {
        let session = fresh_session();
        let initial = session.ms_since_last_heartbeat();
        thread::sleep(Duration::from_millis(50));
        let after_sleep = session.ms_since_last_heartbeat();
        assert!(after_sleep >= 50, "elapsed should grow, got {after_sleep}");

        session.heartbeat();
        let after_heartbeat = session.ms_since_last_heartbeat();
        // After heartbeat the elapsed resets to ~0 (may be a few ms by the time we read it).
        assert!(
            after_heartbeat < 50,
            "heartbeat should reset elapsed, got {after_heartbeat}"
        );
        assert!(initial <= after_sleep);
    }

    #[test]
    fn watchdog_not_fired_immediately() {
        let session = fresh_session();
        assert!(!session.watchdog_fired());
    }

    #[test]
    fn try_begin_release_is_one_shot() {
        let mut session = fresh_session();
        assert!(session.try_begin_release(), "first call must win");
        assert!(!session.try_begin_release(), "second call must lose");
        assert!(!session.try_begin_release(), "third call must lose");
    }

    #[test]
    fn shared_release_flag_coordinates_across_holders() {
        // Simulate a signal thread and the main session racing.
        let flag = Arc::new(AtomicBool::new(false));
        let mut main_session = DiagnosticUnlockSession::new(SessionId::new(), flag.clone());
        let signal_handle = flag.clone();

        // Main session wins the race.
        assert!(main_session.try_begin_release());

        // Signal thread observes the flag and loses — must take the _exit(1) path.
        let signal_won = signal_handle
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        assert!(!signal_won, "signal thread must lose after main wins");
    }

    #[test]
    fn release_in_progress_handle_shares_state() {
        let session = fresh_session();
        let handle = session.release_in_progress_handle();
        assert!(!handle.load(Ordering::Acquire));
        handle.store(true, Ordering::Release);
        // Session's internal view is also true because they share the Arc.
        assert!(session.release_in_progress.load(Ordering::Acquire));
    }

    #[test]
    fn session_id_is_stable_across_calls() {
        let session = fresh_session();
        let id1 = session.session_id();
        let id2 = session.session_id();
        assert_eq!(id1, id2);
    }
}
