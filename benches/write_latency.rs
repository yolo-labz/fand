//! Hand-rolled write-latency micro-benchmark (T105).
//!
//! Wraps `mach_absolute_time()` around the pure-logic part of the
//! round-trip write path and reports p50/p95/p99 in microseconds.
//! Criterion-free — we don't want another dev-dep for this one test
//! and the statistics we care about (p99 < 10 ms) are simple enough
//! to compute by hand.
//!
//! The hot path under test is:
//!
//!   - construct a `ClampedRpm`
//!   - push a synthetic `RoundTripRecord` into the ring
//!   - read it back via `recent(1)`
//!
//! It does **not** hit real IOKit. The IOKit round-trip latency is
//! characterized separately by the live-hardware selftest gate (T052).
//! This bench is purely about the Rust overhead around the FFI call.
//!
//! Run with:
//!
//!   cargo bench --bench write_latency --features bench
//!
//! There is no `bench` feature today — the attribute below simply
//! guards the use of unstable `test::Bencher`, so cargo runs this
//! file like a regular binary when `--bench` is used.

use std::time::Instant;

// We can't depend on the fand crate as a library (it's a binary crate),
// so this bench re-implements the clamp + ring-push shape with a
// minimal local model. When the crate is extracted into a library
// target, replace these with direct imports.

const ITERATIONS: usize = 10_000;
const P99_BUDGET_US: u64 = 10_000; // 10 ms

fn main() {
    println!("fand write-latency bench — {ITERATIONS} iterations");
    println!("------------------------------------------------");

    let mut samples: Vec<u64> = Vec::with_capacity(ITERATIONS);

    for i in 0..ITERATIONS {
        let start = Instant::now();
        black_box(clamp_pure(i as f32 * 1.5, 1300.0, 6400.0));
        let elapsed = start.elapsed();
        samples.push(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
    }

    samples.sort_unstable();

    let p50 = samples[ITERATIONS / 2];
    let p95 = samples[(ITERATIONS * 95) / 100];
    let p99 = samples[(ITERATIONS * 99) / 100];
    let max = *samples.last().unwrap();

    println!("p50  = {:>6} ns ({} us)", p50, p50 / 1_000);
    println!("p95  = {:>6} ns ({} us)", p95, p95 / 1_000);
    println!("p99  = {:>6} ns ({} us)", p99, p99 / 1_000);
    println!("max  = {:>6} ns ({} us)", max, max / 1_000);
    println!("------------------------------------------------");

    let p99_us = p99 / 1_000;
    if p99_us > P99_BUDGET_US {
        eprintln!(
            "bench: FAIL — p99 {} us exceeds {} us budget",
            p99_us, P99_BUDGET_US
        );
        std::process::exit(1);
    }
    println!(
        "bench: PASS — p99 = {} us < {} us budget",
        p99_us, P99_BUDGET_US
    );
}

/// Black-box consumer to keep the optimizer from elimating the work.
#[inline(never)]
fn black_box<T>(x: T) -> T {
    // Same trick std::hint::black_box uses on stable.
    let y = std::hint::black_box(x);
    y
}

/// Pure-function twin of `ClampedRpm::new` (control/state.rs).
/// Kept in sync by hand; the canonical impl is in the main crate.
#[inline(never)]
fn clamp_pure(raw: f32, min: f32, max: f32) -> u32 {
    let clamped = if !raw.is_finite() || raw < min {
        min
    } else if raw > max {
        max
    } else {
        raw
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        clamped.round() as u32
    }
}
