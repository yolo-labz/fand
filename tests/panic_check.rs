use fand::control::panic::{check, PanicAction, PanicState};
use std::time::{Duration, Instant};

#[test]
fn single_tick_no_trigger_debounce() {
    let mut s = PanicState::new();
    let now = Instant::now();
    assert_eq!(check(&mut s, 96.0, 95.0, 10, now), PanicAction::Passthrough);
    assert!(!s.latched);
}

#[test]
fn two_consecutive_ticks_triggers() {
    let mut s = PanicState::new();
    let now = Instant::now();
    check(&mut s, 96.0, 95.0, 10, now);
    assert_eq!(check(&mut s, 96.0, 95.0, 10, now), PanicAction::ForceFxMx);
    assert!(s.latched);
}

#[test]
fn hold_prevents_early_exit() {
    let mut s = PanicState::new();
    let now = Instant::now();
    check(&mut s, 96.0, 95.0, 10, now);
    check(&mut s, 96.0, 95.0, 10, now);

    let too_early = now + Duration::from_secs(5);
    assert_eq!(
        check(&mut s, 90.0, 95.0, 10, too_early),
        PanicAction::ForceFxMx
    );
}

#[test]
fn exits_after_hold_period() {
    let mut s = PanicState::new();
    let now = Instant::now();
    check(&mut s, 96.0, 95.0, 10, now);
    check(&mut s, 96.0, 95.0, 10, now);

    let after_hold = now + Duration::from_secs(11);
    assert_eq!(
        check(&mut s, 90.0, 95.0, 10, after_hold),
        PanicAction::Passthrough
    );
    assert!(!s.latched);
}

#[test]
fn panic_preserves_across_bumpless_reinit() {
    let mut s = PanicState::new();
    let now = Instant::now();
    check(&mut s, 96.0, 95.0, 10, now);
    check(&mut s, 96.0, 95.0, 10, now);
    assert!(s.latched);

    // FR-049: re-evaluate with current temp, don't reset
    assert_eq!(check(&mut s, 96.0, 95.0, 10, now), PanicAction::ForceFxMx);
}

#[test]
fn intermittent_spike_does_not_trigger() {
    let mut s = PanicState::new();
    let now = Instant::now();
    check(&mut s, 96.0, 95.0, 10, now); // above
    check(&mut s, 94.0, 95.0, 10, now); // below - resets counter
    assert_eq!(check(&mut s, 96.0, 95.0, 10, now), PanicAction::Passthrough);
}
