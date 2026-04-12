use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicAction {
    ForceFxMx,
    Passthrough,
}

#[derive(Debug, Clone)]
pub struct PanicState {
    pub latched: bool,
    pub consecutive_above: u8,
    pub entered_at: Option<Instant>,
}

impl PanicState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            latched: false,
            consecutive_above: 0,
            entered_at: None,
        }
    }
}

impl Default for PanicState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn check(
    state: &mut PanicState,
    raw_temp: f32,
    threshold: f32,
    hold_secs: u32,
    now: Instant,
) -> PanicAction {
    if raw_temp > threshold {
        state.consecutive_above = state.consecutive_above.saturating_add(1);
    } else {
        state.consecutive_above = 0;
    }

    if !state.latched && state.consecutive_above >= 2 {
        state.latched = true;
        state.entered_at = Some(now);
    }

    if state.latched {
        if raw_temp <= threshold {
            if let Some(entered) = state.entered_at {
                let elapsed = now.duration_since(entered).as_secs();
                #[allow(clippy::cast_lossless)]
                if elapsed >= hold_secs as u64 {
                    state.latched = false;
                    state.entered_at = None;
                    state.consecutive_above = 0;
                    return PanicAction::Passthrough;
                }
            }
        }
        return PanicAction::ForceFxMx;
    }

    PanicAction::Passthrough
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn single_tick_no_trigger() {
        let mut s = PanicState::new();
        let now = Instant::now();
        let action = check(&mut s, 96.0, 95.0, 10, now);
        assert_eq!(action, PanicAction::Passthrough);
        assert!(!s.latched);
    }

    #[test]
    fn two_ticks_triggers() {
        let mut s = PanicState::new();
        let now = Instant::now();
        check(&mut s, 96.0, 95.0, 10, now);
        let action = check(&mut s, 96.0, 95.0, 10, now);
        assert_eq!(action, PanicAction::ForceFxMx);
        assert!(s.latched);
    }

    #[test]
    fn hold_prevents_exit() {
        let mut s = PanicState::new();
        let now = Instant::now();
        check(&mut s, 96.0, 95.0, 10, now);
        check(&mut s, 96.0, 95.0, 10, now);
        assert!(s.latched);

        let later = now + Duration::from_secs(5);
        let action = check(&mut s, 90.0, 95.0, 10, later);
        assert_eq!(action, PanicAction::ForceFxMx);
    }

    #[test]
    fn exit_after_hold() {
        let mut s = PanicState::new();
        let now = Instant::now();
        check(&mut s, 96.0, 95.0, 10, now);
        check(&mut s, 96.0, 95.0, 10, now);
        assert!(s.latched);

        let later = now + Duration::from_secs(11);
        let action = check(&mut s, 90.0, 95.0, 10, later);
        assert_eq!(action, PanicAction::Passthrough);
        assert!(!s.latched);
    }

    #[test]
    fn panic_preserves_across_reinit() {
        let mut s = PanicState::new();
        let now = Instant::now();
        check(&mut s, 96.0, 95.0, 10, now);
        check(&mut s, 96.0, 95.0, 10, now);
        assert!(s.latched);

        // Simulating FR-049: re-check with current temp, don't reset
        let action = check(&mut s, 96.0, 95.0, 10, now);
        assert_eq!(action, PanicAction::ForceFxMx);
        assert!(s.latched);
    }

    #[test]
    fn intermittent_does_not_trigger() {
        let mut s = PanicState::new();
        let now = Instant::now();
        check(&mut s, 96.0, 95.0, 10, now);
        check(&mut s, 94.0, 95.0, 10, now);
        let action = check(&mut s, 96.0, 95.0, 10, now);
        assert_eq!(action, PanicAction::Passthrough);
        assert!(!s.latched);
    }
}
