//! `wait_for`: block until a UI state appears or disappears, without spending a model call.
//!
//! This is a **DSH extension, not one of the official thirteen window2 methods** (see
//! `super::window2::WINDOW2_TOOLS`). It exists because the observation cadence, not the
//! driving, is what a desktop task actually costs: "act, sleep, screenshot, look, act" spends
//! one image-carrying round trip per step, and each of those is roughly nine seconds. When the
//! next action only depends on *whether* something appeared, the model does not need to look
//! at all -- it needs the helper to watch. So this returns once, with a verdict.
//!
//! # Why polling, and not AT-SPI events
//!
//! The obvious alternative is subscribing to AT-SPI object events and returning when one
//! matches. It is rejected here on three counts. The snapshot path is already the one
//! `get_window_state` exercises on every observation, so a waiter built on it inherits a code
//! path that is continuously verified rather than adding a second one that only `wait_for`
//! ever runs. A subscription needs a match rule, a bus connection held open across the whole
//! wait, and a reconnect story when the a11y bus restarts mid-wait -- a state machine whose
//! failure mode is a wait that silently never completes. And the timeout and poll parameters
//! have to be implemented on top of the subscription anyway, so the simplicity is not even
//! bought. Polling one bounded tree read is the smaller, more predictable thing.
//!
//! # What the poll deliberately does not touch
//!
//! Every read goes through `element::snapshot_window_uncached_blocking`, never
//! `snapshot_window`. Polling must not advance the cached generation: element indexes are
//! only valid against the tree the caller last received, so a waiter that bumped the
//! generation would invalidate the indexes the model just read from `get_window_state`. See
//! `element::read_tree` for the full reasoning.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::rmcp::model::CallToolResult;

use super::{element, window};

/// The method name on the helper wire. The JS tool surface spells it
/// `computer_use_wait_for`; this is the name the call envelope carries.
pub const WAIT_FOR_TOOL: &str = "wait_for";

/// The default wait, in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;
/// The hard ceiling on a wait, in milliseconds.
///
/// 20 s, not 30 s, and the number is a contract rather than a taste. DSH kills a tool call at
/// its own ~25 s budget, which is a heavier event than a clean timeout, and the sidecar's
/// per-request transport budget is 10 s by default. A `wait_for` that promised 30 s could not
/// honour it on any real host, so the ceiling is set below the smallest budget that applies and
/// the clamp is reported in the result instead of being hidden.
pub const MAX_TIMEOUT_MS: u64 = 20_000;
/// The default interval between tree reads, in milliseconds.
pub const DEFAULT_POLL_MS: u64 = 250;
/// The floor on the poll interval, so a caller cannot turn a wait into a busy loop.
pub const MIN_POLL_MS: u64 = 50;
/// Upper bound on the presence-grace window the `gone` condition uses.
const MAX_PRESENCE_GRACE_MS: u64 = 1_000;
/// Node budget for each poll. Matches `get_window_state`, so a hit reports the same tree.
const POLL_MAX_NODES: usize = 1_000;
const POLL_MAX_DEPTH: u32 = 32;
/// How many polls may fail in a row before the whole wait gives up.
///
/// One failure is noise (the app repainted, the bus hiccupped); failing forever and then
/// reporting a clean `matched: false` would be a lie, so the wait stops and says why.
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Which of the three conditions the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Some text appears in the rendered tree.
    TextSubstring,
    /// An element with a matching name appears.
    ElementName,
    /// Text that was present goes away.
    Gone,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::TextSubstring => "text_substring",
            Kind::ElementName => "element_name",
            Kind::Gone => "gone",
        }
    }
}

/// The parsed, validated arguments.
///
/// Validation happens before any polling, so a malformed call is refused immediately instead
/// of burning the caller's whole timeout budget to discover it.
#[derive(Debug, Clone)]
struct Request {
    window_id: u64,
    kind: Kind,
    needle: String,
    timeout_ms: u64,
    poll_ms: u64,
    /// True when the caller asked for more than `MAX_TIMEOUT_MS`.
    timeout_clamped: bool,
}

/// One tree read reduced to the facts a condition is decided on.
#[derive(Debug, Clone)]
struct Probe {
    /// The rendered tree text, in the same format `get_window_state` prints.
    tree: String,
    /// Every element name in the tree, for the `element_name` condition.
    names: Vec<String>,
    /// Index, role and name of every node, so a hit can name the element that satisfied it.
    candidate: Vec<Candidate>,
    nodes: usize,
    generation: u64,
    source: Option<String>,
}

/// The identifying part of one node, for a `match` report.
#[derive(Debug, Clone)]
struct Candidate {
    index: u32,
    role: String,
    name: Option<String>,
}

impl Probe {
    /// Whether this tree satisfies `kind` against `needle`.
    ///
    /// The loop itself asks [`Probe::present`], because the two-phase `gone` logic needs the
    /// presence question. This stays as the direct expression of the contract and is what the
    /// unit tests assert against -- notably that `gone` is the exact negation of
    /// `text_substring`, which is the property a caller pairs them for.
    #[cfg(test)]
    fn matched(&self, kind: Kind, needle: &str) -> bool {
        match kind {
            Kind::TextSubstring | Kind::ElementName => self.present(kind, needle),
            // `gone` is defined on the same text channel as `text_substring`, so the two are
            // exact negations of each other and a caller can pair them without surprises.
            Kind::Gone => !self.present(Kind::Gone, needle),
        }
    }

    /// The node that satisfies `kind` against `needle`, if any.
    ///
    /// A condition that matched is more useful when it says *what* matched: the model can then
    /// act on that element instead of re-observing to find out which one changed.
    fn hit(&self, kind: Kind, needle: &str) -> Option<&Candidate> {
        let needle_lower = needle.to_ascii_lowercase();
        match kind {
            Kind::ElementName => self.candidate.iter().find(|candidate| {
                candidate
                    .name
                    .as_deref()
                    .is_some_and(|name| name.to_ascii_lowercase().contains(&needle_lower))
            }),
            // For text conditions the smallest node carrying the text is the useful answer;
            // the flattened order means the deepest match is also the most specific one.
            Kind::TextSubstring | Kind::Gone => self.candidate.iter().rev().find(|candidate| {
                candidate
                    .name
                    .as_deref()
                    .is_some_and(|name| name.to_ascii_lowercase().contains(&needle_lower))
            }),
        }
    }

    /// Whether the thing being watched is in the tree *right now*.
    ///
    /// Deliberately separate from [`Probe::matched`], and the distinction is not academic.
    /// `gone` inverts its condition, so its `matched` is true when the target is *absent* --
    /// meaning "did this condition match" and "is the target there" are two different questions
    /// for that kind. `run` has to ask both (it tracks presence and then looks for absence), so
    /// conflating them once inverted the whole path: the wait recorded presence for a text that
    /// was never there, and reported a match for a text that was still on screen.
    fn present(&self, kind: Kind, needle: &str) -> bool {
        match kind {
            Kind::TextSubstring | Kind::Gone => contains_ignore_ascii_case(&self.tree, needle),
            Kind::ElementName => self
                .names
                .iter()
                .any(|name| contains_ignore_ascii_case(name, needle)),
        }
    }
}

/// What one condition found, for the result payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The condition holds.
    Matched,
    /// The wait ran out of time with the condition still false.
    TimedOut,
    /// The condition cannot be true any more, so waiting on it would be pointless.
    Vacuous,
}

/// Case-insensitive substring search over ASCII.
///
/// `to_ascii_lowercase` is the right primitive here rather than Unicode case folding: the
/// needles are UI labels, the tree is UTF-8, and folding can change byte lengths, so the
/// comparison stays byte-oriented and predictable.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

/// Render a snapshot the way `get_window_state` does, so a `wait_for` hit names the same
/// elements the model would read out of an observation.
///
/// Kept byte-identical to `window2::render_tree` on purpose. The two must agree, or a
/// `text_substring` that matches here would not be text the model could ever have seen; a
/// test pins the agreement (`a_probe_renders_the_tree_exactly_as_get_window_state_does`).
fn render_tree(snapshot: &element::ElementSnapshot) -> String {
    let mut out = String::new();
    for node in &snapshot.nodes {
        for _ in 0..node.depth {
            out.push_str("  ");
        }
        out.push_str(&format!("[{}] {}", node.index, node.role));
        if let Some(name) = node.name.as_ref().filter(|name| !name.trim().is_empty()) {
            out.push_str(&format!(" {:?}", name));
        }
        if node.supports_editable_text {
            out.push_str(" (editable)");
        }
        out.push('\n');
    }
    out
}

fn probe_from(snapshot: &element::ElementSnapshot) -> Probe {
    Probe {
        tree: render_tree(snapshot),
        names: snapshot
            .nodes
            .iter()
            .filter_map(|node| node.name.clone())
            .filter(|name| !name.trim().is_empty())
            .collect(),
        candidate: snapshot
            .nodes
            .iter()
            .map(|node| Candidate {
                index: node.index,
                role: node.role.clone(),
                name: node.name.clone(),
            })
            .collect(),
        nodes: snapshot.nodes.len(),
        generation: snapshot.generation,
        source: snapshot.app_name.clone(),
    }
}

/// The JSON tool result carrying no image.
fn json_result(value: Value) -> CallToolResult {
    CallToolResult::success(vec![crate::rmcp::model::Content::text(value.to_string())])
}

/// A structured refusal: what was wrong, and what to do instead.
///
/// Shaped like the window2 refusal so the model meets one refusal dialect on this surface.
fn refusal(reason: &str, alternative: &str) -> String {
    json!({
        "error": "unsupported",
        "method": WAIT_FOR_TOOL,
        "reason": reason,
        "alternative": alternative,
    })
    .to_string()
}

/// The window argument every window2 action takes.
///
/// Wording and shape deliberately match `window2::window_argument` byte for byte. A malformed
/// argument is not an "unsupported" method, and saying so would be actively harmful: a model
/// that reads `error: "unsupported"` for a call it merely spelled wrong concludes the tool does
/// not exist on this platform and abandons the approach. The structured refusal is reserved for
/// the case where that verdict is true -- see the unreadable-tree path in `run`.
fn window_id_argument(arguments: &Map<String, Value>) -> Result<u64, String> {
    let object = arguments.get("window").and_then(Value::as_object).ok_or_else(|| {
        "window is required and must be a Window object from list_windows()".to_string()
    })?;
    object.get("id").and_then(Value::as_u64).ok_or_else(|| {
        "window.id is required and must be the Window id from list_windows()".to_string()
    })
}

fn optional_u64(arguments: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            format!("{key} must be a non-negative integer, in milliseconds (for example 5000)")
        }),
    }
}

/// Parse and validate the arguments.
fn request_from(arguments: &Map<String, Value>) -> Result<Request, String> {
    let window_id = window_id_argument(arguments)?;

    // Exactly one condition. Zero would make the call ambiguous; two would make "matched"
    // mean whichever was checked first, which no caller can reason about.
    let mut conditions: Vec<(Kind, String)> = Vec::new();
    for (key, kind) in [
        ("text_substring", Kind::TextSubstring),
        ("element_name", Kind::ElementName),
        ("gone", Kind::Gone),
    ] {
        match arguments.get(key) {
            None | Some(Value::Null) => continue,
            Some(value) => match value.as_str() {
                Some(text) if !text.is_empty() => conditions.push((kind, text.to_string())),
                Some(_) => {
                    return Err(format!("{key} must not be empty"))
                }
                None => {
                    return Err(format!(
                        "{key} must be a string (text_substring, element_name or gone)"
                    ))
                }
            },
        }
    }
    let (kind, needle) = match conditions.len() {
        1 => conditions.remove(0),
        0 => {
            return Err(
                "wait_for needs exactly one condition: pass text_substring, element_name or gone"
                    .to_string(),
            )
        }
        _ => {
            return Err(
                "wait_for takes exactly one condition, not several: pass only one of text_substring, element_name or gone"
                    .to_string(),
            )
        }
    };

    let requested_timeout = optional_u64(arguments, "timeout_ms")?;
    let timeout_clamped = requested_timeout.is_some_and(|value| value > MAX_TIMEOUT_MS);
    let timeout_ms = requested_timeout
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, MAX_TIMEOUT_MS);
    let poll_ms = optional_u64(arguments, "poll_ms")?
        .unwrap_or(DEFAULT_POLL_MS)
        .clamp(MIN_POLL_MS, MAX_TIMEOUT_MS);

    Ok(Request {
        window_id,
        kind,
        needle,
        timeout_ms,
        poll_ms,
        timeout_clamped,
    })
}

/// Run the wait. Synchronous: the window2 dispatcher already runs on the blocking pool.
pub fn wait_for(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let request = request_from(arguments)?;
    // A window that does not exist is refused before the wait starts: otherwise every poll
    // would fail and the caller would get a timeout that hides the real reason.
    let target = window::get_window(request.window_id).map_err(|error| error.to_string())?;
    run(&request, &target)
}

fn run(request: &Request, target: &window::X11Window) -> Result<CallToolResult, String> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(request.timeout_ms);
    let poll = Duration::from_millis(request.poll_ms);
    // `gone` has to be told apart from "was never there": a caller waiting for a dialog to
    // disappear should not sit out the whole budget when it is already gone, and should not be
    // told "matched" when it never appeared (a false positive it cannot detect). So a bounded
    // grace window establishes presence first.
    let presence_grace = Duration::from_millis(request.poll_ms.min(MAX_PRESENCE_GRACE_MS));
    let grace_deadline = started + presence_grace;

    let mut polls: u64 = 0;
    let mut consecutive_errors: u32 = 0;
    let mut last_error: Option<String> = None;
    let mut saw_present = false;
    let mut last: Option<Probe> = None;
    let mut outcome = Outcome::TimedOut;
    let mut never_read: Option<String> = None;
    // A window whose app publishes no accessibility tree at all (a raw X window, or an
    // Electron app started without its a11y bridge) answers every poll with zero nodes.
    // That is not an error, but reporting a bare `matched: false` after the whole budget
    // would hide the one fact that explains it, so it is surfaced separately below.
    let mut empty_polls: u64 = 0;

    loop {
        // Reading the tree is the only thing here that can fail, and a failure is not a
        // non-match: it is tracked separately, and only a run of them gives up.
        match element::snapshot_window_uncached_blocking(
            request.window_id,
            POLL_MAX_NODES,
            POLL_MAX_DEPTH,
        ) {
            Ok(snapshot) => {
                consecutive_errors = 0;
                // Cleared on success so a warning only ever describes the trailing failures,
                // never one that a later good poll already recovered from.
                if last_error.is_some() {
                    last_error = None;
                }
                polls += 1;
                let probe = probe_from(&snapshot);
                if probe.nodes == 0 {
                    empty_polls += 1;
                }
                // `there` answers "is the target in the tree now", which is what the two-phase
                // `gone` logic needs; `matched` would answer the inverted question for `gone`.
                let there = probe.present(request.kind, &request.needle);
                last = Some(probe);

                match request.kind {
                    Kind::Gone => {
                        if there {
                            // Seen on screen: from here on, its disappearance is a real event
                            // worth waiting for.
                            saw_present = true;
                        } else if saw_present {
                            // It was there and it is not any more: this is the condition.
                            outcome = Outcome::Matched;
                            break;
                        } else if Instant::now() >= grace_deadline {
                            // Never observed during the grace window: it is already gone, so
                            // there is nothing to wait for. Reported distinctly from a timeout.
                            outcome = Outcome::Vacuous;
                            break;
                        }
                    }
                    _ => {
                        if there {
                            outcome = Outcome::Matched;
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                consecutive_errors += 1;
                last_error = Some(error.to_string());
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    // Distinguish "no tree at all" from "the condition never held". A
                    // `matched: false` here would be a false negative the caller acts on.
                    if polls == 0 {
                        never_read = last_error.clone();
                    }
                    break;
                }
            }
        }

        if Instant::now() >= deadline {
            break;
        }
        // Never sleep past the deadline, so a small timeout is not rounded up to a multiple
        // of the poll interval.
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(poll.min(remaining));
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;

    if let Some(error) = never_read {
        return Err(refusal(
            &format!("the accessibility tree could not be read for this window: {error}"),
            "check that at-spi2-core is running and the app publishes an accessibility tree; get_window_state reports the same failure",
        ));
    }

    // Both `Matched` and `Vacuous` are satisfied conditions: the first because the target was
    // observed leaving, the second because it was never there to begin with. `Vacuous` is
    // reported with `observedPresent: false` and its own note so the caller can tell the two
    // apart -- a distinction that matters, because "I watched it disappear" and "it was never
    // there" call for different follow-up.
    let matched = matches!(outcome, Outcome::Matched | Outcome::Vacuous);
    let mut value = json!({
        "ok": true,
        "matched": matched,
        "elapsedMs": elapsed_ms,
        "polls": polls,
        "condition": { "kind": request.kind.label(), "value": request.needle },
        "timeoutMs": request.timeout_ms,
        "pollMs": request.poll_ms,
        "window": { "app": target.app, "id": target.id },
        "backend": super::X11_NATIVE_BACKEND,
    });
    if let Some(title) = target.title.as_ref() {
        value["window"]["title"] = json!(title);
    }
    if request.timeout_clamped {
        // Reported, never silent: the caller asked for more than this helper will hold a wait
        // open, and needs to know the ceiling it actually got.
        value["timeoutClamped"] = json!(true);
        value["maxTimeoutMs"] = json!(MAX_TIMEOUT_MS);
    }
    if request.kind == Kind::Gone {
        value["observedPresent"] = json!(saw_present);
    }
    // Only when *every* successful poll was empty: a window that merely failed to paint for
    // one poll is not evidence of anything.
    if polls > 0 && empty_polls == polls {
        value["emptyTree"] = json!(true);
        value["warning"] = json!(format!(
            "all {polls} polls returned an empty accessibility tree; this app publishes no AT-SPI tree, so a text or element condition cannot match. Check that at-spi2-core is running and the app was started with its accessibility bridge enabled.",
        ));
    }
    match (request.kind, outcome) {
        (_, Outcome::TimedOut) => {
            // A timeout is a fact about the UI, not a failure of the call, so it is a
            // successful result with `matched: false`. The caller decides what to do next.
            value["note"] = json!(format!(
                "the condition {:?} was still false after {} ms ({} polls); this is not an error",
                request.kind.label(),
                request.timeout_ms,
                polls
            ));
        }
        (Kind::Gone, Outcome::Vacuous) => {
            value["note"] = json!(format!(
                "\"{}\" was never present during the first {} ms of polling, so the condition already holds; observedPresent is false",
                request.needle,
                presence_grace.as_millis()
            ));
        }
        _ => {}
    }
    if let Some(error) = last_error.as_ref() {
        // A run of failures that ended below the give-up threshold is still worth telling the
        // caller about: the verdict rests on fewer polls than it looks like.
        value["warning"] = json!(format!("some polls failed; last error: {error}"));
    }
    if matched {
        // Which element satisfied the condition, when the satisfying condition is name-based.
        // `gone` reports no `match` because nothing is there to point at by definition.
        if request.kind != Kind::Gone {
            if let Some(hit) = last
                .as_ref()
                .and_then(|probe| probe.hit(request.kind, &request.needle))
            {
                let mut entry = json!({ "index": hit.index, "role": hit.role });
                if let Some(name) = hit.name.as_ref() {
                    entry["name"] = json!(name);
                }
                value["match"] = entry;
            }
        }
    }
    if let Some(probe) = last.as_ref() {
        value["nodes"] = json!(probe.nodes);
        if probe.generation > 0 {
            value["treeGeneration"] = json!(probe.generation);
        }
        if let Some(source) = probe.source.as_ref() {
            value["source"] = json!(source);
        }
    }
    Ok(json_result(value))
}

/// The tool definition, in the shape the helper's `tools` method returns.
pub fn definition() -> Value {
    json!({
        "name": WAIT_FOR_TOOL,
        "description": "DSH extension (not one of the official window2 methods). Block until a UI state appears or disappears, then answer once. Use it instead of polling with repeated get_window_state calls: it costs one call and no screenshot instead of one model round trip per step. Returns matched=true when the condition held, and matched=false after timeout_ms without it holding (which is not an error). Exactly one of text_substring, element_name or gone must be given.",
        "parameters": {
            "type": "object",
            "properties": {
                "window": {
                    "type": "object",
                    "description": "Window object from list_apps() or list_windows() to watch.",
                    "properties": {
                        "app": { "type": "string", "description": "App identifier for the app that owns this window; process-backed identifiers may include the full process path." },
                        "id": { "type": "integer", "description": "Opaque identifier for the open window." },
                        "title": { "type": "string", "description": "User-visible window title when available; may contain PII." }
                    },
                    "required": ["app", "id"],
                    "additionalProperties": false
                },
                "text_substring": { "type": "string", "description": "Wait until this text appears in the accessibility tree (case-insensitive substring)." },
                "element_name": { "type": "string", "description": "Wait until an element whose name contains this text appears (case-insensitive substring)." },
                "gone": { "type": "string", "description": "Wait until this text is no longer in the accessibility tree. When it was never there, the call answers matched=true with observedPresent=false instead of waiting out the budget." },
                "timeout_ms": { "type": "integer", "description": "How long to wait, in milliseconds; defaults to 5000 and is clamped to 20000." },
                "poll_ms": { "type": "integer", "description": "How often to read the tree, in milliseconds; defaults to 250 and is floored at 50." }
            },
            "required": ["window"],
            "additionalProperties": false
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atspi_tree::AccessibilityNode;

    fn window_args(extra: &[(&str, Value)]) -> Map<String, Value> {
        let mut arguments = Map::new();
        arguments.insert("window".to_string(), json!({ "app": "Fixture", "id": 42 }));
        for (key, value) in extra {
            arguments.insert((*key).to_string(), value.clone());
        }
        arguments
    }

    fn node(index: u32, depth: u32, role: &str, name: Option<&str>, editable: bool) -> AccessibilityNode {
        AccessibilityNode {
            index,
            parent_index: None,
            depth,
            object_ref: format!(":1.{index}"),
            role: role.to_string(),
            name: name.map(str::to_string),
            description: None,
            child_count: 0,
            bounds: None,
            states: Vec::new(),
            actions: Vec::new(),
            value: None,
            text: None,
            supports_editable_text: editable,
        }
    }

    #[test]
    fn it_is_not_advertised_as_one_of_the_official_methods() {
        // The parity assertion in window2.rs pins the official thirteen, so this name must
        // stay out of that table and be advertised separately (see helper.rs).
        assert!(!super::super::window2::WINDOW2_TOOLS.contains(&WAIT_FOR_TOOL));
        assert_eq!(definition()["name"], json!(WAIT_FOR_TOOL));
    }

    #[test]
    fn the_defaults_are_what_the_documentation_promises() {
        let request = request_from(&window_args(&[("text_substring", json!("Ready"))])).unwrap();
        assert_eq!(request.timeout_ms, 5_000);
        assert_eq!(request.poll_ms, 250);
        assert!(!request.timeout_clamped);
        assert_eq!(request.kind, Kind::TextSubstring);
        assert_eq!(request.window_id, 42);
    }

    #[test]
    fn a_timeout_above_the_ceiling_is_clamped_and_says_so() {
        let request = request_from(&window_args(&[
            ("gone", json!("Busy")),
            ("timeout_ms", json!(90_000)),
        ]))
        .unwrap();
        assert_eq!(request.timeout_ms, MAX_TIMEOUT_MS);
        assert!(request.timeout_clamped);
    }

    #[test]
    fn a_poll_interval_below_the_floor_is_raised() {
        // 0 would be a busy loop that pins a core for the whole wait.
        let request = request_from(&window_args(&[
            ("text_substring", json!("Ready")),
            ("poll_ms", json!(0)),
        ]))
        .unwrap();
        assert_eq!(request.poll_ms, MIN_POLL_MS);
    }

    #[test]
    fn no_condition_and_several_conditions_are_both_refused() {
        let none = request_from(&window_args(&[])).unwrap_err();
        assert!(none.contains("exactly one condition"), "{none}");
        // An argument mistake must NOT be dressed up as an unsupported method: a model that
        // reads that verdict stops using the tool instead of fixing the call.
        assert!(!none.contains("unsupported"), "{none}");
        let several = request_from(&window_args(&[
            ("text_substring", json!("Ready")),
            ("element_name", json!("Confirm")),
        ]))
        .unwrap_err();
        assert!(several.contains("exactly one condition"), "{several}");
        // A present-but-empty condition is refused too: it would match everything.
        let empty = request_from(&window_args(&[("text_substring", json!(""))])).unwrap_err();
        assert!(empty.contains("must not be empty"), "{empty}");
    }

    #[test]
    fn a_missing_window_is_refused_by_name() {
        let error = request_from(&Map::new()).unwrap_err();
        assert!(error.contains("window is required"), "{error}");
        // Byte-identical wording to `window2::window_argument`, which the official thirteen
        // use, so a caller sees one dialect on this surface.
        assert_eq!(
            error,
            "window is required and must be a Window object from list_windows()"
        );
        // And the same shape: a plain message, not a structured refusal.
        assert!(!error.contains("\"error\""), "{error}");
    }

    #[test]
    fn the_definition_documents_every_parameter_it_accepts() {
        let definition = definition();
        let properties = definition["parameters"]["properties"].as_object().unwrap();
        for key in [
            "window",
            "text_substring",
            "element_name",
            "gone",
            "timeout_ms",
            "poll_ms",
        ] {
            assert!(properties.contains_key(key), "{key} must be documented");
        }
        assert_eq!(definition["parameters"]["required"], json!(["window"]));
        assert!(definition["description"]
            .as_str()
            .unwrap()
            .contains("DSH extension"));
    }

    #[test]
    fn matching_is_case_insensitive_and_gone_negates_text() {
        let probe = Probe {
            tree: "[0] application \"App\"\n  [1] label \"Ready\"\n".to_string(),
            names: vec!["App".to_string(), "Ready".to_string()],
            candidate: vec![
                Candidate { index: 0, role: "application".to_string(), name: Some("App".to_string()) },
                Candidate { index: 1, role: "label".to_string(), name: Some("Ready".to_string()) },
            ],
            nodes: 2,
            generation: 7,
            source: Some("pid 1".to_string()),
        };
        assert!(probe.matched(Kind::TextSubstring, "ready"));
        assert!(probe.matched(Kind::TextSubstring, "READY"));
        assert!(!probe.matched(Kind::TextSubstring, "Finished"));
        assert!(probe.matched(Kind::ElementName, "read"));
        assert!(!probe.matched(Kind::ElementName, "Finished"));
        // `present` and `matched` must NOT be the same question for `gone`: `present` says
        // whether the text is on screen, `matched` whether the condition holds. Conflating
        // them inverted the whole `gone` path once (a never-present text was recorded as
        // present, and a still-visible text was reported as matched), so it is pinned here.
        // The hit must name the element that satisfied the condition.
        let text_hit = probe.hit(Kind::TextSubstring, "ready").expect("a hit");
        assert_eq!(text_hit.role, "label");
        assert_eq!(text_hit.name.as_deref(), Some("Ready"));
        assert_eq!(text_hit.index, 1);
        assert!(probe.hit(Kind::ElementName, "nope").is_none());
        assert!(probe.present(Kind::Gone, "Ready"));
        assert!(!probe.present(Kind::Gone, "Finished"));
        assert!(!probe.matched(Kind::Gone, "Ready"));
        assert!(probe.matched(Kind::Gone, "Finished"));
        // Paired with text_substring: exactly one of the two can hold, for any needle.
        for needle in ["Ready", "Finished", ""] {
            assert_ne!(
                probe.matched(Kind::TextSubstring, needle),
                probe.matched(Kind::Gone, needle),
                "gone must be the negation of text_substring for {needle:?}"
            );
        }
    }

    #[test]
    fn a_probe_renders_the_tree_exactly_as_get_window_state_does() {
        // The two renderers must agree, or a `text_substring` that matches in the waiter
        // would not be text the model could ever have seen in an observation.
        let snapshot = element::ElementSnapshot {
            window_id: 1,
            generation: 3,
            nodes: vec![
                node(0, 0, "frame", Some("Window"), false),
                node(1, 1, "entry", Some("Name"), true),
                node(2, 1, "filler", None, false),
                node(3, 2, "label", Some("   "), false),
            ],
            captured_at_ms: 0,
            app_name: Some("Fixture".to_string()),
        };
        let expected =
            "[0] frame \"Window\"\n  [1] entry \"Name\" (editable)\n  [2] filler\n    [3] label\n";
        assert_eq!(render_tree(&snapshot), expected);
        // A whitespace-only name is not printed, and so is not a possible match target.
        let probe = probe_from(&snapshot);
        assert_eq!(probe.names, vec!["Window", "Name"]);
        assert!(!probe.matched(Kind::TextSubstring, "label \""));
    }
}
