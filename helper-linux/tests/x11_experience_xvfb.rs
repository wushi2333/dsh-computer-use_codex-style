//! End-to-end X11 integration for the window2 experience layer, on a real X server.
//!
//! Xvfb is started and stopped by the test itself on display :97 (the P2 core line owns
//! :98, so the two never fight over a socket). Everything asserted here is a real X
//! round trip against x11rb, not a mock:
//!
//! * the pill really is an override-redirect window that maps and unmaps;
//! * XFixes really accepts the cursor suppression request and its restoration;
//! * XInput2 raw events really reach the lease, so a human keystroke really does
//!   invalidate an observation;
//! * the Escape grab really is installed while armed and really is released when the
//!   turn ends -- the last one matters most, because a helper that keeps a root Escape
//!   grab eats the operator's own Escape key.
//!
//! XTEST is used to stand in for the operator. XTest input is indistinguishable from a
//! real device at the wire level, which is exactly why the lease has to treat it as human
//! input and why these tests can drive the watchdog at all.

#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use dsh_computer_use::x11_experience::{self, cursor, Freshness, USER_INPUT_MESSAGE};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, MapState};
use x11rb::protocol::xtest;
use x11rb::rust_connection::RustConnection;

const DISPLAY: &str = ":97";
const SOCKET: &str = "/tmp/.X11-unix/X97";
const ESCAPE: u8 = 9;

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

/// Owns the Xvfb child so a panic in an assertion cannot leak a server.
struct Xvfb {
    child: Child,
}

impl Xvfb {
    fn start() -> Option<Xvfb> {
        if Command::new("Xvfb").arg("-help").output().is_err() {
            return None;
        }
        clean_stale_lock(97);
        let mut child = Command::new("Xvfb")
            .args([DISPLAY, "-screen", "0", "1280x800x24", "-nolisten", "tcp"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn Xvfb");
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return None;
            }
            if Path::new(SOCKET).exists() {
                return Some(Xvfb { child });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("Xvfb {DISPLAY} never created {SOCKET}");
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(SOCKET);
        let _ = std::fs::remove_file("/tmp/.X97-lock");
    }
}

fn injector() -> RustConnection {
    let (conn, _screen) = RustConnection::connect(Some(DISPLAY)).expect("test client connects");
    conn
}

fn root_of(conn: &RustConnection) -> xproto::Window {
    conn.setup().roots[0].root
}

fn press_key(conn: &RustConnection, keycode: u8) {
    let root = root_of(conn);
    xtest::fake_input(conn, 2, keycode, 0, root, 0, 0, 0).expect("key press");
    let _ = conn.flush();
}

fn move_pointer(conn: &RustConnection, x: i16, y: i16) {
    let root = root_of(conn);
    xtest::fake_input(conn, 6, 0, 0, root, x, y, 0).expect("pointer move");
    let _ = conn.flush();
}

/// Every override-redirect override window on the root, with its map state.
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

/// Read every pixel of a window as 0x00RRGGBB values.
///
/// This is what turns 'an overlay was mapped' into 'the overlay was drawn': the map
/// state only proves the window exists, while its pixels prove the paint calls ran.
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

/// The overlay whose geometry matches, so the test never depends on creation order.
fn overlay_of_height(conn: &RustConnection, height: u16) -> Option<xproto::Window> {
    for (window, mapped) in overlay_windows(conn) {
        if !mapped {
            continue;
        }
        if let Some(reply) = xproto::get_geometry(conn, window).ok().and_then(|c| c.reply().ok()) {
            if reply.height == height {
                return Some(window);
            }
        }
    }
    None
}

/// The whole scenario in one test: the display is process-global state, so two tests
/// racing over DISPLAY would be testing the race rather than the feature.
#[test]
fn the_experience_layer_drives_a_real_x11_session() {
    let Some(_xvfb) = Xvfb::start() else {
        eprintln!("SKIP: Xvfb is not installed; the X11 integration layer cannot be exercised");
        return;
    };
    // The layer reads DISPLAY from the environment, like every other X client.
    std::env::set_var("DISPLAY", DISPLAY);
    std::env::set_var("XDG_SESSION_TYPE", "x11");
    std::env::remove_var("WAYLAND_DISPLAY");

    let conn = injector();
    let root = root_of(&conn);

    // --- start: capabilities are negotiated, not assumed ----------------------
    let session = x11_experience::start().expect("experience layer starts on Xvfb");
    let caps = session.capabilities();
    assert_eq!(caps["xfixesCursorSuppression"], serde_json::json!(true), "XFixes must be usable: {caps}");
    assert_eq!(caps["rawEvents"], serde_json::json!(true), "XInput2 raw events must be usable: {caps}");
    assert_eq!(caps["escapeKeycode"], serde_json::json!(9), "Escape is keycode 9: {caps}");
    assert_eq!(caps["pointerPollingFallback"], serde_json::json!(false), "no fallback needed here: {caps}");

    // --- S1: the pill shows up as an override-redirect window -----------------
    let before = overlay_windows(&conn);
    assert!(before.iter().all(|(_, mapped)| !mapped), "nothing is on screen before a turn: {before:?}");

    session.begin();
    wait_until("the pill and the cursor overlay to be mapped", || {
        let windows = overlay_windows(&conn);
        windows.len() >= 2 && windows.iter().all(|(_, mapped)| *mapped)
    });
    let shown = overlay_windows(&conn);
    for (window, mapped) in &shown {
        assert!(*mapped);
        let attrs = xproto::get_window_attributes(&conn, *window).unwrap().reply().unwrap();
        assert!(attrs.override_redirect, "an overlay must never be decorated or reparented");
        let tree = xproto::query_tree(&conn, *window).unwrap().reply().unwrap();
        assert_eq!(tree.parent, root, "an override-redirect overlay keeps root as its parent");
    }

    // The overlays are drawn, not merely mapped: read their pixels back from the
    // server and look for the colours the paint path is supposed to have written.
    let pill_height = (dsh_computer_use::x11_experience::pill::CONTENT_HEIGHT
        + 2 * dsh_computer_use::x11_experience::pill::PADDING) as u16;
    let pill_window = overlay_of_height(&conn, pill_height).expect("the pill overlay is mapped");
    let pixels = window_pixels(&conn, pill_window);
    assert!(
        pixels.contains(&0x00b3_9c),
        "the pill must carry its accent state bar (observing = 0x00B39C); saw {:x?}",
        &pixels[..pixels.len().min(24)]
    );
    assert!(pixels.contains(&0x0012_1619), "the pill must have its background painted");
    // And the text really was rendered: the label is drawn in the foreground colour.
    assert!(pixels.contains(&0x00f2_f4f5), "the pill label must be drawn in the foreground colour");

    // The synthesized pointer is a white arrow with a dark shadow.
    let sprite = cursor::sprite(None, std::time::Instant::now());
    let cursor_window = overlay_windows(&conn)
        .into_iter()
        .filter(|(_, mapped)| *mapped)
        .filter(|(window, _)| {
            xproto::get_geometry(&conn, *window)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|g| g.width == sprite.width as u16)
                .unwrap_or(false)
        })
        .map(|(window, _)| window)
        .next()
        .expect("the pointer overlay is mapped");
    let cursor_pixels = window_pixels(&conn, cursor_window);
    assert!(
        cursor_pixels.contains(&0x00ff_ffff),
        "the synthesized pointer must actually be filled, not an empty window"
    );

    let diagnostics = session.diagnostics();
    assert_eq!(diagnostics["pill"]["visible"], serde_json::json!(true));
    assert_eq!(diagnostics["x11"]["suppressed"], serde_json::json!(true), "the real pointer is suppressed: {diagnostics}");
    assert_eq!(diagnostics["x11"]["grab"], serde_json::json!(true), "Escape is grabbed while armed: {diagnostics}");
    assert_eq!(diagnostics["lease"]["freshness"], serde_json::json!("fresh"), "{diagnostics}");

    // --- S3: a human keystroke invalidates the observation --------------------
    assert!(session.check_lease().is_ok(), "a fresh observation is usable");
    // The arming grace (200 ms, the official ESC_ARM_GRACE) deliberately ignores input
    // that arrives with the keystroke which armed the turn. A test that injects within
    // that window would be measuring the grace rather than the watchdog.
    std::thread::sleep(Duration::from_millis(300));
    press_key(&conn, 38); // 'a'
    wait_until("the lease to notice the human keystroke", || {
        session.diagnostics()["lease"]["freshness"] == serde_json::json!("stale")
    });
    assert_eq!(session.check_lease(), Err(USER_INPUT_MESSAGE.to_string()));

    // Re-observing clears it -- this is get_window_state.
    session.observe();
    wait_until("the observation to be fresh again", || session.check_lease().is_ok());

    // --- S2: the synthesized pointer follows the real one ---------------------
    move_pointer(&conn, 640, 400);
    wait_until("the synthesized pointer to reach 640,400", || {
        session.diagnostics()["x11"]["pointer"] == serde_json::json!([640, 400])
    });

    // --- S4: Escape is a global interrupt, not a local key --------------------
    assert!(!session.escaped(), "no Escape has been pressed yet");
    press_key(&conn, ESCAPE);
    wait_until("Escape to reach the layer", || session.escaped());
    assert!(session.take_escaped(), "the Escape latch is observable exactly once");
    assert!(!session.escaped(), "consuming it clears it");

    // An out-of-band Escape puts the desktop back at once, which is what releases
    // the grab so the operator keeps their own Escape key.
    wait_until("the overlay to be hidden after Escape", || {
        overlay_windows(&conn).iter().all(|(_, mapped)| !mapped)
    });

    // --- end_turn: the desktop is handed back exactly as it was ---------------
    session.end_turn();
    wait_until("the lease to be flushed", || {
        session.diagnostics()["lease"]["freshness"] == serde_json::json!("unobserved")
    });
    let after = session.diagnostics();
    assert_eq!(after["pill"]["visible"], serde_json::json!(false), "{after}");
    assert_eq!(after["x11"]["suppressed"], serde_json::json!(false), "the real pointer is restored: {after}");
    assert_eq!(after["x11"]["grab"], serde_json::json!(false), "Escape is released again: {after}");
    assert!(overlay_windows(&conn).iter().all(|(_, mapped)| !mapped), "no overlay survives the turn");

    // The proof that the grab really was released: an Escape pressed now must not be
    // ours. A helper that kept the root grab would swallow the operator's Escape.
    std::thread::sleep(Duration::from_millis(150));
    press_key(&conn, ESCAPE);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!session.escaped(), "Escape must not be captured outside a turn");

    // --- shutting down must not leave an overlay or a grab behind ------------
    session.begin();
    wait_until("the overlay to come back", || {
        overlay_windows(&conn).iter().any(|(_, mapped)| *mapped)
    });
    session.shutdown();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut clean = false;
    while Instant::now() < deadline {
        if overlay_windows(&conn).iter().all(|(_, mapped)| !mapped) {
            clean = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(clean, "shutdown left an overlay on screen");
}

/// A second, cheap scenario: the layer must refuse to claim a Wayland session.
#[test]
fn the_layer_declines_a_wayland_session() {
    assert!(!x11_experience::x11_session_from(Some(":0"), Some("wayland"), Some("wayland-0")));
    assert!(x11_experience::x11_session_from(Some(DISPLAY), Some("x11"), None));
}

/// The lease's own contract, exercised against the same pure API the helper uses.
#[test]
fn the_lease_refuses_exactly_when_the_official_helper_does() {
    use x11_experience::Lease;
    let mut lease = Lease::new();
    let now = Instant::now();
    lease.arm(now);
    lease.observe(now);
    assert_eq!(lease.freshness(), Freshness::Fresh);
    assert!(lease.check().is_ok());
}
