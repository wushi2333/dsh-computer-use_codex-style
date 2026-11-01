//! WINDOW2 + harness tool JSON schemas matching `computer_use/tools.py` + `surfaces.py`.

use serde_json::{json, Value};

fn obj(properties: Value, required: &[&str]) -> Value {
    let mut body = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        body["required"] = json!(required);
    }
    body
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn n(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}

fn i(desc: &str) -> Value {
    json!({"type": "integer", "description": desc})
}

fn b(desc: &str) -> Value {
    json!({"type": "boolean", "description": desc})
}

/// Official `Window` object. `purpose` completes the doc-comment sentence
/// "`Window object from \`list_apps()\` or \`list_windows()\` to <purpose>.`"
/// exactly as `docs/api.md` words it for each tool.
fn window(purpose: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("Window object from list_apps() or list_windows() {purpose}."),
        "properties": {
            "app": s("App identifier for the app that owns this window; process-backed identifiers may include the full process path."),
            "id": i("Opaque identifier for the open window."),
            "title": s("User-visible window title when available; may contain PII."),
        },
        "required": ["app", "id"],
        "additionalProperties": false,
    })
}

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "parameters": parameters,
    })
}

/// Official window2 tool table: the 13 methods in `@oai/sky` `types/window2/*`
/// (`Window2ComputerUseClient`). Descriptions and parameter documentation are
/// verbatim from the official `docs/api.md` TypeScript reference.
pub fn window2_tools() -> Vec<Value> {
    let mut tools = vec![
        tool(
            "list_windows",
            "List open windows that can be targeted by the window2 API.",
            obj(json!({}), &[]),
        ),
        tool(
            "get_window",
            "Rehydrate a currently open window by id; useful after losing a window binding.",
            obj(
                json!({
                    "app": s("Optional app identifier to carry forward from a previously returned Window."),
                    "id": i("Opaque window identifier from a previously returned Window."),
                }),
                &["id"],
            ),
        ),
        tool(
            "list_apps",
            "List installed apps, including their currently open targetable windows when present.",
            obj(json!({}), &[]),
        ),
        tool(
            "launch_app",
            "Launch an app by id so its window can be selected from list_apps().",
            obj(
                json!({
                    "app": s("App id returned by list_apps(), or an explicit .exe process path/identifier for apps that are not yet discoverable in list_apps()."),
                }),
                &["app"],
            ),
        ),
        tool(
            "get_window_state",
            "Capture selected state for an open window.",
            obj(
                json!({
                    "window": window("to capture"),
                    "include_screenshot": b("Whether to capture and display a screenshot of the window; defaults to true."),
                    "include_text": b("Whether to capture accessibility text describing visible elements and indexes; defaults to false."),
                }),
                &["window"],
            ),
        ),
        tool(
            "click",
            "Click either an indexed element from the latest window state or a coordinate in the window.",
            obj(
                json!({
                    "window": window("to click in"),
                    "click_count": i("Number of clicks to perform."),
                    "element_index": i("Element index from the latest get_window_state() accessibility tree."),
                    "mouse_button": json!({"type": "string", "enum": ["left", "right", "middle", "l", "r", "m"], "description": "Mouse button to click."}),
                    "screenshotId": s("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "x": n("Window-relative X coordinate."),
                    "y": n("Window-relative Y coordinate."),
                }),
                &["window"],
            ),
        ),
        tool(
            "press_key",
            "Press a + separated keyboard chord in a window.",
            obj(
                json!({
                    "key": s("Key or `+`-separated key chord using X Window System keysym-style names, such as `a`, `space`, `Return`, `Tab`, `Control_L+a`, `Control_L+Shift_L+period`, or `KP_0`; whitespace around `+` is ignored, and common aliases such as `Control`, `Ctrl`, `Alt`, `Shift`, `period`, `greater`, and `Numpad_0` are accepted."),
                    "window": window("to receive the key press"),
                }),
                &["window", "key"],
            ),
        ),
        tool(
            "type_text",
            "Type text into the current focus in a window.",
            obj(
                json!({
                    "text": s("Text to type into the current focus."),
                    "window": window("to type into"),
                }),
                &["window", "text"],
            ),
        ),
        tool(
            "scroll",
            "Scroll by a delta from a specific coordinate in the window.",
            obj(
                json!({
                    "screenshotId": s("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "scrollX": n("Horizontal scroll delta; negative means left, positive means right."),
                    "scrollY": n("Vertical scroll delta; negative means up, positive means down."),
                    "window": window("to scroll"),
                    "x": n("Window-relative X coordinate to scroll from."),
                    "y": n("Window-relative Y coordinate to scroll from."),
                }),
                &["window", "x", "y", "scrollX", "scrollY"],
            ),
        ),
        tool(
            "set_value",
            "Replace the value of an indexed editable element.",
            obj(
                json!({
                    "element_index": i("Element index from the latest get_window_state() accessibility tree."),
                    "value": s("Replacement value for the editable element."),
                    "window": window("containing the editable element"),
                }),
                &["window", "element_index", "value"],
            ),
        ),
        tool(
            "drag",
            "Drag from one window coordinate to another.",
            obj(
                json!({
                    "from_x": n("Starting window-relative X coordinate."),
                    "from_y": n("Starting window-relative Y coordinate."),
                    "screenshotId": s("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "to_x": n("Ending window-relative X coordinate."),
                    "to_y": n("Ending window-relative Y coordinate."),
                    "window": window("to drag in"),
                }),
                &["window", "from_x", "from_y", "to_x", "to_y"],
            ),
        ),
        tool(
            "perform_secondary_action",
            "Invoke a secondary accessibility action on an indexed element.",
            obj(
                json!({
                    "action": s("Secondary action label from get_window_state(), such as `Raise`, `Scroll Up`, `Scroll Down`, `Scroll Left`, `Scroll Right`, `Expand`, or `Collapse`; matching is case-insensitive."),
                    "element_index": i("Element index from the latest get_window_state() accessibility tree."),
                    "window": window("containing the element"),
                }),
                &["window", "element_index", "action"],
            ),
        ),
        tool(
            "activate_window",
            "Optional escape hatch to bring an open window to the foreground; input methods activate their target window automatically.",
            obj(json!({"window": window("to bring to the foreground")}), &["window"]),
        ),
        tool(
            "batch_actions",
            "Run several UI actions then one refresh (the official batch-actions-then-observe loop). Only batch actions that do not consume a fresh `element_index`: coordinate actions from one `screenshotId`, or focus actions such as `press_key`/`type_text`. Element-indexed actions must be one per refresh; do not pass `element_index` to `scroll`, use `scroll_element` for indexed scrolling.",
            obj(
                json!({
                    "actions": json!({"type": "array", "items": {"type": "object"}, "description": "{name, arguments} calls"}),
                    "then": s("get_window_state"),
                    "refresh_args": json!({"type": "object"}),
                }),
                &["actions"],
            ),
        ),
        tool(
            "session_note",
            "Append cross-step internal harness notes (not user-visible).",
            obj(json!({"text": s("Note"), "key": s("Optional note key"), "value": s("Optional note value")}), &[]),
        ),
        tool(
            "session_state",
            "Return persisted handles, screenshot ids, and reasoning notes.",
            obj(json!({}), &[]),
        ),
        tool(
            "end_turn",
            "End the Computer Use turn (official Interrupt/Stop: helper end_turn + Local\\CodexComputerUseTurnEnded-*).",
            obj(json!({"session_id": s("Codex session id"), "turn_id": s("Codex turn id")}), &[]),
        ),
        tool(
            "diagnostic_state",
            "Helper diagnostic_state: backend, DPI, overlay, lease, approved apps.",
            obj(json!({}), &[]),
        ),
    ];
    // Official gate: the two audio methods exist only when SKY_ENABLE_AUDIO=1.
    if std::env::var("SKY_ENABLE_AUDIO").ok().as_deref() == Some("1") {
        tools.push(tool(
            "start_audio_recording",
            "Start recording loopback audio while other computer-use actions continue.",
            obj(
                json!({
                    "max_duration_ms": i("Stop automatically after this many milliseconds (default 60000, maximum 300000)."),
                }),
                &[],
            ),
        ));
        tools.push(tool(
            "stop_audio_recording",
            "Stop the active computer audio recording and return a 24 kHz stereo WAV.",
            obj(json!({}), &[]),
        ));
    }
    tools
}

/// Official helper-wire `scroll_element` (`ElementScrollParams with 4 elements`,
/// `strings_all.txt:18091 @0x130640`: window, element_index, direction, pages).
///
/// It is **not** on the official 13-method model surface (`Window2ComputerUseClient`),
/// so it is only advertised through the DSH `harness` surface. The official model
/// surface tells the model to click a pane and then scroll by coordinate
/// (`guidance.md:222`: "Do not pass `element_index` to `scroll`").
pub fn scroll_element_tool() -> Value {
    tool(
        "scroll_element",
        "Scroll a specific accessibility element by direction and pages (DSH extension). The official 13-method model surface has no scroll_element; it exists on the helper wire as ElementScrollParams, and the official guidance instead says to click the pane first and then scroll by coordinate. Use this when a specific pane must be scrolled without a coordinate click.",
        obj(
            json!({
                "window": window("containing the element"),
                "element_index": i("Element index from the latest get_window_state() accessibility tree."),
                "direction": json!({"type": "string", "enum": ["up", "down", "left", "right"], "description": "Scroll direction."}),
                "pages": n("Number of pages to scroll; must be a finite number > 0."),
            }),
            &["window", "element_index", "direction", "pages"],
        ),
    )
}

/// DSH harness extensions that exist nowhere in the official catalogue.
pub fn harness_only_tools() -> Vec<Value> {
    vec![scroll_element_tool(), fill_form_tool()]
}

fn fill_form_tool() -> Value {
    tool(
        "fill_form",
        "Atomically fill several form fields in one call, then return the per-step results for a single final observation. Use this instead of separate set_value/click round-trips whenever a form has 2+ fields: it *replaces* values (select-all then write) so a stale value can never become \"old + new\", and it stops at the first failing step unless continue_on_error is set. Field actions: \"set\" (write value), \"check\" (tick a checkbox), \"click\" (press a button), \"press\" (a key, default Return).",
        obj(
            json!({
                "window": window("containing the form"),
                "continue_on_error": b("Keep going after a failing step instead of stopping (default false)."),
                "fields": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "element_index": i("Element index from the latest get_window_state() accessibility tree."),
                            "action": s("One of: set, check, click, press (defaults to set when value is present, otherwise click)."),
                            "value": s("Value to write for action \"set\"."),
                            "key": s("Key to press for action \"press\" (default Return)."),
                        },
                        "required": ["element_index"],
                    },
                    "description": "Ordered form steps, executed in one shot.",
                },
            }),
            &["window", "fields"],
        ),
    )
}

/// The DSH `harness` surface: every catalogue entry that is not one of the
/// official 13 (`batch_actions`/`session_note`/`session_state`/`end_turn`/
/// `diagnostic_state`, plus the two `SKY_ENABLE_AUDIO=1` audio methods) and the
/// `scroll_element` extension. The sidecar plugin requests this separately from
/// `computer`, so the official surface stays exactly 13.
pub fn harness_tools() -> Vec<Value> {
    let mut tools: Vec<Value> = window2_tools()
        .into_iter()
        .filter(|tool| !is_window2_core(tool))
        .collect();
    tools.extend(harness_only_tools());
    tools
}

fn is_window2_core(tool: &Value) -> bool {
    matches!(
        tool.get("name").and_then(Value::as_str),
        Some(name) if WINDOW2_CORE.contains(&name)
    )
}

fn window2_core_tools() -> Vec<Value> {
    window2_tools().into_iter().filter(is_window2_core).collect()
}

pub const VOID_TOOLS: &[&str] = &[
    "click",
    "click_element",
    "type_text",
    "press_key",
    "scroll",
    "scroll_element",
    "drag",
    "set_value",
    "perform_secondary_action",
    "activate_window",
    "launch_app",
    "start_audio_recording",
];

pub fn is_void(name: &str) -> bool {
    VOID_TOOLS.contains(&name)
}

pub fn skip_approval(name: &str) -> bool {
    matches!(
        name,
        "list_windows" | "list_apps" | "end_turn" | "batch_actions" | "session_note" | "session_state" | "health" | "tools" | "prompt" | "diagnostic_state"
    )
}

/// The official model-facing window2 surface: exactly the 13 methods on
/// `Window2ComputerUseClient` (`@oai/sky` `types/window2/Window2ComputerUseClient.d.ts`).
/// `click_element` / `scroll_element` exist on the helper wire but are routed by
/// `click` / `scroll`; they are never advertised as tools.
pub const WINDOW2_CORE: &[&str] = &[
    "list_windows",
    "get_window",
    "list_apps",
    "launch_app",
    "get_window_state",
    "click",
    "press_key",
    "type_text",
    "scroll",
    "set_value",
    "drag",
    "perform_secondary_action",
    "activate_window",
];

/// `tools` RPC payload. Browser / mac catalogs stay Python (`deferred`).
///
/// Surfaces:
/// - `computer`: the official 13-method model surface, exactly.
/// - `gated`: converges to the same official 13 (browser gating happens in the
///   sidecar); harness extras must be requested as `harness`.
/// - `harness`: the DSH extensions only (the five harness helpers, the
///   `SKY_ENABLE_AUDIO=1` audio methods, and `scroll_element`).
/// - `desktop`: the whole native non-browser catalogue (13 + harness + audio).
/// - `all`: the whole native catalogue plus `deferred: python` so the sidecar
///   merges the browser/mac catalogs.
/// - anything else: default to the official 13. The old `_ =>` branch returned
///   the full 18-entry table, so any unknown surface leaked DSH-only tools.
pub fn tools_for_surface(surface: &str) -> Value {
    let surface = surface.trim();
    match surface {
        "browser" | "mac" => json!({
            "tools": [],
            "surface": surface,
            "deferred": "python",
        }),
        "computer" | "gated" => json!({ "tools": window2_core_tools(), "surface": surface }),
        "harness" => json!({ "tools": harness_tools(), "surface": surface }),
        "desktop" => json!({ "tools": window2_tools(), "surface": surface }),
        "all" => json!({
            "tools": window2_tools(),
            "surface": surface,
            "deferred": "python",
        }),
        _ => json!({ "tools": window2_core_tools(), "surface": surface, "unknownSurface": true }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(surface: &str) -> Vec<String> {
        tools_for_surface(surface)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    #[test]
    fn browser_and_mac_defer_python() {
        for surface in ["browser", "mac"] {
            let payload = tools_for_surface(surface);
            assert_eq!(payload["tools"].as_array().unwrap().len(), 0);
            assert_eq!(payload["deferred"], "python");
            assert_eq!(payload["surface"], surface);
        }
    }

    #[test]
    fn computer_is_exactly_the_official_thirteen() {
        let listed = names("computer");
        assert_eq!(listed.len(), 13, "computer surface must be the official 13 methods: {listed:?}");
        for name in WINDOW2_CORE {
            assert!(listed.contains(&(*name).to_string()), "missing {name}");
        }
        for absent in ["click_element", "scroll_element", "batch_actions", "session_note"] {
            assert!(!listed.contains(&absent.to_string()), "{absent} must not be advertised");
        }
        assert!(!listed.iter().any(|name| name.starts_with("tab_") || name.starts_with("browser_")));
    }

    #[test]
    fn gated_converges_to_the_official_thirteen() {
        // TC-01: gated used to fall through to the full 18-entry table. It now
        // returns exactly the official 13; harness extras live on "harness".
        let listed = names("gated");
        assert_eq!(listed, names("computer"), "gated must equal the official 13");
        for absent in ["batch_actions", "session_note", "session_state", "end_turn", "diagnostic_state", "scroll_element", "click_element"] {
            assert!(!listed.contains(&absent.to_string()), "{absent} must not be on gated");
        }
        assert!(!listed.iter().any(|name| name.starts_with("tab_")));
    }

    #[test]
    fn unknown_surface_defaults_to_the_official_thirteen() {
        // The old `_ =>` arm returned window2_tools() (18) for every unknown
        // surface, so a typo leaked DSH-only tools into the model catalog.
        for surface in ["", "whatever", "desktop-typo"] {
            let listed = names(surface);
            assert_eq!(listed, names("computer"), "surface {surface:?} must default to the 13");
            assert!(!listed.contains(&"batch_actions".into()));
            assert!(!listed.contains(&"diagnostic_state".into()));
            assert!(!listed.contains(&"scroll_element".into()));
        }
    }

    #[test]
    fn harness_surface_lists_exactly_the_dsh_extensions() {
        let listed = names("harness");
        for required in ["batch_actions", "session_note", "session_state", "end_turn", "diagnostic_state", "scroll_element"] {
            assert!(listed.contains(&required.to_string()), "harness missing {required}");
        }
        // No official core method and no internal wire route leaks in.
        for absent in ["get_window_state", "click", "press_key", "click_element", "list_windows"] {
            assert!(!listed.contains(&absent.to_string()), "harness must not carry {absent}");
        }
    }

    #[test]
    fn scroll_schema_is_the_official_six_fields() {
        // TC-02: the official ScrollInput is {window,x,y,screenshotId,scrollX,scrollY}
        // and guidance.md:222 forbids element_index on scroll.
        let payload = tools_for_surface("computer");
        let scroll = payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "scroll")
            .expect("scroll tool");
        let props: Vec<&str> = scroll["parameters"]["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(props, vec!["screenshotId", "scrollX", "scrollY", "window", "x", "y"]);
        for forbidden in ["element_index", "direction", "pages"] {
            assert!(scroll["parameters"]["properties"].get(forbidden).is_none(), "scroll must not advertise {forbidden}");
        }
        let required: Vec<&str> = scroll["parameters"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(required, vec!["window", "x", "y", "scrollX", "scrollY"]);
    }

    #[test]
    fn get_window_state_schema_is_the_official_three_fields() {
        // TC-04: disableDiffing was a browser-surface invention.
        let payload = tools_for_surface("computer");
        let state = payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "get_window_state")
            .expect("get_window_state tool");
        let props: Vec<&str> = state["parameters"]["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(props, vec!["include_screenshot", "include_text", "window"]);
        assert!(state["parameters"]["properties"].get("disableDiffing").is_none());
    }

    #[test]
    fn scroll_element_is_only_on_the_harness_surface() {
        for surface in ["computer", "gated", "all"] {
            assert!(!names(surface).contains(&"scroll_element".to_string()), "{surface} must not advertise scroll_element");
        }
        assert!(names("harness").contains(&"scroll_element".to_string()));
    }

    #[test]
    fn all_keeps_window2_and_defers_python() {
        let payload = tools_for_surface("all");
        assert_eq!(payload["deferred"], "python");
        let listed = names("all");
        assert!(listed.contains(&"list_windows".into()));
        assert!(listed.contains(&"end_turn".into()));
    }

    #[test]
    fn desktop_keeps_the_full_native_catalogue() {
        let payload = tools_for_surface("desktop");
        assert!(payload.get("deferred").is_none(), "desktop must not force the python merge");
        let listed = names("desktop");
        assert!(listed.contains(&"list_windows".into()));
        assert!(listed.contains(&"batch_actions".into()));
    }
}
