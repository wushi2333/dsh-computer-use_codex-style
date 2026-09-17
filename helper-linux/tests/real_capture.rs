//! Real-session capture checks, against the operator's own X server.
//!
//! These are ignored by default and gated a second time by an environment variable, because
//! they read the live desktop rather than a private Xvfb. Run them with:
//!
//!     DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1 --nocapture
//!
//! They are **read-only with respect to the desktop**: nothing here raises, moves, maps or
//! focuses a window, so the operator's session is left exactly as it was found. That is also
//! why the hidden case asserts on the *contract* (an actionable refusal, both screenshot
//! channels empty) instead of activating the window to force a screenshot -- the decision to
//! change the operator's desktop belongs to the model calling activate_window, not to a
//! regression test.
//!
//! The bug these exist for was invisible to the headless suite: on a real session every
//! *visible* window returned an empty screenshots array beside an orphaned PNG, and every
//! *hidden* window returned a bare "screenshot unavailable". Xvfb could not show either,
//! because it has no window manager to hide a window with and no compositor to own a
//! redirection.

use dsh_computer_use::x11::window;

/// The variable that turns these on, separate from the Xvfb gate: a machine can have an X
/// server without being a machine whose live desktop a test should touch.
const ENABLED: &str = "DSH_CUA_REAL_CAPTURE";

fn enabled() -> bool {
    std::env::var(ENABLED).map(|value| value == "1").unwrap_or(false)
}

fn skip_if_disabled() -> bool {
    if !enabled() {
        eprintln!(
            "skipping: set {ENABLED}=1 (and pass --ignored) to run the real-session capture checks"
        );
        return true;
    }
    false
}

/// One window2 call, answered as the pair of channels the wire actually carries.
struct CallResult {
    value: serde_json::Value,
    images: usize,
}

fn call(
    name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> CallResult {
    let result = dsh_computer_use::x11::window2::dispatch(name, arguments)
        .unwrap_or_else(|error| panic!("{name} must not fail outright: {error}"));
    let images = result
        .content
        .iter()
        .filter(|content| content.as_image().is_some())
        .count();
    let text = result
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .map(|text| text.text.clone())
        .collect::<String>();
    let value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{name} must answer one JSON object: {error}; got {text}"));
    CallResult { value, images }
}

fn get_window_state(id: u64) -> CallResult {
    let mut arguments = serde_json::Map::new();
    arguments.insert("window".to_string(), serde_json::json!({ "app": "", "id": id }));
    arguments.insert("include_screenshot".to_string(), serde_json::json!(true));
    call("get_window_state", arguments)
}

/// The strongest statement this suite can make about any window: the two channels agree.
///
/// An image without an entry is a screenshot the model can see but cannot name or click
/// with; an entry without an image is a coordinate space with no pixels behind it. Either
/// one is a lie about what was captured, so both are refused here.
fn assert_channels_agree(result: &CallResult, context: &str) {
    let entries = result
        .value
        .get("screenshots")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        entries, result.images,
        "{context}: screenshots[] has {entries} entries but {} images were attached; the two \
         channels describe one capture and must never disagree: {}",
        result.images, result.value
    );
}

/// Every window the session reports, so the test can classify rather than guess.
fn all_windows() -> Vec<window::X11Window> {
    window::list_windows().unwrap_or_default()
}

/// A visible capture must carry exactly one entry and exactly one image.
///
/// This is the case that was broken on a real session while the headless suite was green:
/// the state object was nested under a second value key, so the entry the JSON did contain
/// was invisible to every consumer that reads result.value.screenshots.
#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1"]
fn a_visible_window_captures_into_one_entry_and_one_image() {
    if skip_if_disabled() {
        return;
    }
    let visible: Vec<_> = all_windows()
        .into_iter()
        .filter(|candidate| !candidate.hidden)
        .collect();
    if visible.is_empty() {
        eprintln!("skipping: the session reports no visible window to capture");
        return;
    }
    for target in visible {
        let result = get_window_state(target.id);
        assert_channels_agree(
            &result,
            &format!("visible window {} ({})", target.id, target.app),
        );
        let entries = result.value["screenshots"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "a visible window must produce exactly one entry: {}",
            result.value
        );
        let entry = &entries[0];
        assert_eq!(entry["id"], serde_json::json!(format!("0x{:x}:0", target.id)));
        assert!(
            entry["width"].as_u64().unwrap_or(0) > 0,
            "the entry must declare a real size: {entry}"
        );
        assert!(
            entry["height"].as_u64().unwrap_or(0) > 0,
            "the entry must declare a real size: {entry}"
        );
        // The caption must be reachable at the top level, which is the whole point: a
        // consumer reads result.value.screenshots, never result.value.value.
        assert!(
            result.value.get("value").is_none(),
            "the state must not be nested under a second value key: {}",
            result.value
        );
    }
}

/// A hidden or minimized window must answer with a structured refusal, never a stub image.
///
/// The contract has to hold whether or not the server can still produce pixels for the
/// window (a compositor holding its redirection can): what may never happen is an image the
/// model would mistake for a current screenshot, or a bare "unavailable" with no next step.
#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1"]
fn a_hidden_window_either_captures_cleanly_or_refuses_with_a_next_step() {
    if skip_if_disabled() {
        return;
    }
    let hidden: Vec<_> = all_windows()
        .into_iter()
        .filter(|candidate| candidate.hidden)
        .collect();
    if hidden.is_empty() {
        eprintln!("skipping: the session reports no hidden window to capture");
        return;
    }
    for target in hidden {
        let result = get_window_state(target.id);
        let context = format!("hidden window {} ({})", target.id, target.app);
        assert_channels_agree(&result, &context);

        let entries = result.value["screenshots"].as_array().unwrap().len();
        if entries == 1 {
            // Captured after all: the entry must be complete and say how it was obtained.
            let entry = &result.value["screenshots"][0];
            assert!(
                entry["method"].as_str().is_some(),
                "{context}: a successful capture must name its method: {entry}"
            );
            continue;
        }

        assert_eq!(entries, 0, "{context}");
        assert_eq!(result.images, 0, "{context}: a refusal must attach no image");
        // A refusal the model can act on, not a dead end. A hidden window that still cannot
        // be captured must say so in a way the model can act on.
        let refusal = result.value.get("screenshotError").unwrap_or_else(|| {
            panic!(
                "{context}: an empty capture must explain itself: {}",
                result.value
            )
        });
        assert_eq!(refusal["error"], serde_json::json!("screenshot-unavailable"));
        assert!(
            refusal["alternative"]
                .as_str()
                .unwrap_or_default()
                .contains("activate_window"),
            "{context}: the refusal must name the way out: {refusal}"
        );
    }
}

/// Orphaned images are the exact defect, so they get their own statement.
///
/// Sweeping every window in one pass is what makes this a check of the *surface* rather
/// than of one lucky window: a fix that only worked for the window a developer happened to
/// look at would still fail here.
#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1"]
fn no_window_in_the_session_returns_an_orphaned_image() {
    if skip_if_disabled() {
        return;
    }
    let windows = all_windows();
    if windows.is_empty() {
        eprintln!("skipping: the session reports no window at all");
        return;
    }
    let mut captured = 0usize;
    let mut refused = 0usize;
    let mut total = 0usize;
    for target in windows {
        total += 1;
        let result = get_window_state(target.id);
        assert_channels_agree(&result, &format!("window {} ({})", target.id, target.app));
        if result.images > 0 {
            captured += 1;
        } else {
            refused += 1;
        }
    }
    // A run where nothing captured would pass the loop vacuously, so say what happened.
    assert!(total > 0, "the sweep must have classified something");
    eprintln!("swept {total} windows: {captured} captured, {refused} refused");
}

/// include_screenshot: false must produce neither channel, so the invariant is not an
/// artefact of always capturing.
#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1"]
fn asking_for_no_screenshot_produces_neither_channel() {
    if skip_if_disabled() {
        return;
    }
    let Some(target) = all_windows().into_iter().next() else {
        eprintln!("skipping: the session reports no window");
        return;
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert("window".to_string(), serde_json::json!({ "app": "", "id": target.id }));
    arguments.insert("include_screenshot".to_string(), serde_json::json!(false));
    let result = call("get_window_state", arguments);
    assert_channels_agree(&result, "no-screenshot request");
    assert_eq!(
        result.value["screenshots"].as_array().unwrap().len(),
        0,
        "{}",
        result.value
    );
    assert_eq!(result.images, 0, "{}", result.value);
}

// ---------------------------------------------------------------------------
// Wire-level checks: the JSONL envelope the plugin actually reads.
//
// These are the ones that catch a defect in the transport rather than in a handler. An
// in-process call to `window2::dispatch` returns a `CallToolResult`, whose `content` and
// `structured_content` are still separate fields; only `pack_call_result` decides what the
// sidecar receives. The orphaned-image bug lived exactly there, so it is invisible to any
// test that stops at the handler -- and that is why this suite drives the real binary.
// ---------------------------------------------------------------------------

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

/// The helper, spoken to over its real stdio protocol.
struct Helper {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Helper {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dsh-computer-use"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the helper binary starts");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self { child, stdin, stdout, next_id: 0 }
    }

    /// One call, answered as the value plus the number of image parts the wire carried.
    fn call(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> (serde_json::Value, usize) {
        self.next_id += 1;
        let id = self.next_id;
        let request = serde_json::json!({
            "id": id,
            "method": "call",
            "params": { "name": name, "arguments": arguments },
        });
        let mut line = request.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write request");
        self.stdin.flush().expect("flush request");

        loop {
            let mut response = String::new();
            let read = self.stdout.read_line(&mut response).expect("read response");
            assert!(read > 0, "the helper closed stdout during {name}");
            if response.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(response.trim()).expect("a JSON response");
            if parsed["id"] != serde_json::json!(id) {
                continue;
            }
            assert_eq!(parsed["ok"], serde_json::json!(true), "{parsed}");
            let result = &parsed["result"];
            let images = result["images"].as_array().map(Vec::len).unwrap_or(0);
            return (result["value"].clone(), images);
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(b"{\"id\":9999,\"method\":\"shutdown\",\"params\":{}}\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The wire-level form of the channel invariant, plus the nesting that hid the entry.
fn assert_wire_channels_agree(value: &serde_json::Value, images: usize, context: &str) {
    assert!(
        value.get("value").is_none(),
        "{context}: the caption must not be nested under a second value key: {value}"
    );
    let entries = value
        .get("screenshots")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        entries, images,
        "{context}: the wire carried {images} images but {entries} screenshots[] entries; \
         a consumer resolves a click through the entry, so the two must describe one capture: {value}"
    );
}

/// The defect exactly as the operator saw it: an entry that existed but was unreachable.
///
/// `get_window_state` was the only call that set `structured_content` *and* returned an
/// image, and the transport nested its payload under `result.value.value` for that case
/// alone. Every consumer reads `result.value.screenshots`, so on a real session every
/// visible window looked like it had no screenshot while a multi-megabyte PNG rode along in
/// `result.images`. Only a wire-level check can see this.
#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1"]
fn the_wire_keeps_a_visible_capture_at_one_entry_and_one_image() {
    if skip_if_disabled() {
        return;
    }
    let mut helper = Helper::start();
    let target = all_windows().into_iter().find(|candidate| !candidate.hidden);
    let Some(target) = target else {
        eprintln!("skipping: the session reports no visible window");
        return;
    };
    let (value, images) = helper.call(
        "get_window_state",
        serde_json::json!({ "window": { "app": "", "id": target.id }, "include_screenshot": true }),
    );
    assert_wire_channels_agree(&value, images, "visible window on the wire");
    assert_eq!(entries_of(&value), 1, "{value}");
    assert_eq!(images, 1, "{value}");
    assert_eq!(value["screenshots"][0]["id"], serde_json::json!(format!("0x{:x}:0", target.id)));
}

/// A sweep of the whole session at the wire level: no window may carry an orphaned image.
#[test]
#[ignore = "acts on the live session; run with DSH_CUA_REAL_CAPTURE=1 cargo test --test real_capture -- --ignored --test-threads=1"]
fn no_window_on_the_wire_returns_an_orphaned_image() {
    if skip_if_disabled() {
        return;
    }
    let windows = all_windows();
    if windows.is_empty() {
        eprintln!("skipping: the session reports no window");
        return;
    }
    let mut helper = Helper::start();
    let (listed, _) = helper.call("list_windows", serde_json::json!({}));
    let ids: Vec<u64> = listed["windows"]
        .as_array()
        .map(|windows| {
            windows
                .iter()
                .filter_map(|window| window["id"].as_u64())
                .collect()
        })
        .unwrap_or_default();
    assert!(!ids.is_empty(), "list_windows must report the session: {listed}");
    let mut captured = 0usize;
    let mut refused = 0usize;
    for id in &ids {
        let (value, images) = helper.call(
            "get_window_state",
            serde_json::json!({ "window": { "app": "", "id": id }, "include_screenshot": true }),
        );
        assert_wire_channels_agree(&value, images, &format!("window {id} on the wire"));
        if images > 0 {
            captured += 1;
        } else {
            refused += 1;
            // A refusal must still be actionable on the wire, not just empty.
            assert_eq!(
                value["screenshotError"]["tool"],
                serde_json::json!("activate_window"),
                "window {id} refused a capture without naming the way out: {value}"
            );
        }
    }
    // The session mixes viewable and hidden windows, so both outcomes are legitimate; what
    // is not legitimate is a window whose two channels disagree, which the loop asserts.
    let viewable = listed["detail"]
        .as_array()
        .map(|details| details.iter().filter(|detail| detail["hidden"] == serde_json::json!(false)).count())
        .unwrap_or(0);
    eprintln!("swept {} windows on the wire: {captured} captured, {refused} refused", ids.len());
    assert_eq!(
        captured, viewable,
        "every window the session reports as visible must capture: {listed}"
    );
}

fn entries_of(value: &serde_json::Value) -> usize {
    value
        .get("screenshots")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

