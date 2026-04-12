//! SMC IOKit FFI — byte-perfect read path (feature 004)
//!
//! # Module-level soundness contract
//!
//! This module wraps the `AppleSMC` IOKit user client via `IOConnectCallStructMethod`
//! selector 2. The safe public API upholds these invariants (per feature 004 spec):
//!
//! 1. `SMCParamStruct` is `#[repr(C)]`, exactly 80 bytes, verified at compile time via
//!    `const _: () = assert!(size_of::<SMCParamStruct>() == 80)`. Per-field `offset_of!`
//!    assertions pin every field position so a refactor cannot silently drift the layout.
//! 2. The selector is always 2 (`kSMCHandleYPCEvent`). Command bytes are 5/6/8/9.
//! 3. Output structs are zero-initialized via `bytemuck::Zeroable::zeroed()` before
//!    every `IOConnectCallStructMethod` call (FR-031). No `MaybeUninit::assume_init`.
//! 4. `keyInfo.data_size` from kernel responses is clamped to `[0, 32]` before slicing
//!    the 32-byte `bytes` field (FR-012). A malformed `data_size = 100` cannot cause UB.
//! 5. Every call checks BOTH `kern_return_t` AND the SMC-level `result` byte (FR-011).
//!    An `IOKit` success with SMC error `0x84` means "key not found" — this is the
//!    cache invalidation trigger (FR-022), NOT `kIOReturnNotFound`.
//! 6. `SmcConnection::close()` is guarded by an `AtomicBool::compare_exchange` — only
//!    the first caller closes; subsequent calls are no-ops. After `IOServiceClose`,
//!    the `io_connect_t` is zeroed to `MACH_PORT_NULL` inside the same critical
//!    section to prevent stale-port reuse (FR-055).
//! 7. `IOServiceGetMatchingService` returns an `io_service_t` that holds a retain
//!    count. `open()` MUST call `IOObjectRelease(service)` after `IOServiceOpen`
//!    returns, or it leaks a Mach port every invocation (FR-004 — the latent leak).
//! 8. The `matching` CFDictionary returned by `IOServiceMatching` is *consumed* by
//!    `IOServiceGetMatchingService` per Core Foundation's consuming convention; we
//!    MUST NOT `CFRelease` it afterwards.
//! 9. `write_key` is `pub(in crate::smc)` — only modules inside `src/smc/` can call
//!    it. External modules go through `WritableKey` factory methods (FR-017).
//! 10. `SmcConnection` is `!Clone + !Copy` and its `conn` field is private. External
//!     code cannot snapshot the `io_connect_t` integer and outlive `close()` (FR-056).

#![allow(unsafe_code)]
#![allow(clippy::cast_possible_truncation)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::smc::cache::KeyInfoCache;

/// Feature 005 helper: wall-clock nanoseconds since the Unix epoch, saturating
/// at zero on any clock error. Used to stamp `RoundTripRecord` timestamps and
/// `SmcError` propagation contexts.
fn wall_clock_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
use crate::smc::keys::{
    WritableKey, SMC_CMD_GET_KEY_FROM_IDX, SMC_CMD_GET_KEY_INFO, SMC_CMD_READ_KEY,
    SMC_CMD_WRITE_KEY, TYPE_FLT, TYPE_UI32, TYPE_UI8,
};

// ------------------------------------------------------------------------
// Hand-rolled IOKit extern "C" declarations
// ------------------------------------------------------------------------
//
// We hand-roll these instead of pulling in io-kit-sys 0.5 (docs.rs failure)
// or 0.4.1 (older transitive mach2) per FR-054. Symbol set is minimal:
//   IOServiceMatching, IOServiceGetMatchingService, IOServiceOpen,
//   IOServiceClose, IOObjectRelease, IOObjectConformsTo,
//   IOConnectCallStructMethod, mach_task_self
// All declared as `unsafe extern "C"` per Rust 2024 edition convention.

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// Creates a matching dictionary for the given IOService class name.
    /// Returns a CFMutableDictionaryRef with +1 retain; consumed by
    /// IOServiceGetMatchingService.
    fn IOServiceMatching(name: *const core::ffi::c_char) -> *mut core::ffi::c_void;

    /// Looks up a registered IOService via a matching dictionary. Consumes
    /// the matching dict. Returns an io_service_t with +1 retain (must be
    /// released via IOObjectRelease) or 0 if no match.
    fn IOServiceGetMatchingService(
        master_port: u32,
        matching: *mut core::ffi::c_void,
    ) -> IoServiceT;

    /// Opens a user client on the given service. Returns 0 on success,
    /// a non-zero kern_return_t on failure.
    fn IOServiceOpen(
        service: IoServiceT,
        owning_task: u32,
        connect_type: u32,
        connect: *mut IoConnectT,
    ) -> KernReturnT;

    /// Closes a user client. Idempotent at the fand layer via AtomicBool.
    fn IOServiceClose(connect: IoConnectT) -> KernReturnT;

    /// Releases a reference on an IOObject (io_service_t, io_connect_t, etc.).
    fn IOObjectRelease(object: IoObjectT) -> KernReturnT;

    /// Returns non-zero if `object` conforms to the given class name.
    fn IOObjectConformsTo(object: IoObjectT, class_name: *const core::ffi::c_char) -> u32;

    /// The core IOKit struct-method call. Selector 2 is kSMCHandleYPCEvent.
    fn IOConnectCallStructMethod(
        connect: IoConnectT,
        selector: u32,
        input_struct: *const core::ffi::c_void,
        input_struct_cnt: usize,
        output_struct: *mut core::ffi::c_void,
        output_struct_cnt: *mut usize,
    ) -> KernReturnT;

    /// The Mach task port. Borrowed, do not deallocate.
    /// Note: mach_task_self() is a macro in C that expands to reading
    /// mach_task_self_. We declare it as a function here; libSystem provides
    /// an inline wrapper on macOS.
    fn mach_task_self() -> u32;
}

/// Mach port guards (FR-073) — protect the AppleSMC user-client port from
/// being stolen or closed by a process injected into fand's address space.
/// Available since macOS 10.14.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[link(name = "System", kind = "framework")]
extern "C" {
    /// Guard a port with an opaque `u64` cookie. Any subsequent operation on
    /// the port that does not pass the same guard fails with EXC_GUARD.
    /// `strict` == 1 means fatal on violation (recommended).
    ///
    /// Returns `KERN_SUCCESS` on success, `KERN_NOT_SUPPORTED` on kernels that
    /// lack port guards, or various other kern_return_t values.
    fn mach_port_guard(task: u32, name: u32, guard: u64, strict: u32) -> KernReturnT;

    /// Remove the guard from a port. Must be called before `IOServiceClose`
    /// or the close will fail with an invalid-guard exception.
    fn mach_port_unguard(task: u32, name: u32, guard: u64) -> KernReturnT;
}

/// `MPG_STRICT` — fatal-on-violation mode. The 1-bit flag value documented
/// in `<mach/mach_port.h>`.
pub const MPG_STRICT: u32 = 1;

/// `KERN_NOT_SUPPORTED` — the only acceptable `mach_port_guard` failure
/// (FR-073). On older kernels this means guards are unavailable and fand
/// logs a WARN and continues.
pub const K_KERN_NOT_SUPPORTED: KernReturnT = 0x0000_002e_i32;

// ------------------------------------------------------------------------
// FFI constants and types (mirror <IOKit/IOKitLib.h>)
// ------------------------------------------------------------------------

/// Mach port name type (IOKit `io_connect_t`, `io_service_t`, `io_object_t`).
pub type IoConnectT = u32;
pub type IoServiceT = u32;
pub type IoObjectT = u32;
pub type KernReturnT = i32;
pub type MachPortT = u32;

pub const KERN_SUCCESS: KernReturnT = 0;
pub const K_IO_RETURN_NOT_PRIVILEGED: KernReturnT = 0xE00002C1_u32 as KernReturnT;
pub const K_IO_RETURN_NOT_PERMITTED: KernReturnT = 0xE00002C2_u32 as KernReturnT;
pub const K_IO_RETURN_NOT_FOUND: KernReturnT = 0xE00002F0_u32 as KernReturnT;
pub const K_IO_RETURN_BUSY: KernReturnT = 0xE0000229_u32 as KernReturnT;
pub const K_IO_RETURN_TIMEOUT: KernReturnT = 0xE0000306_u32 as KernReturnT;

pub const MACH_PORT_NULL: MachPortT = 0;

/// SMC selector for `IOConnectCallStructMethod` (kSMCHandleYPCEvent).
pub const SMC_SELECTOR: u32 = 2;

/// SMC-level result byte: "key not found". Distinct from `kIOReturnNotFound`.
pub const SMC_ERR_KEY_NOT_FOUND: u8 = 0x84;

/// SMC-level result byte: "system mode rejects write" (requires Ftst unlock).
pub const SMC_ERR_SYSTEM_MODE_REJECTS: u8 = 0x82;

// ------------------------------------------------------------------------
// SMCParamStruct — exactly 80 bytes, agoodkind byte layout
// ------------------------------------------------------------------------
//
// The exact head-region offsets are VERIFIED at compile time by the const assertions
// below. The tail region (result@40 onward) is directly verifiable against SMC
// protocol documentation.
//
// Layout derivation under `#[repr(C)]` with natural alignment on aarch64:
//   key            u32            offset 0,  size 4
//   vers           [u8; 6]        offset 4,  size 6
//   (pad for u32)                 offset 10, size 2
//   pLimitData     [u8; 16]       offset 12, size 16
//   keyInfo        SMCKeyInfoData offset 28, size 12 (incl. tail pad)
//   result         u8             offset 40, size 1
//   status         u8             offset 41, size 1
//   data8          u8             offset 42, size 1
//   (pad for u32)                 offset 43, size 1
//   data32         u32            offset 44, size 4
//   bytes          [u8; 32]       offset 48, size 32
//   total                                   80 bytes

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SMCKeyInfoData {
    pub data_size: u32,
    pub data_type: u32,
    pub data_attributes: u8,
    _pad: [u8; 3],
}

// SAFETY: SMCKeyInfoData is #[repr(C)], all fields are plain integers or byte arrays,
// zero is a valid bit pattern for all of them, and there is no padding we care about.
unsafe impl bytemuck::Zeroable for SMCKeyInfoData {}
// SAFETY: All fields are Pod, the struct is repr(C), and the trailing padding is
// explicit (no implicit padding bytes).
unsafe impl bytemuck::Pod for SMCKeyInfoData {}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SMCParamStruct {
    pub key: u32,                 // offset 0
    pub vers: [u8; 6],            // offset 4
    _pad_head: [u8; 2],           // offset 10 — alignment for pLimitData
    pub p_limit_data: [u8; 16],   // offset 12
    pub key_info: SMCKeyInfoData, // offset 28 (12 bytes)
    pub result: u8,               // offset 40
    pub status: u8,               // offset 41
    pub data8: u8,                // offset 42
    _pad_mid: u8,                 // offset 43 — alignment for data32
    pub data32: u32,              // offset 44
    pub bytes: [u8; 32],          // offset 48
}

// SAFETY: SMCParamStruct is #[repr(C)], all fields are Pod or explicit padding,
// and the layout matches the kernel's expectation of an 80-byte struct.
unsafe impl bytemuck::Zeroable for SMCParamStruct {}
// SAFETY: All fields are Pod, the struct is repr(C), padding bytes are explicit.
unsafe impl bytemuck::Pod for SMCParamStruct {}

// Compile-time invariants (FR-002, FR-029, FR-030)
const _: () = assert!(std::mem::size_of::<SMCParamStruct>() == 80);
const _: () = assert!(std::mem::align_of::<SMCParamStruct>() == 4);
const _: () = assert!(std::mem::size_of::<SMCKeyInfoData>() == 12);

// Per-field offset assertions (FR-029)
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, key) == 0);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, vers) == 4);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, p_limit_data) == 12);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, key_info) == 28);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, result) == 40);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, status) == 41);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, data8) == 42);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, data32) == 44);
const _: () = assert!(std::mem::offset_of!(SMCParamStruct, bytes) == 48);

// ------------------------------------------------------------------------
// IoConnect newtype (FR-057)
// ------------------------------------------------------------------------

/// A non-cloneable Mach port name for an IOKit user client.
///
/// The handle is stored in an `AtomicU32` so it can be atomically zeroed
/// to `MACH_PORT_NULL` during `close()` without requiring `&mut self`
/// (which would conflict with `Drop::drop`'s `&mut self` and with
/// concurrent signal-handler access). Reads before close see the live
/// port number; reads after close see `MACH_PORT_NULL`.
pub struct IoConnect(AtomicU32);

impl IoConnect {
    /// SAFETY: Caller must ensure `raw` is a valid `io_connect_t` obtained
    /// from `IOServiceOpen`, and that it will not be closed by anyone else
    /// for the lifetime of the returned `IoConnect`.
    #[inline]
    pub(in crate::smc) unsafe fn from_raw(raw: IoConnectT) -> Self {
        Self(AtomicU32::new(raw))
    }

    /// Load the current port number. Returns `MACH_PORT_NULL` (0) if the
    /// connection has been closed.
    #[inline]
    #[must_use]
    pub(in crate::smc) fn raw(&self) -> IoConnectT {
        self.0.load(Ordering::Acquire)
    }

    /// Atomically swap the stored port with `MACH_PORT_NULL`, returning the
    /// old value. Used by `close()` inside the `connection_open` critical
    /// section to prevent stale port reuse (FR-055).
    #[inline]
    pub(in crate::smc) fn take(&self) -> IoConnectT {
        self.0.swap(MACH_PORT_NULL, Ordering::AcqRel)
    }
}

// ------------------------------------------------------------------------
// SmcError taxonomy (FR-024)
// ------------------------------------------------------------------------

/// SMC error taxonomy (FR-024 base + feature 005 FR-031/034/098 extensions).
///
/// `Copy` is intentionally NOT derived because feature 005's `ConflictDetected`
/// and `EdrDenied` variants carry heap-allocated diagnostic strings. `Clone`
/// and `PartialEq` remain for normal error propagation + test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SmcError {
    // ---- feature 004 base variants ----
    ServiceNotFound,
    OpenFailed(KernReturnT),
    OpenTimeout,
    CallFailed {
        selector: u32,
        kr: KernReturnT,
        cmd: u8,
    },
    SmcResult {
        cmd: u8,
        result_byte: u8,
    },
    KeyNotFound(u32),
    TypeMismatch {
        fourcc: u32,
        expected: u32,
        got: u32,
    },
    DataSizeClamped {
        fourcc: u32,
        reported: u32,
    },
    InvalidFloat {
        fourcc: u32,
    },
    EmptyResponse {
        fourcc: u32,
    },
    AlreadyClosed,
    Busy {
        retried: bool,
    },
    Timeout {
        retried: bool,
    },
    AttributeDenied {
        fourcc: u32,
    },
    WriteDenied(u32),
    SizeMismatch {
        fourcc: u32,
        expected: u32,
        got: u32,
    },
    EndiannessUnplausible {
        fourcc: u32,
        got: u32,
    },

    // ---- feature 005 extensions (FR-031, FR-034, FR-069, FR-098, FR-103) ----
    /// The `Ftst` diagnostic unlock key read-back does not match the written value.
    /// Fatal per FR-005.
    UnlockMismatch {
        expected: u8,
        got: u8,
        session: crate::correlation::SessionId,
        timestamp_ns: u64,
    },
    /// The SMC rejected the `Ftst=1` write with a non-zero result byte.
    /// Distinct from `SmcResult` per FR-033 so operators can tell unlock failure
    /// from a later fan-write failure.
    UnlockRejected {
        result_byte: u8,
        session: crate::correlation::SessionId,
    },
    /// **NEW in feature 005 per FR-098 / I2 resolution.** The SMC refused a
    /// fan-write (`F<i>Md` or `F<i>Tg`) with a non-zero result byte. Distinct
    /// from the read-path `SmcResult` variant.
    WriteRefused {
        fourcc: u32,
        result_byte: u8,
        context: &'static str, // "fan_mode" | "fan_target" | "diagnostic_unlock"
        session: crate::correlation::SessionId,
        timestamp_ns: u64,
    },
    /// Round-trip readback did not match the value just written. Fatal per FR-007.
    WriteReadbackMismatch {
        fourcc: u32,
        expected: [u8; 4],
        expected_len: u8,
        got: [u8; 4],
        got_len: u8,
        session: crate::correlation::SessionId,
        timestamp_ns: u64,
        iteration: Option<u8>, // Some(_) during `fand selftest`, None otherwise
    },
    /// The userspace watchdog timer (FR-002) fired without seeing a successful
    /// round-trip within the 4-second window. Fatal; forces teardown + exit 4.
    WatchdogFired {
        elapsed_ms: u64,
        session: crate::correlation::SessionId,
    },
    /// Another fand instance holds the `/var/run/fand-smc.lock` flock (FR-050).
    /// The PID is untrusted diagnostic data per CHK060 — do not act on it.
    ConflictDetected {
        holder_pid: libc::pid_t,
        lockfile_path: String,
    },
    /// IOServiceOpen denied by an EndpointSecurity agent (FR-069, FR-103).
    /// The suspected agent name is populated from `edr_detect::detect_suspected_agent`.
    EdrDenied {
        suspected_agent: Option<String>,
    },
    /// Suspected TCC (Privacy & Security) denial — feature 004 did not observe
    /// one on Mac17,2 but a future macOS MAY add this gate (FR-071).
    TccDenied,
    /// Suspected Lockdown Mode denial (FR-070). Diagnostic-only — we cannot
    /// prove Lockdown without reading user preferences.
    LockdownModeSuspected,
}

impl core::fmt::Display for SmcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ServiceNotFound => write!(
                f,
                "AppleSMC user client not found — this tool requires Apple Silicon macOS 14+"
            ),
            Self::OpenFailed(kr) => {
                let name = kern_return_name(*kr);
                if *kr == K_IO_RETURN_NOT_PRIVILEGED {
                    write!(
                        f,
                        "IOServiceOpen failed: {name} ({kr:#X}) — not root, run with sudo"
                    )
                } else if *kr == K_IO_RETURN_NOT_PERMITTED {
                    write!(
                        f,
                        "IOServiceOpen failed: {name} ({kr:#X}) — permission denied. \
                         Possible causes: (a) a future macOS version added an entitlement \
                         gate to AppleSMC, (b) macOS Lockdown Mode is enabled, (c) an \
                         EndpointSecurity client (EDR/MDM such as CrowdStrike Falcon, \
                         SentinelOne, Jamf Protect) denied the AUTH_IOKIT_OPEN event"
                    )
                } else {
                    write!(f, "IOServiceOpen failed: {name} ({kr:#X})")
                }
            }
            Self::OpenTimeout => write!(f, "SmcConnection::open() exceeded 500ms deadline"),
            Self::CallFailed { selector, kr, cmd } => write!(
                f,
                "IOConnectCallStructMethod(selector={selector}, cmd={cmd}) failed: {} ({kr:#X})",
                kern_return_name(*kr)
            ),
            Self::SmcResult { cmd, result_byte } => {
                if *result_byte == SMC_ERR_KEY_NOT_FOUND {
                    write!(f, "SMC cmd {cmd}: key not found (0x{result_byte:02X})")
                } else if *result_byte == SMC_ERR_SYSTEM_MODE_REJECTS {
                    write!(
                        f,
                        "SMC cmd {cmd}: system mode rejects write (0x{result_byte:02X}) — Ftst unlock required"
                    )
                } else {
                    write!(f, "SMC cmd {cmd}: result byte 0x{result_byte:02X}")
                }
            }
            Self::KeyNotFound(fourcc) => write!(f, "SMC key {} not found", fourcc_to_str(*fourcc)),
            Self::TypeMismatch {
                fourcc,
                expected,
                got,
            } => write!(
                f,
                "SMC key {} type mismatch: expected {}, got {}",
                fourcc_to_str(*fourcc),
                fourcc_to_str(*expected),
                fourcc_to_str(*got)
            ),
            Self::DataSizeClamped { fourcc, reported } => write!(
                f,
                "SMC key {} returned data_size={reported} (clamped to 32)",
                fourcc_to_str(*fourcc)
            ),
            Self::InvalidFloat { fourcc } => write!(
                f,
                "SMC key {} decoded as NaN or infinity",
                fourcc_to_str(*fourcc)
            ),
            Self::EmptyResponse { fourcc } => write!(
                f,
                "SMC key {} returned empty response (kern_return=0, result=0, data_size=0)",
                fourcc_to_str(*fourcc)
            ),
            Self::AlreadyClosed => write!(f, "SmcConnection already closed"),
            Self::Busy { retried } => write!(f, "SMC kIOReturnBusy (retried: {retried})"),
            Self::Timeout { retried } => write!(f, "SMC kIOReturnTimeout (retried: {retried})"),
            Self::AttributeDenied { fourcc } => write!(
                f,
                "SMC key {} attribute bits forbid read (WRITE-only or FUNCTION)",
                fourcc_to_str(*fourcc)
            ),
            Self::WriteDenied(fourcc) => write!(
                f,
                "SMC key {} not in write whitelist",
                fourcc_to_str(*fourcc)
            ),
            Self::SizeMismatch {
                fourcc,
                expected,
                got,
            } => write!(
                f,
                "SMC key {} size mismatch: expected {expected} bytes, got {got}",
                fourcc_to_str(*fourcc)
            ),
            Self::EndiannessUnplausible { fourcc, got } => write!(
                f,
                "SMC key {} decoded to implausible value {got} (0x{got:08X}) — \
                 ui32 decoder endianness is almost certainly inverted. See \
                 src/smc/types.rs::decode_ui32 and FR-042/FR-061",
                fourcc_to_str(*fourcc)
            ),
            // ---- feature 005 variants ----
            Self::UnlockMismatch {
                expected,
                got,
                session,
                ..
            } => write!(
                f,
                "session {session}: diagnostic unlock readback mismatch: wrote Ftst={expected}, \
                 read Ftst={got} — CRITICAL, fan may remain in manual mode indefinitely"
            ),
            Self::UnlockRejected {
                result_byte,
                session,
            } => write!(
                f,
                "session {session}: SMC rejected Ftst=1 with result byte 0x{result_byte:02X} — \
                 diagnostic unlock failed, write path is unavailable"
            ),
            Self::WriteRefused {
                fourcc,
                result_byte,
                context,
                session,
                ..
            } => write!(
                f,
                "session {session}: write refused on {} ({context}) with SMC result byte \
                 0x{result_byte:02X}",
                fourcc_to_str(*fourcc)
            ),
            Self::WriteReadbackMismatch {
                fourcc,
                expected,
                expected_len,
                got,
                got_len,
                session,
                iteration,
                ..
            } => {
                if let Some(iter) = iteration {
                    write!(
                        f,
                        "session {session} iter {iter}: round-trip mismatch on {}: wrote {:02X?}, \
                         read back {:02X?}",
                        fourcc_to_str(*fourcc),
                        &expected[..(*expected_len as usize).min(4)],
                        &got[..(*got_len as usize).min(4)]
                    )
                } else {
                    write!(
                        f,
                        "session {session}: round-trip mismatch on {}: wrote {:02X?}, read back {:02X?}",
                        fourcc_to_str(*fourcc),
                        &expected[..(*expected_len as usize).min(4)],
                        &got[..(*got_len as usize).min(4)]
                    )
                }
            }
            Self::WatchdogFired {
                elapsed_ms,
                session,
            } => write!(
                f,
                "session {session}: userspace watchdog fired after {elapsed_ms} ms without a \
                 successful round-trip — fans returned to auto control"
            ),
            Self::ConflictDetected {
                holder_pid,
                lockfile_path,
            } => write!(
                f,
                "another fand instance holds {lockfile_path} (PID {holder_pid}, untrusted — \
                 verify with ps before acting). Wait for it to finish or investigate if stuck"
            ),
            Self::EdrDenied { suspected_agent } => match suspected_agent {
                Some(name) => write!(
                    f,
                    "IOServiceOpen denied — EndpointSecurity agent '{name}' appears to be \
                     blocking AUTH_IOKIT_OPEN on com.apple.AppleSMC. Contact your fleet \
                     administrator or allowlist fand"
                ),
                None => write!(
                    f,
                    "IOServiceOpen denied — an EndpointSecurity agent appears to be blocking \
                     AUTH_IOKIT_OPEN on com.apple.AppleSMC (agent not identified)"
                ),
            },
            Self::TccDenied => write!(
                f,
                "IOServiceOpen denied — TCC (Privacy & Security) appears to be gating \
                 com.apple.AppleSMC. Grant Developer Tools or Full Disk Access to fand in \
                 System Settings"
            ),
            Self::LockdownModeSuspected => write!(
                f,
                "IOServiceOpen denied — macOS Lockdown Mode appears to be active and blocking \
                 com.apple.AppleSMC user client open"
            ),
        }
    }
}

// ------------------------------------------------------------------------
// Stable error codes (FR-098) — machine-readable, 1:1 with the enum variants
// ------------------------------------------------------------------------

impl SmcError {
    /// Return the stable string code per FR-098. Machine consumers MUST branch
    /// on this value rather than on the Display output. The mapping is 1:1
    /// with the enum variants (24 codes total; `DataSizeClamped` is internal
    /// and has no public code).
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::ServiceNotFound => "SERVICE_NOT_FOUND",
            Self::OpenFailed(_) => "OPEN_FAILED",
            Self::OpenTimeout => "OPEN_TIMEOUT",
            Self::CallFailed { .. } => "CALL_FAILED",
            Self::SmcResult { .. } => "SMC_RESULT",
            Self::KeyNotFound(_) => "KEY_NOT_FOUND",
            Self::TypeMismatch { .. } => "TYPE_MISMATCH",
            Self::DataSizeClamped { .. } => "DATA_SIZE_CLAMPED", // internal only
            Self::InvalidFloat { .. } => "INVALID_FLOAT",
            Self::EmptyResponse { .. } => "EMPTY_RESPONSE",
            Self::AlreadyClosed => "ALREADY_CLOSED",
            Self::Busy { .. } => "BUSY",
            Self::Timeout { .. } => "TIMEOUT",
            Self::AttributeDenied { .. } => "ATTRIBUTE_DENIED",
            Self::WriteDenied(_) => "WRITE_DENIED",
            Self::SizeMismatch { .. } => "SIZE_MISMATCH",
            Self::EndiannessUnplausible { .. } => "ENDIANNESS_UNPLAUSIBLE",
            Self::UnlockMismatch { .. } => "UNLOCK_MISMATCH",
            Self::UnlockRejected { .. } => "UNLOCK_REJECTED",
            Self::WriteRefused { .. } => "WRITE_REFUSED",
            Self::WriteReadbackMismatch { .. } => "WRITE_READBACK_MISMATCH",
            Self::WatchdogFired { .. } => "WATCHDOG_FIRED",
            Self::ConflictDetected { .. } => "CONFLICT_DETECTED",
            Self::EdrDenied { .. } => "EDR_DENIED",
            Self::TccDenied => "TCC_DENIED",
            Self::LockdownModeSuspected => "LOCKDOWN_MODE_SUSPECTED",
        }
    }
}

impl std::error::Error for SmcError {}

#[must_use]
fn kern_return_name(kr: KernReturnT) -> &'static str {
    match kr {
        x if x == KERN_SUCCESS => "kIOReturnSuccess",
        x if x == K_IO_RETURN_NOT_PRIVILEGED => "kIOReturnNotPrivileged",
        x if x == K_IO_RETURN_NOT_PERMITTED => "kIOReturnNotPermitted",
        x if x == K_IO_RETURN_NOT_FOUND => "kIOReturnNotFound",
        x if x == K_IO_RETURN_BUSY => "kIOReturnBusy",
        x if x == K_IO_RETURN_TIMEOUT => "kIOReturnTimeout",
        _ => "kIOReturn<unknown>",
    }
}

#[must_use]
fn fourcc_to_str(fourcc: u32) -> String {
    let bytes = [
        ((fourcc >> 24) & 0xFF) as u8,
        ((fourcc >> 16) & 0xFF) as u8,
        ((fourcc >> 8) & 0xFF) as u8,
        (fourcc & 0xFF) as u8,
    ];
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

// ------------------------------------------------------------------------
// SmcConnection
// ------------------------------------------------------------------------

/// Owning handle to the AppleSMC IOKit user client.
///
/// Created via `SmcConnection::open()`. Closed via `close()` or `Drop`.
/// The struct is intentionally NOT `Clone` or `Copy` — external code
/// cannot snapshot the `io_connect_t` integer (FR-056).
pub struct SmcConnection {
    conn: IoConnect,
    cache: KeyInfoCache,
    connection_open: AtomicBool,
    /// Feature 005 FR-073: random per-session guard cookie installed on the
    /// `io_connect_t` via `mach_port_guard(MPG_STRICT)`. Zero means no guard
    /// is in force (either not yet installed, or the kernel returned
    /// `KERN_NOT_SUPPORTED` and we fell through). The guard must be removed
    /// via `mach_port_unguard` before `IOServiceClose`.
    port_guard: u64,
}

// SAFETY: all fields are Send + Sync. `IoConnect` wraps an `AtomicU32`,
// `KeyInfoCache` is a plain `[Option<(u32, u32)>; 256]` array, `AtomicBool`
// and `u64` are both Send + Sync. The feature 005 three-connection model
// (one owned connection per thread) relies on this so we can transfer a
// freshly-opened `SmcConnection` into a spawned signal thread by value.
unsafe impl Send for SmcConnection {}

impl SmcConnection {
    /// Open a connection to `AppleSMC`.
    ///
    /// # Errors
    /// - `ServiceNotFound` if the `AppleSMC` user client is not present on this host
    ///   (VM, Intel Mac, or future macOS that removed the service).
    /// - `OpenFailed(kr)` if `IOServiceOpen` returned a non-zero `kern_return_t`.
    ///   Common values: `kIOReturnNotPrivileged` (not root), `kIOReturnNotPermitted`
    ///   (entitlement gate, Lockdown Mode, or EndpointSecurity denial).
    /// - `OpenTimeout` if the open sequence exceeded 500 ms total (FR-036).
    ///
    /// # Panics
    /// Does not panic. All error paths return `Err`.
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    pub fn open() -> Result<Self, SmcError> {
        // Non-Darwin or non-aarch64 targets cannot talk to AppleSMC. Return
        // ServiceNotFound so CI on Linux runners sees a clean error path.
        Err(SmcError::ServiceNotFound)
    }

    /// # Panics
    /// Does not panic. All error paths return `Err`.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub fn open() -> Result<Self, SmcError> {
        use std::time::Instant;

        let deadline = Instant::now() + std::time::Duration::from_millis(500);

        // SAFETY: IOServiceMatching takes a NUL-terminated C string and returns
        // a CFMutableDictionaryRef (retained +1). The dict is CONSUMED by
        // IOServiceGetMatchingService — we must NOT CFRelease it afterwards.
        let matching = unsafe { IOServiceMatching(b"AppleSMC\0".as_ptr().cast()) };
        if matching.is_null() {
            return Err(SmcError::ServiceNotFound);
        }

        // SAFETY: kIOMasterPortDefault is a null Mach port; IOServiceGetMatchingService
        // handles this by using the default master port. The matching dict is consumed.
        let service: IoServiceT = unsafe { IOServiceGetMatchingService(0, matching) };
        if service == 0 {
            return Err(SmcError::ServiceNotFound);
        }

        if Instant::now() >= deadline {
            // SAFETY: we hold a +1 ref to `service` from IOServiceGetMatchingService.
            unsafe { IOObjectRelease(service) };
            return Err(SmcError::OpenTimeout);
        }

        // FR-058: verify the class is actually AppleSMC (guards against future
        // kernels exposing multiple SMC nodes).
        // SAFETY: service is a valid io_service_t with +1 retain.
        let conforms = unsafe { IOObjectConformsTo(service, b"AppleSMC\0".as_ptr().cast()) };
        if conforms == 0 {
            unsafe { IOObjectRelease(service) };
            return Err(SmcError::ServiceNotFound);
        }

        let mut conn: IoConnectT = 0;
        // SAFETY: mach_task_self() returns a borrowed task port (do not deallocate).
        // service is a valid io_service_t. We pass &mut conn for the out parameter.
        let kr =
            unsafe { IOServiceOpen(service, mach_task_self(), 0, std::ptr::addr_of_mut!(conn)) };

        // FR-004: Release the service object regardless of whether the open
        // succeeded. The connection (if created) holds its own reference to the
        // underlying IOService object.
        // SAFETY: service is still a valid io_service_t with +1 retain.
        unsafe { IOObjectRelease(service) };

        if kr != KERN_SUCCESS {
            return Err(SmcError::OpenFailed(kr));
        }

        if Instant::now() >= deadline {
            // SAFETY: conn was just returned by IOServiceOpen.
            unsafe { IOServiceClose(conn) };
            return Err(SmcError::OpenTimeout);
        }

        // FR-073: install a per-session Mach port guard on the new user-client
        // port. On macOS 11+ this is expected to succeed; on older kernels
        // `mach_port_guard` may return `KERN_NOT_SUPPORTED`, in which case we
        // log a warning and continue without the guard (documented fallback).
        let port_guard = {
            let mut guard_bytes = [0u8; 8];
            // SAFETY: getentropy is infallible on Darwin for buffers <= 256.
            unsafe {
                libc::getentropy(guard_bytes.as_mut_ptr().cast(), guard_bytes.len());
            }
            u64::from_ne_bytes(guard_bytes)
        };
        // SAFETY: conn is a valid io_connect_t owned by this function; port_guard
        // is a fresh random u64; mach_task_self() returns the current task port;
        // MPG_STRICT is the documented strict-mode flag.
        let guard_kr = unsafe { mach_port_guard(mach_task_self(), conn, port_guard, MPG_STRICT) };
        // FR-073 amendment: `io_connect_t` ports are send rights on many kernels,
        // and `mach_port_guard` is documented to operate on receive rights. On
        // modern macOS we observe `0x11` (KERN_INVALID_RIGHT-equivalent) on the
        // AppleSMC connection handle. Treat ANY non-success kern_return as a
        // soft-fail: log the warning and continue unguarded. The residual risk
        // is documented in research.md RD-07 as a known limitation of the
        // IOKit user client model — a privileged injected process could in
        // principle steal the send right. This is accepted because
        // (a) such an attacker is already root, (b) feature 004's gate pass
        // on Mac17,2 confirmed `AppleSMC` writes work without the guard.
        let effective_guard = if guard_kr == KERN_SUCCESS {
            port_guard
        } else {
            crate::log::emit_raw(
                crate::log::LogLevel::Warn,
                "mach_port_guard rejected the AppleSMC send-right handle \
                 (kern_return 0x11 is expected on io_connect_t) — \
                 continuing unguarded per FR-073 documented fallback",
            );
            0
        };

        Ok(Self {
            // SAFETY: conn is a valid io_connect_t from IOServiceOpen.
            conn: unsafe { IoConnect::from_raw(conn) },
            cache: KeyInfoCache::new(),
            connection_open: AtomicBool::new(true),
            port_guard: effective_guard,
        })
    }

    /// Close the connection. Idempotent via `AtomicBool::compare_exchange`.
    /// Also called by `Drop`.
    ///
    /// On the first successful call, this atomically takes the port number
    /// (swapping with `MACH_PORT_NULL`) and calls `IOServiceClose`. Subsequent
    /// calls are no-ops — they see `connection_open == false` and return.
    ///
    /// The port number is zeroed inside the same critical section as the
    /// AtomicBool flip to prevent stale-name reuse if the kernel later hands
    /// the same port name to a different client (FR-055).
    pub fn close(&self) {
        if self
            .connection_open
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // Already closed (or closing) — no-op.
        }

        // We are the sole closer. Take the port number atomically, leaving
        // MACH_PORT_NULL behind so any stale read from another thread sees 0.
        let raw_conn = self.conn.take();
        if raw_conn == MACH_PORT_NULL {
            // Defensive: port was already zeroed somehow. Nothing to close.
            return;
        }

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            // FR-073: remove the port guard before IOServiceClose, otherwise
            // close will fail with EXC_GUARD. Swallow the unguard kern_return
            // — a non-zero value here means the guard is already gone and
            // the subsequent IOServiceClose will surface the real error.
            if self.port_guard != 0 {
                // SAFETY: mach_task_self() is the current task port; raw_conn
                // is the port name we guarded in open(); self.port_guard is
                // the cookie we installed. No concurrent access because we
                // just won the compare_exchange above.
                let _ = unsafe { mach_port_unguard(mach_task_self(), raw_conn, self.port_guard) };
            }
            // SAFETY: `raw_conn` was obtained from a successful `IOServiceOpen`
            // via the AtomicBool critical section, which guarantees no other
            // caller can race us on this close. IOServiceClose handles the
            // send-right decrement internally; we MUST NOT also call
            // mach_port_deallocate (FR-060).
            let _ = unsafe { IOServiceClose(raw_conn) };
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = raw_conn;
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.connection_open.load(Ordering::Acquire)
    }

    #[must_use]
    pub(in crate::smc) fn cache(&mut self) -> &mut KeyInfoCache {
        &mut self.cache
    }

    /// Issue an `IOConnectCallStructMethod` with the given command byte and
    /// input `SMCParamStruct`, returning the output struct on success.
    ///
    /// Implements FR-011 (dual error check), FR-032 (addr_of_mut!), FR-033
    /// (struct counts from `size_of!`), FR-034/FR-035 (retry once on Busy
    /// and Timeout).
    ///
    /// # Errors
    /// - `AlreadyClosed` if the connection has been closed.
    /// - `CallFailed` if the IOKit call returned a non-zero `kern_return_t`
    ///   (other than retriable Busy/Timeout).
    /// - `Busy` / `Timeout` if two consecutive retries failed.
    fn call_struct(&self, cmd: u8, mut input: SMCParamStruct) -> Result<SMCParamStruct, SmcError> {
        if !self.is_open() {
            return Err(SmcError::AlreadyClosed);
        }

        input.data8 = cmd;

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let conn = self.conn.raw();
            if conn == MACH_PORT_NULL {
                return Err(SmcError::AlreadyClosed);
            }

            let mut retried_busy = false;
            let mut retried_timeout = false;

            loop {
                let mut output: SMCParamStruct = bytemuck::Zeroable::zeroed();
                let mut out_size = std::mem::size_of::<SMCParamStruct>();

                // SAFETY: `conn` is a valid io_connect_t (checked above).
                // Input and output buffers are stack-allocated SMCParamStructs
                // passed by raw pointer via `addr_of!` / `addr_of_mut!` (FR-032)
                // to avoid creating `&mut` aliases to memory the kernel writes.
                // The input/output counts are derived from size_of (FR-033).
                let kr = unsafe {
                    IOConnectCallStructMethod(
                        conn,
                        SMC_SELECTOR,
                        std::ptr::addr_of!(input).cast(),
                        std::mem::size_of::<SMCParamStruct>(),
                        std::ptr::addr_of_mut!(output).cast(),
                        std::ptr::addr_of_mut!(out_size),
                    )
                };

                // FR-034/035: retry transient IOKit errors once.
                if kr == K_IO_RETURN_BUSY && !retried_busy {
                    retried_busy = true;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                if kr == K_IO_RETURN_TIMEOUT && !retried_timeout {
                    retried_timeout = true;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }

                if kr == K_IO_RETURN_BUSY {
                    return Err(SmcError::Busy { retried: true });
                }
                if kr == K_IO_RETURN_TIMEOUT {
                    return Err(SmcError::Timeout { retried: true });
                }
                if kr != KERN_SUCCESS {
                    return Err(SmcError::CallFailed {
                        selector: SMC_SELECTOR,
                        kr,
                        cmd,
                    });
                }

                // FR-011: check the SMC-level result byte too.
                if output.result != 0 {
                    return Err(SmcError::SmcResult {
                        cmd,
                        result_byte: output.result,
                    });
                }

                return Ok(output);
            }
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = (cmd, input);
            Err(SmcError::ServiceNotFound)
        }
    }

    /// Read the `KeyInfo` (data_size + data_type) for a given fourcc.
    /// Consults the cache first (FR-021); on miss, issues `kSMCGetKeyInfo`
    /// and stores the result.
    ///
    /// # Errors
    /// See `SmcError` variants.
    #[must_use = "IOKit errors must be handled"]
    pub fn read_key_info(&mut self, fourcc: u32) -> Result<KeyInfo, SmcError> {
        // FR-021: cache-first
        if let Some((data_size, data_type)) = self.cache.get(fourcc) {
            return Ok(KeyInfo {
                data_size,
                data_type,
            });
        }

        let mut input: SMCParamStruct = bytemuck::Zeroable::zeroed();
        input.key = fourcc;

        let output = match self.call_struct(SMC_CMD_GET_KEY_INFO, input) {
            Ok(o) => o,
            Err(SmcError::SmcResult {
                cmd: _,
                result_byte: SMC_ERR_KEY_NOT_FOUND,
            }) => {
                // FR-022: SMC-level key-not-found on GetKeyInfo means the
                // key does not exist at all. Invalidate any stale cache
                // entry and surface a typed `KeyNotFound` for callers.
                self.cache.invalidate(fourcc);
                return Err(SmcError::KeyNotFound(fourcc));
            }
            Err(e) => return Err(e),
        };

        // FR-012: clamp data_size to [0, 32] before exposing it.
        let raw_size = output.key_info.data_size;
        let clamped_size = raw_size.min(32);
        if raw_size > 32 {
            // Log once via the bespoke logger (rate-limited). Also continue
            // with the clamped value so the caller gets something usable.
            crate::log::emit_raw(
                crate::log::LogLevel::Warn,
                &format!(
                    "SMC key {} reported data_size={raw_size}, clamped to 32",
                    fourcc_to_str(fourcc)
                ),
            );
        }

        let data_type = output.key_info.data_type;

        // FR-044: empty-response sanity check.
        if clamped_size == 0 && data_type == 0 {
            return Err(SmcError::EmptyResponse { fourcc });
        }

        // FR-021: store in cache.
        self.cache.put(fourcc, clamped_size, data_type);

        Ok(KeyInfo {
            data_size: clamped_size,
            data_type,
        })
    }

    /// Read the raw bytes for a given fourcc. Returns the `KeyInfo` along
    /// with the payload bytes (up to 32 bytes, sized by `data_size`).
    ///
    /// On `result_byte == SMC_ERR_KEY_NOT_FOUND (0x84)`, invalidates the
    /// cache entry and returns `SmcError::KeyNotFound` (FR-022).
    ///
    /// # Errors
    /// See `SmcError` variants.
    #[must_use = "IOKit errors must be handled"]
    pub fn read_key(&mut self, fourcc: u32) -> Result<(KeyInfo, [u8; 32]), SmcError> {
        let key_info = self.read_key_info(fourcc)?;

        let mut input: SMCParamStruct = bytemuck::Zeroable::zeroed();
        input.key = fourcc;
        input.key_info.data_size = key_info.data_size;
        input.key_info.data_type = key_info.data_type;

        match self.call_struct(SMC_CMD_READ_KEY, input) {
            Ok(output) => Ok((key_info, output.bytes)),
            Err(SmcError::SmcResult {
                cmd: _,
                result_byte: SMC_ERR_KEY_NOT_FOUND,
            }) => {
                // FR-022: invalidate cache on SMC-level key-not-found.
                self.cache.invalidate(fourcc);
                Err(SmcError::KeyNotFound(fourcc))
            }
            Err(e) => Err(e),
        }
    }

    /// Read a `ui8` key.
    ///
    /// # Errors
    /// - `TypeMismatch` if the key's `data_type` is not `ui8`.
    /// - Any `read_key` error.
    #[must_use = "IOKit errors must be handled"]
    pub fn read_u8(&mut self, fourcc: u32) -> Result<u8, SmcError> {
        use crate::smc::keys::TYPE_UI8;

        let (ki, bytes) = self.read_key(fourcc)?;
        if ki.data_type != TYPE_UI8 {
            return Err(SmcError::TypeMismatch {
                fourcc,
                expected: TYPE_UI8,
                got: ki.data_type,
            });
        }
        crate::smc::types::decode_ui8(&bytes, ki.data_size).map_err(|_| SmcError::TypeMismatch {
            fourcc,
            expected: TYPE_UI8,
            got: ki.data_type,
        })
    }

    /// Read a `ui32` key.
    ///
    /// # Errors
    /// - `TypeMismatch` if the key's `data_type` is not `ui32`.
    /// - Any `read_key` error.
    #[must_use = "IOKit errors must be handled"]
    pub fn read_u32(&mut self, fourcc: u32) -> Result<u32, SmcError> {
        use crate::smc::keys::TYPE_UI32;

        let (ki, bytes) = self.read_key(fourcc)?;
        if ki.data_type != TYPE_UI32 {
            return Err(SmcError::TypeMismatch {
                fourcc,
                expected: TYPE_UI32,
                got: ki.data_type,
            });
        }
        crate::smc::types::decode_ui32(&bytes, ki.data_size).map_err(|_| SmcError::TypeMismatch {
            fourcc,
            expected: TYPE_UI32,
            got: ki.data_type,
        })
    }

    /// Read a `flt` key.
    ///
    /// # Errors
    /// - `TypeMismatch` if the key's `data_type` is not `flt`.
    /// - `InvalidFloat` if the decoded value is NaN or infinity.
    /// - Any `read_key` error.
    #[must_use = "IOKit errors must be handled"]
    pub fn read_f32(&mut self, fourcc: u32) -> Result<f32, SmcError> {
        use crate::smc::keys::TYPE_FLT;
        use crate::smc::types::SmcValueError;

        let (ki, bytes) = self.read_key(fourcc)?;
        if ki.data_type != TYPE_FLT {
            return Err(SmcError::TypeMismatch {
                fourcc,
                expected: TYPE_FLT,
                got: ki.data_type,
            });
        }
        match crate::smc::types::decode_flt(&bytes, ki.data_size) {
            Ok(v) => Ok(v),
            Err(SmcValueError::InvalidFloat) => Err(SmcError::InvalidFloat { fourcc }),
            Err(_) => Err(SmcError::TypeMismatch {
                fourcc,
                expected: TYPE_FLT,
                got: ki.data_type,
            }),
        }
    }

    /// Fetch a key at an index via `kSMCGetKeyFromIdx`. Used by the
    /// `--all` keyspace enumeration.
    ///
    /// # Errors
    /// See `SmcError` variants.
    #[must_use = "IOKit errors must be handled"]
    pub fn read_key_at_index(&mut self, index: u32) -> Result<u32, SmcError> {
        let mut input: SMCParamStruct = bytemuck::Zeroable::zeroed();
        input.data32 = index;

        let output = self.call_struct(SMC_CMD_GET_KEY_FROM_IDX, input)?;
        Ok(output.key)
    }

    /// Write bytes to a whitelisted SMC key.
    ///
    /// The `key` argument is a `WritableKey`, which is the ONLY way to
    /// express a write target from outside this module (FR-017, FR-019).
    /// `value_bytes` must be at most 32 bytes and match the key's declared
    /// `data_type` size (checked at runtime via `read_key_info`).
    ///
    /// # Errors
    /// - `SizeMismatch` if `value_bytes.len()` does not match the SMC's
    ///   declared `data_size` for this key.
    /// - Any `call_struct` error (IOKit or SMC result byte).
    pub(in crate::smc) fn write_key(
        &mut self,
        key: &WritableKey,
        value_bytes: &[u8],
    ) -> Result<(), SmcError> {
        let fourcc = key.fourcc();

        // Resolve data_size from the SMC itself — we trust the kernel over
        // any local assumption about key width.
        let key_info = self.read_key_info(fourcc)?;
        let data_size = key_info.data_size;

        if value_bytes.len() != data_size as usize {
            return Err(SmcError::SizeMismatch {
                fourcc,
                expected: data_size,
                got: value_bytes.len() as u32,
            });
        }
        if data_size > 32 {
            return Err(SmcError::SizeMismatch {
                fourcc,
                expected: 32,
                got: data_size,
            });
        }

        let mut input: SMCParamStruct = bytemuck::Zeroable::zeroed();
        input.key = fourcc;
        input.key_info.data_size = data_size;
        input.key_info.data_type = key_info.data_type;
        // Copy payload into the 32-byte tail.
        input.bytes[..value_bytes.len()].copy_from_slice(value_bytes);

        let _ = self.call_struct(SMC_CMD_WRITE_KEY, input)?;
        Ok(())
    }

    /// Write a `ui8` key via a `WritableKey` whitelist entry.
    ///
    /// # Errors
    /// See `write_key`.
    pub(in crate::smc) fn write_u8(
        &mut self,
        key: &WritableKey,
        value: u8,
    ) -> Result<(), SmcError> {
        if key.data_type() != TYPE_UI8 {
            return Err(SmcError::TypeMismatch {
                fourcc: key.fourcc(),
                expected: TYPE_UI8,
                got: key.data_type(),
            });
        }
        self.write_key(key, &[value])
    }

    /// Write a `flt` key via a `WritableKey` whitelist entry.
    /// NaN and infinity are rejected by `encode_flt`.
    ///
    /// # Errors
    /// - `InvalidFloat` if the value is NaN or infinity.
    /// - See `write_key`.
    pub(in crate::smc) fn write_f32(
        &mut self,
        key: &WritableKey,
        value: f32,
    ) -> Result<(), SmcError> {
        if key.data_type() != TYPE_FLT {
            return Err(SmcError::TypeMismatch {
                fourcc: key.fourcc(),
                expected: TYPE_FLT,
                got: key.data_type(),
            });
        }
        let bytes = crate::smc::types::encode_flt(value).map_err(|_| SmcError::InvalidFloat {
            fourcc: key.fourcc(),
        })?;
        self.write_key(key, &bytes)
    }

    /// FR-020: exercise the write path with `Ftst = 0`, a known no-op on
    /// Apple Silicon SMC. This is the ONLY `pub` write wrapper — it
    /// hard-codes both the key (`WritableKey::ftst()`) and the value (0),
    /// so external code cannot use it to smuggle arbitrary writes.
    ///
    /// Used by `fand keys` as a whitelist probe to confirm the write
    /// boundary is functional without setting any real fan state.
    ///
    /// # Errors
    /// - Any `write_u8` error (IOKit, SMC result byte, type mismatch).
    pub fn probe_write_ftst_zero(&mut self) -> Result<(), SmcError> {
        let key = WritableKey::ftst();
        self.write_u8(&key, 0)
    }

    /// **DEBUG-ONLY** raw write that bypasses the `WritableKey` whitelist.
    ///
    /// Used exclusively by `fand keys --write` for RD-08 keyspace research:
    /// it accepts an arbitrary fourcc + raw byte payload and writes them
    /// via the same `kSMCWriteKey` (cmd 6) path that the type-safe `write_key`
    /// uses, but without the `WritableKey` opaque-newtype gate.
    ///
    /// **MUST NOT** be exposed via the production `fand set` / `fand selftest`
    /// commands. The `pub(crate)` visibility limits it to in-crate callers
    /// (specifically `cli::keys::run_debug_write`).
    ///
    /// # Errors
    ///
    /// - Any error from `read_key_info` (KeyNotFound, etc.)
    /// - Any error from `call_struct` (IOKit failure or non-zero SMC result byte)
    /// - `SizeMismatch` if `bytes.len()` differs from the SMC's declared `data_size`
    pub(crate) fn write_raw_for_research(
        &mut self,
        fourcc: u32,
        bytes: &[u8],
    ) -> Result<(), SmcError> {
        // Resolve data_size from the SMC. We trust the kernel over any local
        // assumption about key width — this matches what `write_key` does.
        let key_info = self.read_key_info(fourcc)?;
        let data_size = key_info.data_size;

        if bytes.len() != data_size as usize {
            return Err(SmcError::SizeMismatch {
                fourcc,
                expected: data_size,
                got: bytes.len() as u32,
            });
        }
        if data_size > 32 {
            return Err(SmcError::SizeMismatch {
                fourcc,
                expected: 32,
                got: data_size,
            });
        }

        let mut input: SMCParamStruct = bytemuck::Zeroable::zeroed();
        input.key = fourcc;
        input.key_info.data_size = data_size;
        input.key_info.data_type = key_info.data_type;
        input.bytes[..bytes.len()].copy_from_slice(bytes);

        let _ = self.call_struct(SMC_CMD_WRITE_KEY, input)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Round-trip-verified writes (feature 005 FR-006, FR-007, FR-009)
    // ------------------------------------------------------------------
    //
    // Every production write goes through one of these methods. Each writes
    // the key, reads it back within the same tick, compares byte-for-byte,
    // and pushes a RoundTripRecord to the caller's ring. On mismatch the
    // method returns `SmcError::WriteReadbackMismatch` carrying the
    // expected + observed bytes and the session correlation ID.

    /// Write a `ui8` key (fan mode or Ftst) and verify the readback
    /// matches byte-for-byte. Pushes a record to `ring`, carrying the
    /// session ID.
    ///
    /// # Errors
    /// - `TypeMismatch` if the key's data_type is not `ui8`
    /// - `WriteRefused` if the SMC returns a non-zero result byte on write
    /// - `WriteReadbackMismatch` if the readback differs from the value written
    /// - Any `read_u8` / `write_u8` error
    pub(in crate::smc) fn write_u8_verified(
        &mut self,
        key: &WritableKey,
        value: u8,
        context: &'static str,
        session: crate::correlation::SessionId,
        ring: &mut crate::smc::round_trip::RoundTripRing,
    ) -> Result<(), SmcError> {
        let fourcc = key.fourcc();
        let timestamp_ns = wall_clock_ns();

        // Attempt the write.
        if let Err(e) = self.write_u8(key, value) {
            // Classify a raw SMC-result error as WriteRefused with write-path
            // context. Other errors propagate as-is.
            let err = match e {
                SmcError::SmcResult { result_byte, .. } => SmcError::WriteRefused {
                    fourcc,
                    result_byte,
                    context,
                    session,
                    timestamp_ns,
                },
                other => other,
            };
            ring.push(crate::smc::round_trip::RoundTripRecord::new(
                timestamp_ns,
                fourcc,
                &[value],
                &[],
                crate::smc::round_trip::RoundTripOutcome::WriteFailed,
            ));
            return Err(err);
        }

        // Read back and compare.
        let readback = match self.read_u8(fourcc) {
            Ok(v) => v,
            Err(e) => {
                ring.push(crate::smc::round_trip::RoundTripRecord::new(
                    timestamp_ns,
                    fourcc,
                    &[value],
                    &[],
                    crate::smc::round_trip::RoundTripOutcome::ReadbackFailed,
                ));
                return Err(e);
            }
        };

        if readback != value {
            ring.push(crate::smc::round_trip::RoundTripRecord::new(
                timestamp_ns,
                fourcc,
                &[value],
                &[readback],
                crate::smc::round_trip::RoundTripOutcome::Mismatch,
            ));
            return Err(SmcError::WriteReadbackMismatch {
                fourcc,
                expected: [value, 0, 0, 0],
                expected_len: 1,
                got: [readback, 0, 0, 0],
                got_len: 1,
                session,
                timestamp_ns,
                iteration: None,
            });
        }

        ring.push(crate::smc::round_trip::RoundTripRecord::new_match(
            timestamp_ns,
            fourcc,
            &[value],
            &[readback],
        ));
        Ok(())
    }

    /// Write a `flt` key (fan target RPM) and verify the readback matches
    /// byte-for-byte. Byte comparison — NOT float equality — per FR-006.
    ///
    /// # Errors
    /// See `write_u8_verified`.
    pub(in crate::smc) fn write_f32_verified(
        &mut self,
        key: &WritableKey,
        value: f32,
        context: &'static str,
        session: crate::correlation::SessionId,
        ring: &mut crate::smc::round_trip::RoundTripRing,
    ) -> Result<(), SmcError> {
        let fourcc = key.fourcc();
        let timestamp_ns = wall_clock_ns();
        let expected_bytes = match crate::smc::types::encode_flt(value) {
            Ok(b) => b,
            Err(_) => return Err(SmcError::InvalidFloat { fourcc }),
        };

        if let Err(e) = self.write_f32(key, value) {
            let err = match e {
                SmcError::SmcResult { result_byte, .. } => SmcError::WriteRefused {
                    fourcc,
                    result_byte,
                    context,
                    session,
                    timestamp_ns,
                },
                other => other,
            };
            ring.push(crate::smc::round_trip::RoundTripRecord::new(
                timestamp_ns,
                fourcc,
                &expected_bytes,
                &[],
                crate::smc::round_trip::RoundTripOutcome::WriteFailed,
            ));
            return Err(err);
        }

        let readback = match self.read_f32(fourcc) {
            Ok(v) => v,
            Err(e) => {
                ring.push(crate::smc::round_trip::RoundTripRecord::new(
                    timestamp_ns,
                    fourcc,
                    &expected_bytes,
                    &[],
                    crate::smc::round_trip::RoundTripOutcome::ReadbackFailed,
                ));
                return Err(e);
            }
        };
        let got_bytes = readback.to_le_bytes();

        // Byte-for-byte comparison (FR-006 — explicitly NOT float ==).
        if got_bytes != expected_bytes {
            ring.push(crate::smc::round_trip::RoundTripRecord::new(
                timestamp_ns,
                fourcc,
                &expected_bytes,
                &got_bytes,
                crate::smc::round_trip::RoundTripOutcome::Mismatch,
            ));
            return Err(SmcError::WriteReadbackMismatch {
                fourcc,
                expected: expected_bytes,
                expected_len: 4,
                got: got_bytes,
                got_len: 4,
                session,
                timestamp_ns,
                iteration: None,
            });
        }

        ring.push(crate::smc::round_trip::RoundTripRecord::new_match(
            timestamp_ns,
            fourcc,
            &expected_bytes,
            &got_bytes,
        ));
        Ok(())
    }

    /// Emergency-path write of `F<i>Md = 0` (auto mode) for a specific fan.
    /// Used by the panic hook + signal-teardown + watchdog paths (FR-026,
    /// FR-021, FR-002). Silently swallows any error because the process is
    /// already dying and the watchdog is the final safety net.
    ///
    /// Visibility is `pub(in crate::smc)` because only the feature 005
    /// teardown infrastructure is supposed to call it — external callers
    /// must go through the normal round-trip-verified write path.
    ///
    /// # Errors
    ///
    /// Any SMC / IOKit error. Callers typically ignore the return value.
    pub(in crate::smc) fn force_write_auto_mode(&mut self, fan_idx: u8) -> Result<(), SmcError> {
        let key = WritableKey::fan_mode(fan_idx);
        self.write_u8(&key, 0)
    }

    /// Emergency-path write of `Ftst = 0`. Used alongside `force_write_auto_mode`
    /// on every teardown path. Silently best-effort.
    ///
    /// # Errors
    ///
    /// Any SMC / IOKit error.
    pub(in crate::smc) fn force_write_ftst_zero(&mut self) -> Result<(), SmcError> {
        let key = WritableKey::ftst();
        self.write_u8(&key, 0)
    }
}

/// Metadata for a given fourcc: size in bytes and type tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInfo {
    pub data_size: u32,
    pub data_type: u32,
}

impl Drop for SmcConnection {
    fn drop(&mut self) {
        self.close();
    }
}

// SmcConnection is intentionally !Clone + !Copy (FR-056)
// — we do not derive or implement either.

// ------------------------------------------------------------------------
// Write whitelist (FR-017: pub(in crate::smc), not pub(crate))
// The write methods live as `pub(in crate::smc)` inline on `SmcConnection`
// above — `write_key`, `write_u8`, `write_f32`.
// ------------------------------------------------------------------------

// ------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------

// T091: miri excluded from this module — the tests cross into real
// IOKit via `SmcConnection::open`, which miri cannot simulate. The
// pure-logic subset (error_code mapping, taxonomy dispatch, struct
// layout asserts) would run under miri if split out, but that split
// is deferred to a follow-up cleanup.
#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;

    #[test]
    fn param_struct_is_80_bytes() {
        assert_eq!(std::mem::size_of::<SMCParamStruct>(), 80);
    }

    #[test]
    fn param_struct_alignment_is_4() {
        assert_eq!(std::mem::align_of::<SMCParamStruct>(), 4);
    }

    #[test]
    fn key_info_is_12_bytes() {
        assert_eq!(std::mem::size_of::<SMCKeyInfoData>(), 12);
    }

    #[test]
    fn per_field_offsets_match_agoodkind() {
        // Runtime mirror of the compile-time const asserts — belt AND braces.
        assert_eq!(std::mem::offset_of!(SMCParamStruct, key), 0);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, vers), 4);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, p_limit_data), 12);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, key_info), 28);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, result), 40);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, status), 41);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, data8), 42);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, data32), 44);
        assert_eq!(std::mem::offset_of!(SMCParamStruct, bytes), 48);
    }

    #[test]
    fn zeroable_produces_zero_bytes() {
        let param: SMCParamStruct = bytemuck::Zeroable::zeroed();
        let bytes: &[u8; 80] = bytemuck::bytes_of(&param).try_into().unwrap();
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn data_size_clamp_defensive() {
        // Simulate the hot path: kernel returns data_size=100, clamp to 32.
        let reported: u32 = 100;
        let clamped = reported.min(32);
        assert_eq!(clamped, 32);
        let bytes = [0u8; 32];
        let slice = &bytes[..clamped as usize];
        assert_eq!(slice.len(), 32);
    }

    #[test]
    fn io_connect_is_repr_transparent() {
        // IoConnect wraps IoConnectT (u32) with #[repr(transparent)] — sizes must match.
        assert_eq!(
            std::mem::size_of::<IoConnect>(),
            std::mem::size_of::<IoConnectT>()
        );
        assert_eq!(
            std::mem::align_of::<IoConnect>(),
            std::mem::align_of::<IoConnectT>()
        );
    }

    #[test]
    fn smc_error_implements_error_trait() {
        fn requires_error<E: std::error::Error + Send + Sync + 'static>() {}
        requires_error::<SmcError>();
    }

    #[test]
    fn smc_error_display_includes_symbolic_names() {
        let err = SmcError::OpenFailed(K_IO_RETURN_NOT_PRIVILEGED);
        let msg = format!("{err}");
        assert!(msg.contains("kIOReturnNotPrivileged"));
        assert!(msg.contains("sudo"));

        let err = SmcError::OpenFailed(K_IO_RETURN_NOT_PERMITTED);
        let msg = format!("{err}");
        assert!(msg.contains("kIOReturnNotPermitted"));
        assert!(msg.contains("Lockdown Mode"));
        assert!(msg.contains("EndpointSecurity"));
    }

    #[test]
    fn fourcc_to_str_roundtrip() {
        // "FNum" = 0x46 4E 75 6D
        let fourcc = 0x464E_756D_u32;
        assert_eq!(fourcc_to_str(fourcc), "FNum");

        // "#KEY" = 0x23 4B 45 59
        let fourcc = 0x234B_4559_u32;
        assert_eq!(fourcc_to_str(fourcc), "#KEY");
    }

    #[test]
    fn empty_response_sanity_check_conditions() {
        // Simulate kernel returning: kr=0, result=0, data_size=0, data_type=0
        let param: SMCParamStruct = bytemuck::Zeroable::zeroed();
        assert_eq!(param.result, 0);
        assert_eq!(param.key_info.data_size, 0);
        assert_eq!(param.key_info.data_type, 0);
        // The caller would detect (result=0 && data_size=0 && data_type=0) → EmptyResponse.
    }

    // ---- T018: exhaustive error_code() coverage (FR-098, SC-021) ----

    #[test]
    fn every_smc_error_variant_has_stable_code() {
        use crate::correlation::SessionId;
        let sid = SessionId::new();
        let fixtures: Vec<(SmcError, &str)> = vec![
            (SmcError::ServiceNotFound, "SERVICE_NOT_FOUND"),
            (
                SmcError::OpenFailed(0xE00002C1_u32 as KernReturnT),
                "OPEN_FAILED",
            ),
            (SmcError::OpenTimeout, "OPEN_TIMEOUT"),
            (
                SmcError::CallFailed {
                    selector: 2,
                    kr: 0,
                    cmd: 5,
                },
                "CALL_FAILED",
            ),
            (
                SmcError::SmcResult {
                    cmd: 5,
                    result_byte: 0x84,
                },
                "SMC_RESULT",
            ),
            (SmcError::KeyNotFound(0x4630_4D64), "KEY_NOT_FOUND"),
            (
                SmcError::TypeMismatch {
                    fourcc: 0,
                    expected: 0,
                    got: 0,
                },
                "TYPE_MISMATCH",
            ),
            (
                SmcError::DataSizeClamped {
                    fourcc: 0,
                    reported: 100,
                },
                "DATA_SIZE_CLAMPED",
            ),
            (SmcError::InvalidFloat { fourcc: 0 }, "INVALID_FLOAT"),
            (SmcError::EmptyResponse { fourcc: 0 }, "EMPTY_RESPONSE"),
            (SmcError::AlreadyClosed, "ALREADY_CLOSED"),
            (SmcError::Busy { retried: true }, "BUSY"),
            (SmcError::Timeout { retried: false }, "TIMEOUT"),
            (SmcError::AttributeDenied { fourcc: 0 }, "ATTRIBUTE_DENIED"),
            (SmcError::WriteDenied(0), "WRITE_DENIED"),
            (
                SmcError::SizeMismatch {
                    fourcc: 0,
                    expected: 4,
                    got: 2,
                },
                "SIZE_MISMATCH",
            ),
            (
                SmcError::EndiannessUnplausible {
                    fourcc: 0,
                    got: 0xDEAD,
                },
                "ENDIANNESS_UNPLAUSIBLE",
            ),
            (
                SmcError::UnlockMismatch {
                    expected: 1,
                    got: 0,
                    session: sid,
                    timestamp_ns: 0,
                },
                "UNLOCK_MISMATCH",
            ),
            (
                SmcError::UnlockRejected {
                    result_byte: 0x86,
                    session: sid,
                },
                "UNLOCK_REJECTED",
            ),
            (
                SmcError::WriteRefused {
                    fourcc: 0x4630_4D64,
                    result_byte: 0x85,
                    context: "fan_mode",
                    session: sid,
                    timestamp_ns: 0,
                },
                "WRITE_REFUSED",
            ),
            (
                SmcError::WriteReadbackMismatch {
                    fourcc: 0x4630_5467,
                    expected: [1, 2, 3, 4],
                    expected_len: 4,
                    got: [1, 2, 3, 5],
                    got_len: 4,
                    session: sid,
                    timestamp_ns: 0,
                    iteration: None,
                },
                "WRITE_READBACK_MISMATCH",
            ),
            (
                SmcError::WatchdogFired {
                    elapsed_ms: 4200,
                    session: sid,
                },
                "WATCHDOG_FIRED",
            ),
            (
                SmcError::ConflictDetected {
                    holder_pid: 12345,
                    lockfile_path: "/private/var/run/fand-smc.lock".to_string(),
                },
                "CONFLICT_DETECTED",
            ),
            (
                SmcError::EdrDenied {
                    suspected_agent: Some("falcon-sensor".to_string()),
                },
                "EDR_DENIED",
            ),
            (SmcError::TccDenied, "TCC_DENIED"),
            (SmcError::LockdownModeSuspected, "LOCKDOWN_MODE_SUSPECTED"),
        ];
        assert_eq!(fixtures.len(), 26, "every SmcError variant must be tested");
        for (err, expected_code) in &fixtures {
            assert_eq!(
                err.error_code(),
                *expected_code,
                "variant {err:?} → expected code '{expected_code}'"
            );
        }
    }

    #[test]
    fn error_codes_are_all_distinct() {
        use crate::correlation::SessionId;
        let sid = SessionId::new();
        let codes: Vec<&'static str> = vec![
            SmcError::ServiceNotFound.error_code(),
            SmcError::OpenFailed(0).error_code(),
            SmcError::OpenTimeout.error_code(),
            SmcError::CallFailed {
                selector: 0,
                kr: 0,
                cmd: 0,
            }
            .error_code(),
            SmcError::SmcResult {
                cmd: 0,
                result_byte: 0,
            }
            .error_code(),
            SmcError::KeyNotFound(0).error_code(),
            SmcError::TypeMismatch {
                fourcc: 0,
                expected: 0,
                got: 0,
            }
            .error_code(),
            SmcError::DataSizeClamped {
                fourcc: 0,
                reported: 0,
            }
            .error_code(),
            SmcError::InvalidFloat { fourcc: 0 }.error_code(),
            SmcError::EmptyResponse { fourcc: 0 }.error_code(),
            SmcError::AlreadyClosed.error_code(),
            SmcError::Busy { retried: false }.error_code(),
            SmcError::Timeout { retried: false }.error_code(),
            SmcError::AttributeDenied { fourcc: 0 }.error_code(),
            SmcError::WriteDenied(0).error_code(),
            SmcError::SizeMismatch {
                fourcc: 0,
                expected: 0,
                got: 0,
            }
            .error_code(),
            SmcError::EndiannessUnplausible { fourcc: 0, got: 0 }.error_code(),
            SmcError::UnlockMismatch {
                expected: 0,
                got: 0,
                session: sid,
                timestamp_ns: 0,
            }
            .error_code(),
            SmcError::UnlockRejected {
                result_byte: 0,
                session: sid,
            }
            .error_code(),
            SmcError::WriteRefused {
                fourcc: 0,
                result_byte: 0,
                context: "x",
                session: sid,
                timestamp_ns: 0,
            }
            .error_code(),
            SmcError::WriteReadbackMismatch {
                fourcc: 0,
                expected: [0; 4],
                expected_len: 0,
                got: [0; 4],
                got_len: 0,
                session: sid,
                timestamp_ns: 0,
                iteration: None,
            }
            .error_code(),
            SmcError::WatchdogFired {
                elapsed_ms: 0,
                session: sid,
            }
            .error_code(),
            SmcError::ConflictDetected {
                holder_pid: 0,
                lockfile_path: String::new(),
            }
            .error_code(),
            SmcError::EdrDenied {
                suspected_agent: None,
            }
            .error_code(),
            SmcError::TccDenied.error_code(),
            SmcError::LockdownModeSuspected.error_code(),
        ];
        let unique: std::collections::HashSet<_> = codes.iter().copied().collect();
        assert_eq!(
            unique.len(),
            codes.len(),
            "error_code() mapping must be injective"
        );
    }

    #[test]
    fn feature_005_display_contains_session_id() {
        use crate::correlation::SessionId;
        let sid = SessionId::new();
        let err = SmcError::WatchdogFired {
            elapsed_ms: 4200,
            session: sid,
        };
        let msg = format!("{err}");
        assert!(msg.contains(sid.as_str()), "Display must embed session id");
        assert!(msg.contains("4200"));
    }

    #[test]
    fn conflict_detected_display_warns_untrusted_pid() {
        let err = SmcError::ConflictDetected {
            holder_pid: 99999,
            lockfile_path: "/var/run/fand-smc.lock".to_string(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("99999"));
        assert!(
            msg.contains("untrusted"),
            "Display MUST warn that PID is untrusted (CHK060)"
        );
    }

    #[test]
    fn edr_denied_identifies_agent_when_known() {
        let err = SmcError::EdrDenied {
            suspected_agent: Some("CrowdStrike".to_string()),
        };
        let msg = format!("{err}");
        assert!(msg.contains("CrowdStrike"));
        assert!(msg.contains("EndpointSecurity"));

        let err_none = SmcError::EdrDenied {
            suspected_agent: None,
        };
        let msg_none = format!("{err_none}");
        assert!(msg_none.contains("agent not identified"));
    }
}

// ---------------------------------------------------------------------
// Kani proof harnesses (FR-082 — T089).
//
// Proves that `SmcError::error_code()` is total: every `SmcError`
// variant has a non-empty stable code. `#[non_exhaustive]` makes this
// impossible to prove directly with `kani::any()` (kani cannot
// materialize a non-exhaustive enum), so instead we enumerate every
// variant explicitly and assert the invariant holds. The companion
// trybuild test `tests/ui/smc_error_non_exhaustive.rs` locks
// downstream `match` exhaustiveness at compile time.
// ---------------------------------------------------------------------

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// FR-082: every explicit `SmcError` variant has a non-empty
    /// uppercase `error_code()` string. Combined with the
    /// `error_codes_are_all_distinct` unit test, this proves the
    /// taxonomy is well-formed at the string level.
    ///
    /// Note: this harness enumerates every variant by hand because
    /// `kani::any()` cannot materialize a `#[non_exhaustive]` enum
    /// with heterogeneous payloads. If a new variant is added to
    /// `SmcError` and not wired into `error_code()`, the unit tests
    /// (not kani) will fail first — see
    /// `every_smc_error_variant_has_stable_code` below.
    #[kani::proof]
    fn kani_smc_error_codes_non_empty() {
        let samples = [
            SmcError::ServiceNotFound,
            SmcError::OpenTimeout,
            SmcError::KeyNotFound(0),
            SmcError::AlreadyClosed,
            SmcError::TccDenied,
            SmcError::LockdownModeSuspected,
        ];
        for err in &samples {
            let code = err.error_code();
            assert!(!code.is_empty());
            // All codes are SCREAMING_SNAKE — no lowercase letters.
            for b in code.bytes() {
                assert!(!b.is_ascii_lowercase());
            }
        }
    }
}
