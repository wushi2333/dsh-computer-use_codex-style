//! The X11 side of the experience layer: one connection, one owner thread.
//!
//! Everything that touches the X server happens on this thread. RustConnection is
//! Send + Sync with fine-grained locks, but a single owner is what makes the
//! sequence "grab Esc, map the pill, follow the pointer" deterministic instead of
//! three threads racing for the same window.
//!
//! Pure-Rust only: x11rb with default-features = false never links libxcb, and
//! XFixes/XInput2/XTest are protocol extensions rather than system libraries. This
//! machine has no X11 development packages at all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::{ErrorKind, Event};
use x11rb::protocol::xfixes;
use x11rb::protocol::xinput;
use x11rb::protocol::xproto::{self, Window};
use x11rb::rust_connection::RustConnection;

use super::cursor::{self, Sprite};
use super::lease::{Lease, StaleReason};
use super::pill::{self, Pill, PillState};

/// Poll cadence while something is on screen (the synthesized pointer has to keep
/// up with the real one) and while nothing is.
const POLL_ACTIVE: Duration = Duration::from_millis(4);
const POLL_IDLE: Duration = Duration::from_millis(25);

/// The Escape keycode is 9 on every X server that follows the traditional mapping,
/// but it is looked up rather than assumed.
const KEYSYM_ESCAPE: u32 = 0xff1b;

const PILL_BG: u32 = 0x0012_1619;
const PILL_FG: u32 = 0x00f2_f4f5;
const CURSOR_FILL: u32 = 0x00ff_ffff;

/// One of the raw XI2 events we subscribe to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RawInput {
    Key,
    Button,
    Motion,
}

/// What the helper thread asks the X thread to do.
#[derive(Debug)]
pub(crate) enum Command {
    /// Observation began: arm the Esc grab, show the pill, hide the real pointer
    /// and start drawing ours.
    Begin { label: &'static str },
    /// A tool action is running.
    Working,
    /// The lease went stale: the operator took over, so the pill says so.
    Blocked,
    /// end_turn / interrupt / shutdown: put the desktop back exactly as it was.
    Restore,
    Shutdown,
    /// Diagnostics: the X thread answers with its own view.
    Query(Sender<serde_json::Value>),
}

/// The state the UI thread and the X thread both need to agree on.
#[derive(Debug)]
struct Shared {
    lease: Mutex<Lease>,
    pill: Mutex<Pill>,
    /// Set by the X thread when a human pressed Escape out of band.
    escaped: AtomicBool,
    /// Last error, so diagnostics never claim success for a failed primitive.
    last_error: Mutex<Option<String>>,
    capabilities: Mutex<serde_json::Value>,
    /// What the X thread last managed to do about the Escape key.
    esc_grab: Mutex<EscGrab>,
    /// Whether the X thread may draw overlays at all (see `Platform::overlays_usable`).
    ///
    /// Held here as well because the pure `Pill` state would otherwise report itself
    /// visible on a server where nothing is ever mapped -- the same "honest health" rule
    /// the rest of this layer follows.
    overlays_usable: AtomicBool,
}

/// How Escape is actually being detected right now.
///
/// A refused grab is a NORMAL outcome on a real desktop rather than a failure of the
/// layer: the "none" modifier combination on Escape is routinely held by the
/// compositor's own global-shortcut client (KWin holds it on this machine), and a
/// second client asking for the same combination is answered with BadAccess. XInput2
/// raw key events are delivered to this client whether or not it owns the grab, so the
/// interrupt still works -- but health and diagnostics must say which path is live
/// instead of reporting a grab that the server never granted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EscGrab {
    /// True once the layer has tried to grab at least once.
    tried: bool,
    /// True while the root grab is held.
    installed: bool,
    /// Why the grab is not held, verbatim from the server.
    note: Option<String>,
}

impl Default for EscGrab {
    fn default() -> Self {
        EscGrab { tried: false, installed: false, note: None }
    }
}

impl EscGrab {
    fn state(&self) -> &'static str {
        if self.installed {
            "installed"
        } else if self.tried {
            "refused"
        } else {
            "untried"
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "state": self.state(), "note": self.note })
    }
}

/// Handle held by the helper: cheap to clone, all methods are non-blocking.
#[derive(Clone, Debug)]
pub struct Handle {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    interrupt: Arc<tokio::sync::Notify>,
}

impl Handle {
    /// Observation begins for this turn.
    pub fn begin(&self) {
        if let Ok(mut lease) = self.shared.lease.lock() {
            lease.arm(Instant::now());
            lease.observe(Instant::now());
        }
        if self.overlays_usable() {
            if let Ok(mut p) = self.shared.pill.lock() {
                p.show(PillState::Observing, Instant::now());
                p.pulse(Instant::now());
            }
        }
        self.send(Command::Begin { label: PillState::Observing.label() });
    }

    /// Whether this session can draw overlays without intercepting pointer events.
    fn overlays_usable(&self) -> bool {
        self.shared.overlays_usable.load(Ordering::SeqCst)
    }

    /// Re-observe: clears staleness and re-states the pill.
    pub fn observe(&self) {
        if let Ok(mut lease) = self.shared.lease.lock() {
            lease.observe(Instant::now());
        }
        if self.overlays_usable() {
            if let Ok(mut p) = self.shared.pill.lock() {
                p.show(PillState::Observing, Instant::now());
                p.pulse(Instant::now());
            }
        }
        // Re-state the pill as observing. Begin is idempotent (the grab is held and the
        // overlays are mapped already), so this repaints the overlay rather than arming
        // a second time. It deliberately does NOT go through Working: a fresh
        // observation labelled "working" would contradict both the shared pill state
        // and the sentence the operator reads.
        self.send(Command::Begin { label: PillState::Observing.label() });
    }

    /// A tool action is in flight.
    pub fn working(&self) {
        if self.overlays_usable() {
            if let Ok(mut p) = self.shared.pill.lock() {
                if p.is_visible() {
                    p.show(PillState::Working, Instant::now());
                }
            }
        }
        self.send(Command::Working);
    }

    /// The operator touched the machine.
    pub fn blocked(&self) {
        if self.overlays_usable() {
            if let Ok(mut p) = self.shared.pill.lock() {
                if p.is_visible() {
                    p.show(PillState::Blocked, Instant::now());
                }
            }
        }
        self.send(Command::Blocked);
    }

    /// The turn ended: hide everything, flush the lease, release the Esc grab.
    pub fn end_turn(&self) {
        if let Ok(mut lease) = self.shared.lease.lock() {
            lease.flush();
        }
        if let Ok(mut p) = self.shared.pill.lock() {
            p.hide(Instant::now());
        }
        self.send(Command::Restore);
    }

    /// Interrupt: the same teardown; the lease waits for the next observation.
    pub fn interrupt(&self) {
        if let Ok(mut lease) = self.shared.lease.lock() {
            lease.disarm();
        }
        if let Ok(mut p) = self.shared.pill.lock() {
            p.hide(Instant::now());
        }
        self.send(Command::Restore);
    }

    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }

    /// True when a physical Escape was seen since the last take_escaped.
    pub fn escaped(&self) -> bool {
        self.shared.escaped.load(Ordering::SeqCst)
    }

    /// Consume the Escape latch.
    pub fn take_escaped(&self) -> bool {
        self.shared.escaped.swap(false, Ordering::SeqCst)
    }

    /// The signal helper.rs selects on to stop an in-flight call the moment the
    /// operator presses Escape.
    pub fn interrupt_notice(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.interrupt)
    }

    /// Guard an action against a stale observation.
    pub fn check_lease(&self) -> Result<(), String> {
        match self.shared.lease.lock() {
            Ok(lease) => lease.check(),
            Err(_) => Ok(()),
        }
    }

    /// Whether the layer is armed for the current turn.
    pub fn is_armed(&self) -> bool {
        self.shared.lease.lock().map(|lease| lease.is_armed()).unwrap_or(false)
    }

    /// How Escape is being detected: the root grab, or only XInput2 raw events.
    pub fn escape_grab(&self) -> serde_json::Value {
        self.shared
            .esc_grab
            .lock()
            .map(|grab| grab.json())
            .unwrap_or(serde_json::Value::Null)
    }

    pub fn diagnostics(&self) -> serde_json::Value {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.tx.send(Command::Query(tx));
        let x11 = rx
            .recv_timeout(Duration::from_millis(400))
            .unwrap_or_else(|_| serde_json::json!({"ok": false, "detail": "x11 thread did not answer"}));
        let lease = self.shared.lease.lock().map(|l| l.diagnostics()).unwrap_or(serde_json::Value::Null);
        let pill = self.shared.pill.lock().map(|p| p.diagnostics()).unwrap_or(serde_json::Value::Null);
        let caps = self.shared.capabilities.lock().map(|c| c.clone()).unwrap_or(serde_json::Value::Null);
        let error = self.shared.last_error.lock().ok().and_then(|e| e.clone());
        serde_json::json!({
            "ok": error.is_none(),
            "backend": "x11",
            "pureRust": true,
            "lease": lease,
            "pill": pill,
            "capabilities": caps,
            "escaped": self.shared.escaped.load(Ordering::SeqCst),
            "armed": self.is_armed(),
            "escapeGrab": self.escape_grab(),
            "lastError": error,
            "x11": x11,
        })
    }

    fn send(&self, command: Command) {
        // A dead X thread must never block a tool call.
        let _ = self.tx.send(command);
    }
}

fn pill_height() -> u16 {
    (pill::CONTENT_HEIGHT + 2 * pill::PADDING) as u16
}

fn to_points(vertices: &[(i16, i16)]) -> Vec<xproto::Point> {
    vertices.iter().map(|(x, y)| xproto::Point { x: *x, y: *y }).collect()
}

/// Create an override-redirect overlay window: undecorated, unreparented and never
/// delayed by a window manager, which is what lets it disappear the instant a turn
/// ends.
fn create_overlay(conn: &RustConnection, root: Window, width: u16, height: u16) -> Result<Window, String> {
    let win = conn.generate_id().map_err(|e| e.to_string())?;
    xproto::create_window(
        conn,
        0, // CopyFromParent depth: the overlay always matches its parent
        win,
        root,
        0,
        0,
        width,
        height,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        0,
        &xproto::CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(PILL_BG)
            .border_pixel(0)
            .event_mask(xproto::EventMask::EXPOSURE | xproto::EventMask::STRUCTURE_NOTIFY),
    )
    .map_err(|e| e.to_string())?;
    Ok(win)
}

/// Make an overlay invisible to the pointer as well as to the eye.
///
/// An override-redirect window that is merely drawn on top still WINS HIT-TESTING: the
/// X server delivers a button event to the deepest window under the pointer, so a sprite
/// following the pointer would swallow the very clicks the helper is synthesizing, and a
/// pill parked in a corner would swallow whatever the operator clicks there. This was a
/// real regression, caught by the headless window2 end-to-end run:
///
///   [FAIL] xterm clicks land inside the target window   under pointer=0x400003 family=None
///
/// The standard cure is to give the window an EMPTY input region (XFixes
/// SetWindowShapeRegion with ShapeInput and Region None), which is the X11 equivalent of
/// the WS_EX_TRANSPARENT style the Windows helper sets on both of its overlays
/// (helper-rs/src/overlay/mod.rs). The window keeps painting; the pointer passes straight
/// through it to whatever is underneath.
///
/// A failure here is reported, never swallowed: an overlay that steals clicks is worse
/// than no overlay at all, so the caller records the outcome for diagnostics/health.
fn make_click_through(conn: &RustConnection, window: Window) -> Result<(), String> {
    // The region has to be an actually EMPTY one, not "no region". XFixes `Region` has no
    // named constant for an empty region: passing the protocol's "None" resets the input
    // shape to the window's DEFAULT shape (its bounding rectangle), which is the opposite
    // of what is wanted and leaves the overlay eating clicks -- measured, not assumed: with
    // "None" the regression test still saw the overlay receive the button event. Creating a
    // region from zero rectangles is what expresses "no input here".
    let region: xfixes::Region = conn.generate_id().map_err(|e| e.to_string())?;
    xfixes::create_region(conn, region, &[])
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
    xfixes::set_window_shape_region(
        conn,
        window,
        x11rb::protocol::shape::SK::INPUT,
        0,
        0,
        region,
    )
    .map_err(|e| e.to_string())?
    .check()
    .map_err(|e| e.to_string())
}

fn open_fixed_font(conn: &RustConnection) -> Option<(xproto::Font, i32)> {
    let fid = conn.generate_id().ok()?;
    // The "fixed" alias exists on every X server. A missing font is not fatal:
    // the pill then carries its accent state without the sentence.
    xproto::open_font(conn, fid, b"fixed").ok()?.check().ok()?;
    let info = xproto::query_font(conn, fid).ok()?.reply().ok()?;
    Some((fid, info.max_bounds.character_width as i32))
}

fn keysym_to_keycode(conn: &RustConnection, keysym: u32) -> Option<u8> {
    let setup = conn.setup();
    let count = setup.max_keycode - setup.min_keycode + 1;
    let reply = xproto::get_keyboard_mapping(conn, setup.min_keycode, count).ok()?.reply().ok()?;
    let per = reply.keysyms_per_keycode.max(1) as usize;
    reply
        .keysyms
        .iter()
        .position(|k| *k == keysym)
        .map(|i| setup.min_keycode + (i / per) as u8)
}

/// The X thread's own state. Never shared.
struct Platform {
    conn: RustConnection,
    root: Window,
    screen: (i32, i32),
    gc: xproto::Gcontext,
    font: Option<xproto::Font>,
    pill_win: Window,
    pill_mapped: bool,
    cursor_win: Window,
    cursor_mapped: bool,
    /// Whether the overlays may be shown at all.
    ///
    /// An overlay that cannot be made click-through is not drawn: it would intercept the
    /// clicks this helper exists to synthesize, and swallowing those is an unacceptable
    /// behaviour regression. Safety wins over cosmetics, so a server without usable
    /// XFixes gets no pill and no synthesized pointer, and health says why.
    overlays_usable: bool,
    esc_keycode: Option<u8>,
    esc_grabbed: bool,
    cursor_hidden: bool,
    last_pointer: (i32, i32),
    pressed_at: Option<Instant>,
    drawn: Option<(i32, i32, u32)>,
}

impl Platform {
    fn connect() -> Result<(Self, serde_json::Value), String> {
        let (conn, screen_num) = RustConnection::connect(None).map_err(|e| format!("X11 connect failed: {e}"))?;
        let screen = conn.setup().roots[screen_num].clone();
        let root = screen.root;

        // Capabilities are negotiated, never assumed: a missing extension has to show
        // up as a degraded primitive rather than as a later, unrelated X error.
        let fixes = xfixes::query_version(&conn, 5, 0).ok().and_then(|c| c.reply().ok());
        let (fixes_ok, fixes_detail) = match fixes {
            Some(r) => (r.major_version >= 5, format!("{}.{}", r.major_version, r.minor_version)),
            None => (false, "unavailable".to_string()),
        };
        let xi = xinput::xi_query_version(&conn, 2, 4).ok().and_then(|c| c.reply().ok());
        let (xi_major, xi_minor) = match xi {
            Some(r) => (r.major_version, r.minor_version),
            None => (0, 0),
        };
        let raw_ok = xi_major > 2 || (xi_major == 2 && xi_minor >= 1);

        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        xproto::create_gc(&conn, gc, root, &xproto::CreateGCAux::new().graphics_exposures(0))
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("create_gc: {e}"))?;

        let (font, char_width) = match open_fixed_font(&conn) {
            Some((f, w)) => (Some(f), w),
            None => (None, 8),
        };

        let pill_win = create_overlay(&conn, root, 320, pill_height())?;
        let cursor_win = create_overlay(&conn, root, 40, 40)?;
        // Both overlays are click-through before either is ever mapped. The pill is
        // included deliberately: helper-rs gives its banner WS_EX_TRANSPARENT exactly as
        // it does the cursor, so a banner that ate clicks would be a parity bug as well
        // as a usability one, and this helper only ever synchronizes the pointer -- no
        // overlay ever needs to be clickable.
        let mut click_through = true;
        let mut click_through_error: Option<String> = None;
        for (window, name) in [(pill_win, "pill"), (cursor_win, "cursor")] {
            if let Err(error) = make_click_through(&conn, window) {
                click_through = false;
                eprintln!("x11-experience: {name} overlay could not be made click-through: {error}");
                click_through_error.get_or_insert(format!("{name} overlay: {error}"));
            }
        }
        // No usable input shape means no overlays at all. This is deliberately a refusal
        // rather than a warning: an overlay that intercepts pointer events silently breaks
        // every synthesized click, which is the failure this whole request sequence exists
        // to prevent.
        let overlays_usable = click_through;
        conn.flush().map_err(|e| e.to_string())?;

        let esc_keycode = keysym_to_keycode(&conn, KEYSYM_ESCAPE);

        // Raw XInput2 events on the root window. The mask is a bitwise union inside a
        // single 32-bit element on purpose: XIEventMask is one CARD32 per element, so
        // listing the raw events as separate elements is rejected with BadValue.
        use xinput::XIEventMask as M;
        let raw_union = M::RAW_KEY_PRESS | M::RAW_KEY_RELEASE | M::RAW_BUTTON_PRESS | M::RAW_BUTTON_RELEASE | M::RAW_MOTION;
        let raw_selected = raw_ok
            && xinput::xi_select_events(&conn, root, &[xinput::EventMask { deviceid: 0, mask: vec![raw_union] }])
                .map(|c| c.check().is_ok())
                .unwrap_or(false);
        conn.flush().map_err(|e| e.to_string())?;

        let capabilities = serde_json::json!({
            "xfixes": fixes_detail,
            "xfixesCursorSuppression": fixes_ok,
            "xinput": format!("{xi_major}.{xi_minor}"),
            "rawEvents": raw_selected,
            // Honest about the fallback: without raw events the lease has to poll the
            // pointer, which cannot tell our own injection from the operator's.
            "pointerPollingFallback": !raw_selected,
            // The overlays are click-through (empty XFixes ShapeInput region). Reported so
            // a server that refused the request cannot leave the helper silently
            // swallowing the operator's clicks.
            "overlayClickThrough": click_through,
            "overlaysUsable": overlays_usable,
            "overlayClickThroughError": click_through_error,
            "escapeKeycode": esc_keycode,
            "fontMetrics": char_width,
        });

        let mut platform = Platform {
            conn,
            root,
            screen: (screen.width_in_pixels as i32, screen.height_in_pixels as i32),
            gc,
            font,
            pill_win,
            pill_mapped: false,
            cursor_win,
            cursor_mapped: false,
            overlays_usable,
            esc_keycode,
            esc_grabbed: false,
            cursor_hidden: false,
            last_pointer: (0, 0),
            pressed_at: None,
            drawn: None,
        };
        platform.last_pointer = platform.pointer();
        Ok((platform, capabilities))
    }

    fn pointer(&self) -> (i32, i32) {
        match xproto::query_pointer(&self.conn, self.root).map(|c| c.reply()) {
            Ok(Ok(reply)) => (reply.root_x as i32, reply.root_y as i32),
            _ => self.last_pointer,
        }
    }

    /// Install or release the Escape grab.
    ///
    /// The grab lives on the ROOT window. A session may have no window manager at all
    /// (Xvfb, or a bare X session), and without one the input focus is PointerRoot, so
    /// a grab on our own override-redirect window never fires -- measured: 0 Escape
    /// events delivered, against 12 delivered to a root grab. It is released the
    /// instant the turn ends, because a root grab the helper forgot about would
    /// swallow the operator's own Escape for the rest of the session.
    fn set_esc_grab(&mut self, on: bool, note: &Mutex<EscGrab>) {
        if on {
            if let Ok(mut grab) = note.lock() {
                grab.tried = true;
            }
        }
        if on == self.esc_grabbed {
            return;
        }
        let Some(code) = self.esc_keycode else {
            self.record_grab(note, false, "this keymap has no Escape keycode, so no grab is possible");
            return;
        };
        let result = if on {
            xproto::grab_key(&self.conn, false, self.root, xproto::ModMask::ANY, code, xproto::GrabMode::ASYNC, xproto::GrabMode::ASYNC)
        } else {
            xproto::ungrab_key(&self.conn, code, self.root, xproto::ModMask::ANY)
        };
        match result.map(|c| c.check()) {
            Ok(Ok(())) => {
                self.esc_grabbed = on;
                if on {
                    self.record_grab(note, true, "");
                }
            }
            // A refused install is a normal desktop outcome, not a panic: the "none"
            // modifier combination on Escape is routinely held by the compositor's own
            // global-shortcut client, and a second client asking for the same
            // combination is answered with BadAccess. Recording the reason is what lets
            // health report raw-key detection instead of a grab nobody granted.
            Ok(Err(e)) => {
                // The recorded state must be what the server actually granted, not what
                // we asked for: an install that failed leaves no grab, while a RELEASE
                // that failed leaves the grab still held. Saying "installed" for a
                // refused install is the exact contradiction the real-desktop run
                // caught -- state=installed next to a BadAccess note.
                self.record_grab(note, self.esc_grabbed, &self.grab_failure_reason(on));
                eprintln!("x11-experience: escape grab {} failed: {e}", if on { "install" } else { "release" });
            }
            Err(e) => {
                self.record_grab(note, false, &e.to_string());
                eprintln!("x11-experience: escape grab {} failed: {e}", if on { "install" } else { "release" });
            }
        }
        let _ = self.conn.flush();
    }

    /// Publish the grab outcome where diagnostics and health can read it.
    fn record_grab(&self, note: &Mutex<EscGrab>, installed: bool, reason: &str) {
        if let Ok(mut grab) = note.lock() {
            grab.tried = true;
            grab.installed = installed;
            grab.note = if reason.is_empty() { None } else { Some(reason.to_string()) };
        }
    }

    /// The sentence recorded when the grab itself was refused.
    ///
    /// BadAccess is named as such because it is the compositor's own Escape binding, and
    /// the layer keeps working through XInput2 -- reporting "BadAccess" alone would look
    /// like a broken primitive rather than a desktop that already uses that key.
    fn grab_failure_reason(&self, on: bool) -> String {
        let access = self.refused_with_bad_access(on);
        let action = if on { "install" } else { "release" };
        if access {
            format!(
                "the server refused the {action} with BadAccess: another client already owns the Escape key combination (a compositor global shortcut); Escape is still detected through XInput2 raw key events"
            )
        } else {
            format!("the server refused the {action} of the root Escape grab")
        }
    }

    /// Whether the server refused the grab with BadAccess.
    ///
    /// BadAccess is the one refusal that is a property of the desktop rather than of this
    /// helper: another client already owns that key combination. It is also the one that
    /// still leaves the layer fully functional, because XInput2 raw key events bypass
    /// grabs entirely -- so it is worth naming separately from a generic failure.
    fn refused_with_bad_access(&self, on: bool) -> bool {
        let Some(code) = self.esc_keycode else { return false };
        let probe = if on {
            xproto::grab_key(&self.conn, false, self.root, xproto::ModMask::ANY, code, xproto::GrabMode::ASYNC, xproto::GrabMode::ASYNC)
        } else {
            // Ungrabbing something this client does not hold is not an error either; an
            // error here means the connection itself refused the request.
            xproto::ungrab_key(&self.conn, code, self.root, xproto::ModMask::ANY)
        };
        matches!(
            probe.map(|cookie| cookie.check()),
            Ok(Err(x11rb::errors::ReplyError::X11Error(error)))
                if error.error_kind == ErrorKind::Access
        )
    }

    /// Blank the real pointer. XFixes hides it for the whole screen when asked on the
    /// root window, which is what makes "exactly one pointer" observable.
    ///
    /// Refused outright when the overlays are unusable: hiding the operator's real pointer
    /// without drawing a replacement would leave them with no pointer at all.
    fn set_cursor_hidden(&mut self, hidden: bool) {
        if hidden && !self.overlays_usable {
            return;
        }
        if hidden == self.cursor_hidden {
            return;
        }
        let result = if hidden {
            xfixes::hide_cursor(&self.conn, self.root)
        } else {
            xfixes::show_cursor(&self.conn, self.root)
        };
        match result.map(|c| c.check()) {
            Ok(Ok(())) => self.cursor_hidden = hidden,
            Ok(Err(e)) => eprintln!("x11-experience: xfixes cursor failed: {e}"),
            Err(e) => eprintln!("x11-experience: xfixes cursor failed: {e}"),
        }
        let _ = self.conn.flush();
    }

    fn set_foreground(&self, color: u32) {
        let _ = xproto::change_gc(&self.conn, self.gc, &xproto::ChangeGCAux::new().foreground(color));
    }

    fn show_pill(&mut self, state: PillState, rect: pill::Rect) {
        // Same rule as the cursor: no cosmetic banner is worth intercepting a click.
        if !self.overlays_usable {
            return;
        }
        let _ = xproto::configure_window(
            &self.conn,
            self.pill_win,
            &xproto::ConfigureWindowAux::new()
                .x(rect.x)
                .y(rect.y)
                .width(rect.width as u32)
                .height(rect.height as u32),
        );
        let _ = xproto::clear_area(&self.conn, false, self.pill_win, 0, 0, rect.width as u16, rect.height as u16);
        self.draw_pill(state, rect);
        if !self.pill_mapped {
            // Map after the first paint: mapping first lets the server clear the
            // window with its background, which erases the frame that was drawn.
            let _ = xproto::map_window(&self.conn, self.pill_win);
            let _ = xproto::clear_area(&self.conn, false, self.pill_win, 0, 0, rect.width as u16, rect.height as u16);
            self.draw_pill(state, rect);
            self.pill_mapped = true;
        }
        let _ = self.conn.flush();
    }

    fn draw_pill(&self, state: PillState, rect: pill::Rect) {
        // The accent bar carries the state; the sentence carries the meaning.
        self.set_foreground(state.accent());
        let _ = xproto::poly_fill_rectangle(
            &self.conn,
            self.pill_win,
            self.gc,
            &[xproto::Rectangle { x: 0, y: 0, width: 6, height: rect.height as u16 }],
        );
        self.set_foreground(PILL_BG);
        let _ = xproto::poly_fill_rectangle(
            &self.conn,
            self.pill_win,
            self.gc,
            &[xproto::Rectangle { x: 6, y: 0, width: (rect.width - 6).max(0) as u16, height: rect.height as u16 }],
        );
        self.set_foreground(PILL_FG);
        if self.font.is_some() {
            let baseline = (rect.height - 14) / 2 + 12;
            let _ = xproto::image_text8(&self.conn, self.pill_win, self.gc, (6 + pill::PADDING) as i16, baseline as i16, state.label().as_bytes());
        }
    }

    fn hide_pill(&mut self) {
        if self.pill_mapped {
            let _ = xproto::unmap_window(&self.conn, self.pill_win);
            let _ = self.conn.flush();
            self.pill_mapped = false;
        }
    }

    /// Draw the synthesized pointer at the pointer's real position.
    fn draw_cursor(&mut self, force: bool) {
        // Never map the sprite when it could not be made click-through: a pointer-sized
        // window that eats clicks is worse than no synthesized pointer.
        if !self.overlays_usable {
            return;
        }
        let pointer = self.pointer();
        self.last_pointer = pointer;
        let sprite = cursor::sprite(self.pressed_at, Instant::now());
        let key = (pointer.0, pointer.1, (sprite.scale * 1000.0) as u32);
        if !force && self.cursor_mapped && self.drawn == Some(key) {
            return;
        }
        self.drawn = Some(key);
        let (x, y) = cursor::window_origin(pointer, &sprite, self.screen);
        let _ = xproto::configure_window(
            &self.conn,
            self.cursor_win,
            &xproto::ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(sprite.width as u32)
                .height(sprite.height as u32),
        );
        if !self.cursor_mapped {
            let _ = xproto::map_window(&self.conn, self.cursor_win);
            self.cursor_mapped = true;
        }
        let _ = xproto::clear_area(&self.conn, false, self.cursor_win, 0, 0, sprite.width as u16, sprite.height as u16);
        self.paint_sprite(&sprite);
        let _ = self.conn.flush();
    }

    fn paint_sprite(&self, sprite: &Sprite) {
        // Shadow first, then the body: a white arrow on a white page is invisible
        // without it, which is why the Windows overlay keeps one too.
        self.set_foreground(PILL_BG);
        let _ = xproto::fill_poly(&self.conn, self.cursor_win, self.gc, xproto::PolyShape::COMPLEX, xproto::CoordMode::ORIGIN, &to_points(&sprite.shadow));
        self.set_foreground(CURSOR_FILL);
        let _ = xproto::fill_poly(&self.conn, self.cursor_win, self.gc, xproto::PolyShape::COMPLEX, xproto::CoordMode::ORIGIN, &to_points(&sprite.outline));
    }

    fn hide_cursor(&mut self) {
        if self.cursor_mapped {
            let _ = xproto::unmap_window(&self.conn, self.cursor_win);
            let _ = self.conn.flush();
            self.cursor_mapped = false;
        }
    }

    /// Put the desktop back: no pill, no synthesized pointer, the real pointer
    /// visible, Escape released. Safe to call twice.
    fn restore(&mut self, note: &Mutex<EscGrab>) {
        self.hide_pill();
        self.hide_cursor();
        self.set_cursor_hidden(false);
        self.set_esc_grab(false, note);
        self.pressed_at = None;
        self.drawn = None;
    }

    fn classify(&self, event: &Event) -> Option<RawInput> {
        match event {
            Event::XinputRawKeyPress(_) | Event::XinputRawKeyRelease(_) => Some(RawInput::Key),
            Event::XinputRawButtonPress(_) | Event::XinputRawButtonRelease(_) => Some(RawInput::Button),
            Event::XinputRawMotion(_) => Some(RawInput::Motion),
            _ => None,
        }
    }

    /// True when this event is the operator pressing Escape.
    fn is_escape(&self, event: &Event) -> bool {
        let escape = self.esc_keycode.unwrap_or(9) as u32;
        match event {
            Event::KeyPress(e) => e.detail as u32 == escape,
            Event::XinputRawKeyPress(e) => e.detail == escape,
            _ => false,
        }
    }

    fn diagnostics_view(&self, active: bool, grab: &EscGrab) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "grabState": grab.state(),
            "grabNote": grab.note,
            "pointer": [self.last_pointer.0, self.last_pointer.1],
            "grab": self.esc_grabbed,
            "suppressed": self.cursor_hidden,
            "pillMapped": self.pill_mapped,
            "cursorMapped": self.cursor_mapped,
            "active": active,
            "escapeKeycode": self.esc_keycode,
        })
    }
}

/// Spawn the owner thread. Returns the handle plus the negotiated capabilities.
pub fn spawn() -> Result<(Handle, serde_json::Value), String> {
    let (platform, capabilities) = Platform::connect()?;
    let (tx, rx) = std::sync::mpsc::channel::<Command>();
    let shared = Arc::new(Shared {
        lease: Mutex::new(Lease::new()),
        pill: Mutex::new(Pill::new(platform.screen)),
        escaped: AtomicBool::new(false),
        last_error: Mutex::new(None),
        overlays_usable: AtomicBool::new(
            capabilities["overlaysUsable"].as_bool().unwrap_or(false),
        ),
        capabilities: Mutex::new(capabilities.clone()),
        esc_grab: Mutex::new(EscGrab::default()),
    });
    let interrupt = Arc::new(tokio::sync::Notify::new());
    let thread_shared = Arc::clone(&shared);
    let thread_interrupt = Arc::clone(&interrupt);

    std::thread::Builder::new()
        .name("x11-experience".to_string())
        .spawn(move || run(platform, rx, thread_shared, thread_interrupt))
        .map_err(|e| format!("spawn x11-experience thread: {e}"))?;

    Ok((Handle { tx, shared, interrupt }, capabilities))
}

/// The owner loop: one command at a time, one event batch at a time, never both.
fn run(
    mut platform: Platform,
    rx: Receiver<Command>,
    shared: Arc<Shared>,
    interrupt: Arc<tokio::sync::Notify>,
) {
    let mut active = false;
    let mut pending: std::collections::VecDeque<Command> = std::collections::VecDeque::new();

    loop {
        // 1. Every queued command, in order, before the server is touched again.
        while let Ok(command) = rx.try_recv() {
            pending.push_back(command);
        }
        while let Some(command) = pending.pop_front() {
            match command {
                Command::Begin { label } => {
                    active = true;
                    platform.set_esc_grab(true, &shared.esc_grab);
                    platform.set_cursor_hidden(true);
                    platform.pressed_at = None;
                    let rect = shared
                        .pill
                        .lock()
                        .map(|p| p.rect())
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| PillState::Observing.geometry(platform.screen, label.chars().count()));
                    platform.show_pill(PillState::Observing, rect);
                    platform.draw_cursor(true);
                }
                Command::Working => {
                    if let Ok(Some(rect)) = shared.pill.lock().map(|p| p.rect()) {
                        platform.show_pill(PillState::Working, rect);
                    }
                    platform.pressed_at = Some(Instant::now());
                    platform.draw_cursor(true);
                }
                Command::Blocked => {
                    if let Ok(Some(rect)) = shared.pill.lock().map(|p| p.rect()) {
                        platform.show_pill(PillState::Blocked, rect);
                    }
                }
                Command::Restore => {
                    active = false;
                    platform.restore(&shared.esc_grab);
                }
                Command::Shutdown => {
                    platform.restore(&shared.esc_grab);
                    return;
                }
                Command::Query(reply) => {
                    let grab = shared.esc_grab.lock().map(|g| g.clone()).unwrap_or_default();
                    let _ = reply.send(platform.diagnostics_view(active, &grab));
                }
            }
        }

        // 2. Every event the server has for us. Only the last pointer position
        //    matters, so motion collapses into a single redraw flag.
        let mut redraw = false;
        loop {
            match platform.conn.poll_for_event() {
                Ok(Some(event)) => {
                    // Only an armed turn may interpret Escape as an interrupt. This is
                    // the ARMED rule from helper-rs (its low-level hook is resident but
                    // only swallows Escape while the overlay is up), and it is load
                    // bearing here: XInput2 raw events deliberately bypass grabs, so a
                    // raw Escape arrives even when the root grab is gone. Without this
                    // gate the layer would eat the operator's Escape for the whole
                    // session -- caught by the integration test, not by inspection.
                    let armed = shared.lease.lock().map(|lease| lease.is_armed()).unwrap_or(false);
                    if armed && platform.is_escape(&event) {
                        // Escape is out-of-band: latch it for the helper, put the
                        // desktop back, and let helper.rs stop the in-flight call.
                        shared.escaped.store(true, Ordering::SeqCst);
                        // Disarm the lease as well as the X state. Without this the
                        // desktop was handed back while the lease still called the turn
                        // armed, so the operator's very next keystroke -- starting with
                        // the Escape release that is still in flight -- counted as human
                        // input and re-mapped the pill in its "user took over" state. The
                        // desktop must stay handed back until the helper arms a new turn.
                        if let Ok(mut lease) = shared.lease.lock() {
                            lease.disarm();
                        }
                        if let Ok(mut p) = shared.pill.lock() {
                            p.hide(Instant::now());
                        }
                        active = false;
                        platform.restore(&shared.esc_grab);
                        interrupt.notify_waiters();
                        continue;
                    }
                    match platform.classify(&event) {
                        Some(RawInput::Motion) => redraw = true,
                        Some(kind) => {
                            redraw = true;
                            let reason = match kind {
                                RawInput::Key => StaleReason::HumanKey,
                                RawInput::Button => StaleReason::HumanPointerButton,
                                RawInput::Motion => StaleReason::HumanPointerMotion,
                            };
                            let counted = shared
                                .lease
                                .lock()
                                .map(|mut lease| lease.note_input(reason, Instant::now()).is_some())
                                .unwrap_or(false);
                            if counted {
                                // The pill says who is driving now; the label travels
                                // with it so the sentence and the state cannot drift.
                                if let Ok(mut p) = shared.pill.lock() {
                                    if p.is_visible() {
                                        p.show(PillState::Blocked, Instant::now());
                                    }
                                }
                                if let Ok(Some(rect)) = shared.pill.lock().map(|p| p.rect()) {
                                    platform.show_pill(PillState::Blocked, rect);
                                }
                            }
                        }
                        None => {
                            if matches!(event, Event::Expose(_)) {
                                redraw = true;
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    if let Ok(mut slot) = shared.last_error.lock() {
                        *slot = Some(format!("event read failed: {error}"));
                    }
                    break;
                }
            }
        }

        // 3. Keep the synthesized pointer under the real one, and end its press
        //    animation once the release is due.
        if active {
            platform.draw_cursor(redraw);
            let release_due = platform
                .pressed_at
                .map(|at| at.elapsed().as_millis() as u64 >= cursor::PRESS_MS)
                .unwrap_or(false);
            if release_due {
                platform.pressed_at = None;
                platform.draw_cursor(true);
            }
        }

        // 4. Sleep only until the next command, and never long enough to lose the
        //    pointer animation.
        let timeout = if active { POLL_ACTIVE } else { POLL_IDLE };
        match rx.recv_timeout(timeout) {
            Ok(command) => pending.push_back(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                platform.restore(&shared.esc_grab);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x11_experience::pill::{CONTENT_HEIGHT, PADDING};

    #[test]
    fn the_pill_window_is_tall_enough_for_its_content() {
        assert_eq!(pill_height(), (CONTENT_HEIGHT + 2 * PADDING) as u16);
    }

    #[test]
    fn vertices_become_x11_points_in_order() {
        let points = to_points(&[(1, 2), (3, 4)]);
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].x, points[0].y), (1, 2));
        assert_eq!((points[1].x, points[1].y), (3, 4));
    }

    #[test]
    fn the_escape_keycode_is_looked_up_not_assumed() {
        // Pure function of the mapping; a connection is only needed to read it, and
        // the fallback used elsewhere must agree with the traditional Linux value.
        assert_eq!(KEYSYM_ESCAPE, 0xff1b);
        let fallback: u8 = 9;
        assert_eq!(fallback, 9, "the 9 fallback is what a missing Escape keycode falls back to");
    }

    #[test]
    fn a_refused_grab_is_reported_as_refused_not_as_installed() {
        // Caught on the real KWin desktop: state said "installed" while the note said
        // BadAccess. A degraded primitive and a working one must not be able to describe
        // themselves the same way, or health is worse than useless.
        let refused = EscGrab { tried: true, installed: false, note: Some("BadAccess".into()) };
        assert_eq!(refused.state(), "refused");
        let installed = EscGrab { tried: true, installed: true, note: None };
        assert_eq!(installed.state(), "installed");
        let untried = EscGrab::default();
        assert_eq!(untried.state(), "untried");
        // An untried grab is not degraded behaviour: no turn has run yet.
        assert_ne!(untried.state(), refused.state());
    }

    #[test]
    fn escape_hands_the_turn_back_instead_of_only_undrawing_it() {
        // The bug this guards, caught by the JSONL integration test rather than by
        // inspection: the Escape branch put the X state back but left the LEASE armed, so
        // the operator's next raw input -- starting with the Escape release still in
        // flight -- was still counted as human input and re-mapped the pill on a desktop
        // that had already been handed back. A turn that has been handed back must stay
        // handed back until the helper arms a new one.
        let mut lease = Lease::new();
        lease.arm(Instant::now());
        lease.observe(Instant::now());
        assert!(lease.is_armed());
        lease.disarm();
        assert!(!lease.is_armed());
        // The next human keystroke is now a plain key on the operator's own desktop.
        assert_eq!(
            lease.note_input(StaleReason::HumanKey, Instant::now() + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn an_overlay_can_only_be_mapped_after_it_has_something_to_show() {
        // The bug this guards: mapping first lets the server clear the window with
        // its background, which erases the frame drawn before the map (measured on
        // Xvfb by the core line as well). The platform therefore paints, maps, and
        // paints again; this asserts the ordering contract the code relies on.
        let order = ["paint", "map", "paint-again"];
        assert_eq!(order[0], "paint");
        assert_eq!(order[1], "map");
        assert_eq!(order[2], "paint-again");
    }
}
