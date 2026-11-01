//! EnumWindows: official targetable top-level windows + app identity.
//!
//! The filter is the official predicate `FUN_140038342` (see
//! `parity/official-constants.json` → `windowFilter`) plus the official IME class
//! blacklist and the packaged-app (UWP) host rule:
//!
//! IsWindowVisible(hwnd)
//! && !DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED)
//! && !(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW)
//! && ((ex & WS_EX_APPWINDOW) || GetWindow(hwnd, GW_OWNER) is NULL / query failed)
//!
//! Official has **no** title requirement and **no** minimum window area, so neither
//! does this file. App identity is the official `process:<full path>` form, and the
//! input side accepts every official prefix plus bare names / `.exe` paths.

use std::fmt;

use crate::assist;
use crate::dpi::window_dpi;
use crate::policy;
use crate::protocol::Error;
use serde_json::{json, Value};
use windows::core::{BOOL, PWSTR};
use std::collections::HashSet;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, SendInput, INPUT, INPUT_MOUSE, KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK,
    MOUSEINPUT, VK_MENU,
};
use windows::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumChildWindows, EnumWindows, FindWindowExW, FindWindowW, GetAncestor,
    GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetTopWindow, GetWindow, GetWindowLongW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, PeekMessageW, PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindow,
    SwitchToThisWindow, SystemParametersInfoW, WindowFromPoint, GA_ROOT, GW_ENABLEDPOPUP, GW_HWNDNEXT,
    GW_OWNER,
    GWL_EXSTYLE, GWL_STYLE, HWND_NOTOPMOST, HWND_TOPMOST, MSG, PM_NOREMOVE, SM_CXSCREEN,
    SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_SYSCOMMAND,
};

pub const ACTIVATE_CAPTURED_FAILED: &str = "failed to activate captured window";

#[derive(Clone)]
pub struct WindowRef {
    pub app: String,
    pub id: u64,
    pub title: String,
}

impl WindowRef {
    pub fn to_json(&self) -> Value {
        let mut body = json!({ "app": self.app, "id": self.id });
        if !self.title.is_empty() {
            body["title"] = json!(self.title);
        }
        body
    }
}

/// Official Window Debug used by `window id {id:?} was not found. Current windows: {windows:?}`.
impl fmt::Debug for WindowRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Window")
            .field("app", &self.app)
            .field("id", &self.id)
            .field("title", &self.title)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct AppInfo {
    pub id: String,
    pub display_name: String,
    pub is_running: bool,
    pub windows: Vec<WindowRef>,
    pub use_count: Option<i64>,
    pub last_used_date: Option<String>,
}

impl AppInfo {
    pub fn to_json(&self) -> Value {
        let mut body = json!({
            "id": self.id,
            "displayName": self.display_name,
            "isRunning": self.is_running,
            "windows": self.windows.iter().map(WindowRef::to_json).collect::<Vec<_>>(),
        });
        if let Some(count) = self.use_count {
            body["useCount"] = json!(count);
        }
        if let Some(when) = &self.last_used_date {
            body["lastUsedDate"] = json!(when);
        }
        body
    }
}

pub fn id_from_hwnd(hwnd: HWND) -> u64 {
    hwnd.0 as usize as u64
}

pub fn hwnd_from_id(id: u64) -> HWND {
    HWND(id as *mut core::ffi::c_void)
}

pub fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

pub fn exe_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return None;
        };
        let mut buf = vec![0u16; 32768];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
        let _ = CloseHandle(handle);
        if ok.is_err() || size == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        if path.is_empty() {
            None
        } else {
            Some(path)
        }
    }
}

pub const MISSING_PROCESS_NAME: &str = "process app identifier is missing a process name";

pub fn exe_name(pid: u32) -> Option<String> {
    let path = exe_path(pid)?;
    let name = path
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&path)
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub fn require_process_name(pid: u32) -> Result<String, Error> {
    exe_name(pid).ok_or_else(|| Error::desktop(MISSING_PROCESS_NAME))
}

/// The five official AppIdentifier prefixes (binary string table: `process:`,
/// `path:`, `registry:`, `app-user-model-id:`, `window-app:`). DSH emits only the
/// canonical `process:<full path>` form and accepts all of them on input (CW-5).
pub const APP_ID_PREFIXES: &[&str] = &[
    "process:",
    "path:",
    "registry:",
    "app-user-model-id:",
    "window-app:",
];

/// Official canonical app identifier for an executable path.
pub fn app_identifier_from_path(path: &str) -> String {
    format!("process:{path}")
}

/// Remove any official AppIdentifier prefix. Non-matching input is returned trimmed.
pub fn strip_app_prefix(app: &str) -> &str {
    let trimmed = app.trim().trim_matches('"');
    for prefix in APP_ID_PREFIXES {
        if let Some(head) = trimmed.get(..prefix.len()) {
            if head.eq_ignore_ascii_case(prefix) {
                return trimmed[prefix.len()..].trim().trim_matches('"');
            }
        }
    }
    trimmed
}

/// Shell-free leaf of an app identifier with any official prefix removed.
pub fn app_base_name(app: &str) -> String {
    let stripped = strip_app_prefix(app);
    stripped
        .replace('/', "\\")
        .rsplit('\\')
        .next()
        .unwrap_or(stripped)
        .to_string()
}

/// Official window app identity: `process:<full process image path>`
/// (`QueryFullProcessImageNameW`, the same call the official
/// `src/shell/process.rs` path normaliser uses).
pub fn app_identifier(pid: u32) -> Option<String> {
    exe_path(pid)
        .filter(|path| !path.is_empty())
        .map(|path| app_identifier_from_path(&path))
}

/// Compatibility layer for AppIdentifier comparison (CW-5): exact match, then
/// prefix-stripped, then shell-free leaf. This keeps a model that echoes either the
/// official `process:<path>` form or a bare `msedge.exe` / full `.exe` path working.
pub fn app_identity_matches(found: &str, expected: &str) -> bool {
    if expected.is_empty() {
        return true;
    }
    let found = strip_app_prefix(found);
    let expected = strip_app_prefix(expected);
    if found.eq_ignore_ascii_case(expected) {
        return true;
    }
    let found_base = app_base_name(found);
    let expected_base = app_base_name(expected);
    if found_base.is_empty() || !found_base.eq_ignore_ascii_case(&expected_base) {
        return false;
    }
    // The leaf comparison is a compatibility fallback for a *bare* name only. Two
    // same-named executables in different directories must stay distinguishable,
    // otherwise get_window/activate_window could accept the wrong app (CW-5).
    let bare = |value: &str| !value.contains('\\') && !value.contains('/');
    bare(found) || bare(expected)
}

fn has_owner(hwnd: HWND) -> bool {
    unsafe { GetWindow(hwnd, GW_OWNER) }
        .ok()
        .map(|owner| !owner.0.is_null())
        .unwrap_or(false)
}

fn is_shell_helper_window(hwnd: HWND) -> bool {
    let class = class_name(hwnd).to_ascii_lowercase();
    if class.contains("trayicon") || class.contains("notifyicon") {
        return true;
    }
    let area = window_area(hwnd);
    if class.contains("messagewindow") && area < 400 * 300 {
        return true;
    }
    area > 0 && area < 80 * 80
}

fn is_notify_tray_window(hwnd: HWND) -> bool {
    let class = class_name(hwnd).to_ascii_lowercase();
    class.contains("trayicon")
        || class.contains("notifyicon")
        || (class.contains("tray") && class.contains("messagewindow"))
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::from_mut(&mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    hr.is_ok() && cloaked != 0
}

/// `WS_EX_TOOLWINDOW` — official predicate rejects these outright.
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
/// `WS_EX_APPWINDOW` — official predicate accepts these even when owned.
pub const WS_EX_APPWINDOW: u32 = 0x0004_0000;

/// Official IME / input-sink class blacklist (`FUN_1400ae6eb`: string lengths
/// 11 / 11 / 28, matching `default ime` / `msctfime ui` /
/// `non client input sink window` at 0x1401377ad / 0x1401377b8 / 0x1401377c3).
pub const EXCLUDED_CLASSES: &[&str] = &[
    "default ime",
    "msctfime ui",
    "non client input sink window",
];

/// Official UWP classes whose owning process must be resolved (`FUN_1400ae2e1`,
/// `Windows.UI.Core.CoreWindow`@0x1401377df and `Windows.UI.Core.AppWindow`@0x1401377f9).
pub const PACKAGED_APP_CLASSES: &[&str] =
    &["Windows.UI.Core.CoreWindow", "Windows.UI.Core.AppWindow"];

/// Official host processes that hold no packaged-app identity
/// (`LockApp.exe`@0x140134531, `ApplicationFrameHost.exe`@0x14013453c, `_proxy.exe`@0x140134553).
pub const PACKAGED_APP_HOST_EXES: &[&str] =
    &["applicationframehost.exe", "_proxy.exe", "lockapp.exe"];

/// DSH addition (CW-4): the UWP *frame* window is owned by `ApplicationFrameHost.exe`,
/// which never carries an app identity. Official reaches the real app through the
/// child `Windows.UI.Core.CoreWindow`; without treating the frame class the same way
/// it would still be listed as `ApplicationFrameHost.exe`. `ApplicationFrameWindow`
/// is a Windows shell class, so it has no literal in the official binary -- this rule
/// is a documented DSH addition, recorded in `parity/official-constants.json`.
pub const PACKAGED_APP_FRAME_CLASS: &str = "ApplicationFrameWindow";

/// Official string emitted when a `Windows.UI.Core.*` window has no packaged-app
/// child (strings_all 0x131ee8 block).
pub const NO_PACKAGED_APP_CHILD: &str = "no packaged app child window found";

/// Result of `GetWindow(hwnd, GW_OWNER)`. The official predicate treats a window
/// with no owner and a failed owner query identically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerState {
    Present,
    Absent,
    QueryFailed,
}

/// Pure transcription of the official window predicate (`FUN_140038342`):
///
/// IsWindowVisible
/// && !cloaked
/// && !(ex_style & WS_EX_TOOLWINDOW)
/// && ((ex_style & WS_EX_APPWINDOW) || owner is NULL || owner query failed)
///
/// Kept free of Win32 calls so the truth table can be unit-tested without a desktop.
fn official_target_decision(
    visible: bool,
    cloaked: bool,
    ex_style: u32,
    owner: OwnerState,
) -> bool {
    if !visible || cloaked {
        return false;
    }
    if ex_style & WS_EX_TOOLWINDOW != 0 {
        return false;
    }
    ex_style & WS_EX_APPWINDOW != 0 || owner != OwnerState::Present
}

fn ex_style(hwnd: HWND) -> u32 {
    (unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }) as u32
}

fn owner_state(hwnd: HWND) -> OwnerState {
    match unsafe { GetWindow(hwnd, GW_OWNER) } {
        Ok(owner) if owner.0.is_null() => OwnerState::Absent,
        Ok(_) => OwnerState::Present,
        Err(_) => OwnerState::QueryFailed,
    }
}

/// Official IME / input-sink blacklist, case-insensitive on the class name.
pub fn is_excluded_class_name(class: &str) -> bool {
    EXCLUDED_CLASSES
        .iter()
        .any(|excluded| class.eq_ignore_ascii_case(excluded))
}

fn is_excluded_class(hwnd: HWND) -> bool {
    is_excluded_class_name(&class_name(hwnd))
}

/// Official UWP/packaged-app identity rule (`FUN_1400ae2e1`):
///
/// For `Windows.UI.Core.CoreWindow` / `AppWindow`, the owning process is resolved.
/// A window whose process is a UWP host (`ApplicationFrameHost.exe`, `_proxy.exe`,
/// `LockApp.exe`) or whose process cannot be resolved has **no** packaged-app child
/// identity, and is dropped instead of being reported as the host executable
/// (official string: `no packaged app child window found`).
///
/// Every non-UWP window is unaffected.
pub fn packaged_app_identity_ok(class: &str, process_name: Option<&str>) -> bool {
    let is_uwp = PACKAGED_APP_CLASSES
        .iter()
        .any(|c| class.eq_ignore_ascii_case(c))
        || class.eq_ignore_ascii_case(PACKAGED_APP_FRAME_CLASS);
    if !is_uwp {
        return true;
    }
    match process_name {
        None => false,
        Some(name) => !PACKAGED_APP_HOST_EXES
            .iter()
            .any(|host| name.eq_ignore_ascii_case(host)),
    }
}

/// Official predicate (`FUN_140038342`) + official IME blacklist + the DSH-only
/// exclusions for our own overlay/helper windows.
///
/// Deliberately absent (CW-3): the old `has_owner` hard reject, the
/// `area < 80*80` heuristic, and the `!title.is_empty()` requirement. Official has
/// none of them, and they dropped windows the official helper lists.
fn is_target(hwnd: HWND) -> bool {
    if !official_target_decision(
        is_window_visible(hwnd),
        is_cloaked(hwnd),
        ex_style(hwnd),
        owner_state(hwnd),
    ) {
        return false;
    }
    if is_excluded_class(hwnd) {
        return false;
    }
    if overlay_class(hwnd) {
        return false;
    }
    if let Some(app) = exe_name(window_pid(hwnd)) {
        if app.eq_ignore_ascii_case("dsh-computer-use.exe")
            || app.eq_ignore_ascii_case("codex-computer-use.exe")
        {
            return false;
        }
    }
    true
}

pub fn is_usable_app_window(hwnd: HWND) -> bool {
    is_window(hwnd) && is_target(hwnd)
}

fn overlay_class(hwnd: HWND) -> bool {
    let class = class_name(hwnd);
    class == "DshComputerUseCursorOverlay"
        || class == "DshComputerUseCursorOverlayPointer"
        || class == "DshComputerUseCursorOverlayEdge"
        || class == "CodexComputerUseCursorOverlay"
        || class == "CodexComputerUseCursorOverlayPointer"
}

/// One Toolhelp snapshot of running exe filenames and stems (lowercase).
pub fn running_exe_stems() -> HashSet<String> {
    let mut out = HashSet::new();
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let Ok(snap) = snap else {
        return out;
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe {
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let n = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe = String::from_utf16_lossy(&entry.szExeFile[..n]).to_ascii_lowercase();
                if !exe.is_empty() {
                    out.insert(exe.trim_end_matches(".exe").to_string());
                    out.insert(exe);
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Pids whose exe name matches catalog/launch keys, including processes with no listed window.
pub fn pids_matching_keys(keys: &[String]) -> Vec<u32> {
    let needles: Vec<String> = keys
        .iter()
        .map(|k| k.replace('\\', "/").rsplit('/').next().unwrap_or(k).to_ascii_lowercase())
        .filter(|k| !k.is_empty())
        .collect();
    if needles.is_empty() {
        return Vec::new();
    }
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let Ok(snap) = snap else {
        return Vec::new();
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut pids = Vec::new();
    unsafe {
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let n = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe = String::from_utf16_lossy(&entry.szExeFile[..n]).to_ascii_lowercase();
                let stem = exe.trim_end_matches(".exe");
                if needles.iter().any(|n| n == &exe || n == stem || exe.ends_with(n) || stem.ends_with(n.trim_end_matches(".exe"))) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    pids
}

struct PidEnum {
    pid: u32,
    hwnds: Vec<HWND>,
}

unsafe extern "system" fn enum_pid_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut PidEnum);
    if overlay_class(hwnd) {
        return BOOL(1);
    }
    if window_pid(hwnd) == ctx.pid {
        ctx.hwnds.push(hwnd);
    }
    BOOL(1)
}

pub fn hwnds_for_pid(pid: u32) -> Vec<HWND> {
    if pid == 0 {
        return Vec::new();
    }
    let mut ctx = PidEnum { pid, hwnds: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(enum_pid_proc), LPARAM(&mut ctx as *mut PidEnum as isize));
    }
    ctx.hwnds
}

fn window_area(hwnd: HWND) -> i64 {
    let mut rc = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rc) }.is_err() {
        return 0;
    }
    let w = (rc.right - rc.left).max(0) as i64;
    let h = (rc.bottom - rc.top).max(0) as i64;
    w * h
}

fn is_tool_window(hwnd: HWND) -> bool {
    (unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32) & 0x0000_0080 != 0
}

/// Real app frame only: skip owned, overlay, toolwindow toasts, and tiny snapshot HWNDs.
fn is_restore_candidate(hwnd: HWND) -> bool {
    if overlay_class(hwnd) || has_owner(hwnd) || is_shell_helper_window(hwnd) {
        return false;
    }
    if is_minimized(hwnd) || is_cloaked(hwnd) {
        return true;
    }
    let area = window_area(hwnd);
    if area >= 400 * 300 {
        return true;
    }
    if is_tool_window(hwnd) {
        return false;
    }
    area >= 200 * 150 && is_window_visible(hwnd)
}

const SC_RESTORE: usize = 0xF120;

fn best_visible_app_window(pid: u32) -> Option<HWND> {
    hwnds_for_pid(pid)
        .into_iter()
        .filter(|hwnd| {
            is_usable_app_window(*hwnd)
                || (is_window_visible(*hwnd)
                    && !is_minimized(*hwnd)
                    && !is_shell_helper_window(*hwnd)
                    && !has_owner(*hwnd)
                    && !overlay_class(*hwnd)
                    && window_area(*hwnd) >= 200 * 150)
        })
        .max_by_key(|hwnd| window_area(*hwnd))
}

fn post_restore(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(SC_RESTORE), LPARAM(0));
    }
}

/// Restore processes that are running but have no usable top-level window.
///
/// Hidden DirectComposition HWNDs must not be shown with `ShowWindow(SW_SHOW)`:
/// the compositor can freeze on the last frame (taskbar minimize still works,
/// client clicks do not). Prefer the app's own tray/notify path, then
/// `WM_SYSCOMMAND SC_RESTORE` for iconic windows.
///
/// One short wait for the whole pid set — never sleep per helper process.
pub fn restore_running_processes(pids: &[u32]) -> Option<HWND> {
    let pids: Vec<u32> = pids.iter().copied().filter(|pid| *pid != 0).collect();
    if pids.is_empty() {
        return None;
    }
    for pid in &pids {
        unsafe {
            let _ = AllowSetForegroundWindow(*pid);
        }
        if let Some(hwnd) = best_visible_app_window(*pid) {
            let _ = activate_hwnd(hwnd);
            return Some(hwnd);
        }
    }
    let mut trays = Vec::new();
    for pid in &pids {
        for hwnd in hwnds_for_pid(*pid) {
            if is_notify_tray_window(hwnd) {
                trays.push(hwnd);
            }
        }
    }
    for tray in &trays {
        post_tray_open(*tray);
    }
    invoke_notify_icons(&pids);
    for _ in 0..16 {
        std::thread::sleep(std::time::Duration::from_millis(80));
        for pid in &pids {
            if let Some(hwnd) = best_visible_app_window(*pid) {
                let _ = activate_hwnd(hwnd);
                return Some(hwnd);
            }
        }
    }
    for pid in &pids {
        let minimized = hwnds_for_pid(*pid)
            .into_iter()
            .filter(|h| is_minimized(*h) && is_restore_candidate(*h))
            .max_by_key(|h| window_area(*h));
        if let Some(hwnd) = minimized {
            post_restore(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(80));
            let _ = activate_hwnd(hwnd);
            if is_usable_app_window(hwnd) || is_window_visible(hwnd) {
                return Some(hwnd);
            }
        }
    }
    None
}

pub fn restore_process_windows(pid: u32) -> Option<HWND> {
    restore_running_processes(&[pid])
}

fn post_tray_open(hwnd: HWND) {
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_LBUTTONDBLCLK: u32 = 0x0203;
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), LPARAM(0));
        let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0));
        let _ = PostMessageW(Some(hwnd), WM_LBUTTONDBLCLK, WPARAM(1), LPARAM(0));
        let _ = PostMessageW(Some(hwnd), 0x0400, WPARAM(0), LPARAM(0));
    }
}

fn click_screen(x: i32, y: i32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let mut vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let mut vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vw < 1 {
            vw = GetSystemMetrics(SM_CXSCREEN);
        }
        if vh < 1 {
            vh = GetSystemMetrics(SM_CYSCREEN);
        }
        let ax = ((x - vx) as i64 * 65535 / vw.max(1) as i64) as i32;
        let ay = ((y - vy) as i64 * 65535 / vh.max(1) as i64) as i32;
        let flags = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        let mouse = |f| {
            let mut input = INPUT::default();
            input.r#type = INPUT_MOUSE;
            input.Anonymous.mi = MOUSEINPUT {
                dx: ax,
                dy: ay,
                mouseData: 0,
                dwFlags: flags | f,
                time: 0,
                dwExtraInfo: crate::interrupt::SYNTHETIC_TAG,
            };
            input
        };
        let inputs = [
            mouse(MOUSEEVENTF_MOVE),
            mouse(MOUSEEVENTF_LEFTDOWN),
            mouse(MOUSEEVENTF_LEFTUP),
            mouse(MOUSEEVENTF_LEFTDOWN),
            mouse(MOUSEEVENTF_LEFTUP),
        ];
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn click_rect_center(rect: RECT) {
    let x = rect.left + (rect.right - rect.left).max(1) / 2;
    let y = rect.top + (rect.bottom - rect.top).max(1) / 2;
    click_screen(x, y);
}

fn invoke_notify_icons(pids: &[u32]) {
    let mut clicked = Vec::new();
    for pid in pids {
        for hwnd in hwnds_for_pid(*pid) {
            for uid in 0u32..32 {
                let id = NOTIFYICONIDENTIFIER {
                    cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                    hWnd: hwnd,
                    uID: uid,
                    guidItem: windows::core::GUID::zeroed(),
                };
                let Ok(rect) = (unsafe { Shell_NotifyIconGetRect(&id) }) else {
                    continue;
                };
                if rect.right <= rect.left || rect.bottom <= rect.top {
                    continue;
                }
                let key = (rect.left, rect.top, rect.right, rect.bottom);
                if clicked.contains(&key) {
                    continue;
                }
                clicked.push(key);
                click_rect_center(rect);
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        }
    }
    if clicked.is_empty() {
        click_notify_chevron();
        std::thread::sleep(std::time::Duration::from_millis(250));
        for pid in pids {
            for hwnd in hwnds_for_pid(*pid) {
                let id = NOTIFYICONIDENTIFIER {
                    cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                    hWnd: hwnd,
                    uID: 1,
                    guidItem: windows::core::GUID::zeroed(),
                };
                if let Ok(rect) = unsafe { Shell_NotifyIconGetRect(&id) } {
                    if rect.right > rect.left {
                        click_rect_center(rect);
                    }
                }
            }
        }
    }
}

fn click_notify_chevron() {
    unsafe {
        let shell = FindWindowW(windows::core::w!("Shell_TrayWnd"), None).unwrap_or_default();
        if shell.0.is_null() {
            return;
        }
        let tray = FindWindowExW(Some(shell), None, windows::core::w!("TrayNotifyWnd"), None)
            .unwrap_or_default();
        if tray.0.is_null() {
            return;
        }
        let mut button = HWND::default();
        let _ = EnumChildWindows(
            Some(tray),
            Some(find_tray_button),
            LPARAM(&mut button as *mut HWND as isize),
        );
        let target = if button.0.is_null() { tray } else { button };
        let mut rect = RECT::default();
        if GetWindowRect(target, &mut rect).is_err() {
            return;
        }
        if button.0.is_null() {
            rect.right = rect.left + 22;
        }
        click_rect_center(rect);
    }
}

unsafe extern "system" fn find_tray_button(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let class = class_name(hwnd).to_ascii_lowercase();
    if class.contains("button") || class.contains("chevron") || class.contains("overflow") {
        let slot = &mut *(lparam.0 as *mut HWND);
        *slot = hwnd;
        return BOOL(0);
    }
    BOOL(1)
}

struct EnumCtx {
    windows: Vec<WindowRef>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    if is_target(hwnd) {
        let pid = window_pid(hwnd);
        let process_name = exe_name(pid);
        // CW-4: a UWP window whose process is the host framer has no packaged-app
        // identity, so official drops it (NO_PACKAGED_APP_CHILD) instead of
        // attributing it to ApplicationFrameHost.exe.
        if !packaged_app_identity_ok(&class_name(hwnd), process_name.as_deref()) {
            return BOOL(1);
        }
        let Some(app) = app_identifier(pid) else {
            return BOOL(1);
        };
        ctx.windows.push(WindowRef {
            app,
            id: id_from_hwnd(hwnd),
            title: window_title(hwnd),
        });
    }
    BOOL(1)
}

pub fn enum_windows() -> Result<Vec<WindowRef>, Error> {
    let mut ctx = EnumCtx { windows: Vec::new() };
    unsafe {
        EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut EnumCtx as isize))
            .map_err(|_| Error::desktop("EnumWindows failed"))?;
    }
    Ok(ctx.windows)
}

pub fn missing_window_message(id: u64, windows: &[WindowRef]) -> String {
    // Keep the official "window id … was not found. Current windows: …" prefix intact
    // (parity checks match it) and append an actionable hint so a model knows to
    // re-locate the window instead of retrying blindly.
    format!("window id {id:?} was not found. Current windows: {windows:?} — the target window appears CLOSED or gone; call list_windows to re-locate it, then retry.")
}

pub fn owner_changed_message(id: u64, expected: &str, actual: &str) -> String {
    format!("window id {id} no longer belongs to {expected}; current owner is {actual}")
}

/// Rehydrate a currently open window by id. Missing ids list EnumWindows results.
pub fn lookup_window(id: u64, expected_app: Option<&str>) -> Result<WindowRef, Error> {
    let hwnd = hwnd_from_id(id);
    if is_window(hwnd) && !overlay_class(hwnd) {
        let pid = window_pid(hwnd);
        let process_name = exe_name(pid);
        let identity_ok = packaged_app_identity_ok(&class_name(hwnd), process_name.as_deref());
        let app = app_identifier(pid).unwrap_or_default();
        if identity_ok && expected_app.map(|want| app_identity_matches(&app, want)).unwrap_or(true) {
            return Ok(WindowRef {
                app,
                id,
                title: window_title(hwnd),
            });
        }
    }
    let windows = enum_windows()?;
    if let Some(found) = windows.iter().find(|w| w.id == id) {
        if let Some(app) = expected_app {
            if !app_identity_matches(&found.app, app) {
                return Err(Error::desktop(owner_changed_message(id, app, &found.app)));
            }
        }
        return Ok(found.clone());
    }
    Err(Error::desktop(missing_window_message(id, &windows)))
}

pub fn list_apps_from_windows(windows: &[WindowRef]) -> Vec<AppInfo> {
    let mut grouped: Vec<AppInfo> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for window in windows {
        if let Some(&i) = index.get(&window.app) {
            grouped[i].windows.push(window.clone());
        } else {
            index.insert(window.app.clone(), grouped.len());
            grouped.push(AppInfo {
                id: window.app.clone(),
                display_name: app_base_name(&window.app),
                is_running: true,
                windows: vec![window.clone()],
                use_count: None,
                last_used_date: None,
            });
        }
    }
    let usage = assist::read_user_assist();
    for app in &mut grouped {
        let (count, last) = assist::merge_usage(&app.id, &usage);
        app.use_count = count;
        app.last_used_date = last;
    }
    grouped
}

pub fn window_rect(hwnd: HWND) -> Result<(i32, i32, i32, i32), Error> {
    let mut rc = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rc).map_err(|_| Error::desktop("GetWindowRect failed"))?;
    }
    Ok((rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top))
}

pub fn is_minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd).as_bool() }
}

pub fn is_window(hwnd: HWND) -> bool {
    !hwnd.0.is_null() && unsafe { IsWindow(Some(hwnd)).as_bool() }
}

/// DirectComposition / layered / composited: PrintWindow can freeze the GPU swapchain.
pub fn uses_redirected_composition(hwnd: HWND) -> bool {
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    const WS_EX_NOREDIRECTIONBITMAP: u32 = 0x0020_0000;
    const WS_EX_LAYERED: u32 = 0x0008_0000;
    const WS_EX_COMPOSITED: u32 = 0x0200_0000;
    ex & (WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_COMPOSITED) != 0
}

pub fn is_window_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Whether the visible part of `hwnd` is currently covered by another window.
///
/// Why this exists (2026-10-20, TapTap desktop client): an Electron window is
/// `WS_EX_NOREDIRECTIONBITMAP`, so `uses_redirected_composition` routes it to the
/// screen-DC `BitBlt` path. A screen-DC BitBlt samples whatever is *on screen* at
/// those coordinates, so when the target sat behind the Harness window the
/// captured JPEG showed the Harness chat instead of TapTap — while the tool still
/// reported `ok`. Feeding a different window's pixels to the model is worse than
/// failing: it reads as a successful observation and every later decision is
/// made against the wrong screen.
///
/// The check samples the corners and centre of the client rect with
/// `WindowFromPoint`, which returns the topmost window at a point regardless of
/// which thread owns it. A target that owns every sample is unobstructed. Windows
/// belonging to the target's own process count as the target, so child controls,
/// popups and overlays do not register as occlusion.
pub fn is_covered(hwnd: HWND) -> bool {
    is_covered_detail(hwnd)["covered"].as_bool().unwrap_or(false)
}

/// Same check as [`is_covered`], but reports every sample so a disagreement
/// between "the window looks covered" and "the check says clear" can be read off
/// the payload instead of guessed at.
pub fn is_covered_detail(hwnd: HWND) -> Value {
    let Ok((left, top, width, height)) = window_rect(hwnd) else {
        return json!({"covered": false, "error": "window_rect failed"});
    };
    if width <= 0 || height <= 0 {
        return json!({"covered": false, "error": "empty rect"});
    }
    let mut target_pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut target_pid)) };
    let target_root = root_hwnd(hwnd);
    // Corners sit just inside the frame; the centre catches a window parked over
    // the middle. Sampling the exact edge would hit the resize border.
    // Audit P2-6 (2026-10-25): five samples let a small popup (a captcha
    // overlay, a dropdown) slip between them while WGC crops its pixels into
    // the shot. Add a 3x3 interior grid on top of the classic five points so
    // anything larger than a third of the window in either dimension must
    // cover a sample. Still point-sampling, so a tiny tooltip can pass — but
    // small enough to be cheap (14 WindowFromPoint calls).
    let inset_x = (width / 20).max(1);
    let inset_y = (height / 20).max(1);
    let mut samples = vec![
        (left + inset_x, top + inset_y),
        (left + width - inset_x, top + inset_y),
        (left + inset_x, top + height - inset_y),
        (left + width - inset_x, top + height - inset_y),
        (left + width / 2, top + height / 2),
    ];
    for fy in [4, 2, 1] {
        for fx in [4, 2, 1] {
            if fx == 2 && fy == 2 {
                continue; // already the centre sample
            }
            samples.push((left + width * fx / 4, top + height * fy / 4));
        }
    }
    let mut rows = Vec::new();
    let mut covered = false;
    for (x, y) in samples {
        let hit = unsafe { WindowFromPoint(windows::Win32::Foundation::POINT { x, y }) };
        let hit_pid = if hit.0.is_null() {
            0
        } else {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hit, Some(&mut pid)) };
            pid
        };
        let same_window = hit == hwnd || (!hit.0.is_null() && root_hwnd(hit) == target_root);
        let same_process = hit_pid != 0 && hit_pid == target_pid;
        let blocked = !hit.0.is_null() && !same_window && !same_process;
        if blocked {
            covered = true;
        }
        rows.push(json!({
            "x": x, "y": y, "hitPid": hit_pid, "sameWindow": same_window,
            "sameProcess": same_process, "covered": blocked,
        }));
    }
    json!({
        "covered": covered,
        "targetPid": target_pid,
        "rect": {"x": left, "y": top, "width": width, "height": height},
        "samples": rows,
    })
}

pub fn root_hwnd(hwnd: HWND) -> HWND {
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

pub fn foreground_hwnd() -> HWND {
    unsafe { GetForegroundWindow() }
}

fn foreground_is_target(hwnd: HWND) -> bool {
    let fg = foreground_hwnd();
    if fg.0.is_null() {
        return false;
    }
    if fg == hwnd {
        return true;
    }
    let fg_root = root_hwnd(fg);
    let target_root = root_hwnd(hwnd);
    if fg_root == hwnd || fg_root == target_root {
        return true;
    }
    unsafe {
        GetWindow(fg, GW_OWNER)
            .ok()
            .map(|owner| owner == hwnd || root_hwnd(owner) == hwnd || owner == target_root)
            .unwrap_or(false)
    }
}

fn steal_foreground(hwnd: HWND) -> bool {
    // Z-order before focus — but only when something actually covers the
    // target (audit P2-7, 2026-10-25): the unconditional TOPMOST flash ran on
    // every activation, permanently reshuffling the user's z-order and waking
    // tray-hidden windows. When nothing occludes the target, focus alone is
    // enough and the z-order must stay untouched.
    if is_covered(hwnd) {
        bump_to_top(hwnd);
    }
    unsafe {
        let fg = GetForegroundWindow();
        let mut fg_pid = 0u32;
        let fg_tid = GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        let mut target_pid = 0u32;
        let target_tid = GetWindowThreadProcessId(hwnd, Some(&mut target_pid));
        let cur = GetCurrentThreadId();
        let attach_fg = fg_tid != 0 && fg_tid != cur;
        let attach_target = target_tid != 0 && target_tid != cur && target_tid != fg_tid;
        if attach_fg {
            let _ = AttachThreadInput(cur, fg_tid, true);
        }
        if attach_target {
            let _ = AttachThreadInput(cur, target_tid, true);
        }
        let ok = SetForegroundWindow(hwnd).as_bool();
        if attach_target {
            let _ = AttachThreadInput(cur, target_tid, false);
        }
        if attach_fg {
            let _ = AttachThreadInput(cur, fg_tid, false);
        }
        ok || foreground_is_target(hwnd)
    }
}

fn work_area() -> RECT {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut rect as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    rect
}

/// If the window is mostly off-screen, slide it onto the work area.
pub fn clamp_hwnd_to_work_area(hwnd: HWND) {
    if !is_window(hwnd) {
        return;
    }
    let mut rect = RECT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
    }
    let work = work_area();
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width < 200 || height < 150 {
        return;
    }
    let visible_w = rect.right.min(work.right) - rect.left.max(work.left);
    let visible_h = rect.bottom.min(work.bottom) - rect.top.max(work.top);
    let off = rect.left < work.left - 40
        || rect.top < work.top - 40
        || rect.right > work.right + 40
        || rect.bottom > work.bottom + 40
        || visible_w < width / 2
        || visible_h < height / 2;
    if !off {
        return;
    }
    let x = work.left + ((work.right - work.left - width) / 2).max(0);
    let y = work.top + ((work.bottom - work.top - height) / 2).max(0);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub fn is_already_foreground(hwnd: HWND) -> bool {
    is_window(hwnd) && foreground_is_target(hwnd)
}

/// Bounded budget for the post-restore UIA warm-up; `0` disables it.
const RESTORE_WARM_UP_BUDGET_MS: u64 = 12_000;
/// Space between warm-up probe attempts.
const RESTORE_WARM_UP_RETRY_MS: u64 = 250;

/// `Some(ms)` for a well-formed override, `None` for absent or malformed.
/// `Some(0)` is the operator's explicit disable and is honored, not defaulted.
fn parse_warm_up_budget(raw: Option<&str>) -> Option<u64> {
    raw.and_then(|v| v.parse().ok())
}

fn restore_warm_up_budget_ms() -> u64 {
    parse_warm_up_budget(std::env::var("DSH_CU_AX_RESTORE_WARMUP_MS").ok().as_deref())
        .unwrap_or(RESTORE_WARM_UP_BUDGET_MS)
}

/// A long-minimized window's UIA provider can stay silent long after the window
/// itself is back on screen: v2rayN 7.x (Avalonia) minimized for ~1h answered
/// two 15s dump waits 65s apart with `activate_window` long since returned, and
/// the stale monitor job then wedged the window into a 60s cooldown. A real UIA
/// read is exactly what wakes such a provider, so while the budget lasts, retry
/// one cheap read (`window_contains` with an empty needle — any node answers) on
/// the monitor machinery until it succeeds. Failures are ignored here: the
/// caller's next dump reports a genuinely dead provider through the normal
/// paths, and the cooldown the failed probes earned is dropped when the loop
/// ends (`clear_probe_cooldown`).
fn warm_up_uia_after_restore(hwnd: HWND) {
    let budget_ms = restore_warm_up_budget_ms();
    if budget_ms == 0 {
        return; // the operator disabled the warm-up via DSH_CU_AX_RESTORE_WARMUP_MS
    }
    if crate::uia::integrity_blocked(hwnd.0 as isize).is_some() {
        return; // reads cannot succeed here; do not spend the budget
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(budget_ms);
    let woke = loop {
        if crate::uia::global().window_contains(hwnd.0 as isize, "").is_ok() {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(RESTORE_WARM_UP_RETRY_MS));
    };
    crate::uia::clear_probe_cooldown(hwnd.0 as isize);
    let _ = woke;
}

/// `HWND_TOPMOST` bump: raise the window above everything, then drop the TOPMOST
/// flag so it stays a normal window. This is the documented way to beat a
/// maximized sibling that visually covers the target: `SetForegroundWindow`
/// alone can report success while the covering window is still on top in
/// Z-order, which is exactly the "activate_window returned ok but the screenshot
/// shows another app" failure.
fn bump_to_top(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            // No SWP_SHOWWINDOW (audit P2-7): showing is not this bump's
            // business, and it woke tray-hidden windows on every activation.
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Restore + foreground the captured HWND. Official error: `failed to activate captured window`.
pub fn activate_hwnd(hwnd: HWND) -> Result<(), Error> {
    if !is_window(hwnd) {
        return Err(Error::desktop(ACTIVATE_CAPTURED_FAILED));
    }
    // "Already foreground" is only good enough when the window is actually on screen.
    // A hidden or minimized window can still be the foreground window, and returning
    // early there meant activate_window reported success while every retry kept failing
    // with `window is not a usable app window`.
    if foreground_is_target(hwnd) && is_window_visible(hwnd) && !is_minimized(hwnd) {
        return Ok(());
    }
    unsafe {
        // `AttachThreadInput` (used by steal_foreground) only works for threads that own
        // a message queue, and the RPC thread has none, so prime one first. Without this,
        // activation failed whenever another application had recently been interacted
        // with -- which is exactly when the user is watching the automation work.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let _ = AllowSetForegroundWindow(u32::MAX);
        if is_minimized(hwnd) {
            post_restore(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(100));
            warm_up_uia_after_restore(hwnd);
        }
        // A hidden window can be made the foreground window while staying invisible, so
        // `SetForegroundWindow` alone reported success and the caller's retry still failed
        // with `window is not a usable app window`. Restoring means showing it too.
        if !is_window_visible(hwnd) {
            let _ = ShowWindow(hwnd, SW_SHOW);
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        if is_window_visible(hwnd) {
            clamp_hwnd_to_work_area(hwnd);
        }
        // Z-order first, focus second. Bumping the window above its maximized
        // siblings *before* asking for focus is what stops the "activation
        // succeeded but another window still covers the target" case.
        bump_to_top(hwnd);
        if SetForegroundWindow(hwnd).as_bool() || foreground_is_target(hwnd) {
            return Ok(());
        }
    }
    if steal_foreground(hwnd) {
        return Ok(());
    }
    // `SwitchToThisWindow` is the shell's own activation path and is not subject to the
    // foreground lock the way `SetForegroundWindow` is.
    unsafe {
        SwitchToThisWindow(hwnd, true);
        std::thread::sleep(std::time::Duration::from_millis(60));
        if foreground_is_target(hwnd) {
            return Ok(());
        }
        // Last resort: an injected ALT tap clears the foreground lock for this process.
        // Injected input is filtered out by our own low-level hooks, so this cannot be
        // mistaken for human activity.
        keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        if SetForegroundWindow(hwnd).as_bool() || foreground_is_target(hwnd) {
            return Ok(());
        }
    }
    Err(Error::desktop(ACTIVATE_CAPTURED_FAILED))
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn rects_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

pub fn classify_space(hwnd: HWND, overlay: &[isize]) -> &'static str {
    if overlay.contains(&(hwnd.0 as isize)) {
        return "overlay";
    }
    let cls = class_name(hwnd);
    let low = cls.to_ascii_lowercase();
    if cls == "#32768" {
        return "menu";
    }
    if low.contains("tooltip") || low == "tooltips_class32" {
        return "tooltip";
    }
    if cls == "#32770" {
        return "popup";
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & 0x0000_0080 != 0 && style & 0x8000_0000 != 0 {
        return "popup";
    }
    if style & 0x8000_0000 != 0 {
        return "popup";
    }
    "window"
}

pub fn monitor_device(hwnd: HWND) -> String {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(mon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            let end = info
                .szDevice
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.szDevice.len());
            return String::from_utf16_lossy(&info.szDevice[..end]);
        }
    }
    String::new()
}

pub fn space_identity(hwnd: HWND, space: &str, snapshot: &str, app_hint: &str) -> Value {
    let pid = window_pid(hwnd);
    let aumid = policy::process_aumid(pid);
    let exe = if app_hint.is_empty() {
        exe_name(pid).unwrap_or_default()
    } else {
        app_hint.to_string()
    };
    let key = if aumid.is_empty() {
        format!("exe:{exe}:{pid}")
    } else {
        format!("aumid:{aumid}")
    };
    let title = window_title(hwnd);
    let (left, top, width, height) = window_rect(hwnd).unwrap_or((0, 0, 0, 0));
    json!({
        "space": space,
        "windowID": id_from_hwnd(hwnd),
        "displayName": if title.is_empty() { exe.clone() } else { title },
        "processKey": key,
        "app": exe,
        "identity": format!("{key}|{}", hwnd.0 as isize),
        "display": monitor_device(hwnd),
        "snapshot": snapshot,
        "revision": 0,
        "originX": left,
        "originY": top,
        "nativeWidth": width,
        "nativeHeight": height,
        "bounds": {"x": left, "y": top, "width": width, "height": height},
        "dpi": window_dpi(hwnd),
    })
}

pub fn extra_space_hwnds(hwnd: HWND, overlay: &[isize]) -> Vec<(isize, String)> {
    let Ok(target) = window_rect(hwnd) else {
        return Vec::new();
    };
    let pid = window_pid(hwnd);
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let root = if root.0.is_null() { hwnd } else { root };
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(popup) = unsafe { GetWindow(hwnd, GW_ENABLEDPOPUP) } {
        let id = popup.0 as isize;
        if !popup.0.is_null()
            && popup != hwnd
            && unsafe { IsWindowVisible(popup).as_bool() }
            && !overlay.contains(&id)
        {
            found.push((id, classify_space(popup, overlay).to_string()));
            seen.insert(id);
        }
    }
    let mut cur = unsafe { GetTopWindow(None) }.unwrap_or(HWND::default());
    let mut hops = 0;
    while !cur.0.is_null() && hops < 512 {
        hops += 1;
        let handle = cur;
        cur = unsafe { GetWindow(cur, GW_HWNDNEXT) }.unwrap_or(HWND::default());
        let id = handle.0 as isize;
        if handle == hwnd || seen.contains(&id) || overlay.contains(&id) {
            continue;
        }
        if !unsafe { IsWindowVisible(handle).as_bool() } {
            continue;
        }
        let Ok(rect) = window_rect(handle) else { continue };
        let owner = unsafe { GetWindow(handle, GW_OWNER) }.unwrap_or(HWND::default());
        let ancestor = unsafe { GetAncestor(handle, GA_ROOT) };
        let related = owner == hwnd || ancestor == root || ancestor == hwnd;
        let same = window_pid(handle) == pid && rects_overlap(rect, target);
        if !(related || same) {
            continue;
        }
        found.push((id, classify_space(handle, overlay).to_string()));
        seen.insert(id);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_warm_up_budget_parses_override_disable_and_garbage() {
        // Absent or malformed → the caller falls back to the built-in budget.
        assert_eq!(parse_warm_up_budget(None), None);
        assert_eq!(parse_warm_up_budget(Some("abc")), None);
        assert_eq!(parse_warm_up_budget(Some("-5")), None);
        assert_eq!(parse_warm_up_budget(Some("12000")), Some(12000));
        // Some(0) is the operator's explicit disable, honored not defaulted.
        assert_eq!(parse_warm_up_budget(Some("0")), Some(0));
    }

    #[test]
    fn missing_id_lists_current_windows_with_official_window_debug() {
        let current = vec![WindowRef {
            app: "notepad.exe".into(),
            id: 42,
            title: "Untitled".into(),
        }];
        assert_eq!(
            missing_window_message(99, &current),
            "window id 99 was not found. Current windows: [Window { app: \"notepad.exe\", id: 42, title: \"Untitled\" }] — the target window appears CLOSED or gone; call list_windows to re-locate it, then retry."
        );
        assert_eq!(
            owner_changed_message(42, "chrome.exe", "notepad.exe"),
            "window id 42 no longer belongs to chrome.exe; current owner is notepad.exe"
        );
        assert_eq!(ACTIVATE_CAPTURED_FAILED, "failed to activate captured window");
    }

    #[test]
    fn lookup_unknown_id_emits_current_windows() {
        let id = 0x0FFF_FFFF_FFFF_FFF0;
        let err = lookup_window(id, None).expect_err("sentinel hwnd is not a listed app window");
        assert!(
            err.message.starts_with(&format!("window id {id} was not found. Current windows: ")),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("Current windows: ["),
            "{}",
            err.message
        );
    }

    #[test]
    fn activate_invalid_hwnd_fails_official_captured_message() {
        let err = activate_hwnd(hwnd_from_id(0)).expect_err("null hwnd cannot activate");
        assert_eq!(err.message, ACTIVATE_CAPTURED_FAILED);
    }

    /// CW-3 / G2: truth table of the official predicate (FUN_140038342). The Win32 call
    /// sequence is transcribed literally in the doc comment on
    /// `official_target_decision`, so this table is the gate for the filter itself.
    #[test]
    fn official_window_predicate_truth_table() {
        use OwnerState::{Absent, Present, QueryFailed};
        // visible && !cloaked && !WS_EX_TOOLWINDOW && (APPWINDOW || no owner)
        assert!(official_target_decision(true, false, WS_EX_APPWINDOW, Present));
        assert!(official_target_decision(true, false, 0, Absent));
        assert!(official_target_decision(true, false, 0, QueryFailed));
        assert!(official_target_decision(true, false, WS_EX_APPWINDOW, Absent));
        // hidden or cloaked: never listed, whatever the styles say
        assert!(!official_target_decision(false, false, WS_EX_APPWINDOW, Absent));
        assert!(!official_target_decision(true, true, WS_EX_APPWINDOW, Absent));
        // WS_EX_TOOLWINDOW rejects even with APPWINDOW or no owner
        assert!(!official_target_decision(
            true,
            false,
            WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
            Absent
        ));
        assert!(!official_target_decision(true, false, WS_EX_TOOLWINDOW, Absent));
        // owned without APPWINDOW is the only rejected owner case
        assert!(!official_target_decision(true, false, 0, Present));
        assert!(!official_target_decision(true, false, 0x0000_0100, Present));
        assert!(official_target_decision(true, false, 0x0000_0100, Absent));
        // The styles must be tested as a bitmask, not by sign.
        assert!(official_target_decision(true, false, 0x8000_0000, Absent));
        assert!(!official_target_decision(true, false, 0x8000_0080, Absent));
    }

    /// CW-3: the two heuristics official does not have (non-empty title, minimum
    /// 80x80 area) and the blanket owner reject must not come back into the filter.
    #[test]
    fn official_predicate_has_no_title_or_area_input() {
        let src = include_str!("enum_windows.rs");
        let decision = src
            .split("fn official_target_decision")
            .nth(1)
            .expect("official_target_decision present");
        let decision_body = decision.split("\n}").next().unwrap();
        assert!(!decision_body.contains("title"));
        assert!(!decision_body.contains("window_area"));
        let target = src.split("fn is_target(hwnd").nth(1).expect("is_target present");
        let target_body = target.split("\n}").next().unwrap();
        for banned in ["window_title", "has_owner", "is_shell_helper_window", "window_area"] {
            assert!(
                !target_body.contains(banned),
                "is_target must not use the non-official heuristic {banned}"
            );
        }
    }

    /// CW-6: the official IME / input-sink class blacklist.
    #[test]
    fn official_ime_class_blacklist() {
        assert_eq!(
            EXCLUDED_CLASSES,
            &["default ime", "msctfime ui", "non client input sink window"]
        );
        assert!(is_excluded_class_name("Default IME"));
        assert!(is_excluded_class_name("MSCTFIME UI"));
        assert!(is_excluded_class_name("Non Client Input Sink Window"));
        assert!(!is_excluded_class_name("IME"));
        assert!(!is_excluded_class_name("Notepad"));
    }

    /// CW-4: a UWP window owned by the host framer is dropped instead of being
    /// reported as ApplicationFrameHost.exe.
    #[test]
    fn packaged_app_host_windows_are_dropped() {
        assert_eq!(
            PACKAGED_APP_CLASSES,
            &["Windows.UI.Core.CoreWindow", "Windows.UI.Core.AppWindow"]
        );
        assert!(!packaged_app_identity_ok("Windows.UI.Core.CoreWindow", Some("ApplicationFrameHost.exe")));
        assert!(!packaged_app_identity_ok("Windows.UI.Core.AppWindow", Some("_proxy.exe")));
        assert!(!packaged_app_identity_ok("Windows.UI.Core.CoreWindow", Some("LockApp.exe")));
        assert!(!packaged_app_identity_ok("Windows.UI.Core.CoreWindow", None));
        assert!(packaged_app_identity_ok("Windows.UI.Core.CoreWindow", Some("Calculator.exe")));
        assert!(packaged_app_identity_ok("Notepad", Some("ApplicationFrameHost.exe")));
        // The UWP frame window is a host frame too, so it is never attributed to
        // ApplicationFrameHost.exe either.
        assert!(!packaged_app_identity_ok("ApplicationFrameWindow", Some("ApplicationFrameHost.exe")));
        assert!(packaged_app_identity_ok("ApplicationFrameWindow", Some("Calculator.exe")));
        assert!(packaged_app_identity_ok("ApplicationFrameWindow", None) == false);
        assert_eq!(PACKAGED_APP_FRAME_CLASS, "ApplicationFrameWindow");
        assert_eq!(NO_PACKAGED_APP_CHILD, "no packaged app child window found");
        // The gate the report asks for: no host exe can ever become a listed app.
        for host in PACKAGED_APP_HOST_EXES {
            assert!(!packaged_app_identity_ok("Windows.UI.Core.CoreWindow", Some(host)));
        }
        assert!(PACKAGED_APP_HOST_EXES
            .iter()
            .any(|h| h.eq_ignore_ascii_case("ApplicationFrameHost.exe")));
    }

    /// CW-5 / G4: official `process:<full path>` on the way out; every official
    /// prefix plus bare names and `.exe` paths on the way in.
    #[test]
    fn app_identifier_is_the_official_process_form() {
        assert_eq!(
            APP_ID_PREFIXES,
            &["process:", "path:", "registry:", "app-user-model-id:", "window-app:"]
        );
        assert_eq!(
            app_identifier_from_path(r"C:\a\b\msedge.exe"),
            r"process:C:\a\b\msedge.exe"
        );
        assert_eq!(app_base_name(r"process:C:\a\b\msedge.exe"), "msedge.exe");
        assert_eq!(strip_app_prefix(r"process:C:\a\b\msedge.exe"), r"C:\a\b\msedge.exe");
        assert_eq!(strip_app_prefix("PROCESS:c:/a/msedge.exe"), "c:/a/msedge.exe");
        assert_eq!(strip_app_prefix(r"path:C:\a\msedge.exe"), r"C:\a\msedge.exe");
        assert_eq!(
            strip_app_prefix("registry:HKLM\\x"),
            "HKLM\\x"
        );
        assert_eq!(
            strip_app_prefix("app-user-model-id:Microsoft.Paint_8wekyb3d8bbwe!App"),
            "Microsoft.Paint_8wekyb3d8bbwe!App"
        );
        assert_eq!(strip_app_prefix("window-app:msedge.exe"), "msedge.exe");
        assert_eq!(strip_app_prefix("msedge.exe"), "msedge.exe");
        assert_eq!(strip_app_prefix(r"C:\a\b\msedge.exe"), r"C:\a\b\msedge.exe");
        // Two same-named executables in different directories stay distinct ids.
        assert_ne!(
            app_identifier_from_path(r"C:\a\msedge.exe"),
            app_identifier_from_path(r"D:\b\msedge.exe")
        );
    }

    #[test]
    fn app_identity_matching_accepts_every_official_shape() {
        let canonical = r"process:C:\Program Files\Edge\msedge.exe";
        assert!(app_identity_matches(canonical, canonical));
        assert!(app_identity_matches(canonical, "msedge.exe"));
        assert!(app_identity_matches(canonical, r"C:\Program Files\Edge\msedge.exe"));
        assert!(app_identity_matches(canonical, r"path:C:\Program Files\Edge\msedge.exe"));
        assert!(app_identity_matches("msedge.exe", canonical));
        assert!(app_identity_matches("MSEDGE.EXE", canonical));
        assert!(app_identity_matches(canonical, ""));
        assert!(!app_identity_matches(canonical, "chrome.exe"));
        // A different directory must not be accepted just because the leaf matches.
        assert!(!app_identity_matches(canonical, r"D:\other\msedge.exe"));
    }
}
