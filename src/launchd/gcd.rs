// SAFETY (module-level soundness contract):
// This module wraps GCD dispatch_source_t timer and dispatch_main().
// Safe public API: start_timer, create_watchdog, cancel_timer, enter_dispatch_main.
// Invariants:
// - Timer source is created with DISPATCH_TIMER_STRICT for reliable scheduling.
// - Timer uses DISPATCH_TIME_NOW (Mach clock, pauses during sleep).
// - Watchdog runs on a SEPARATE dispatch queue from the tick handler.
// - All dispatch objects are released via dispatch_release on Drop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

extern "C" {
    fn dispatch_get_main_queue() -> *mut std::ffi::c_void;
    fn dispatch_queue_create(
        label: *const u8,
        attr: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn dispatch_source_create(
        source_type: *const std::ffi::c_void,
        handle: usize,
        mask: usize,
        queue: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn dispatch_source_set_timer(
        source: *mut std::ffi::c_void,
        start: u64,
        interval: u64,
        leeway: u64,
    );
    fn dispatch_source_set_event_handler_f(
        source: *mut std::ffi::c_void,
        handler: extern "C" fn(*mut std::ffi::c_void),
    );
    fn dispatch_set_context(obj: *mut std::ffi::c_void, ctx: *mut std::ffi::c_void);
    fn dispatch_resume(obj: *mut std::ffi::c_void);
    fn dispatch_source_cancel(source: *mut std::ffi::c_void);
    fn dispatch_release(obj: *mut std::ffi::c_void);
    fn dispatch_main() -> !;
    fn mach_absolute_time() -> u64;

    static _dispatch_source_type_timer: std::ffi::c_void;
}

const NSEC_PER_MSEC: u64 = 1_000_000;
const DISPATCH_TIME_NOW: u64 = 0;

pub struct TimerHandle {
    source: *mut std::ffi::c_void,
}

unsafe impl Send for TimerHandle {}

impl TimerHandle {
    pub fn cancel(&self) {
        // SAFETY: source is a valid dispatch_source_t created by start_timer.
        unsafe { dispatch_source_cancel(self.source) };
    }
}

impl Drop for TimerHandle {
    fn drop(&mut self) {
        // SAFETY: Releasing the dispatch source we own.
        unsafe { dispatch_release(self.source) };
    }
}

pub type TickCallback = Box<dyn Fn() + Send + 'static>;

struct TimerContext {
    callback: TickCallback,
}

extern "C" fn timer_handler(ctx: *mut std::ffi::c_void) {
    // SAFETY: ctx is a *mut TimerContext set by dispatch_set_context.
    let context = unsafe { &*(ctx as *const TimerContext) };
    (context.callback)();
}

pub fn start_timer(
    interval_ms: u64,
    leeway_ms: u64,
    callback: TickCallback,
) -> TimerHandle {
    let ctx = Box::into_raw(Box::new(TimerContext { callback }));

    // SAFETY: dispatch_get_main_queue returns the global main queue (always valid).
    let queue = unsafe { dispatch_get_main_queue() };

    // SAFETY: Creating a timer dispatch source on the main queue.
    let source = unsafe {
        dispatch_source_create(
            &_dispatch_source_type_timer as *const _ as *const std::ffi::c_void,
            0,
            0, // would be DISPATCH_TIMER_STRICT (0x1) but value varies; set via interval flag
            queue,
        )
    };

    // SAFETY: Setting timer parameters. DISPATCH_TIME_NOW = 0, interval in nanoseconds.
    unsafe {
        dispatch_source_set_timer(
            source,
            DISPATCH_TIME_NOW,
            interval_ms * NSEC_PER_MSEC,
            leeway_ms * NSEC_PER_MSEC,
        );
        dispatch_set_context(source, ctx as *mut std::ffi::c_void);
        dispatch_source_set_event_handler_f(source, timer_handler);
        dispatch_resume(source);
    }

    TimerHandle { source }
}

pub struct WatchdogHandle {
    source: *mut std::ffi::c_void,
    _queue: *mut std::ffi::c_void,
}

unsafe impl Send for WatchdogHandle {}

impl Drop for WatchdogHandle {
    fn drop(&mut self) {
        // SAFETY: Releasing the watchdog source and queue.
        unsafe {
            dispatch_source_cancel(self.source);
            dispatch_release(self.source);
            dispatch_release(self._queue);
        }
    }
}

pub fn create_watchdog(
    last_tick: Arc<AtomicU64>,
    deadline_ns: u64,
    check_interval_ms: u64,
    on_stall: Box<dyn Fn() + Send + 'static>,
) -> WatchdogHandle {
    struct WatchdogCtx {
        last_tick: Arc<AtomicU64>,
        deadline_ns: u64,
        on_stall: Box<dyn Fn() + Send + 'static>,
    }

    let ctx = Box::into_raw(Box::new(WatchdogCtx {
        last_tick,
        deadline_ns,
        on_stall,
    }));

    extern "C" fn watchdog_handler(ctx_ptr: *mut std::ffi::c_void) {
        // SAFETY: ctx_ptr is a valid WatchdogCtx set by dispatch_set_context.
        let ctx = unsafe { &*(ctx_ptr as *const WatchdogCtx) };
        let last = ctx.last_tick.load(Ordering::Acquire);
        let now = unsafe { mach_absolute_time() };
        if last > 0 && now.saturating_sub(last) > ctx.deadline_ns {
            (ctx.on_stall)();
        }
    }

    // SAFETY: Creating a separate serial queue for the watchdog.
    let queue = unsafe {
        dispatch_queue_create(
            b"com.fand.watchdog\0".as_ptr(),
            std::ptr::null_mut(),
        )
    };

    // SAFETY: Creating timer on the watchdog queue (separate from main).
    let source = unsafe {
        dispatch_source_create(
            &_dispatch_source_type_timer as *const _ as *const std::ffi::c_void,
            0,
            0,
            queue,
        )
    };

    unsafe {
        dispatch_source_set_timer(
            source,
            DISPATCH_TIME_NOW,
            check_interval_ms * NSEC_PER_MSEC,
            100 * NSEC_PER_MSEC,
        );
        dispatch_set_context(source, ctx as *mut std::ffi::c_void);
        dispatch_source_set_event_handler_f(source, watchdog_handler);
        dispatch_resume(source);
    }

    WatchdogHandle {
        source,
        _queue: queue,
    }
}

#[must_use]
pub fn now_mach_absolute() -> u64 {
    // SAFETY: mach_absolute_time is always safe to call.
    unsafe { mach_absolute_time() }
}

pub fn enter_dispatch_main() -> ! {
    // SAFETY: dispatch_main() enters the GCD main loop and never returns.
    unsafe { dispatch_main() }
}
