//! End-to-end `wait_for` tests against a real X server, a real session bus and a real
//! accessibility tree.
//!
//! These are **ignored by default**, because they need Xvfb, a D-Bus session bus and a
//! GTK3 app that publishes an AT-SPI tree. Run them with:
//!
//! ```text
//! DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1
//! ```
//!
//! The fixture is a GTK3 process, not a raw X window, and that is the point: `wait_for`
//! decides on the accessibility tree, and a bare X window has none. This is the same reason
//! `xvfb_window2.rs` leaves `element_indexes_are_stable_within_one_observation` ignored --
//! its raw window cannot carry a tree. Here the tree is real, so the appear, disappear and
//! timeout paths are all exercised end to end rather than mocked.
//!
//! Each test starts its own Xvfb on a private display and its own private session bus, and
//! sets the child's environment itself, so the developer's own session is never touched.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod common;
mod support;

use serde_json::{json, Map, Value};

/// The only thing that turns these tests on.
const ENABLED: &str = "DSH_CUA_XVFB_TEST";
/// The display range this suite scans for a free server.
///
/// Deliberately *dynamic* rather than one fixed number. The other suites each own a fixed
/// display and rely on a process-local mutex, which is enough while only one test binary runs
/// at a time. This suite is often run beside the others (and beside a second copy of itself
/// during a red/green check), and with a fixed number the loser does not fail: it finds the
/// display busy, decides the tooling is unavailable, and *silently skips*. That is worse than
/// a failure, because a skipped test reports `ok` -- it once produced a fully green run in
/// which no assertion had executed. Scanning for a free display removes the collision, and
/// `fixture_or_skip` below fails loudly if the tooling is present but the fixture will not
/// come up.
const DISPLAY_RANGE_START: u32 = 130;
const DISPLAY_RANGE_END: u32 = 160;

/// Hands out a distinct display number to every fixture in this process.
///
/// Uniqueness matters more than it looks. The helper caches its X connection keyed on the
/// `$DISPLAY` *string*: if a fixture is torn down and the next one is handed the same number, the
/// helper sees an unchanged `DISPLAY`, keeps its cached connection, and talks to a server that no
/// longer exists. That is a silent hang rather than a clean failure, and it is precisely what
/// happened here -- the first gated test in the binary passed while every later one timed out
/// starting its fixture. Handing a number out once per process removes the reuse entirely.
static NEXT_DISPLAY: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(DISPLAY_RANGE_START);
/// The label text the fixture starts with, and the one it flips to.
const WAITING: &str = "WAITING";
const READY: &str = "READY";

fn enabled() -> bool {
    std::env::var(ENABLED).map(|value| value == "1").unwrap_or(false)
}

/// Xvfb displays are serialized: two servers cannot share a display number.
static DISPLAY_LOCK: Mutex<()> = Mutex::new(());

fn is_pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

fn clean_stale_lock(display_number: u32) {
    let lock_path = format!("/tmp/.X{display_number}-lock");
    let socket_path = format!("/tmp/.X11-unix/X{display_number}");
    if !std::path::Path::new(&lock_path).exists() && !std::path::Path::new(&socket_path).exists() {
        return;
    }
    let is_alive = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|content| content.trim().parse::<i32>().ok())
        .is_some_and(is_pid_alive);
    if !is_alive {
        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_file(&socket_path);
    }
}

/// A private Xvfb, killed on drop.
struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    /// Start a server on the first display in the range that is actually free.
    ///
    /// `-displayfd` would be cleaner, but it is not in every Xvfb build, so the range is
    /// probed directly: a display is taken only when nothing holds its lock or socket.
    fn start() -> Option<Self> {
        use std::sync::atomic::Ordering;
        loop {
            // Every fixture gets a number this process has not used before, so the helper's
            // display-keyed connection cache can never be handed a stale entry (see
            // `NEXT_DISPLAY`).
            let display_number = NEXT_DISPLAY.fetch_add(1, Ordering::SeqCst);
            if display_number >= DISPLAY_RANGE_END {
                return None;
            }
            if display_is_taken(display_number) {
                continue;
            }
            if let Some(server) = Self::try_start(display_number) {
                return Some(server);
            }
        }
    }

    fn try_start(display_number: u32) -> Option<Self> {
        clean_stale_lock(display_number);
        let display = format!(":{display_number}");
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
                // Lost the race for this number: the caller moves on to the next one.
                return None;
            }
            if std::path::Path::new(&socket).exists() {
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

/// Whether another X server already owns this display number.
fn display_is_taken(display_number: u32) -> bool {
    let busy = std::path::Path::new(&format!("/tmp/.X11-unix/X{display_number}")).exists()
        || std::path::Path::new(&format!("/tmp/.X{display_number}-lock")).exists();
    if !busy {
        return false;
    }
    // A stale lock from a crashed run must not burn a number forever.
    let alive = std::fs::read_to_string(format!("/tmp/.X{display_number}-lock"))
        .ok()
        .and_then(|content| content.trim().parse::<i32>().ok())
        .is_some_and(is_pid_alive);
    if !alive {
        clean_stale_lock(display_number);
        return false;
    }
    true
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

/// The whole fixture: Xvfb, a private session bus, and the GTK3 app on it.
///
/// The session bus comes from `common::PrivateSession`, which is what makes the accessibility
/// stack private. A hand-rolled bus is not enough: `at-spi-bus-launcher` takes its socket path
/// from `$XDG_RUNTIME_DIR` -- the environment of the `dbus-daemon` that activates it, not the
/// environment of this test -- so a private bus started from an inherited runtime directory
/// puts the private a11y bus on top of the desktop's own socket and unlinks it when the fixture
/// ends. See `tests/common/mod.rs`.
struct Fixture {
    _guard: std::sync::MutexGuard<'static, ()>,
    xvfb: Xvfb,
    bus: common::PrivateSession,
    app: Child,
    trigger: std::path::PathBuf,
    window_id: u64,
}

impl Fixture {
    fn start(tag: &str) -> Option<Self> {
        let guard = DISPLAY_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        // Before anything touches the environment: this is the last moment the desktop's own
        // XDG_RUNTIME_DIR can be read.
        common::pin_desktop_a11y_socket();
        let xvfb = Xvfb::start()?;
        // Started before the lock-free helper calls below, and installed in this process as
        // well, because the dispatcher reads DISPLAY and the bus address from the environment.
        let mut bus = common::PrivateSession::start()?;
        bus.install_env();

        let trigger = std::env::temp_dir()
            .join(format!("dsh-waitfor-trigger-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&trigger);

        let mut command = Command::new("python3");
        command
            .arg(fixture_script())
            .arg(&trigger)
            .env("DISPLAY", &xvfb.display)
            .env("GTK_MODULES", "gail:atk-bridge");
        // The app is a child of this process, so it would inherit the private environment
        // anyway; pointing it explicitly keeps the fixture correct even if it is ever started
        // from somewhere else.
        bus.configure_command(&mut command);
        let app = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()?;

        let mut fixture = Self {
            _guard: guard,
            xvfb,
            bus,
            app,
            trigger,
            window_id: 0,
        };
        fixture.window_id = fixture.discover_window()?;
        Some(fixture)
    }

    /// Run a closure with this fixture's environment installed.
    ///
    /// The helper reads `DISPLAY` and the bus address from the process environment and caches
    /// its connections, so setting and restoring them is exactly how a private session is
    /// simulated -- the technique `xvfb_window2.rs` uses for `DISPLAY`.
    fn with_env<T>(&self, f: impl FnOnce() -> T) -> T {
        // `XDG_RUNTIME_DIR` is part of the window: the AT-SPI client reads it to locate the
        // accessibility bus, so presenting the fixture's private value is what keeps the
        // helper on the fixture's bus rather than the desktop's.
        let previous = [
            ("DISPLAY", std::env::var("DISPLAY").ok()),
            (
                "DBUS_SESSION_BUS_ADDRESS",
                std::env::var("DBUS_SESSION_BUS_ADDRESS").ok(),
            ),
            ("XDG_RUNTIME_DIR", std::env::var("XDG_RUNTIME_DIR").ok()),
            ("XDG_CONFIG_HOME", std::env::var("XDG_CONFIG_HOME").ok()),
            ("XDG_SESSION_TYPE", std::env::var("XDG_SESSION_TYPE").ok()),
        ];
        std::env::set_var("DISPLAY", &self.xvfb.display);
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", self.bus.address());
        std::env::set_var("XDG_RUNTIME_DIR", self.bus.root());
        std::env::set_var("XDG_CONFIG_HOME", self.bus.root().join("conf"));
        std::env::set_var("XDG_SESSION_TYPE", "x11");
        let result = f();
        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        result
    }

    /// The accessibility bus address this fixture resolves to, through the helper's own
    /// discovery path rather than a reimplementation of it.
    fn a11y_bus_address(&self) -> Option<String> {
        self.with_env(|| {
            let output = Command::new(env!("CARGO_BIN_EXE_a11y-probe"))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
    }

    /// The safety gate every test in this suite ends with.
    ///
    /// Two properties, both of which this suite violated once:
    ///
    /// * the desktop's accessibility socket is still there and still accepts connections, and
    /// * the accessibility bus this fixture resolves to lives in the fixture's **private**
    ///   runtime directory, not the desktop's.
    ///
    /// The second is the direct regression test. The launcher that put its socket at
    /// /run/user/1000/at-spi/bus_0 was reachable exactly this way, and asserting on the resolved
    /// address catches it without having to catch a syscall.
    fn assert_desktop_intact(&self) {
        let mut safety = support::DesktopSafety::fatal("xvfb_waitfor");
        if let Some(socket) = safety.socket().map(std::path::Path::to_path_buf) {
            let address = self
                .a11y_bus_address()
                .expect("the helper's AT-SPI discovery must answer while the fixture is alive");
            let private = self.bus.root().to_path_buf();
            // Compare the path the address points at, not a substring of the address: a
            // leftover tree from an earlier fixture has a name that is a prefix of this
            // one's, and a substring test would accept it.
            let resolved = address
                .strip_prefix("unix:path=")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(&address));
            assert!(
                resolved.starts_with(&private),
                "the fixture resolved its accessibility bus to {address}, which is outside its \
                 private runtime directory {}; a bus outside it belongs to the desktop",
                private.display()
            );
            assert!(
                !resolved.starts_with(&socket),
                "the fixture resolved its accessibility bus to the desktop socket {}",
                socket.display()
            );
        }
        safety.observe_env();
        safety.assert_intact();
    }

    /// Find the fixture window by polling list_windows until the app publishes one.
    fn discover_window(&mut self) -> Option<u64> {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let found = self.with_env(|| {
                let listed =
                    dsh_computer_use::x11::window2::dispatch("list_windows", Map::new()).ok()?;
                let parsed = value_of(&listed);
                parsed["windows"].as_array().and_then(|windows| {
                    windows.iter().find_map(|window| {
                        let app = window["app"].as_str().unwrap_or_default();
                        // The WM_CLASS of a python3 GTK app is its script name.
                        if app.to_ascii_lowercase().contains("waitfor_gtk_app") {
                            window["id"].as_u64()
                        } else {
                            None
                        }
                    })
                })
            });
            if let Some(id) = found {
                return Some(id);
            }
            if let Ok(Some(_)) = self.app.try_wait() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }

    /// Wait until the fixture has a real accessibility tree with the starting label.
    fn wait_for_tree(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.tree_text().is_some_and(|text| text.contains(WAITING)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }

    fn tree_text(&self) -> Option<String> {
        self.with_env(|| {
            let mut arguments = Map::new();
            arguments.insert("window".to_string(), json!({ "id": self.window_id }));
            arguments.insert("include_screenshot".to_string(), json!(false));
            arguments.insert("include_text".to_string(), json!(true));
            let result =
                dsh_computer_use::x11::window2::dispatch("get_window_state", arguments).ok()?;
            value_of(&result)["accessibility"]["tree"]
                .as_str()
                .map(str::to_string)
        })
    }

}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.app.kill();
        let _ = self.app.wait();
        let _ = std::fs::remove_file(&self.trigger);
    }
}

fn fixture_script() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("waitfor_gtk_app.py")
}

/// The JSON half of a window2 tool result.
fn value_of(result: &dsh_computer_use::rmcp::model::CallToolResult) -> Value {
    let text = result
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .map(|text| text.text.clone())
        .collect::<String>();
    serde_json::from_str(&text).expect("a window2 result carries JSON text")
}

/// Call `wait_for` through the real dispatcher, the way the host would.
fn wait(fixture: &Fixture, arguments: Value) -> Value {
    let mut map = Map::new();
    map.insert(
        "window".to_string(),
        json!({ "id": fixture.window_id, "app": "waitfor_gtk_app.py" }),
    );
    for (key, value) in arguments.as_object().expect("object") {
        map.insert(key.clone(), value.clone());
    }
    let result = fixture
        .with_env(|| dsh_computer_use::x11::window2::dispatch("wait_for", map))
        .expect("wait_for must not fail on a live tree");
    value_of(&result)
}

fn skip_if_disabled() -> bool {
    if enabled() {
        return false;
    }
    if std::env::var(REQUIRE_ENV).map(|value| value == "1").unwrap_or(false) {
        panic!("{REQUIRE_ENV}=1 was set, so these tests may not skip: set {ENABLED}=1 and pass --ignored");
    }
    eprintln!("skipping: set {ENABLED}=1 (and pass --ignored) to run the Xvfb wait_for tests");
    true
}

/// Whether a skip is forbidden, so a run proves the tests executed rather than skipped.
///
/// Without this a green run is ambiguous: `fixture_or_skip` returning `None` marks the test
/// `ok`, so a missing Xvfb, a busy display or a GTK app that never published a tree all look
/// exactly like a passing test. A verification run must set this, and then any skip fails.
const REQUIRE_ENV: &str = "DSH_CUA_XVFB_REQUIRE";

fn skip(reason: &str) -> bool {
    if std::env::var(REQUIRE_ENV).map(|value| value == "1").unwrap_or(false) {
        panic!("skip is forbidden by {REQUIRE_ENV}=1, but the fixture could not start: {reason}");
    }
    eprintln!("SKIP: {reason}");
    true
}

fn fixture_or_skip(tag: &str) -> Option<Fixture> {
    let fixture = match Fixture::start(tag) {
        Some(fixture) => fixture,
        None => {
            skip("Xvfb, dbus-daemon or python3+GTK3 is unavailable");
            return None;
        }
    };
    if !fixture.wait_for_tree() {
        skip("the GTK fixture never published an accessibility tree");
        return None;
    }
    Some(fixture)
}

const NEEDS_XVFB: &str = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1";

/// Text that appears: the condition is recognised while the caller waits.
#[test]
#[ignore = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1"]
fn text_appearing_is_matched_while_the_caller_waits() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture_or_skip("appear") else {
        skip(NEEDS_XVFB);
        return;
    };

    // The label starts at WAITING, so READY is genuinely absent when the wait begins.
    assert!(
        fixture.tree_text().is_some_and(|text| text.contains(WAITING)),
        "the fixture must start at {WAITING}"
    );

    // Flip it from another thread, mid-wait: this is what makes the test prove the waiter
    // *reacts* rather than merely returning because the state was already true when it
    // started. The delay is well inside the timeout, so a correct implementation returns
    // early and one that ignores changes would report a timeout instead.
    let trigger = fixture.trigger.clone();
    let flipper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_200));
        std::fs::write(&trigger, b"go").expect("write the trigger file");
    });

    let started = Instant::now();
    let result = wait(&fixture, json!({ "text_substring": READY, "timeout_ms": 15_000 }));
    let elapsed = started.elapsed();
    flipper.join().expect("the flipper thread must not panic");

    assert_eq!(result["matched"], json!(true), "result: {result}");
    assert_eq!(result["condition"]["kind"], json!("text_substring"));
    assert_eq!(result["condition"]["value"], json!(READY));
    assert!(
        result["polls"].as_u64().unwrap_or(0) >= 1,
        "at least one poll must have run: {result}"
    );
    // It must return once the condition holds, not sit out the budget.
    assert!(
        elapsed < Duration::from_millis(12_000),
        "the wait must end when the condition holds, took {elapsed:?}"
    );
    let reported = result["elapsedMs"].as_u64().expect("elapsedMs");
    assert!(
        reported >= 1_000,
        "it cannot have matched before the flip at 1200 ms, reported {reported} ms"
    );
    assert!(
        result["nodes"].as_u64().unwrap_or(0) > 0,
        "a hit reports the tree it saw: {result}"
    );
    // A hit must name the element that satisfied it, or the model has to observe again to
    // find out what changed -- which is the round trip this tool exists to avoid.
    let hit = result["match"].as_object().expect("a hit names its element");
    assert_eq!(hit["role"], json!("label"), "result: {result}");
    assert_eq!(hit["name"], json!(READY), "result: {result}");
    assert!(hit["index"].is_u64(), "the match must carry a usable index: {result}");
    fixture.assert_desktop_intact();
}

/// An element name that appears: the second condition kind, on the same live tree.
#[test]
#[ignore = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1"]
fn an_element_name_is_matched_against_the_live_tree() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture_or_skip("element") else {
        skip(NEEDS_XVFB);
        return;
    };

    // The button exists from the start, so this also proves an already-true condition
    // answers immediately instead of waiting.
    let started = Instant::now();
    let result = wait(&fixture, json!({ "element_name": "Confirm", "timeout_ms": 15_000 }));
    assert_eq!(result["matched"], json!(true), "result: {result}");
    assert_eq!(result["condition"]["kind"], json!("element_name"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an already-present element must answer at once"
    );

    // And a name that is not in the tree must not match.
    let absent = wait(
        &fixture,
        json!({ "element_name": "NoSuchElementAnywhere", "timeout_ms": 1_000 }),
    );
    assert_eq!(absent["matched"], json!(false), "result: {absent}");
    fixture.assert_desktop_intact();
}

/// Text that disappears: the `gone` path, including its two-phase presence semantics.
#[test]
#[ignore = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1"]
fn gone_waits_for_text_to_disappear() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture_or_skip("gone") else {
        skip(NEEDS_XVFB);
        return;
    };

    // WAITING is present now, and flipping removes it, so the waiter has to observe presence
    // first and then the disappearance -- exactly the two-phase semantics.
    assert!(
        fixture.tree_text().is_some_and(|text| text.contains(WAITING)),
        "the fixture must start at {WAITING}"
    );

    let trigger = fixture.trigger.clone();
    let flipper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_200));
        std::fs::write(&trigger, b"go").expect("write the trigger file");
    });

    let result = wait(
        &fixture,
        json!({ "gone": WAITING, "timeout_ms": 15_000, "poll_ms": 100 }),
    );
    flipper.join().expect("the flipper thread must not panic");

    assert_eq!(result["matched"], json!(true), "result: {result}");
    assert_eq!(result["condition"]["kind"], json!("gone"));
    assert_eq!(
        result["observedPresent"],
        json!(true),
        "the wait saw it present and then leave: {result}"
    );
    fixture.assert_desktop_intact();
}

/// `gone` on something that was never there finishes early, and says which case it was.
#[test]
#[ignore = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1"]
fn gone_on_something_never_present_does_not_wait_out_the_budget() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture_or_skip("gone-absent") else {
        skip(NEEDS_XVFB);
        return;
    };

    let started = Instant::now();
    let result = wait(
        &fixture,
        json!({ "gone": "NeverWasHere", "timeout_ms": 15_000, "poll_ms": 100 }),
    );
    let elapsed = started.elapsed();

    assert_eq!(result["matched"], json!(true), "result: {result}");
    assert_eq!(
        result["observedPresent"],
        json!(false),
        "it must report that it never saw the text: {result}"
    );
    // The presence grace is `min(poll_ms, 1000)`, so this must return quickly rather than
    // burning the full budget on a condition that is already satisfied.
    assert!(
        elapsed < Duration::from_secs(6),
        "a never-present gone must finish early, took {elapsed:?}"
    );
    fixture.assert_desktop_intact();
}

/// Timeout: the condition never holds, and the call reports that honestly.
#[test]
#[ignore = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1"]
fn a_condition_that_never_holds_times_out_without_erroring() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture_or_skip("timeout") else {
        skip(NEEDS_XVFB);
        return;
    };

    let started = Instant::now();
    // 1500 ms with a 250 ms poll: enough for several polls, short enough to stay fast.
    let result = wait(
        &fixture,
        json!({ "text_substring": "ThisTextNeverAppears", "timeout_ms": 1_500 }),
    );
    let elapsed = started.elapsed();

    // A timeout is a fact about the UI, not a failed call.
    assert_eq!(result["ok"], json!(true), "result: {result}");
    assert_eq!(result["matched"], json!(false), "result: {result}");
    assert!(
        result["polls"].as_u64().unwrap_or(0) >= 2,
        "the wait must poll repeatedly, not once: {result}"
    );
    // It must actually use the budget it was given rather than returning early.
    assert!(
        elapsed >= Duration::from_millis(1_400),
        "the wait must use its budget, returned after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the wait must not overshoot its budget by much, took {elapsed:?}"
    );
    assert!(
        result["note"]
            .as_str()
            .is_some_and(|note| note.contains("not an error")),
        "a timeout must explain itself: {result}"
    );
    fixture.assert_desktop_intact();
}

/// The timeout is clamped to the documented 20 s ceiling, and the clamp is reported.
#[test]
#[ignore = "needs Xvfb, dbus-daemon and a GTK3 app; run with DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_waitfor -- --ignored --test-threads=1"]
fn a_timeout_above_the_ceiling_is_clamped_and_reported() {
    if skip_if_disabled() {
        return;
    }
    let Some(fixture) = fixture_or_skip("clamp") else {
        skip(NEEDS_XVFB);
        return;
    };

    // Ask for far more than the ceiling, with a condition that is already true, so the call
    // returns at once and the assertion is about the reported contract and not a 20 s wait.
    let result = wait(
        &fixture,
        json!({ "element_name": "Confirm", "timeout_ms": 600_000 }),
    );
    assert_eq!(result["matched"], json!(true), "result: {result}");
    assert_eq!(
        result["timeoutMs"],
        json!(20_000),
        "the wait must be clamped to the documented ceiling: {result}"
    );
    assert_eq!(result["timeoutClamped"], json!(true), "result: {result}");
    assert_eq!(result["maxTimeoutMs"], json!(20_000), "result: {result}");
    fixture.assert_desktop_intact();
}

/// The extension is advertised without disturbing the official thirteen.
///
/// This one needs no X server, so it always runs.
#[test]
fn the_extension_is_advertised_without_touching_the_official_thirteen() {
    use dsh_computer_use::x11::{waitfor, window2};

    // The official table stays exactly thirteen: every parity assertion depends on it.
    assert_eq!(window2::WINDOW2_TOOLS.len(), 13);
    assert!(!window2::WINDOW2_TOOLS.contains(&waitfor::WAIT_FOR_TOOL));

    // And it is a real, dispatchable method rather than a name that would be refused as
    // unknown: with no window it must fail for the *argument*, not for the name.
    let error = window2::dispatch("wait_for", Map::new())
        .err()
        .expect("wait_for without a window must be refused");
    assert!(
        error.contains("window is required"),
        "it must be refused on its arguments, not as an unknown method: {error}"
    );
    // The wording and shape must match the official thirteen exactly: a caller that sees a
    // different dialect on this one method would treat an argument mistake as a missing tool.
    assert_eq!(error, "window is required and must be a Window object from list_windows()");
    assert!(
        !error.contains("unsupported window2 method"),
        "wait_for must be a known method on this dispatcher: {error}"
    );
}
