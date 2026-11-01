//! Esc-to-cancel + human input monitor.
//!
//! INT-2: the official interrupt monitor installs its low-level hooks from a resident
//! thread (GH:14005e822) and never removes them while the process lives. DSH used to
//! install them only after `arm()` and unhook on `disarm()`, so a physical Escape
//! between two tool calls did nothing. The hooks are now resident; `ARMED` only decides
//! whether non-Escape human input is recorded as dirty and whether an Escape is swallowed.
//!
//! TURN-3: the helper also waits on the official
//! `Local\CodexComputerUseTurnEnded-<key>` event, which is the external stop-this-turn
//! channel the official exposes (S:18365).

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, HWND, LPARAM, LRESULT, POINT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentThreadId, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetAncestor, GetForegroundWindow, GetMessageW,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WindowFromPoint,
    GA_ROOT, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT,
    LLMHF_INJECTED, MSLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOWS_HOOK_ID, WM_KEYDOWN,
    WM_QUIT,
};

use crate::protocol::Error;

pub const USER_INPUT_MESSAGE: &str =
    "user input was detected in this window; call get_window_state before continuing";
pub const MONITOR_UNAVAILABLE: &str =
    "user input monitor unavailable; guarded input cannot continue";
pub const MONITOR_START_UNAVAILABLE: &str =
    "computer-use user interruption monitor unavailable: ";
const HOOK_KEYBOARD: &str = "failed to install keyboard hook";
const HOOK_MOUSE: &str = "failed to install mouse hook";
const HOOK_STATE_POISONED: &str = "hook state mutex poisoned";
const VK_ESCAPE: u32 = 0x1B;
const WM_APP_ARM: u32 = 0x8001;

static STOPPED: AtomicBool = AtomicBool::new(false);
static ENDED: AtomicBool = AtomicBool::new(false);
static ARMED: AtomicBool = AtomicBool::new(false);
static DIRTY: AtomicBool = AtomicBool::new(false);
static AVAILABLE: AtomicBool = AtomicBool::new(true);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// `dwExtraInfo` stamped on every event this process injects, so the low-level hooks
/// can tell our own synthetic input apart from the human's.
///
/// The `LLMHF_INJECTED` / `LLKHF_INJECTED` flags cannot be used for that: remote
/// desktop and VM-console input stacks set them on the *human's* mouse events, and
/// filtering those out meant a real user grabbing the mouse was never noticed
/// (`hooks[downs=19 injected=19]` while the generation stayed at 0).
pub const SYNTHETIC_TAG: usize = 0x4453_4855_0000_0001;
/// Raw low-level-hook callbacks seen, before any filtering. A zero count while the
/// operator is clicking proves the callback never ran (another hook swallowing the
/// event upstream, or a hook that Windows silently dropped) rather than an
/// over-strict filter inside our own callback.
static MOUSE_EVENTS: AtomicU64 = AtomicU64::new(0);
static KEY_EVENTS: AtomicU64 = AtomicU64::new(0);
static MOUSE_DOWNS: AtomicU64 = AtomicU64::new(0);
/// Button-downs that carried `LLMHF_INJECTED`. Remote-desktop and VM-console input
/// stacks mark their events this way, which makes a real human click look synthetic.
static MOUSE_DOWNS_INJECTED: AtomicU64 = AtomicU64::new(0);
static ARM_GEN: AtomicU64 = AtomicU64::new(0);
static DONE_GEN: AtomicU64 = AtomicU64::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static TARGET_ROOT: AtomicIsize = AtomicIsize::new(0);
static STARTED: OnceLock<()> = OnceLock::new();
static REASON: Mutex<String> = Mutex::new(String::new());
static INSTALL_ERROR: Mutex<String> = Mutex::new(String::new());
static DIRTY_ROOTS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static SYNTHETIC_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static ARM_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// Only guards the keystroke that *caused* the overlay to appear, plus the tags around our
/// own injections. The official has no grace at all; the DSH value used to be 1.5 s, which
/// swallowed a real Escape pressed right after arming.
const ESC_ARM_GRACE: Duration = Duration::from_millis(200);

/// Official event-name prefix (S:18365: `Local\CodexComputerUseTurnEnded-`). TURN-4: the
/// DSH-only `Local\DshComputerUseTurnEnded-` name is gone; only this prefix is used.
pub const TURN_ENDED_EVENT_PREFIX: &str = "Local\\CodexComputerUseTurnEnded-";
/// Official fragments (S:18366, S:18370, S:18371).
pub const TURN_ENDED_WATCHER_STOPPED: &str = "-computer-use turn-end event watcher stopped: ";
pub const TURN_ENDED_WATCHER_UNAVAILABLE: &str = "-computer-use turn-ended watcher unavailable: ";
pub const TURN_ENDED_WAIT_FAILED: &str = "wait for computer-use turn-ended event failed";
pub const SIGNAL_TURN_ENDED: &str = "signal computer-use turn-ended event";
pub const CREATE_TURN_ENDED_EVENT: &str = "create computer-use turn-ended event";

fn hwnd_id(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

fn root_hwnd(hwnd: HWND) -> HWND {
    if hwnd.0.is_null() {
        return hwnd;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if root.0.is_null() {
        hwnd
    } else {
        root
    }
}

/// Listing 14005ede5: GetAncestor(GA_ROOT=2), fall back to the hwnd itself.
fn root_from_point(x: i32, y: i32) -> HWND {
    let hit = unsafe { WindowFromPoint(POINT { x, y }) };
    let hwnd = if hit.0.is_null() {
        unsafe { GetForegroundWindow() }
    } else {
        hit
    };
    root_hwnd(hwnd)
}

pub fn watch_window(hwnd: HWND) {
    TARGET_ROOT.store(hwnd_id(root_hwnd(hwnd)), Ordering::SeqCst);
}

pub fn stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

pub fn ended() -> bool {
    ENDED.load(Ordering::SeqCst)
}

pub fn check() -> Result<(), Error> {
    if ENDED.load(Ordering::SeqCst) {
        return Err(Error::turn_ended());
    }
    if STOPPED.load(Ordering::SeqCst) {
        return Err(Error::interrupt());
    }
    Ok(())
}

pub fn trip() {
    STOPPED.store(true, Ordering::SeqCst);
    crate::overlay::hide();
}

/// FIX-3: the helper must be able to serve more than one turn in a process
/// lifetime. `end_turn` latches ENDED and sets this flag; the serve loop then
/// clears the per-turn state before the next request is dispatched.
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn request_restart() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn take_restart() -> bool {
    RESTART_REQUESTED.swap(false, Ordering::SeqCst)
}

/// Official src/input/interruption.rs: a physical Escape while the overlay is
/// armed is swallowed by the hook, an interrupt marker is written under
/// `<home>/cache/computer-use/interrupts/<conversation>/<turn>`, and the helper
/// terminates with exit status 130. The transport observes 130 (or the marker)
/// and refuses every later request in that turn.
///
/// `source` distinguishes the low-level-hook path (physical key) from the
/// sidecar RPC path. Both write the marker; only the physical path exits, which
/// mirrors the official binary where the hook callback is the sole `exit(130)`
/// site.
pub fn mark_interrupted(source: &str) -> i32 {
    STOPPED.store(true, Ordering::SeqCst);
    crate::overlay::hide();
    let (session, turn) = match SESSION_SCOPE.lock() {
        Ok(guard) => (guard.0.clone(), guard.1.clone()),
        Err(_) => ("dsh".to_string(), "turn".to_string()),
    };
    let path = crate::notify::interrupt_flag_path(&session, &turn);
    crate::notify::write_interrupt_flag(&path, b"");
    eprintln!("computer-use turn interrupted by {source}");
    130
}

/// `<session_id>`, `<turn_id>` mirrored from the request `meta` so the hook
/// thread can write the official interrupt marker without locking the full
/// helper state.
static SESSION_SCOPE: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

/// When this process first saw the *current* `(session, turn)` scope. A marker file older
/// than that cannot belong to the current turn: it is a leftover from a turn that ended (or
/// a process that died) without cleaning up, and it must not refuse today's calls.
static SCOPE_SINCE: Mutex<Option<(String, String, std::time::SystemTime)>> = Mutex::new(None);
/// Filesystem timestamp resolution / clock skew allowance for the staleness comparison.
const MARKER_CLOCK_SLACK: Duration = Duration::from_millis(500);

pub fn set_session_scope(session: &str, turn: &str) {
    // INT-2/TURN-3: resident from the first request of the process, not only once an
    // input action has shown the overlay, so the Esc listener and the turn-ended watcher
    // are live for the whole turn.
    ensure_started();
    let scope = if let Ok(mut guard) = SESSION_SCOPE.lock() {
        if !session.is_empty() {
            guard.0 = session.to_string();
        }
        if !turn.is_empty() {
            guard.1 = turn.to_string();
        }
        (guard.0.clone(), guard.1.clone())
    } else {
        (session.to_string(), turn.to_string())
    };
    if let Ok(mut since) = SCOPE_SINCE.lock() {
        let changed = since
            .as_ref()
            .map(|(s, t, _)| s != &scope.0 || t != &scope.1)
            .unwrap_or(true);
        if changed {
            *since = Some((scope.0, scope.1, std::time::SystemTime::now()));
        }
    }
}

/// The scope the hook thread would write a marker for right now.
fn current_marker_scope() -> (String, String) {
    SESSION_SCOPE
        .lock()
        .map(|guard| (guard.0.clone(), guard.1.clone()))
        .unwrap_or_else(|_| ("dsh".to_string(), "turn".to_string()))
}

/// Test hook: pretend this process first saw the scope at `at`, so the staleness rule can be
/// exercised against a file whose mtime cannot be backdated portably.
#[cfg(test)]
pub(crate) fn set_scope_since_for_test(at: std::time::SystemTime) {
    if let Ok(mut since) = SCOPE_SINCE.lock() {
        *since = Some(("test".to_string(), "test".to_string(), at));
    }
}

/// True when a marker left over from an earlier turn is the reason the file exists.
pub fn marker_expired(marker_mtime: std::time::SystemTime, scope_since: std::time::SystemTime) -> bool {
    marker_mtime + MARKER_CLOCK_SLACK < scope_since
}

/// Should this marker refuse the current call? A live marker (written during the current
/// scope) does; a leftover one is DELETED and ignored, so a single stale 0-byte file can no
/// longer refuse every later call. Three Wave-3 agents hit exactly that: an hours-old
/// `interrupts/dsh/turn` poisoned every default-scope request with the physical-Escape
/// message.
pub fn marker_blocks(path: &std::path::Path) -> bool {
    if !crate::notify::interrupt_flag_exists(path) {
        return false;
    }
    let scope_since = SCOPE_SINCE
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|(_, _, at)| *at));
    match (scope_since, crate::notify::interrupt_flag_modified(path)) {
        (Some(since), Some(mtime)) if marker_expired(mtime, since) => {
            crate::notify::remove_interrupt_flag(path);
            false
        }
        _ => true,
    }
}

/// Delete the current scope's marker: the turn it belongs to is over.
fn clear_interrupt_marker() {
    let (session, turn) = current_marker_scope();
    crate::notify::remove_interrupt_flag(&crate::notify::interrupt_flag_path(&session, &turn));
}

/// Sidecar RPC timeout / cancel: drop overlay, do **not** treat as physical Escape.
pub fn cancel_work() {
    crate::overlay::hide();
}

pub fn end_turn() {
    ENDED.store(true, Ordering::SeqCst);
    crate::overlay::hide();
    // The interrupt marker is per-turn state, exactly like the transport's refusal: a turn
    // that ended must leave nothing behind, or the next turn (which may reuse the id) starts
    // out already refused.
    clear_interrupt_marker();
    // The serve loop clears per-turn state and re-arms before the next request.
    request_restart();
}

pub fn reset_turn() {
    STOPPED.store(false, Ordering::SeqCst);
    ENDED.store(false, Ordering::SeqCst);
    clear();
}

pub fn mark_synthetic(seconds: f64) {
    if let Ok(mut slot) = SYNTHETIC_UNTIL.lock() {
        *slot = Some(Instant::now() + Duration::from_secs_f64(seconds.max(0.0)));
    }
}

fn is_synthetic() -> bool {
    SYNTHETIC_UNTIL
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|until| Instant::now() <= until)
        .unwrap_or(false)
}

pub fn mark_user_input(reason: &str) {
    let fg = unsafe { GetForegroundWindow() };
    mark_user_input_at(root_hwnd(fg), reason);
}

fn mark_user_input_at(root: HWND, reason: &str) {
    if is_synthetic() {
        return;
    }
    let id = hwnd_id(root);
    if id == 0 {
        return;
    }
    if let Ok(mut roots) = DIRTY_ROOTS.lock() {
        if !roots.contains(&id) {
            roots.push(id);
        }
    }
    let target = TARGET_ROOT.load(Ordering::SeqCst);
    if target != 0 && target == id {
        DIRTY.store(true, Ordering::SeqCst);
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut slot) = REASON.lock() {
        *slot = reason.to_string();
    }
}

pub fn clear() {
    DIRTY.store(false, Ordering::SeqCst);
    if let Ok(mut roots) = DIRTY_ROOTS.lock() {
        roots.clear();
    }
    if let Ok(mut slot) = REASON.lock() {
        slot.clear();
    }
}

pub fn require_clean() -> Result<(), Error> {
    require_clean_for(TARGET_ROOT.load(Ordering::SeqCst))
}

pub fn available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}

fn root_is_dirty(root: isize) -> bool {
    if root == 0 {
        return false;
    }
    let rooted = hwnd_id(root_hwnd(HWND(root as *mut core::ffi::c_void)));
    DIRTY_ROOTS
        .lock()
        .ok()
        .map(|roots| roots.iter().any(|id| *id == root || (rooted != 0 && *id == rooted)))
        .unwrap_or(true)
}

pub fn require_clean_for(root: isize) -> Result<(), Error> {
    if ARMED.load(Ordering::SeqCst) && !available() {
        wait_until(
            || available() || !ARMED.load(Ordering::SeqCst),
            Duration::from_millis(200),
        );
    }
    if !available() {
        return Err(Error::desktop(MONITOR_UNAVAILABLE));
    }
    if root == 0 {
        return Ok(());
    }
    if root_is_dirty(root) {
        return Err(Error::desktop(USER_INPUT_MESSAGE).coded("input_monitor"));
    }
    Ok(())
}

pub fn snapshot() -> serde_json::Value {
    let target = TARGET_ROOT.load(Ordering::SeqCst);
    let dirty = root_is_dirty(target);
    serde_json::json!({
        "dirty": dirty,
        "reason": REASON.lock().ok().map(|g| g.clone()).unwrap_or_default(),
        "available": available(),
        "generation": GENERATION.load(Ordering::SeqCst),
        "mouseEvents": MOUSE_EVENTS.load(Ordering::SeqCst),
        "mouseDowns": MOUSE_DOWNS.load(Ordering::SeqCst),
        "mouseDownsInjected": MOUSE_DOWNS_INJECTED.load(Ordering::SeqCst),
        "keyEvents": KEY_EVENTS.load(Ordering::SeqCst),
        "armed": ARMED.load(Ordering::SeqCst),
        "installed": AVAILABLE.load(Ordering::SeqCst),
        "synthetic": is_synthetic(),
    })
}

pub fn ensure_started() {
    STARTED.get_or_init(|| {
        if thread::Builder::new()
            .name("cu-esc".into())
            .spawn(hook_loop)
            .is_err()
        {
            mark_unavailable(HOOK_KEYBOARD);
        }
    });
    // The turn-ended watcher is resident too: it is the only way an external party can
    // stop a turn while the overlay is down.
    ensure_turn_ended_watcher();
}

fn in_esc_grace() -> bool {
    ARM_AT
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|at| at.elapsed() < ESC_ARM_GRACE)
        .unwrap_or(false)
}

pub fn arm() {
    ensure_started();
    if let Ok(mut slot) = ARM_AT.lock() {
        *slot = Some(Instant::now());
    }
    let already = ARMED.swap(true, Ordering::SeqCst);
    if already && available() && THREAD_ID.load(Ordering::SeqCst) != 0 {
        return;
    }
    let gen = ARM_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    if !wait_until(|| THREAD_ID.load(Ordering::SeqCst) != 0, Duration::from_millis(500)) {
        mark_unavailable(HOOK_KEYBOARD);
        return;
    }
    post(WM_APP_ARM);
    if !wait_until(
        || DONE_GEN.load(Ordering::SeqCst) >= gen || !ARMED.load(Ordering::SeqCst),
        Duration::from_millis(1500),
    ) {
        mark_unavailable(HOOK_KEYBOARD);
    }
}

/// INT-2: the hooks stay resident, exactly like the official monitor thread. Disarming
/// only stops recording non-Escape human input and stops swallowing Escape, so a physical
/// Escape between two tool calls still ends the turn the way the official does.
pub fn disarm() {
    ARMED.store(false, Ordering::SeqCst);
    if let Ok(mut slot) = ARM_AT.lock() {
        *slot = None;
    }
}

fn wait_until(pred: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return pred();
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn mark_unavailable(what: &str) {
    AVAILABLE.store(false, Ordering::SeqCst);
    let mut slot = INSTALL_ERROR.lock().expect(HOOK_STATE_POISONED);
    *slot = format!("{MONITOR_START_UNAVAILABLE}{what}");
}

fn finish_install(ok: bool, err: Option<&str>) {
    if ok {
        AVAILABLE.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = INSTALL_ERROR.lock() {
            slot.clear();
        }
    } else if let Some(what) = err {
        mark_unavailable(what);
    } else {
        AVAILABLE.store(false, Ordering::SeqCst);
    }
    DONE_GEN.store(ARM_GEN.load(Ordering::SeqCst), Ordering::SeqCst);
}

fn set_ll_hook(id: WINDOWS_HOOK_ID, proc: HOOKPROC, what: &'static str) -> Result<HHOOK, &'static str> {
    match unsafe { SetWindowsHookExW(id, proc, None, 0) } {
        Ok(hook) if !hook.0.is_null() => Ok(hook),
        _ => Err(what),
    }
}

fn post(msg: u32) {
    let tid = THREAD_ID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn hook_loop() {
    unsafe {
        THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);
        let mut kbd = HHOOK::default();
        let mut mouse = HHOOK::default();
        let kbd_proc: HOOKPROC = Some(keyboard_proc);
        let mouse_proc: HOOKPROC = Some(mouse_proc);
        let install = |kbd: &mut HHOOK, mouse: &mut HHOOK| {
            if kbd.0.is_null() {
                match set_ll_hook(WH_KEYBOARD_LL, kbd_proc, HOOK_KEYBOARD) {
                    Ok(hook) => *kbd = hook,
                    Err(what) => {
                        finish_install(false, Some(what));
                        return;
                    }
                }
            }
            if mouse.0.is_null() {
                match set_ll_hook(WH_MOUSE_LL, mouse_proc, HOOK_MOUSE) {
                    Ok(hook) => *mouse = hook,
                    Err(what) => {
                        if !kbd.0.is_null() {
                            let _ = UnhookWindowsHookEx(*kbd);
                            *kbd = HHOOK::default();
                        }
                        finish_install(false, Some(what));
                        return;
                    }
                }
            }
            finish_install(!kbd.0.is_null() && !mouse.0.is_null(), None);
        };
        let uninstall = |kbd: &mut HHOOK, mouse: &mut HHOOK| {
            if !kbd.0.is_null() {
                let _ = UnhookWindowsHookEx(*kbd);
                *kbd = HHOOK::default();
            }
            if !mouse.0.is_null() {
                let _ = UnhookWindowsHookEx(*mouse);
                *mouse = HHOOK::default();
            }
            AVAILABLE.store(false, Ordering::SeqCst);
            DONE_GEN.store(ARM_GEN.load(Ordering::SeqCst), Ordering::SeqCst);
        };
        // INT-2: resident from process start, not only while armed.
        install(&mut kbd, &mut mouse);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_APP_ARM => install(&mut kbd, &mut mouse),
                WM_QUIT => break,
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        uninstall(&mut kbd, &mut mouse);
    }
}

/// What the keyboard hook should do with a key-down event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    Ignore,
    MarkDirty,
    Escape,
}

/// INT-1 / INT-2: the pure decision behind `keyboard_proc`.
///
/// * Our own injections are recognised by the `dwExtraInfo` tag (and the short window
///   around an action); anything else is the human. The tag is mandatory -- the
///   `LLKHF_INJECTED` flag is set on real input by RDP/VM consoles.
/// * Escape is swallowed (and the helper exits 130) whenever it is neither ours nor
///   inside the arming grace, *not* only while armed: the official monitor is resident
///   and interrupts in exactly that situation.
/// * Non-Escape human input is only recorded while armed.
pub(crate) fn classify_key(vk: u32, ours: bool, armed: bool, in_grace: bool) -> KeyAction {
    if ours {
        return KeyAction::Ignore;
    }
    if vk == VK_ESCAPE {
        if in_grace {
            KeyAction::Ignore
        } else {
            KeyAction::Escape
        }
    } else if armed {
        KeyAction::MarkDirty
    } else {
        KeyAction::Ignore
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
    if code >= 0 && wparam.0 == WM_KEYDOWN as usize {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let ours = info.dwExtraInfo == SYNTHETIC_TAG || is_synthetic();
        let action = classify_key(
            info.vkCode,
            ours,
            ARMED.load(Ordering::SeqCst),
            in_esc_grace(),
        );
        match action {
            KeyAction::Ignore => {}
            KeyAction::MarkDirty => {
                let fg = GetForegroundWindow();
                mark_user_input_at(root_hwnd(fg), "keyboard");
            }
            KeyAction::Escape => {
                // Official: swallow the key (return 1, never CallNextHookEx) and
                // terminate so the transport sees exit status 130. The automated
                // app must never observe this Escape.
                std::process::exit(mark_interrupted("the physical Escape key"));
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn official_mouse_dirty(msg: u32) -> bool {
    // Listing 14005ebf3: wparam in [WM_LBUTTONDOWN=0x201, +9] and bit mask 0x249
    // (LBUTTONDOWN / RBUTTONDOWN / MBUTTONDOWN / MOUSEWHEEL).
    let Some(delta) = msg.checked_sub(0x0201) else {
        return false;
    };
    delta < 10 && ((0x249u32 >> delta) & 1) != 0
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam.0 as u32;
    if code >= 0 {
        MOUSE_EVENTS.fetch_add(1, Ordering::Relaxed);
        if official_mouse_dirty(msg) {
            MOUSE_DOWNS.fetch_add(1, Ordering::Relaxed);
            let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            if info.flags & LLMHF_INJECTED != 0 {
                MOUSE_DOWNS_INJECTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if code >= 0 && ARMED.load(Ordering::SeqCst) && official_mouse_dirty(msg) {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if info.dwExtraInfo != SYNTHETIC_TAG && !is_synthetic() {
            let root = root_from_point(info.pt.x, info.pt.y);
            mark_user_input_at(root, "pointer");
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn safe_event_key(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') { ch } else { '_' })
        .take(80)
        .collect()
}

/// Official turn-ended event name (S:18365). The exact turn key is not pinned by the
/// static evidence, so the bare turn id is the primary name and the session-turn
/// composite is kept for callers that used the older DSH shape -- both under the official
/// prefix. `Local\DshComputerUseTurnEnded-` (TURN-4) no longer exists.
pub fn turn_ended_event_name(turn_id: &str) -> String {
    format!("{TURN_ENDED_EVENT_PREFIX}{}", safe_event_key(turn_id))
}

fn turn_ended_event_names(session_id: &str, turn_id: &str) -> [String; 2] {
    [
        turn_ended_event_name(turn_id),
        format!(
            "{TURN_ENDED_EVENT_PREFIX}{}-{}",
            safe_event_key(session_id),
            safe_event_key(turn_id)
        ),
    ]
}

fn open_turn_ended_event(name: &str) -> Option<windows::Win32::Foundation::HANDLE> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { CreateEventW(None, true, false, windows::core::PCWSTR(wide.as_ptr())).ok() }
}

#[allow(dead_code)]
pub fn signal_turn_ended(session_id: &str, turn_id: &str) {
    for name in turn_ended_event_names(session_id, turn_id) {
        if let Some(handle) = open_turn_ended_event(&name) {
            let _ = unsafe { SetEvent(handle) };
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

static TURN_ENDED_WATCHER: OnceLock<()> = OnceLock::new();

fn ensure_turn_ended_watcher() {
    TURN_ENDED_WATCHER.get_or_init(|| {
        if thread::Builder::new()
            .name("cu-turn-end".into())
            .spawn(turn_ended_watch_loop)
            .is_err()
        {
            eprintln!("{TURN_ENDED_EVENT_PREFIX}{TURN_ENDED_WATCHER_UNAVAILABLE}thread spawn failed");
        }
    });
}

fn current_scope() -> (String, String) {
    match SESSION_SCOPE.lock() {
        Ok(guard) => (guard.0.clone(), guard.1.clone()),
        Err(_) => (String::new(), String::new()),
    }
}

/// TURN-3: wait on the official `Local\CodexComputerUseTurnEnded-*` event and end the
/// turn when it fires. The signal is consumed per scope, so our own `signal_turn_ended`
/// cannot re-end the next turn, and a scope change re-targets the wait.
fn turn_ended_watch_loop() {
    let _ = SIGNAL_TURN_ENDED;
    let mut consumed: Option<(String, String)> = None;
    while !STOPPED.load(Ordering::SeqCst) {
        let (session, turn) = current_scope();
        if turn.is_empty() || consumed.as_ref() == Some(&(session.clone(), turn.clone())) {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        let mut fired = false;
        for name in turn_ended_event_names(&session, &turn) {
            let Some(handle) = open_turn_ended_event(&name) else {
                eprintln!("{name}{TURN_ENDED_WATCHER_UNAVAILABLE}{CREATE_TURN_ENDED_EVENT}");
                continue;
            };
            let waited = unsafe { WaitForSingleObject(handle, 200) };
            let _ = unsafe { CloseHandle(handle) };
            if waited == WAIT_OBJECT_0 {
                fired = true;
                break;
            }
            if waited != WAIT_TIMEOUT {
                eprintln!("{name}{TURN_ENDED_WATCHER_STOPPED}{TURN_ENDED_WAIT_FAILED}");
            }
        }
        if fired {
            if current_scope() == (session.clone(), turn.clone()) {
                end_turn();
            }
            consumed = Some((session, turn));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn fake(id: isize) -> HWND {
        HWND(id as *mut core::ffi::c_void)
    }

    /// The hook state (`DIRTY`, `DIRTY_ROOTS`, `TARGET_ROOT`, `AVAILABLE`) is
    /// process-global, so tests that touch it must not interleave.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_state() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    struct AvailGuard {
        previous: bool,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl AvailGuard {
        fn new() -> Self {
            let lock = lock_state();
            Self { previous: AVAILABLE.load(Ordering::SeqCst), _lock: lock }
        }
    }
    impl Drop for AvailGuard {
        fn drop(&mut self) {
            AVAILABLE.store(self.previous, Ordering::SeqCst);
        }
    }

    #[test]
    fn dirty_is_limited_to_the_watched_window() {
        let _avail = AvailGuard::new();
        AVAILABLE.store(true, Ordering::SeqCst);
        clear();
        watch_window(fake(42));
        mark_user_input_at(fake(99), "pointer");
        assert!(require_clean_for(42).is_ok());
        assert!(require_clean_for(99).is_err());
        mark_user_input_at(fake(42), "pointer");
        let err = require_clean_for(42).unwrap_err();
        assert_eq!(err.message, USER_INPUT_MESSAGE);
        assert!(require_clean().is_err());
        clear();
        assert!(require_clean_for(42).is_ok());
        assert!(require_clean().is_ok());
    }

    #[test]
    fn unavailable_monitor_fails_guarded_input() {
        let _avail = AvailGuard::new();
        clear();
        AVAILABLE.store(false, Ordering::SeqCst);
        watch_window(fake(42));
        let err = require_clean_for(42).unwrap_err();
        assert_eq!(err.message, MONITOR_UNAVAILABLE);
        let err = require_clean_for(0).unwrap_err();
        assert_eq!(err.message, MONITOR_UNAVAILABLE);
        let snap = snapshot();
        assert_eq!(snap["available"], serde_json::json!(false));
        AVAILABLE.store(true, Ordering::SeqCst);
        assert!(require_clean_for(42).is_ok());
        assert_eq!(snapshot()["available"], serde_json::json!(true));
    }

    #[test]
    fn official_monitor_unavailable_strings_match_rdata() {
        assert_eq!(
            MONITOR_UNAVAILABLE,
            "user input monitor unavailable; guarded input cannot continue"
        );
        assert_eq!(
            MONITOR_START_UNAVAILABLE,
            "computer-use user interruption monitor unavailable: "
        );
        assert_eq!(HOOK_KEYBOARD, "failed to install keyboard hook");
        assert_eq!(HOOK_MOUSE, "failed to install mouse hook");
        assert_eq!(HOOK_STATE_POISONED, "hook state mutex poisoned");
        let wrapped = format!("{MONITOR_START_UNAVAILABLE}{HOOK_KEYBOARD}");
        assert!(wrapped.starts_with("computer-use user interruption monitor unavailable: "));
        assert!(wrapped.contains("failed to install keyboard hook"));
    }

    #[test]
    fn official_mouse_mask_matches_listing_14005ebf3() {
        assert!(official_mouse_dirty(0x0201)); // WM_LBUTTONDOWN
        assert!(official_mouse_dirty(0x0204)); // WM_RBUTTONDOWN
        assert!(official_mouse_dirty(0x0207)); // WM_MBUTTONDOWN
        assert!(official_mouse_dirty(0x020A)); // WM_MOUSEWHEEL
        assert!(!official_mouse_dirty(0x0200)); // WM_MOUSEMOVE
        assert!(!official_mouse_dirty(0x0202)); // WM_LBUTTONUP
    }

    /// INT-2: a physical Escape ends the turn even while the overlay is down (the
    /// official monitor is resident). Non-Escape human input is still only recorded
    /// while armed, and our own tagged injections are never human input.
    #[test]
    fn escape_is_swallowed_even_when_the_overlay_is_down() {
        assert_eq!(classify_key(VK_ESCAPE, false, false, false), KeyAction::Escape);
        assert_eq!(classify_key(VK_ESCAPE, false, true, false), KeyAction::Escape);
        // The grace only covers the keystroke that armed the overlay.
        assert_eq!(classify_key(VK_ESCAPE, false, true, true), KeyAction::Ignore);
        assert_eq!(classify_key(VK_ESCAPE, true, true, false), KeyAction::Ignore);
        assert_eq!(classify_key(0x41, false, true, false), KeyAction::MarkDirty);
        assert_eq!(classify_key(0x41, false, false, false), KeyAction::Ignore);
        assert_eq!(classify_key(0x41, true, true, false), KeyAction::Ignore);
    }

    /// TURN-3 / TURN-4: one official prefix, and the sanitising rule matches the
    /// interrupt marker path.
    #[test]
    fn turn_ended_event_names_use_the_official_prefix_only() {
        assert_eq!(
            turn_ended_event_name("turn-1"),
            "Local\\CodexComputerUseTurnEnded-turn-1"
        );
        let names = turn_ended_event_names("sess", "turn-1");
        assert!(names.iter().all(|name| name.starts_with(TURN_ENDED_EVENT_PREFIX)));
        assert!(!names.iter().any(|name| name.contains("DshComputerUseTurnEnded")));
        assert_eq!(
            turn_ended_event_name("a/b c"),
            "Local\\CodexComputerUseTurnEnded-a_b_c"
        );
        assert_eq!(TURN_ENDED_WATCHER_STOPPED, "-computer-use turn-end event watcher stopped: ");
        assert_eq!(TURN_ENDED_WAIT_FAILED, "wait for computer-use turn-ended event failed");
        assert_eq!(CREATE_TURN_ENDED_EVENT, "create computer-use turn-ended event");
        assert_eq!(SIGNAL_TURN_ENDED, "signal computer-use turn-ended event");
    }

    /// A marker is per-turn state. DSH only ever wrote it, so one Escape refused every later
    /// call in that scope -- three Wave-3 agents hit an hours-old `interrupts/dsh/turn` that
    /// made the whole helper answer with the physical-Escape message.
    #[test]
    fn a_leftover_marker_is_ignored_and_deleted() {
        let dir = std::env::temp_dir().join(format!("dsh-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("turn");
        crate::notify::remove_interrupt_flag(&path);

        // Missing marker: never blocks.
        assert!(!marker_blocks(&path));

        // A marker written after this process first saw the scope is live and blocks.
        set_scope_since_for_test(std::time::SystemTime::now());
        crate::notify::write_interrupt_flag(&path, b"");
        assert!(crate::notify::interrupt_flag_exists(&path));
        assert!(marker_blocks(&path));

        // The same file, seen by a scope that started later, is a leftover: ignored AND
        // removed, which is the self-healing step the old code lacked.
        set_scope_since_for_test(std::time::SystemTime::now() + Duration::from_secs(10));
        assert!(!marker_blocks(&path));
        assert!(!crate::notify::interrupt_flag_exists(&path), "stale marker must be deleted");

        let _ = crate::notify::remove_interrupt_flag(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn the_expiry_rule_is_a_time_comparison() {
        let now = std::time::SystemTime::now();
        assert!(marker_expired(now - Duration::from_secs(60), now));
        assert!(!marker_expired(now, now));
        assert!(!marker_expired(now + Duration::from_secs(1), now));
    }
}



