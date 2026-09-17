//! Real-session launch checks, against the operator's own X server.
//!
//! These are ignored by default and gated a second time by an environment variable,
//! because they act on the live desktop rather than on a private Xvfb: they may raise a
//! window and take the focus for a moment. Run them with:
//!
//!     DSH_CUA_REAL_LAUNCH=1 cargo test --test real_launch -- --ignored --test-threads=1 --nocapture
//!
//! Everything here cleans up after itself: the only app it starts is an xterm it then
//! kills, and the dedup check raises the already-running app without spawning anything.

use dsh_computer_use::x11::launch;
use dsh_computer_use::x11::window;

/// The variable that turns these on, separate from the Xvfb gate: a machine can have an
/// X server without being a machine an operator wants a popup on.
const ENABLED: &str = "DSH_CUA_REAL_LAUNCH";

fn enabled() -> bool {
    std::env::var(ENABLED).map(|value| value == "1").unwrap_or(false)
}

fn skip_if_disabled() -> bool {
    if !enabled() {
        eprintln!("skipping: set {ENABLED}=1 (and pass --ignored) to run the real-session launch checks");
        return true;
    }
    false
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

/// Every window whose WM_CLASS instance or class is this name, case-insensitively.
fn windows_named(name: &str) -> Vec<window::X11Window> {
    window::list_windows()
        .unwrap_or_default()
        .into_iter()
        .filter(|candidate| {
            [candidate.wm_class.as_deref(), candidate.wm_instance.as_deref()]
                .into_iter()
                .flatten()
                .any(|value| launch::normalize(value) == launch::normalize(name))
        })
        .collect()
}

/// Every process running this exact executable path.
fn processes_running(executable: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if std::fs::read_link(entry.path().join("exe"))
            .is_ok_and(|target| target == std::path::Path::new(executable))
        {
            found.push(pid);
        }
    }
    found.sort_unstable();
    found
}

/// Kills a process when the test ends, however it ends.
///
/// A bare cleanup call at the end of the body is not enough: a failing assertion unwinds
/// past it, and the app this test started would be left running on the operator's desktop.
/// Tying the kill to a guard makes the cleanup unconditional.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        kill_and_wait(self.0);
    }
}

/// Kill a process and wait for it to disappear, so a later check cannot see a corpse.
fn kill_and_wait(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_LAUNCH=1 cargo test --test real_launch -- --ignored --test-threads=1"]
fn launching_xterm_on_the_real_session_produces_a_window_and_a_detached_process() {
    if skip_if_disabled() {
        return;
    }
    let before = processes_running("/usr/bin/xterm");

    let started = std::time::Instant::now();
    let parsed = launch_call("xterm").expect("launch_app must start xterm");
    let elapsed = started.elapsed();

    assert_eq!(parsed["launched"], serde_json::json!(true), "{parsed}");
    assert_eq!(parsed["alreadyRunning"], serde_json::json!(false));
    // xterm resolves either way depending on the distribution: Debian ships a
    // debian-xterm.desktop entry, while a bare install has only the binary on PATH. Both
    // are correct resolutions, so the assertion is that one of them happened rather than
    // that a particular machine's packaging won.
    let source = parsed["app"]["source"].as_str().unwrap_or_default();
    assert!(
        source == "path" || source.starts_with("desktop:"),
        "xterm must resolve through a desktop entry or PATH, not {source}"
    );
    let window_id = parsed["window"]["id"]
        .as_u64()
        .expect("the launched xterm must report its window");
    let pid = parsed["detail"]["pid"].as_u64().expect("xterm sets _NET_WM_PID") as u32;
    // From here on the test owns a live xterm, so the kill is armed before the first
    // assertion that could fail.
    let _cleanup = KillOnDrop(pid);
    eprintln!(
        "launch_app(xterm) -> launched={} window=0x{window_id:x} pid={pid} in {elapsed:?}",
        parsed["launched"]
    );

    // The process is real, new, and belongs to this launch.
    let after = processes_running("/usr/bin/xterm");
    let newest: Vec<u32> = after.iter().copied().filter(|pid| !before.contains(pid)).collect();
    assert_eq!(
        newest,
        vec![pid],
        "exactly one new xterm must have been started: before={before:?} after={after:?}"
    );

    // It is detached: a different session from this test process, which is what keeps the
    // helper from having to reap an app that outlives it.
    let my_session = launch::session_of(std::process::id()).expect("this process has a session");
    let app_session = launch::session_of(pid).expect("xterm has a session");
    assert_ne!(app_session, my_session, "setsid must detach the launched app");
    eprintln!("detached: app session={app_session} vs test session={my_session}");

    // The window is enumerable and addressable through the window2 surface.
    let listed = windows_named("XTerm");
    assert!(
        listed.iter().any(|candidate| candidate.id == window_id),
        "the launched window must be listed: {listed:?}"
    );

    // Clean up: the xterm this test started must not outlive it. The guard already owns
    // the kill, so dropping it here proves the cleanup path before the final assertions.
    drop(_cleanup);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !windows_named("XTerm").iter().any(|candidate| candidate.id == window_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        !windows_named("XTerm").iter().any(|candidate| candidate.id == window_id),
        "the test's own xterm must be gone after cleanup"
    );
    assert_eq!(
        processes_running("/usr/bin/xterm")
            .iter()
            .filter(|pid| !before.contains(pid))
            .count(),
        0,
        "no xterm started by this test may survive"
    );
}

#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_LAUNCH=1 cargo test --test real_launch -- --ignored --test-threads=1"]
fn launching_a_running_app_raises_it_without_starting_a_second_instance() {
    if skip_if_disabled() {
        return;
    }
    // The QQ desktop entry carries Name=QQ, Exec=/opt/QQ/qq %U and StartupWMClass=QQ. This
    // is the case the whole feature exists for: the operator already has QQ open, and the
    // agent must not start a second one behind a second login prompt.
    let existing = windows_named("QQ");
    if existing.is_empty() {
        eprintln!("skipping: QQ is not running on this session, so there is nothing to deduplicate");
        return;
    }
    let before_pids = processes_running("/opt/QQ/qq");
    let before_windows: Vec<u64> = existing.iter().map(|window| window.id).collect();
    eprintln!("QQ before: windows={before_windows:?} pids={before_pids:?}");

    let parsed = launch_call("qq").expect("launch_app(qq) must be answered, not refused");

    assert_eq!(
        parsed["launched"],
        serde_json::json!(false),
        "an already running app must not be launched again: {parsed}"
    );
    assert_eq!(parsed["alreadyRunning"], serde_json::json!(true), "{parsed}");
    let raised = parsed["window"]["id"]
        .as_u64()
        .expect("the running instance's window must come back");
    assert!(
        before_windows.contains(&raised),
        "the window must be one that already existed: raised=0x{raised:x} before={before_windows:?}"
    );
    assert!(
        parsed["note"]
            .as_str()
            .is_some_and(|note| note.contains("running instance")),
        "the note must say the instance was raised: {}",
        parsed["note"]
    );

    // The decisive assertion: nothing new was started.
    let after_pids = processes_running("/opt/QQ/qq");
    assert_eq!(
        after_pids, before_pids,
        "no second QQ may have been started: before={before_pids:?} after={after_pids:?}"
    );
    let after_windows: Vec<u64> = windows_named("QQ").iter().map(|window| window.id).collect();
    assert_eq!(
        after_windows, before_windows,
        "no new QQ window may have appeared"
    );
    eprintln!(
        "launch_app(qq) -> alreadyRunning=true raised=0x{raised:x}; QQ processes unchanged ({} of them)",
        before_pids.len()
    );
}

#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_LAUNCH=1 cargo test --test real_launch -- --ignored --test-threads=1"]
fn an_app_that_does_not_exist_is_refused_on_the_real_session_too() {
    if skip_if_disabled() {
        return;
    }
    let error = launch_call("definitely-not-installed-app-xyz-42")
        .expect_err("an unresolvable app must be refused");
    let parsed: serde_json::Value = serde_json::from_str(&error).expect("a structured refusal");
    assert_eq!(parsed["error"], serde_json::json!("unsupported"));
    assert_eq!(parsed["method"], serde_json::json!("launch_app"));
    assert!(parsed["alternative"].as_str().is_some_and(|text| !text.is_empty()));
    eprintln!("refusal: {}", parsed["reason"]);

    // The refusal must be a real check, not a blanket one: a name that resolves still
    // resolves on this session.
    let resolved = launch::resolve("qq").expect("qq resolves from its desktop entry");
    assert_eq!(resolved.program, std::path::PathBuf::from("/opt/QQ/qq"));
}
