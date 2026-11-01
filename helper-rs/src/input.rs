//! SendInput mouse (absolute virtual desktop), unicode typing, key chords, wheel.

use crate::enum_windows::{exe_name, window_pid, window_title};
use crate::overlay;
use crate::protocol::Error;
use windows::Win32::Foundation::{GetLastError, HANDLE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetGUIThreadInfo, GetSystemMetrics, GetWindow,
    GetWindowThreadProcessId, PostMessageW, WindowFromPoint, GUITHREADINFO, WM_PASTE, GA_ROOT,
    GA_ROOTOWNER, GW_OWNER, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

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

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn overlay_related(hwnd: HWND, overlay: &[isize]) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let id = hwnd_id(hwnd);
    if overlay.contains(&id) || overlay.contains(&hwnd_id(root_hwnd(hwnd))) {
        return true;
    }
    let cls = class_name(hwnd);
    cls == overlay::CLASS_NAME || cls == overlay::CURSOR_CLASS
}

fn as_hwnd(id: isize) -> HWND {
    HWND(id as *mut core::ffi::c_void)
}

/// Owned popup whose `GetAncestor(GA_ROOTOWNER)` is non-null (FUN_14002015e).
fn root_owner_if_owned(hwnd: HWND) -> Option<HWND> {
    if hwnd.0.is_null() {
        return None;
    }
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
    if owner.0.is_null() {
        return None;
    }
    let root_owner = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    if root_owner.0.is_null() {
        None
    } else {
        Some(root_owner)
    }
}

/// FUN_1400ab865: same hwnd, same GA_ROOT, or owned popup whose GA_ROOTOWNER
/// equals the other window's GA_ROOT.
fn same_window_family(a: HWND, b: HWND) -> bool {
    if a.0.is_null() || b.0.is_null() {
        return false;
    }
    if a == b {
        return true;
    }
    let ra = root_hwnd(a);
    let rb = root_hwnd(b);
    if ra.0.is_null() || rb.0.is_null() {
        return false;
    }
    if ra == rb {
        return true;
    }
    if let Some(ro) = root_owner_if_owned(a) {
        if ro == rb {
            return true;
        }
    }
    if let Some(ro) = root_owner_if_owned(b) {
        if ro == ra {
            return true;
        }
    }
    false
}

/// Official extra-space walk: `[x, x+width) × [y, y+height)` in physical pixels.
pub fn extra_rect_contains(px: i32, py: i32, x: i32, y: i32, width: i32, height: i32) -> bool {
    let px = px as i64;
    let py = py as i64;
    let left = x as i64;
    let top = y as i64;
    let right = left + width as i64;
    let bottom = top + height as i64;
    px >= left && py >= top && px < right && py < bottom
}

pub fn extra_space_covers(px: i32, py: i32, rects: &[(i32, i32, i32, i32)]) -> bool {
    rects.iter().any(|&(x, y, w, h)| extra_rect_contains(px, py, x, y, w, h))
}

/// FUN_1400a90b0: WindowFromPoint, same-root overlay hwnds, then PID + captured extra-space rects.
fn hit_allowed(
    hit: HWND,
    target: HWND,
    overlay: &[isize],
    pid: u32,
    extra_rects: &[(i32, i32, i32, i32)],
    px: i32,
    py: i32,
) -> bool {
    if target.0.is_null() {
        return true;
    }
    if same_window_family(hit, target) || overlay_related(hit, overlay) {
        return true;
    }
    for id in overlay {
        if *id != 0 && same_window_family(hit, as_hwnd(*id)) {
            return true;
        }
    }
    if let Some(root_owner) = root_owner_if_owned(hit) {
        if same_window_family(root_owner, target) {
            return true;
        }
    }
    if pid != 0 && window_pid(hit) == pid && extra_space_covers(px, py, extra_rects) {
        return true;
    }
    false
}

fn describe_hwnd(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return "no desktop window".into();
    }
    let title = window_title(hwnd);
    let app = exe_name(window_pid(hwnd)).unwrap_or_default();
    match (title.is_empty(), app.is_empty()) {
        (true, true) => "unknown window".into(),
        (true, false) => app,
        (false, true) => title,
        (false, false) => format!("{title} ({app})"),
    }
}

fn not_target_message(x: i32, y: i32, over: &str, target: &str) -> String {
    format!(
        "point ({x}, {y}) is over {over}, not target window {target}; activate the target or take a fresh screenshot before retrying"
    )
}

/// Official coordinate click/scroll/drag gate: WindowFromPoint then PID / extra-space rects.
pub fn require_point_on_target(
    x: f64,
    y: f64,
    target: HWND,
    overlay: &[isize],
    pid: u32,
    extra_rects: &[(i32, i32, i32, i32)],
) -> Result<(), Error> {
    if target.0.is_null() {
        return Ok(());
    }
    let px = x.round() as i32;
    let py = y.round() as i32;
    let hit = unsafe { WindowFromPoint(POINT { x: px, y: py }) };
    if hit_allowed(hit, target, overlay, pid, extra_rects, px, py) {
        return Ok(());
    }
    Err(Error::desktop(not_target_message(
        px,
        py,
        &describe_hwnd(hit),
        &describe_hwnd(target),
    )))
}

/// Test-only recorder for synthetic input.
///
/// `replace_text` / `paste_text` correctness is a *chord-order* property
/// ("select-all before writing"), which no fake desktop can observe. When the
/// recorder is armed, `send` captures the keyboard events instead of calling
/// `SendInput`, so the chord order can be asserted without touching a real
/// desktop.
#[cfg(test)]
thread_local! {
    static INPUT_RECORDER: std::cell::RefCell<Option<Vec<RecordedInput>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedInput {
    vk: u16,
    scan: u16,
    key_up: bool,
    unicode: bool,
}

#[cfg(test)]
fn arm_recorder() {
    INPUT_RECORDER.with(|cell| *cell.borrow_mut() = Some(Vec::new()));
}

#[cfg(test)]
fn disarm_recorder() {
    INPUT_RECORDER.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
fn take_recorded() -> Vec<RecordedInput> {
    INPUT_RECORDER
        .with(|cell| cell.borrow_mut().take().unwrap_or_default())
}

fn send(inputs: &[INPUT]) -> Result<(), Error> {
    if inputs.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    {
        if INPUT_RECORDER.with(|cell| cell.borrow().is_some()) {
            for input in inputs {
                if input.r#type == INPUT_KEYBOARD {
                    let ki = unsafe { input.Anonymous.ki };
                    let flags = ki.dwFlags;
                    INPUT_RECORDER.with(|cell| {
                        cell.borrow_mut().as_mut().expect("recorder armed").push(RecordedInput {
                            vk: ki.wVk.0,
                            scan: ki.wScan,
                            key_up: (flags & KEYEVENTF_KEYUP) == KEYEVENTF_KEYUP,
                            unicode: (flags & KEYEVENTF_UNICODE) == KEYEVENTF_UNICODE,
                        })
                    });
                }
            }
            return Ok(());
        }
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        let code = unsafe { GetLastError() }.0;
        return Err(Error::desktop(send_input_error(sent as usize, inputs.len(), code)));
    }
    Ok(())
}

/// Official rdata 0x1373fd: `SendInput sent <n> of <total> events; GetLastError=<code>`.
/// The partial count is the useful part: some events already landed.
pub fn send_input_error(sent: usize, total: usize, code: u32) -> String {
    format!("SendInput sent {sent} of {total} events; GetLastError={code}")
}

pub fn virtual_abs(x: f64, y: f64, vx: i32, vy: i32, vw: i32, vh: i32) -> (i32, i32) {
    let span_x = (vw - 1).max(1) as f64;
    let span_y = (vh - 1).max(1) as f64;
    (
        (((x - vx as f64) * 65535.0 / span_x) as i32).clamp(0, 65535),
        (((y - vy as f64) * 65535.0 / span_y) as i32).clamp(0, 65535),
    )
}

fn abs_point(x: f64, y: f64) -> (i32, i32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let mut vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let mut vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vw == 0 {
            vw = GetSystemMetrics(SM_CXSCREEN);
        }
        if vh == 0 {
            vh = GetSystemMetrics(SM_CYSCREEN);
        }
        virtual_abs(x, y, vx, vy, vw, vh)
    }
}

fn abs_flags(base: MOUSE_EVENT_FLAGS) -> MOUSE_EVENT_FLAGS {
    base | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: data,
        dwFlags: flags,
        time: 0,
        // Stamped so our own low-level hooks can recognise this as a synthetic event.
        // Filtering on `LLMHF_INJECTED` alone is not usable: remote-desktop and
        // VM-console input stacks flag the *human's* mouse events as injected too, so
        // the human-input guard would never fire on this machine.
        dwExtraInfo: crate::interrupt::SYNTHETIC_TAG,
    };
    input
}

fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        wVk: VIRTUAL_KEY(vk),
        wScan: scan,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: crate::interrupt::SYNTHETIC_TAG,
    };
    input
}

fn button_flags(button: &str) -> Result<(MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS), Error> {
    match button {
        "left" | "l" => Ok((MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)),
        "right" | "r" => Ok((MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)),
        "middle" | "m" => Ok((MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP)),
        other => Err(Error::type_err(format!("unsupported mouse_button: {other}"))),
    }
}

/// Official both-layer guard (rdata 0x131c32): `click_count` must be >= 1. The
/// old code silently clamped `0` to one real click.
pub fn require_click_count(count: i64) -> Result<i64, Error> {
    if count < 1 {
        return Err(Error::type_err("click_count must be >= 1"));
    }
    Ok(count)
}

pub fn move_click(x: f64, y: f64, button: &str, count: i64) -> Result<(), Error> {
    let n = require_click_count(count)?;
    let (down, up) = button_flags(button)?;
    if (button == "right" || button == "r") && n >= 2 {
        return Err(Error::type_err("right double click is not supported"));
    }
    let (ax, ay) = abs_point(x, y);
    for _ in 0..n {
        send(&[
            mouse(ax, ay, 0, abs_flags(MOUSEEVENTF_MOVE)),
            mouse(ax, ay, 0, abs_flags(down)),
            mouse(ax, ay, 0, abs_flags(up)),
        ])?;
    }
    Ok(())
}

pub fn drag_points(x1: f64, y1: f64, x2: f64, y2: f64) -> Result<(), Error> {
    let (a1x, a1y) = abs_point(x1, y1);
    let (a2x, a2y) = abs_point(x2, y2);
    send(&[
        mouse(a1x, a1y, 0, abs_flags(MOUSEEVENTF_MOVE)),
        mouse(a1x, a1y, 0, abs_flags(MOUSEEVENTF_LEFTDOWN)),
        mouse(a2x, a2y, 0, abs_flags(MOUSEEVENTF_MOVE)),
        mouse(a2x, a2y, 0, abs_flags(MOUSEEVENTF_LEFTUP)),
    ])
}

/// P1 migration: interpolated SendInput swipe used to replace scripts/uia-swipe.ps1.
///
/// WMPF/page readers bind a drag to the move events between down and up; a single
/// start->end jump (drag_points) is not recognised. This walks `steps` intermediate
/// MOVE events with the window-relative client coordinates mapped to screen, matching
/// the ps1's New-Mouse loop. `from`/`to` are window-relative CLIENT coords; they are
/// clamped into the client rect and converted with ClientToScreen, then to virtual-desk
/// absolute. Sleeps ~duration/steps between steps so the gesture has real timing.
pub fn swipe_client(
    hwnd: isize,
    from: (i32, i32),
    to: (i32, i32),
    duration_ms: i32,
    steps: i32,
) -> Result<(), Error> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    let h = HWND(hwnd as *mut core::ffi::c_void);
    let mut cr = RECT::default();
    if unsafe { GetClientRect(h, &mut cr) }.is_err() {
        return Err(Error::type_err("GetClientRect failed"));
    }
    let cw = cr.right - cr.left;
    let ch = cr.bottom - cr.top;
    if cw <= 0 || ch <= 0 {
        return Err(Error::type_err("empty client rect"));
    }
    let clampx = |x: i32| x.max(1).min(cw - 2);
    let clampy = |y: i32| y.max(1).min(ch - 2);
    let screen = |x: i32, y: i32| -> (i32, i32) {
        let mut p = POINT { x, y };
        let _ = unsafe { ClientToScreen(h, &mut p) };
        (p.x, p.y)
    };
    let (sx, sy) = screen(clampx(from.0), clampy(from.1));
    let (ex, ey) = screen(clampx(to.0), clampy(to.1));

    let steps = steps.max(1);
    let per = ((duration_ms - 40).max(1) / steps).max(1);
    let (asx, asy) = abs_point(sx as f64, sy as f64);
    send(&[mouse(asx, asy, 0, abs_flags(MOUSEEVENTF_MOVE))])?;
    std::thread::sleep(std::time::Duration::from_millis(30));
    send(&[mouse(asx, asy, 0, abs_flags(MOUSEEVENTF_LEFTDOWN))])?;
    std::thread::sleep(std::time::Duration::from_millis(30));
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let mx = sx as f64 + (ex - sx) as f64 * t;
        let my = sy as f64 + (ey - sy) as f64 * t;
        let (ax, ay) = abs_point(mx, my);
        send(&[mouse(ax, ay, 0, abs_flags(MOUSEEVENTF_MOVE))])?;
        std::thread::sleep(std::time::Duration::from_millis(per as u64));
    }
    let (aex, aey) = abs_point(ex as f64, ey as f64);
    send(&[mouse(aex, aey, 0, abs_flags(MOUSEEVENTF_LEFTUP))])?;
    Ok(())
}

pub fn scroll_at(x: f64, y: f64, scroll_x: f64, scroll_y: f64) -> Result<(), Error> {
    let (ax, ay) = abs_point(x, y);
    let dy = if scroll_y != 0.0 { -scroll_y } else { scroll_x };
    send(&[
        mouse(ax, ay, 0, abs_flags(MOUSEEVENTF_MOVE)),
        mouse(ax, ay, dy as i32 as u32, abs_flags(MOUSEEVENTF_WHEEL)),
    ])
}

const CF_UNICODETEXT: u32 = 13;
const CLIPBOARD_INVALID_UTF16: &str = "clipboard contained invalid UTF-16 data";
const CLIPBOARD_LOCK_READ: &str = "GlobalLock returned a null pointer while reading clipboard";
const CLIPBOARD_LOCK: &str = "GlobalLock returned a null pointer";
const CLIPBOARD_ALLOC: &str = "GlobalAlloc returned an invalid handle";
const CLIPBOARD_MISMATCH: &str = "clipboard did not contain requested text after setting it";
const CLIPBOARD_VERIFY: &str = "verify clipboard text before paste";

/// Official type_text: set/verify CF_UNICODETEXT then Control+V (rdata CTRLV).
pub fn paste_text(text: &str) -> Result<(), Error> {
    paste_text_for(None, text)
}

/// `paste_text` with an explicit IME-suppression target (the window that will
/// receive the keystrokes). `None` falls back to the foreground window.
pub fn paste_text_for(hwnd: Option<isize>, text: &str) -> Result<(), Error> {
    if text.is_empty() {
        return Ok(());
    }
    set_and_verify_clipboard(text)?;
    press_key_for(hwnd, "Control_L+v")
}

/// Replace the focused control's entire content with `text` (select-all, then paste).
///
/// `paste_text` alone *inserts at the caret*, which is what turned a `set_value`
/// fallback into "old value + one stray character". UIA `ValuePattern::SetValue`
/// is specified to replace, but when the pattern is missing (or lies about having
/// applied the write) the coordinate fallback has to reproduce replace semantics
/// itself. Selecting all first also makes the call idempotent: re-running
/// `set_value` with the same value can never grow the field.
///
/// `Control+A` is only meaningful while the target really has keyboard focus, so
/// the caller is responsible for clicking into the field (or we focus it via UIA)
/// before this runs.
pub fn replace_text(text: &str) -> Result<(), Error> {
    replace_text_for(None, text)
}

/// `replace_text` with an explicit IME-suppression target.
pub fn replace_text_for(hwnd: Option<isize>, text: &str) -> Result<(), Error> {
    // Empty value is a legitimate way to clear a field (official set_value accepts "").
    press_key_for(hwnd, "Control_L+a")?;
    if text.is_empty() {
        // Delete the selection so "clear the field" actually clears it.
        return press_key_for(hwnd, "Delete");
    }
    set_and_verify_clipboard(text)?;
    press_key_for(hwnd, "Control_L+v")
}

/// Foreground- and hook-independent paste: set the clipboard, then post
/// `WM_PASTE` straight to the focused control of `window_hwnd`'s GUI thread.
/// This bypasses `SendInput` entirely, so it is a fallback for the classic
/// Win32 EDIT case where a `SendInput` Ctrl+V does not land. It does NOT apply
/// to WinUI/XAML controls (no Win32 `WM_PASTE`), and it cannot cross a UIPI
/// integrity boundary — a higher-IL target rejects input from this Low-IL
/// helper regardless of path.
pub fn paste_text_via_postmessage(window_hwnd: isize, text: &str) -> Result<(), Error> {
    if text.is_empty() {
        return Ok(());
    }
    set_and_verify_clipboard(text)?;
    let hwnd = as_hwnd(window_hwnd);
    if hwnd.0.is_null() {
        return Err(Error::desktop("paste target window handle is null"));
    }
    unsafe {
        let tid = GetWindowThreadProcessId(hwnd, None);
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let focus = if GetGUIThreadInfo(tid, &mut info).is_ok() && !info.hwndFocus.0.is_null() {
            info.hwndFocus
        } else {
            hwnd
        };
        let _ = PostMessageW(Some(focus), WM_PASTE, WPARAM(0), LPARAM(0));
    }
    Ok(())
}

pub fn type_unicode(text: &str) -> Result<(), Error> {
    type_unicode_for(None, text)
}

/// `type_unicode` with an explicit IME-suppression target.
pub fn type_unicode_for(hwnd: Option<isize>, text: &str) -> Result<(), Error> {
    // `KEYEVENTF_UNICODE` bypasses the IME by construction, but an active IME can
    // still hold a composition state that swallows the first character, so the
    // guard is installed anyway — on the *target* window's thread.
    let _ime = crate::ime::ImeGuard::install_for(hwnd);
    let mut events = Vec::new();
    for ch in text.chars() {
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf).iter().copied() {
            events.push(key(0, unit, KEYEVENTF_UNICODE));
            events.push(key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
    }
    send(&events)
}

fn set_and_verify_clipboard(text: &str) -> Result<(), Error> {
    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    unsafe {
        OpenClipboard(None).map_err(|_| Error::desktop(CLIPBOARD_VERIFY))?;
        let _ = EmptyClipboard();
        let hmem = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return Err(Error::desktop(CLIPBOARD_ALLOC));
            }
        };
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err(Error::desktop(CLIPBOARD_LOCK));
        }
        std::ptr::copy_nonoverlapping(wide.as_mut_ptr().cast::<u8>(), ptr.cast::<u8>(), bytes);
        let _ = GlobalUnlock(hmem);
        if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hmem.0))).is_err() {
            let _ = CloseClipboard();
            return Err(Error::desktop(CLIPBOARD_VERIFY));
        }
        let _ = CloseClipboard();
        OpenClipboard(None).map_err(|_| Error::desktop(CLIPBOARD_VERIFY))?;
        let got = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return Err(Error::desktop(CLIPBOARD_VERIFY));
            }
        };
        let read_ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(got.0));
        if read_ptr.is_null() {
            let _ = CloseClipboard();
            return Err(Error::desktop(CLIPBOARD_LOCK_READ));
        }
        let restored = match utf16_zstring(read_ptr.cast()) {
            Ok(s) => s,
            Err(err) => {
                let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(got.0));
                let _ = CloseClipboard();
                return Err(err);
            }
        };
        let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(got.0));
        let _ = CloseClipboard();
        if restored != text {
            return Err(Error::desktop(CLIPBOARD_MISMATCH));
        }
    }
    Ok(())
}

fn utf16_zstring(ptr: *const u16) -> Result<String, Error> {
    if ptr.is_null() {
        return Err(Error::desktop(CLIPBOARD_LOCK_READ));
    }
    let mut len = 0usize;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 8 * 1024 * 1024 {
                return Err(Error::desktop(CLIPBOARD_INVALID_UTF16));
            }
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        String::from_utf16(slice).map_err(|_| Error::desktop(CLIPBOARD_INVALID_UTF16))
    }
}

pub fn press_key(chord: &str) -> Result<(), Error> {
    press_key_for(None, chord)
}

/// `press_key` with an explicit IME-suppression target.
///
/// The guard must target the window that will receive the keystrokes: the layout
/// is per-thread, and switching the helper's own thread does nothing for a
/// chord delivered to the target application (see `ime` module docs).
pub fn press_key_for(hwnd: Option<isize>, chord: &str) -> Result<(), Error> {
    let vks = virtual_keys(chord)?;
    // A Chinese IME eats `Control+A` / `Control+V` (the classic "set_value typed
    // a literal v" bug), so run every synthetic chord with the target's IME out
    // of the way.
    let _ime = crate::ime::ImeGuard::install_for(hwnd);
    let mut events = Vec::new();
    for vk in &vks {
        events.push(key(*vk, 0, KEYBD_EVENT_FLAGS(0)));
    }
    for vk in vks.iter().rev() {
        events.push(key(*vk, 0, KEYEVENTF_KEYUP));
    }
    send(&events)
}

/// Official unknown-key string (rdata 0x137bdf): `unsupported key: <name>`.
pub const UNSUPPORTED_KEY_PREFIX: &str = "unsupported key: ";

/// The official key-alias blob, `strings_all.txt:18385 @0x1374cc`, split at the
/// word boundaries the helper table implies. This is the test anchor for
/// `lookup_vk`: every one of these must resolve. `; ` and `~` are single-character
/// aliases between `BAR`/`PIPE` and `KP_EQUAL` in the official run.
pub const OFFICIAL_KEY_ALIASES: &str = "CONTROL CTRL_L CONTROL_L CTRL_R CONTROL_R SHIFT SHIFT_L SHIFT_R \
ALT OPTION ALT_L OPTION_L ALT_R OPTION_R WIN WINDOWS META COMMAND CMD SUPER OS WIN_L WINDOWS_L \
META_L COMMAND_L CMD_L SUPER_L OS_L WIN_R WINDOWS_R META_R COMMAND_R CMD_R SUPER_R OS_R ENTER \
RETURN LINEFEED KP_ENTER NUMPAD_ENTER NUMPADENTER NUM_ENTER NUMENTER TAB CLEAR SPACE ESC ESCAPE \
BACKSPACE DELETE DEL INSERT HOME BEGIN END PRIOR PAGEUP PAGE_UP NEXT PAGEDOWN PAGE_DOWN UP DOWN \
LEFT RIGHT MENU HELP SELECT PRINT SYS_REQ EXECUTE CANCEL BREAK MODE_SWITCH SCRIPT_SWITCH PAUSE \
SCROLL_LOCK NUM_LOCK CAPS_LOCK SHIFT_LOCK KP_DELETE NUMPAD_DELETE NUMPADDELETE NUM_DELETE \
NUMDELETE PLUS EQUAL EQUALS MINUS HYPHEN UNDERSCORE COMMA LESS PERIOD DOT FULL_STOP GREATER \
SLASH FORWARD_SLASH QUESTION SEMICOLON COLON APOSTROPHE QUOTE SINGLE_QUOTE DOUBLE_QUOTE GRAVE \
BACKTICK TILDE BRACKETLEFT BRACKET_LEFT LEFTBRACKET LEFT_BRACKET BRACELEFT BRACE_LEFT LEFTBRACE \
LEFT_BRACE BRACKETRIGHT BRACKET_RIGHT RIGHTBRACKET RIGHT_BRACKET BRACERIGHT BRACE_RIGHT RIGHTBRACE \
RIGHT_BRACE BACKSLASH BACK_SLASH BAR PIPE ; ` ~ KP_EQUAL NUMPAD_EQUAL NUMPADEQUAL NUM_EQUAL \
NUMEQUAL KP_MULTIPLY KP_STAR NUMPAD_MULTIPLY NUMPADMULTIPLY NUMPAD_STAR NUMPADSTAR NUM_MULTIPLY \
NUMMULTIPLY KP_ADD KP_PLUS NUMPAD_ADD NUMPADADD NUMPAD_PLUS NUMPADPLUS NUM_ADD NUMADD \
KP_SUBTRACT KP_MINUS NUMPAD_SUBTRACT NUMPADSUBTRACT NUMPAD_MINUS NUMPADMINUS NUM_SUBTRACT \
NUMSUBTRACT KP_DECIMAL KP_PERIOD KP_DOT NUMPAD_DECIMAL NUMPADDECIMAL NUMPAD_PERIOD NUMPADPERIOD \
NUMPAD_DOT NUMPADDOT KP_DIVIDE KP_SLASH NUMPAD_DIVIDE NUMPADDIVIDE NUMPAD_SLASH NUMPADSLASH \
NUM_DIVIDE NUMDIVIDE KP_0 KP0 NUMPAD_0 NUMPAD0 NUM_0 NUM0 KP_1 KP1 NUMPAD_1 NUMPAD1 NUM_1 NUM1 \
KP_2 KP2 NUMPAD_2 NUMPAD2 NUM_2 NUM2 KP_3 KP3 NUMPAD_3 NUMPAD3 NUM_3 NUM3 KP_4 KP4 NUMPAD_4 \
NUMPAD4 NUM_4 NUM4 KP_5 KP5 NUMPAD_5 NUMPAD5 NUM_5 NUM5 KP_6 KP6 NUMPAD_6 NUMPAD6 NUM_6 NUM6 \
KP_7 KP7 NUMPAD_7 NUMPAD7 NUM_7 NUM7 KP_8 KP8 NUMPAD_8 NUMPAD8 NUM_8 NUM8 KP_9 KP9 NUMPAD_9 \
NUMPAD9 NUM_9 NUM9 KP_SEPARATOR NUMPAD_SEPARATOR NUMPADSEPARATOR \
F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 F13 F14 F15 F16 F17 F18 F19 F20";

pub fn virtual_keys(key: &str) -> Result<Vec<u16>, Error> {
    let parts: Vec<&str> = key.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(Error::type_err("key is required"));
    }
    let mut codes = Vec::new();
    for part in parts {
        let alias = part.to_ascii_lowercase().replace('-', "_");
        if let Some(vk) = lookup_vk(&alias) {
            codes.push(vk);
        } else if part.chars().count() == 1 && part.chars().next().unwrap().is_ascii_alphanumeric() {
            // Letters and digits map to their ASCII code, which is the VK.
            // Punctuation must never take this path: ASCII 0x5B/0x5C are
            // VK_LWIN/VK_RWIN, so pressing "[" or "\\" would otherwise be a
            // Windows-key bypass.
            codes.push(part.chars().next().unwrap().to_ascii_uppercase() as u16);
        } else {
            return Err(Error::type_err(format!("{UNSUPPORTED_KEY_PREFIX}{part}")));
        }
    }
    Ok(codes)
}

fn lookup_vk(alias: &str) -> Option<u16> {
    Some(match alias {
        "return" | "enter" | "linefeed" | "kp_enter" | "numpad_enter" | "numpadenter" | "num_enter"
        | "numenter" => 0x0D,
        "tab" => 0x09,
        "space" => 0x20,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" | "kp_delete" | "numpad_delete" | "numpaddelete" | "num_delete"
        | "numdelete" => 0x2E,
        "insert" => 0x2D,
        "home" | "begin" => 0x24,
        "end" => 0x23,
        "prior" | "pageup" | "page_up" => 0x21,
        "next" | "pagedown" | "page_down" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "control" | "ctrl" | "ctrl_l" | "control_l" | "ctrl_r" | "control_r" => 0x11,
        "shift" | "shift_l" | "shift_r" => 0x10,
        "alt" | "option" | "alt_l" | "option_l" | "alt_r" | "option_r" | "menu" => 0x12,
        "clear" => 0x0C,
        "help" => 0x2F,
        "select" => 0x29,
        "print" | "sys_req" => 0x2C,
        "execute" => 0x2B,
        "cancel" | "break" => 0x03,
        // The official table accepts the X11 Mode_switch / Script_switch names;
        // Windows' closest concept is VK_MODECHANGE.
        "mode_switch" | "script_switch" => 0x1F,
        "pause" => 0x13,
        "scroll_lock" => 0x91,
        "num_lock" => 0x90,
        "caps_lock" | "shift_lock" => 0x14,
        "period" | "dot" | "full_stop" | "greater" => 0xBE,
        "comma" | "less" => 0xBC,
        "slash" | "forward_slash" | "question" => 0xBF,
        "minus" | "hyphen" | "underscore" => 0xBD,
        "plus" | "equal" | "equals" => 0xBB,
        "semicolon" | "colon" => 0xBA,
        "apostrophe" | "quote" | "single_quote" | "double_quote" => 0xDE,
        "grave" | "backtick" | "tilde" => 0xC0,
        "bracketleft" | "bracket_left" | "leftbracket" | "left_bracket" | "braceleft"
        | "brace_left" | "leftbrace" | "left_brace" => 0xDB,
        "bracketright" | "bracket_right" | "rightbracket" | "right_bracket" | "braceright"
        | "brace_right" | "rightbrace" | "right_brace" => 0xDD,
        "backslash" | "back_slash" | "bar" | "pipe" => 0xDC,
        "kp_equal" | "numpad_equal" | "numpadequal" | "num_equal" | "numequal" => 0x92,
        "kp_multiply" | "kp_star" | "numpad_multiply" | "numpadmultiply" | "numpad_star"
        | "numpadstar" | "num_multiply" | "nummultiply" => 0x6A,
        "kp_add" | "kp_plus" | "numpad_add" | "numpadadd" | "numpad_plus" | "numpadplus"
        | "num_add" | "numadd" => 0x6B,
        "kp_subtract" | "kp_minus" | "numpad_subtract" | "numpadsubtract" | "numpad_minus"
        | "numpadminus" | "num_subtract" | "numsubtract" => 0x6D,
        "kp_decimal" | "kp_period" | "kp_dot" | "numpad_decimal" | "numpaddecimal"
        | "numpad_period" | "numpadperiod" | "numpad_dot" | "numpaddot" => 0x6E,
        "kp_divide" | "kp_slash" | "numpad_divide" | "numpaddivide" | "numpad_slash"
        | "numpadslash" | "num_divide" | "numdivide" => 0x6F,
        "kp_separator" | "numpad_separator" | "numpadseparator" => 0x6C,
        "kp_0" | "kp0" | "numpad_0" | "numpad0" | "num_0" | "num0" => 0x60,
        "kp_1" | "kp1" | "numpad_1" | "numpad1" | "num_1" | "num1" => 0x61,
        "kp_2" | "kp2" | "numpad_2" | "numpad2" | "num_2" | "num2" => 0x62,
        "kp_3" | "kp3" | "numpad_3" | "numpad3" | "num_3" | "num3" => 0x63,
        "kp_4" | "kp4" | "numpad_4" | "numpad4" | "num_4" | "num4" => 0x64,
        "kp_5" | "kp5" | "numpad_5" | "numpad5" | "num_5" | "num5" => 0x65,
        "kp_6" | "kp6" | "numpad_6" | "numpad6" | "num_6" | "num6" => 0x66,
        "kp_7" | "kp7" | "numpad_7" | "numpad7" | "num_7" | "num7" => 0x67,
        "kp_8" | "kp8" | "numpad_8" | "numpad8" | "num_8" | "num8" => 0x68,
        "kp_9" | "kp9" | "numpad_9" | "numpad9" | "num_9" | "num9" => 0x69,
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "f13" => 0x7C,
        "f14" => 0x7D,
        "f15" => 0x7E,
        "f16" => 0x7F,
        "f17" => 0x80,
        "f18" => 0x81,
        "f19" => 0x82,
        "f20" => 0x83,
        "win" | "windows" | "meta" | "command" | "cmd" | "super" | "os" | "win_l" | "windows_l"
        | "meta_l" | "command_l" | "cmd_l" | "super_l" | "os_l" | "lwin" => 0x5B,
        "win_r" | "windows_r" | "meta_r" | "command_r" | "cmd_r" | "super_r" | "os_r" | "rwin" => 0x5C,
        // Single-character aliases from the official blob, plus the rest of the
        // US punctuation set so no character ever falls through to its ASCII code.
        "_" => 0xBD,
        "=" => 0xBB,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        ";" => 0xBA,
        "'" => 0xDE,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        "`" => 0xC0,
        "~" => 0xC0,
        "|" => 0xDC,
        "{" => 0xDB,
        "}" => 0xDD,
        ":" => 0xBA,
        "\"" => 0xDE,
        "<" => 0xBC,
        ">" => 0xBE,
        "?" => 0xBF,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_target_message_matches_official_fragments() {
        let msg = not_target_message(886, 715, "Chat (Weixin.exe)", "WeChat (Weixin.exe)");
        assert_eq!(
            msg,
            "point (886, 715) is over Chat (Weixin.exe), not target window WeChat (Weixin.exe); activate the target or take a fresh screenshot before retrying"
        );
        assert!(msg.contains("point ("));
        assert!(msg.contains(") is over "));
        assert!(msg.contains(", not target window "));
        assert!(msg.contains("; activate the target or take a fresh screenshot before retrying"));
    }

    #[test]
    fn describe_null_hwnd_is_no_desktop_window() {
        assert_eq!(describe_hwnd(HWND::default()), "no desktop window");
    }

    #[test]
    fn clipboard_paste_strings_match_official_rdata() {
        assert_eq!(CLIPBOARD_VERIFY, "verify clipboard text before paste");
        assert_eq!(CLIPBOARD_INVALID_UTF16, "clipboard contained invalid UTF-16 data");
        assert_eq!(CLIPBOARD_LOCK_READ, "GlobalLock returned a null pointer while reading clipboard");
        assert_eq!(CLIPBOARD_LOCK, "GlobalLock returned a null pointer");
        assert_eq!(CLIPBOARD_ALLOC, "GlobalAlloc returned an invalid handle");
        assert_eq!(CLIPBOARD_MISMATCH, "clipboard did not contain requested text after setting it");
        assert_eq!(virtual_keys("Control_L+v").unwrap(), vec![0x11, b'V' as u16]);
        assert_eq!(virtual_keys("Control_L+a").unwrap(), vec![0x11, b'A' as u16]);
        assert_eq!(virtual_keys("Delete").unwrap(), vec![0x2E]);
        assert_eq!(virtual_keys("F13").unwrap(), vec![0x7C]);
        assert_eq!(virtual_keys("F20").unwrap(), vec![0x83]);
    }

    /// The regression this guards: `set_value` on a field that already held a
    /// value used to fall back to a bare `paste_text`, which *inserts* at the
    /// caret. The observed result was `15168866515` becoming `15168866515v`
    /// instead of being replaced. `replace_text` must therefore always emit a
    /// select-all chord first, and treat the empty string as "clear".
    #[test]
    fn replace_text_selects_all_before_writing() {
        arm_recorder();
        let _ = replace_text("17653116208");
        let recorded = take_recorded();
        // Group the flat event list into chords by walking down/up pairs.
        let chords = chords_of(&recorded);
        assert_eq!(
            chords.first().cloned().unwrap_or_default(),
            vec![0x11, b'A' as u16],
            "the first chord must be Ctrl+A (select-all), never a bare paste: {chords:?}"
        );
        assert!(
            chords.iter().any(|c| *c == vec![0x11, b'V' as u16]),
            "the write itself is a Ctrl+V paste: {chords:?}"
        );
        assert_eq!(
            chords.iter().filter(|c| **c == vec![0x11, b'A' as u16]).count(),
            1,
            "exactly one select-all: no double selection, no stray chord: {chords:?}"
        );
        // The select-all must come first, not after the paste.
        let select_all = chords.iter().position(|c| *c == vec![0x11, b'A' as u16]);
        let paste = chords.iter().position(|c| *c == vec![0x11, b'V' as u16]);
        assert!(select_all < paste, "order matters: {chords:?}");
    }

    #[test]
    fn replace_text_empty_value_clears_the_selection() {
        arm_recorder();
        let _ = replace_text("");
        let recorded = take_recorded();
        let chords = chords_of(&recorded);
        assert_eq!(chords.first().cloned().unwrap_or_default(), vec![0x11, b'A' as u16]);
        assert_eq!(
            chords.last().cloned().unwrap_or_default(),
            vec![0x2E],
            "empty value = select-all then Delete, and no clipboard write: {chords:?}"
        );
        assert!(!chords.iter().any(|c| *c == vec![0x11, b'V' as u16]));
    }

    /// Group a flat down/up event list into chords.
    ///
    /// `press_key` emits all keydowns first and then the keyups in reverse
    /// order, so a chord is simply a run of consecutive keydowns. Anything that
    /// is not a plain keydown (a keyup, or a `KEYEVENTF_UNICODE` character)
    /// ends the current run.
    fn chords_of(recorded: &[RecordedInput]) -> Vec<Vec<u16>> {
        let mut chords: Vec<Vec<u16>> = Vec::new();
        for event in recorded {
            if !event.unicode && !event.key_up {
                if let Some(last) = chords.last_mut().filter(|c| !c.is_empty()) {
                    last.push(event.vk);
                } else {
                    chords.push(vec![event.vk]);
                }
            } else if let Some(last) = chords.last_mut() {
                // A closing event seals the run so the next keydown starts a
                // fresh chord.
                if !last.is_empty() {
                    chords.push(Vec::new());
                }
            }
        }
        chords.retain(|c| !c.is_empty());
        chords
    }

    #[test]
    fn extra_space_rect_is_half_open_physical() {
        assert!(extra_rect_contains(100, 200, 100, 200, 50, 40));
        assert!(extra_rect_contains(149, 239, 100, 200, 50, 40));
        assert!(!extra_rect_contains(150, 200, 100, 200, 50, 40));
        assert!(!extra_rect_contains(100, 240, 100, 200, 50, 40));
        assert!(!extra_rect_contains(99, 200, 100, 200, 50, 40));
        assert!(!extra_rect_contains(100, 199, 100, 200, 50, 40));
        assert!(extra_space_covers(120, 210, &[(0, 0, 10, 10), (100, 200, 50, 40)]));
        assert!(!extra_space_covers(50, 50, &[(100, 200, 50, 40)]));
    }

    #[test]
    fn official_key_alias_blob_all_resolve() {
        // TC-17: table-driven against the official 0x1374cc blob.
        let mut missing = Vec::new();
        for alias in OFFICIAL_KEY_ALIASES.split_whitespace() {
            if virtual_keys(alias).is_err() {
                missing.push(alias.to_string());
            }
        }
        assert!(missing.is_empty(), "official aliases not accepted: {missing:?}");
    }

    #[test]
    fn every_windows_key_alias_is_rejected_by_policy() {
        // TC-16 invariant: any table alias that resolves to VK_LWIN/VK_RWIN must be
        // denied by the policy guard, so the key table and WIN_KEY cannot drift.
        for alias in OFFICIAL_KEY_ALIASES.split_whitespace() {
            let Ok(codes) = virtual_keys(alias) else { continue };
            if codes.iter().any(|vk| *vk == 0x5B || *vk == 0x5C) {
                assert!(
                    crate::policy::deny_press_key(alias).is_err(),
                    "{alias} maps to a Windows key but is not denied"
                );
            }
        }
        assert_eq!(virtual_keys("lwin").unwrap(), vec![0x5B]);
        assert_eq!(virtual_keys("rwin").unwrap(), vec![0x5C]);
        assert_eq!(virtual_keys("Win_L").unwrap(), vec![0x5B]);
        assert_eq!(virtual_keys("os_r").unwrap(), vec![0x5C]);
    }

    #[test]
    fn punctuation_never_falls_through_to_ascii_vk() {
        // ASCII 0x5B/0x5C are VK_LWIN/VK_RWIN; a single "[" or "\\" must not press them.
        assert_eq!(virtual_keys("[").unwrap(), vec![0xDB]);
        assert_eq!(virtual_keys("]").unwrap(), vec![0xDD]);
        assert_eq!(virtual_keys("\\").unwrap(), vec![0xDC]);
        assert_eq!(virtual_keys(";").unwrap(), vec![0xBA]);
        assert_eq!(virtual_keys("`").unwrap(), vec![0xC0]);
        assert_eq!(virtual_keys("~").unwrap(), vec![0xC0]);
        assert_eq!(virtual_keys("|").unwrap(), vec![0xDC]);
        assert_eq!(virtual_keys("{").unwrap(), vec![0xDB]);
        assert_eq!(virtual_keys("}").unwrap(), vec![0xDD]);
        assert_eq!(virtual_keys("_").unwrap(), vec![0xBD]);
        assert_eq!(virtual_keys("a").unwrap(), vec![0x41]);
        assert_eq!(virtual_keys("7").unwrap(), vec![0x37]);
        assert!(!virtual_keys("[").unwrap().iter().any(|vk| *vk == 0x5B || *vk == 0x5C));
    }

    #[test]
    fn unknown_key_and_mouse_button_match_the_official_strings() {
        assert_eq!(UNSUPPORTED_KEY_PREFIX, "unsupported key: ");
        assert_eq!(virtual_keys("Frobnicate").unwrap_err().message, "unsupported key: Frobnicate");
        assert_eq!(button_flags("sideways").unwrap_err().message, "unsupported mouse_button: sideways");
        assert_eq!(require_click_count(0).unwrap_err().message, "click_count must be >= 1");
        assert_eq!(require_click_count(-3).unwrap_err().message, "click_count must be >= 1");
        assert_eq!(require_click_count(1).unwrap(), 1);
        assert_eq!(require_click_count(3).unwrap(), 3);
        assert_eq!(send_input_error(2, 5, 87), "SendInput sent 2 of 5 events; GetLastError=87");
    }

    #[test]
    fn virtual_abs_clamps_to_u16_bounds() {
        // Normal bounds within single screen
        assert_eq!(virtual_abs(0.0, 0.0, 0, 0, 1920, 1080), (0, 0));
        assert_eq!(virtual_abs(1919.0, 1079.0, 0, 0, 1920, 1080), (65535, 65535));

        // Out-of-bounds coordinates must be clamped to 0..=65535
        assert_eq!(virtual_abs(-500.0, -200.0, 0, 0, 1920, 1080), (0, 0));
        assert_eq!(virtual_abs(99999.0, 88888.0, 0, 0, 1920, 1080), (65535, 65535));
    }
}
