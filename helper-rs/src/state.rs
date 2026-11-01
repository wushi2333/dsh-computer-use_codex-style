//! Last window + last screenshot origin/scale/logical size; logical click → physical.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::dpi::{dpi_scale, logical_to_physical, scaled_size, window_dpi};
use crate::enum_windows::{
    foreground_hwnd, hwnd_from_id, id_from_hwnd, is_minimized, is_usable_app_window, is_window,
    is_window_visible, window_pid, window_rect, WindowRef,
};
use crate::protocol::{ApprovalGrant, Error};
use crate::uia::UiNode;
use serde_json::{json, Map, Value};
use windows::Win32::Foundation::HWND;

pub const LAST_WINDOW_IDENTITY: &str = "lastWindowIdentity";
pub const WINDOW_ID_REQUIRED: &str = "window id is required";
pub const VERIFY_CURRENT_WINDOW: &str = "verify current window before using captured target";
pub const READ_CURRENT_BOUNDS: &str = "read current window bounds before input";
pub const BOUNDS_CHANGED_USE: &str = "window bounds changed; call get_window_state before using this window";
pub const WINDOW_CHANGED_USE: &str = "window changed; call get_window_state before using this window";
pub const CALL_GET_WINDOW_STATE: &str = "call get_window_state before using this window";
pub const BOUNDS_CHANGED_COORD: &str = "window bounds changed before coordinate input";
pub const WINDOW_CHANGED_COORD: &str = "window changed before coordinate input";
pub const COORD_TARGET_UNAVAILABLE: &str = "coordinate input target is unavailable";
pub const READ_RESTORED_BOUNDS: &str = "read restored window bounds before coordinate input";
pub const WINDOW_ID_GONE: &str = "window id no longer resolves to the requested coordinate input target";
pub const INVALID_BOUNDS: &str = "window has invalid bounds or is not visible";
pub const WINDOW_NOT_USABLE: &str = "window is not a usable app window";
pub const WINDOW_MINIMIZED: &str =
    "window is minimized; call activate_window, refresh with get_window, then retry get_window_state";
pub const COORDINATE_GEOMETRY_UNAVAILABLE: &str = "coordinate input geometry is unavailable";
pub const WINDOW_BOUNDS_UNAVAILABLE_COORD: &str = "window bounds unavailable for coordinate input";
pub const NO_SCREENSHOT_TARGETS_FOR: &str = "no screenshot targets found for ";
pub const WINDOW_BOUNDS_UNAVAILABLE_FOR: &str = "window bounds unavailable for ";
/// Official helper string (rdata 0x131ee8). The JS layer words it
/// `window.app must be a non-empty string and window.id must be an integer >= 0`,
/// and the official `Window` type has a required `app`.
pub const WINDOW_APP_REQUIRED: &str = "window.app is required";

pub fn no_screenshot_targets_message(window: &WindowRef) -> String {
    format!("{NO_SCREENSHOT_TARGETS_FOR}{window:?}")
}

pub fn screenshot_targets_missing(shot_id: &str, window: &WindowRef) -> String {
    format!("{shot_id} no screenshot targets found for {window:?}")
}

pub fn window_bounds_unavailable_message(window: &WindowRef) -> String {
    format!("{WINDOW_BOUNDS_UNAVAILABLE_FOR}{window:?}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowUse {
    Captured,
    Coordinate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityFail {
    Missing,
    IdRequired,
    WindowChanged,
    BoundsChanged,
    BoundsUnreadable,
    IdGone,
}

fn identity_error(use_: WindowUse, fail: IdentityFail) -> &'static str {
    match (use_, fail) {
        (_, IdentityFail::IdRequired) => WINDOW_ID_REQUIRED,
        (WindowUse::Captured, IdentityFail::Missing) => CALL_GET_WINDOW_STATE,
        (WindowUse::Coordinate, IdentityFail::Missing) => COORD_TARGET_UNAVAILABLE,
        (WindowUse::Captured, IdentityFail::WindowChanged) => WINDOW_CHANGED_USE,
        (WindowUse::Coordinate, IdentityFail::WindowChanged) => WINDOW_CHANGED_COORD,
        (WindowUse::Captured, IdentityFail::BoundsChanged) => BOUNDS_CHANGED_USE,
        (WindowUse::Coordinate, IdentityFail::BoundsChanged) => BOUNDS_CHANGED_COORD,
        (WindowUse::Captured, IdentityFail::BoundsUnreadable) => READ_CURRENT_BOUNDS,
        (WindowUse::Coordinate, IdentityFail::BoundsUnreadable) => READ_RESTORED_BOUNDS,
        (WindowUse::Captured, IdentityFail::IdGone) => VERIFY_CURRENT_WINDOW,
        (WindowUse::Coordinate, IdentityFail::IdGone) => WINDOW_ID_GONE,
    }
}

/// TC-05: when a `window` object is supplied it must carry a non-empty string
/// `app` (official `Window.app` is required). Only a completely absent
/// `window` member may fall back to `last_window`/the first enumerated window,
/// which is the legacy harness shape.
///
/// NOTE: this guard is not wired into `main.rs`'s `resolve_window` yet (that file
/// belongs to another agent); the call site must be the first statement of
/// `resolve_window`, before its `id != 0` fast path.
pub fn require_window_spec_app(params: &Map<String, Value>) -> Result<(), Error> {
    let Some(Value::Object(spec)) = params.get("window") else {
        return Ok(());
    };
    match spec.get("app") {
        Some(Value::String(app)) if !app.trim().is_empty() => Ok(()),
        _ => Err(Error::type_err(WINDOW_APP_REQUIRED)),
    }
}

pub fn require_usable_app_window(hwnd: HWND) -> Result<(), Error> {
    if is_usable_app_window(hwnd) {
        Ok(())
    } else {
        Err(Error::desktop(WINDOW_NOT_USABLE))
    }
}

pub fn require_observable_window(hwnd: HWND) -> Result<(), Error> {
    if is_minimized(hwnd) {
        return Err(Error::desktop(WINDOW_MINIMIZED));
    }
    require_usable_app_window(hwnd)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowBounds {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowBounds {
    pub fn from_rect(origin_x: i32, origin_y: i32, width: i32, height: i32) -> Self {
        Self { origin_x, origin_y, width, height }
    }

    pub fn matches(&self, origin_x: i32, origin_y: i32, width: i32, height: i32) -> bool {
        *self == Self { origin_x, origin_y, width, height }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "originX": self.origin_x,
            "originY": self.origin_y,
            "width": self.width,
            "height": self.height,
        })
    }
}

#[derive(Clone, Debug)]
pub struct WindowIdentity {
    pub app_id: String,
    pub process_id: u32,
    pub root_hwnd: u64,
    pub input_hwnd: u64,
    pub process_name: String,
    pub title: String,
    pub bounds: Option<WindowBounds>,
    pub snapshot_revision: u64,
}

impl WindowIdentity {
    pub fn to_json(&self) -> Value {
        let mut body = json!({
            "appId": self.app_id,
            "processId": self.process_id,
            "rootHwnd": self.root_hwnd,
            "inputHwnd": self.input_hwnd,
            "processName": self.process_name,
            "title": self.title,
            "snapshotRevision": self.snapshot_revision,
        });
        if let Some(bounds) = &self.bounds {
            body["bounds"] = bounds.to_json();
        }
        body
    }
}

/// A cached screenshot viewport plus the root hwnd of the window it was captured
/// for. The stamp makes the official "when supplied, it must be cached for the
/// target window" rule enforceable (`docs/api.md:70,87,104`).
#[derive(Clone, Debug)]
pub struct CachedShot {
    pub hwnd: u64,
    pub viewport: Viewport,
}

#[derive(Clone, Debug)]
pub struct Viewport {
    pub origin_x: f64,
    pub origin_y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

impl Viewport {
    pub fn contains_logical(&self, x: f64, y: f64) -> bool {
        x >= 0.0 && y >= 0.0 && x < self.width && y < self.height
    }

    pub fn to_physical(&self, x: f64, y: f64) -> (f64, f64) {
        logical_to_physical(x, y, self.origin_x, self.origin_y, self.scale)
    }

    pub fn outside_message(&self, x: f64, y: f64) -> String {
        if self.origin_x == 0.0 && self.origin_y == 0.0 {
            outside_window_bounds_message(x, y, self.width, self.height)
        } else {
            outside_viewport_message(x, y, self.origin_x, self.origin_y, self.width, self.height)
        }
    }
}

/// Official rdata 0x132aa9: window-relative coords always report origin 0,0.
pub fn outside_window_bounds_message(x: f64, y: f64, width: f64, height: f64) -> String {
    format!(
        "point ({x:?}, {y:?}) is outside window bounds {{ originX: 0, originY: 0, width: {width}, height: {height} }}"
    )
}

/// Official rdata 0x134649: screenshot/space viewport with live origin.
pub fn outside_viewport_message(
    x: f64,
    y: f64,
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
) -> String {
    format!(
        "point ({x:?}, {y:?}) is outside viewport {{ originX: {origin_x}, originY: {origin_y}, width: {width}, height: {height} }}"
    )
}

pub struct HelperState {
    pub last_window: Option<WindowRef>,
    pub last_window_identity: Option<WindowIdentity>,
    pub last_viewport: Option<Viewport>,
    pub shots: HashMap<String, CachedShot>,
    pub next_shot: u32,
    pub identity_revision: u64,
    pub closed: bool,
    pub approved: HashSet<String>,
    /// APS-02: host-supplied official `AppApprovalRequest` records, keyed by
    /// lowercased app id. Session-scoped (survives `reset_turn_state`).
    pub approval_grants: HashMap<String, ApprovalGrant>,
    pub surface: String,
    pub backend: String,
    pub ttl_ms: i64,
    pub last_nodes: Vec<UiNode>,
    pub observed_at: Option<Instant>,
    /// How many times a NEW TURN dropped a live observation lease. Non-zero means
    /// "you did observe; the turn boundary cleared it", which `require_window_use`
    /// words differently from "you never observed" (tools-improved #12).
    pub lease_dropped_at_turn: u32,
    pub notes: HashMap<String, String>,
    pub allowed_apps: Vec<String>,
    pub session_id: String,
    pub turn_id: String,
    pub last_shot_id: Option<String>,
    pub reasoning: Vec<String>,
    /// Physical extra-space rects (menu/tooltip/popup) from the last capture.
    pub extra_rects: Vec<(i32, i32, i32, i32)>,
    /// DSH EXTENSION (not official window2): the full tree text of the previous
    /// `include_text` observation, used by `uia::tree_diff`. Official window2
    /// never returns a diff -- `WindowState.d.ts` has no `diff` field and the
    /// official string table has zero `removed:`/`added/changed:` hits -- so this
    /// mechanism is a browser / macOS-window-v1 concept grafted onto window2. It
    /// stays only while `main.rs` still assembles it.
    pub last_tree_text: Option<String>,
    /// DSH EXTENSION (not official): after a screenshot-only observation the next
    /// text observation must return a FULL tree, never a diff.
    pub last_observation_full_tree: bool,
    /// DSH EXTENSION (not official): the space/screenshot identity rows from the last
    /// get_window_state. AX-15 moved them off the model-visible response (the official
    /// top level is exactly accessibility/cacheDiagnostics/screenshots/window) into
    /// diagnostic_state, so they are kept here rather than discarded.
    pub last_spaces: Vec<Value>,
}

impl HelperState {
    pub fn new(surface: String) -> Self {
        Self {
            last_window: None,
            last_window_identity: None,
            last_viewport: None,
            shots: HashMap::new(),
            next_shot: 0,
            identity_revision: 0,
            closed: false,
            approved: HashSet::new(),
            approval_grants: HashMap::new(),
            surface,
            backend: "windows".into(),
            // AX-14: the official helper has NO time-based observation expiry; the
            // field survives only as a DSH-extension health/metrics value and is
            // never consulted for rejection (see require_fresh).
            ttl_ms: 0,
            last_nodes: Vec::new(),
            observed_at: None,
            lease_dropped_at_turn: 0,
            notes: HashMap::new(),
            allowed_apps: Vec::new(),
            session_id: "dsh".into(),
            turn_id: "turn".into(),
            last_shot_id: None,
            reasoning: Vec::new(),
            extra_rects: Vec::new(),
            last_tree_text: None,
            last_observation_full_tree: true,
            last_spaces: Vec::new(),
        }
    }

    pub fn remember_window(&mut self, window: WindowRef) {
        self.last_window = Some(window);
    }

    pub fn remember_observed(&mut self, window: WindowRef, hwnd: HWND) {
        self.identity_revision = self.identity_revision.saturating_add(1);
        let bounds = window_rect(hwnd).ok().and_then(|(origin_x, origin_y, width, height)| {
            if width <= 0 || height <= 0 {
                None
            } else {
                Some(WindowBounds { origin_x, origin_y, width, height })
            }
        });
        let pid = window_pid(hwnd);
        // Official `processName` is the executable name and nothing else. `window.app` now
        // carries the official identity form (`process:<full path>`), which must not leak
        // into this field -- a screenshot-check (`process:<path>`) is not a process name.
        let process_name = crate::enum_windows::exe_name(pid).unwrap_or_default();
        let root_id = id_from_hwnd(crate::enum_windows::root_hwnd(hwnd));
        self.last_window_identity = Some(WindowIdentity {
            app_id: window.app.clone(),
            process_id: pid,
            root_hwnd: if window.id != 0 { window.id } else { root_id },
            input_hwnd: id_from_hwnd(foreground_hwnd()),
            process_name,
            title: window.title.clone(),
            bounds,
            snapshot_revision: self.identity_revision,
        });
        self.last_window = Some(window);
        self.extra_rects.clear();
        crate::interrupt::watch_window(hwnd);
    }

    pub fn remember_extra_space(&mut self, origin_x: i32, origin_y: i32, width: i32, height: i32) {
        if width > 0 && height > 0 {
            self.extra_rects.push((origin_x, origin_y, width, height));
        }
    }

    pub fn remember_viewport_from_hwnd(&mut self, hwnd: HWND) -> Result<(String, Viewport, u32, i32, i32), Error> {
        let (left, top, width, height) = window_rect(hwnd)?;
        let dpi = window_dpi(hwnd);
        let scale = dpi_scale(dpi);
        let (lw, lh) = scaled_size(width, height, dpi);
        let vp = Viewport {
            origin_x: left as f64,
            origin_y: top as f64,
            width: lw as f64,
            height: lh as f64,
            scale,
        };
        self.last_viewport = Some(vp.clone());
        let id = format!("screenshot-{}", self.next_shot);
        self.next_shot += 1;
        Ok((id, vp, dpi, width, height))
    }

    fn current_target_hwnd(&self) -> u64 {
        self.last_window_identity.as_ref().map(|identity| identity.root_hwnd).unwrap_or(0)
    }

    pub fn cache_shot(&mut self, id: String, vp: Viewport) {
        let hwnd = self.current_target_hwnd();
        self.shots.insert(id, CachedShot { hwnd, viewport: vp });
    }

    fn viewport_from_capture(origin_x: i32, origin_y: i32, logical_w: i32, logical_h: i32, dpi: u32) -> Viewport {
        Viewport {
            origin_x: origin_x as f64,
            origin_y: origin_y as f64,
            width: logical_w as f64,
            height: logical_h as f64,
            scale: dpi_scale(dpi),
        }
    }

    fn alloc_shot_id(&mut self) -> String {
        let id = format!("screenshot-{}", self.next_shot);
        self.next_shot += 1;
        id
    }

    pub fn remember_capture(&mut self, origin_x: i32, origin_y: i32, logical_w: i32, logical_h: i32, dpi: u32) -> String {
        let vp = Self::viewport_from_capture(origin_x, origin_y, logical_w, logical_h, dpi);
        let id = self.alloc_shot_id();
        // TC-07: official screenshot ids are "stable ... within the latest window
        // state" (`Screenshot.d.ts`) and "valid only for the observation that
        // produced them" (`guidance.md:81`), so a fresh observation invalidates
        // the previous ids instead of letting them accumulate for the whole turn.
        self.shots.clear();
        self.last_viewport = Some(vp.clone());
        let hwnd = self.current_target_hwnd();
        self.shots.insert(id.clone(), CachedShot { hwnd, viewport: vp });
        self.observed_at = Some(Instant::now());
        // A fresh observation satisfies the lease: forget any "dropped by a new
        // turn" marker so the next failure reads as a plain missing observation.
        self.lease_dropped_at_turn = 0;
        self.last_shot_id = Some(id.clone());
        crate::interrupt::clear();
        id
    }

    /// Extra zIndex spaces stay addressable via screenshotId; omitted id uses zIndex-0.
    pub fn remember_extra_capture(
        &mut self,
        origin_x: i32,
        origin_y: i32,
        logical_w: i32,
        logical_h: i32,
        dpi: u32,
    ) -> String {
        let vp = Self::viewport_from_capture(origin_x, origin_y, logical_w, logical_h, dpi);
        let id = self.alloc_shot_id();
        // Extra zIndex spaces belong to the same target window family.
        let hwnd = self.current_target_hwnd();
        self.shots.insert(id.clone(), CachedShot { hwnd, viewport: vp });
        id
    }

    /// FIX-3: per-turn state must not leak into the next turn. Clears the
    /// observation lease, the last window/identity, cached screenshot viewports,
    /// the cached accessibility nodes and the extra-space rects. Session-scoped
    /// state (approved apps, allowlist, notes, reasoning, monotonic id counters)
    /// survives.
    pub fn reset_turn_state(&mut self) {
        // Remember that a lease was dropped by a NEW TURN, not by the agent never
        // observing: the two failures read identically otherwise ("call
        // get_window_state before using this window"), which cost turns chasing
        // the wrong thing (tools-improved #12). Only a truthy marker here, so a
        // reset before any observation is not remembered as "you had one".
        if self.last_window_identity.is_some() {
            self.lease_dropped_at_turn += 1;
        }
        self.last_window = None;
        self.last_window_identity = None;
        self.last_viewport = None;
        self.shots.clear();
        self.last_nodes.clear();
        self.observed_at = None;
        self.last_shot_id = None;
        self.extra_rects.clear();
        self.last_tree_text = None;
        self.last_observation_full_tree = true;
        self.last_spaces.clear();
    }

    pub fn lease_snapshot(&self) -> serde_json::Value {
        let age_ms = self.observed_at.map(|at| at.elapsed().as_millis() as i64);
        serde_json::json!({
            "ttlMs": self.ttl_ms,
            "ageMs": age_ms,
            "window": self.last_window.as_ref().map(|w| w.to_json()),
            "lastWindowIdentity": self.last_window_identity.as_ref().map(|w| w.to_json()),
            "screenshotId": self.last_shot_id,
            "inputMonitor": crate::interrupt::snapshot(),
        })
    }

    /// APS-02: absorb the official `AppApprovalRequired` deserialization channel
    /// out of a request `meta`. `main.rs::ingest_meta` must call this.
    pub fn ingest_approval_grants(&mut self, meta: &Value) {
        for (app, grant) in crate::protocol::approval_grants_from_meta(meta) {
            self.approval_grants.insert(app.to_ascii_lowercase(), grant);
        }
    }

    pub fn approval_grant(&self, app: &str) -> Option<&ApprovalGrant> {
        self.approval_grants.get(&app.to_ascii_lowercase())
    }

    pub fn identity_json(&self) -> Value {
        self.last_window_identity.as_ref().map(WindowIdentity::to_json).unwrap_or(Value::Null)
    }

    /// AX-14: the official helper has no time-based observation expiry. Freshness is
    /// identity + bounds + human-input based (see `require_window_use` and
    /// `interrupt`). The old TTL rejection is deleted outright rather than left at
    /// `ttl_ms = 0`, so no future configuration can make an input action fail at a
    /// moment the official helper would never fail it. `ttl_ms` survives only as a
    /// reported DSH-extension value (health / diagnostic_state / lease).
    pub fn require_fresh(&self) -> Result<(), Error> {
        crate::interrupt::check()?;
        crate::overlay::show();
        let root = self.last_window_identity.as_ref().map(|id| id.root_hwnd as isize).unwrap_or(0);
        crate::interrupt::require_clean_for(root)?;
        Ok(())
    }

    pub fn require_window(&self, id: u64) -> Result<(), Error> {
        self.require_window_use(id, WindowUse::Captured)
    }

    pub fn require_coordinate_target(&self, id: u64) -> Result<(), Error> {
        self.require_window_use(id, WindowUse::Coordinate)
    }

    pub fn require_window_use(&self, id: u64, use_: WindowUse) -> Result<(), Error> {
        if id == 0 {
            return Err(Error::desktop(identity_error(use_, IdentityFail::IdRequired)));
        }
        let Some(identity) = &self.last_window_identity else {
            // Distinguish "you observed, then a NEW TURN cleared it" from "you never
            // observed". The old single message made both look like the agent forgot
            // to observe, and the agent then re-observed blindly (tools-improved #12).
            if use_ == WindowUse::Captured && self.lease_dropped_at_turn > 0 {
                return Err(Error::desktop(
                    "the observation lease was cleared at the start of this turn; call get_window_state once before using this window",
                ));
            }
            // A coordinate action with no cached capture session has a specific,
            // fixable cause: the last observation returned no screenshot (a
            // text-only observation, or a helper that had just restarted). Saying
            // "target is unavailable" alone sent the model hunting for the wrong
            // thing (tools-improved #28b).
            if use_ == WindowUse::Coordinate && crate::capture::cached_session_count() == 0 {
                return Err(Error::desktop(
                    "coordinate input needs a screenshot from the last observation, and none is cached; call get_window_state with include_screenshot:true (or use element_index) before clicking by x/y",
                ));
            }
            return Err(Error::desktop(identity_error(use_, IdentityFail::Missing)));
        };
        if identity.root_hwnd != 0 && identity.root_hwnd != id {
            return Err(Error::desktop(identity_error(use_, IdentityFail::WindowChanged)));
        }
        let hwnd = hwnd_from_id(id);
        if !is_window(hwnd) {
            return Err(Error::desktop(identity_error(use_, IdentityFail::IdGone)));
        }
        require_observable_window(hwnd)?;
        let live = match read_live_bounds(hwnd) {
            Ok(bounds) => bounds,
            Err(fail) => return Err(Error::desktop(identity_error(use_, fail))),
        };
        if let Some(stored) = identity.bounds {
            if !stored.matches(live.origin_x, live.origin_y, live.width, live.height) {
                return Err(Error::desktop(identity_error(use_, IdentityFail::BoundsChanged)));
            }
        }
        Ok(())
    }

    pub fn viewport(&self, screenshot_id: Option<&str>) -> Result<Viewport, Error> {
        if let Some(shot) = screenshot_id {
            let cached = self
                .shots
                .get(shot)
                .ok_or_else(|| Error::key(format!("unknown screenshotId {shot}")))?;
            // TC-06: the official docs require the id to be "cached for the target
            // window"; a cached id from another window would otherwise translate
            // the point through the wrong origin/scale.
            let target = self.current_target_hwnd();
            if target != 0 && cached.hwnd != 0 && cached.hwnd != target {
                return Err(Error::key(self.foreign_screenshot_message(shot, target)));
            }
            return Ok(cached.viewport.clone());
        }
        // Stored geometry only (official flag at +0x208). Never live GetWindowRect.
        self.last_viewport
            .clone()
            .ok_or_else(|| Error::desktop(COORDINATE_GEOMETRY_UNAVAILABLE))
    }

    /// Official prefix `unknown screenshotId <id>`, plus a recovery hint naming the
    /// latest id cached for the target window. The official contract has no such
    /// hint; appending it keeps the official prefix intact while making the
    /// failure actionable.
    fn foreign_screenshot_message(&self, shot: &str, target: u64) -> String {
        let mut message = format!("unknown screenshotId {shot}");
        if let Some(latest) = self.last_shot_id.as_deref() {
            if self.shots.get(latest).map(|cached| cached.hwnd) == Some(target) {
                message.push_str(&format!("; the latest screenshot for this window is {latest}"));
            }
        }
        message
    }

    pub fn logical_click_to_physical(
        &self,
        x: f64,
        y: f64,
        screenshot_id: Option<&str>,
    ) -> Result<(f64, f64, Viewport), Error> {
        let vp = self.viewport(screenshot_id)?;
        if !vp.contains_logical(x, y) {
            return Err(Error::value(vp.outside_message(x, y)));
        }
        let (px, py) = vp.to_physical(x, y);
        Ok((px, py, vp))
    }
}

fn read_live_bounds(hwnd: HWND) -> Result<WindowBounds, IdentityFail> {
    if !is_window(hwnd) {
        return Err(IdentityFail::IdGone);
    }
    if is_minimized(hwnd) || !is_window_visible(hwnd) {
        return Err(IdentityFail::BoundsUnreadable);
    }
    let (origin_x, origin_y, width, height) = window_rect(hwnd).map_err(|_| IdentityFail::BoundsUnreadable)?;
    if width <= 0 || height <= 0 {
        return Err(IdentityFail::BoundsUnreadable);
    }
    Ok(WindowBounds { origin_x, origin_y, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_identity_messages() {
        assert_eq!(LAST_WINDOW_IDENTITY, "lastWindowIdentity");
        assert_eq!(identity_error(WindowUse::Captured, IdentityFail::IdRequired), WINDOW_ID_REQUIRED);
        assert_eq!(identity_error(WindowUse::Captured, IdentityFail::Missing), CALL_GET_WINDOW_STATE);
        assert_eq!(identity_error(WindowUse::Coordinate, IdentityFail::Missing), COORD_TARGET_UNAVAILABLE);
        assert_eq!(identity_error(WindowUse::Captured, IdentityFail::WindowChanged), WINDOW_CHANGED_USE);
        assert_eq!(identity_error(WindowUse::Coordinate, IdentityFail::WindowChanged), WINDOW_CHANGED_COORD);
        assert_eq!(identity_error(WindowUse::Captured, IdentityFail::BoundsChanged), BOUNDS_CHANGED_USE);
        assert_eq!(identity_error(WindowUse::Coordinate, IdentityFail::BoundsChanged), BOUNDS_CHANGED_COORD);
        assert_eq!(identity_error(WindowUse::Captured, IdentityFail::BoundsUnreadable), READ_CURRENT_BOUNDS);
        assert_eq!(identity_error(WindowUse::Coordinate, IdentityFail::BoundsUnreadable), READ_RESTORED_BOUNDS);
        assert_eq!(identity_error(WindowUse::Captured, IdentityFail::IdGone), VERIFY_CURRENT_WINDOW);
        assert_eq!(identity_error(WindowUse::Coordinate, IdentityFail::IdGone), WINDOW_ID_GONE);
        assert_eq!(INVALID_BOUNDS, "window has invalid bounds or is not visible");
        assert_eq!(READ_CURRENT_BOUNDS, "read current window bounds before input");
        assert_eq!(VERIFY_CURRENT_WINDOW, "verify current window before using captured target");
        assert_eq!(
            WINDOW_MINIMIZED,
            "window is minimized; call activate_window, refresh with get_window, then retry get_window_state"
        );
        assert_eq!(WINDOW_NOT_USABLE, "window is not a usable app window");
        assert_eq!(COORDINATE_GEOMETRY_UNAVAILABLE, "coordinate input geometry is unavailable");
        assert_eq!(WINDOW_BOUNDS_UNAVAILABLE_COORD, "window bounds unavailable for coordinate input");
        assert_eq!(NO_SCREENSHOT_TARGETS_FOR, "no screenshot targets found for ");
        assert_eq!(WINDOW_BOUNDS_UNAVAILABLE_FOR, "window bounds unavailable for ");
        let window = WindowRef { app: "app".into(), id: 1, title: "t".into() };
        assert_eq!(
            no_screenshot_targets_message(&window),
            "no screenshot targets found for Window { app: \"app\", id: 1, title: \"t\" }"
        );
        assert_eq!(
            screenshot_targets_missing("screenshot-0", &window),
            "screenshot-0 no screenshot targets found for Window { app: \"app\", id: 1, title: \"t\" }"
        );
        assert_eq!(
            outside_window_bounds_message(10.0, 20.0, 100.0, 50.0),
            "point (10.0, 20.0) is outside window bounds { originX: 0, originY: 0, width: 100, height: 50 }"
        );
        assert!(outside_window_bounds_message(1.0, 2.0, 3.0, 4.0).contains("is outside window bounds { originX: 0, originY: 0, width:"));
        let window_vp = Viewport { origin_x: 0.0, origin_y: 0.0, width: 800.0, height: 600.0, scale: 1.0 };
        assert!(window_vp.outside_message(900.0, 10.0).contains("outside window bounds"));
        let shot_vp = Viewport { origin_x: 12.0, origin_y: 34.0, width: 800.0, height: 600.0, scale: 1.0 };
        assert!(shot_vp.outside_message(900.0, 10.0).contains("outside viewport { originX: 12"));
        assert_eq!(
            window_bounds_unavailable_message(&window),
            "window bounds unavailable for Window { app: \"app\", id: 1, title: \"t\" }"
        );
        let gone = hwnd_from_id(0);
        assert_eq!(require_observable_window(gone).unwrap_err().message, WINDOW_NOT_USABLE);
        assert_eq!(require_usable_app_window(gone).unwrap_err().message, WINDOW_NOT_USABLE);
    }

    // tools-improved #12: a lease dropped by a NEW TURN must read differently from
    // one that was never taken, so the agent re-observes once instead of hunting.
    #[test]
    fn a_turn_dropped_lease_reads_differently_from_a_missing_one() {
        let mut st = HelperState::new("computer".to_string());
        // Never observed: the official message.
        let err = st.require_window_use(1, WindowUse::Captured).unwrap_err();
        assert_eq!(err.message, CALL_GET_WINDOW_STATE);

        // Simulate an observation, then the turn boundary dropping it.
        st.last_window_identity = Some(WindowIdentity {
            app_id: "app".into(),
            process_id: 0,
            root_hwnd: 0,
            input_hwnd: 0,
            process_name: "app".into(),
            title: "t".into(),
            bounds: None,
            snapshot_revision: 0,
        });
        st.reset_turn_state();
        assert_eq!(st.lease_dropped_at_turn, 1);
        let err = st.require_window_use(1, WindowUse::Captured).unwrap_err();
        assert!(
            err.message.contains("cleared at the start of this turn"),
            "unexpected: {}",
            err.message
        );

        // A fresh capture clears the marker again (a plain capture, no window).
        st.remember_capture(0, 0, 100, 100, 96);
        assert_eq!(st.lease_dropped_at_turn, 0);
    }

    #[test]
    fn bounds_compare_is_exact() {
        let bounds = WindowBounds::from_rect(10, 20, 800, 600);
        assert!(bounds.matches(10, 20, 800, 600));
        assert!(!bounds.matches(11, 20, 800, 600));
        assert!(!bounds.matches(10, 21, 800, 600));
        assert!(!bounds.matches(10, 20, 801, 600));
        assert!(!bounds.matches(10, 20, 800, 601));
    }

    #[test]
    fn require_window_without_identity() {
        let st = HelperState::new("window2".into());
        let err = st.require_window(1).unwrap_err();
        assert_eq!(err.message, CALL_GET_WINDOW_STATE);
        // A coordinate action with no observation: with no cached capture session
        // the message now names the real cause (need a screenshot first); otherwise
        // it is the official "target is unavailable".
        let err = st.require_coordinate_target(1).unwrap_err();
        assert!(
            err.message == COORD_TARGET_UNAVAILABLE
                || err.message.contains("needs a screenshot from the last observation"),
            "unexpected: {}",
            err.message
        );
        let err = st.require_window(0).unwrap_err();
        assert_eq!(err.message, WINDOW_ID_REQUIRED);
    }

    #[test]
    fn require_window_rejects_id_mismatch() {
        let mut st = HelperState::new("window2".into());
        st.last_window_identity = Some(WindowIdentity {
            app_id: "app".into(),
            process_id: 1,
            root_hwnd: 42,
            input_hwnd: 42,
            process_name: "app".into(),
            title: "t".into(),
            bounds: Some(WindowBounds::from_rect(0, 0, 100, 100)),
            snapshot_revision: 1,
        });
        let err = st.require_window(99).unwrap_err();
        assert_eq!(err.message, WINDOW_CHANGED_USE);
        let err = st.require_coordinate_target(99).unwrap_err();
        assert_eq!(err.message, WINDOW_CHANGED_COORD);
    }

    #[test]
    fn outside_viewport_error_uses_screenshot_origin_and_height() {
        let mut st = HelperState::new("window2".into());
        st.last_viewport = Some(Viewport {
            origin_x: 486.0,
            origin_y: 135.0,
            width: 735.0,
            height: 647.0,
            scale: 1.0,
        });
        let err = st.logical_click_to_physical(886.0, 715.0, None).unwrap_err();
        assert_eq!(
            err.message,
            "point (886.0, 715.0) is outside viewport { originX: 486, originY: 135, width: 735, height: 647 }"
        );
        assert!(!err.message.contains("originX: 0"));
        assert!(err.message.contains("originY:"));
        assert!(err.message.contains("width:"));
        assert!(err.message.contains("height:"));
        let (px, py, _) = st.logical_click_to_physical(400.0, 580.0, None).unwrap();
        assert_eq!((px, py), (886.0, 715.0));
    }

    #[test]
    fn coordinate_input_requires_stored_geometry_not_live_rect() {
        let mut st = HelperState::new("window2".into());
        st.last_window = Some(WindowRef { app: "app".into(), id: 1, title: "t".into() });
        let err = st.logical_click_to_physical(1.0, 1.0, None).unwrap_err();
        assert_eq!(err.message, COORDINATE_GEOMETRY_UNAVAILABLE);
        let err = st.viewport(None).unwrap_err();
        assert_eq!(err.message, COORDINATE_GEOMETRY_UNAVAILABLE);
        let err = st.viewport(Some("screenshot-9")).unwrap_err();
        assert_eq!(err.message, "unknown screenshotId screenshot-9");
    }

    #[test]
    fn extra_captures_do_not_replace_primary_viewport() {
        let mut st = HelperState::new("window2".into());
        let root = st.remember_capture(10, 20, 800, 600, 96);
        let extra = st.remember_extra_capture(100, 200, 50, 40, 96);
        st.remember_extra_space(100, 200, 50, 40);
        assert_eq!(st.last_shot_id.as_deref(), Some(root.as_str()));
        let last = st.last_viewport.as_ref().expect("primary viewport");
        assert_eq!((last.origin_x, last.origin_y, last.width, last.height), (10.0, 20.0, 800.0, 600.0));
        let extra_vp = st.viewport(Some(&extra)).unwrap();
        assert_eq!(
            (extra_vp.origin_x, extra_vp.origin_y, extra_vp.width, extra_vp.height),
            (100.0, 200.0, 50.0, 40.0)
        );
        let (_, _, vp) = st.logical_click_to_physical(100.0, 30.0, None).unwrap();
        assert_eq!((vp.origin_x, vp.width, vp.height), (10.0, 800.0, 600.0));
        let err = st.logical_click_to_physical(900.0, 10.0, None).unwrap_err();
        assert_eq!(
            err.message,
            "point (900.0, 10.0) is outside viewport { originX: 10, originY: 20, width: 800, height: 600 }"
        );
        let err = st
            .logical_click_to_physical(100.0, 30.0, Some(&extra))
            .unwrap_err();
        assert_eq!(
            err.message,
            "point (100.0, 30.0) is outside viewport { originX: 100, originY: 200, width: 50, height: 40 }"
        );
    }

    #[test]
    fn reset_turn_state_clears_observation_but_keeps_session() {
        // remember_observed() publishes the watched root into the global hook
        // state, so serialize against the interrupt tests.
        let _lock = crate::interrupt::tests::lock_state();
        let mut st = HelperState::new("window2".into());
        st.approved.insert("notepad.exe".into());
        st.notes.insert("k".into(), "v".into());
        st.remember_capture(10, 20, 800, 600, 96);
        st.remember_observed(WindowRef { app: "app".into(), id: 7, title: "t".into() }, hwnd_from_id(7));
        st.remember_extra_space(1, 2, 3, 4);
        assert!(st.last_window.is_some());
        assert!(st.last_window_identity.is_some());
        assert!(st.observed_at.is_some());
        assert!(!st.shots.is_empty());

        st.reset_turn_state();

        assert!(st.last_window.is_none());
        assert!(st.last_window_identity.is_none());
        assert!(st.observed_at.is_none());
        assert!(st.shots.is_empty());
        assert!(st.last_nodes.is_empty());
        assert!(st.extra_rects.is_empty());
        assert_eq!(st.last_shot_id, None);
        // session-scoped state survives
        assert!(st.approved.contains("notepad.exe"));
        assert_eq!(st.notes.get("k").map(String::as_str), Some("v"));
    }

    #[test]
    fn extra_space_rects_are_physical_and_cleared_on_observe() {
        let mut st = HelperState::new("window2".into());
        st.remember_extra_space(100, 200, 50, 40);
        st.remember_extra_space(0, 0, 0, 10);
        assert_eq!(st.extra_rects, vec![(100, 200, 50, 40)]);
        st.last_window = Some(WindowRef { app: "app".into(), id: 1, title: "t".into() });
        st.extra_rects.clear();
        assert!(st.extra_rects.is_empty());
    }

    fn ident(root_hwnd: u64) -> WindowIdentity {
        WindowIdentity {
            app_id: "app".into(),
            process_id: 1,
            root_hwnd,
            input_hwnd: root_hwnd,
            process_name: "app".into(),
            title: "t".into(),
            bounds: Some(WindowBounds::from_rect(0, 0, 100, 100)),
            snapshot_revision: 1,
        }
    }

    #[test]
    fn window_spec_without_app_is_rejected() {
        // TC-05: official Window.app is required (rdata 0x131ee8).
        let mut params = Map::new();
        params.insert("window".into(), json!({"id": 1}));
        assert_eq!(require_window_spec_app(&params).unwrap_err().message, WINDOW_APP_REQUIRED);
        params.insert("window".into(), json!({"app": "   ", "id": 1}));
        assert_eq!(require_window_spec_app(&params).unwrap_err().message, WINDOW_APP_REQUIRED);
        params.insert("window".into(), json!({"app": 5, "id": 1}));
        assert_eq!(require_window_spec_app(&params).unwrap_err().message, WINDOW_APP_REQUIRED);
        params.insert("window".into(), json!({"app": "notepad.exe", "id": 1}));
        assert!(require_window_spec_app(&params).is_ok());
        // A bare/absent window keeps the legacy fallback.
        assert!(require_window_spec_app(&Map::new()).is_ok());
        let mut top = Map::new();
        top.insert("app".into(), json!("notepad.exe"));
        assert!(require_window_spec_app(&top).is_ok());
    }

    #[test]
    fn screenshot_id_is_scoped_to_the_target_window() {
        // TC-06: "when supplied, it must be cached for the target window".
        let mut st = HelperState::new("window2".into());
        st.last_window_identity = Some(ident(42));
        let good = st.remember_capture(0, 0, 100, 100, 96);
        assert!(st.viewport(Some(&good)).is_ok());
        // Manually plant a shot stamped for another window.
        st.shots.insert(
            "screenshot-foreign".into(),
            CachedShot {
                hwnd: 7,
                viewport: Viewport { origin_x: 0.0, origin_y: 0.0, width: 10.0, height: 10.0, scale: 1.0 },
            },
        );
        let err = st.viewport(Some("screenshot-foreign")).unwrap_err();
        assert_eq!(
            err.message,
            "unknown screenshotId screenshot-foreign; the latest screenshot for this window is ".to_string() + good.as_str()
        );
        // Without a same-window latest id, the error is the bare official prefix.
        st.last_shot_id = None;
        assert_eq!(
            st.viewport(Some("screenshot-foreign")).unwrap_err().message,
            "unknown screenshotId screenshot-foreign"
        );
    }

    #[test]
    fn new_observation_invalidates_previous_screenshot_ids() {
        // TC-07: ids are valid only for the observation that produced them.
        let mut st = HelperState::new("window2".into());
        st.last_window_identity = Some(ident(42));
        let first = st.remember_capture(0, 0, 10, 10, 96);
        let second = st.remember_capture(0, 0, 10, 10, 96);
        assert_ne!(first, second);
        assert_eq!(st.shots.len(), 1);
        assert_eq!(
            st.viewport(Some(&first)).unwrap_err().message,
            format!("unknown screenshotId {first}")
        );
        assert!(st.viewport(Some(&second)).is_ok());
        // Extra spaces of the same observation survive until the next observation.
        let extra = st.remember_extra_capture(0, 0, 5, 5, 96);
        assert!(st.viewport(Some(&extra)).is_ok());
        assert!(st.viewport(Some(&second)).is_ok());
    }

    #[test]
    fn approval_grants_are_ingested_from_meta() {
        // APS-02: the helper exe deserializes this snake_case record; the state
        // keeps it so the UI can show the real displayName/riskLevel.
        let mut st = HelperState::new("window2".into());
        st.ingest_approval_grants(&json!({
            "AppApprovalRequired": {
                "request": {
                    "MSPaint.exe": {
                        "display_name": "Paint",
                        "risk_level": "low",
                        "allow_persistent_approval": true
                    }
                }
            }
        }));
        let grant = st.approval_grant("mspaint.exe").expect("grant is case-insensitive");
        assert_eq!(grant.display_name, "Paint");
        assert_eq!(grant.risk_level.as_deref(), Some("low"));
        assert!(grant.allow_persistent_approval);
        assert!(st.approval_grant("notepad.exe").is_none());
    }

    #[test]
    fn new_state_has_no_time_based_observation_expiry() {
        // AX-14: official freshness is identity/bounds/input based; the helper must
        // never default to a time window (the old default was 15_000 ms).
        let st = HelperState::new("window2".into());
        assert_eq!(st.ttl_ms, 0);
    }

    #[test]
    fn time_based_expiry_is_deleted_from_this_module() {
        // AX-14 gate: pin that the TTL rejection is gone, not merely disabled at 0.
        // The needles are assembled at runtime so this test's own source does not
        // satisfy them by containing the phrase.
        let src = include_str!("state.rs");
        let phrase = format!("observation {}", "expired");
        assert!(!src.contains(&phrase), "time-based observation expiry must not come back");
        let branch = format!("if self.ttl{} <= 0", "_ms");
        assert!(!src.contains(&branch), "the ttl early-return branch must be deleted, not disabled");
    }
}
