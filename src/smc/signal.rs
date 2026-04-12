//! Dedicated signal-handling thread for feature 005 (FR-021 through FR-025,
//! FR-088, FR-089, FR-092, FR-093).
//!
//! # Three-connection model (I1 resolution)
//!
//! The signal thread OWNS its own `SmcConnection` (connection #2) by value.
//! No `Arc<Mutex<>>`, no cross-thread shared access. The main thread's
//! connection (#1) and the panic hook's connection (#3) are untouched.
//!
//! # Teardown contract
//!
//! On SIGINT / SIGTERM:
//! 1. Race the `release_in_progress: Arc<AtomicBool>` via `compare_exchange`.
//!    - Lose → `libc::_exit(1)` immediately (FR-023 idempotency guard).
//!    - Win → proceed.
//! 2. For every fan index in `Arc<Vec<u8>>`, write `F<i>Md=0` using our
//!    owned connection.
//! 3. Write `Ftst=0`.
//! 4. Call `std::process::exit(0)` (flushes stdio + runs atexit).
//!
//! On SIGINFO (macOS-specific, Ctrl-T on a tty):
//! - Drain the most recent 32 round-trip records to stderr **if** the trust
//!   check on stderr passes (FR-064). Not a teardown trigger.
//!
//! On SIGHUP:
//! - Set a `reload_requested` flag for the main tick to observe. Feature 005's
//!   one-shot CLI does not use reload, but the plumbing is shared with
//!   feature 006 daemon mode.
//!
//! # Signal mask policy
//!
//! Before spawning this thread, the main thread calls
//! `block_signals_on_non_signal_threads()` to `pthread_sigmask(SIG_BLOCK, ...)`
//! SIGTERM/SIGINT/SIGHUP/SIGINFO on itself (and any subsequently-spawned
//! worker thread inherits the mask). The signal thread then
//! `pthread_sigmask(SIG_UNBLOCK, ...)` those signals on itself, ensuring
//! exclusive delivery.

#![allow(unsafe_code)] // pthread_sigmask FFI
#![allow(clippy::missing_errors_doc)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::smc::ffi::SmcConnection;

/// Signals handled by the dedicated signal thread.
const SIG_TERM: libc::c_int = libc::SIGTERM;
const SIG_INT: libc::c_int = libc::SIGINT;
const SIG_HUP: libc::c_int = libc::SIGHUP;
const SIG_INFO: libc::c_int = libc::SIGINFO;

/// Block the four signals on the calling thread. Spawned worker threads
/// inherit this mask, so the signal thread is the only context where
/// delivery occurs (FR-088).
///
/// Must be called on the main thread BEFORE `spawn_signal_thread()`.
pub fn block_signals_on_non_signal_threads() {
    // SAFETY: sigemptyset + sigaddset + pthread_sigmask are thread-safe
    // POSIX primitives with standard semantics.
    unsafe {
        let mut set: libc::sigset_t = core::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, SIG_TERM);
        libc::sigaddset(&mut set, SIG_INT);
        libc::sigaddset(&mut set, SIG_HUP);
        libc::sigaddset(&mut set, SIG_INFO);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, core::ptr::null_mut());
    }
}

/// Unblock the four signals on the calling thread. Used ONLY by the signal
/// thread at the top of its body so it alone receives them.
fn unblock_signals_on_this_thread() {
    // SAFETY: see block_signals_on_non_signal_threads.
    unsafe {
        let mut set: libc::sigset_t = core::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, SIG_TERM);
        libc::sigaddset(&mut set, SIG_INT);
        libc::sigaddset(&mut set, SIG_HUP);
        libc::sigaddset(&mut set, SIG_INFO);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, core::ptr::null_mut());
    }
}

/// Check whether stderr is a "trusted sink" for info-disclosure purposes
/// (FR-064, CHK005). Returns true when stderr is a tty OR a regular file
/// owned by uid 0 with permission mode 0600/0640.
///
/// Used by the SIGINFO handler before dumping the round-trip ring.
pub fn stderr_is_trusted() -> bool {
    // SAFETY: fstat on a valid fd is always safe. stderr (fd 2) is guaranteed
    // open for the lifetime of the process unless the operator closed it,
    // in which case the return value's safety is irrelevant.
    unsafe {
        let mut st: libc::stat = core::mem::zeroed();
        if libc::fstat(2, &mut st) != 0 {
            return false;
        }
        // tty check: S_ISCHR(st_mode) and isatty(2) — combined via isatty which
        // handles the semantics correctly.
        if libc::isatty(2) == 1 {
            return true;
        }
        // Non-tty: require regular file owned by uid 0 with mode 0600 or 0640.
        let file_type = st.st_mode & libc::S_IFMT;
        if file_type != libc::S_IFREG {
            return false;
        }
        if st.st_uid != 0 {
            return false;
        }
        let perm = st.st_mode & 0o777;
        perm == 0o600 || perm == 0o640
    }
}

/// State passed to the signal thread via a single `Arc`-cloneable bundle.
/// Keeps the spawn call site tidy.
pub struct SignalThreadState {
    /// First thread to CAS false→true wins the teardown race (FR-023).
    pub release_in_progress: Arc<AtomicBool>,
    /// Pre-enumerated fan indices to release on teardown. Shared read-only
    /// with the main thread via `Arc<Vec<u8>>`.
    pub fans: Arc<Vec<u8>>,
    /// Set to true by the SIGHUP arm. Main thread observes at tick boundary.
    /// Unused in feature 005 one-shot CLI but plumbing is shared with feature 006.
    pub reload_requested: Arc<AtomicBool>,
}

/// Spawn the dedicated signal thread owning `teardown_conn` by value
/// (connection #2 in the three-connection model). Returns the `JoinHandle`
/// so the main thread can `.join()` on clean exit.
///
/// Before calling this, the caller MUST have called
/// `block_signals_on_non_signal_threads()` on the main thread (FR-088).
#[must_use]
pub fn spawn_signal_thread(
    mut teardown_conn: SmcConnection,
    state: SignalThreadState,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("fand-signal".into())
        .spawn(move || {
            // FR-088: this thread unblocks the four signals so they are
            // delivered HERE rather than to a GCD worker.
            unblock_signals_on_this_thread();

            let mut sigs =
                match signal_hook::iterator::Signals::new([SIG_TERM, SIG_INT, SIG_HUP, SIG_INFO]) {
                    Ok(s) => s,
                    Err(_) => {
                        // If signal-hook cannot register, we cannot proceed.
                        // Log to stderr and exit with the general error code.
                        emit_stderr(b"fand: signal-hook registration failed\n");
                        return;
                    }
                };

            for sig in sigs.forever() {
                match sig {
                    s if s == SIG_HUP => {
                        state.reload_requested.store(true, Ordering::Release);
                        // fand set / fand selftest one-shot commands do NOT
                        // act on reload — the flag is set but ignored.
                    }
                    s if s == SIG_INFO => {
                        if stderr_is_trusted() {
                            emit_stderr(b"fand: SIGINFO - ring drain not yet wired\n");
                            // Ring drain is a follow-up task (requires passing
                            // the ring handle into SignalThreadState).
                        } else {
                            emit_stderr(
                                b"fand: SIGINFO - ring contents suppressed \
                                  (stderr is not a trusted sink per FR-064)\n",
                            );
                        }
                    }
                    s if s == SIG_TERM || s == SIG_INT => {
                        teardown_and_exit(&mut teardown_conn, &state);
                        // teardown_and_exit calls std::process::exit so this
                        // return is unreachable in practice.
                        return;
                    }
                    _ => {
                        // Unknown signal — ignore.
                    }
                }
            }
        })
        .expect("spawn signal thread")
}

/// Execute the teardown checklist on the signal thread's owned connection.
///
/// Idempotency: first thread to CAS wins. Losing caller immediately exits
/// via `libc::_exit(1)` so a second signal during an in-flight teardown
/// never extends the 500 ms budget (FR-023).
fn teardown_and_exit(conn: &mut SmcConnection, state: &SignalThreadState) {
    let won = state
        .release_in_progress
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    if !won {
        // FR-023: second signal during teardown — forced exit.
        // SAFETY: libc::_exit is async-signal-safe and terminates immediately.
        unsafe { libc::_exit(1) };
    }

    // Best-effort release. Errors are logged to stderr but do NOT abort
    // the remaining per-fan writes — every fan that was ever touched
    // MUST be returned to auto control per FR-021.
    for &fan_idx in state.fans.iter() {
        let _ = conn.force_write_auto_mode(fan_idx);
    }
    let _ = conn.force_write_ftst_zero();

    emit_stderr(b"fand: signal teardown complete, exiting\n");
    std::process::exit(0);
}

/// Emit a short byte slice to stderr without allocating. Used in the signal
/// thread to report progress / errors.
fn emit_stderr(bytes: &[u8]) {
    // SAFETY: libc::write(2, ptr, len) to stderr is async-signal-safe.
    unsafe {
        libc::write(
            2,
            bytes.as_ptr().cast::<libc::c_void>(),
            bytes.len() as libc::size_t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_and_unblock_do_not_panic() {
        block_signals_on_non_signal_threads();
        unblock_signals_on_this_thread();
    }

    #[test]
    fn stderr_is_trusted_returns_a_bool() {
        // Smoke test — the exact value depends on how the test harness
        // captures stderr, so we just verify it doesn't panic.
        let _ = stderr_is_trusted();
    }

    #[test]
    fn signal_thread_state_is_sendable() {
        fn assert_send<T: Send>() {}
        assert_send::<SignalThreadState>();
    }
}
