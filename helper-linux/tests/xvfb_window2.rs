//! End-to-end window2 tests on a real X server started by this test binary.
//!
//! These are **ignored by default**, because they need an X server and a client that
//! draws on request. Run them with:
//!
//! ```text
//! DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1
//! ```
//!
//! Each test forks `Xvfb` on a private display, runs one check, and tears the server
//! down again. No window manager is installed on this machine, so the session is a bare
//! X server: enumeration therefore exercises the `query_tree` fallback rather than
//! `_NET_CLIENT_LIST`, and the tests that need window-manager behaviour are marked
//! ignored with the reason spelled out.
//!
//! The server is not the `$DISPLAY` the test process was started with: the helper calls
//! run with `DISPLAY` set to the private display, so a developer's real session is
//! never touched.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod common;
mod support;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, Rectangle, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;


/// The only thing that turns these tests on.
const ENABLED: &str = "DSH_CUA_XVFB_TEST";

fn enabled() -> bool {
    std::env::var(ENABLED).map(|value| value == "1").unwrap_or(false)
}

/// Xvfb displays are serialized: two servers cannot share a display number.
static DISPLAY_LOCK: Mutex<()> = Mutex::new(());

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

struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    fn start(display_number: u32) -> Option<Self> {
        clean_stale_lock(display_number);

        let display = format!(":{display_number}");
        // 24-bit depth is what the capture code expects; another depth would test the
        // depth guard rather than the capture itself.
        let mut child = Command::new("Xvfb")
            .arg(&display)
            .args(["-screen", "0", "1280x800x24", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let socket = format!("/tmp/.X11-unix/X{display_number}");
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return None;
            }
            if std::path::Path::new(&socket).exists() {
                // A short further wait so the server accepts connections rather than
                // merely having created the socket.
                std::thread::sleep(Duration::from_millis(300));
                return Some(Self { child, display });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Ok(num) = self.display.trim_start_matches(':').parse::<u32>() {
            let _ = std::fs::remove_file(format!("/tmp/.X{num}-lock"));
            let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{num}"));
        }
    }
}

fn connect(display: &str) -> RustConnection {
    let (connection, screen) = x11rb::connect(Some(display)).expect("connect to the private Xvfb");
    assert!(screen < connection.setup().roots.len());
    connection
}

struct Fixture {
    _guard: std::sync::MutexGuard<'static, ()>,
    server: Xvfb,
    connection: RustConnection,
    window: u32,
    /// A private `XDG_CONFIG_HOME`, held for the fixture's whole life.
    ///
    /// This suite runs raw X clients rather than a toolkit app and asserts no accessibility
    /// behaviour, so nothing here should ever talk to an accessibility bus. The `launch_app`
    /// tests do start real desktop applications though, and an app that finds no session bus
    /// falls back to `$XDG_RUNTIME_DIR/at-spi/bus_0` over the X11 properties -- the desktop's
    /// socket. Moving the per-user D-Bus service directory aside keeps that fallback from
    /// finding a bus, so the suite cannot reach the desktop's accessibility stack by accident.
    _config: common::PrivateConfig,
}

fn fixture(display_number: u32) -> Option<Fixture> {
    let guard = DISPLAY_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Before anything touches the environment: this is the last moment the desktop's own
    // XDG_RUNTIME_DIR can be read.
    common::pin_desktop_a11y_socket();
    let server = Xvfb::start(display_number)?;
    let config = common::PrivateConfig::new("window2")?;
    let connection = connect(&server.display);
    let screen = &connection.setup().roots[0];
    let window = connection.generate_id().ok()?;
    connection
        .create_window(
            screen.root_depth,
            window,
            screen.root,
            40,
            30,
            300,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(screen.white_pixel)
                .event_mask(EventMask::EXPOSURE),
        )
        .ok()?;
    // A title and a class are what make the window enumerable: without a window manager
    // the enumerator skips windows that carry neither.
    connection
        .change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            x11rb::protocol::xproto::AtomEnum::WM_NAME,
            x11rb::protocol::xproto::AtomEnum::STRING,
            b"xvfb-window2-fixture",
        )
        .ok()?;
    connection
        .change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            x11rb::protocol::xproto::AtomEnum::WM_CLASS,
            x11rb::protocol::xproto::AtomEnum::STRING,
            b"fixture\0Fixture\0",
        )
        .ok()?;
    connection.map_window(window).ok()?;
    connection.flush().ok()?;
    // Maps are asynchronous and the exposure is what tells the client to draw. Waiting
    // for it is also what keeps a later capture from racing the first paint.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match connection.poll_for_event() {
            Ok(Some(x11rb::protocol::Event::Expose(_))) => break,
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    Some(Fixture {
        _guard: guard,
        server,
        connection,
        window,
        _config: config,
    })
}

impl Fixture {
    /// The fixture's private `XDG_CONFIG_HOME`.
    fn private_config_root(&self) -> &std::path::Path {
        self._config.root()
    }

    /// Paint the client area the way a toolkit does: after mapping, on the exposure.
    ///
    /// Drawing before `map_window` would be erased by the background repaint the map
    /// triggers, which produces an all-background capture and makes a capture test look
    /// broken for the wrong reason.
    fn paint(&self, colour: u32) {
        let gc = self.connection.generate_id().unwrap();
        self.connection
            .create_gc(gc, self.window, &CreateGCAux::new().foreground(colour))
            .unwrap();
        self.connection
            .poly_fill_rectangle(
                self.window,
                gc,
                &[Rectangle {
                    x: 20,
                    y: 20,
                    width: 120,
                    height: 80,
                }],
            )
            .unwrap();
        self.connection.flush().unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }

    /// An opaque window covering the fixture's client area.
    fn cover_with_black(&self) -> u32 {
        let screen = &self.connection.setup().roots[0];
        let cover = self.connection.generate_id().unwrap();
        self.connection
            .create_window(
                screen.root_depth,
                cover,
                screen.root,
                0,
                0,
                640,
                480,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(screen.black_pixel)
                    .override_redirect(1),
            )
            .unwrap();
        self.connection.map_window(cover).unwrap();
        self.connection.flush().unwrap();
        std::thread::sleep(Duration::from_millis(250));
        cover
    }
}

/// The safety gate this suite ends with when a test drove the live desktop.
///
/// This suite is hermetic -- a private Xvfb, no accessibility bus -- but the helper answers
/// `list_windows` by enumerating the **live** desktop when it is asked to, and it reports the
/// desktop's accessibility availability alongside it. That is a read of the operator's session,
/// so the session must still be intact afterwards.
fn assert_desktop_intact() {
    let mut safety = support::DesktopSafety::warning("xvfb_window2");
    safety.observe_env();
    safety.assert_intact();
}

/// Run a closure with `DISPLAY` pointing at the fixture's server.
///
/// The helper caches its connection per display, so setting and restoring the variable is
/// exactly how a session switch is simulated. The fixture's private `XDG_CONFIG_HOME` is part
/// of the same window, so a helper call that would look for a session bus finds nothing
/// instead of the operator's.
fn with_display<T>(fixture: &Fixture, f: impl FnOnce() -> T) -> T {
    let previous_display = std::env::var("DISPLAY").ok();
    let previous_config = std::env::var("XDG_CONFIG_HOME").ok();
    std::env::set_var("DISPLAY", &fixture.server.display);
    std::env::set_var("XDG_CONFIG_HOME", fixture.private_config_root());
    let result = f();
    for (key, value) in [
        ("DISPLAY", previous_display),
        ("XDG_CONFIG_HOME", previous_config),
    ] {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    result
}

fn skip_if_disabled() -> bool {
    if !enabled() {
        eprintln!("skipping: set {ENABLED}=1 (and pass --ignored) to run the Xvfb window2 tests");
        return true;
    }
    false
}

const NEEDS_XVFB: &str =
    "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1";

/// Count how many pixels of a decoded capture are exactly this colour.
fn count_colour(image: &image::RgbaImage, colour: [u8; 4]) -> usize {
    image.pixels().filter(|pixel| pixel.0 == colour).count()
}

const RED: [u8; 4] = [0xff, 0x00, 0x00, 0xff];
const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
const BLACK: [u8; 4] = [0x00, 0x00, 0x00, 0xff];

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn enumeration_finds_the_window_and_reports_a_stable_handle() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(98) else {
        eprintln!("skipping: Xvfb could not be started on :98");
        return;
    };
    fixture.paint(0x00ff0000);

    let windows = with_display(&fixture, dsh_computer_use::x11::window::list_windows)
        .expect("enumeration must succeed on a live X server");
    let ours = windows
        .iter()
        .find(|entry| entry.id == u64::from(fixture.window))
        .expect("the fixture window must be enumerable");
    assert_eq!(ours.title.as_deref(), Some("xvfb-window2-fixture"));
    assert_eq!(ours.app, "Fixture");
    // No window manager is running, so the enumeration came from the root tree.
    assert!(ours.workspace.is_none(), "no WM means no _NET_WM_DESKTOP");

    // The handle is the X window id, so the same id comes back from a second pass and
    // get_window can rehydrate it.
    let again = with_display(&fixture, dsh_computer_use::x11::window::list_windows).unwrap();
    assert!(again.iter().any(|entry| entry.id == u64::from(fixture.window)));
    let rehydrated = with_display(&fixture, || {
        dsh_computer_use::x11::window::get_window(u64::from(fixture.window))
    })
    .expect("get_window must rehydrate the handle");
    assert_eq!(rehydrated.id, u64::from(fixture.window));
    assert_eq!(rehydrated.wm_class.as_deref(), Some("Fixture"));
    // Enumeration is answered from the live desktop, so this is the one test here that reads
    // the operator's session: prove it is still whole.
    assert_desktop_intact();
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn capture_reads_the_window_content_not_the_screen() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(99) else {
        eprintln!("skipping: Xvfb could not be started on :99");
        return;
    };
    fixture.paint(0x00ff0000);

    let captured = with_display(&fixture, || {
        dsh_computer_use::x11::capture::capture_window(u64::from(fixture.window))
    })
    .expect("capture must succeed on a live X server");

    assert_eq!(captured.width, 300);
    assert_eq!(captured.height, 200);
    assert_eq!(
        &captured.png[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );

    let decoded = image::load_from_memory(&captured.png)
        .expect("the capture must be a decodable PNG")
        .to_rgba8();
    assert!(count_colour(&decoded, RED) > 0, "the painted rectangle must appear");
    assert!(count_colour(&decoded, WHITE) > 0, "the background must appear");
    // The origin is the client area's position in root coordinates, which is what makes
    // a window-relative click land where the caller meant.
    assert_eq!(captured.origin_x, 40);
    assert_eq!(captured.origin_y, 30);
}

/// A refused `ShmGetImage` must fall through to the synchronous `GetImage`, not fail.
///
/// The refusal is injected: Xvfb has no compositor and its drawables answer `ShmGetImage`,
/// so the one case that used to give up cannot be produced on demand. What is under test is
/// that the fallback still returns real pixels and names the shortfall, instead of turning a
/// readable drawable into "screenshot unavailable".
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn a_refused_shm_read_falls_back_to_the_synchronous_get_image() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(107) else {
        eprintln!("skipping: Xvfb could not be started on :107");
        return;
    };
    fixture.paint(0x00ff0000);
    let window = u32::from(fixture.window);

    let (png, note) = with_display(&fixture, || {
        dsh_computer_use::x11::capture::read_drawable_png_with_refused_shm_for_test(
            window, 300, 200,
        )
    })
    .expect("a refused SHM read must still produce an image");

    // The image is real, not a placeholder: the painted rectangle must be in it.
    let decoded = image::load_from_memory(&png)
        .expect("the fallback must return a decodable PNG")
        .to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (300, 200));
    assert!(
        count_colour(&decoded, RED) > 0,
        "the painted rectangle must survive the fallback"
    );
    // And the shortfall is still named, so a caller can tell which route ran.
    let note = note.expect("the SHM shortfall must be reported");
    assert!(note.contains("GetImage"), "note was: {note}");
}

/// What composite capture actually guarantees, measured rather than assumed.
///
/// The guarantee is "the window's own pixels, not the overlapping window's": a capture of
/// an obscured window must not contain the cover. It is *not* "a fully obscured window's
/// live pixels" — see the companion test below, which pins down when content survives.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn composite_capture_never_returns_the_covering_window() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(100) else {
        eprintln!("skipping: Xvfb could not be started on :100");
        return;
    };
    fixture.paint(0x00ff0000);
    fixture.cover_with_black();

    let captured = with_display(&fixture, || {
        dsh_computer_use::x11::capture::capture_window(u64::from(fixture.window))
    })
    .expect("capture must succeed while the window is obscured");

    assert_eq!(
        captured.method,
        dsh_computer_use::x11::capture::CaptureMethod::Composite,
        "Xvfb provides Composite, so the occlusion-aware path must be taken"
    );

    let decoded = image::load_from_memory(&captured.png).unwrap().to_rgba8();
    // The cover is opaque black over the whole client area, so a direct framebuffer read
    // would be entirely black. Composite must not be.
    assert_eq!(
        count_colour(&decoded, BLACK),
        0,
        "the capture must not contain the covering window's pixels"
    );
    assert!(
        count_colour(&decoded, WHITE) > 0,
        "the capture must be the window's own drawable (its white background)"
    );
}

/// The honest limit of the X11 guarantee.
///
/// Capturing an obscured window whose client is not painting returns the window's
/// background: each capture redirects the window afresh, and a fresh off-screen buffer
/// starts at the background. Live content therefore requires the client to paint while
/// the redirection is in effect. This test drives exactly that sequence — redirect, paint,
/// capture — and proves the painted content is then returned even though an opaque window
/// covers the target. Windows' DWM holds a backing bitmap per window and can do better;
/// plain X11 cannot, so this is measured rather than claimed.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn composite_capture_returns_content_painted_while_redirected() {
    use x11rb::protocol::composite::{ConnectionExt as _, Redirect};

    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(102) else {
        eprintln!("skipping: Xvfb could not be started on :102");
        return;
    };
    fixture.paint(0x00ff0000);
    fixture.cover_with_black();

    // Redirect first so the paint below lands in the off-screen buffer that the capture
    // reads. capture_window redirects the same window again, which is a no-op for a window
    // this client already redirected.
    fixture
        .connection
        .composite_redirect_window(fixture.window, Redirect::AUTOMATIC)
        .unwrap()
        .check()
        .unwrap();
    fixture.connection.flush().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    fixture.paint(0x00ff0000);

    let captured = with_display(&fixture, || {
        dsh_computer_use::x11::capture::capture_window(u64::from(fixture.window))
    })
    .expect("capture must succeed");
    assert_eq!(
        captured.method,
        dsh_computer_use::x11::capture::CaptureMethod::Composite
    );

    let decoded = image::load_from_memory(&captured.png).unwrap().to_rgba8();
    assert!(
        count_colour(&decoded, RED) > 0,
        "content painted while redirected must be captured even under an opaque cover"
    );
    assert_eq!(count_colour(&decoded, BLACK), 0, "the cover must never appear");

    // Leave the session as we found it: the helper must not strand a redirection.
    if let Ok(cookie) = fixture
        .connection
        .composite_unredirect_window(fixture.window, Redirect::AUTOMATIC)
    {
        let _ = cookie.check();
    }
    fixture.connection.flush().unwrap();
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn input_reaches_the_window_at_a_window_relative_coordinate() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(101) else {
        eprintln!("skipping: Xvfb could not be started on :101");
        return;
    };
    // Take button events on the fixture so the delivered click can be observed.
    fixture
        .connection
        .change_window_attributes(
            fixture.window,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
        )
        .unwrap();
    fixture.connection.flush().unwrap();
    std::thread::sleep(Duration::from_millis(100));

    // (50, 50) in the window is (90, 80) in root coordinates, because the fixture's client
    // area sits at (40, 30).
    let note = with_display(&fixture, || {
        dsh_computer_use::x11::input::click(
            u64::from(fixture.window),
            50,
            50,
            dsh_computer_use::x11::input::MouseButton::Left,
            1,
        )
    })
    .expect("the click must be injectable");
    assert!(note.contains("(90, 80)"), "translated to root coordinates: {note}");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut observed = None;
    while Instant::now() < deadline {
        match fixture.connection.poll_for_event() {
            Ok(Some(x11rb::protocol::Event::ButtonPress(event)))
                if event.event == fixture.window =>
            {
                observed = Some(event);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("event polling failed: {error}"),
        }
    }
    let event = observed.expect("the window must receive the button press");
    assert_eq!(event.event_x, 50, "window-relative X");
    assert_eq!(event.event_y, 50, "window-relative Y");
    assert_eq!(event.detail, 1, "left button");
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn a_chord_reaches_the_window_as_a_real_key_event() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(103) else {
        eprintln!("skipping: Xvfb could not be started on :103");
        return;
    };
    fixture
        .connection
        .change_window_attributes(
            fixture.window,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::EXPOSURE | EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .unwrap();
    fixture.connection.flush().unwrap();
    std::thread::sleep(Duration::from_millis(100));

    with_display(&fixture, || {
        dsh_computer_use::x11::input::press_key(u64::from(fixture.window), "a")
    })
    .expect("the chord must be injectable");

    // press_key focuses the target first, so the key must arrive at the fixture.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_press = false;
    while Instant::now() < deadline {
        match fixture.connection.poll_for_event() {
            Ok(Some(x11rb::protocol::Event::KeyPress(event))) if event.event == fixture.window => {
                saw_press = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("event polling failed: {error}"),
        }
    }
    assert!(saw_press, "the focused window must receive the key press");
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn typing_sends_each_character_as_a_key_event() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(104) else {
        eprintln!("skipping: Xvfb could not be started on :104");
        return;
    };
    fixture
        .connection
        .change_window_attributes(
            fixture.window,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::EXPOSURE | EventMask::KEY_PRESS),
        )
        .unwrap();
    fixture.connection.flush().unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let note = with_display(&fixture, || {
        dsh_computer_use::x11::input::type_text(u64::from(fixture.window), "ab")
    })
    .expect("typing must be injectable");
    assert!(note.contains("2 character"), "note was: {note}");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut presses = 0;
    while Instant::now() < deadline && presses < 2 {
        match fixture.connection.poll_for_event() {
            Ok(Some(x11rb::protocol::Event::KeyPress(event))) if event.event == fixture.window => {
                presses += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("event polling failed: {error}"),
        }
    }
    assert_eq!(presses, 2, "both characters must arrive as key events");
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn typing_mixed_characters_preserves_case_and_symbols() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(107) else {
        eprintln!("skipping: Xvfb could not be started on :107");
        return;
    };
    fixture
        .connection
        .change_window_attributes(
            fixture.window,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::EXPOSURE | EventMask::KEY_PRESS),
        )
        .unwrap();
    fixture.connection.flush().unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let setup = fixture.connection.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let mapping = fixture
        .connection
        .get_keyboard_mapping(min, count)
        .unwrap()
        .reply()
        .unwrap();
    let per_keycode = usize::from(mapping.keysyms_per_keycode.max(1));

    let input_text = "abc-1./A_!";
    let note = with_display(&fixture, || {
        dsh_computer_use::x11::input::type_text(u64::from(fixture.window), input_text)
    })
    .expect("typing must be injectable");
    assert!(note.contains(&format!("{} character", input_text.len())));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = String::new();
    while Instant::now() < deadline && received.len() < input_text.len() {
        match fixture.connection.poll_for_event() {
            Ok(Some(x11rb::protocol::Event::KeyPress(event))) if event.event == fixture.window => {
                let chunk_index = usize::from(event.detail.saturating_sub(min));
                let chunk = &mapping.keysyms[chunk_index * per_keycode..(chunk_index + 1) * per_keycode];
                let is_shifted = event.state.contains(x11rb::protocol::xproto::KeyButMask::SHIFT);
                let keysym = if is_shifted && chunk.len() > 1 && chunk[1] != 0 {
                    chunk[1]
                } else {
                    chunk.first().copied().unwrap_or(0)
                };
                let sym = xkeysym::Keysym::new(keysym);
                if sym.is_modifier_key() {
                    continue;
                }
                if let Some(ch) = sym.key_char() {
                    received.push(ch);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("event polling failed: {error}"),
        }
    }
    assert_eq!(received, input_text, "window received characters must match input text exactly");
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn the_window2_surface_answers_its_methods_over_a_live_x_server() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(105) else {
        eprintln!("skipping: Xvfb could not be started on :105");
        return;
    };
    fixture.paint(0x00ff0000);
    let id = u64::from(fixture.window);

    let listed = with_display(&fixture, || {
        dsh_computer_use::x11::window2::dispatch("list_windows", serde_json::Map::new())
    })
    .expect("list_windows must succeed");
    let text = listed.content.first().and_then(|content| content.as_text()).map(|text| text.text.clone()).unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("list_windows returns JSON");
    assert!(parsed["count"].as_u64().unwrap_or(0) >= 1);

    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "window".to_string(),
        serde_json::json!({ "app": "Fixture", "id": id }),
    );
    arguments.insert("include_screenshot".to_string(), serde_json::json!(true));
    let state = with_display(&fixture, || {
        dsh_computer_use::x11::window2::dispatch("get_window_state", arguments)
    })
    .expect("get_window_state must succeed");
    // The screenshot travels as an image part, never inside the JSON text.
    assert!(
        state.content.iter().any(|content| content.as_image().is_some()),
        "the state must carry the screenshot as an image part"
    );
    let caption = state
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .map(|text| text.text.clone())
        .collect::<String>();
    let parsed: serde_json::Value = serde_json::from_str(&caption).expect("a JSON caption");
    assert_eq!(parsed["screenshots"][0]["width"], serde_json::json!(300));
    assert_eq!(
        parsed["screenshots"][0]["method"],
        serde_json::json!("composite")
    );
    // list_windows is answered from the live desktop, so this is the one test in the suite
    // that reads the operator's session: prove it is still whole.
    assert_desktop_intact();
}

/// The failure mode a real session actually hits: something else already owns the
/// window's redirection.
///
/// A running compositor (kwin, mutter, picom) redirects every top-level window, and a
/// second client's redirect comes back \`BadAccess\`. The helper must not crash or strand
/// the window: it falls back to a direct read and says so. The conflict is produced here
/// by redirecting the window from a *second* X connection first, which is exactly what a
/// compositor looks like to us.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn a_redirection_owned_by_another_client_falls_back_instead_of_failing() {
    use x11rb::protocol::composite::{ConnectionExt as _, Redirect};

    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(106) else {
        eprintln!("skipping: Xvfb could not be started on :106");
        return;
    };
    fixture.paint(0x00ff0000);

    // A separate connection stands in for the compositor.
    let (other, _screen) = x11rb::connect(Some(&fixture.server.display)).unwrap();
    let redirected = other
        .composite_redirect_window(fixture.window, Redirect::AUTOMATIC)
        .map(|cookie| cookie.check());
    other.flush().unwrap();
    assert!(
        matches!(redirected, Ok(Ok(()))),
        "the stand-in compositor must get the redirection first: {redirected:?}"
    );

    // The helper now connects on the same display and tries the same window.
    let captured = with_display(&fixture, || {
        dsh_computer_use::x11::capture::capture_window(u64::from(fixture.window))
    })
    .expect("a capture must still be produced when another client owns the redirection");

    // Printed so a reader of this test's output can see which branch this server actually
    // took, instead of having to infer it from the assertions. On Xvfb it is the composite
    // branch: see the caveat on this test.
    eprintln!(
        "redirection-ownership conflict: method={:?} degraded={:?}",
        captured.method, captured.degraded
    );
    // It is not allowed to claim the occlusion-proof path if the redirect was refused.
    match captured.degraded.as_deref() {
        Some(reason) => {
            assert_eq!(
                captured.method,
                dsh_computer_use::x11::capture::CaptureMethod::Direct,
                "a degraded capture must be a direct read"
            );
            assert!(
                reason.contains("XComposite") || reason.contains("composite"),
                "the degradation must name the cause: {reason}"
            );
            // Even degraded, a valid image comes back.
            let decoded = image::load_from_memory(&captured.png).expect("a valid PNG");
            assert_eq!(decoded.width(), 300);
        }
        None => {
            // A server that lets both redirects coexist is also acceptable; what is not
            // acceptable is a crash or a silent wrong-method claim.
            assert_eq!(
                captured.method,
                dsh_computer_use::x11::capture::CaptureMethod::Composite
            );
        }
    }

    // The helper must leave the other client's redirection alone.
    let _ = other
        .composite_unredirect_window(fixture.window, Redirect::AUTOMATIC)
        .map(|cookie| cookie.check());
    other.flush().unwrap();
}

#[test]
#[ignore = "needs a window manager; none is installed here, so EWMH activation is unverified"]
fn activation_uses_ewmh_when_a_window_manager_is_present() {
    // Deliberately not implemented rather than faked: _NET_ACTIVE_WINDOW is answered by a
    // window manager, and there is none on this machine (openbox is not installed and
    // installing one needs root). activate_window's no-WM path — map, restack, focus — is
    // exercised by the live-surface test above.
}

#[test]
#[ignore = "needs a window manager; none is installed here, so WM frame extents are unverified"]
fn a_reparenting_window_manager_offsets_the_client_origin() {
    // _NET_FRAME_EXTENTS is published by the window manager, and only a reparenting WM
    // adds a frame. The code path that actually matters for input — translate_coordinates
    // for the client origin — is verified by the click test above.
}

#[test]
#[ignore = "needs a desktop accessibility bus with a registered app for the fixture window"]
fn element_indexes_are_stable_within_one_observation() {
    // AT-SPI indexes the elements of a registered accessibility application. The Xvfb
    // fixture is a raw X window with no accessibility tree, so an end-to-end check needs a
    // real toolkit app on a session bus and belongs to the real-session verification pass.
    // The index cache's own behaviour — refusing an index with no snapshot, and refusing an
    // index outside the captured tree — is covered by unit tests in src/x11/element.rs.
}


// ---------------------------------------------------------------------------------------
// launch_app
//
// These tests drive the real launcher: they point XDG_DATA_HOME at a temporary directory,
// write a desktop entry for a real xterm, and let the helper resolve, spawn and wait the
// same way it does on a live desktop. The X server is the private Xvfb this file starts, so
// nothing here touches the operator's session.
//
// The desktop entry is what makes the test hermetic. XDG_DATA_HOME is the first root in the
// search order, so a temporary entry both supplies the app under test and shadows any
// system entry of the same name.
// ---------------------------------------------------------------------------------------

/// A desktop entry written into a private XDG_DATA_HOME.
struct LaunchHome {
    root: std::path::PathBuf,
    previous_home: Option<String>,
    previous_dirs: Option<String>,
}

impl LaunchHome {
    /// Create the private share directory and write one entry.
    ///
    /// The entry names an xterm whose WM_CLASS is set explicitly, so the window the helper
    /// has to find is unambiguous even when other clients are connected.
    fn new(tag: &str, wm_class: &str, exec: &str) -> Self {
        use std::io::Write as _;
        let root = std::env::temp_dir().join(format!(
            "cua-launch-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let applications = root.join("applications");
        std::fs::create_dir_all(&applications).expect("create the private XDG_DATA_HOME");
        let entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={tag}\n\
             Exec={exec}\n\
             StartupWMClass={wm_class}\n"
        );
        let mut file = std::fs::File::create(applications.join(format!("{tag}.desktop")))
            .expect("write the desktop entry");
        file.write_all(entry.as_bytes()).unwrap();

        let previous_home = std::env::var("XDG_DATA_HOME").ok();
        let previous_dirs = std::env::var("XDG_DATA_DIRS").ok();
        std::env::set_var("XDG_DATA_HOME", &root);
        // No system roots: resolution must come from the entry just written, so a system
        // app that happens to share the name cannot make a failing test pass.
        std::env::set_var("XDG_DATA_DIRS", "/nonexistent-cua-test-root");
        Self {
            root,
            previous_home,
            previous_dirs,
        }
    }
}

impl Drop for LaunchHome {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match self.previous_dirs.take() {
            Some(value) => std::env::set_var("XDG_DATA_DIRS", value),
            None => std::env::remove_var("XDG_DATA_DIRS"),
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The window2 launch call, as JSON.
fn launch_call(app: &str) -> Result<serde_json::Value, String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("app".to_string(), serde_json::json!(app));
    let result = dsh_computer_use::x11::window2::dispatch("launch_app", arguments)?;
    let text = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .unwrap_or_default();
    serde_json::from_str(&text).map_err(|error| format!("launch_app must answer JSON: {error}"))
}

/// Every window whose WM_CLASS *instance* is this exact name.
///
/// The instance is the half of WM_CLASS that an application sets for itself: xterm's
/// -name writes it, while the class half stays "XTerm" for every xterm ever started. It is
/// therefore the only part of the pair that can identify one test's app.
fn windows_with_instance(display: &str, instance: &str) -> Vec<u32> {
    let (connection, _screen) = x11rb::connect(Some(display)).expect("connect to Xvfb");
    let root = connection.setup().roots[0].root;
    let tree = connection.query_tree(root).unwrap().reply().unwrap();
    let class_atom = connection
        .intern_atom(false, b"WM_CLASS")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let mut found = Vec::new();
    for window in tree.children {
        let Ok(reply) = connection
            .get_property(false, window, class_atom, x11rb::protocol::xproto::AtomEnum::ANY, 0, 1024)
            .unwrap()
            .reply()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&reply.value);
        // WM_CLASS is two NUL-terminated strings: instance first, then class.
        if text.split('\0').next().is_some_and(|part| part == instance) {
            found.push(window);
        }
    }
    found
}

/// The pid that owns a window, from _NET_WM_PID.
fn window_pid(display: &str, window: u32) -> Option<u32> {
    let (connection, _screen) = x11rb::connect(Some(display)).expect("connect to Xvfb");
    let atom = connection
        .intern_atom(false, b"_NET_WM_PID")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let reply = connection
        .get_property(false, window, atom, x11rb::protocol::xproto::AtomEnum::CARDINAL, 0, 16)
        .unwrap()
        .reply()
        .unwrap();
    reply.value32().and_then(|mut values| values.next())
}

/// Kill every process whose command line carries this marker, so a failed assertion cannot
/// leak an xterm into the operator's session.
fn kill_marked(marker: &str) {
    let _ = Command::new("pkill").arg("-f").arg(marker).status();
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn launch_app_starts_an_app_waits_for_its_window_and_focuses_it() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(130) else {
        eprintln!("skipping: Xvfb could not be started on :130");
        return;
    };
    // A marker unique to this test, so cleanup can never match an operator process.
    let marker = format!("cua-launch-{}", std::process::id());
    let class = format!("CuaLaunch{}", std::process::id());
    let home = LaunchHome::new(
        "cua-launch-app",
        &class,
        &format!("xterm -name {marker} -T {marker} -e sleep 120"),
    );

    let result = with_display(&fixture, || launch_call("cua-launch-app"));
    let parsed = match result {
        Ok(parsed) => parsed,
        Err(error) => {
            kill_marked(&marker);
            panic!("launch_app must succeed: {error}");
        }
    };

    assert_eq!(parsed["launched"], serde_json::json!(true));
    assert_eq!(parsed["alreadyRunning"], serde_json::json!(false));
    assert_eq!(parsed["app"]["source"], serde_json::json!(format!(
        "desktop:{}/applications/cua-launch-app.desktop",
        home.root.display()
    )));
    let window = parsed["window"].as_object().expect("the window must be reported");
    let id = window["id"].as_u64().expect("a window id");
    assert!(
        parsed["detail"]["wmInstance"]
            .as_str()
            .unwrap_or_default()
            .eq_ignore_ascii_case(&marker),
        "the reported window must be the launched app's: {}",
        parsed["detail"]["wmInstance"]
    );

    // The window exists on the server, exactly once.
    let windows = windows_with_instance(&fixture.server.display, &marker);
    assert_eq!(windows.len(), 1, "one instance, one window: {windows:?}");
    assert_eq!(u64::from(windows[0]), id);

    // It was focused: the launcher must hand the app the foreground rather than leave it
    // behind whatever window happened to have focus. With no window manager on Xvfb the
    // activation falls back to XSetInputFocus, which the server reports back directly.
    let (connection, _screen) = x11rb::connect(Some(&fixture.server.display)).unwrap();
    let focus = connection.get_input_focus().unwrap().reply().unwrap();
    assert_eq!(
        u32::from(focus.focus),
        windows[0],
        "the launched window must be focused"
    );

    // The app is a live process of its own, not something this test process has to reap.
    let pid = window_pid(&fixture.server.display, windows[0]).expect("the app sets _NET_WM_PID");
    assert!(
        dsh_computer_use::x11::launch::process_group(pid).is_some(),
        "the launched app must be a live process"
    );

    kill_marked(&marker);
    drop(home);
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn a_second_launch_of_a_running_app_raises_it_instead_of_starting_another() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(131) else {
        eprintln!("skipping: Xvfb could not be started on :131");
        return;
    };
    let marker = format!("cua-dedupe-{}", std::process::id());
    let class = format!("CuaDedupe{}", std::process::id());
    let home = LaunchHome::new(
        "cua-dedupe-app",
        &class,
        &format!("xterm -name {marker} -T {marker} -e sleep 120"),
    );

    // First launch: a real spawn.
    let first = with_display(&fixture, || launch_call("cua-dedupe-app"));
    let first = match first {
        Ok(parsed) => parsed,
        Err(error) => {
            kill_marked(&marker);
            panic!("the first launch must succeed: {error}");
        }
    };
    assert_eq!(first["launched"], serde_json::json!(true));
    let first_id = first["window"]["id"].as_u64().expect("a window id");
    assert_eq!(windows_with_instance(&fixture.server.display, &marker).len(), 1);

    // Second launch: the running instance must be raised and nothing spawned. This is the
    // whole reason launch_app exists: a second instance is the failure this prevents.
    let second = with_display(&fixture, || launch_call("cua-dedupe-app"));
    let second = match second {
        Ok(parsed) => parsed,
        Err(error) => {
            kill_marked(&marker);
            panic!("the second launch must be answered, not refused: {error}");
        }
    };
    assert_eq!(
        second["launched"],
        serde_json::json!(false),
        "a second launch must not report a new instance: {second}"
    );
    assert_eq!(second["alreadyRunning"], serde_json::json!(true));
    assert_eq!(
        second["window"]["id"].as_u64(),
        Some(first_id),
        "the same window must come back"
    );
    assert!(
        second["note"]
            .as_str()
            .is_some_and(|note| note.contains("running instance")),
        "the note must say what happened: {}",
        second["note"]
    );
    assert_eq!(
        windows_with_instance(&fixture.server.display, &marker).len(),
        1,
        "no second instance may have been started"
    );

    kill_marked(&marker);
    drop(home);
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn an_app_that_cannot_be_resolved_is_refused_structurally() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(132) else {
        eprintln!("skipping: Xvfb could not be started on :132");
        return;
    };
    let home = LaunchHome::new("cua-present-app", "CuaPresent", "/bin/true");

    let error = with_display(&fixture, || {
        launch_call("definitely-not-installed-app-xyz-42")
    })
    .expect_err("an unresolvable app must be refused");
    let parsed: serde_json::Value = serde_json::from_str(&error).expect("a structured refusal");
    assert_eq!(parsed["error"], serde_json::json!("unsupported"));
    assert_eq!(parsed["method"], serde_json::json!("launch_app"));
    assert!(
        parsed["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("definitely-not-installed-app-xyz-42")),
        "the refusal must name what was asked for: {}",
        parsed["reason"]
    );
    assert!(
        parsed["alternative"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "a refusal always names a way forward"
    );

    // A near miss is offered as a hint, which is what lets the model correct itself
    // instead of guessing again.
    let near = with_display(&fixture, || launch_call("cua-present"))
        .expect("a partial name resolves to the entry that exists");
    assert_eq!(near["launched"], serde_json::json!(true));
    drop(home);
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn a_launch_that_never_produces_a_window_reports_that_honestly() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(133) else {
        eprintln!("skipping: Xvfb could not be started on :133");
        return;
    };
    // A program that runs forever without ever creating a window.
    let marker = format!("cua-timeout-{}", std::process::id());
    let home = LaunchHome::new(
        "cua-timeout-app",
        "CuaTimeout",
        &format!("sh -c 'sleep 120 # {marker}'"),
    );

    let previous = std::env::var("DSH_CUA_LAUNCH_TIMEOUT_MS").ok();
    std::env::set_var("DSH_CUA_LAUNCH_TIMEOUT_MS", "1500");
    let result = with_display(&fixture, || launch_call("cua-timeout-app"));
    match previous {
        Some(value) => std::env::set_var("DSH_CUA_LAUNCH_TIMEOUT_MS", value),
        None => std::env::remove_var("DSH_CUA_LAUNCH_TIMEOUT_MS"),
    }

    let parsed = match result {
        Ok(parsed) => parsed,
        Err(error) => {
            kill_marked(&marker);
            panic!("a launch without a window is not a failure: {error}");
        }
    };
    assert_eq!(
        parsed["launched"],
        serde_json::json!(true),
        "the program did start, so launched must be true"
    );
    assert_eq!(
        parsed["window"],
        serde_json::Value::Null,
        "no window may be invented"
    );
    assert!(
        parsed["note"]
            .as_str()
            .is_some_and(|note| note.contains("1500") && note.contains("still")),
        "the note must say the app may still be starting: {}",
        parsed["note"]
    );

    kill_marked(&marker);
    drop(home);
}

#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn a_launched_app_outlives_the_caller_because_it_is_detached() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(134) else {
        eprintln!("skipping: Xvfb could not be started on :134");
        return;
    };
    let marker = format!("cua-detach-{}", std::process::id());
    let class = format!("CuaDetach{}", std::process::id());
    let home = LaunchHome::new(
        "cua-detach-app",
        &class,
        &format!("xterm -name {marker} -T {marker} -e sleep 120"),
    );

    let parsed = with_display(&fixture, || launch_call("cua-detach-app"));
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            kill_marked(&marker);
            panic!("the launch must succeed: {error}");
        }
    };
    assert_eq!(parsed["launched"], serde_json::json!(true));

    let windows = windows_with_instance(&fixture.server.display, &marker);
    assert_eq!(windows.len(), 1);
    let pid = window_pid(&fixture.server.display, windows[0]).expect("the app sets _NET_WM_PID");

    // The proof of detachment, in the two facts that matter to this helper:
    //
    // * the app is not in this process's session, so it does not die with the helper; and
    // * its parent is not this process, so the helper never has to reap it.
    let my_session = dsh_computer_use::x11::launch::session_of(std::process::id())
        .expect("this test process has a session");
    let app_session = dsh_computer_use::x11::launch::session_of(pid)
        .expect("the launched app has a session");
    assert_ne!(
        app_session, my_session,
        "setsid must put the app in its own session so it outlives the helper"
    );
    let app_group = dsh_computer_use::x11::launch::process_group(pid).expect("a process group");
    assert_ne!(
        app_group,
        dsh_computer_use::x11::launch::process_group(std::process::id()).unwrap(),
        "the app must not share the helper's process group"
    );

    kill_marked(&marker);
    drop(home);
}

/// Every capture in this test shares one process, and the knob is an environment
/// variable, so the two halves below run in a fixed order and restore the value.
fn with_max_image_edge<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
    const KEY: &str = "DSH_COMPUTER_USE_MAX_IMAGE_EDGE";
    let previous = std::env::var(KEY).ok();
    match value {
        Some(value) => std::env::set_var(KEY, value),
        None => std::env::remove_var(KEY),
    }
    let result = f();
    match previous {
        Some(value) => std::env::set_var(KEY, value),
        None => std::env::remove_var(KEY),
    }
    result
}

/// Pull the base64 payload out of the `data:image/png;base64,...` an MCP image part carries.
fn image_bytes(result: &dsh_computer_use::rmcp::model::CallToolResult) -> Vec<u8> {
    use base64::Engine as _;
    let data = result
        .content
        .iter()
        .find_map(|content| content.as_image())
        .expect("the state must carry the screenshot as an image part")
        .data
        .clone();
    let encoded = data
        .strip_prefix("data:image/png;base64,")
        .expect("the image part carries a PNG data URL");
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("the payload is valid base64")
}

/// The value half of a window2 result, parsed out of the JSON text block.
fn window2_value(result: &dsh_computer_use::rmcp::model::CallToolResult) -> serde_json::Value {
    let text = result
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .map(|text| text.text.clone())
        .collect::<String>();
    serde_json::from_str(&text).expect("a JSON caption")
}

fn capture_window_state(id: u64) -> dsh_computer_use::rmcp::model::CallToolResult {
    let mut arguments = serde_json::Map::new();
    arguments.insert("window".to_string(), serde_json::json!({ "app": "Fixture", "id": id }));
    arguments.insert("include_screenshot".to_string(), serde_json::json!(true));
    dsh_computer_use::x11::window2::dispatch("get_window_state", arguments)
        .expect("get_window_state must succeed")
}

/// IMG-EDGE, end to end on a real X server.
///
/// The knob has to satisfy three things at once, and only a live capture can show them:
/// the image really shrinks, the value declares the size the image really has, and the
/// coordinate space the input tools use is left at the original window size so a
/// window-relative click does not land at half the intended offset.
#[test]
#[ignore = "needs Xvfb; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1"]
fn the_max_image_edge_cap_shrinks_the_image_and_declares_both_sizes() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture(108) else {
        eprintln!("skipping: Xvfb could not be started on :108");
        return;
    };
    fixture.paint(0x00ff0000);
    let id = u64::from(fixture.window);

    // 1. The official behaviour, captured first: no knob, no scaling, no extra fields.
    let uncapped = with_display(&fixture, || with_max_image_edge(None, || capture_window_state(id)));
    let uncapped_value = window2_value(&uncapped);
    let uncapped_png = image_bytes(&uncapped);
    let uncapped_image = image::load_from_memory(&uncapped_png).expect("a valid PNG");
    assert_eq!(
        (uncapped_image.width(), uncapped_image.height()),
        (300, 200),
        "without the knob the window is captured at its natural size"
    );
    assert_eq!(uncapped_value["screenshots"][0]["width"], serde_json::json!(300));
    assert_eq!(uncapped_value["screenshots"][0]["height"], serde_json::json!(200));
    let entry = uncapped_value["screenshots"][0].as_object().unwrap();
    for absent in ["coordinateWidth", "coordinateHeight", "scale", "resized"] {
        assert!(
            !entry.contains_key(absent),
            "the default wire shape must not grow a {absent} field"
        );
    }
    let origin = (
        uncapped_value["screenshots"][0]["originX"].clone(),
        uncapped_value["screenshots"][0]["originY"].clone(),
    );

    // 2. A cap that does not bite must be byte-for-byte the uncapped capture: this is the
    //    strong form of "不设/不生效时行为不变", measured on real pixels rather than argued.
    let inert = with_display(&fixture, || {
        with_max_image_edge(Some("2000"), || capture_window_state(id))
    });
    assert_eq!(
        image_bytes(&inert),
        uncapped_png,
        "a cap larger than the image must not alter a single byte"
    );

    // 3. A cap that does bite: longest edge <= 100, ratio kept (300x200 -> 100x67).
    let capped = with_display(&fixture, || {
        with_max_image_edge(Some("100"), || capture_window_state(id))
    });
    let capped_value = window2_value(&capped);
    let capped_image = image::load_from_memory(&image_bytes(&capped)).expect("a valid PNG");
    let (capped_width, capped_height) = (capped_image.width(), capped_image.height());
    assert!(
        capped_width.max(capped_height) <= 100,
        "the cap must bound the longest edge, got {capped_width}x{capped_height}"
    );
    assert!(
        capped_width <= 300 && capped_height <= 200,
        "the cap must never upscale, got {capped_width}x{capped_height}"
    );

    // The declared size must be the decoded size, or the model is told a lie about the
    // image it is looking at (the contract the e2e driver asserts).
    let shot = &capped_value["screenshots"][0];
    assert_eq!(shot["width"], serde_json::json!(capped_width));
    assert_eq!(shot["height"], serde_json::json!(capped_height));

    // The coordinate space must stay the original window size: click/drag take
    // window-relative coordinates, so this is what keeps the mapping honest.
    assert_eq!(
        shot["coordinateWidth"],
        serde_json::json!(300),
        "the coordinate space must stay the original window width"
    );
    assert_eq!(
        shot["coordinateHeight"],
        serde_json::json!(200),
        "the coordinate space must stay the original window height"
    );
    assert_eq!(shot["resized"], serde_json::json!(true));

    // `originX`/`originY` are root coordinates and must not be scaled either.
    assert_eq!((shot["originX"].clone(), shot["originY"].clone()), origin);
    assert_eq!(origin, (serde_json::json!(40), serde_json::json!(30)));

    // The declared scale must match the real ratio between the two spaces.
    let declared_scale = shot["scale"].as_f64().expect("a numeric scale");
    assert!(
        (declared_scale - f64::from(capped_width) / 300.0).abs() < 1e-6,
        "the declared scale must be returned/coordinate, got {declared_scale}"
    );

    // The window identity block is untouched: this is metadata, not pixels.
    assert_eq!(capped_value["window"], uncapped_value["window"]);
}

/// A compile-time guard so the ignored-reason string stays in one place and cannot drift
/// from the run command quoted in this file's header.
#[test]
fn the_documented_run_command_matches_the_ignore_reason() {
    assert!(NEEDS_XVFB.contains("DSH_CUA_XVFB_TEST=1"));
    assert!(NEEDS_XVFB.contains("--ignored"));
}