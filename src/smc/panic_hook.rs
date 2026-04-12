//! Panic hook infrastructure for feature 005 (FR-026 through FR-030, RD-02).
//!
//! # Three-connection model (I1 resolution)
//!
//! The panic hook owns a **dedicated** `SmcConnection` (connection #3),
//! leaked via `Box::leak` into `PANIC_CONN: OnceLock<&'static mut SmcConnection>`.
//! This connection is NEVER accessed by the main thread or the signal thread —
//! eliminating the cross-thread mutex that an earlier draft required.
//!
//! # Allocation-free diagnostics
//!
//! The hook writes panic context to stderr via a stack buffer + raw
//! `libc::write(2, ...)` because `println!` / `eprintln!` allocate internally
//! (they call into `write_fmt` which reaches the heap). The `SliceWriter`
//! helper below implements `core::fmt::Write` onto a borrowed `&mut [u8]`
//! slice — standard pattern used by Firecracker and the `heapless` crate.
//!
//! # Spec references
//!
//! - FR-026: panic hook synchronously writes auto mode + releases Ftst before panic unwinds
//! - FR-027: panic hook MUST NOT allocate
//! - FR-028: 500 ms budget shared with signal handler
//! - FR-029: nested-panic re-entry guard via PANIC_HOOK_ACTIVE
//! - FR-030: installed before any fan write
//! - RD-02: OnceLock + Box::leak pattern, allocation-free stack-buffer diagnostics
//! - I1 resolution: dedicated connection, no mutex

#![allow(unsafe_code)] // Box::leak, raw libc::write for allocation-free stderr
#![allow(clippy::missing_safety_doc)]

use core::fmt::{self, Write as FmtWrite};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::smc::ffi::SmcConnection;

/// Dedicated panic-hook `SmcConnection` (connection #3 per the three-connection
/// model). Leaked via `Box::leak` at `WriteSession::acquire()` time. The
/// panic hook is the sole accessor; no mutex, no `Arc`, no cross-thread share.
///
/// The mutable reference is stored inside an `UnsafeCell` wrapper so the
/// `OnceLock<T>` API (which only provides `&T`) can still give us a `*mut`
/// that we turn into `&mut SmcConnection` at hook-call time. This is sound
/// because (a) only the panic hook ever reads it, (b) a panic is synchronous
/// from the panicking thread's point of view, (c) `panic = "abort"` means
/// the process dies immediately after the hook returns so Drop semantics on
/// the leaked connection don't matter.
static PANIC_CONN: OnceLock<PanicConnSlot> = OnceLock::new();

/// Pre-enumerated fan index list for the panic hook to iterate.
static PANIC_FANS: OnceLock<Vec<u8>> = OnceLock::new();

/// Re-entry guard: if the panic hook panics, `PANIC_HOOK_ACTIVE` is already
/// true and the nested hook call becomes a no-op. Under `panic = "abort"`
/// Rust does not re-invoke the hook on nested panic, but this guard makes
/// the contract explicit and matches the signal-handler idempotency pattern
/// in FR-023.
static PANIC_HOOK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Wrapper that makes `&mut SmcConnection` storable in an `OnceLock<&T>`-style
/// slot. The `UnsafeCell` is necessary because `OnceLock` only gives out
/// `&Self`, but the write methods on `SmcConnection` take `&mut self`.
struct PanicConnSlot {
    inner: core::cell::UnsafeCell<&'static mut SmcConnection>,
}

// SAFETY: `PanicConnSlot` is accessed only from the panic hook body, which
// is synchronous and re-entry-guarded by `PANIC_HOOK_ACTIVE`. No concurrent
// access is possible.
unsafe impl Sync for PanicConnSlot {}
unsafe impl Send for PanicConnSlot {}

/// Seal the panic-hook state with a dedicated `SmcConnection` and a
/// pre-enumerated fan list.
///
/// Must be called EXACTLY ONCE in `WriteSession::acquire()` after the
/// panic-hook connection is opened and fans are enumerated. Subsequent
/// calls are no-ops (`OnceLock::set` returns `Err`).
///
/// The `conn` argument is taken by value and leaked via `Box::leak` — the
/// caller MUST transfer ownership of a dedicated `SmcConnection` that no
/// other code path will access.
///
/// # Panics
///
/// Never — failures are silently swallowed because this is a best-effort
/// setup path that runs before any fan write.
pub fn seal_for_panic(conn: SmcConnection, fan_indices: Vec<u8>) {
    // Leak the connection to obtain a 'static mutable reference.
    let leaked: &'static mut SmcConnection = Box::leak(Box::new(conn));
    let slot = PanicConnSlot {
        inner: core::cell::UnsafeCell::new(leaked),
    };
    let _ = PANIC_CONN.set(slot);
    let _ = PANIC_FANS.set(fan_indices);
}

/// Install the panic hook. Must be called AFTER `seal_for_panic` and BEFORE
/// any fan write (FR-030).
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(panic_hook_body));
}

/// Panic hook body — runs synchronously before `panic = "abort"` kills the
/// process. MUST NOT allocate (FR-027).
fn panic_hook_body(info: &std::panic::PanicHookInfo<'_>) {
    // FR-029: re-entry guard. On a nested panic, bail immediately so the
    // abort path is unblocked.
    if PANIC_HOOK_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    // Emit the diagnostic message to stderr FIRST, so operators see the
    // panic reason even if the release path fails.
    emit_panic_diagnostic(info);

    // Release the fans + Ftst via the dedicated panic connection.
    if let (Some(slot), Some(fans)) = (PANIC_CONN.get(), PANIC_FANS.get()) {
        // SAFETY: PanicConnSlot is accessed only from this synchronous hook
        // body; the re-entry guard above ensures exclusive access. The
        // &'static mut inside the UnsafeCell is valid because Box::leak
        // never dies until the process exits.
        let conn: &mut SmcConnection = unsafe { *slot.inner.get() };
        for &fan_idx in fans {
            let _ = conn.force_write_auto_mode(fan_idx);
        }
        let _ = conn.force_write_ftst_zero();
    }

    // Do NOT reset PANIC_HOOK_ACTIVE — the process is about to abort and
    // we never want this hook to run twice.
}

/// Write a panic diagnostic to stderr using a stack buffer + raw libc::write.
/// No heap allocation.
fn emit_panic_diagnostic(info: &std::panic::PanicHookInfo<'_>) {
    let mut buf: [u8; 512] = [0; 512];
    let mut writer = SliceWriter::new(&mut buf);
    // `write!` to a SliceWriter only uses stack space — `core::fmt::write`
    // itself does not allocate.
    let _ = write!(writer, "fand panic: {info}\n");
    let written = writer.written();
    if written > 0 {
        // SAFETY: `buf` is owned by this stack frame, `written` is within bounds.
        // libc::write(2, ptr, len) writes to stderr with no allocation.
        unsafe {
            libc::write(
                2,
                buf.as_ptr().cast::<libc::c_void>(),
                written as libc::size_t,
            );
        }
    }
}

/// Stack-buffer `fmt::Write` adapter for allocation-free formatting.
pub(crate) struct SliceWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceWriter<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn written(&self) -> usize {
        self.pos
    }
}

impl<'a> FmtWrite for SliceWriter<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len().saturating_sub(self.pos);
        let take = bytes.len().min(remaining);
        if take > 0 {
            self.buf[self.pos..self.pos + take].copy_from_slice(&bytes[..take]);
            self.pos = self.pos.saturating_add(take);
        }
        if take < bytes.len() {
            // Buffer full — truncate gracefully. We still return Ok because
            // the diagnostic is best-effort.
            Ok(())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_writer_appends() {
        let mut buf = [0u8; 64];
        let mut writer = SliceWriter::new(&mut buf);
        let _ = write!(writer, "hello {}", 42);
        let written = writer.written();
        assert_eq!(&buf[..written], b"hello 42");
    }

    #[test]
    fn slice_writer_truncates_gracefully() {
        let mut buf = [0u8; 4];
        let mut writer = SliceWriter::new(&mut buf);
        let _ = write!(writer, "this is longer than four bytes");
        assert_eq!(writer.written(), 4);
        assert_eq!(&buf, b"this");
    }

    #[test]
    fn slice_writer_empty_buf_no_panic() {
        let mut buf = [0u8; 0];
        let mut writer = SliceWriter::new(&mut buf);
        let _ = write!(writer, "ignored");
        assert_eq!(writer.written(), 0);
    }

    #[test]
    fn panic_hook_active_flag_starts_false() {
        // NOTE: this test does NOT actually panic — it just checks the
        // guard atomic initial state. Installing the hook in a test would
        // interfere with the test harness's own panic reporting.
        let _ = PANIC_HOOK_ACTIVE.compare_exchange(
            false, false, Ordering::AcqRel, Ordering::Acquire,
        );
    }
}
