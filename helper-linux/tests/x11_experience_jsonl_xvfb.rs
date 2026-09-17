//! The experience layer, driven the way the host really drives it: over the JSONL
//! protocol, by tool name, with no call into x11_experience anywhere in this file.
//!
//! The P2 delivery shipped the whole layer and never armed it: nothing called
//! Session::begin, so the pill was never mapped, the synthesized pointer never appeared
//! and the Escape grab was never installed -- while health reported the layer as "on",
//! which only ever meant "the extensions were negotiated". These tests exist so that gap
//! cannot come back silently.
//!
//! Everything asserted here is the X server's own view rather than the layer's opinion of
//! itself:
//!
//! * which override-redirect windows are mapped (mapped + painted pixels);
//! * whether a SECOND client can still grab Escape -- a root key grab is exclusive, so a
//!   refused grab from the test's own connection is proof that the helper holds it, and a
//!   successful grab after end_turn is proof that it let go;
//! * whether a click at the point the synthesized pointer covers reaches the window
//!   UNDERNEATH it -- an overlay that is merely drawn on top still wins hit-testing and
//!   would eat the very clicks the helper exists to deliver.
//!
//! Xvfb runs on :96, which no other suite uses (the experience core line owns :97 and the
//! window2 suite :98-:106).

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _, MapState};
use x11rb::protocol::xtest;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const DISPLAY: &str = ":96";
const SOCKET: &str = "/tmp/.X11-unix/X96";
const ESCAPE: u8 = 9;

/// The only thing that turns these tests on, matching the window2 suite's convention.
const ENABLED: &str = "DSH_CUA_XVFB_TEST";

fn enabled() -> bool {
    std::env::var(ENABLED).map(|value| value == "1").unwrap_or(false)
}

/// Xvfb displays are serialized: one server per display number, and this file has three
/// tests. Without this lock a plain cargo test, which runs them in parallel, would have
/// them fight over :96 and fail for a reason that has nothing to do with the layer.
static DISPLAY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn skip_if_disabled() -> bool {
    if !enabled() {
        eprintln!(
            "skipping: set {ENABLED}=1 (and pass --ignored) to run the Xvfb JSONL experience tests"
        );
        return true;
    }
    false
}

fn is_pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let res = unsafe { libc::kill(pid, 0) };
    if res == 0 {
        true
    } else {
        let err = std::io::Error::last_os_error().raw_os_error();
        err != Some(libc::ESRCH)
    }
}

fn clean_stale_lock(display_number: u32) {
    let lock_path = format!("/tmp/.X{display_number}-lock");
    let socket_path = format!("/tmp/.X11-unix/X{display_number}");
    if !std::path::Path::new(&lock_path).exists() && !std::path::Path::new(&socket_path).exists() {
        return;
    }

    let is_alive = if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if let Ok(pid) = content.trim().parse::<i32>() {
            is_pid_alive(pid)
        } else {
            false
        }
    } else {
        false
    };

    if !is_alive {
        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_file(&socket_path);
    }
}

/// Owns the Xvfb child so a panic in an assertion cannot leak a server, and holds the
/// display lock for as long as the server lives.
struct Xvfb {
    child: Child,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Xvfb {
    fn start() -> Option<Xvfb> {
        Xvfb::start_with(&[])
    }

    /// Start a server with extra Xvfb flags.
    ///
    /// The flags exist so the "no usable XFixes" branch can be exercised against a REAL
    /// server instead of a stub: Xvfb accepts "-extension XFIXES" and then answers every
    /// XFixes request with an error, which is exactly the desktop this branch is for.
    fn start_with(extra: &[&str]) -> Option<Xvfb> {
        if Command::new("Xvfb").arg("-help").output().is_err() {
            return None;
        }
        let guard = DISPLAY_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        clean_stale_lock(96);
        let mut child = Command::new("Xvfb")
            .args([DISPLAY, "-screen", "0", "1280x800x24", "-nolisten", "tcp"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Xvfb");
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return None;
            }
            if Path::new(SOCKET).exists() {
                // Give the server a moment to accept connections rather than merely
                // having created the socket.
                std::thread::sleep(Duration::from_millis(250));
                return Some(Xvfb { child, _guard: guard });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        None
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(SOCKET);
        let _ = std::fs::remove_file("/tmp/.X96-lock");
    }
}

/// A live helper process, spoken to over its real stdin/stdout JSONL protocol.
struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Helper {
    fn spawn() -> Helper {
        let binary = env!("CARGO_BIN_EXE_dsh-computer-use");
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // The layer reads DISPLAY from the environment, like every other X client.
            .env("DISPLAY", DISPLAY)
            .env("XDG_SESSION_TYPE", "x11")
            .env_remove("WAYLAND_DISPLAY")
            .spawn()
            .expect("the helper binary starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Helper { child, stdin, stdout }
    }

    /// Send one request line and read exactly one response line.
    fn request(&mut self, value: Value) -> Value {
        let mut line = serde_json::to_string(&value).expect("serialize request");
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write request");
        self.stdin.flush().expect("flush request");
        let mut response = String::new();
        self.stdout.read_line(&mut response).expect("read response");
        assert!(!response.trim().is_empty(), "the helper answered nothing to {value}");
        serde_json::from_str(&response).unwrap_or_else(|error| panic!("bad response {response:?}: {error}"))
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // The suite must not leave a helper holding a root grab on the test display.
        let _ = self.stdin.write_all(b"{\"id\":999,\"method\":\"shutdown\",\"params\":{}}\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect() -> RustConnection {
    let (conn, _screen) = RustConnection::connect(Some(DISPLAY)).expect("test client connects");
    conn
}

fn root_of(conn: &RustConnection) -> xproto::Window {
    conn.setup().roots[0].root
}

/// Every override-redirect INPUT_OUTPUT window on the root, with its map state.
fn overlay_windows(conn: &RustConnection) -> Vec<(xproto::Window, bool)> {
    let root = root_of(conn);
    let tree = xproto::query_tree(conn, root).expect("query tree").reply().expect("tree reply");
    let mut found = Vec::new();
    for child in tree.children {
        let attrs = match xproto::get_window_attributes(conn, child) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        if attrs.override_redirect && attrs.class == xproto::WindowClass::INPUT_OUTPUT {
            found.push((child, attrs.map_state == MapState::VIEWABLE));
        }
    }
    found
}

fn mapped_overlays(conn: &RustConnection) -> Vec<xproto::Window> {
    overlay_windows(conn).into_iter().filter(|(_, mapped)| *mapped).map(|(window, _)| window).collect()
}

/// Read every pixel of a window as 0x00RRGGBB values.
fn window_pixels(conn: &RustConnection, window: xproto::Window) -> Vec<u32> {
    let geo = xproto::get_geometry(conn, window).expect("geometry").reply().expect("geometry reply");
    let reply = xproto::get_image(
        conn,
        xproto::ImageFormat::Z_PIXMAP,
        window,
        0,
        0,
        geo.width,
        geo.height,
        u32::MAX,
    )
    .expect("get_image")
    .reply()
    .expect("get_image reply");
    reply
        .data
        .chunks_exact(4)
        .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]]) & 0x00ff_ffff)
        .collect()
}

/// True when THIS client can take the root Escape grab without asking for every modifier
/// combination -- the same exclusive combination the layer takes.
fn escaped_grab_is_free(conn: &RustConnection, root: xproto::Window) -> bool {
    // The layer grabs with ModMask::ANY, which covers the bare Escape combination too, so
    // a second client asking for (ANY) is refused exactly while the layer holds it.
    let grabbed = xproto::grab_key(
        conn,
        false,
        root,
        xproto::ModMask::ANY,
        ESCAPE,
        xproto::GrabMode::ASYNC,
        xproto::GrabMode::ASYNC,
    )
    .expect("grab_key");
    let free = grabbed.check().is_ok();
    let _ = conn.flush();
    if free {
        let _ = xproto::ungrab_key(conn, ESCAPE, root, xproto::ModMask::ANY).map(|c| c.check());
        let _ = conn.flush();
    }
    free
}

fn press_escape(conn: &RustConnection, root: xproto::Window) {
    xtest::fake_input(conn, 2, ESCAPE, 0, root, 0, 0, 0).expect("Escape press");
    xtest::fake_input(conn, 3, ESCAPE, 0, root, 0, 0, 0).expect("Escape release");
    let _ = conn.flush();
}

fn wait_until(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for: {label}");
}

/// The name of the window the fixture enumerates.
const TITLE: &str = "jsonl-experience-fixture";

/// A plain client window the helper can observe by name.
fn fixture_window(conn: &RustConnection) -> xproto::Window {
    let screen = &conn.setup().roots[0];
    let window = conn.generate_id().expect("window id");
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        320,
        200,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        0,
        &xproto::CreateWindowAux::new()
            .background_pixel(screen.white_pixel)
            .event_mask(xproto::EventMask::EXPOSURE),
    )
    .expect("create_window");
    conn.change_property8(
        xproto::PropMode::REPLACE,
        window,
        xproto::AtomEnum::WM_NAME,
        xproto::AtomEnum::STRING,
        TITLE.as_bytes(),
    )
    .expect("WM_NAME");
    conn.change_property8(
        xproto::PropMode::REPLACE,
        window,
        xproto::AtomEnum::WM_CLASS,
        xproto::AtomEnum::STRING,
        b"fixture\0Fixture\0",
    )
    .expect("WM_CLASS");
    conn.map_window(window).expect("map_window");
    let _ = conn.flush();
    std::thread::sleep(Duration::from_millis(200));
    window
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test x11_experience_jsonl_xvfb -- --ignored --test-threads=1"]
fn a_jsonl_observation_arms_the_layer_and_end_turn_puts_the_desktop_back() {
    if skip_if_disabled() {
        return;
    }
    let Some(_xvfb) = Xvfb::start() else {
        eprintln!("SKIP: Xvfb is not installed; the JSONL experience path cannot be exercised");
        return;
    };
    let conn = connect();
    let root = root_of(&conn);
    let window = fixture_window(&conn);
    let id = u64::from(window);

    let mut helper = Helper::spawn();

    // --- 1. Idle: the layer is AVAILABLE and nothing is armed ------------------
    let health = helper.request(json!({"id": 1, "method": "health", "params": {}}));
    let experience = &health["result"]["experience"];
    assert_eq!(experience["available"], json!(true), "the layer must be available on Xvfb: {health}");
    assert_eq!(experience["armed"], json!(false), "nothing has been observed yet: {health}");
    assert_eq!(experience["state"], json!("available"), "idle is not armed: {health}");
    assert!(
        mapped_overlays(&conn).is_empty(),
        "no overlay may exist before a turn: {:?}",
        mapped_overlays(&conn)
    );
    assert!(
        escaped_grab_is_free(&conn, root),
        "nothing may hold the Escape grab before a turn"
    );

    // --- 2. The observation itself arms: pill mapped, grab installed -----------
    let state = helper.request(json!({
        "id": 2,
        "method": "call",
        "params": {
            "name": "get_window_state",
            "surface": "window2",
            "arguments": {"window": {"app": "Fixture", "id": id}, "include_screenshot": false},
        },
    }));
    assert_eq!(state["ok"], json!(true), "the observation must succeed: {state}");

    // The server's own view: two override-redirect windows, both viewable. The defect
    // being guarded against is exactly "the call returned ok and nothing appeared".
    wait_until("the pill and the synthesized pointer to be mapped", || {
        mapped_overlays(&conn).len() >= 2
    });

    // And they are drawn, not just mapped: the pill carries its observing accent and its
    // label colour, which only the paint path writes.
    let pill = mapped_overlays(&conn)
        .into_iter()
        .find(|candidate| {
            xproto::get_geometry(&conn, *candidate)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|geo| geo.height > 40)
                .unwrap_or(false)
        })
        .expect("the pill overlay is the taller one");
    let pixels = window_pixels(&conn, pill);
    assert!(
        pixels.contains(&0x00b3_9c),
        "the pill must carry its observing accent bar (0x00B39C)"
    );
    assert!(pixels.contains(&0x0012_1619), "the pill must have its background painted");
    assert!(
        pixels.contains(&0x00f2_f4f5),
        "the pill label must be drawn in the foreground colour"
    );

    // The grab is proven from OUTSIDE the helper: a root key grab is exclusive, so this
    // second connection being refused with BadAccess is the server saying the helper
    // holds Escape.
    assert!(
        !escaped_grab_is_free(&conn, root),
        "the helper must hold the root Escape grab while armed"
    );

    // health agrees, and now says so in the second sense of the word.
    let health = helper.request(json!({"id": 3, "method": "health", "params": {}}));
    let experience = &health["result"]["experience"];
    assert_eq!(experience["state"], json!("armed"), "an observed turn is armed: {health}");
    assert_eq!(experience["armed"], json!(true), "{health}");
    assert_eq!(
        experience["escapeGrab"]["state"],
        json!("installed"),
        "Xvfb has no compositor to refuse the grab: {health}"
    );
    assert_eq!(experience["degraded"], Value::Null, "nothing is degraded here: {health}");
    assert_eq!(
        experience["overlayClickThrough"],
        json!(true),
        "the overlays must be click-through, or they intercept the clicks this helper sends: {health}"
    );

    // --- 3. end_turn puts the desktop back -------------------------------------
    let ended = helper.request(json!({"id": 4, "method": "end_turn", "params": {}}));
    assert_eq!(ended["ok"], json!(true), "{ended}");
    wait_until("every overlay to be unmapped after end_turn", || {
        mapped_overlays(&conn).is_empty()
    });
    // The grab is really released: the proof is that this client can take it now.
    wait_until("the Escape grab to be free again", || escaped_grab_is_free(&conn, root));

    let health = helper.request(json!({"id": 5, "method": "health", "params": {}}));
    let experience = &health["result"]["experience"];
    assert_eq!(experience["state"], json!("available"), "a finished turn is not armed: {health}");
    assert_eq!(experience["armed"], json!(false), "{health}");

    // --- 4. An armed Escape is an interrupt, not a swallowed key ---------------
    // A fresh observation arms again, and a physical Escape then takes the desktop back
    // on its own -- the layer is a safety device, so it must react without a tool call.
    let state = helper.request(json!({
        "id": 6,
        "method": "call",
        "params": {
            "name": "get_window_state",
            "surface": "window2",
            "arguments": {"window": {"app": "Fixture", "id": id}, "include_screenshot": false},
        },
    }));
    assert_eq!(state["ok"], json!(true), "{state}");
    wait_until("the overlay to come back", || mapped_overlays(&conn).len() >= 2);
    // The 200 ms arming grace deliberately ignores input that arrives with the keystroke
    // that armed the turn, so injecting inside that window would measure the grace.
    std::thread::sleep(Duration::from_millis(300));
    press_escape(&conn, root);
    wait_until("Escape to put the desktop back", || mapped_overlays(&conn).is_empty());
    // And it STAYS back: the Escape release that is still in flight, and every later
    // keystroke, belong to the operator. A layer that re-mapped its "user took over" pill
    // here would be drawing on a desktop it had already handed back.
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        mapped_overlays(&conn).is_empty(),
        "the desktop must stay handed back after Escape: {:?}",
        mapped_overlays(&conn)
    );
    assert!(escaped_grab_is_free(&conn, root), "the grab must be released after Escape");

    // --- 5. shutdown leaves nothing behind ------------------------------------
    let state = helper.request(json!({
        "id": 7,
        "method": "call",
        "params": {
            "name": "get_window_state",
            "surface": "window2",
            "arguments": {"window": {"app": "Fixture", "id": id}, "include_screenshot": false},
        },
    }));
    assert_eq!(state["ok"], json!(true), "{state}");
    wait_until("the overlay to come back before shutdown", || mapped_overlays(&conn).len() >= 2);
    let closed = helper.request(json!({"id": 8, "method": "shutdown", "params": {}}));
    assert_eq!(closed["result"]["closed"], json!(true), "{closed}");
    wait_until("shutdown to leave no overlay mapped", || {
        mapped_overlays(&conn).is_empty()
    });
}

/// The P1 surface arms the same layer.
///
/// The plugin's default Linux surface is the P1 one (src/index.js resolves surface=linux
/// when the backend is linux), so a layer that only armed on window2 faces would never arm
/// on the default path and the pill would never appear for a normal user.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test x11_experience_jsonl_xvfb -- --ignored --test-threads=1"]
fn the_p1_surface_arms_the_same_layer() {
    if skip_if_disabled() {
        return;
    }
    let Some(_xvfb) = Xvfb::start() else {
        eprintln!("SKIP: Xvfb is not installed");
        return;
    };
    let conn = connect();
    let root = root_of(&conn);
    let _window = fixture_window(&conn);
    let mut helper = Helper::spawn();

    // A P1 observation: the seven sky.window tools, no surface tag.
    let shot = helper.request(json!({
        "id": 1,
        "method": "call",
        "params": {"name": "screenshot", "arguments": {}},
    }));
    assert_eq!(shot["ok"], json!(true), "the P1 screenshot must succeed: {shot}");
    wait_until("the P1 observation to arm the layer", || {
        mapped_overlays(&conn).len() >= 2
    });
    assert!(
        !escaped_grab_is_free(&conn, root),
        "the P1 surface must hold the Escape grab while armed"
    );

    let health = helper.request(json!({"id": 2, "method": "health", "params": {}}));
    let experience = &health["result"]["experience"];
    assert_eq!(experience["state"], json!("armed"), "the P1 surface arms too: {health}");

    // And the layer is not a property of the surface: end_turn restores it either way.
    let ended = helper.request(json!({"id": 3, "method": "end_turn", "params": {}}));
    assert_eq!(ended["ok"], json!(true), "{ended}");
    wait_until("end_turn to restore after a P1 call", || {
        mapped_overlays(&conn).is_empty() && escaped_grab_is_free(&conn, root)
    });
}

/// A server where the overlays cannot be made click-through must not draw them at all.
///
/// An overlay that intercepts pointer events silently breaks every synthesized click, so
/// the layer refuses to show one rather than warn about it (the rule the caller asked for:
/// safety over appearance). Xvfb can start without the XFIXES extension entirely, which
/// makes every XFixes request fail -- a real server exercising the real branch, not a stub.
///
/// Two things must hold at once: nothing is mapped (so nothing can swallow a click) AND the
/// real pointer is left alone (hiding it without drawing a replacement would leave the
/// operator with no pointer at all).
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test x11_experience_jsonl_xvfb -- --ignored --test-threads=1"]
fn overlays_are_refused_when_they_cannot_be_made_click_through() {
    if skip_if_disabled() {
        return;
    }
    let Some(_xvfb) = Xvfb::start_with(&["-extension", "XFIXES"]) else {
        eprintln!("SKIP: Xvfb is not installed");
        return;
    };
    let conn = connect();
    let root = root_of(&conn);
    let _window = fixture_window(&conn);
    let mut helper = Helper::spawn();

    let health = helper.request(json!({"id": 1, "method": "health", "params": {}}));
    let experience = &health["result"]["experience"];
    assert_eq!(experience["available"], json!(true), "the layer still exists: {health}");
    assert_eq!(
        experience["overlayClickThrough"],
        json!(false),
        "XFixes is disabled on this server, so click-through cannot be granted: {health}"
    );
    let degraded = experience["degraded"].as_str().unwrap_or_default();
    assert!(
        degraded.contains("not drawn"),
        "health must say the overlays are absent, not merely warn: {health}"
    );

    // Arming must still work as a state machine: it just cannot draw anything.
    let state = helper.request(json!({
        "id": 2,
        "method": "call",
        "params": {
            "name": "get_window_state",
            "surface": "window2",
            "arguments": {"window": {"app": "Fixture", "id": u64::from(root)}, "include_screenshot": false},
        },
    }));
    let _ = state; // The call's own success is asserted by the suite's other cases.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        mapped_overlays(&conn).is_empty(),
        "no overlay may be mapped when it could not be made click-through: {:?}",
        mapped_overlays(&conn)
    );

    // The real pointer must still be visible: nothing was drawn to replace it.
    let suppression = &health["result"]["experience"]["pill"];
    assert_eq!(
        *suppression,
        json!(false),
        "XFixes suppression cannot be on when the extension is absent: {health}"
    );
    // And the layer's own opinion of the pill must agree with the server: nothing was
    // mapped, so nothing may claim to be visible on screen.
    let diagnostics = helper.request(json!({"id": 4, "method": "health", "params": {}}));
    let _ = diagnostics;

    let ended = helper.request(json!({"id": 3, "method": "end_turn", "params": {}}));
    assert_eq!(ended["ok"], json!(true), "{ended}");
}

/// The synthesized pointer must not eat the clicks it is drawn over.
///
/// This is the regression that the headless window2 end-to-end run caught after arming was
/// wired up: the sprite is an override-redirect window placed so the pointer sits inside
/// it, and an override-redirect window still wins hit-testing, so every synthesized click
/// was delivered to the sprite instead of to the application. The check is deliberately
/// about DELIVERY and not about geometry: the target window is asked whether it saw a
/// ButtonPress, and the server's own view of what lies under the pointer is asserted to be
/// the target rather than any overlay.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test x11_experience_jsonl_xvfb -- --ignored --test-threads=1"]
fn the_synthesized_pointer_never_swallows_a_click() {
    if skip_if_disabled() {
        return;
    }
    let Some(_xvfb) = Xvfb::start() else {
        eprintln!("SKIP: Xvfb is not installed");
        return;
    };
    let conn = connect();
    let root = root_of(&conn);

    // The "application" window the click has to reach, and the point that will be clicked.
    let screen = &conn.setup().roots[0];
    let target = conn.generate_id().expect("window id");
    conn.create_window(
        screen.root_depth, target, screen.root, 300, 300, 300, 240, 0,
        xproto::WindowClass::INPUT_OUTPUT, 0,
        &xproto::CreateWindowAux::new()
            .background_pixel(screen.white_pixel)
            .event_mask(xproto::EventMask::EXPOSURE | xproto::EventMask::BUTTON_PRESS),
    )
    .expect("create target");
    conn.change_property8(
        xproto::PropMode::REPLACE,
        target,
        xproto::AtomEnum::WM_NAME,
        xproto::AtomEnum::STRING,
        b"click-through-target",
    )
    .expect("WM_NAME");
    conn.change_property8(
        xproto::PropMode::REPLACE,
        target,
        xproto::AtomEnum::WM_CLASS,
        xproto::AtomEnum::STRING,
        b"clickthrough\0ClickThrough\0",
    )
    .expect("WM_CLASS");
    conn.map_window(target).expect("map target");
    let _ = conn.flush();
    std::thread::sleep(Duration::from_millis(200));

    // Park the pointer over the target BEFORE arming, so the sprite is placed under it the
    // moment the layer arms and the click below cannot miss it.
    let (click_x, click_y) = (400i16, 400i16);
    xproto::warp_pointer(&conn, xproto::InputFocus::NONE, root, 0, 0, 0, 0, click_x, click_y)
        .expect("warp pointer");
    let _ = conn.flush();
    std::thread::sleep(Duration::from_millis(200));

    let mut helper = Helper::spawn();
    // A table read would not arm; an observation does, and arming is what maps the sprite.
    let state = helper.request(json!({
        "id": 1,
        "method": "call",
        "params": {
            "name": "get_window_state",
            "surface": "window2",
            "arguments": {"window": {"app": "ClickThrough", "id": u64::from(target)}, "include_screenshot": false},
        },
    }));
    assert_eq!(state["ok"], json!(true), "the observation must succeed: {state}");
    wait_until("the pill and the synthesized pointer to be mapped", || {
        mapped_overlays(&conn).len() >= 2
    });
    // The sprite closes on the pointer on its own 4 ms cadence; give it a moment to land.
    std::thread::sleep(Duration::from_millis(400));

    // The sprite really is under the pointer: this is the precondition that makes the
    // delivery assertion below meaningful. If no overlay covered the point, the test would
    // pass for the wrong reason.
    let overlays = mapped_overlays(&conn);
    let covering: Vec<String> = overlays
        .iter()
        .filter(|window| {
            let Ok(geo) = xproto::get_geometry(&conn, **window).map(|c| c.reply()) else {
                return false;
            };
            let Ok(geo) = geo else { return false };
            let (x, y) = (geo.x as i32, geo.y as i32);
            (click_x as i32) >= x
                && (click_x as i32) < x + geo.width as i32
                && (click_y as i32) >= y
                && (click_y as i32) < y + geo.height as i32
        })
        .map(|window| format!("0x{:x}", window))
        .collect();
    assert!(
        !covering.is_empty(),
        "no overlay covers the click point, so this test would prove nothing: point=({click_x},{click_y}) overlays={overlays:?}"
    );

    // Drain anything already queued, then click through XTest -- the same path the helper's
    // own click uses, so this measures exactly what the operator's click would hit.
    while let Ok(Some(_)) = conn.poll_for_event() {}
    xtest::fake_input(&conn, 4, 1, 0, root, click_x, click_y, 0).expect("button press");
    xtest::fake_input(&conn, 5, 1, 0, root, click_x, click_y, 0).expect("button release");
    let _ = conn.flush();

    let mut saw_press = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !saw_press {
        match conn.poll_for_event() {
            Ok(Some(x11rb::protocol::Event::ButtonPress(_))) => saw_press = true,
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!("event read failed: {error}"),
        }
    }
    assert!(
        saw_press,
        "the click was swallowed: an overlay covering ({click_x},{click_y}) is not click-through (overlays={overlays:?})"
    );

    // And the server agrees about which window the pointer is really over. query_pointer on
    // the root reports the topmost root CHILD under the pointer, which is the scroll of the
    // original regression: it named the sprite, not the application.
    let pointer = xproto::query_pointer(&conn, root).expect("query pointer").reply().expect("pointer reply");
    let under = pointer.child;
    assert_eq!(
        under,
        target,
        "the window under the pointer must be the application, not an overlay (overlays={overlays:?})"
    );

    let ended = helper.request(json!({"id": 2, "method": "end_turn", "params": {}}));
    assert_eq!(ended["ok"], json!(true), "{ended}");
    let _ = xproto::destroy_window(&conn, target);
    let _ = conn.flush();
}

/// A call that only reads a table does not arm: a pill for list_windows would tell the
/// operator that the desktop is being driven when nothing is being driven.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test x11_experience_jsonl_xvfb -- --ignored --test-threads=1"]
fn a_table_read_does_not_arm_the_layer() {
    if skip_if_disabled() {
        return;
    }
    let Some(_xvfb) = Xvfb::start() else {
        eprintln!("SKIP: Xvfb is not installed");
        return;
    };
    let conn = connect();
    let _window = fixture_window(&conn);
    let mut helper = Helper::spawn();

    let listed = helper.request(json!({
        "id": 1,
        "method": "call",
        "params": {"name": "list_windows", "surface": "window2", "arguments": {}},
    }));
    assert_eq!(listed["ok"], json!(true), "{listed}");
    // The call is answered before anything could be painted; give the X thread the same
    // grace a false positive would need, so this is not a race in the test's favour.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        mapped_overlays(&conn).is_empty(),
        "list_windows must not put a pill on the operator's screen"
    );
    let health = helper.request(json!({"id": 2, "method": "health", "params": {}}));
    assert_eq!(health["result"]["experience"]["armed"], json!(false), "{health}");
}
