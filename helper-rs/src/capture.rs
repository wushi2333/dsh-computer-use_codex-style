//! WGC / WinRT screenshot pipeline matching official helper `src/capture/image.rs`.
//!
//! Official reference values live in `parity/official-constants.json`; the
//! `official_constants_single_source_of_truth` test fails the build when a constant
//! here drifts from that file. Official pipeline:
//!
//! CreateForMonitor → Direct3D11CaptureFramePool.CreateFreeThreaded (official: **1**
//! buffer; DSH deliberately uses 2, see `FRAMEPOOL_BUFFERS`) →
//! SetIsCursorCaptureEnabled(**true**) → SetIsBorderRequired(false) → StartCapture →
//! TryGetNextFrame (timeout "window capture timed out") →
//! SoftwareBitmap.CreateCopyFromSurfaceAsync (full surface) → CPU crop →
//! JPEG BitmapEncoder "ImageQuality" **0.8** with ScaledWidth/Height.
//!
//! Official is WGC-only and fails loudly; the `gdi-*` fallbacks below are a DSH
//! extension (CW-7) and are reported through `capture_diagnostics()`.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{Interface, BOOL, HSTRING};
use windows::Foundation::{PropertyType, PropertyValue, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapEncoder, BitmapPixelFormat, BitmapPropertySet, BitmapTypedValue,
    SoftwareBitmap,
};
use windows::Graphics::SizeInt32;
use windows::Storage::Streams::{Buffer, DataReader, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::System::Com::{CoIncrementMTAUsage, CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForSingleObject,
};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{IBufferByteAccess, RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwareness, SetProcessDpiAwarenessContext,
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, IsWindow, SetWindowDisplayAffinity, ShowCursor,
    ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE,
};

const PW_RENDERFULLCONTENT: u32 = 2;

#[link(name = "user32")]
unsafe extern "system" {
    fn PrintWindow(hwnd: HWND, hdcblt: HDC, nflags: u32) -> i32;
}

/// Official reference: `Direct3D11CaptureFramePool.CreateFreeThreaded(..., 1, size)`
/// (all_functions.c L53825 Recreate / L232061 Create). DSH deliberately allocates 2:
/// the arrival handler keeps a frame parked in `latest`, and with a single buffer that
/// would leave the pool unable to produce the next one, so every screenshot would be
/// frozen at whatever it captured first. The *behaviour* (encode the newest available
/// frame) matches official; only the buffer count differs, and the deviation is
/// recorded in `parity/official-constants.json` under `deviations.framePoolBuffers`.
pub const FRAMEPOOL_BUFFERS: i32 = 2;

/// Official `PropertyValue::CreateSingle(0.8f)` — float @VA 0x14012c74c is
/// `cd cc 4c 3f`, used with the `ImageQuality` bitmap property at listing 140043b6d /
/// 140043bc0 (CW-1). See `parity/official-constants.json` → `capture.jpegQuality`.
pub const JPEG_QUALITY: f32 = 0.8;

/// Official `SetIsCursorCaptureEnabled(true)` (listing 14003e341: `MOV DL,0x1`).
/// With WGC cursor capture on, the real pointer is composited into the frame and the
/// system-cursor manager suppresses it; the overlay pointer is what the model sees.
pub const CURSOR_CAPTURE: bool = true;

/// Official `SetIsBorderRequired(false)` (listing 14003e3df: `XOR EDX,EDX`).
pub const BORDER_REQUIRED: bool = false;

/// Official physical→96-DPI logical conversion (`FUN_14004572d` @0x14004572d).
/// Recorded here so the gate can compare the formula text with the JSON.
pub const DPI_LOGICAL_PIXELS: &str = "max(1, (v*96 + dpi/2)/dpi)";

/// Every value `LAST_CAPTURE_PATH` may take. Only `wgc` exists in the official
/// helper; the `gdi-*` entries are the documented DSH fallback extension (CW-7).
pub const CAPTURE_PATHS: &[&str] = &[
    "wgc",
    "gdi-bitblt",
    "gdi-printwindow",
    "gdi-screen-bitblt",
    "gdi-window-bitblt",
];

/// The official capture path. Anything else is a DSH-only fallback whose pixels may
/// not reflect occlusion (the frame can show a covering window instead of the target).
pub const OFFICIAL_CAPTURE_PATH: &str = "wgc";
/// Official `no monitor found for window` (strings_all 0x138ee6). The distinct
/// official string `no screenshot targets found for ` belongs to the empty screenshot
/// target set and is owned by `state.rs`.
const NO_MONITOR_FOR_WINDOW: &str = "no monitor found for window";
const CROP_OUTSIDE: &str = "window crop is outside captured monitor";
const CAPTURE_TIMEOUT: &str = "window capture timed out";
const SEND_CAPTURE: &str = "send window capture request";
const WORKER_NOT_RUNNING: &str = "window capture worker is not running";
const FRAME_ARRIVED_TIMEOUT: &str = "window capture timed out";
const TRYGET_AFTER_ARRIVED: &str = "TryGetNextFrame after FrameArrived failed";
const CACHE_FAILED: &str = "computer-use cached capture session failed: ";
const UNEXPECTED_PIXEL: &str = "captured bitmap has unexpected pixel format: ";
/// Display-overlay window classes that must be hidden from capture.
///
/// IMPORTANT: the cursor window classes are deliberately absent. Official
/// (Ghidra 14004791d:292) applies WDA_EXCLUDEFROMCAPTURE to the *display*
/// overlay only; the fake cursor is the model's own pointer and must stay
/// visible in the screenshots the model reads. Adding a cursor class here
/// silently re-breaks that, because this sweep runs on every observe.
const OVERLAY_CLASSES: &[&str] = &[
    "DshComputerUseCursorOverlay",
    "CodexComputerUseCursorOverlay",
];

/// Which code path produced the most recent frame ("wgc", "gdi-bitblt",
/// "gdi-printwindow", "gdi-window-bitblt"). Exposed through the diagnostic tool so
/// a missing cursor overlay can be attributed to the capture path instead of
/// guessed at.
static LAST_CAPTURE_PATH: Mutex<String> = Mutex::new(String::new());

fn note_capture_path(path: &str) {
    if let Ok(mut slot) = LAST_CAPTURE_PATH.lock() {
        *slot = path.to_string();
    }
}

/// The capture path used for the most recent frame. Empty before the first capture.
pub fn capture_path() -> String {
    LAST_CAPTURE_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Every value `capture_path()` may legitimately take (CW-7 gate G6).
pub fn capture_path_is_known(path: &str) -> bool {
    path.is_empty() || CAPTURE_PATHS.contains(&path)
}

/// Last occlusion verdict, so a "captured the wrong window" report can be
/// attributed to the check instead of guessed at.
static LAST_COVERED: Mutex<Option<bool>> = Mutex::new(None);

fn note_covered(covered: bool) {
    if let Ok(mut slot) = LAST_COVERED.lock() {
        *slot = Some(covered);
    }
}

/// Whether the target is covered, recording the verdict for diagnostics.
fn target_covered(hwnd: isize) -> bool {
    let covered = crate::enum_windows::is_covered(as_hwnd(hwnd));
    note_covered(covered);
    covered
}

/// External diagnostic: the full per-sample occlusion picture for a window.
pub fn coverage_probe(hwnd: isize) -> serde_json::Value {
    let mut detail = crate::enum_windows::is_covered_detail(as_hwnd(hwnd));
    detail["lastPath"] = serde_json::json!(capture_path());
    if let Ok(slot) = LAST_COVERED.lock() {
        detail["lastCovered"] = serde_json::json!(*slot);
    }
    detail
}

/// CW-7: official has exactly one capture path (`wgc`) and fails loudly when it is
/// unavailable. DSH keeps GDI fallbacks, so the chosen path and whether it is the
/// official one must be visible; a `gdi-*` frame may show a covering window instead
/// of the target.
pub fn capture_diagnostics() -> serde_json::Value {
    let path = capture_path();
    let display = crate::overlay::display_hwnds().len();
    let covered = LAST_COVERED.lock().ok().and_then(|slot| *slot);
    serde_json::json!({
        "path": path,
        "officialPath": OFFICIAL_CAPTURE_PATH,
        "knownPaths": CAPTURE_PATHS,
        "fallback": !path.is_empty() && path != OFFICIAL_CAPTURE_PATH,
        "gdiFallbackIsDshExtension": true,
        "displayOverlays": display,
        "lastCovered": covered,
    })
}

static OVERLAY_HWNDS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static CACHE: Mutex<Option<HashMap<isize, CachedPool>>> = Mutex::new(None);
static LAST_INVALIDATION: Mutex<String> = Mutex::new(String::new());

#[derive(Clone, Debug)]
pub struct CaptureFrame {
    pub jpeg_bytes: Vec<u8>,
    pub origin_x: i32,
    pub origin_y: i32,
    pub phys_w: i32,
    pub phys_h: i32,
    pub logical_w: i32,
    pub logical_h: i32,
    pub dpi: u32,
}

impl CaptureFrame {
    pub fn data_url(&self) -> String {
        use base64::Engine;
        format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&self.jpeg_bytes)
        )
    }
}

struct MonitorFrame {
    origin_x: i32,
    origin_y: i32,
    width: i32,
    height: i32,
}

/// The newest composed frame, parked by the arrival handler.
///
/// WGC hands frames out oldest-first and a full buffer makes it drop newer ones, so
/// pulling only on demand made every screenshot show the composition from one
/// request earlier (measured: the capture after a click still showed the previous
/// click's cursor, and the capture after a button press showed the pre-press text).
/// Consuming eagerly and keeping the newest frame is what makes a capture reflect
/// the action that has just been performed.
struct FrameSlot(Direct3D11CaptureFrame);

unsafe impl Send for FrameSlot {}

struct CachedPool {
    _d3d: ID3D11Device,
    _ctx: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    width: i32,
    height: i32,
    arrived_event: HANDLE,
    latest: Arc<Mutex<Option<FrameSlot>>>,
    token: Option<i64>,
    _arrived: Option<TypedEventHandler<Direct3D11CaptureFramePool, windows::core::IInspectable>>,
}

unsafe impl Send for CachedPool {}
unsafe impl Sync for CachedPool {}

impl Drop for CachedPool {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.latest.lock() {
            *slot = None;
        }
        if let Some(token) = self.token.take() {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
        if !self.arrived_event.is_invalid() {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.arrived_event) };
        }
    }
}

pub fn capture_hwnd(hwnd: isize) -> Result<CaptureFrame> {
    capture_hwnd_timeout(hwnd, 800)
}

struct CaptureJob {
    hwnd: isize,
    timeout_ms: u32,
    resp: mpsc::SyncSender<Result<CaptureFrame, String>>,
    /// True when the pill was masked for this capture: the frame pool's parked frame
    /// predates the mask, so the newest frame has to be dropped and a frame arriving
    /// after the mask has to be waited for. Without this the capture returns the
    /// pre-mask frame and the model reads the pill back.
    fresh: bool,
}

static WORKER: Mutex<Option<mpsc::Sender<CaptureJob>>> = Mutex::new(None);

fn capture_worker_tx() -> Result<mpsc::Sender<CaptureJob>> {
    let mut slot = WORKER.lock().map_err(|_| anyhow!(WORKER_NOT_RUNNING))?;
    if let Some(tx) = slot.as_ref() {
        return Ok(tx.clone());
    }
    let (tx, rx) = mpsc::channel::<CaptureJob>();
    thread::Builder::new()
        .name("cu-capture".into())
        .spawn(move || {
            let _ = ensure_runtime();
            enable_per_monitor_dpi();
            enable_thread_dpi();
            for job in rx {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    capture_one(job.hwnd, job.timeout_ms, job.fresh)
                }))
                .unwrap_or_else(|_| Err(anyhow!(CAPTURE_TIMEOUT)));
                let _ = job.resp.send(result.map_err(|e| e.to_string()));
            }
            if let Ok(mut slot) = WORKER.lock() {
                *slot = None;
            }
        })
        .map_err(|_| anyhow!(WORKER_NOT_RUNNING))?;
    *slot = Some(tx.clone());
    Ok(tx)
}

pub fn capture_hwnd_timeout(hwnd: isize, timeout_ms: u32) -> Result<CaptureFrame> {
    enable_per_monitor_dpi();
    enable_thread_dpi();
    ensure_runtime().context("RoInitialize failed")?;
    // The mask has to come off on every path, including the error paths: a pill that
    // stays hidden because a capture failed is the failure mode this whole mechanism
    // exists to prevent. `restore_overlays` is idempotent and cheap when no mask ran.
    let fresh = exclude_overlays();
    let result = capture_hwnd_timeout_inner(hwnd, timeout_ms, fresh);
    restore_overlays();
    result
}

fn capture_hwnd_timeout_inner(hwnd: isize, timeout_ms: u32, fresh: bool) -> Result<CaptureFrame> {
    let mut last = anyhow!(WORKER_NOT_RUNNING);
    for _ in 0..2 {
        let tx = match capture_worker_tx() {
            Ok(tx) => tx,
            Err(err) => {
                last = err;
                continue;
            }
        };
        let (resp_tx, resp_rx) = mpsc::sync_channel(1);
        if tx
            .send(CaptureJob {
                hwnd,
                timeout_ms,
                resp: resp_tx,
                fresh,
            })
            .is_err()
        {
            if let Ok(mut slot) = WORKER.lock() {
                *slot = None;
            }
            last = anyhow!(WORKER_NOT_RUNNING);
            continue;
        }
        let wait = Duration::from_millis(timeout_ms.max(1) as u64).saturating_add(Duration::from_millis(2500));
        return match resp_rx.recv_timeout(wait) {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(err)) => {
                if err.contains(CAPTURE_TIMEOUT) {
                    Err(anyhow!(CAPTURE_TIMEOUT))
                } else if err.contains(WORKER_NOT_RUNNING) {
                    Err(anyhow!(WORKER_NOT_RUNNING))
                } else {
                    Err(anyhow!(err))
                }
            }
            Err(_) => Err(anyhow!(CAPTURE_TIMEOUT)),
        };
    }
    Err(last)
}

pub fn register_overlay_hwnd(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let mut list = OVERLAY_HWNDS.lock().unwrap_or_else(|e| e.into_inner());
    if !list.contains(&hwnd) {
        list.push(hwnd);
    }
}

pub fn exclude_from_capture(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    unsafe { SetWindowDisplayAffinity(as_hwnd(hwnd), WDA_EXCLUDEFROMCAPTURE) }.is_ok()
}

pub fn wgc_available() -> bool {
    ensure_runtime().is_ok()
        && windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().is_ok()
}

pub fn cached_session_count() -> usize {
    CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|m| m.len()))
        .unwrap_or(0)
}

pub fn last_capture_invalidation() -> String {
    LAST_INVALIDATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn note_invalidation(reason: impl Into<String>) {
    if let Ok(mut slot) = LAST_INVALIDATION.lock() {
        *slot = reason.into();
    }
}

pub fn invalidate_cached_sessions(reason: &str) {
    note_invalidation(reason);
    if let Ok(mut guard) = CACHE.lock() {
        *guard = None;
    }
}

pub fn clamp_crop(
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    src_w: i32,
    src_h: i32,
) -> Result<(i32, i32, i32, i32)> {
    let x0 = left.max(0).min(src_w);
    let y0 = top.max(0).min(src_h);
    let x1 = (left + width).max(0).min(src_w);
    let y1 = (top + height).max(0).min(src_h);
    if x1 <= x0 || y1 <= y0 {
        bail!("{CROP_OUTSIDE}");
    }
    Ok((x0, y0, x1 - x0, y1 - y0))
}

pub fn scaled_size(width: i32, height: i32, dpi: u32) -> (i32, i32) {
    (scale_one(width, dpi), scale_one(height, dpi))
}

fn scale_one(n: i32, dpi: u32) -> i32 {
    let d = dpi.max(1);
    let v = (n as i64 * 96 + (d as i64 / 2)) / d as i64;
    if v < 2 {
        1
    } else {
        v as i32
    }
}

fn as_hwnd(hwnd: isize) -> HWND {
    HWND(hwnd as *mut core::ffi::c_void)
}

fn hmon_key(hmon: HMONITOR) -> isize {
    hmon.0 as isize
}

fn enable_per_monitor_dpi() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        unsafe {
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE);
            }
            let _ = SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
        }
    });
}

fn enable_thread_dpi() {
    unsafe {
        let prev = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if prev.0.is_null() {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE);
        }
    }
}

fn ensure_runtime() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    match INIT.get_or_init(|| {
        unsafe {
            let _cookie = CoIncrementMTAUsage();
            match RoInitialize(RO_INIT_MULTITHREADED) {
                Ok(()) => {}
                Err(e) if e.code().0 == 1 => {}
                Err(e) if e.code().0 as u32 == 0x80010106 => {}
                Err(e) => return Err(format!("RoInitialize failed: {e}")),
            }
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        Ok(())
    }) {
        Ok(()) => Ok(()),
        Err(e) => bail!("{e}"),
    }
}

/// Keep our own overlay out of the frame that is about to be composed.
///
/// `Mask` (the default) hides the pill in the compositor for the duration of the
/// capture: the operator keeps seeing it, the model never reads it back, and no window
/// affinity is involved. `Wda` applies `WDA_EXCLUDEFROMCAPTURE` instead -- the
/// official-style affinity, which on some Windows/DWM/GPU combinations stops the DWM
/// from presenting the DirectComposition content on screen as well (operator sees the
/// fake cursor and no pill). `Off` leaves the pill in the frame.
/// Returns true when the capture is masked and therefore needs a frame composed after
/// the mask (see the `fresh` field of `CaptureJob`).
fn exclude_overlays() -> bool {
    match crate::overlay::capture_exclusion() {
        crate::overlay::CaptureExclusion::Mask => {
            return crate::overlay::mask_for_capture();
        }
        crate::overlay::CaptureExclusion::Off => return false,
        crate::overlay::CaptureExclusion::Wda => {}
    }
    let registered: Vec<isize> = OVERLAY_HWNDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    for hwnd in registered {
        // The registry carries every overlay window, cursor included. Excluding
        // the cursor here would erase the model's own pointer from the next
        // screenshot, which is the same FIX-1 regression as the class sweep.
        if !is_display_overlay(hwnd) {
            continue;
        }
        let _ = exclude_from_capture(hwnd);
    }
    unsafe {
        let _ = EnumWindows(Some(enum_overlay_proc), LPARAM(0));
    }
    // The affinity mode composes with the pill still in the frame.
    false
}

/// Lift whatever `exclude_overlays()` applied for the frame that has just been taken.
fn restore_overlays() {
    if crate::overlay::capture_exclusion() == crate::overlay::CaptureExclusion::Mask {
        crate::overlay::unmask_after_capture();
    }
}

/// True when the window class is one of the display overlays (status pill /
/// shimmer). Anything else in the registry -- notably the cursor window -- must
/// stay capturable.
fn is_display_overlay(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(HWND(hwnd as *mut core::ffi::c_void), &mut buf) };
    if n <= 0 {
        return false;
    }
    let class = String::from_utf16_lossy(&buf[..n as usize]);
    OVERLAY_CLASSES.iter().any(|c| class.eq_ignore_ascii_case(c))
}

unsafe extern "system" fn enum_overlay_proc(hwnd: HWND, _lparam: LPARAM) -> windows::core::BOOL {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n > 0 {
        let class = String::from_utf16_lossy(&buf[..n as usize]);
        if OVERLAY_CLASSES.iter().any(|c| class.eq_ignore_ascii_case(c)) {
            let _ = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) };
        }
    }
    BOOL(1)
}

fn window_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi > 0 {
        return dpi;
    }
    let dpi = unsafe { GetDpiForSystem() };
    if dpi > 0 {
        dpi
    } else {
        96
    }
}

fn window_rect(hwnd: HWND) -> Result<(i32, i32, i32, i32)> {
    let mut rc = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rc) }.context("GetWindowRect failed")?;
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;
    if width <= 0 || height <= 0 {
        bail!("window has invalid bounds or is not visible");
    }
    Ok((rc.left, rc.top, width, height))
}

fn monitor_frame_for_hwnd(hwnd: HWND) -> Result<(HMONITOR, MonitorFrame)> {
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_invalid() {
        bail!("{NO_MONITOR_FOR_WINDOW}");
    }
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmon, &mut info) };
    if !ok.as_bool() {
        bail!("GetMonitorInfoW failed");
    }
    let rc = info.rcMonitor;
    Ok((
        hmon,
        MonitorFrame {
            origin_x: rc.left,
            origin_y: rc.top,
            width: rc.right - rc.left,
            height: rc.bottom - rc.top,
        },
    ))
}

fn create_item(hwnd: HWND) -> Result<(GraphicsCaptureItem, SizeInt32, HMONITOR)> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .context("capture factory")?;
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_invalid() {
        bail!("{NO_MONITOR_FOR_WINDOW}");
    }
    let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(hmon) }
        .context("IGraphicsCaptureItemInterop.CreateForMonitor failed")?;
    let size = item.Size().context("GraphicsCaptureItem.Size failed")?;
    if size.Width < 1 || size.Height < 1 {
        bail!("capture item has invalid size");
    }
    Ok((item, size, hmon))
}

fn create_d3d() -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice)> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None::<&[D3D_FEATURE_LEVEL]>,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("D3D11CreateDevice failed")?;
    let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice failed"))?;
    let context = context.ok_or_else(|| anyhow!("D3D11CreateDevice failed"))?;
    let dxgi: IDXGIDevice = device.cast().context("cast D3D device to IDXGIDevice")?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .context("CreateDirect3D11DeviceFromDXGIDevice failed")?;
    let winrt: IDirect3DDevice = inspectable.cast().context("cast WinRT Direct3D device")?;
    Ok((device, context, winrt))
}

fn new_pool(item: GraphicsCaptureItem, size: SizeInt32) -> Result<CachedPool> {
    let (d3d, ctx, winrt_device) = create_d3d()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        FRAMEPOOL_BUFFERS,
        size,
    )
    .context("Direct3D11CaptureFramePool.CreateFreeThreaded failed")?;
    let session = pool
        .CreateCaptureSession(&item)
        .context("CreateCaptureSession failed")?;
    session
        .SetIsCursorCaptureEnabled(CURSOR_CAPTURE)
        .context("SetIsCursorCaptureEnabled failed")?;
    session
        .SetIsBorderRequired(BORDER_REQUIRED)
        .context("SetIsBorderRequired failed")?;

    let arrived_event = unsafe { CreateEventW(None, true, false, None) }.unwrap_or_default();
    let latest: Arc<Mutex<Option<FrameSlot>>> = Arc::new(Mutex::new(None));
    let mut token = None;
    let mut handler = None;
    if !arrived_event.is_invalid() {
        let event_raw = arrived_event.0 as isize;
        let slot = latest.clone();
        let arrived = TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
            move |sender, _args| {
                // Take the frame immediately so the pool's buffers are always free for
                // the next composition; keeping the newest one is what stops the
                // capture from lagging an action behind.
                if let Some(pool) = sender.as_ref() {
                    if let Ok(frame) = pool.TryGetNextFrame() {
                        if let Ok(mut guard) = slot.lock() {
                            *guard = Some(FrameSlot(frame));
                        }
                    }
                }
                unsafe {
                    let _ = SetEvent(HANDLE(event_raw as *mut core::ffi::c_void));
                }
                Ok(())
            },
        );
        match pool.FrameArrived(&arrived) {
            Ok(t) => {
                token = Some(t);
                handler = Some(arrived);
            }
            Err(_) => {
                let _ = unsafe { windows::Win32::Foundation::CloseHandle(arrived_event) };
            }
        }
    }
    let arrived_event = if handler.is_some() {
        arrived_event
    } else {
        HANDLE::default()
    };

    session.StartCapture().context("StartCapture failed")?;
    Ok(CachedPool {
        _d3d: d3d,
        _ctx: ctx,
        winrt_device,
        pool,
        session,
        width: size.Width,
        height: size.Height,
        arrived_event,
        latest,
        token,
        _arrived: handler,
    })
}

impl CachedPool {
    fn take_latest(&self) -> Option<Direct3D11CaptureFrame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take()).map(|s| s.0)
    }

    fn recreate(&mut self, size: SizeInt32) -> Result<()> {
        self.pool
            .Recreate(
                &self.winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                FRAMEPOOL_BUFFERS,
                size,
            )
            .context("Direct3D11CaptureFramePool.Recreate failed")?;
        self.width = size.Width;
        self.height = size.Height;
        Ok(())
    }
}

fn with_cached_pool<T>(hwnd: HWND, f: impl FnOnce(&CachedPool) -> Result<T>) -> Result<T> {
    let (item, size, hmon) = create_item(hwnd)?;
    let key = hmon_key(hmon);
    let mut guard = CACHE
        .lock()
        .map_err(|_| anyhow!("{CACHE_FAILED}lock"))?;
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(cached) = map.get_mut(&key) {
        if cached.width != size.Width || cached.height != size.Height {
            if let Err(err) = cached.recreate(size) {
                map.remove(&key);
                return Err(anyhow!("{CACHE_FAILED}{err}"));
            }
        }
        return f(map.get(&key).expect("cached pool"));
    }
    let cached = new_pool(item, size).map_err(|e| anyhow!("{CACHE_FAILED}{e}"))?;
    map.insert(key, cached);
    f(map.get(&key).expect("cached pool"))
}

fn take_frame(cached: &CachedPool, wait_ms: u32) -> Result<Direct3D11CaptureFrame> {
    if let Ok(frame) = cached.pool.TryGetNextFrame() {
        return Ok(frame);
    }
    if !cached.arrived_event.is_invalid() {
        let _ = unsafe { ResetEvent(cached.arrived_event) };
        let wr = unsafe { WaitForSingleObject(cached.arrived_event, wait_ms) };
        let _ = unsafe { ResetEvent(cached.arrived_event) };
        if wr == WAIT_OBJECT_0 {
            if let Ok(frame) = cached.pool.TryGetNextFrame() {
                return Ok(frame);
            }
        }
    }
    if let Some(frame) = poll_frame(&cached.pool, 120) {
        return Ok(frame);
    }
    bail!("{CAPTURE_TIMEOUT}");
}

/// A frame that arrived *after* this call. The arrival handler parks the newest frame
/// it saw, which for a masked capture is the one composed before the pill disappeared;
/// dropping it is what makes the capture show the masked desktop.
fn wait_fresh_frame(cached: &CachedPool, timeout_ms: u32) -> Result<Direct3D11CaptureFrame> {
    let _ = cached.take_latest();
    take_frame(cached, timeout_ms.clamp(50, 400) as u32)
}

/// Newest available frame: whatever the arrival handler parked most recently.
fn wait_frame(cached: &CachedPool, timeout_ms: u32) -> Result<Direct3D11CaptureFrame> {
    let wait_ms = timeout_ms.clamp(50, 400) as u64;
    let deadline = Instant::now() + Duration::from_millis(wait_ms + 300);
    loop {
        if let Some(frame) = cached.take_latest() {
            return Ok(frame);
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(4));
    }
    // No parked frame (for example when the handler could not be registered):
    // fall back to pulling one directly.
    take_frame(cached, wait_ms as u32)
}

fn drop_cached_sessions() {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = None;
    }
}

fn poll_frame(pool: &Direct3D11CaptureFramePool, timeout_ms: u64) -> Option<Direct3D11CaptureFrame> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    while Instant::now() < deadline {
        if let Ok(frame) = pool.TryGetNextFrame() {
            return Some(frame);
        }
        thread::sleep(Duration::from_millis(16));
    }
    None
}

fn capture_one(hwnd: isize, timeout_ms: u32, fresh: bool) -> Result<CaptureFrame> {
    match capture_wgc(hwnd, timeout_ms, fresh) {
        Ok(frame) => {
            note_capture_path("wgc");
            Ok(frame)
        }
        Err(wgc) => {
            drop_cached_sessions();
            let gdi = with_overlay_hidden(|| capture_gdi_bitblt_only(hwnd))
                .map(|frame| {
                    note_capture_path("gdi-bitblt");
                    frame
                })
                .or_else(|first| {
                    capture_gdi(hwnd).map(|frame| {
                        note_capture_path("gdi-printwindow");
                        frame
                    })
                    .map_err(|second| anyhow!("{first}; {second}"))
                });
            gdi.map_err(|gdi| anyhow!("{CACHE_FAILED}{wgc}; {gdi}"))
        }
    }
}

fn with_overlay_hidden<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    // Display overlays only: hiding the cursor window here would erase the model's
    // own pointer from every GDI-fallback screenshot (FIX-1).
    let hwnds: Vec<HWND> = crate::overlay::display_hwnds()
        .into_iter()
        .filter(|id| *id != 0)
        .map(as_hwnd)
        .collect();
    let visible = crate::overlay::visible();
    if visible {
        unsafe {
            for hwnd in &hwnds {
                let _ = ShowWindow(*hwnd, SW_HIDE);
            }
        }
        thread::sleep(Duration::from_millis(16));
    }
    let result = f();
    if visible {
        unsafe {
            for hwnd in &hwnds {
                let _ = ShowWindow(*hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
    result
}

fn capture_wgc(hwnd_raw: isize, timeout_ms: u32, fresh: bool) -> Result<CaptureFrame> {
    let hwnd = as_hwnd(hwnd_raw);
    if hwnd_raw == 0 || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        bail!("window id is required");
    }
    // WGC captures a MONITOR (`CreateForMonitor` in `create_item`), not the window:
    // it records the composited desktop for the display that hosts the target and
    // then crops to the window rect. That means the frame shows whatever is on top
    // at those coordinates — a covered target yields the *covering* window's pixels
    // while the tool still reports `ok`. Observed live 2026-10-20 (TapTap behind the
    // Harness window): two observations returned the Harness chat.
    //
    // Refusing here sends the caller down the GDI path, which has a window-DC
    // fallback that cannot pick up a covering window. Failing loudly beats
    // answering with someone else's screen.
    if target_covered(hwnd_raw) {
        bail!(
            "target window is covered by another window; monitor capture would return the \
             covering window's pixels"
        );
    }
    let (left, top, win_w, win_h) = window_rect(hwnd)?;
    let (_hmon, mon) = monitor_frame_for_hwnd(hwnd)?;
    let (crop_x, crop_y, crop_w, crop_h) =
        clamp_crop(left - mon.origin_x, top - mon.origin_y, win_w, win_h, mon.width, mon.height)?;
    let dpi = window_dpi(hwnd);
    let (logical_w, logical_h) = scaled_size(crop_w, crop_h, dpi);

    with_cached_pool(hwnd, |cached| {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1) as u64);
        let frame = if fresh {
            wait_fresh_frame(cached, timeout_ms)
        } else {
            wait_frame(cached, timeout_ms)
        }
        .context(CAPTURE_TIMEOUT)?;
        let surface = frame.Surface().context(SEND_CAPTURE)?;
        let jpeg = encode_from_surface(
            &surface,
            crop_x,
            crop_y,
            crop_w,
            crop_h,
            logical_w,
            logical_h,
            deadline,
        )
        .context(SEND_CAPTURE)?;
        Ok(CaptureFrame {
            jpeg_bytes: jpeg,
            origin_x: left,
            origin_y: top,
            phys_w: crop_w,
            phys_h: crop_h,
            logical_w,
            logical_h,
            dpi,
        })
    })
}

fn wait_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
    deadline: Instant,
) -> Result<T> {
    let remain = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(remain) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(anyhow!(CAPTURE_TIMEOUT)),
    }
}

fn encode_from_surface(
    surface: &windows::Graphics::DirectX::Direct3D11::IDirect3DSurface,
    crop_x: i32,
    crop_y: i32,
    crop_w: i32,
    crop_h: i32,
    logical_w: i32,
    logical_h: i32,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let op = SoftwareBitmap::CreateCopyFromSurfaceAsync(surface)
        .context("SoftwareBitmap.CreateCopyFromSurfaceAsync failed")?;
    let bitmap: SoftwareBitmap = wait_blocking(move || op.get().map_err(|e| anyhow!(e)), deadline)
        .context("copy Direct3D surface to SoftwareBitmap failed")?;
    let fmt = bitmap.BitmapPixelFormat()?;
    if fmt != BitmapPixelFormat::Bgra8 {
        bail!("{UNEXPECTED_PIXEL}{}", fmt.0);
    }
    let cropped = cpu_crop(&bitmap, crop_x, crop_y, crop_w, crop_h)?;
    encode_software_bitmap(&cropped, logical_w, logical_h, deadline)
}

fn software_bitmap_bgra(bitmap: &SoftwareBitmap) -> Result<(i32, i32, Vec<u8>)> {
    let width = bitmap.PixelWidth().context("SoftwareBitmap.PixelWidth failed")?;
    let height = bitmap.PixelHeight().context("SoftwareBitmap.PixelHeight failed")?;
    let len = (width as u32)
        .checked_mul(height as u32)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| anyhow!("SoftwareBitmap.PixelWidth failed"))?;
    let buffer = Buffer::Create(len)?;
    buffer.SetLength(len)?;
    bitmap.CopyToBuffer(&buffer)?;
    Ok((width, height, read_buffer(&buffer)?))
}

fn cpu_crop(
    bitmap: &SoftwareBitmap,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<SoftwareBitmap> {
    let (src_w, src_h, bgra) = software_bitmap_bgra(bitmap)?;
    let (x, y, w, h) = clamp_crop(x, y, w, h, src_w, src_h)?;
    let tiled = crop_bgra(&bgra, src_w, src_h, x, y, w, h);
    bitmap_from_bgra(w, h, &tiled)
}

fn crop_bgra(src: &[u8], src_w: i32, src_h: i32, x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
    let _ = src_h;
    let stride = src_w as usize * 4;
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for row in 0..h as usize {
        let src_off = (y as usize + row) * stride + x as usize * 4;
        let dst_off = row * w as usize * 4;
        let n = w as usize * 4;
        out[dst_off..dst_off + n].copy_from_slice(&src[src_off..src_off + n]);
    }
    out
}

fn bitmap_from_bgra(width: i32, height: i32, bgra: &[u8]) -> Result<SoftwareBitmap> {
    let buf = write_buffer(bgra)?;
    SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, width, height)
        .or_else(|_| {
            SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
                &buf,
                BitmapPixelFormat::Bgra8,
                width,
                height,
                BitmapAlphaMode::Straight,
            )
        })
        .context("copy Direct3D surface to SoftwareBitmap failed")
}

fn read_buffer(buffer: &Buffer) -> Result<Vec<u8>> {
    let len = buffer.Length()? as usize;
    let access: IBufferByteAccess = buffer.cast()?;
    let ptr = unsafe { access.Buffer()? };
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

fn write_buffer(bytes: &[u8]) -> Result<Buffer> {
    let buf = Buffer::Create(bytes.len() as u32)?;
    buf.SetLength(bytes.len() as u32)?;
    let access: IBufferByteAccess = buf.cast()?;
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), access.Buffer()?, bytes.len());
    }
    Ok(buf)
}

fn encode_software_bitmap(
    bitmap: &SoftwareBitmap,
    logical_w: i32,
    logical_h: i32,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let stream = InMemoryRandomAccessStream::new().context("InMemoryRandomAccessStream failed")?;
    let jpeg_id = BitmapEncoder::JpegEncoderId().context("create JPEG encoder failed")?;
    let create = BitmapEncoder::CreateAsync(jpeg_id, &stream).context("BitmapEncoder.CreateAsync failed")?;
    let encoder: BitmapEncoder = wait_blocking(move || create.get().map_err(|e| anyhow!(e)), deadline)
        .context("create JPEG encoder failed")?;
    encoder
        .SetSoftwareBitmap(bitmap)
        .context("create JPEG encoder failed")?;
    let transform = encoder
        .BitmapTransform()
        .context("BitmapEncoder.BitmapTransform failed")?;
    transform
        .SetScaledWidth(logical_w.max(1) as u32)
        .context("BitmapEncoder.BitmapTransform failed")?;
    transform
        .SetScaledHeight(logical_h.max(1) as u32)
        .context("BitmapEncoder.BitmapTransform failed")?;
    let _ = apply_image_quality(&encoder, deadline);
    let flush = encoder.FlushAsync().context("flush JPEG encoder failed")?;
    wait_blocking(move || flush.get().map_err(|e| anyhow!(e)), deadline).context("flush JPEG encoder failed")?;
    read_jpeg_stream(&stream, deadline)
}

fn apply_image_quality(encoder: &BitmapEncoder, deadline: Instant) -> Result<()> {
    let boxed = PropertyValue::CreateSingle(JPEG_QUALITY)?;
    let typed = BitmapTypedValue::Create(&boxed, PropertyType::Single)
        .context("BitmapTypedValue.Create failed")?;
    let props = BitmapPropertySet::new().context("BitmapPropertySet.new failed")?;
    props
        .Insert(&HSTRING::from("ImageQuality"), &typed)
        .context("BitmapPropertySet.Insert failed")?;
    let set_props = encoder.BitmapProperties()?.SetPropertiesAsync(&props)?;
    wait_blocking(move || set_props.get().map_err(|e| anyhow!(e)), deadline)?;
    Ok(())
}

fn read_jpeg_stream(stream: &InMemoryRandomAccessStream, deadline: Instant) -> Result<Vec<u8>> {
    stream.Seek(0)?;
    let size = stream.Size().context("read JPEG stream size failed")?;
    let input = stream
        .GetInputStreamAt(0)
        .context("read JPEG stream failed")?;
    let reader = DataReader::CreateDataReader(&input).context("read JPEG stream failed")?;
    let load = reader.LoadAsync(size as u32).context("read JPEG stream failed")?;
    wait_blocking(move || load.get().map_err(|e| anyhow!(e)), deadline).context("read JPEG stream failed")?;
    let mut buf = vec![0u8; size as usize];
    reader
        .ReadBytes(&mut buf)
        .context("read JPEG stream failed")?;
    if !buf.starts_with(&[0xFF, 0xD8]) {
        bail!("read JPEG stream failed");
    }
    Ok(buf)
}

fn encode_jpeg_bgra(width: i32, height: i32, bgra: &[u8], logical_w: i32, logical_h: i32) -> Result<Vec<u8>> {
    if let Ok(bitmap) = bitmap_from_bgra(width, height, bgra) {
        if let Ok(jpeg) = encode_software_bitmap(&bitmap, logical_w, logical_h, Instant::now() + Duration::from_millis(800)) {
            return Ok(jpeg);
        }
    }
    encode_jpeg_image_crate(width, height, bgra)
}

fn encode_jpeg_image_crate(width: i32, height: i32, bgra: &[u8]) -> Result<Vec<u8>> {
    let w = width as u32;
    let h = height as u32;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in bgra.chunks_exact(4) {
        rgb.push(px[2]);
        rgb.push(px[1]);
        rgb.push(px[0]);
    }
    let mut out = Vec::new();
    // The fallback encoder must use the same official quality as the WinRT path;
    // a hard-coded 85 here silently drifted from CW-1's 0.8 (-> 80).
    let quality = (JPEG_QUALITY * 100.0).round().clamp(1.0, 100.0) as u8;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    encoder
        .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .context("image crate JPEG encode failed")?;
    if !out.starts_with(&[0xFF, 0xD8]) {
        bail!("image crate did not produce JPEG");
    }
    Ok(out)
}

fn skip_printwindow(hwnd_raw: isize) -> bool {
    crate::enum_windows::uses_redirected_composition(as_hwnd(hwnd_raw))
}

/// Screen-DC BitBlt, used as the first GDI fallback when WGC cannot produce a
/// frame.
///
/// A screen BitBlt samples the composited desktop, so it only shows the target
/// while the target is actually on top. When another window covers it, the frame
/// contains the *covering* window's pixels — and the caller still reports `ok`.
/// Observed live (2026-10-20, TapTap login behind the Harness window): this path
/// returned the Harness chat twice.
///
/// When the target is covered, prefer the window DC (which cannot pick up a
/// covering window) and otherwise fail loudly; a wrong frame that looks like a
/// successful observation is the worse outcome.
fn capture_gdi_bitblt_only(hwnd_raw: isize) -> Result<CaptureFrame> {
    let hwnd = as_hwnd(hwnd_raw);
    let (left, top, width, height) = window_rect(hwnd)?;
    let dpi = window_dpi(hwnd);
    let (logical_w, logical_h) = scaled_size(width, height, dpi);
    // A covered target has no trustworthy GDI frame either: the screen DC returns
    // the covering window, and the window DC of a DirectComposition/Electron window
    // returns an empty (black) bitmap. Both would be reported as a successful
    // observation, which is the failure mode this guard exists to prevent — so the
    // honest answer is the error.
    if target_covered(hwnd_raw) {
        bail!(
            "target window is covered by another window; no trustworthy frame is available — \
             call activate_window first"
        );
    }
    let bgra = bitblt_screen_bgra(left, top, width, height)?;
    let jpeg = encode_jpeg_bgra(width, height, &bgra, logical_w, logical_h)?;
    Ok(CaptureFrame {
        jpeg_bytes: jpeg,
        origin_x: left,
        origin_y: top,
        phys_w: width,
        phys_h: height,
        logical_w,
        logical_h,
        dpi,
    })
}

fn capture_gdi(hwnd_raw: isize) -> Result<CaptureFrame> {
    let hwnd = as_hwnd(hwnd_raw);
    let (left, top, width, height) = window_rect(hwnd)?;
    let dpi = window_dpi(hwnd);
    let (logical_w, logical_h) = scaled_size(width, height, dpi);
    unsafe { ShowCursor(false) };
    let result = (|| {
        let bgra = if skip_printwindow(hwnd_raw) {
            // Electron / DirectComposition windows (TapTap, Slack, VS Code…)
            // cannot use PrintWindow without freezing their swapchain, so this
            // branch used to go straight to a screen-DC BitBlt. A screen BitBlt
            // samples the composited desktop: if the target is covered, it
            // captures the *covering* window and still reports success.
            //
            // Observed live (2026-10-20, TapTap login): the Harness window sat on
            // top and two observations returned the Harness chat instead of the
            // TapTap form. A covered Electron window has no trustworthy GDI frame
            // either — the window DC comes back black — so a covered target is
            // reported as an error instead of being answered with a wrong picture.
            let covered = target_covered(hwnd_raw);
            if covered {
                bail!(
                    "target window is covered by another window; no trustworthy frame is \
                     available — call activate_window first"
                );
            } else {
                note_capture_path("gdi-screen-bitblt");
                bitblt_screen_bgra(left, top, width, height)
                    .or_else(|_| {
                        note_capture_path("gdi-window-bitblt");
                        bitblt_window_bgra(hwnd, width, height)
                    })?
            }
        } else {
            print_window_bgra(hwnd, width, height)
                .map(|px| {
                    note_capture_path("gdi-printwindow");
                    px
                })
                .or_else(|_| {
                    note_capture_path("gdi-window-bitblt");
                    bitblt_window_bgra(hwnd, width, height)
                })
                .or_else(|_| {
                    // Last resort is the screen, but only when nothing covers
                    // the target — see the note above.
                    if target_covered(hwnd_raw) {
                        bail!("target window is covered; screen capture refused");
                    }
                    note_capture_path("gdi-screen-bitblt");
                    bitblt_screen_bgra(left, top, width, height)
                })?
        };
        let jpeg = encode_jpeg_bgra(width, height, &bgra, logical_w, logical_h)?;
        Ok(CaptureFrame {
            jpeg_bytes: jpeg,
            origin_x: left,
            origin_y: top,
            phys_w: width,
            phys_h: height,
            logical_w,
            logical_h,
            dpi,
        })
    })();
    unsafe { ShowCursor(true) };
    result
}

fn print_window_bgra(hwnd: HWND, width: i32, height: i32) -> Result<Vec<u8>> {
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.is_invalid() {
        bail!("GetDC failed");
    }
    let mem = unsafe { CreateCompatibleDC(Some(hdc)) };
    let bitmap = unsafe { CreateCompatibleBitmap(hdc, width, height) };
    let prev = unsafe { SelectObject(mem, HGDIOBJ(bitmap.0)) };
    let printed = unsafe { PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT) } != 0;
    let pixels = if printed {
        dibits_bgra(mem, bitmap, width, height)
    } else {
        Err(anyhow!("window capture failed"))
    };
    unsafe {
        SelectObject(mem, prev);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        ReleaseDC(Some(hwnd), hdc);
    }
    pixels
}

fn bitblt_window_bgra(hwnd: HWND, width: i32, height: i32) -> Result<Vec<u8>> {
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.is_invalid() {
        bail!("GetDC failed");
    }
    let mem = unsafe { CreateCompatibleDC(Some(hdc)) };
    let bitmap = unsafe { CreateCompatibleBitmap(hdc, width, height) };
    let prev = unsafe { SelectObject(mem, HGDIOBJ(bitmap.0)) };
    let ok = unsafe { BitBlt(mem, 0, 0, width, height, Some(hdc), 0, 0, SRCCOPY) }.is_ok();
    let pixels = if ok {
        dibits_bgra(mem, bitmap, width, height)
    } else {
        Err(anyhow!("window capture failed"))
    };
    unsafe {
        SelectObject(mem, prev);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        ReleaseDC(Some(hwnd), hdc);
    }
    pixels
}

fn bitblt_screen_bgra(left: i32, top: i32, width: i32, height: i32) -> Result<Vec<u8>> {
    let screen = unsafe { GetDC(None) };
    if screen.is_invalid() {
        bail!("GetDC failed");
    }
    let mem = unsafe { CreateCompatibleDC(Some(screen)) };
    let bitmap = unsafe { CreateCompatibleBitmap(screen, width, height) };
    let prev = unsafe { SelectObject(mem, HGDIOBJ(bitmap.0)) };
    // CAPTUREBLT is required for layered windows (the cursor overlay is
    // WS_EX_LAYERED + SetLayeredWindowAttributes) to appear in a screen-DC
    // BitBlt.  Without it the overlay is silently omitted from every capture,
    // which is exactly the FIX-1 regression we hit once already.
    let rop = SRCCOPY | CAPTUREBLT;
    let ok = unsafe { BitBlt(mem, 0, 0, width, height, Some(screen), left, top, rop) }.is_ok();
    let pixels = if ok {
        dibits_bgra(mem, bitmap, width, height)
    } else {
        Err(anyhow!("monitor capture failed"))
    };
    unsafe {
        SelectObject(mem, prev);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
    pixels
}

fn dibits_bgra(mem: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Result<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let got = unsafe {
        GetDIBits(
            mem,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut core::ffi::c_void),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if got == 0 {
        bail!("GetDIBits failed");
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_size_144dpi() {
        assert_eq!(scaled_size(1500, 900, 144), (1000, 600));
        assert_eq!(scaled_size(96, 96, 96), (96, 96));
        assert_eq!(scale_one(0, 96), 1);
    }

    #[test]
    fn official_capture_timeout_strings() {
        assert_eq!(CAPTURE_TIMEOUT, "window capture timed out");
        assert_eq!(SEND_CAPTURE, "send window capture request");
        assert_eq!(WORKER_NOT_RUNNING, "window capture worker is not running");
        assert_eq!(FRAME_ARRIVED_TIMEOUT, "window capture timed out");
        assert_eq!(TRYGET_AFTER_ARRIVED, "TryGetNextFrame after FrameArrived failed");
        assert_eq!(CACHE_FAILED, "computer-use cached capture session failed: ");
    }

    #[test]
    fn clamp_crop_straddle() {
        let (x, y, w, h) = clamp_crop(-10, 0, 50, 50, 100, 100).unwrap();
        assert_eq!((x, y, w, h), (0, 0, 40, 50));
        assert!(clamp_crop(200, 0, 50, 50, 100, 100).is_err());
    }

    #[test]
    fn crop_bgra_tile() {
        let src = [1u8, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255];
        let out = crop_bgra(&src, 2, 2, 1, 1, 1, 1);
        assert_eq!(out, [4, 0, 0, 255]);
    }

    #[test]
    fn last_capture_invalidation_reason_is_recorded() {
        invalidate_cached_sessions("device");
        assert_eq!(last_capture_invalidation(), "device");
        note_invalidation(format!("{CACHE_FAILED}recreate"));
        assert!(last_capture_invalidation().starts_with(CACHE_FAILED));
        invalidate_cached_sessions("exit");
        assert_eq!(last_capture_invalidation(), "exit");
    }

    /// C8 / G1: `parity/official-constants.json` is the single source of truth. If a
    /// constant here drifts from the official binary value, this test fails (CW-16).
    #[test]
    fn official_constants_single_source_of_truth() {
        let raw = include_str!("../../parity/official-constants.json");
        let c: serde_json::Value =
            serde_json::from_str(raw).expect("parity/official-constants.json must be valid JSON");
        let capture = &c["capture"];
        assert_eq!(JPEG_QUALITY, capture["jpegQuality"].as_f64().unwrap() as f32);
        assert_eq!(capture["jpegQuality"].as_f64(), Some(0.8));
        assert_eq!(CURSOR_CAPTURE, capture["cursorCapture"].as_bool().unwrap());
        assert!(CURSOR_CAPTURE, "official SetIsCursorCaptureEnabled gets true");
        assert_eq!(BORDER_REQUIRED, capture["borderRequired"].as_bool().unwrap());
        assert!(!BORDER_REQUIRED);
        assert_eq!(capture["imageQualityProperty"].as_str(), Some("ImageQuality"));
        assert_eq!(capture["pixelFormat"].as_i64(), Some(87));
        assert_eq!(capture["alphaMode"].as_i64(), Some(1));
        assert_eq!(capture["dpiLogicalPixels"].as_str(), Some(DPI_LOGICAL_PIXELS));
        assert_eq!(capture["wgcOnlyPath"].as_bool(), Some(true));
        // Frame pool: official is 1; DSH's 2 is an explicit, recorded deviation and
        // must never be presented as parity (CW-8).
        assert_eq!(capture["framePoolBuffers"].as_i64(), Some(1));
        assert_eq!(
            FRAMEPOOL_BUFFERS,
            c["deviations"]["framePoolBuffers"].as_i64().unwrap() as i32,
            "an unrecorded frame-pool deviation is exactly what CW-8 warns about"
        );
        assert!(c["deviations"]["framePoolBuffersReason"].as_str().unwrap().len() > 20);
        // maxImageEdge must stay declared as a non-official DSH extension (CW-15), and the
        // declared default must be 0 = official behaviour (decision D-E).
        assert!(c["maxImageEdge"]["official"].is_null());
        assert_eq!(c["maxImageEdge"]["dshExtension"].as_i64(), Some(0));
        assert_eq!(c["maxImageEdge"]["kind"].as_str(), Some("dsh-extension"));
    }

    /// CW-15: DPI normalisation is the *only* scaling the official desktop path does.
    /// There is no maxImageEdge (1280) clamp anywhere in the Rust capture path.
    #[test]
    fn dpi_scaling_has_no_max_image_edge_cap() {
        assert_eq!(scaled_size(2000, 1200, 192), (1000, 600));
        assert_eq!(scaled_size(4000, 3000, 96), (4000, 3000));
        assert_eq!(scaled_size(4000, 3000, 192), (2000, 1500));
        assert_eq!(scaled_size(2560, 1440, 144), (1707, 960));
        assert_eq!(scale_one(1, 192), 1);
        assert_eq!(scale_one(0, 96), 1);
    }

    /// CW-7 / G6: the reported capture path is always one of the known values, and
    /// only `wgc` is the official one.
    #[test]
    fn capture_path_is_a_known_value() {
        assert_eq!(CAPTURE_PATHS.len(), 5);
        assert_eq!(OFFICIAL_CAPTURE_PATH, "wgc");
        assert!(CAPTURE_PATHS.contains(&OFFICIAL_CAPTURE_PATH));
        assert!(capture_path_is_known(""));
        for path in CAPTURE_PATHS {
            assert!(capture_path_is_known(path), "{path}");
        }
        assert!(!capture_path_is_known("gdi-magic"));
    }

    /// CW-13: the monitor-resolution failure string is the official one.
    #[test]
    fn official_monitor_strings() {
        assert_eq!(NO_MONITOR_FOR_WINDOW, "no monitor found for window");
        assert_ne!(NO_MONITOR_FOR_WINDOW, "no screenshot targets found for ");
        assert_eq!(CROP_OUTSIDE, "window crop is outside captured monitor");
        assert_eq!(UNEXPECTED_PIXEL, "captured bitmap has unexpected pixel format: ");
    }
}
