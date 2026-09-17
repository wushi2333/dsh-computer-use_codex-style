//! The status pill: what it says, and where it sits.
//!
//! `parity/overlay-lifecycle.json` is the shape this mirrors -- an idle desktop shows
//! exactly one pointer, `show` draws the fake cursor *and* the banner and suppresses
//! the real pointer, `cancel`/`end_turn` hide both and restore the real one. The
//! Windows implementation draws the banner with DirectComposition
//! (`compute_pill_layout`, `PILL_CONTENT_HEIGHT`, `PILL_PADDING`); here it is a plain
//! X window the X server fills, which is why only the arithmetic lives in this module.

use std::time::{Duration, Instant};

/// Banner text, one per lifecycle state. The wording is what the operator reads, so
/// it names the state rather than repeating the tool name.
pub const LABEL_IDLE: &str = "Computer Use: ready";
pub const LABEL_OBSERVING: &str = "Computer Use: observing";
pub const LABEL_WORKING: &str = "Computer Use: working";
pub const LABEL_BLOCKED: &str = "Computer Use: user took over";

/// Content height and padding, in device pixels at scale 1.0. Both are the official
/// values (`scale * 48.0` content, `scale * 18.0` padding; 140057dbb:404-494).
pub const CONTENT_HEIGHT: i32 = 48;
pub const PADDING: i32 = 18;
/// Margin from the screen edge.
pub const MARGIN: i32 = 24;
/// The pulse that marks a fresh observation, and the fade the exit uses.
pub const PULSE_MS: u64 = 550;
pub const FADE_MS: u64 = 220;

/// What the pill is currently reporting. Drives both the text and the accent colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillState {
    Hidden,
    Idle,
    Observing,
    Working,
    /// The human touched the machine; the model must re-observe.
    Blocked,
}

impl PillState {
    pub fn label(self) -> &'static str {
        match self {
            PillState::Hidden | PillState::Idle => LABEL_IDLE,
            PillState::Observing => LABEL_OBSERVING,
            PillState::Working => LABEL_WORKING,
            PillState::Blocked => LABEL_BLOCKED,
        }
    }

    /// Accent colour as `0xRRGGBB`: the shared warm accent the official pill uses,
    /// a calmer one while idle, and an alert tone once the human has taken over.
    pub fn accent(self) -> u32 {
        match self {
            PillState::Hidden | PillState::Idle => 0x00_8C_7A,
            PillState::Observing => 0x00_B3_9C,
            PillState::Working => 0x00_C2_A8,
            PillState::Blocked => 0x00_C7_6B,
        }
    }

    /// Screen-space geometry of the pill for a given text length and scale.
    ///
    /// The width is derived from the label rather than fixed, because the sentence
    /// doubles as the operator's only explanation of what the helper is doing.
    pub fn geometry(self, screen: (i32, i32), chars: usize) -> Rect {
        let scale = 1.0f32;
        let char_w = (8.0 * scale).round() as i32;
        let content_w = (chars as i32) * char_w;
        let width = (content_w + 2 * PADDING).max(160);
        let height = CONTENT_HEIGHT + 2 * PADDING;
        // Top-right corner, the official pill's resting place.
        let x = (screen.0 - width - MARGIN).max(0);
        let y = MARGIN;
        Rect { x, y, width, height }
    }
}

/// Pixel rectangle in root coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, point: (i32, i32)) -> bool {
        point.0 >= self.x
            && point.0 < self.x + self.width
            && point.1 >= self.y
            && point.1 < self.y + self.height
    }
}

/// The pill's own lifetime: position, opacity, and the pulse that follows an
/// observation. Kept free of X11 so the animation is testable.
#[derive(Debug)]
pub struct Pill {
    state: PillState,
    rect: Option<Rect>,
    screen: (i32, i32),
    label: &'static str,
    shown_at: Option<Instant>,
    hidden_at: Option<Instant>,
    pulse_at: Option<Instant>,
}

impl Pill {
    pub fn new(screen: (i32, i32)) -> Self {
        Pill {
            state: PillState::Hidden,
            rect: None,
            screen,
            label: LABEL_IDLE,
            shown_at: None,
            hidden_at: None,
            pulse_at: None,
        }
    }

    pub fn state(&self) -> PillState {
        self.state
    }

    pub fn rect(&self) -> Option<Rect> {
        self.rect
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Show (or re-state) the pill. Idempotent for the same state: a repeated
    /// `show` must not restart the pulse, or the operator sees a strobe.
    pub fn show(&mut self, state: PillState, at: Instant) {
        let same = self.state == state && self.rect.is_some();
        self.state = state;
        self.label = state.label();
        self.rect = Some(state.geometry(self.screen, self.label.chars().count()));
        self.hidden_at = None;
        if !same {
            self.shown_at = Some(at);
            self.pulse_at = Some(at);
        }
    }

    /// Hide it. Keeps the last rect so the exit fade still knows where to draw.
    pub fn hide(&mut self, at: Instant) {
        if self.state == PillState::Hidden {
            return;
        }
        self.state = PillState::Hidden;
        self.hidden_at = Some(at);
        self.shown_at = None;
        self.pulse_at = None;
    }

    pub fn is_visible(&self) -> bool {
        self.state != PillState::Hidden
    }

    /// Mark a fresh observation so the pill pulses.
    pub fn pulse(&mut self, at: Instant) {
        self.pulse_at = Some(at);
    }

    /// Pulse brightness in [0, 1]; 0 when the pulse is over or absent.
    pub fn pulse_alpha(&self, now: Instant) -> f32 {
        match self.pulse_at {
            Some(at) => {
                let elapsed = now.saturating_duration_since(at);
                if elapsed >= Duration::from_millis(PULSE_MS) {
                    return 0.0;
                }
                let progress = elapsed.as_millis() as f32 / PULSE_MS as f32;
                1.0 - progress
            }
            None => 0.0,
        }
    }

    /// True once the exit fade has finished and the window may be unmapped.
    pub fn fade_finished(&self, now: Instant) -> bool {
        match self.hidden_at {
            Some(at) => now.saturating_duration_since(at) >= Duration::from_millis(FADE_MS),
            // Never shown: nothing to fade.
            None => !self.is_visible(),
        }
    }

    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "visible": self.is_visible(),
            "state": match self.state {
                PillState::Hidden => "hidden",
                PillState::Idle => "idle",
                PillState::Observing => "observing",
                PillState::Working => "working",
                PillState::Blocked => "blocked",
            },
            "label": self.label,
            "rect": self.rect.map(|r| serde_json::json!([r.x, r.y, r.width, r.height])),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn an_idle_desktop_has_no_pill() {
        let pill = Pill::new((1280, 800));
        assert!(!pill.is_visible());
        assert_eq!(pill.rect(), None);
        assert_eq!(pill.state(), PillState::Hidden);
    }

    #[test]
    fn showing_reports_visibility_and_a_rect_inside_the_screen() {
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Working, at(0));
        assert!(pill.is_visible());
        let r = pill.rect().expect("shown pill has a rect");
        assert!(r.width > 0 && r.height > 0);
        assert!(r.x >= 0 && r.y >= 0);
        assert!(r.x + r.width <= 1280, "pill runs off the right edge: {r:?}");
        assert!(r.y + r.height <= 800, "pill runs off the bottom edge: {r:?}");
        assert_eq!(r.height, CONTENT_HEIGHT + 2 * PADDING);
    }

    #[test]
    fn the_label_follows_the_state() {
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Observing, at(0));
        assert_eq!(pill.label(), LABEL_OBSERVING);
        pill.show(PillState::Blocked, at(10));
        assert_eq!(pill.label(), LABEL_BLOCKED);
        assert_ne!(PillState::Blocked.accent(), PillState::Idle.accent());
    }

    #[test]
    fn a_narrow_screen_still_gets_a_usable_pill() {
        let mut pill = Pill::new((120, 200));
        pill.show(PillState::Working, at(0));
        let r = pill.rect().unwrap();
        assert_eq!(r.x, 0);
        assert!(r.width >= 160);
    }

    #[test]
    fn hiding_is_idempotent_and_stops_the_pulse() {
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Working, at(0));
        pill.hide(at(10));
        pill.hide(at(20));
        assert!(!pill.is_visible());
        assert_eq!(pill.pulse_alpha(at(30)), 0.0);
    }

    #[test]
    fn the_pulse_decays_to_zero() {
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Working, at(0));
        assert!(pill.pulse_alpha(at(0)) > 0.9);
        assert!(pill.pulse_alpha(at(PULSE_MS / 2)) < 0.9);
        assert_eq!(pill.pulse_alpha(at(PULSE_MS + 1)), 0.0);
    }

    #[test]
    fn restating_the_same_state_does_not_restart_the_pulse() {
        // A strobe on every call would be worse than no pill at all.
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Working, at(0));
        pill.show(PillState::Working, at(500));
        assert_eq!(pill.pulse_alpha(at(600)), 0.0);
    }

    #[test]
    fn the_exit_fade_finishes_and_knows_where_it_was() {
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Working, at(0));
        let rect = pill.rect().unwrap();
        pill.hide(at(100));
        assert!(!pill.fade_finished(at(100)));
        assert!(pill.fade_finished(at(100 + FADE_MS)));
        assert_eq!(pill.rect(), Some(rect), "the fade needs the last rect to draw into");
    }

    #[test]
    fn a_pill_that_was_never_shown_is_already_faded() {
        let pill = Pill::new((1280, 800));
        assert!(pill.fade_finished(at(0)));
    }

    #[test]
    fn rect_hit_testing_is_half_open() {
        let r = Rect { x: 10, y: 10, width: 100, height: 50 };
        assert!(r.contains((10, 10)));
        assert!(r.contains((109, 59)));
        assert!(!r.contains((110, 59)));
        assert!(!r.contains((10, 60)));
        assert!(!r.contains((9, 10)));
    }

    #[test]
    fn diagnostics_report_the_orb_state_not_a_boolean() {
        let mut pill = Pill::new((1280, 800));
        pill.show(PillState::Observing, at(0));
        let d = pill.diagnostics();
        assert_eq!(d["visible"], serde_json::json!(true));
        assert_eq!(d["state"], serde_json::json!("observing"));
        assert!(d["rect"].is_array());
    }
}
