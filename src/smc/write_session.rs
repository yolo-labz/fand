//! `WriteSession` — top-level owner of the feature 005 write path.
//!
//! Implements the **three-connection model** from FR-090 (I1 resolution):
//! - Connection #1 (main): owned by `WriteSession.conn`, single-threaded.
//! - Connection #2 (teardown): moved into the signal thread by value.
//! - Connection #3 (panic): leaked via `Box::leak` into `PANIC_CONN`.
//!
//! This eliminates `Arc<Mutex<SmcConnection>>` from the write path entirely.
//! Each thread owns its own connection; no cross-thread mutex contention;
//! no poisoned-mutex fall-through path needed.
//!
//! The `WriteSession` lifecycle is:
//!
//! ```text
//!   WriteSession::acquire()
//!       ├─ generate SessionId (FR-100)
//!       ├─ FlockGuard::try_acquire() (FR-050 — O_NOFOLLOW + mode 0600)
//!       ├─ SmcConnection::open() ×3 — main + teardown + panic
//!       ├─ enumerate_fans(&mut main_conn) (feature 004)
//!       ├─ seal_for_panic(panic_conn, fan_indices.clone()) — leaks conn #3
//!       ├─ install_panic_hook()
//!       ├─ block_signals_on_non_signal_threads() (FR-088)
//!       ├─ spawn_signal_thread(teardown_conn, SignalThreadState { ... })
//!       └─ return WriteSession owning { main_conn, fans, lock_guard,
//!              round_trips, tear_down_once, signal_thread, session_id }
//!
//!   WriteSession::teardown() or drop()
//!       ├─ compare_exchange tear_down_once — bail if lost
//!       ├─ write F<i>Md=0 for each fan via self.conn
//!       ├─ write Ftst=0 via self.conn
//!       ├─ close self.conn
//!       ├─ drain round_trips to stderr
//!       ├─ release lock_guard (flock released on fd close)
//!       └─ (signal_thread has its own exit path via std::process::exit)
//! ```

#![allow(clippy::missing_errors_doc)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::correlation::SessionId;
use crate::smc::enumerate::{enumerate_fans, Fan};
use crate::smc::ffi::{SmcConnection, SmcError};
use crate::smc::keys::WritableKey;
use crate::smc::panic_hook::{install_panic_hook, seal_for_panic};
use crate::smc::round_trip::RoundTripRing;
use crate::smc::signal::{
    block_signals_on_non_signal_threads, spawn_signal_thread, SignalThreadState,
};
use crate::smc::single_instance::FlockGuard;
use crate::smc::unlock::DiagnosticUnlockSession;

/// Wall-clock nanoseconds since the Unix epoch, saturating on clock error.
/// Used for stamping error propagation contexts with a wall-clock timestamp.
fn wall_clock_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Top-level owner of the fand write path.
///
/// Only one `WriteSession` can exist process-wide — the `FlockGuard` held
/// inside enforces this via `/var/run/fand-smc.lock`. Dropping the session
/// runs the teardown checklist idempotently.
pub struct WriteSession {
    /// Main-thread connection (#1 in the three-connection model).
    conn: SmcConnection,
    /// Pre-enumerated fan metadata. Shared with the signal thread via Arc.
    fans: Arc<Vec<u8>>,
    /// Richer fan metadata for the main thread's write path (envelope + mode key).
    /// Not shared with the signal thread because teardown only needs indices.
    fan_envelopes: Vec<Fan>,
    /// Lockfile guard — dropped on teardown releases the flock.
    lock_guard: FlockGuard,
    /// Bounded round-trip history ring with embedded `SessionId` (FR-100).
    round_trips: RoundTripRing,
    /// Active diagnostic unlock session (`Ftst=1`). `Some` after `commit_set_fan`
    /// has acquired it; `None` before and after teardown.
    unlock: Option<DiagnosticUnlockSession>,
    /// Shared idempotency guard (FR-023) — first thread to CAS wins the teardown race.
    tear_down_once: Arc<AtomicBool>,
    /// Handle to the dedicated signal thread. Used for `.join()` on clean exit.
    /// Wrapped in `Option` so `teardown()` can `.take()` it without needing
    /// `&mut self`-through-a-mutex gymnastics.
    signal_thread: Option<JoinHandle<()>>,
    /// Per-session correlation ID (FR-100) — identical to `round_trips.session_id()`.
    session_id: SessionId,
}

impl WriteSession {
    /// Acquire a new write session.
    ///
    /// Opens three independent `SmcConnection`s, enumerates fans, installs
    /// the panic hook, blocks signals on the main thread, and spawns the
    /// dedicated signal thread. Returns `Err` if any step fails — on failure,
    /// all partially-opened connections and the flock are released cleanly.
    ///
    /// # Errors
    ///
    /// - `SmcError::ConflictDetected` if another fand instance holds the flock
    ///   (via `FlockGuard::try_acquire`).
    /// - Any error from `SmcConnection::open()`.
    /// - Any error from `enumerate_fans()`.
    pub fn acquire() -> Result<Self, SmcError> {
        // Step 1: generate the correlation ID (FR-100).
        let session_id = SessionId::new();

        // Step 2: acquire the flock. If a conflict or filesystem issue occurs,
        // fail fast before opening any SMC connection.
        let lock_guard = match FlockGuard::try_acquire() {
            Ok(guard) => guard,
            Err(e) => return Err(Self::flock_error_to_smc_error(e)),
        };

        // Step 3: open the three connections. Each is independent per FR-090
        // (I1 resolution). If any open fails, the earlier connections are
        // dropped automatically (their `Drop` impl closes the Mach port).
        let mut main_conn = SmcConnection::open()?;
        let teardown_conn = SmcConnection::open()?;
        let panic_conn = SmcConnection::open()?;

        // Step 4: enumerate fans on the main connection.
        let fan_envelopes = enumerate_fans(&mut main_conn)?;
        let fan_indices: Vec<u8> = fan_envelopes.iter().map(|f| f.index).collect();
        let fans_arc: Arc<Vec<u8>> = Arc::new(fan_indices.clone());

        // Step 5: seal the panic hook with connection #3. This moves
        // `panic_conn` into a `Box::leak` + `OnceLock` — it is no longer
        // accessible to this function after this call.
        seal_for_panic(panic_conn, fan_indices.clone());
        install_panic_hook();

        // Step 6: block signals on the main thread BEFORE spawning the
        // signal thread, so the signal thread is the only context where
        // SIGINT/SIGTERM/SIGHUP/SIGINFO are delivered (FR-088).
        block_signals_on_non_signal_threads();

        // Step 7: spawn the signal thread owning connection #2 by value.
        let tear_down_once = Arc::new(AtomicBool::new(false));
        let reload_requested = Arc::new(AtomicBool::new(false));
        let state = SignalThreadState {
            release_in_progress: tear_down_once.clone(),
            fans: fans_arc.clone(),
            reload_requested,
        };
        let signal_thread = spawn_signal_thread(teardown_conn, state);

        // Step 8: construct the round-trip ring with the session ID stamped in.
        let round_trips = RoundTripRing::new(session_id);

        Ok(Self {
            conn: main_conn,
            fans: fans_arc,
            fan_envelopes,
            lock_guard,
            round_trips,
            unlock: None,
            tear_down_once,
            signal_thread: Some(signal_thread),
            session_id,
        })
    }

    /// Return the per-session correlation ID.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Return the list of enumerated fans.
    #[must_use]
    pub fn fans(&self) -> &[Fan] {
        &self.fan_envelopes
    }

    /// Return the lockfile canonical path for conflict diagnostics.
    #[must_use]
    pub fn lockfile_path(&self) -> &std::path::Path {
        self.lock_guard.canonical_path()
    }

    /// Borrow the round-trip ring for read-only inspection.
    #[must_use]
    pub fn round_trips(&self) -> &RoundTripRing {
        &self.round_trips
    }

    /// Number of enumerated fans.
    #[must_use]
    pub fn fan_count(&self) -> usize {
        self.fan_envelopes.len()
    }

    /// Look up a fan's envelope by index. Returns None if the index
    /// is not in the enumerated set.
    #[must_use]
    pub fn fan_envelope(&self, index: u8) -> Option<&Fan> {
        self.fan_envelopes.iter().find(|f| f.index == index)
    }

    /// Read the current actual RPM for a fan (F<i>Ac, flt).
    pub fn read_actual_rpm(&mut self, fan_index: u8) -> Result<f32, SmcError> {
        let fourcc = crate::smc::keys::fan_key_fourcc(b'A', b'c', fan_index);
        self.conn.read_f32(fourcc)
    }

    /// Mutable borrow of the main SmcConnection (for sensor reads in
    /// the tick loop). The connection is NOT shared — only the main
    /// thread uses it.
    pub fn connection_mut(&mut self) -> &mut SmcConnection {
        &mut self.conn
    }

    /// Record a successful round-trip heartbeat for the watchdog.
    pub fn heartbeat(&self) {
        if let Some(ref unlock) = self.unlock {
            unlock.heartbeat();
        }
    }

    /// Commit a fan target write end-to-end (FR-037, FR-001–007).
    ///
    /// Sequence:
    /// 1. Acquire the diagnostic unlock (`Ftst=1`) with round-trip verify.
    /// 2. Bumpless-transfer seed: read `F<i>Ac` (current actual) for the
    ///    operator diagnostic only — the commanded `target` takes precedence
    ///    over any bumpless seed (FR-011 applies to initial takeover; once
    ///    the operator commands a value, that is the target).
    /// 3. Write `F<i>Md=1` (manual mode) with round-trip verify.
    /// 4. Write `F<i>Tg=<clamped>` (target RPM) with round-trip verify.
    ///
    /// On success, the override is held until `teardown()` runs. The caller
    /// is expected to enter a hold loop after this method returns.
    ///
    /// On any failure, the session state is partially-committed and the
    /// caller MUST immediately call `teardown()` (or drop the session) to
    /// release the fan.
    ///
    /// # Errors
    ///
    /// - `SmcError::UnlockMismatch` / `UnlockRejected` on Ftst unlock failure
    /// - `SmcError::WriteReadbackMismatch` on any round-trip mismatch
    /// - `SmcError::WriteRefused` on SMC-level write rejection
    /// - Any `read_u8` / `read_f32` error during readback
    pub fn commit_set_fan(
        &mut self,
        fan_index: u8,
        target: crate::control::state::ClampedRpm,
    ) -> Result<(), SmcError> {
        let fan_idx_usize = usize::from(fan_index);
        if fan_idx_usize >= self.fan_envelopes.len() {
            return Err(SmcError::KeyNotFound(u32::from_be_bytes([
                b'F',
                b'0' + fan_index,
                b'T',
                b'g',
            ])));
        }

        // Step 1: acquire the diagnostic unlock if we don't already have one.
        if self.unlock.is_none() {
            self.begin_diagnostic_unlock()?;
        }
        let session_id = self.session_id;

        // Step 2: bumpless-transfer seed for the diagnostic log.
        // Build F<i>Ac fourcc on the fly; the read is informational because
        // the operator's commanded target takes precedence over any seed.
        let _actual_rpm_for_diag = {
            let actual_key = u32::from_be_bytes([b'F', b'0' + fan_index, b'A', b'c']);
            self.conn.read_f32(actual_key).unwrap_or(0.0)
        };

        // Per RD-08 session 5 live findings on Mac17,2: Apple Silicon M-series
        // exposes a severely constrained fan-write surface. The ONLY usable
        // mode is `F0md=1` ("forced minimum"), which sets the fan to its
        // declared `F0Mn` (~2317 RPM on Mac17,2). Arbitrary RPM targets via
        // `F0Tg` are read-only aliases, and `F0Dc` returns SMC result 0x86
        // even when in `F0md=1`. Modes `F0md=2/3` cause the fan to STOP
        // entirely and are explicitly forbidden by FR-019. Modes ≥4 are
        // rejected by the SMC with `0x82`.
        //
        // Operator semantics: if the commanded target equals the fan
        // minimum (within a 5% tolerance), engage `F0md=1`. Otherwise the
        // commit is refused with a clear diagnostic explaining the SoC
        // limitation. The dry-run path is unaffected — operators can still
        // preview any RPM and see the planned writes.
        let fan = &self.fan_envelopes[fan_idx_usize];
        let target_rpm = target.as_f32();
        let min_tolerance = (fan.max_rpm - fan.min_rpm) * 0.05;
        let is_minimum_request = (target_rpm - fan.min_rpm).abs() <= min_tolerance;

        if !is_minimum_request {
            return Err(SmcError::WriteRefused {
                fourcc: u32::from_be_bytes([b'F', b'0' + fan_index, b'm', b'd']),
                result_byte: 0x86,
                context: "apple_silicon_m_series_only_supports_min_mode",
                session: session_id,
                timestamp_ns: crate::smc::write_session::wall_clock_ns(),
            });
        }

        // Write F<i>md=1 (forced minimum) with round-trip verify. The SMC
        // firmware automatically updates F<i>Dc to ~7% and F<i>Ac drops to
        // the declared minimum within 1-2 seconds. No subsequent F<i>Dc or
        // F<i>Tg write is needed (or accepted).
        let mode_key = WritableKey::fan_mode(fan_index);
        self.conn.write_u8_verified(
            &mode_key,
            1,
            "fan_mode_force_min",
            session_id,
            &mut self.round_trips,
        )?;
        if let Some(ref unlock) = self.unlock {
            unlock.heartbeat();
        }

        Ok(())
    }

    /// Acquire the diagnostic unlock session (FR-001).
    ///
    /// Writes `Ftst=1`, reads it back, compares byte-for-byte. On success
    /// stores the `DiagnosticUnlockSession` and its GCD watchdog state. On
    /// mismatch returns `SmcError::UnlockMismatch` with the session correlation
    /// ID stamped in.
    ///
    /// Idempotent: subsequent calls while a session is held are a no-op.
    ///
    /// # Errors
    ///
    /// - `SmcError::UnlockMismatch` if `Ftst` readback != 1
    /// - `SmcError::UnlockRejected` if the SMC returned a non-zero result byte
    /// - Any `write_u8` / `read_u8` error from the IOKit layer
    pub fn begin_diagnostic_unlock(&mut self) -> Result<(), SmcError> {
        if self.unlock.is_some() {
            return Ok(());
        }

        let session_id = self.session_id;
        let release_handle = self.tear_down_once.clone();
        let ftst = WritableKey::ftst();

        // Attempt the Ftst=1 write. Feature 004 live verification on Mac17,2
        // established that `Ftst` is NOT present on every Apple Silicon SoC
        // — the RD-01 assumption that Ftst is universal across M1–M5 was
        // falsified by this hardware. We handle three cases:
        //
        //   (a) KeyNotFound — the SoC does not have `Ftst` at all. Log a
        //       warning and proceed without unlock. Subsequent `F<i>Md=1`
        //       writes either succeed directly (no unlock needed on this
        //       SoC) or fail with SMC result 0x82 "system mode rejects"
        //       (which becomes `WriteRefused` and is the authoritative
        //       signal that a different unlock mechanism is required).
        //   (b) SmcResult { other_byte } — the SMC rejected the write. This
        //       is an `UnlockRejected` with the raw result byte.
        //   (c) Other errors — propagate as-is.
        let ftst_write_result = self.conn.write_u8(&ftst, 1);
        let mut ftst_present = true;
        if let Err(e) = ftst_write_result {
            match e {
                SmcError::KeyNotFound(_) => {
                    crate::log::emit_raw(
                        crate::log::LogLevel::Warn,
                        "Ftst diagnostic unlock key is absent on this SoC — \
                         proceeding without unlock (feature 005 RD-01 fallback). \
                         F<i>Md writes will surface SMC result 0x82 if a \
                         different unlock mechanism is required.",
                    );
                    ftst_present = false;
                }
                SmcError::SmcResult { result_byte, .. } => {
                    return Err(SmcError::UnlockRejected {
                        result_byte,
                        session: session_id,
                    });
                }
                other => return Err(other),
            }
        }

        if ftst_present {
            // Round-trip verify.
            let readback = self.conn.read_u8(u32::from_be_bytes(*b"Ftst"))?;
            if readback != 1 {
                return Err(SmcError::UnlockMismatch {
                    expected: 1,
                    got: readback,
                    session: session_id,
                    timestamp_ns: crate::smc::write_session::wall_clock_ns(),
                });
            }

            // Record the round-trip in the ring.
            self.round_trips
                .push(crate::smc::round_trip::RoundTripRecord::new_match(
                    crate::smc::write_session::wall_clock_ns(),
                    u32::from_be_bytes(*b"Ftst"),
                    &[1],
                    &[1],
                ));
        }

        // Install the DiagnosticUnlockSession (watchdog state machine + thread).
        // The session is active whether or not Ftst is in force — the
        // watchdog and heartbeat semantics are identical.
        let mut unlock = DiagnosticUnlockSession::new(session_id, release_handle);
        unlock.heartbeat();
        // FR-002: spawn the watchdog poller thread. On a 4-second stall it
        // raises SIGTERM, which the signal thread catches and runs the
        // existing teardown path. The watchdog itself does not touch the SMC.
        unlock.start_watchdog();
        self.unlock = Some(unlock);

        Ok(())
    }

    /// Rearm the watchdog timer without issuing an SMC write. Called from the
    /// `fand set --commit` hold loop every 500 ms while the override is held.
    /// This is a no-op if no diagnostic unlock is active.
    pub fn heartbeat_unlock(&self) {
        if let Some(ref unlock) = self.unlock {
            unlock.heartbeat();
        }
    }

    /// Run the selftest sequence on every enumerated fan (FR-043 through
    /// FR-049, Phase 4 US2). Adapted for the Apple Silicon F0md=0/1
    /// control surface per RD-08 — see `src/smc/selftest.rs` for the
    /// design rationale.
    ///
    /// Per-fan loop (`iterations` times):
    /// 1. Force `F0md=1` (forced minimum), round-trip verify.
    /// 2. Hold + sample `F0Ac` 5 times at 200ms intervals.
    /// 3. Force `F0md=0` (auto), round-trip verify.
    /// 4. Hold + sample `F0Ac` 5 times at 200ms intervals.
    ///
    /// Returns a `SelftestReport` with per-fan medians and the aggregate
    /// pass/inconclusive/fail classification.
    ///
    /// # Errors
    ///
    /// Returns the FIRST `SmcError` from any failed `F0md` write or
    /// readback. Partial results up to the failure point are NOT returned —
    /// the caller is expected to teardown via `Drop` and report the error
    /// to the user.
    pub fn run_selftest(
        &mut self,
        iterations: u8,
    ) -> Result<crate::smc::selftest::SelftestReport, SmcError> {
        use crate::smc::keys::WritableKey;
        use crate::smc::selftest::{
            classify_fan, classify_iteration, hold_and_sample, SelftestFanReport,
        };

        let session_id = self.session_id;
        let started_at = std::time::Instant::now();
        let mut per_fan: Vec<SelftestFanReport> = Vec::with_capacity(self.fan_envelopes.len());

        // Snapshot fan list to a Vec<u8> so we can iterate without holding a
        // borrow on self.fan_envelopes (which would conflict with &mut self.conn).
        let fan_indices: Vec<u8> = self.fan_envelopes.iter().map(|f| f.index).collect();

        for fan_index in fan_indices {
            let mode_key = WritableKey::fan_mode(fan_index);
            let actual_key = u32::from_be_bytes([b'F', b'0' + fan_index, b'A', b'c']);

            let mut samples_per_iteration = Vec::with_capacity(usize::from(iterations));
            let mut round_trip_count: u64 = 0;
            let mut mismatch_count: u64 = 0;
            let mut iterations_completed: u8 = 0;

            for iter in 0..iterations {
                // Step A: engage forced-min (F0md=1).
                if let Err(e) = self.conn.write_u8_verified(
                    &mode_key,
                    1,
                    "selftest_force_min",
                    session_id,
                    &mut self.round_trips,
                ) {
                    if matches!(e, SmcError::WriteReadbackMismatch { .. }) {
                        mismatch_count = mismatch_count.saturating_add(1);
                    }
                    return Err(e);
                }
                round_trip_count = round_trip_count.saturating_add(1);
                if let Some(ref unlock) = self.unlock {
                    unlock.heartbeat();
                }

                // Step B: sample F0Ac during the hold window.
                let min_samples = hold_and_sample(|| self.conn.read_f32(actual_key).unwrap_or(0.0));

                // Step C: return to auto (F0md=0).
                if let Err(e) = self.conn.write_u8_verified(
                    &mode_key,
                    0,
                    "selftest_return_auto",
                    session_id,
                    &mut self.round_trips,
                ) {
                    if matches!(e, SmcError::WriteReadbackMismatch { .. }) {
                        mismatch_count = mismatch_count.saturating_add(1);
                    }
                    return Err(e);
                }
                round_trip_count = round_trip_count.saturating_add(1);
                if let Some(ref unlock) = self.unlock {
                    unlock.heartbeat();
                }

                // Step D: sample F0Ac during the auto-hold window.
                let auto_samples =
                    hold_and_sample(|| self.conn.read_f32(actual_key).unwrap_or(0.0));

                samples_per_iteration.push(classify_iteration(iter, auto_samples, min_samples));
                iterations_completed = iterations_completed.saturating_add(1);
            }

            per_fan.push(classify_fan(
                fan_index,
                iterations_completed,
                iterations,
                round_trip_count,
                mismatch_count,
                samples_per_iteration,
            ));
        }

        Ok(crate::smc::selftest::SelftestReport::classify(
            per_fan,
            started_at.elapsed(),
        ))
    }

    /// Release the diagnostic unlock (`Ftst=0`) with round-trip verify (FR-004).
    ///
    /// Called from `teardown()`. Idempotent — if no session is held, this is
    /// a no-op.
    ///
    /// Errors are silently swallowed on this path because the fan has already
    /// been written to `F<i>Md=0` by the caller and the watchdog is the final
    /// safety net. Returning a `Result` would complicate the drop path.
    fn release_diagnostic_unlock(&mut self) {
        if self.unlock.is_none() {
            return;
        }
        // Stop the watchdog thread BEFORE the Ftst=0 write so it doesn't
        // observe a stalled heartbeat between the cancel and the actual
        // write.
        if let Some(ref mut unlock) = self.unlock {
            unlock.stop_watchdog();
        }
        // Best-effort Ftst=0 write with round-trip verify.
        let session_id = self.session_id;
        let ftst = WritableKey::ftst();
        let _ = self.conn.write_u8_verified(
            &ftst,
            0,
            "diagnostic_unlock_release",
            session_id,
            &mut self.round_trips,
        );
        self.unlock = None;
    }

    /// Run the teardown checklist (data-model.md §9). Idempotent per FR-023
    /// via the shared `tear_down_once` Arc<AtomicBool>.
    ///
    /// Called explicitly from normal exit paths, and implicitly from `Drop`
    /// when the `WriteSession` goes out of scope. The signal thread and
    /// panic hook have their OWN teardown paths on their OWN connections
    /// (#2 and #3); this function only releases connection #1.
    pub fn teardown(&mut self) {
        // FR-023: only the first caller runs the body.
        let won = self
            .tear_down_once
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !won {
            // Someone else (signal thread, watchdog) already ran the checklist.
            // Still drop our connection cleanly below.
            return;
        }

        // Step 1: per-fan write F<i>Md=0 on the main connection.
        for &fan_idx in self.fans.clone().iter() {
            let _ = self.conn.force_write_auto_mode(fan_idx);
        }

        // Step 2: release the diagnostic unlock if held (writes Ftst=0 with
        // round-trip verify via the main connection). Best-effort — any
        // failure here is already covered by the GCD watchdog + panic hook
        // safety nets.
        self.release_diagnostic_unlock();

        // Step 2b: also issue a raw force_write_ftst_zero as a belt-and-braces
        // guarantee that the unlock is released even if the verified path
        // above hit a mismatch mid-stream.
        let _ = self.conn.force_write_ftst_zero();

        // Step 3: drain the round-trip ring to stderr (best-effort).
        let _ = self.round_trips.drain_to(std::io::stderr());

        // Step 4: close the main connection explicitly. Drop will also close
        // it but we want the ordering to be: force_write → close → flock
        // release (so the close write is observable before another fand
        // instance can acquire).
        self.conn.close();

        // Step 5: the flock is released when `lock_guard` is dropped. We
        // don't explicitly drop it here — the `WriteSession`'s Drop impl
        // takes care of it.

        // Step 6: the signal thread has its own exit path via
        // `std::process::exit(0)` when it receives a signal. If we're on
        // the main-thread clean exit path, the signal thread is still
        // waiting in `Signals::forever()`. We can't cleanly join it because
        // it's blocked indefinitely — the process exit below kills it.
        //
        // Dropping the JoinHandle without joining detaches the thread,
        // which is acceptable here because the process is exiting.
        let _ = self.signal_thread.take();
    }
}

impl Drop for WriteSession {
    fn drop(&mut self) {
        self.teardown();
    }
}

impl WriteSession {
    /// Map a `FlockError` into the public `SmcError` taxonomy (FR-098).
    ///
    /// Permission-denied on lockfile create is mapped to `ServiceNotFound`
    /// so the CLI's `code_from_smc_error` returns exit code 2 ("not root")
    /// with a sudo hint — this is the common path for operators running
    /// without `sudo`. Genuine conflicts (another fand holding the lock)
    /// map to `ConflictDetected` with exit code 5.
    fn flock_error_to_smc_error(e: crate::smc::single_instance::FlockError) -> SmcError {
        use crate::smc::single_instance::FlockError;
        let path = crate::smc::single_instance::DEFAULT_LOCKFILE_PATH.to_string();
        match e {
            FlockError::AlreadyHeld { holder_pid } => SmcError::ConflictDetected {
                holder_pid: holder_pid.unwrap_or(0),
                lockfile_path: path,
            },
            FlockError::SymlinkRejected => SmcError::ConflictDetected {
                holder_pid: 0,
                lockfile_path: format!("{path} (rejected — path is a symlink, FR-050 O_NOFOLLOW)"),
            },
            FlockError::UnreliableFilesystem(fs) => SmcError::ConflictDetected {
                holder_pid: 0,
                lockfile_path: format!(
                    "{path} (refused — filesystem '{fs}' unreliable for flock, FR-102)"
                ),
            },
            FlockError::CreateFailed(io) => {
                // Permission-denied is the common path for "not root" — map
                // to ServiceNotFound so the CLI surfaces the sudo hint.
                if io.kind() == std::io::ErrorKind::PermissionDenied {
                    SmcError::ServiceNotFound
                } else {
                    SmcError::ConflictDetected {
                        holder_pid: 0,
                        lockfile_path: format!("{path} (create failed: {io})"),
                    }
                }
            }
            FlockError::CanonicalizationFailed(io) => SmcError::ConflictDetected {
                holder_pid: 0,
                lockfile_path: format!("{path} (realpath failed: {io})"),
            },
            FlockError::WriteFailed(io) => SmcError::ConflictDetected {
                holder_pid: 0,
                lockfile_path: format!("{path} (write failed: {io})"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's tests are mostly smoke-tests for the acquire/teardown
    /// state machine. Actual SMC I/O is exercised in `tests/live_smc_write.rs`
    /// under `#[cfg(feature = "live-hardware")]`.

    #[test]
    fn flock_error_maps_to_smc_error() {
        use crate::smc::single_instance::FlockError;
        let err = WriteSession::flock_error_to_smc_error(FlockError::AlreadyHeld {
            holder_pid: Some(1234),
        });
        assert!(matches!(
            err,
            SmcError::ConflictDetected {
                holder_pid: 1234,
                ..
            }
        ));
        assert_eq!(err.error_code(), "CONFLICT_DETECTED");
    }

    #[test]
    fn flock_symlink_rejection_maps_to_conflict() {
        use crate::smc::single_instance::FlockError;
        let err = WriteSession::flock_error_to_smc_error(FlockError::SymlinkRejected);
        let msg = format!("{err}");
        assert!(msg.contains("symlink"), "diagnostic must mention symlink");
    }

    #[test]
    fn flock_nfs_rejection_maps_to_conflict() {
        use crate::smc::single_instance::FlockError;
        let err = WriteSession::flock_error_to_smc_error(FlockError::UnreliableFilesystem(
            "nfs".to_string(),
        ));
        let msg = format!("{err}");
        assert!(msg.contains("nfs"));
        assert!(msg.contains("unreliable") || msg.contains("FR-102"));
    }
}
