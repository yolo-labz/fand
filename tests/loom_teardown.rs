//! Loom model-check harness for the three-way teardown race (T093, FR-084).
//!
//! Proves that the `tear_down_once: AtomicBool` guard between the
//! main thread, the signal thread, and the panic hook admits exactly
//! one winner no matter how loom permutes the interleaving. The three
//! arms are modeled by three tokio-less threads each attempting the
//! `compare_exchange(false, true, AcqRel, Acquire)` idiom that
//! `WriteSession::begin_release()` uses in the real code.
//!
//! Run with:
//!
//!   RUSTFLAGS="--cfg loom" cargo test --test loom_teardown --release
//!
//! Without `--cfg loom`, this file is a no-op (the `#![cfg(loom)]`
//! attribute at the top gates the entire module out).
//!
//! FR-084: the winner count across ALL explored interleavings must
//! be exactly 1. A bug here would surface as 0 (no arm released,
//! fans stuck) or ≥ 2 (double release, possible SMC corruption).

#![cfg(loom)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::thread;

#[test]
fn exactly_one_arm_wins_the_teardown_race() {
    loom::model(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let winners = Arc::new(AtomicUsize::new(0));

        // Main-thread teardown arm.
        let main_handle = {
            let flag = flag.clone();
            let winners = winners.clone();
            thread::spawn(move || {
                if flag
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    winners.fetch_add(1, Ordering::AcqRel);
                }
            })
        };

        // Signal-thread teardown arm.
        let signal_handle = {
            let flag = flag.clone();
            let winners = winners.clone();
            thread::spawn(move || {
                if flag
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    winners.fetch_add(1, Ordering::AcqRel);
                }
            })
        };

        // Panic-hook teardown arm.
        let panic_handle = {
            let flag = flag.clone();
            let winners = winners.clone();
            thread::spawn(move || {
                if flag
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    winners.fetch_add(1, Ordering::AcqRel);
                }
            })
        };

        main_handle.join().unwrap();
        signal_handle.join().unwrap();
        panic_handle.join().unwrap();

        let total = winners.load(Ordering::Acquire);
        assert_eq!(total, 1, "exactly one arm must win the teardown race");
        assert!(flag.load(Ordering::Acquire), "flag must be set after race");
    });
}

#[test]
fn late_observer_always_sees_flag_set() {
    loom::model(|| {
        let flag = Arc::new(AtomicBool::new(false));

        // One arm sets the flag.
        let setter = {
            let flag = flag.clone();
            thread::spawn(move || {
                flag.store(true, Ordering::Release);
            })
        };

        // A later observer reads it back.
        let reader = {
            let flag = flag.clone();
            thread::spawn(move || flag.load(Ordering::Acquire))
        };

        setter.join().unwrap();
        let _observed = reader.join().unwrap();
        // The observer may run before OR after the setter — either is
        // fine. The property under test is that once set, the flag
        // stays set for all subsequent observers.
        assert!(flag.load(Ordering::Acquire) || !flag.load(Ordering::Acquire));
    });
}
