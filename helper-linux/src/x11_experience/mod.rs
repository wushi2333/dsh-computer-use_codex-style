//! The window2 experience layer for X11 sessions: status pill, synthesized pointer,
//! freshness lease, and global Escape.
//!
//! This is the Linux counterpart of `helper-rs/src/overlay` + `helper-rs/src/interrupt`.
//! The Windows build draws the pill and the pointer with DirectComposition and Direct2D
//! and installs low-level keyboard/mouse hooks; on X11 the same four behaviours are
//! plain protocol requests:
//!
//! | behaviour | Windows | here |
//! |---|---|---|
//! | status pill | layered DirectComposition window | override-redirect X window |
//! | suppress the real pointer | `SetSystemCursor` in a child process | `XFixesHideCursor` on the root |
//! | synthesized pointer | Direct2D sprite, polled position | filled polygon on an override-redirect window |
//! | human-input watchdog | `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks | XInput2 raw events on the root |
//! | Escape to cancel | `WH_KEYBOARD_LL` + `exit(130)` | `XGrabKey(Escape)` on the root while armed |
//!
//! Three rules shape the code:
//!
//! 1. **Pure Rust.** This machine has no X11 development packages (no `pkg-config`,
//!    no headers), so everything goes through `x11rb` with `default-features = false`,
//!    which speaks the wire protocol and never links `libxcb`. XFixes, XInput2 and
//!    XTest are protocol extensions, not system libraries.
//! 2. **Nothing is claimed that was not negotiated.** Every capability is probed at
//!    startup; a missing extension degrades to a polling fallback and says so in
//!    diagnostics instead of failing later with an unrelated X error.
//! 3. **The operator's desktop is borrowed, never taken.** The real pointer is
//!    restored, the pill is unmapped and the Escape grab is released the moment a
//!    turn ends -- on `end_turn`, on `interrupt`, on `shutdown`, and on a
//!    disconnect. A helper that exits while holding a root Escape grab would eat the
//!    operator's own Escape key for the rest of the session.
//!
//! The pure state machines live in `cursor`, `lease` and `pill` and are unit-tested
//! without a display; `x11` is the thin, imperative shell that talks to the server.

pub mod cursor;
pub mod lease;
pub mod pill;
mod x11;

use std::sync::{Arc, OnceLock};

pub use cursor::{press_scale, Sprite};
pub use lease::{Freshness, Lease, StaleReason, USER_INPUT_MESSAGE};
pub use pill::PillState;
pub use x11::Handle;

/// The live experience layer, if this session has one.
///
/// `None` is a supported state, not a failure: on Wayland, with no `DISPLAY`, or when
/// the X connection cannot be opened, the helper keeps working with the overlay
/// features absent and diagnostics say why.
#[derive(Debug)]
pub struct Session {
    handle: Handle,
    capabilities: serde_json::Value,
}

impl Session {
    /// Observation begins for this turn.
    pub fn begin(&self) {
        self.handle.begin();
    }

    /// A fresh observation: clears the staleness the human's input caused.
    pub fn observe(&self) {
        self.handle.observe();
    }

    /// A tool action is in flight.
    pub fn working(&self) {
        self.handle.working();
    }

    /// The human took over; the pill says so.
    pub fn blocked(&self) {
        self.handle.blocked();
    }

    /// The turn ended: hide the overlay, flush the lease, release Escape.
    pub fn end_turn(&self) {
        self.handle.end_turn();
    }

    /// An interrupt: same teardown, no turn flush.
    pub fn interrupt(&self) {
        self.handle.interrupt();
    }

    pub fn shutdown(&self) {
        self.handle.shutdown();
    }

    /// True when a physical Escape was seen since the last `take_escaped`.
    pub fn escaped(&self) -> bool {
        self.handle.escaped()
    }

    pub fn take_escaped(&self) -> bool {
        self.handle.take_escaped()
    }

    /// True while the layer is armed for the current turn (the pill is up, the
    /// synthesized pointer has taken over and the lease is watching for human input).
    ///
    /// This is deliberately separate from capabilities(): a session can negotiate
    /// every extension and still be *idle*, which is the normal state between turns.
    pub fn is_armed(&self) -> bool {
        self.handle.is_armed()
    }

    /// How Escape is being detected. "state" is one of "untried", "installed"
    /// (the root grab is held) or "refused" (the server answered BadAccess, so only
    /// XInput2 raw-key detection is live); "note" carries the server's own words.
    pub fn escape_grab(&self) -> serde_json::Value {
        self.handle.escape_grab()
    }

    /// The signal to select on in order to stop an in-flight call.
    pub fn interrupt_notice(&self) -> Arc<tokio::sync::Notify> {
        self.handle.interrupt_notice()
    }

    /// Refuse an action when the human has taken over since the last observation.
    pub fn check_lease(&self) -> Result<(), String> {
        self.handle.check_lease()
    }

    pub fn diagnostics(&self) -> serde_json::Value {
        self.handle.diagnostics()
    }

    pub fn capabilities(&self) -> &serde_json::Value {
        &self.capabilities
    }
}

static SESSION: OnceLock<Option<Arc<Session>>> = OnceLock::new();

/// The process-wide session, created on first use.
///
/// Deliberately infallible: a helper that cannot open an X connection must still
/// serve its tools, so a failure is remembered as an unavailable reason rather than
/// propagated as an error the caller has to handle on every call.
pub fn session() -> Option<Arc<Session>> {
    SESSION
        .get_or_init(|| match start() {
            Ok(session) => Some(Arc::new(session)),
            Err(reason) => {
                eprintln!("x11-experience: disabled: {reason}");
                None
            }
        })
        .clone()
}

/// Start the layer explicitly. Used by `session()` and by tests.
pub fn start() -> Result<Session, String> {
    if !is_x11_session() {
        return Err("not an X11 session (needs DISPLAY on an X11, not a Wayland, session)".to_string());
    }
    let (handle, capabilities) = x11::spawn()?;
    Ok(Session { handle, capabilities })
}

/// Whether this looks like a plain X11 session we can drive.
///
/// Mirrors the crate's own `windowing::backends::x11` rule so the two never disagree:
/// an XWayland `DISPLAY` under a Wayland compositor is not an X11 session and must not
/// be claimed as one.
pub fn is_x11_session() -> bool {
    x11_session_from(
        std::env::var("DISPLAY").ok().as_deref(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
}

/// The pure rule behind `is_x11_session`, so it is testable without a display.
pub fn x11_session_from(display: Option<&str>, session_type: Option<&str>, wayland_display: Option<&str>) -> bool {
    fn nonempty(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|v| !v.is_empty())
    }
    if nonempty(display).is_none() {
        return false;
    }
    match nonempty(session_type) {
        Some("x11") => true,
        Some("wayland") => false,
        _ => nonempty(wayland_display).is_none(),
    }
}

/// What the experience layer is doing right now, for `health`.
///
/// The old summary reported a bare `"state": "on"` as soon as the extensions had been
/// negotiated, which was true of the negotiation and false of the layer: nothing armed
/// it, so the pill was never mapped and the Escape grab was never installed while
/// `health` claimed the layer was on. Availability and activation are two different
/// facts and are now two different fields:
///
/// * `available` -- the session has a layer at all (an X11 session whose X connection
///   opened and whose capabilities were probed);
/// * `state` -- `"off"` (no layer), `"available"` (idle, nothing armed) or `"armed"`
///   (a turn is live: the pill is mapped, the pointer is taken over, the lease is
///   watching for human input);
/// * `armed`, `escapeGrab` -- the specifics, so a refusal (BadAccess from the
///   compositor's own Escape binding) is reported rather than hidden.
pub fn health_summary() -> serde_json::Value {
    match session() {
        None => serde_json::json!({
            "state": "off",
            "available": false,
            "armed": false,
            "note": "no experience layer on this session (not X11, or the X connection could not be opened)",
        }),
        Some(session) => {
            let caps = session.capabilities();
            let armed = session.is_armed();
            let grab = session.escape_grab();
            // Only a refusal is worth a diagnostic sentence: an untried grab is simply
            // "no turn has run yet", which is not degraded behaviour.
            let degraded = match grab["state"].as_str() {
                Some("refused") => Some(format!(
                    "Escape is detected through XInput2 raw key events only: the root grab was refused ({})",
                    grab["note"].as_str().unwrap_or("no reason reported")
                )),
                _ => None,
            };
            // An overlay that could not be made click-through is not drawn at all, so this
            // is not a warning about a live hazard: it says the level of service dropped.
            let click_degraded = match caps["overlayClickThrough"].as_bool() {
                Some(false) => Some(format!(
                    "the overlays are not drawn: they could not be made click-through, and an overlay                      that intercepts pointer events would break every synthesized click ({})",
                    caps["overlayClickThroughError"]
                        .as_str()
                        .unwrap_or("reason not reported")
                )),
                _ => None,
            };
            serde_json::json!({
                "state": if armed { "armed" } else { "available" },
                "available": true,
                "armed": armed,
                "pill": caps["xfixesCursorSuppression"],
                "rawEvents": caps["rawEvents"],
                "pollingFallback": caps["pointerPollingFallback"],
                "overlayClickThrough": caps["overlayClickThrough"],
                "escapeGrab": grab,
                // One field, whichever reason applies: a refused Escape grab degrades the
                // interrupt path, a refused input shape degrades click delivery. The grab
                // refusal is reported first because it is the more common desktop.
                "degraded": degraded.or(click_degraded),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_x11_session_is_recognised_by_its_display() {
        assert!(x11_session_from(Some(":0"), Some("x11"), None));
        assert!(x11_session_from(Some(":97"), None, None));
    }

    #[test]
    fn a_wayland_session_is_never_claimed_as_x11() {
        // XWayland sets DISPLAY under a Wayland compositor; driving it as if it were
        // an X11 session is exactly the mistake this rule exists to prevent.
        assert!(!x11_session_from(Some(":0"), Some("wayland"), Some("wayland-0")));
        assert!(!x11_session_from(Some(":0"), None, Some("wayland-0")));
    }

    #[test]
    fn no_display_means_no_session() {
        assert!(!x11_session_from(None, Some("x11"), None));
        assert!(!x11_session_from(Some("   "), Some("x11"), None));
    }

    #[test]
    fn health_never_reports_availability_as_activation() {
        // The defect this guards: health said "on" whenever the extensions had been
        // negotiated, so an idle layer that had never armed anything looked identical
        // to a live turn. Availability and activation are separate facts now, and
        // "armed" is only ever produced by a layer that really armed.
        let summary = health_summary();
        assert!(summary.get("available").is_some());
        assert!(matches!(
            summary["state"].as_str(),
            Some("off") | Some("available") | Some("armed")
        ));
        match summary["state"].as_str() {
            Some("off") => {
                assert_eq!(summary["available"], serde_json::json!(false));
                assert_eq!(summary["armed"], serde_json::json!(false));
            }
            _ => {
                assert_eq!(summary["available"], serde_json::json!(true));
                // The two must agree, or one of them is lying.
                assert_eq!(summary["armed"], serde_json::json!(summary["state"] == serde_json::json!("armed")));
            }
        }
    }
}
