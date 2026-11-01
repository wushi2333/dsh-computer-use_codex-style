//! Stdio helper binary. Overlay/parent never call SetSystemCursor.

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsh_computer_use::audio;
use dsh_computer_use::capture;
use dsh_computer_use::desktop;
use dsh_computer_use::dpi;
use dsh_computer_use::app_catalog;
use dsh_computer_use::enum_windows::{
    activate_hwnd, app_identity_matches, enum_windows, exe_name, extra_space_hwnds, foreground_hwnd,
    hwnd_from_id, is_already_foreground,
    id_from_hwnd, lookup_window, space_identity, window_pid, window_rect, WindowRef,
};
use dsh_computer_use::images;
use dsh_computer_use::input;
use dsh_computer_use::interrupt;
use dsh_computer_use::notify;
use dsh_computer_use::overlay;
use dsh_computer_use::pipe;
use dsh_computer_use::policy;
use dsh_computer_use::prompt;
use dsh_computer_use::protocol::{
    self, capture_timeout_ms, json_f64, json_i64, json_str, official_ok, require_text,
    require_text_allow_empty, Error, Request,
    StartAudioRecordingParams, APPROVED_APP_META_KEY, AUDIO_APP, BUDGET_EXHAUSTED, BUDGET_HEADER,
};
use dsh_computer_use::state::{
    require_observable_window, require_usable_app_window, require_window_spec_app,
    screenshot_targets_missing,
    window_bounds_unavailable_message, HelperState, WindowUse, COORDINATE_GEOMETRY_UNAVAILABLE,
    LAST_WINDOW_IDENTITY, WINDOW_BOUNDS_UNAVAILABLE_COORD,
};
use dsh_computer_use::tools;
use dsh_computer_use::uia::{self, STATE_FLAG_ERROR};
use serde_json::{json, Map, Value};
use windows::core::w;
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Threading::{
    CreateEventW, OpenProcess, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    INFINITE, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CopyIcon, CreateCursor, DestroyCursor, SetSystemCursor, SystemParametersInfoW, HCURSOR, HICON,
    OCR_APPSTARTING, OCR_CROSS, OCR_HAND, OCR_HELP, OCR_IBEAM, OCR_NO, OCR_NORMAL, OCR_SIZEALL,
    OCR_SIZENESW, OCR_SIZENS, OCR_SIZENWSE, OCR_SIZEWE, OCR_UP, OCR_WAIT, SPI_SETCURSORS,
    SYSTEM_CURSOR_ID, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

static RUNNING: AtomicBool = AtomicBool::new(true);
static LAST_RPC: Mutex<Option<Instant>> = Mutex::new(None);
static IN_FLIGHT: AtomicU32 = AtomicU32::new(0);

/// Source revision this binary was built from ("unknown" for ad-hoc
/// `cargo build` runs; scripts/ship-helper.ps1 injects the git hash).
pub const BUILD_REVISION: &str = match option_env!("DSH_CU_BUILD_GIT") {
    Some(rev) => rev,
    None => "unknown",
};

fn main() {
    dpi::enable();
    // Audit P3-2 (2026-10-25): the shipped binary embeds the source revision it
    // was built from (set by scripts/ship-helper.ps1 via DSH_CU_BUILD_GIT). A
    // stale committed exe — source changed, binary not rebuilt — is the single
    // most common way these fixes silently regress, so CI compares this against
    // git HEAD. Queried through the environment, not argv: the helper must keep
    // the official CLI surface (unknown flags abort).
    if std::env::var_os("DSH_CU_PRINT_BUILD").is_some() {
        println!("{BUILD_REVISION}");
        std::process::exit(0);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut parent_pid: u32 = 0;
    // FIX-4: the official helper has no clap, no --help and no subcommands. Exactly
    // three flags are parsed by hand (FUN_140081196), comparison is exact-length so
    // "--flag=value" is rejected, and a bad argv aborts before any serve loop:
    //   --parent-pid <u32>
    //   --system-cursor-manager <pid> <suppress> <restore> <shutdown> <ready> <ack>
    //   --previous-notify <payload>
    // DSH-specific knobs (ttl, allow-list) moved to the sidecar config so this
    // binary keeps the official CLI surface.
    let mut cursor_manager = false;
    let mut prev_notify: Option<String> = None;
    let mut missing = |what: &str| -> ! {
        eprintln!("{what}");
        std::process::exit(2);
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--parent-pid" => match args.get(i + 1) {
                Some(v) => {
                    parent_pid = v.parse().unwrap_or_else(|_| missing("parse parent pid"));
                    i += 1;
                }
                None => missing("missing parent pid"),
            },
            "--system-cursor-manager" => cursor_manager = true,
            "--previous-notify" => match args.get(i + 1) {
                Some(v) => {
                    prev_notify = Some(v.clone());
                    i += 1;
                }
                None => missing("missing value for --previous-notify"),
            },
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    // DSH-only knob: the synthetic cursor sprite scale. The official helper has no such
    // flag, so it travels through the environment (like the allow list) and the argv the
    // binary accepts stays exactly the official surface. The overlay follows the display
    // DPI only; it never reads or writes the Windows cursor-size setting.
    if let Ok(raw) = std::env::var("DSH_COMPUTER_USE_CURSOR_SCALE") {
        match raw.trim().parse::<f32>() {
            Ok(scale) => overlay::set_cursor_scale(scale),
            Err(_) => eprintln!("ignoring unparsable DSH_COMPUTER_USE_CURSOR_SCALE"),
        }
    }
    if cursor_manager {
        std::process::exit(run_system_cursor_manager(parent_pid));
    }
    // P1 migration: one-shot image crop, replacing scripts/crop-image.ps1 (System.Drawing)
    // with the Rust `image` crate — no powershell.exe process. Triggered via the ENVIRONMENT
    // (like DSH_CU_PRINT_BUILD), so the argv surface stays the exact official one.
    //   DSH_CU_CROP_IN / DSH_CU_CROP_OUT    input / output png paths (required)
    //   DSH_CU_CROP_X / _Y / _W / _H        region (W/H <= 0 => to the edge; defaults 0)
    if let Ok(in_path) = std::env::var("DSH_CU_CROP_IN") {
        let out_path = std::env::var("DSH_CU_CROP_OUT").unwrap_or_default();
        let env_i = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
        let (x, y, w, h) = (
            env_i("DSH_CU_CROP_X"),
            env_i("DSH_CU_CROP_Y"),
            env_i("DSH_CU_CROP_W"),
            env_i("DSH_CU_CROP_H"),
        );
        std::process::exit(run_crop_image_oneshot(&in_path, &out_path, x, y, w, h));
    }
    // P1 migration: one-shot GDI capture, replacing scripts/gdi-capture.ps1. It reuses the
    // helper's OWN capture_hwnd (WGC -> GDI ladder, nudge, overlay masking), so the Rust
    // path is a superset of the ps1's GDI-only PrintWindow. Env-triggered like the crop mode.
    //   DSH_CU_GDI_CAPTURE_HWND=<hwnd>   DSH_CU_GDI_CAPTURE_OUT=<png path>
    if let Ok(hwnd_raw) = std::env::var("DSH_CU_GDI_CAPTURE_HWND") {
        let out_path = std::env::var("DSH_CU_GDI_CAPTURE_OUT").unwrap_or_default();
        let hwnd: isize = hwnd_raw.trim().parse().unwrap_or(0);
        std::process::exit(run_gdi_capture_oneshot(hwnd, &out_path));
    }
    if let Some(payload) = prev_notify.as_deref() {
        // Official: the notify hook calls back with the previous payload so the
        // helper can restore the user's original notify config on turn end.
        // NTF-2: the official path only accepts the `turn-ended` payload and exits
        // non-zero with `missing turn-ended payload` for anything else.
        if let Err(message) = notify::validate_previous_notify_payload(payload) {
            eprintln!("{message}");
            std::process::exit(2);
        }
        notify::restore_previous_notify();
        return;
    }
    if parent_pid != 0 {
        thread::spawn(move || watch_parent(parent_pid));
    }

    let mut helper = HelperState::new("window2".into());
    // D3/AX-14: official has no time-based observation expiry. Freshness is identity
    // + bounds + human-input based (see state::require_window_use / interrupt); the
    // TTL rejection is deleted in state::require_fresh and ttl_ms now defaults to 0.
    // DSH extension (not part of the official CLI): the sidecar narrows the target
    // set through the environment so the binary keeps the official argument surface.
    helper.allowed_apps = std::env::var("DSH_COMPUTER_USE_ALLOWED_APPS")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let state = Arc::new(Mutex::new(helper));
    let state_pipe = state.clone();
    let _pipe_name = pipe::spawn(move |req| handle(&state_pipe, req));
    // Official has no idle self-termination: the helper lives until its parent
    // goes away, the turn ends via the notify hook, or `close` arrives. An
    // opt-in watchdog remains for leaks, disabled by default.
    if let Ok(ms) = std::env::var("DSH_COMPUTER_USE_IDLE_EXIT_MS") {
        if let Ok(ms) = ms.trim().parse::<u64>() {
            if ms > 0 {
                thread::spawn(move || idle_exit_watch(ms));
            }
        }
    }
    // Backstop, not part of the official contract: the official helper is per-turn and
    // dies with the turn, while this one is reused across turns so a restart is cheap.
    // A turn that ends without the client's `end_turn`/`cancel` -- a dropped event, a
    // killed client -- would otherwise leave the synthetic cursor on the desktop
    // indefinitely. Hide it after a long quiet period; the next request shows it again.
    // `DSH_CU_OVERLAY_IDLE_HIDE_MS=0` disables the backstop.
    let overlay_idle_ms = std::env::var("DSH_CU_OVERLAY_IDLE_HIDE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(120_000);
    if overlay_idle_ms > 0 {
        thread::spawn(move || overlay_idle_hide_watch(overlay_idle_ms));
    }
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // FIX-3: an `end_turn` latched the previous turn. Clear per-turn state and
        // re-arm before dispatching anything from the next turn, so one helper
        // process can serve a whole session.
        if interrupt::take_restart() {
            interrupt::reset_turn();
            RUNNING.store(true, Ordering::SeqCst);
            if let Ok(mut st) = state.lock() {
                st.reset_turn_state();
            }
        }
        let reply = match protocol::decode_request(&line) {
            Ok(req) => handle(&state, req),
            Err(err) => json!({"id": null, "ok": false, "error": format!("invalid json: {err}")}),
        };
        if writeln!(stdout, "{reply}").is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}

/// P1 migration: one-shot crop for `scripts/crop-image.ps1` parity. Decodes InPath (jpg/png),
/// crops [x,y,w,h] (w/h <= 0 => to the edge, clamped into bounds), writes OutPath as PNG.
/// Prints {"ok":true,"out":...} / {"ok":false,"error":...} and returns the exit code
/// (0 write / 1 fail), matching the ps1 contract's exit semantics exactly.
fn run_crop_image_oneshot(in_path: &str, out_path: &str, x: i64, y: i64, w: i64, h: i64) -> i32 {
    if out_path.is_empty() {
        eprintln!("crop-image: empty out path");
        println!("{{\"ok\":false,\"error\":\"empty out path\"}}");
        return 1;
    }
    let result = (|| -> Result<(), String> {
        let img = image::open(in_path).map_err(|e| format!("open {in_path}: {e}"))?;
        let (iw, ih) = (img.width() as i64, img.height() as i64);
        let x0 = x.max(0).min((iw - 1).max(0));
        let y0 = y.max(0).min((ih - 1).max(0));
        let cw = if w <= 0 { iw - x0 } else { w.min(iw - x0) };
        let ch = if h <= 0 { ih - y0 } else { h.min(ih - y0) };
        if cw <= 0 || ch <= 0 {
            return Err("empty crop region".to_string());
        }
        let sub = img.crop_imm(x0 as u32, y0 as u32, cw as u32, ch as u32);
        sub.save(out_path).map_err(|e| format!("save {out_path}: {e}"))?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            let esc = out_path.replace('\\', "\\\\").replace('"', "\\\"");
            println!("{{\"ok\":true,\"out\":\"{esc}\"}}");
            0
        }
        Err(msg) => {
            eprintln!("crop-image: {msg}");
            let esc = msg.replace('\\', "\\\\").replace('"', "\\\"");
            println!("{{\"ok\":false,\"error\":\"{esc}\"}}");
            1
        }
    }
}

/// P1 migration: one-shot GDI/WGC capture for `scripts/gdi-capture.ps1` parity. Reuses the
/// helper's `capture::capture_hwnd` (WGC -> GDI ladder, nudge, overlay masking — a superset of
/// the ps1's GDI-only PrintWindow), converts the returned JPEG frame to PNG, writes OutPath,
/// and prints one JSON verdict with the same geometry fields gdi-capture.ps1 emitted.
fn run_gdi_capture_oneshot(hwnd: isize, out_path: &str) -> i32 {
    if out_path.is_empty() {
        eprintln!("gdi-capture: empty out path");
        println!("{{\"ok\":false,\"error\":\"empty out path\"}}");
        return 1;
    }
    let frame = match capture::capture_hwnd(hwnd) {
        Ok(f) => f,
        Err(e) => {
            let msg = e.to_string();
            eprintln!("gdi-capture: {msg}");
            let esc = msg.replace('\\', "\\\\").replace('"', "\\\"");
            println!("{{\"ok\":false,\"error\":\"{esc}\"}}");
            return 1;
        }
    };
    // CaptureFrame carries JPEG bytes; decode then re-encode as PNG so the caller's
    // image/png assumption (frameObservation) holds unchanged.
    let img = match image::load_from_memory(&frame.jpeg_bytes) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("gdi-capture: decode frame: {e}");
            println!("{{\"ok\":false,\"error\":\"decode frame failed\"}}");
            return 1;
        }
    };
    if let Err(e) = img.save(out_path) {
        eprintln!("gdi-capture: save {out_path}: {e}");
        let esc = format!("save {out_path}: {e}").replace('\\', "\\\\").replace('"', "\\\"");
        println!("{{\"ok\":false,\"error\":\"{esc}\"}}");
        return 1;
    }
    let bytes = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    let (w, h) = (frame.logical_w, frame.logical_h);
    // Screen metrics for the oversize check (matches gdi-capture.ps1's GetSystemMetrics(0/1)).
    let (sw, sh) = unsafe {
        (
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
            ),
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
            ),
        )
    };
    let oversize = sw > 0 && sh > 0 && (frame.phys_w > sw || frame.phys_h > sh);
    println!(
        "{{\"ok\":true,\"nudged\":true,\"width\":{w},\"height\":{h},\"bytes\":{bytes},\
\"window\":{{\"left\":{ol},\"top\":{ot},\"width\":{w},\"height\":{h}}},\
\"screen\":{{\"width\":{sw},\"height\":{sh}}},\"oversize\":{os}}}",
        ol = frame.origin_x,
        ot = frame.origin_y,
        os = if oversize { "true" } else { "false" },
    );
    0
}

fn watch_parent(pid: u32) {
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(handle, INFINITE);
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
}

fn ingest_meta(state: &Mutex<HelperState>, meta: &Map<String, Value>) {
    let mut scope: Option<(String, String)> = None;
    if let Ok(mut st) = state.lock() {
        if let Some(app) = json_str(meta, APPROVED_APP_META_KEY) {
            st.approved.insert(app.to_lowercase());
        }
        if let Some(conv) = json_str(meta, "conversationId")
            .or_else(|| json_str(meta, "conversation_id"))
            .or_else(|| json_str(meta, "sessionId"))
            .or_else(|| json_str(meta, "session_id"))
        {
            st.session_id = conv;
        }
        if let Some(turn) = json_str(meta, "turnId")
            .or_else(|| json_str(meta, "turn_id"))
            .or_else(|| json_str(meta, "callId"))
        {
            st.turn_id = turn;
        }
        st.ingest_approval_grants(&Value::Object(meta.clone()));
        scope = Some((st.session_id.clone(), st.turn_id.clone()));
    }
    // The Esc hook thread writes <home>/cache/computer-use/interrupts/<session>/<turn>,
    // so it needs the current scope without locking the whole helper state.
    if let Some((session, turn)) = scope {
        interrupt::set_session_scope(&session, &turn);
    }
}

fn request_budget_ms(req: &Request, params: &Map<String, Value>) -> i64 {
    let mut budget = req.budget_ms();
    if let Some(Value::Object(meta)) = params.get("meta") {
        if let Some(n) = protocol::meta_i64(&Value::Object(meta.clone()), BUDGET_HEADER) {
            budget = n;
        }
    }
    budget
}

fn budget_check(started: Instant, budget_ms: i64) -> Result<(), Error> {
    if budget_ms <= 0 {
        return Ok(());
    }
    if started.elapsed().as_millis() as i64 >= budget_ms {
        return Err(Error::desktop(BUDGET_EXHAUSTED));
    }
    Ok(())
}

fn touch_rpc() {
    if let Ok(mut slot) = LAST_RPC.lock() {
        *slot = Some(Instant::now());
    }
}

/// Hide a synthetic cursor that outlived its turn. The client is expected to send
/// `end_turn`/`cancel` at `turn/end`; this only bounds the damage when that signal is
/// lost, so the operator is never left with a fake pointer on the desktop.
fn overlay_idle_hide_watch(idle_ms: u64) {
    loop {
        thread::sleep(Duration::from_secs(2));
        if !overlay::visible() || IN_FLIGHT.load(Ordering::SeqCst) > 0 {
            continue;
        }
        let last = LAST_RPC.lock().ok().and_then(|g| *g);
        let Some(last) = last else { continue };
        if last.elapsed() >= Duration::from_millis(idle_ms) {
            overlay::hide();
        }
    }
}

fn idle_exit_watch(idle_ms: u64) {
    let idle = Duration::from_millis(idle_ms);
    loop {
        thread::sleep(Duration::from_secs(2));
        if overlay::visible() || IN_FLIGHT.load(Ordering::SeqCst) > 0 {
            continue;
        }
        let last = LAST_RPC.lock().ok().and_then(|g| *g);
        let Some(last) = last else { continue };
        if last.elapsed() >= idle {
            overlay::hide();
            overlay::stop_system_cursor_manager();
            RUNNING.store(false, Ordering::SeqCst);
            std::process::exit(0);
        }
    }
}

struct InflightGuard;
impl Drop for InflightGuard {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Requests slow enough that the transport's own budget is at risk.
///
/// The transport kills the helper when a request exceeds its budget, so the process's own
/// diagnostics are gone by the time anyone can ask what was slow: the model sees only
/// "computer-use request timed out". This appends one JSON line per slow request to
/// `%LOCALAPPDATA%\computer-use-app-catalog\slow-requests.log` (bounded, local only,
/// never sent anywhere) with the method, the duration and the catalog timing, so a
/// sporadic timeout is diagnosable after the fact instead of unreproducible.
///
/// `DSH_CU_SLOW_REQUEST_MS` overrides the threshold (default 1000 ms; 0 logs every
/// request, which is the supported way to record a baseline).
fn note_slow_request(label: &str, elapsed_ms: u64) {
    let threshold = std::env::var("DSH_CU_SLOW_REQUEST_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(1000);
    if elapsed_ms < threshold {
        return;
    }
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let dir = std::path::PathBuf::from(local).join("computer-use-app-catalog");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("slow-requests.log");
    // Bounded: drop the oldest lines once the file passes 128 KB.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 128 * 1024 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let rows: Vec<&str> = text.lines().rev().take(200).collect();
                let kept: Vec<&str> = rows.into_iter().rev().collect();
                let _ = std::fs::write(&path, kept.join("\n") + "\n");
            }
        }
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = json!({
        "at": at,
        "label": label,
        "elapsedMs": elapsed_ms,
        "processId": std::process::id(),
        "appCatalog": app_catalog::catalog_diagnostics(),
    });
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

fn handle(state: &Mutex<HelperState>, req: Request) -> Value {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    let _inflight = InflightGuard;
    touch_rpc();
    let id = req.id.clone();
    let official = req.is_official();
    let jsonrpc = req.is_jsonrpc_v2();
    let method = req.method_name().to_string();
    let params = req.params_object();
    let started = Instant::now();
    let budget_ms = request_budget_ms(&req, &params);
    ingest_meta(state, &req.meta_object());
    if method == "call" {
        if let Some(Value::Object(meta)) = params.get("meta") {
            ingest_meta(state, meta);
        }
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = match params.get("arguments") {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
        let label = format!("call {name}");
        let value = match gate_and_dispatch(state, name, &arguments, started, budget_ms) {
            Ok(value) => {
                let (value, images) = images::detach_images(value);
                let value = if tools::is_void(name) { Value::Null } else { value };
                let wrapped = protocol::call_result(name, value, images);
                if jsonrpc {
                    protocol::jsonrpc_ok(id, wrapped)
                } else {
                    official_ok(id, wrapped)
                }
            }
            Err(err) => err.to_response(id, jsonrpc),
        };
        note_slow_request(&label, started.elapsed().as_millis() as u64);
        return value;
    }
    let result = if method == "close" {
        dispatch(state, "shutdown", &params)
    } else {
        gate_and_dispatch(state, &method, &params, started, budget_ms)
    };
    let response = match result {
        Ok(value) if official => official_ok(id, value),
        Err(err) => err.to_response(id, jsonrpc),
        Ok(value) if jsonrpc => protocol::jsonrpc_ok(id, value),
        Ok(value) => official_ok(id, value),
    };
    note_slow_request(&method, started.elapsed().as_millis() as u64);
    response
}

fn target_app(method: &str, params: &Map<String, Value>) -> String {
    if method == "start_audio_recording" {
        return AUDIO_APP.into();
    }
    if method == "launch_app" {
        return json_str(params, "app").unwrap_or_default();
    }
    if let Some(Value::Object(window)) = params.get("window") {
        if let Some(app) = json_str(window, "app") {
            return app;
        }
    }
    json_str(params, "app").unwrap_or_default()
}

fn gate_and_dispatch(
    state: &Mutex<HelperState>,
    method: &str,
    params: &Map<String, Value>,
    started: Instant,
    budget_ms: i64,
) -> Result<Value, Error> {
    budget_check(started, budget_ms)?;
    // AX-WEDGE: publish this request's remaining budget so the accessibility client
    // bounds every blocking call by it and answers BEFORE the transport kills the
    // helper. The transport budget stays the outer backstop, not the primary bound.
    uia::begin_request_budget(if budget_ms > 0 {
        budget_ms - started.elapsed().as_millis() as i64
    } else {
        0
    });
    // The turn-lifecycle method must stay reachable while the turn is stopped: `end_turn` is
    // the only path that clears the STOPPED latch and deletes the interrupt marker, so routing
    // it through those very checks made the cleanup unreachable exactly when it was needed
    // (measured: `end_turn` answered with the physical-Escape message while a marker existed).
    let lifecycle = matches!(method, "end_turn");
    if !lifecycle {
        interrupt::check()?;
    }
    if !desktop::skip_lock_check(method) {
        desktop::require_unlocked()?;
    }
    if !policy::skip_managed_policy_check(method) {
        policy::require_computer_use_enabled().map_err(Error::desktop)?;
    }
    if method == "start_audio_recording" || method == "stop_audio_recording" {
        audio::require_enabled()?;
    }
    let marker = {
        let st = state.lock().map_err(|_| Error::other("state lock"))?;
        notify::interrupt_flag_path(&st.session_id, &st.turn_id)
    };
    // A live marker refuses the call; a marker left over from an earlier turn is removed and
    // ignored (`interrupt::marker_blocks`). Existence alone used to mean "refused forever".
    if !lifecycle && interrupt::marker_blocks(&marker) {
        return Err(Error::interrupt());
    }
    // APS-04: computer-audio has its own approval card, subject and message. Handle
    // it explicitly so the official message / persist semantics never depend on
    // `target_app`'s window fallback. `Error::approval(AUDIO_APP)` yields the
    // official question and `allowPersistentApproval:false` (APS-12).
    if method == "start_audio_recording" {
        let approved = {
            let st = state.lock().map_err(|_| Error::other("state lock"))?;
            st.approved.contains(&AUDIO_APP.to_lowercase())
        };
        if !approved {
            return Err(Error::approval(AUDIO_APP));
        }
    } else if method != "launch_app" && !tools::skip_approval(method) {
        // launch_app resolves catalog/exe identity first, then uses
        // `launch_app has no approved target` instead of the generic approval prompt.
        let app = target_app(method, params);
        if !app.is_empty() {
            let approved = {
                let st = state.lock().map_err(|_| Error::other("state lock"))?;
                st.approved.contains(&app.to_lowercase())
            };
            if !approved {
                return Err(Error::approval(&app));
            }
        }
    }
    let value = dispatch(state, method, params)?;
    budget_check(started, budget_ms)?;
    Ok(value)
}

fn dispatch(state: &Mutex<HelperState>, method: &str, params: &Map<String, Value>) -> Result<Value, Error> {
    let method = match method {
        "observe" => "get_window_state",
        "type" => "type_text",
        "keypress" => "press_key",
        "window" => "get_window",
        other => other,
    };
    match method {
        "health" => {
            let st = state.lock().map_err(|_| Error::other("state lock"))?;
            Ok(json!({
                "ok": true,
                "overlay": "dsh",
                "helper": "dsh-computer-use",
                "revision": BUILD_REVISION,
                "ime": {"restoreFailures": dsh_computer_use::ime::restore_failure_count()},
                "native": true,
                "surface": st.surface,
                "backend": st.backend,
                "ttlMs": st.ttl_ms,
                "closed": st.closed,
                "allowedApps": st.allowed_apps,
                "observation": st.lease_snapshot(),
                "pipe": pipe::current_name(),
                "computerUseEnabled": policy::computer_use_enabled(),
                "integrity": {
                    "helperRid": uia::integrity_rid(std::process::id()),
                    "helperLabel": uia::integrity_label(uia::integrity_rid(std::process::id())),
                },
                "capture": {"wgcFramePool": capture::wgc_available(), "jpeg": true, "cachedSessionCount": capture::cached_session_count(), "coverage": capture::capture_diagnostics()},
                "dpi": {"aware": true, "scale": dpi::dpi_scale(dpi::window_dpi(hwnd_from_id(0)))},
            }))
        }
        "tools" => {
            let surface = json_str(params, "surface").unwrap_or_else(|| {
                state
                    .lock()
                    .ok()
                    .map(|st| st.surface.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "computer".into())
            });
            Ok(tools::tools_for_surface(&surface))
        }
        // Diagnostic probe: report exactly what the occlusion check sees for a
        // window, so a "captured the wrong window" report can be rooted instead of
        // reasoned about from the outside.
        "probe_coverage" => {
            let hwnd = json_i64(params, "hwnd")?
                .map(|v| v as isize)
                .or_else(|| state.lock().ok().and_then(|st| st.last_window.as_ref().map(|w| w.id as isize)))
                .ok_or_else(|| Error::type_err("hwnd is required"))?;
            Ok(capture::coverage_probe(hwnd))
        }
        "prompt" => Ok(json!({"prompt": prompt::native_prompt()})),
        // BR-14: the official host hook `assertBrowserUrlAllowed`. Pure decision, no
        // input injection; the Python plane keeps its own mirrored implementation.
        "browser_url_gate" => Ok(policy::url_policy::gate_response(params)),
        "interrupt" => {
            interrupt::trip();
            overlay::hide();
            Ok(json!({"stopped": true}))
        }
        "cancel" => {
            interrupt::cancel_work();
            Ok(json!({"cancelled": true}))
        }
        "end_turn" => {
            let (session_id, turn_id) = {
                let st = state.lock().map_err(|_| Error::other("state lock"))?;
                (
                    json_str(params, "session_id").unwrap_or_else(|| st.session_id.clone()),
                    json_str(params, "turn_id").unwrap_or_else(|| st.turn_id.clone()),
                )
            };
            interrupt::set_session_scope(&session_id, &turn_id);
            interrupt::end_turn();
            interrupt::signal_turn_ended(&session_id, &turn_id);
            let _ = notify::write_notify_config(&session_id, &turn_id);
            if let Ok(mut st) = state.lock() {
                st.reset_turn_state();
            }
            overlay::hide();
            Ok(json!({
                "ended": true,
                "session_id": session_id,
                "turn_id": turn_id,
                "notify": true,
            }))
        }
        "shutdown" => {
            overlay::hide();
            let _ = overlay::shutdown_overlay();
            audio::abort();
            capture::invalidate_cached_sessions("exit");
            RUNNING.store(false, Ordering::SeqCst);
            if let Ok(mut st) = state.lock() {
                st.closed = true;
            }
            Ok(json!({"closed": true}))
        }
        "list_windows" => Ok(Value::Array(
            enum_windows()?.into_iter().map(|w| w.to_json()).collect(),
        )),
        "list_apps" => {
            let windows = enum_windows()?;
            Ok(Value::Array(
                app_catalog::list_apps(&windows)
                    .into_iter()
                    .map(|a| a.to_json())
                    .collect(),
            ))
        }
        "get_window" | "window" => {
            let window = resolve_window(state, params)?;
            require_usable_app_window(hwnd_from_id(window.id))?;
            Ok(window.to_json())
        }
        "activate_window" => {
            let window = resolve_window(state, params)?;
            activate_hwnd(hwnd_from_id(window.id))?;
            state.lock().map_err(|_| Error::other("state lock"))?.remember_window(window);
            Ok(json!({}))
        }
        "launch_app" => {
            let app = require_text(params, "app")?;
            let info = app_catalog::resolve_launch_info(&app)?;
            let (allowed, approved) = {
                let st = state.lock().map_err(|_| Error::other("state lock"))?;
                (st.allowed_apps.clone(), st.approved.clone())
            };
            policy::deny_allowed_apps(&app, &allowed).map_err(Error::desktop)?;
            policy::deny_app(&app).map_err(Error::desktop)?;
            policy::deny_app(&info.launch_file).map_err(Error::desktop)?;
            if !info.identity.is_empty() {
                policy::deny_app(&info.identity).map_err(Error::desktop)?;
            }
            if let Some(name) = info.product_name.as_deref() {
                policy::deny_product_identity(name, info.original_filename.as_deref())
                    .map_err(Error::desktop)?;
            }
            if !app_catalog::launch_target_approved(&approved, &app, &info) {
                // APS-11: derive the risk level from the PE identity, so a renamed
                // antivirus binary is still reported as high risk.
                return Err(Error::launch_unapproved_identity(
                    &app,
                    &info.display_name(),
                    info.product_name.as_deref(),
                    info.original_filename.as_deref(),
                ));
            }
            app_catalog::launch_app(&app)?;
            Ok(json!({}))
        }
        "batch_actions" => batch_actions(state, params),
        "session_note" => {
            let mut st = state.lock().map_err(|_| Error::other("state lock"))?;
            if let Some(text) = json_str(params, "text") {
                let cleaned = text.trim().to_string();
                if !cleaned.is_empty() {
                    st.reasoning.push(cleaned.clone());
                    let n = st.reasoning.len();
                    st.notes.insert(format!("note-{n}"), cleaned);
                }
            } else {
                let key = json_str(params, "key").unwrap_or_else(|| "note".into());
                let value = json_str(params, "value").unwrap_or_default();
                st.notes.insert(key, value);
            }
            Ok(json!({"ok": true, "reasoning": st.reasoning, "notes": st.notes}))
        }
        "session_state" => {
            let st = state.lock().map_err(|_| Error::other("state lock"))?;
            Ok(json!({
                "notes": st.notes,
                "reasoning": st.reasoning,
                "window": st.last_window.as_ref().map(|w| w.to_json()),
                "screenshot_ids": st.shots.keys().cloned().collect::<Vec<_>>(),
            }))
        }
        "get_window_state" => get_window_state(state, params),
        "fill_form" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            fill_form(state, params)
        }
        // DSH extension: satisfy the "call get_window_state before continuing" contract
        // without paying for a capture. The plugin uses it to un-wedge the human-input
        // monitor on windows Windows Graphics Capture can never frame.
        "clear_user_input" => {
            crate::interrupt::clear();
            Ok(json!({ "cleared": true }))
        }
        "start_audio_recording" => {
            let duration = StartAudioRecordingParams::from_map(params)?.duration_ms()?;
            audio::start(duration)?;
            Ok(json!({}))
        }
        "stop_audio_recording" => audio::stop(),
        "set_value" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            set_value(state, params)
        }
        "perform_secondary_action" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            secondary(state, params)
        }
        "select_text" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            select_text(state, params)
        }
        "click" | "click_element" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            click(state, params, method)
        }
        "scroll" | "scroll_element" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            scroll(state, params, method)
        }
        "type_text" => {
            let window = resolve_window(state, params)?;
            prepare_input(state, &window, WindowUse::Captured)?;
            overlay::show();
            // Official `type_text` accepts an empty string: the schema requires the
            // key to be present, not to be non-empty (TC-11).
            let text = require_text_allow_empty(params, "text")?;
            if text.is_empty() {
                return Ok(json!({}));
            }
            // F2: `type_text` is a clipboard paste (Ctrl+V). Verify target-scoped, on
            // the focused control only -- never a window-wide search, which a chat
            // app that echoes the tool call into its transcript would trivially
            // "confirm" (the 2026-10-22 false positive), and which costs a full tree
            // walk on a browser window. Every check goes through
            // `text_landed_settling` (one immediate pass, one after 300ms) and
            // returns a four-state verdict.
            //
            // Only a *proven* absence fails. A surface that publishes no text at all
            // (Chromium contenteditable, canvas editors) can never witness the paste,
            // so its silence counts as success when the focused control inside the
            // target accepts text. Retrying on that case used to paste the text a
            // second time and then report a failure the model had to work around.
            let hwnd = hwnd_from_id(window.id);
            let wid = window.id as isize;
            let before_fv = uia::global().focused_value_opt();
            let _ = activate_hwnd(hwnd);
            interrupt::mark_synthetic(0.3);
            input::paste_text_for(Some(wid), &text)?;
            let mut last = text_landed_settling(wid, &text, &before_fv);
            if last.failed(wid) {
                let _ = activate_hwnd(hwnd);
                // AX-WEDGE anti-doubling: the retry must REPLACE, not append. A
                // stale/uncommitted read-back (a slow Chromium field that still shows
                // the pre-paste value, 2026-10-25 QQNT search box) reads as "did not
                // land", and a second plain Ctrl+V pasted the text a second time at
                // the cursor. When the focus is a text target inside this window,
                // select-all first so the retry overwrites whatever did or did not
                // land; with no editable focus, Ctrl+A could select an arbitrary
                // surface and the paste would overwrite it, so the retry stays a
                // plain insert there.
                if focused_accepts_text(wid) {
                    let _ = input::press_key_for(Some(wid), "Control_L+a");
                    thread::sleep(Duration::from_millis(50));
                }
                interrupt::mark_synthetic(0.3);
                input::paste_text_for(Some(wid), &text)?;
                last = text_landed_settling(wid, &text, &before_fv);
                if last.failed(wid) {
                    // Last resort: post WM_PASTE straight to the focused control of
                    // the target's GUI thread. This bypasses SendInput and helps the
                    // classic Win32 EDIT case; it does not apply to WinUI/XAML and
                    // cannot cross a UIPI integrity boundary. Same select-all guard
                    // as above: the retry must replace, never append.
                    if focused_accepts_text(wid) {
                        let _ = input::press_key_for(Some(wid), "Control_L+a");
                        thread::sleep(Duration::from_millis(50));
                    }
                    let _ = input::paste_text_via_postmessage(wid, &text);
                    last = text_landed_settling(wid, &text, &before_fv);
                    if last.failed(wid) {
                        // A mangled paste gets its own message: something did land,
                        // and saying "click into the field" would send the model
                        // chasing the wrong thing (the classic IME `v`).
                        if last == Landing::Wrong {
                            let got = uia::global()
                                .focused_value_opt()
                                .or_else(|| uia::global().focused_text())
                                .unwrap_or_default();
                            let got = clip(&got, 40);
                            return Err(Error::desktop(format!(
                                "type_text landed the wrong text: the field reads {got:?}; \
                                 an IME or a controlled input likely swallowed the paste \
                                 (use set_value, or clear the field and type again)"
                            )));
                        }
                        return Err(Error::desktop(
                            "type_text did not land in the target window; click into the target field first, then retry",
                        ));
                    }
                }
            }
            Ok(json!({}))
        }
        "press_key" => {
            let window = resolve_window(state, params)?;
            let key = require_text(params, "key")?;
            policy::deny_press_key(&key).map_err(Error::desktop)?;
            prepare_input(state, &window, WindowUse::Captured)?;
            overlay::show();
            interrupt::mark_synthetic(0.3);
            // Target the window that will receive the chord: the IME guard needs
            // the owning thread's layout, not the helper's.
            input::press_key_for(Some(window.id as isize), &key)?;
            Ok(json!({}))
        }
        "drag" => {
            state.lock().map_err(|_| Error::other("state lock"))?.require_fresh()?;
            drag(state, params)
        }
        "diagnostic_state" => diagnostic_state(state),
        other => Err(Error::unknown_method(other)),
    }
}

fn prepare_input(state: &Mutex<HelperState>, window: &WindowRef, use_: WindowUse) -> Result<(), Error> {
    let hwnd = hwnd_from_id(window.id);
    {
        let st = state.lock().map_err(|_| Error::other("state lock"))?;
        st.require_fresh()?;
        st.require_window_use(window.id, use_)?;
        policy::deny_target(&window.app, hwnd, &st.allowed_apps).map_err(Error::desktop)?;
    }
    if !is_already_foreground(hwnd) {
        activate_hwnd(hwnd)?;
    }
    Ok(())
}

/// UIA-pattern actions (perform_secondary_action / native select_text) don't need
/// foreground focus. Activating the target here would emit a focus event and
/// invalidate the very element cache the action is about to use, producing
/// "no cached app state is available for <app>". Keep the freshness/policy
/// checks but skip activation.
fn prepare_uia_target(state: &Mutex<HelperState>, window: &WindowRef) -> Result<(), Error> {
    let hwnd = hwnd_from_id(window.id);
    let st = state.lock().map_err(|_| Error::other("state lock"))?;
    st.require_fresh()?;
    st.require_window_use(window.id, WindowUse::Captured)?;
    policy::deny_target(&window.app, hwnd, &st.allowed_apps).map_err(Error::desktop)?;
    Ok(())
}

/// A value cut to `max` characters for a tool error, with an ellipsis, so a
/// page's whole text or a huge field value never lands in an error string.
fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    format!("{}…", t.chars().take(max).collect::<String>())
}

/// Whether a write's read-back proves the write did NOT take: the control was
/// readable (`seen`), the value we asked for is non-empty, and the control does
/// not carry it. `None` = cannot tell (unreadable control, or nothing to write),
/// which must never be reported as a failure.
///
/// Shared by `set_value` and `fill_form`'s `set` step so both agree with
/// `type_text`: writing something the field then does not show is a failure, not
/// a success (2026-10-22/23 false successes on React controlled inputs).
fn write_missed(want: &str, seen: Option<&str>) -> Option<String> {
    let seen = seen?;
    if want.is_empty() {
        return None;
    }
    let seen = seen.replace('\r', "");
    if seen.contains(want) {
        None
    } else {
        Some(seen)
    }
}

/// How a paste's landing was determined. Four states, not two.
///
/// The old boolean collapsed "we could not read the target's text" into "the text
/// did not land", which is only correct for controls that publish their content to
/// UIA. Text surfaces that never do (Chromium `contenteditable`, ProseMirror,
/// canvas editors, many game/terminal surfaces) therefore answered every
/// `type_text` with a false failure — observed live 2026-10-22 typing into the
/// DSH composer. Reporting a failure requires *proof*; an unreadable target is
/// not proof.
///
/// The reverse trap is just as bad: a control that publishes its text and *changed*
/// is not proof that our text landed. A clipboard paste eaten by an IME leaves a
/// stray character (`v` is the classic one), the value did change, and the old
/// check called that a success — observed live 2026-10-23 typing `E2E-FWD-…`
/// into a React input and reading back `Value: v`. So a change is only proof when
/// the new value carries the typed text; otherwise it is proof of the opposite
/// (`Wrong`), which must fail loudly rather than masquerade as `ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Landing {
    /// Positive evidence the text is there.
    Landed,
    /// Positive evidence it is NOT there: the target publishes text, and neither
    /// the window dump nor the focused value contains it.
    Absent,
    /// Positive evidence something *else* landed: the target publishes text and it
    /// changed, but does not carry what we typed (a mangled paste).
    Wrong,
    /// The target publishes no readable text, so neither conclusion is provable.
    Unverifiable,
}

/// The signals a landing check collects, in the order they are consulted.
///
/// Kept as a plain struct so the verdict is a pure function over synthetic
/// inputs — testable without a live window, and free of the brittle source-text
/// assertions that used to pin its shape.
#[derive(Debug, Clone, Default)]
struct LandingSignals {
    /// The focused control's node text (its `Value`, else its `Name`).
    node_text: Option<String>,
    /// The focused control's TextPattern range text. `None` for a whole-window
    /// container, whose range would span the entire page.
    range_text: Option<String>,
    /// The focused control's value before the paste.
    before_value: Option<String>,
    /// The focused control's value after the paste.
    after_value: Option<String>,
    /// The focused element covers (nearly) the whole window, so it is a container
    /// rather than a text target.
    container_focus: bool,
}

/// The landing verdict, decided from the *focused control only*.
///
/// Deliberately never searches the window. A window-wide text search cannot
/// witness a paste in any app that renders the agent's own tool call back into
/// its UI (a chat transcript, a log pane): the typed string ends up on screen
/// through the echo, so "the window contains the text" is trivially true even
/// when the paste went nowhere. That was the 2026-10-22 false positive. Scoping
/// the check to the focused control is both correct and far cheaper than a tree
/// walk.
///
/// `None` means the signals could not decide.
fn landing_from(signals: &LandingSignals, text: &str) -> Option<Landing> {
    // 1. The focused control's own node text.
    if let Some(current) = &signals.node_text {
        if current.contains(text) {
            return Some(Landing::Landed);
        }
    }
    // 2. The focused value changed to a non-empty value, *and carries the text we
    // typed*. A change alone is not proof: an IME that swallows the paste leaves a
    // stray character behind, the value did change, and calling that a success is
    // the 2026-10-23 false positive (typed `E2E-FWD-…`, read back `v`). Only
    // meaningful when we had a concrete readable value before; never treat
    // "unreadable before, readable after" as landed — that is just focus moving to
    // an empty control.
    match (&signals.before_value, &signals.after_value) {
        (Some(b), Some(a)) if a != b && !a.is_empty() => {
            return Some(if a.contains(text) {
                Landing::Landed
            } else {
                Landing::Wrong
            })
        }
        _ => {}
    }
    // 3. The focused control's text range. This is the signal that works for
    // surfaces whose content is *not* a node `Value`/`Name` — a Chromium
    // `contenteditable`, ProseMirror, a Word document. Those publish their text
    // only through TextPattern, so neither a node check nor a tree walk sees it.
    if let Some(current) = &signals.range_text {
        if current.contains(text) {
            return Some(Landing::Landed);
        }
    }
    // 3.5. The focused value is byte-identical to the pre-paste value. On a slow
    // Chromium field the paste commits asynchronously and the read-back still
    // shows the pre-paste state, so "unchanged" is what a STALE read looks like
    // -- it is not proof the paste failed (2026-10-25, QQNT search box: the
    // paste landed, the first read returned the pre-paste empty value, the old
    // code called it Absent, the retry appended a second copy, and the doubled
    // text is something the caller cannot undo). An unchanged read-back that no
    // positive signal witnessed is honestly "cannot tell"; the caller reports
    // success when the focus is editable and re-observes otherwise.
    if let (Some(before), Some(after)) = (&signals.before_value, &signals.after_value) {
        if before == after {
            return Some(Landing::Unverifiable);
        }
    }
    // 4. The focused control publishes no text whatsoever -> the surface keeps
    // its content private, so no UIA read can witness the paste.
    if signals.node_text.is_none() && signals.range_text.is_none() {
        return Some(Landing::Unverifiable);
    }
    // 5. The focus is a whole-window container. It is not a text target, so its
    // content says nothing about where the paste went: a browser page root
    // reports the whole page, which includes any echo. Cannot conclude.
    if signals.container_focus {
        return Some(Landing::Unverifiable);
    }
    // 6. A real control that publishes text, and not ours: proven absence.
    Some(Landing::Absent)
}

/// Collect the cheap (element-query only) signals for the focused control.
///
/// No window dump: every signal here is a property read on one element, which is
/// what keeps `type_text` fast on a browser/Electron window.
fn collect_focused_signals(wid: isize, before_fv: &Option<String>) -> LandingSignals {
    LandingSignals {
        node_text: uia::global().focused_text(),
        range_text: uia::global().focused_text_range_text(wid),
        before_value: before_fv.clone(),
        after_value: uia::global().focused_value_opt(),
        container_focus: uia::global()
            .focused_is_window_container(wid)
            .unwrap_or(false),
    }
}

/// The landing verdict for a paste into `wid`.
fn text_landed(wid: isize, text: &str, before_fv: &Option<String>) -> Landing {
    let signals = collect_focused_signals(wid, before_fv);
    landing_from(&signals, text).unwrap_or(Landing::Absent)
}

/// How long a *failed* landing check waits before re-reading the surface.
///
/// Web documents (Electron / WebView / CEF) commit the pasted text asynchronously:
/// the first read still shows the old DOM, so a correct paste reads as "did not
/// land". 300ms is long enough for the renderer to commit on this machine and
/// short enough to stay inside the tool budget.
const LANDING_SETTLE_MS: u64 = 300;

/// [`text_landed`], re-read once after [`LANDING_SETTLE_MS`] when the first pass
/// said `Absent`.
///
/// `Absent` means "readable control, text missing", and a slow commit is
/// exactly that case. An `Unverifiable` whose *before* value was readable gets
/// the same second look (audit P1-3, 2026-10-25): a stale asynchronous commit
/// reads exactly like a swallowed paste — the focused value is unchanged — and
/// one settle read can upgrade it to a proven `Landed`/`Wrong` instead of a
/// silent success. An `Unverifiable` from an unreadable target (`before` is
/// `None`) can never change no matter how long we wait, so it returns
/// immediately — which is what keeps a `type_text` into a web document fast.
fn text_landed_settling(wid: isize, text: &str, before_fv: &Option<String>) -> Landing {
    let first = text_landed(wid, text, before_fv);
    // A wrong landing is settled too, and not by waiting: what is there is not
    // what we typed. Re-reading would only paste it again, so return at once and
    // let the caller report it.
    let retry_unverifiable = first == Landing::Unverifiable && before_fv.is_some();
    if first != Landing::Absent && !retry_unverifiable {
        return first;
    }
    std::thread::sleep(Duration::from_millis(LANDING_SETTLE_MS));
    match text_landed(wid, text, before_fv) {
        Landing::Landed => Landing::Landed,
        // A proven wrong landing must surface, not hide behind Unverifiable's
        // "counts as success when the focus accepts text".
        proven @ Landing::Wrong if retry_unverifiable => proven,
        _ => first,
    }
}

impl Landing {
    /// Keep the more informative verdict when two passes disagree.
    fn max_by_confidence(self, other: Landing) -> Landing {
        match (self, other) {
            (Landing::Landed, _) | (_, Landing::Landed) => Landing::Landed,
            (Landing::Unverifiable, _) | (_, Landing::Unverifiable) => Landing::Unverifiable,
            // A mangled paste outranks a plain absence: "wrong text is there" is
            // strictly more informative than "nothing is there".
            (Landing::Wrong, _) | (_, Landing::Wrong) => Landing::Wrong,
            _ => Landing::Absent,
        }
    }

    /// Whether the paste should be reported as failed, given whether the focused
    /// control inside the target accepts text.
    ///
    /// Only a *proven* absence fails — or a proven wrong landing (`Wrong`), which
    /// fails whatever the focus looks like: something did land and it is not what
    /// was typed, so reporting success would be a lie. An unverifiable target
    /// whose focus accepts text counts as success: the paste ran, and the only
    /// surface that could receive it was editable.
    fn failed_with(self, focus_accepts_text: bool) -> bool {
        match self {
            Landing::Landed => false,
            Landing::Absent => true,
            Landing::Wrong => true,
            Landing::Unverifiable => !focus_accepts_text,
        }
    }

    /// [`failed_with`], resolving the "does the focus accept text" question.
    ///
    /// The window scoping matters: a paste is only meaningful if the focus it
    /// landed on belongs to the target, otherwise "editable somewhere on the
    /// desktop" would excuse a genuine miss.
    fn failed(self, wid: isize) -> bool {
        self.failed_with(focused_accepts_text(wid))
    }
}

/// Whether the foreground focus is a text-accepting control *inside* `wid`.
fn focused_accepts_text(wid: isize) -> bool {
    if uia::global().focused_window_id() != Some(wid) {
        return false;
    }
    uia::global().focused_is_editable().unwrap_or(false)
}

fn flag(params: &Map<String, Value>, key: &str, default: bool) -> bool {
    match params.get(key) {
        Some(Value::Bool(v)) => *v,
        Some(Value::Number(n)) => n.as_i64() != Some(0),
        None => default,
        _ => default,
    }
}

/// Lenient reader for the DSH-only diff opt-out spellings (`disableDiffing` / `disable_diffing`
/// / `disableDiff`).
///
/// CORRECTION (report 03 / TC-03 / AX-04): this is **not** an official contract. The official
/// window2 surface has no diffing at all -- `diff`/`iff`/`ferent` are absent from its string
/// table and a second observation returns the full tree (`parity/golden-ax/official-diff.txt`),
/// and `disableDiffing` appears in neither `docs/guidance.md` nor `docs/api.md`. The flag stays
/// because the DSH plugin opts into a line diff; it must not be described as official.
fn flag_any(params: &Map<String, Value>, keys: &[&str], default: bool) -> bool {
    for key in keys {
        if params.contains_key(*key) {
            return flag(params, key, default);
        }
    }
    default
}

fn get_window_state(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let include_shot = flag(params, "include_screenshot", true);
    let include_text = flag(params, "include_text", false);
    if !include_shot && !include_text {
        return Err(Error::type_err(STATE_FLAG_ERROR));
    }
    // Official diffing is on by default; disableDiffing forces a full tree, and a
    // previous screenshot-only observation also forces a full tree.
    let disable_diffing = flag_any(params, &["disableDiffing", "disable_diffing", "disableDiff"], false);
    let window = resolve_window(state, params)?;
    let hwnd = hwnd_from_id(window.id);
    require_observable_window(hwnd)?;
    // An observation request satisfies the "call get_window_state before continuing"
    // contract, so it must clear the human-input flag HERE — before the capture can
    // fail. It used to be cleared only in `remember_capture`, i.e. after a successful
    // frame, which meant a window Windows Graphics Capture can never frame (static
    // page, window larger than the desktop, covered window) kept the flag forever and
    // every later click/scroll answered "user input was detected in this window" with
    // no way out. Measured on the moxueyuan PDF chapter page: dirty stayed true through
    // a text-only and a screenshot observation, and scroll_to could not run at all.
    crate::interrupt::clear();
    overlay::exclude_overlay_from_capture();
    for h in overlay::hwnds() {
        capture::register_overlay_hwnd(h);
    }
    let timeout_ms = capture_timeout_ms(params)?;
    let extra_timeout_ms = timeout_ms.min(400);
    let pending_shot = {
        let st = state.lock().map_err(|_| Error::other("state lock"))?;
        format!("screenshot-{}", st.next_shot)
    };
    let frame = if include_shot {
        match capture::capture_hwnd_timeout(window.id as isize, timeout_ms) {
            Ok(frame) => frame,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("GetWindowRect")
                    || msg.contains("invalid bounds")
                    || msg.contains("window bounds unavailable")
                {
                    return Err(Error::desktop(window_bounds_unavailable_message(&window)));
                }
                if msg.contains("no screenshot targets found for") || msg.contains("no monitor") {
                    return Err(Error::desktop(screenshot_targets_missing(&pending_shot, &window)));
                }
                return Err(Error::desktop(msg));
            }
        }
    } else {
        let (ox, oy, w, h) = match dsh_computer_use::enum_windows::window_rect(hwnd) {
            Ok(rect) if rect.2 > 0 && rect.3 > 0 => rect,
            _ => return Err(Error::desktop(WINDOW_BOUNDS_UNAVAILABLE_COORD)),
        };
        let dpi = dpi::window_dpi(hwnd);
        let (lw, lh) = dpi::scaled_size(w, h, dpi);
        dsh_computer_use::capture::CaptureFrame {
            jpeg_bytes: Vec::new(),
            origin_x: ox,
            origin_y: oy,
            phys_w: w,
            phys_h: h,
            logical_w: lw,
            logical_h: lh,
            dpi,
        }
    };
    let origin = (frame.origin_x as f64, frame.origin_y as f64);
    let scale = dpi::dpi_scale(frame.dpi);
    let mut accessibility = Value::Null;
    let mut nodes = Vec::new();
    let mut full_tree = String::new();
    if include_text {
        // AX-WEDGE degraded observation: the screenshot is already captured, so a
        // dump that cannot answer (a wedged UIA provider, a spent budget) must not
        // fail the request -- that used to cost the whole helper process at the
        // transport budget, and with it the coordinate path this observation feeds.
        // The error travels in `accessibility` instead; the model keeps the screenshot
        // and coordinate actions while the element list is empty.
        match uia::global().dump(window.id as isize, &window.title, origin, scale) {
            Ok(dumped) => {
                full_tree = uia::format_accessibility(&dumped, &window.title, &window.app);
                nodes = dumped.nodes.clone();
                // TC-03/AX-04: the official `accessibility.tree` always carries the FULL
                // formatted tree, and `WindowState.d.ts` has no `diff` field. DSH used to
                // substitute an accessibility diff for the tree and emit a non-official `diff`
                // boolean, which made the field mean something other than the official schema.
                //
                // AX-05/AX-07: `uia::AccessibilityDump::to_json` applies the official
                // serialisation rules (absent optional fields are omitted, and the official
                // `meta` keys appear only when the tree was actually truncated).
                let _ = disable_diffing;
                accessibility = dumped.to_json(&full_tree);
            }
            Err(err) => {
                accessibility = serde_json::json!({
                    "tree": String::new(),
                    "error": err,
                });
            }
        }
    }
    let mut screenshots = Vec::new();
    let mut st = state.lock().map_err(|_| Error::other("state lock"))?;
    st.remember_observed(window.clone(), hwnd);
    st.last_nodes = nodes;
    // Remember the FULL tree (never the diff) so the next observation can diff
    // against it, and remember whether this observation carried text at all.
    if include_text {
        st.last_tree_text = Some(full_tree);
        st.last_observation_full_tree = true;
    } else {
        st.last_observation_full_tree = false;
    }
    let shot_id = st.remember_capture(
        frame.origin_x,
        frame.origin_y,
        frame.logical_w,
        frame.logical_h,
        frame.dpi,
    );
    if include_shot {
        let mut record = space_identity(hwnd, "window", &shot_id, &window.app);
        record["id"] = json!(shot_id);
        record["zIndex"] = json!(0);
        record["url"] = json!(frame.data_url());
        record["originX"] = json!(frame.origin_x);
        record["originY"] = json!(frame.origin_y);
        record["width"] = json!(frame.logical_w);
        record["height"] = json!(frame.logical_h);
        record["nativeWidth"] = json!(frame.phys_w);
        record["nativeHeight"] = json!(frame.phys_h);
        record["space"] = json!("window");
        record["displayName"] = json!(if window.title.is_empty() { window.app.clone() } else { window.title.clone() });
        record["snapshot"] = json!(shot_id);
        screenshots.push(record);
        let overlay = overlay::hwnds();
        let extras = extra_space_hwnds(hwnd, &overlay);
        let n = extras.len();
        for (i, (extra_id, kind)) in extras.into_iter().enumerate() {
            if let Ok(extra) = capture::capture_hwnd_timeout(extra_id, extra_timeout_ms) {
                let eid = st.remember_extra_capture(
                    extra.origin_x,
                    extra.origin_y,
                    extra.logical_w,
                    extra.logical_h,
                    extra.dpi,
                );
                let extra_hwnd = hwnd_from_id(extra_id as u64);
                let mut record = space_identity(extra_hwnd, &kind, &eid, &window.app);
                record["id"] = json!(eid);
                record["zIndex"] = json!(n - i);
                record["url"] = json!(extra.data_url());
                record["originX"] = json!(extra.origin_x);
                record["originY"] = json!(extra.origin_y);
                record["width"] = json!(extra.logical_w);
                record["height"] = json!(extra.logical_h);
                record["nativeWidth"] = json!(extra.phys_w);
                record["nativeHeight"] = json!(extra.phys_h);
                record["space"] = json!(kind);
                record["snapshot"] = json!(eid);
                screenshots.push(record);
                st.remember_extra_space(extra.origin_x, extra.origin_y, extra.phys_w, extra.phys_h);
            }
        }
    }
    let spaces: Vec<Value> = screenshots.iter().map(|s| json!({
        "id": s.get("id"),
        "zIndex": s.get("zIndex"),
        "space": s.get("space"),
        "windowID": s.get("windowID"),
        "displayName": s.get("displayName"),
        "processKey": s.get("processKey"),
        "app": s.get("app"),
        "snapshot": s.get("snapshot"),
        "originX": s.get("originX"),
        "originY": s.get("originY"),
        "width": s.get("width"),
        "height": s.get("height"),
        "nativeWidth": s.get("nativeWidth"),
        "nativeHeight": s.get("nativeHeight"),
    })).collect();
    // AX-15/TC-09: the official top level is exactly
    // {accessibility,cacheDiagnostics,screenshots,window} and every Screenshot has
    // exactly {height,id,originX,originY,url,width,zIndex}. The DSH space identity
    // rows are kept for the diagnostic_state extension method, never in the
    // model-visible payload.
    st.last_spaces = spaces;
    let official_screenshots: Vec<Value> = screenshots.iter().map(official_screenshot).collect();
    let full_diagnostics = overlay_cache_diagnostics(uia::global().diagnostics(), Some(&st), Some(&window));
    let cache_diagnostics = official_cache_diagnostics(&full_diagnostics);
    Ok(json!({
        "window": window.to_json(),
        "screenshots": official_screenshots,
        "accessibility": accessibility,
        "cacheDiagnostics": cache_diagnostics,
    }))
}

/// AX-15/TC-09: the official Screenshot element keys (out/types/Screenshot.d.ts).
const OFFICIAL_SCREENSHOT_KEYS: [&str; 7] =
    ["height", "id", "originX", "originY", "url", "width", "zIndex"];

fn official_screenshot(record: &Value) -> Value {
    let mut out = Map::new();
    for key in OFFICIAL_SCREENSHOT_KEYS {
        if let Some(value) = record.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}

/// AX-15: the official cacheDiagnostics payload is exactly three keys (M-C live
/// sample). The full diagnostic payload stays on the diagnostic_state extension.
const OFFICIAL_CACHE_DIAGNOSTIC_KEYS: [&str; 3] =
    ["accessibilityRevision", "accessibilitySnapshotCount", "captureCachedSessionCount"];

fn official_cache_diagnostics(full: &Value) -> Value {
    let mut out = Map::new();
    for key in OFFICIAL_CACHE_DIAGNOSTIC_KEYS {
        let value = full.get(key).cloned().unwrap_or_else(|| json!(0));
        out.insert(key.into(), value);
    }
    Value::Object(out)
}

fn overlay_cache_diagnostics(mut body: Value, st: Option<&HelperState>, window: Option<&WindowRef>) -> Value {
    let app_id = window
        .map(|w| w.app.as_str())
        .or_else(|| st.and_then(|s| s.last_window.as_ref().map(|w| w.app.as_str())))
        .or_else(|| st.and_then(|s| s.last_window_identity.as_ref().map(|i| i.app_id.as_str())))
        .unwrap_or("");
    body["appId"] = json!(app_id);
    if let Some(window) = window {
        body["rootHwnd"] = json!(window.id);
    } else if let Some(w) = st.and_then(|s| s.last_window.as_ref()) {
        body["rootHwnd"] = json!(w.id);
    } else if body.get("rootHwnd").is_none() {
        body["rootHwnd"] = json!(0);
    }
    body["inputHwnd"] = json!(id_from_hwnd(foreground_hwnd()));
    body["captureCachedSessionCount"] = json!(capture::cached_session_count());
    let capture_reason = capture::last_capture_invalidation();
    if !capture_reason.is_empty() {
        body["lastCaptureInvalidationReason"] = json!(capture_reason);
    } else if body.get("lastCaptureInvalidationReason").is_none() {
        body["lastCaptureInvalidationReason"] = json!("");
    }
    if body.get("accessibilityRevision").is_none() {
        body["accessibilityRevision"] = json!(0);
    }
    body
}

fn diagnostic_state(state: &Mutex<HelperState>) -> Result<Value, Error> {
    let mut body = uia::global().diagnostics();
    // Foreground probe: raw GetForegroundWindow + GetWindowThreadProcessId so a
    // "foreground window did not report a process id" failure is diagnosable
    // without a debugger.
    {
        let fg = foreground_hwnd();
        let mut pid = 0u32;
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, Some(&mut pid));
        }
        body["foreground"] = json!({
            "inputHwnd": id_from_hwnd(fg),
            "pid": pid,
            "exe": exe_name(pid).unwrap_or_default(),
        });
    }
    if let Ok(st) = state.lock() {
        body = overlay_cache_diagnostics(body, Some(&st), st.last_window.as_ref());
        body["ttlMs"] = json!(st.ttl_ms);
        body["backend"] = json!(st.backend);
        body["approvedApps"] = json!(st.approved.iter().cloned().collect::<Vec<_>>());
        body["overlay"] = json!(overlay::visible());
        body["lease"] = st.lease_snapshot();
        // AX-15: the DSH space identity rows removed from the model-visible
        // get_window_state payload stay reachable through this extension method.
        body["spaces"] = json!(st.last_spaces);
        body["screenshotSpaces"] = json!(st.last_spaces);
        body["rootScreenshotID"] = json!(st.last_shot_id);
        body["feature_status"] = notify::feature_status();
        body["notify"] = json!(true);
        body[LAST_WINDOW_IDENTITY] = st.identity_json();
        let hwnd = st.last_window.as_ref().map(|w| w.id).unwrap_or(0);
        let pid = if hwnd != 0 {
            window_pid(hwnd_from_id(hwnd))
        } else {
            0
        };
        if body.get("processId").and_then(Value::as_u64).unwrap_or(0) == 0 && pid != 0 {
            body["processId"] = json!(pid);
        }
        body["aumid"] = json!(policy::process_aumid(pid));
        let helper_rid = uia::integrity_rid(std::process::id());
        let target_rid = if pid != 0 { uia::integrity_rid(pid) } else { None };
        body["integrity"] = json!({
            "helperRid": helper_rid,
            "helperLabel": uia::integrity_label(helper_rid),
            "targetRid": target_rid,
            "targetLabel": uia::integrity_label(target_rid),
            "targetHigher": matches!((target_rid, helper_rid), (Some(t), Some(h)) if t > h),
        });
        if hwnd != 0 {
            if let Ok((x, y, width, height)) = window_rect(hwnd_from_id(hwnd)) {
                body["bounds"] = json!({ "x": x, "y": y, "width": width, "height": height });
            }
        }
        if body.get("processName").and_then(Value::as_str).unwrap_or("").is_empty() && pid != 0 {
            body["processName"] = json!(exe_name(pid).unwrap_or_default());
        }
        body["inputMonitor"] = interrupt::snapshot();
        body["appsFolderError"] = json!(app_catalog::last_appsfolder_error());
        // Catalog timing: the one Computer Use method that can run for seconds on a cold
        // machine, and the one the transport's 10 s budget turns into a helper restart.
        body["appCatalog"] = app_catalog::catalog_diagnostics();
        body["uiaWedge"] = uia::wedge_diagnostics();
        body["computerUseEnabled"] = json!(policy::computer_use_enabled());
        body["overlayState"] = overlay::diagnostics();
        body["captureState"] = capture::capture_diagnostics();
    } else {
        body = overlay_cache_diagnostics(body, None, None);
        body["appCatalog"] = app_catalog::catalog_diagnostics();
        body["aumid"] = json!("");
        body["inputMonitor"] = interrupt::snapshot();
        body["overlayState"] = overlay::diagnostics();
        body["captureState"] = capture::capture_diagnostics();
    }
    Ok(body)
}

/// Diagnostic for a batch entry that names no method.
///
/// A missing or empty `name` used to fall through to `dispatch`, whose official
/// diagnostic is `dsh_computer_use::protocol::UNSUPPORTED_METHOD_PREFIX` *with
/// nothing after it* (`unsupported method: `) -- which reads like a helper bug
/// rather than "this entry has no name". A *non-empty* unknown name keeps the
/// official string (TC-14 parity); only the nameless case gets this message.
const BATCH_ACTION_NAME_REQUIRED: &str = "each action needs a non-empty string \"name\" and an \"arguments\" object, e.g. {\"name\":\"press_key\",\"arguments\":{\"window\":{...},\"key\":\"Return\"}}; an item written as {\"action\":...} names no method to run";

/// The method a batch entry names, or why it cannot be dispatched.
///
/// Extracted from [`batch_actions`] so the guard is testable without a live
/// `HelperState` (the rest of that function needs a `Mutex<HelperState>`).
fn batch_action_name(action: &Value) -> Result<&str, &'static str> {
    match action.get("name").and_then(Value::as_str) {
        Some(name) if !name.trim().is_empty() => Ok(name),
        _ => Err(BATCH_ACTION_NAME_REQUIRED),
    }
}

fn batch_actions(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let Some(Value::Array(actions)) = params.get("actions") else {
        return Err(Error::type_err("actions array required"));
    };
    let mut results = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        // Name the offending index instead of dispatching "": see
        // `batch_action_name` for why the official prefix alone is a dead end.
        let name = match batch_action_name(action) {
            Ok(name) => name,
            Err(message) => {
                results.push(json!({"ok": false, "index": index, "error": message}));
                continue;
            }
        };
        let arguments = match action.get("arguments") {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
        // `index` rides along on both arms: two actions of the same method are
        // otherwise indistinguishable in the results list.
        match dispatch(state, name, &arguments) {
            Ok(v) => results.push(json!({"ok": true, "index": index, "name": name, "value": v})),
            Err(e) => results.push(json!({"ok": false, "index": index, "name": name, "error": e.message})),
        }
    }
    Ok(json!({"results": results}))
}

/// `json!` yields `serde_json::Value`; the internal dispatch helpers take
/// `&Map<String, Value>`. This narrows without cloning when it can.
fn as_map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("_".to_string(), other);
            map
        }
    }
}

/// Atomic form fill: one call, several field operations, one observation.
///
/// Motivation (2026-10-19, WorkBuddy login): filling a three-field login form
/// through the plain act-and-refresh loop cost 6-8 model round-trips, each one
/// re-uploading a full screenshot. A `fill_form` collapses that into a single
/// tool call whose result already contains the post-fill accessibility state.
///
/// Semantics are deliberately *replace*, not insert: every `set` runs through
/// the same `set_value` path (select-all + write + readback verify), so a field
/// that already holds a stale value can never end up as "old + new".
///
/// Steps are ordered and stop at the first failure, unless `continue_on_error`
/// is set — a half-filled form is easier to report than a silently wrong one.
fn fill_form(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    prepare_input(state, &window, WindowUse::Captured)?;
    let Some(Value::Array(steps)) = params.get("fields") else {
        return Err(Error::type_err("fields array required"));
    };
    if steps.is_empty() {
        return Err(Error::type_err("fields array must not be empty"));
    }
    let continue_on_error = params
        .get("continue_on_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Establish (or re-establish) this window's element cache before the first
    // step. A composite fill often runs right after an observation of a
    // *different* window (a `batch_actions` refresh, the overlay, a popup); the
    // per-step `set_value` then fails with "element N has no cached bounds" or
    // "last get_window_state was window id X" even though the caller did observe
    // the form. Running the real observation here is what makes `fill_form`
    // self-sufficient — and it is the same node table the steps then index into.
    //
    // BUT: every walk renumbers the nodes from zero, and this refresh happens
    // *after* the caller read its indices off its own snapshot. Writing a value
    // makes a live page insert or remove siblings (a resend countdown, a clear
    // button, an error line), so the numbering shifts and the index that named
    // the verification-code box in the caller's snapshot can name the
    // phone-number box in ours. Observed live on the TapTap desktop login form
    // (2026-10-20): a two-step `fill_form` wrote the verification code into the
    // phone field, and both steps still reported `ok`.
    //
    // So carry the caller's indices across the refresh as runtime ids: read each
    // step's runtime id from the cache as it stands (the caller's snapshot, when
    // it is still the live one), walk, then map the id back to the fresh index.
    // A control that left the tree is reported as a failed step rather than
    // silently retargeted — acting on the wrong control is worse than acting on
    // none.
    let mut carried: Vec<Option<Vec<i32>>> = Vec::with_capacity(steps.len());
    for step in steps.iter() {
        let runtime_id = step
            .as_object()
            .and_then(|s| s.get("element_index"))
            .and_then(Value::as_i64)
            .and_then(|i| uia::global().runtime_id(i as i32).ok())
            .filter(|id| !id.is_empty());
        carried.push(runtime_id);
    }

    let refresh = json!({
        "window": window.to_json(),
        "include_screenshot": false,
        "include_text": true,
    });
    get_window_state(state, &as_map(refresh))?;

    let mut results: Vec<Value> = Vec::new();
    let mut failed = false;

    for (i, step) in steps.iter().enumerate() {
        if failed && !continue_on_error {
            break;
        }
        let Some(step) = step.as_object() else {
            results.push(json!({"ok": false, "index": i, "error": "field entry must be an object"}));
            failed = true;
            continue;
        };
        // `element_index` is the canonical handle (from get_window_state). `text`
        // is a convenience that resolves against the last observation.
        let Some(index) = step.get("element_index").and_then(Value::as_i64) else {
            results.push(json!({"ok": false, "index": i, "error": "element_index is required"}));
            failed = true;
            continue;
        };
        // Re-resolve the caller's index against the post-refresh numbering.
        let index = match carried.get(i).and_then(Option::as_ref) {
            Some(runtime_id) => match uia::global().index_of(runtime_id) {
                Ok(Some(fresh)) => fresh as i64,
                Ok(None) => {
                    results.push(json!({
                        "ok": false, "index": i, "element_index": index,
                        "error": format!(
                            "element {index} left the tree while filling (the page re-rendered it); re-observe and retry"
                        ),
                    }));
                    failed = true;
                    continue;
                }
                Err(e) => {
                    results.push(json!({
                        "ok": false, "index": i, "element_index": index, "error": e,
                    }));
                    failed = true;
                    continue;
                }
            },
            // No runtime id to carry (the caller's snapshot was already gone):
            // fall back to the raw index, which at least matches a fresh
            // observation taken immediately before the call.
            None => index,
        };
        let value = step.get("value").and_then(Value::as_str);
        let kind = step
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or(if value.is_some() { "set" } else { "click" });

        let outcome = match kind {
            "set" | "value" => {
                let Some(value) = value else {
                    return Err(Error::type_err("action \"set\" needs a value"));
                };
                set_value(
                    state,
                    &as_map(json!({"window": window.to_json(), "element_index": index, "value": value})),
                )
            }
            "check" => dispatch(
                state,
                "perform_secondary_action",
                &as_map(json!({"window": window.to_json(), "element_index": index, "action": "Check"})),
            ),
            "click" => dispatch(
                state,
                "click",
                &as_map(json!({"window": window.to_json(), "element_index": index})),
            ),
            "press" => {
                let key = step.get("key").and_then(Value::as_str).unwrap_or("Return");
                dispatch(
                    state,
                    "press_key",
                    &as_map(json!({"window": window.to_json(), "key": key})),
                )
            }
            other => Err(Error::type_err(format!("unsupported field action: {other}"))),
        };

        match outcome {
            Ok(v) => {
                // Read-back verification for writes. A `set` that reports success
                // while the field kept its old (or empty) value is the 2026-10-22
                // React false success: UIA ValuePattern accepted the write, the
                // controlled input discarded it, and the tool answered `filled: 2`
                // with both fields blank (a login form got the code in the phone
                // box). `value_at` reads the element's *rendered* text when it has
                // one, so a discarded write is caught here and named, not counted
                // as filled. A control that publishes nothing stays unverifiable
                // and is reported as before — we only fail on proof.
                let mut wrong: Option<String> = None;
                if let (true, Some(want)) = (matches!(kind, "set" | "value"), value) {
                    let seen = uia::global().value_at(window.id as isize, index as i32);
                    wrong = write_missed(want, seen.as_deref());
                }
                match wrong {
                    Some(seen) => {
                        results.push(json!({
                            "ok": false, "index": i, "action": kind, "element_index": index,
                            "error": format!(
                                "the field did not keep the value: it reads {:?}; a controlled input or an IME likely discarded it (use type_text, or clear the field and retry)",
                                crate::clip(&seen, 40)
                            ),
                        }));
                        failed = true;
                    }
                    None => results.push(json!({"ok": true, "index": i, "action": kind, "element_index": index, "value": v})),
                }
            }
            Err(e) => {
                results.push(json!({
                    "ok": false, "index": i, "action": kind, "element_index": index,
                    "error": e.message,
                }));
                failed = true;
            }
        }
    }

    let filled = results.iter().filter(|r| r["ok"] == Value::Bool(true)).count();
    Ok(json!({
        "ok": !failed,
        "fields": results.len(),
        "filled": filled,
        "stoppedEarly": failed && !continue_on_error,
        "results": results,
    }))
}

fn set_value(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    prepare_input(state, &window, WindowUse::Captured)?;
    let index = json_i64(params, "element_index")?.ok_or_else(|| Error::type_err("element_index is required"))? as i32;
    // Official `set_value` accepts an empty string (TC-11); `app`/`key` keep the
    // non-empty requirement.
    let value = require_text_allow_empty(params, "value")?;
    match uia::global().set_value(window.id as isize, index, &value) {
        Ok(()) => {
            // Even a UIA SetValue that reported success is verified: a controlled
            // input may accept the write into the UIA value model and then render
            // something else (2026-10-23: `Value: v` was reported as ok). Only a
            // readable-and-wrong field fails; an unreadable one still passes.
            if let Some(seen) = uia::global().value_at(window.id as isize, index) {
                if let Some(seen) = write_missed(&value, Some(&seen)) {
                    return Err(Error::desktop(format!(
                        "set_value did not take: the field reads {:?}; a controlled input or an IME likely discarded it (clear the field and retry, or use type_text)",
                        clip(&seen, 40)
                    )));
                }
            }
            Ok(json!({}))
        }
        Err(e) if e.contains("not settable") => Err(Error::type_err(e)),
        Err(e) if e.contains("no longer matches") || e.contains("no longer belongs") => Err(Error::desktop(e)),
        Err(_) => {
            let (px, py) = {
                let st = state.lock().map_err(|_| Error::other("state lock"))?;
                let node = st
                    .last_nodes
                    .iter()
                    .find(|n| n.index == index)
                    .ok_or_else(|| Error::key(format!("element {index} has no cached bounds")))?;
                let vp = st.last_viewport.clone().ok_or_else(|| Error::desktop(COORDINATE_GEOMETRY_UNAVAILABLE))?;
                let (lx, ly) = node.bounds.center();
                vp.to_physical(lx, ly)
            };
            require_coord_hit(state, &window, px, py)?;
            overlay::show();
            let _ = overlay::position_for("set_value", px as f32, py as f32, true);
            interrupt::mark_synthetic(0.3);
            input::move_click(px, py, "left", 1)?;
            // Replace, never insert: a bare paste lands at the caret and produces
            // "old value + new text" instead of the value the caller asked for.
            input::replace_text_for(Some(window.id as isize), &value)?;
            // Verify the fallback path too: a paste into a controlled input can be
            // swallowed by an IME and leave a stray character (`v`), and the old
            // code returned ok unconditionally here (2026-10-23 false success).
            // Only fail on proof; an unreadable control stays ok.
            if let Some(seen) = uia::global().value_at(window.id as isize, index) {
                if let Some(seen) = write_missed(&value, Some(&seen)) {
                    return Err(Error::desktop(format!(
                        "set_value wrote the wrong text: the field reads {:?}; an IME or a controlled input likely swallowed the write (clear the field and retry, or use type_text)",
                        clip(&seen, 40)
                    )));
                }
            }
            Ok(json!({}))
        }
    }
}

fn secondary(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    prepare_uia_target(state, &window)?;
    let index = json_i64(params, "element_index")?.ok_or_else(|| Error::type_err("element_index is required"))? as i32;
    let action = require_text(params, "action")?;
    // Rebuild the UIA element cache immediately before the pattern call. Focus
    // and foreground events (including the observation overlay) bump the
    // invalidation generation, which clears the cached IUIAutomationElement map;
    // a UIA pattern action does not need a new observation, but it does need the
    // cached element, so refresh without activating the window.
    let hwnd = hwnd_from_id(window.id);
    if let Ok((ox, oy, _w, _h)) = window_rect(hwnd) {
        let dpi = dpi::window_dpi(hwnd);
        let scale = dpi::dpi_scale(dpi);
        let _ = uia::global().dump(window.id as isize, &window.title, (ox as f64, oy as f64), scale);
    }
    match uia::global().secondary(window.id as isize, index, &action) {
        Ok(()) => Ok(json!({})),
        Err(e) => {
            // Some providers (notably Win11 Notepad's RichEditBox) advertise
            // Scroll Up/Down but return an error from IUIAutomationScrollPattern.
            // Fall back to a real wheel scroll at the element center, matching
            // the coordinate scroll path.
            let key = action.trim().to_ascii_lowercase();
            let scroll_key = matches!(key.as_str(), "scroll up" | "scroll down" | "scroll left" | "scroll right");
            if scroll_key && e.contains("perform UIA secondary action") {
                if let Ok(bounds) = uia::global().cached_bounds(index) {
                    let (px, py) = bounds.center();
                    let delta = 600.0;
                    let (sx, sy) = match key.as_str() {
                        "scroll up" => (0.0, -delta),
                        "scroll down" => (0.0, delta),
                        "scroll left" => (-delta, 0.0),
                        _ => (delta, 0.0),
                    };
                    overlay::show();
                    let _ = overlay::position_for(&action, px as f32, py as f32, false);
                    input::scroll_at(px, py, sx, sy)?;
                    return Ok(json!({}));
                }
            }
            let msg = format!("[rust-secondary] {e}");
            if e.contains("has no cached") || e.contains("unsupported secondary") {
                Err(Error::key(msg))
            } else {
                Err(Error::desktop(msg))
            }
        }
    }
}

fn select_text(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    prepare_uia_target(state, &window)?;
    let index = json_i64(params, "element_index")?
        .ok_or_else(|| Error::type_err("element_index is required"))? as i32;
    let text = require_text(params, "text")?;
    let hwnd = hwnd_from_id(window.id);
    if let Ok((ox, oy, _w, _h)) = window_rect(hwnd) {
        let dpi = dpi::window_dpi(hwnd);
        let scale = dpi::dpi_scale(dpi);
        let _ = uia::global().dump(window.id as isize, &window.title, (ox as f64, oy as f64), scale);
    }
    uia::global()
        .select_text(window.id as isize, index, &text)
        .map_err(Error::desktop)?;
    Ok(json!({}))
}

fn click(state: &Mutex<HelperState>, params: &Map<String, Value>, action: &str) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    let indexed = json_i64(params, "element_index")?.is_some();
    prepare_input(state, &window, if indexed { WindowUse::Captured } else { WindowUse::Coordinate })?;
    let (px, py) = click_point(state, params)?;
    require_coord_hit(state, &window, px, py)?;
    let button = json_str(params, "mouse_button").unwrap_or_else(|| "left".into());
    let count = json_i64(params, "click_count")?.unwrap_or(1);
    overlay::show();
    let _ = overlay::position_for(action, px as f32, py as f32, true);
    interrupt::mark_synthetic(0.3);
    input::move_click(px, py, &button, count)?;
    Ok(json!({}))
}

fn scroll_pages(params: &Map<String, Value>) -> Result<f64, Error> {
    let Some(raw) = params.get("pages") else {
        return Ok(1.0);
    };
    if raw.is_null() {
        return Ok(1.0);
    }
    let number = match raw {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| Error::type_err("pages must be a finite number > 0"))?,
        Value::String(s) => s
            .parse::<f64>()
            .map_err(|_| Error::type_err("pages must be a finite number > 0"))?,
        _ => return Err(Error::type_err("pages must be a finite number > 0")),
    };
    if !number.is_finite() || number <= 0.0 {
        return Err(Error::type_err("pages must be a finite number > 0"));
    }
    Ok(number)
}

/// TC-12: the official `ScrollParams` has `scrollX`/`scrollY` as non-Option fields, so
/// a missing delta is a wiring error, not "scroll by zero".
fn required_delta(params: &Map<String, Value>, key: &str) -> Result<f64, Error> {
    json_f64(params, key)?.ok_or_else(|| Error::type_err(format!("{key} is required")))
}

fn scroll(state: &Mutex<HelperState>, params: &Map<String, Value>, action: &str) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    let indexed = json_i64(params, "element_index")?.is_some();
    prepare_input(state, &window, if indexed { WindowUse::Captured } else { WindowUse::Coordinate })?;
    if indexed {
        let index = json_i64(params, "element_index")?.unwrap() as i32;
        let direction = json_str(params, "direction").unwrap_or_else(|| "down".into());
        let pages = scroll_pages(params)?;
        interrupt::mark_synthetic(0.3);
        if uia::global()
            .scroll(window.id as isize, index, &direction, pages as i32)
            .is_ok()
        {
            return Ok(json!({}));
        }
        let (px, py) = click_point(state, params)?;
        require_coord_hit(state, &window, px, py)?;
        overlay::show();
        let _ = overlay::position_for(action, px as f32, py as f32, false);
        let delta = 600.0 * pages;
        let (sx, sy) = match direction.as_str() {
            "up" => (0.0, -delta),
            "down" => (0.0, delta),
            "left" => (-delta, 0.0),
            _ => (delta, 0.0),
        };
        input::scroll_at(px, py, sx, sy)?;
        return Ok(json!({}));
    }
    let (px, py) = click_point(state, params)?;
    require_coord_hit(state, &window, px, py)?;
    let sx = required_delta(params, "scrollX")?;
    let sy = required_delta(params, "scrollY")?;
    overlay::show();
    let _ = overlay::position_for(action, px as f32, py as f32, false);
    interrupt::mark_synthetic(0.3);
    input::scroll_at(px, py, sx, sy)?;
    Ok(json!({}))
}

fn drag(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<Value, Error> {
    let window = resolve_window(state, params)?;
    prepare_input(state, &window, WindowUse::Coordinate)?;
    let from_x = json_f64(params, "from_x")?.ok_or_else(|| Error::type_err("drag requires from_x"))?;
    let from_y = json_f64(params, "from_y")?.ok_or_else(|| Error::type_err("drag requires from_y"))?;
    let to_x = json_f64(params, "to_x")?.ok_or_else(|| Error::type_err("drag requires to_x"))?;
    let to_y = json_f64(params, "to_y")?.ok_or_else(|| Error::type_err("drag requires to_y"))?;
    let st = state.lock().map_err(|_| Error::other("state lock"))?;
    let (x1, y1, _) = st.logical_click_to_physical(from_x, from_y, json_str(params, "screenshotId").as_deref())?;
    let (x2, y2, _) = st.logical_click_to_physical(to_x, to_y, json_str(params, "screenshotId").as_deref())?;
    drop(st);
    require_coord_hit(state, &window, x1, y1)?;
    require_coord_hit(state, &window, x2, y2)?;
    overlay::show();
    let _ = overlay::position_for("drag", x1 as f32, y1 as f32, false);
    interrupt::mark_synthetic(0.3);
    input::drag_points(x1, y1, x2, y2)?;
    let _ = overlay::position_for("drag", x2 as f32, y2 as f32, true);
    Ok(json!({}))
}

fn require_coord_hit(state: &Mutex<HelperState>, window: &WindowRef, x: f64, y: f64) -> Result<(), Error> {
    let hwnd = hwnd_from_id(window.id);
    let (pid, extras) = {
        let st = state.lock().map_err(|_| Error::other("state lock"))?;
        let pid = st
            .last_window_identity
            .as_ref()
            .map(|id| id.process_id)
            .filter(|pid| *pid != 0)
            .unwrap_or_else(|| window_pid(hwnd));
        (pid, st.extra_rects.clone())
    };
    input::require_point_on_target(x, y, hwnd, &overlay::hwnds(), pid, &extras)
}

fn click_point(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<(f64, f64), Error> {
    let st = state.lock().map_err(|_| Error::other("state lock"))?;
    if let Some(index) = json_i64(params, "element_index")? {
        let node = st
            .last_nodes
            .iter()
            .find(|n| n.index == index as i32)
            .ok_or_else(|| Error::key(format!("element {index} has no cached bounds")))?;
        if node.bounds.width <= 0.0 && node.bounds.height <= 0.0 {
            return Err(Error::key(format!("element {index} has no cached bounds")));
        }
        let vp = st.last_viewport.clone().ok_or_else(|| Error::desktop(COORDINATE_GEOMETRY_UNAVAILABLE))?;
        let (lx, ly) = node.bounds.center();
        return Ok(vp.to_physical(lx, ly));
    }
    let x = json_f64(params, "x")?.ok_or_else(|| Error::type_err("click requires x"))?;
    let y = json_f64(params, "y")?.ok_or_else(|| Error::type_err("click requires y"))?;
    let shot = json_str(params, "screenshotId");
    let (px, py, _) = st.logical_click_to_physical(x, y, shot.as_deref())?;
    Ok((px, py))
}

fn resolve_window(state: &Mutex<HelperState>, params: &Map<String, Value>) -> Result<WindowRef, Error> {
    // Official `window.app is required` (TC-05): the identity has to be present
    // before any id-only or remembered-window fallback is considered.
    require_window_spec_app(params)?;
    let spec = match params.get("window") {
        Some(Value::Object(map)) => map,
        _ => params,
    };
    let id = spec
        .get("id")
        .and_then(protocol::json_u64)
        .or_else(|| params.get("id").and_then(protocol::json_u64))
        .unwrap_or(0);
    if id != 0 {
        let app = json_str(spec, "app").or_else(|| json_str(params, "app"));
        return lookup_window(id, app.as_deref());
    }
    if let Some(app) = json_str(spec, "app") {
        let windows = enum_windows()?;
        // `app` is now the official identity form (`process:<full path>`, `path:...`, a
        // bare exe name, ...), so comparing it with `eq_ignore_ascii_case` never matched.
        // `app_identity_matches` accepts every official form plus the bare name.
        if let Some(found) = windows.iter().find(|w| {
            app_identity_matches(&w.app, &app)
                || w.title.to_lowercase().contains(&app.to_lowercase())
        }) {
            return Ok(found.clone());
        }
    }
    if let Ok(st) = state.lock() {
        if let Some(window) = st.last_window.clone() {
            return Ok(window);
        }
    }
    enum_windows()?
        .into_iter()
        .next()
        .ok_or_else(|| Error::desktop("no targetable windows"))
}

/// Child process only. The overlay/parent path never calls SetSystemCursor.
fn run_system_cursor_manager(parent_pid: u32) -> i32 {
    unsafe {
        let suppress = CreateEventW(None, true, false, w!("Local\\DshComputerUse-CursorSuppress")).ok();
        let restore = CreateEventW(None, true, false, w!("Local\\DshComputerUse-CursorRestore")).ok();
        let shutdown = CreateEventW(None, true, false, w!("Local\\DshComputerUse-CursorShutdown")).ok();
        let ready = CreateEventW(None, true, false, w!("Local\\DshComputerUse-CursorReady")).ok();
        let ack = CreateEventW(None, true, false, w!("Local\\DshComputerUse-CursorAck")).ok();
        if let Some(ready) = ready {
            let _ = SetEvent(ready);
        }
        let parent = if parent_pid != 0 {
            OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid).ok()
        } else {
            None
        };
        let mut handles = Vec::new();
        if let Some(h) = suppress {
            handles.push(h);
        }
        if let Some(h) = restore {
            handles.push(h);
        }
        if let Some(h) = shutdown {
            handles.push(h);
        }
        if let Some(h) = parent {
            handles.push(h);
        }
        loop {
            if handles.is_empty() {
                break;
            }
            let idx = WaitForMultipleObjects(&handles, false, INFINITE).0;
            if idx == WAIT_OBJECT_0.0 {
                if let Some(h) = suppress {
                    let _ = ResetEvent(h);
                }
                let blanked = blank_and_set();
                // Acknowledge only a suppression that really replaced the cursors. The
                // parent waits for this ack, so acknowledging a failure would hide the
                // one thing it needs to know: the real pointer is still on screen.
                if blanked {
                    if let Some(h) = ack {
                        let _ = SetEvent(h);
                    }
                }
                if !blanked {
                    eprintln!("failed to suppress system cursors");
                }
            } else if idx == WAIT_OBJECT_0.0 + 1 {
                if let Some(h) = restore {
                    let _ = ResetEvent(h);
                }
                restore_cursors();
                if let Some(h) = ack {
                    let _ = SetEvent(h);
                }
            } else {
                break;
            }
        }
        restore_cursors();
    }
    0
}

fn restore_cursors() {
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
}

/// Blank every system cursor. Returns whether the replacement actually succeeded, so the
/// caller can decide whether the request deserves an acknowledgement.
fn blank_and_set() -> bool {
    unsafe {
        let and_mask = [0xFFu8; 128];
        let xor_mask = [0u8; 128];
        let Ok(blank) = CreateCursor(None, 0, 0, 32, 32, and_mask.as_ptr() as *const _, xor_mask.as_ptr() as *const _) else {
            return false;
        };
        let ids = [
            OCR_NORMAL,
            OCR_IBEAM,
            OCR_WAIT,
            OCR_CROSS,
            OCR_UP,
            SYSTEM_CURSOR_ID(32631),
            OCR_SIZENWSE,
            OCR_SIZENESW,
            OCR_SIZEWE,
            OCR_SIZENS,
            OCR_SIZEALL,
            OCR_NO,
            OCR_HAND,
            OCR_APPSTARTING,
            OCR_HELP,
        ];
        let mut ok = true;
        for id in ids {
            match CopyIcon(HICON(blank.0)) {
                Ok(copy) => {
                    if SetSystemCursor(HCURSOR(copy.0), id).is_err() {
                        ok = false;
                    }
                }
                Err(_) => ok = false,
            }
        }
        let _ = DestroyCursor(blank);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AX-15: the live M-C sample shows the official cacheDiagnostics object is exactly
    // {accessibilityRevision,accessibilitySnapshotCount,captureCachedSessionCount}.
    // Before this fix get_window_state returned the whole uia::diagnostics() payload
    // (16 keys), so this assertion is red on the pre-fix behaviour.
    #[test]
    fn official_cache_diagnostics_keeps_exactly_the_official_three_keys() {
        let full = json!({
            "appId": "mspaint.exe",
            "processId": 42,
            "rootHwnd": 7,
            "inputHwnd": 7,
            "processName": "mspaint.exe",
            "bounds": {"x": 0.0, "y": 0.0, "width": 100.0, "height": 100.0},
            "snapshotRevision": 3,
            "accessibilityRevision": 5,
            "accessibilitySnapshotCount": 6,
            "captureCachedSessionCount": 0,
            "lastCaptureInvalidationReason": "",
            "cachedElements": 12,
            "eventMonitor": true,
            "windowOpenedReady": true,
            "windowOpenedError": "",
            "elementIdSpace": 1,
            "invalidateGeneration": 9,
        });
        let projected = official_cache_diagnostics(&full);
        let mut keys: Vec<&str> = projected.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, OFFICIAL_CACHE_DIAGNOSTIC_KEYS.to_vec());
        assert_eq!(projected["accessibilityRevision"], 5);
        assert_eq!(projected["accessibilitySnapshotCount"], 6);
        assert_eq!(projected["captureCachedSessionCount"], 0);
    }

    #[test]
    fn official_cache_diagnostics_defaults_missing_counts_to_zero() {
        let projected = official_cache_diagnostics(&json!({"accessibilityRevision": 2}));
        assert_eq!(projected["accessibilityRevision"], 2);
        assert_eq!(projected["accessibilitySnapshotCount"], 0);
        assert_eq!(projected["captureCachedSessionCount"], 0);
    }

    // AX-15/TC-09: a Screenshot carries exactly the seven official keys; the DSH
    // identity extras (space/windowID/processKey/nativeWidth/...) must be dropped
    // from the model-visible payload, not just renamed.
    #[test]
    fn official_screenshot_keeps_exactly_the_official_seven_keys() {
        let record = json!({
            "id": "screenshot-0",
            "zIndex": 0,
            "url": "data:image/jpeg;base64,AAAA",
            "originX": 1,
            "originY": 2,
            "width": 640,
            "height": 480,
            "nativeWidth": 1280,
            "nativeHeight": 960,
            "space": "window",
            "windowID": 7,
            "displayName": "Paint",
            "processKey": "exe:mspaint.exe:42",
            "app": "mspaint.exe",
            "snapshot": "screenshot-0",
        });
        let projected = official_screenshot(&record);
        let mut keys: Vec<&str> = projected.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = OFFICIAL_SCREENSHOT_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
        assert_eq!(projected["nativeWidth"], Value::Null);
        assert_eq!(projected["width"], 640);
    }

    // TC-12: the official ScrollParams marks scrollX/scrollY required.
    #[test]
    fn scroll_deltas_are_required() {
        let empty = Map::new();
        assert_eq!(
            required_delta(&empty, "scrollX").unwrap_err().message,
            "scrollX is required"
        );
        assert_eq!(
            required_delta(&empty, "scrollY").unwrap_err().message,
            "scrollY is required"
        );
        let params = json!({"scrollX": 0, "scrollY": -120});
        let map = params.as_object().unwrap();
        assert_eq!(required_delta(map, "scrollX").unwrap(), 0.0);
        assert_eq!(required_delta(map, "scrollY").unwrap(), -120.0);
    }

    // The landing verdict is a pure function over collected signals, so the whole
    // decision table is testable without a live window.
    //
    // The case that motivated it (2026-10-22, DSH composer): a Chromium
    // `contenteditable` publishes its text through TextPattern only — its node
    // Value/Name are empty and the window dump does not contain the typed text —
    // so the pre-fix boolean check reported every paste as "did not land".
    #[test]
    fn landing_prefers_proof_over_silence() {
        // A native edit box that exposes its value: proven landed.
        let native = LandingSignals {
            node_text: Some("hello world".into()),
            after_value: Some("hello world".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&native, "hello"), Some(Landing::Landed));

        // A contenteditable: node text empty, but the text range carries the text.
        let editable = LandingSignals {
            node_text: None,
            range_text: Some("draft: hello".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&editable, "hello"), Some(Landing::Landed));

        // A control that publishes nothing at all cannot witness the paste.
        let opaque = LandingSignals::default();
        assert_eq!(landing_from(&opaque, "hello"), Some(Landing::Unverifiable));

        // A real control that publishes text, but not ours: proven absent.
        let absent = LandingSignals {
            node_text: Some("Copy".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&absent, "hello"), Some(Landing::Absent));
    }

    // A whole-window container must never be read as a witness. This is the
    // 2026-10-22 false positive: the focus sat on the browser's `RootWebArea`,
    // whose TextPattern range is the entire page — and the page contained the
    // agent's own tool-call echo (`type_text · SHOULD-NOT-LAND`), so a page-wide
    // search "confirmed" a paste that went nowhere.
    #[test]
    fn a_window_container_is_not_a_landing_witness() {
        // Container focus: publishes text (its title), no usable range, covers the
        // window. It cannot say where the paste went.
        let container = LandingSignals {
            node_text: Some("My App - Document".into()),
            range_text: None,
            container_focus: true,
            ..Default::default()
        };
        assert_eq!(
            landing_from(&container, "hello"),
            Some(Landing::Unverifiable)
        );

        // The same element *without* the container flag is an ordinary control, and
        // then absence is provable. The flag is what changes the verdict.
        let control = LandingSignals {
            node_text: Some("Copy".into()),
            range_text: None,
            container_focus: false,
            ..Default::default()
        };
        assert_eq!(landing_from(&control, "hello"), Some(Landing::Absent));

        // A container whose range still carried the text is landed by rule 3 only
        // if the range was trusted — it is not, because `collect_focused_signals`
        // drops the range for a container. Assert the ordering that makes that safe.
        let container_with_text = LandingSignals {
            node_text: Some("My App - Document".into()),
            range_text: Some("hello".into()),
            container_focus: true,
            ..Default::default()
        };
        assert_eq!(
            landing_from(&container_with_text, "hello"),
            Some(Landing::Landed)
        );
    }

    // A value that appeared out of nowhere is not proof of landing: it is just
    // focus moving to an empty control. The pre-fix check got this right and it
    // must stay right.
    #[test]
    fn a_value_that_had_nothing_before_is_not_proof() {
        let appeared = LandingSignals {
            node_text: Some("x".into()),
            before_value: None,
            after_value: Some("hello".into()),
            ..Default::default()
        };
        // Not `Landed`: nothing readable before means the change proves nothing.
        assert_eq!(landing_from(&appeared, "hello"), Some(Landing::Absent));

        let unchanged = LandingSignals {
            node_text: Some("x".into()),
            before_value: Some("old".into()),
            after_value: Some("old".into()),
            ..Default::default()
        };
        // AX-WEDGE anti-doubling (2026-10-25): an unchanged read-back is exactly what
        // a stale Chromium read looks like, so it is "cannot tell" now, NOT a proven
        // absence -- the old Absent verdict re-pasted and doubled the text in the
        // QQNT search box. A genuinely missed paste is caught by the caller's own
        // read-back instead (the false success costs a re-observe; the false failure
        // cost corrupted field content).
        assert_eq!(landing_from(&unchanged, "hello"), Some(Landing::Unverifiable));

        let changed = LandingSignals {
            node_text: Some("x".into()),
            before_value: Some("old".into()),
            after_value: Some("hello".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&changed, "hello"), Some(Landing::Landed));
    }

    // A changed value that does not carry the typed text is NOT a landing: the
    // 2026-10-23 false positive, where an IME ate the paste and left a stray `v`
    // in a React input while type_text reported ok.
    #[test]
    fn a_changed_value_without_the_text_is_wrong_not_landed() {
        // Typed "E2E-FWD-20261023", the field reads "v".
        let mangled = LandingSignals {
            node_text: Some("v".into()),
            before_value: Some(String::new()),
            after_value: Some("v".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&mangled, "E2E-FWD-20261023"), Some(Landing::Wrong));

        // A prefix of the text is still not the text.
        let partial = LandingSignals {
            node_text: Some("E2E".into()),
            before_value: Some(String::new()),
            after_value: Some("E2E".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&partial, "E2E-FWD-20261023"), Some(Landing::Wrong));

        // The text plus other content around it is a landing (the field may already
        // have held something, or the app may append).
        let around = LandingSignals {
            node_text: Some("see E2E-FWD-20261023 now".into()),
            before_value: Some("see ".into()),
            after_value: Some("see E2E-FWD-20261023 now".into()),
            ..Default::default()
        };
        assert_eq!(landing_from(&around, "E2E-FWD-20261023"), Some(Landing::Landed));
    }

    // A wrong landing must be reported as a failure and must outrank a plain
    // absence when two passes disagree.
    #[test]
    fn a_wrong_landing_fails_and_outranks_absence() {
        assert!(Landing::Wrong.failed_with(true));
        assert!(Landing::Wrong.failed_with(false));
        assert!(!Landing::Landed.failed_with(false));

        assert_eq!(
            Landing::Absent.max_by_confidence(Landing::Wrong),
            Landing::Wrong
        );
        assert_eq!(
            Landing::Wrong.max_by_confidence(Landing::Absent),
            Landing::Wrong
        );
        // A proven landing still wins over a wrong reading from another pass.
        assert_eq!(
            Landing::Wrong.max_by_confidence(Landing::Landed),
            Landing::Landed
        );
    }

    // The write read-back used by set_value and fill_form's `set`: a control that
    // reads back without the value is a named miss; an unreadable one is not.
    #[test]
    fn a_write_readback_without_the_value_is_a_miss() {
        // Kept: the rendered text carries what we wrote.
        assert_eq!(write_missed("1234", Some("1234")), None);
        assert_eq!(write_missed("1234", Some("code 1234 ok")), None);
        // Missed: the field shows something else (an IME fragment, old text).
        assert_eq!(write_missed("E2E-FWD", Some("v")), Some("v".to_string()));
        assert_eq!(write_missed("1234", Some("")), Some(String::new()));
        // Unreadable, or nothing to write: never a failure.
        assert_eq!(write_missed("1234", None), None);
        assert_eq!(write_missed("", Some("anything")), None);
        // Windows line endings do not count against a match: the read-back is
        // stripped of CR, so a CRLF-carrying field still matches a plain want.
        assert_eq!(write_missed("line1\nline2", Some("line1\r\nline2")), None);
    }

    // The landing check must never search the whole window: a window-wide text
    // search cannot witness a paste in any app that echoes the tool call back into
    // its UI, and it costs a full tree walk on a browser window (which is what
    // blew the 20s tool budget). Assert the source has no window-level probe left
    // in the landing path.
    #[test]
    fn the_landing_check_never_searches_the_window() {
        let src = include_str!("main.rs");
        let start = src
            .find("fn landing_from(")
            .expect("landing_from must exist");
        let end = src
            .find("/// Whether the paste should be reported as failed")
            .expect("the Landing impl must follow the landing helpers");
        let landing = &src[start..end];
        for forbidden in ["window_landing_probe", "window_contains", "dump("] {
            assert!(
                !landing.contains(forbidden),
                "the landing path must not search the window ({forbidden})"
            );
        }
    }

    // A paste into a surface that never publishes its text must NOT be reported as
    // a failure. This is the general rule the tri-state verdict exists for: only a
    // proven absence fails, and "the target cannot be read" is not proof.
    //
    // Regression (2026-10-22): typing into the DSH composer — a Chromium
    // contenteditable — pasted the text correctly, saw an empty accessibility dump
    // (accessibilityRevision 0, no node carrying the text), then retried, pasting it
    // a second time before finally reporting
    // "type_text did not land in the target window".
    #[test]
    fn an_unreadable_target_does_not_prove_absence() {
        // Unverifiable + the focus is an editable control inside the target => ok.
        assert!(!Landing::Unverifiable.failed_with(true));
        // Unverifiable + the focus cannot take text => a real failure.
        assert!(Landing::Unverifiable.failed_with(false));
        // A readable target whose dump lacks the text is a proven miss.
        assert!(Landing::Absent.failed_with(true));
        // Proven landing never fails, whatever the focus looks like.
        assert!(!Landing::Landed.failed_with(false));
    }

    /// AX-WEDGE anti-doubling (2026-10-25): a slow Chromium field commits the paste
    /// asynchronously, so the read-back can still carry the PRE-paste value. A
    /// readable-but-unchanged read-back that no positive signal witnessed used to
    /// fall through to `Absent` ("proven absence"), which re-pasted and doubled the
    /// text in the QQNT search box. Unchanged is "cannot tell", not "proven
    /// absence"; a read-back that DID change still decides exactly as before.
    #[test]
    fn a_stale_unchanged_read_back_is_not_a_proven_absence() {
        // The QQNT search box case: empty before, stale empty after.
        let stale = LandingSignals {
            node_text: Some(String::new()),
            range_text: None,
            before_value: Some(String::new()),
            after_value: Some(String::new()),
            container_focus: false,
        };
        assert_eq!(landing_from(&stale, "1018595918"), Some(Landing::Unverifiable));
        // Non-empty stale value: same verdict.
        let stale = LandingSignals {
            node_text: Some("old note".to_string()),
            range_text: None,
            before_value: Some("old note".to_string()),
            after_value: Some("old note".to_string()),
            container_focus: false,
        };
        assert_eq!(landing_from(&stale, "1018595918"), Some(Landing::Unverifiable));
        // A read-back that DID change still decides, unchanged behavior:
        // carrying the text is Landed, carrying something else is Wrong.
        let landed = LandingSignals {
            node_text: Some(String::new()),
            range_text: None,
            before_value: Some(String::new()),
            after_value: Some("1018595918".to_string()),
            container_focus: false,
        };
        assert_eq!(landing_from(&landed, "1018595918"), Some(Landing::Landed));
        let wrong = LandingSignals {
            node_text: Some("v".to_string()),
            range_text: None,
            before_value: Some(String::new()),
            after_value: Some("v".to_string()),
            container_focus: false,
        };
        assert_eq!(landing_from(&wrong, "1018595918"), Some(Landing::Wrong));
        // before=None (unreadable before) with a readable non-carrying after stays
        // Absent: the value DID change, so the read is not stale.
        let absent = LandingSignals {
            node_text: Some("header".to_string()),
            range_text: None,
            before_value: None,
            after_value: Some("header".to_string()),
            container_focus: false,
        };
        assert_eq!(landing_from(&absent, "1018595918"), Some(Landing::Absent));
    }

    #[test]
    fn the_settle_never_downgrades_a_proven_verdict() {
        // A slow commit turns "not landed yet" into Landed; a second look must not
        // turn a proven absence into "unverifiable" (that would excuse real misses).
        assert_eq!(
            Landing::Absent.max_by_confidence(Landing::Unverifiable),
            Landing::Unverifiable
        );
        assert_eq!(
            Landing::Unverifiable.max_by_confidence(Landing::Absent),
            Landing::Unverifiable
        );
        assert_eq!(
            Landing::Absent.max_by_confidence(Landing::Landed),
            Landing::Landed
        );
        assert_eq!(
            Landing::Absent.max_by_confidence(Landing::Absent),
            Landing::Absent
        );
    }

    // A batch entry that names no method must say so, and say which one. Before
    // the guard, `dispatch("")` answered the bare official prefix
    // `unsupported method: ` -- nothing after the colon, so the caller could not
    // tell a malformed entry from a broken helper.
    #[test]
    fn a_batch_action_without_a_name_is_named_by_its_index() {
        for nameless in [
            json!({"action": "press_key"}),   // wrong key: the model wrote `action`
            json!({"name": ""}),              // present but empty
            json!({"name": "   "}),           // whitespace only
            json!({"name": 7}),               // present but not a string
        ] {
            assert_eq!(
                batch_action_name(&nameless).unwrap_err(),
                BATCH_ACTION_NAME_REQUIRED,
                "nameless entry must be rejected: {nameless}"
            );
        }
        // A named entry still resolves, so the guard cannot swallow real work.
        assert_eq!(
            batch_action_name(&json!({"name": "press_key", "arguments": {}})).unwrap(),
            "press_key"
        );
        // The official unknown-method string stays reserved for a *named* method
        // (TC-14 parity): this guard must not repurpose it.
        assert_ne!(
            BATCH_ACTION_NAME_REQUIRED,
            dsh_computer_use::protocol::UNSUPPORTED_METHOD_PREFIX
        );
    }

    // APS-04: computer audio has a fixed approval subject; it never falls through to
    // the window/app identity of the request.
    #[test]
    fn audio_approval_subject_is_the_fixed_computer_audio_id() {
        assert_eq!(target_app("start_audio_recording", &Map::new()), AUDIO_APP);
        let audio = Error::approval(AUDIO_APP);
        assert!(matches!(audio.kind, protocol::ErrorKind::Approval));
        assert_eq!(audio.message, protocol::AUDIO_APPROVAL_MESSAGE);
        let request = audio.approval.as_ref().unwrap();
        assert_eq!(request["app"], AUDIO_APP);
        assert_eq!(request["displayName"], protocol::AUDIO_DISPLAY);
        assert_eq!(request["allowPersistentApproval"], false);
    }

    /// Regression for the 2026-10-20 TapTap login run.
    ///
    /// `fill_form` re-observes before running its steps, and that walk renumbers
    /// every node from zero. A page that inserts a sibling after the first write
    /// (the resend countdown the login form grows, a clear button, an error line)
    /// shifts the numbering, so the caller's index for the *second* field then
    /// names whatever slid into that slot. On TapTap that wrote the SMS code into
    /// the phone-number box while both steps reported `ok`.
    ///
    /// The contract this pins down: an index is only meaningful inside one
    /// snapshot; a runtime id is the handle that survives renumbering.
    #[test]
    fn renumbering_between_steps_must_be_resolved_by_runtime_id_not_index() {
        // Snapshot A: what the model saw, indices off the caller's observation.
        let before: Vec<(i32, Vec<i32>)> = vec![(19, vec![42, 91]), (20, vec![42, 92])];
        // Snapshot B: the page inserted a node ("重新发送 57s") *between* the two
        // fields, so every node below it shifted down by one. Index 20 now names
        // the countdown instead of the verification-code box.
        let after: Vec<(i32, Vec<i32>)> = vec![(19, vec![42, 91]), (20, vec![42, 2088]), (21, vec![42, 92])];

        let index_of = |runtime_id: &Vec<i32>| -> Option<i32> {
            after.iter().find(|(_, id)| id == runtime_id).map(|(i, _)| *i)
        };
        let runtime_id_at = |index: i32| -> Option<Vec<i32>> {
            after.iter().find(|(i, _)| *i == index).map(|(_, id)| id.clone())
        };

        // The flattened index the caller passed for the code field no longer
        // names the code field.
        let caller_index_for_code = 20;
        assert_eq!(runtime_id_at(caller_index_for_code), Some(vec![42, 2088]));
        assert_ne!(runtime_id_at(caller_index_for_code), Some(before[1].1.clone()));

        // Carrying the runtime id finds it again.
        let caller_runtime_id_for_code = before[1].1.clone();
        assert_eq!(index_of(&caller_runtime_id_for_code), Some(21));

        // And a control the page dropped is reported as gone, never retargeted.
        let vanished = vec![42, 999];
        assert_eq!(index_of(&vanished), None);
    }
}

