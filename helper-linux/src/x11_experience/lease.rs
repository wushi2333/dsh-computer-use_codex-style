//! Freshness lease: an observation is good until a *human* touches the machine.
//!
//! The semantics are the ones `helper-rs/src/interrupt.rs` implements on Windows
//! (`USER_INPUT_MESSAGE`, the armed/dirty split, the synthetic-input grace) and the
//! turn lifecycle `src/sidecar.js` drives (`end_turn` = "hide the overlay, flush the
//! observation lease and re-arm for the next turn").
//!
//! This module is deliberately free of X11 and of time: every transition takes the
//! `Instant` it happened at, so the whole state machine is unit-testable without a
//! display.

use std::time::{Duration, Instant};

/// The sentence a guarded call refuses with. Must stay byte-identical to the
/// official wording so the model cannot tell the two implementations apart.
pub const USER_INPUT_MESSAGE: &str =
    "user input was detected in this window; call get_window_state before continuing";

/// How long an injected event keeps being attributed to us rather than to the
/// human. Mirrors `interrupt::SYNTHETIC_UNTIL` on Windows: our own XTest input is
/// indistinguishable from the operator's at the wire level, so the only honest
/// signal is the window we mark around our own injections.
pub const DEFAULT_SYNTHETIC_GRACE: Duration = Duration::from_millis(250);

/// The keystroke that brought the overlay up must not count as human input; the
/// official Windows value is 200 ms (`ESC_ARM_GRACE`).
pub const DEFAULT_ARM_GRACE: Duration = Duration::from_millis(200);

/// What the model may do with the observation it holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freshness {
    /// Nothing has been observed yet this turn.
    Unobserved,
    /// Observed and untouched since.
    Fresh,
    /// Observed, then the human took over.
    Stale,
}

impl Freshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Freshness::Unobserved => "unobserved",
            Freshness::Fresh => "fresh",
            Freshness::Stale => "stale",
        }
    }
}

/// Why an observation is no longer trustworthy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StaleReason {
    HumanKey,
    HumanPointerButton,
    HumanPointerMotion,
}

impl StaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StaleReason::HumanKey => "human_key",
            StaleReason::HumanPointerButton => "human_pointer_button",
            StaleReason::HumanPointerMotion => "human_pointer_motion",
        }
    }
}

/// The lease itself. One per helper process; `generation` identifies the turn it
/// belongs to so a late event from a finished turn cannot poison the next one.
#[derive(Debug)]
pub struct Lease {
    freshness: Freshness,
    reason: Option<StaleReason>,
    generation: u64,
    armed: bool,
    armed_at: Option<Instant>,
    synthetic_until: Option<Instant>,
    /// Counters, exposed through diagnostics: they are what tells "the monitor is
    /// working" from "the monitor never saw anything".
    pub human_events: u64,
    pub synthetic_events: u64,
    pub flushes: u64,
}

impl Default for Lease {
    fn default() -> Self {
        Self::new()
    }
}

impl Lease {
    pub fn new() -> Self {
        Lease {
            freshness: Freshness::Unobserved,
            reason: None,
            generation: 0,
            armed: false,
            armed_at: None,
            synthetic_until: None,
            human_events: 0,
            synthetic_events: 0,
            flushes: 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn freshness(&self) -> Freshness {
        self.freshness
    }

    pub fn stale_reason(&self) -> Option<StaleReason> {
        self.reason
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Arm the monitor for a turn. Idempotent: arming twice must not restart the
    /// grace window, or a repeated show() would keep swallowing real Escapes.
    pub fn arm(&mut self, at: Instant) {
        if !self.armed {
            self.armed = true;
            self.armed_at = Some(at);
        }
    }

    /// Disarm and forget the turn. `end_turn` and `shutdown` both land here.
    pub fn disarm(&mut self) {
        self.armed = false;
        self.armed_at = None;
    }

    /// A fresh observation. This is `get_window_state`: it clears staleness.
    pub fn observe(&mut self, _at: Instant) {
        self.freshness = Freshness::Fresh;
        self.reason = None;
    }

    /// Flush for a new turn: nothing observed yet, and the generation moves on so
    /// the caller can tell observations from the previous turn apart.
    pub fn flush(&mut self) {
        self.freshness = Freshness::Unobserved;
        self.reason = None;
        self.generation = self.generation.wrapping_add(1);
        self.flushes = self.flushes.wrapping_add(1);
        self.disarm();
    }

    /// Mark a window in which injected events are ours, not the operator's.
    pub fn note_synthetic(&mut self, window: Duration, at: Instant) {
        let until = at + window;
        self.synthetic_until = Some(match self.synthetic_until {
            Some(existing) if existing > until => existing,
            _ => until,
        });
    }

    /// True while an event at `at` is attributed to our own injection.
    pub fn is_synthetic(&self, at: Instant) -> bool {
        matches!(self.synthetic_until, Some(until) if at <= until)
    }

    /// True while the grace that covers the keystroke which armed us is running.
    fn in_arm_grace(&self, at: Instant) -> bool {
        match self.armed_at {
            Some(armed) => at.saturating_duration_since(armed) < DEFAULT_ARM_GRACE,
            None => false,
        }
    }

    /// Record one raw input event. Returns the reason it counted as *human* input,
    /// or `None` when it was ignored (ours, or the arming keystroke).
    ///
    /// Motion only counts while armed and outside the grace: the pointer drifts
    /// constantly on a real desktop and a lease that dies on every pixel would be
    /// useless.
    pub fn note_input(&mut self, kind: StaleReason, at: Instant) -> Option<StaleReason> {
        if self.is_synthetic(at) {
            self.synthetic_events = self.synthetic_events.wrapping_add(1);
            return None;
        }
        if !self.armed {
            return None;
        }
        if self.in_arm_grace(at) {
            return None;
        }
        self.human_events = self.human_events.wrapping_add(1);
        if self.freshness == Freshness::Fresh {
            self.freshness = Freshness::Stale;
            self.reason = Some(kind);
        }
        Some(kind)
    }

    /// The guard every action runs through before it touches the desktop.
    ///
    /// Deliberately not an error for `Unobserved`: a click straight after a
    /// `get_window_state` is the normal path, and the official helper only refuses
    /// once the human has actually interfered.
    pub fn check(&self) -> Result<(), String> {
        match self.freshness {
            Freshness::Stale => Err(USER_INPUT_MESSAGE.to_string()),
            _ => Ok(()),
        }
    }

    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "freshness": self.freshness.as_str(),
            "staleReason": self.reason.map(StaleReason::as_str),
            "armed": self.armed,
            "generation": self.generation,
            "humanEvents": self.human_events,
            "syntheticEvents": self.synthetic_events,
            "flushes": self.flushes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn the_refusal_wording_is_the_official_one() {
        assert_eq!(
            USER_INPUT_MESSAGE,
            "user input was detected in this window; call get_window_state before continuing"
        );
    }

    #[test]
    fn a_fresh_observation_is_usable() {
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(10));
        assert_eq!(lease.freshness(), Freshness::Fresh);
        assert!(lease.check().is_ok());
    }

    #[test]
    fn an_unobserved_turn_is_not_refused() {
        // The normal path: observe, act. Nothing observed yet must not refuse either,
        // because the model may legitimately act on a window it just listed.
        let lease = Lease::new();
        assert_eq!(lease.freshness(), Freshness::Unobserved);
        assert!(lease.check().is_ok());
    }

    #[test]
    fn human_input_while_armed_makes_the_observation_stale() {
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(500));
        // Outside the arming grace.
        assert_eq!(lease.note_input(StaleReason::HumanKey, t(900)), Some(StaleReason::HumanKey));
        assert_eq!(lease.freshness(), Freshness::Stale);
        assert_eq!(lease.check(), Err(USER_INPUT_MESSAGE.to_string()));
    }

    #[test]
    fn a_second_observation_clears_the_staleness() {
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(500));
        lease.note_input(StaleReason::HumanPointerButton, t(900));
        assert!(lease.check().is_err());
        lease.observe(t(1000));
        assert!(lease.check().is_ok());
        assert_eq!(lease.stale_reason(), None);
    }

    #[test]
    fn input_outside_a_turn_is_ignored() {
        let mut lease = Lease::new();
        lease.observe(t(0));
        assert_eq!(lease.note_input(StaleReason::HumanKey, t(50)), None);
        assert_eq!(lease.freshness(), Freshness::Fresh);
    }

    #[test]
    fn the_arming_keystroke_does_not_stale_the_lease() {
        let mut lease = Lease::new();
        let armed = t(0);
        lease.arm(armed);
        lease.observe(armed);
        // Escape pressed by the arm path itself, inside the 200 ms grace.
        assert_eq!(lease.note_input(StaleReason::HumanKey, armed + Duration::from_millis(50)), None);
        assert_eq!(lease.freshness(), Freshness::Fresh);
    }

    #[test]
    fn our_own_injected_input_is_not_human_input() {
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(500));
        lease.note_synthetic(Duration::from_millis(250), t(900));
        assert_eq!(lease.note_input(StaleReason::HumanPointerMotion, t(950)), None);
        assert_eq!(lease.freshness(), Freshness::Fresh);
        assert_eq!(lease.synthetic_events, 1);
        // Once the window expires the same event counts again.
        assert!(lease.note_input(StaleReason::HumanPointerMotion, t(1200)).is_some());
    }

    #[test]
    fn the_synthetic_window_never_shrinks() {
        let mut lease = Lease::new();
        lease.note_synthetic(Duration::from_millis(500), t(0));
        lease.note_synthetic(Duration::from_millis(100), t(10));
        assert!(lease.is_synthetic(t(400)));
    }

    #[test]
    fn end_turn_flushes_the_lease_and_moves_the_generation_on() {
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(100));
        lease.note_input(StaleReason::HumanKey, t(900));
        let before = lease.generation();
        lease.flush();
        assert_eq!(lease.freshness(), Freshness::Unobserved);
        assert!(lease.check().is_ok(), "a flushed lease must not refuse the next turn");
        assert_eq!(lease.generation(), before + 1);
        assert!(!lease.is_armed(), "end_turn re-arms for the next turn, it does not stay armed");
    }

    #[test]
    fn arming_twice_does_not_restart_the_grace() {
        // Otherwise a show() repeated while working would keep refreshing the grace
        // and swallow a real Escape indefinitely.
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(300));
        lease.arm(t(400));
        assert!(lease.note_input(StaleReason::HumanKey, t(500)).is_some());
    }

    #[test]
    fn diagnostics_name_the_state_instead_of_a_bare_boolean() {
        let mut lease = Lease::new();
        lease.arm(t(0));
        lease.observe(t(100));
        lease.note_input(StaleReason::HumanPointerButton, t(900));
        let d = lease.diagnostics();
        assert_eq!(d["freshness"], serde_json::json!("stale"));
        assert_eq!(d["staleReason"], serde_json::json!("human_pointer_button"));
        assert_eq!(d["humanEvents"], serde_json::json!(1));
        assert_eq!(d["armed"], serde_json::json!(true));
    }
}
