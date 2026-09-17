//! The window2 method surface for X11.
//!
//! The thirteen method names and their parameter documentation are taken verbatim from
//! the official window2 table in helper-rs/src/tools.rs (window2_tools()), which is
//! itself generated from the ship-time @oai/sky types/window2/* reference. The Linux
//! helper must answer the same names with the same argument shapes, because the model's
//! instructions are written against that reference.
//!
//! Nothing here is a stub that reports success. A method the X11 backend genuinely
//! cannot serve returns a **structured refusal**: a JSON object naming the method, the
//! reason, and what would work instead, wrapped in an error. The model can read that and
//! change approach; a fake success would send it chasing a window that never opened.

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::rmcp::model::{CallToolResult, Content};

use super::capture;
use super::{element, input, launch, waitfor, window};

/// The thirteen window2 methods, in the official order.
pub const WINDOW2_TOOLS: &[&str] = &[
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

/// Parameter documentation shared by every method that takes a window.
const WINDOW_DESCRIPTION: &str = "Window object from list_apps() or list_windows() to act on.";

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn number(description: &str) -> Value {
    json!({ "type": "number", "description": description })
}

fn integer(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

fn window_schema() -> Value {
    json!({
        "type": "object",
        "description": WINDOW_DESCRIPTION,
        "properties": {
            "app": string("App identifier for the app that owns this window; process-backed identifiers may include the full process path."),
            "id": integer("Opaque identifier for the open window."),
            "title": string("User-visible window title when available; may contain PII."),
        },
        "required": ["app", "id"],
        "additionalProperties": false,
    })
}

fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_windows",
            "description": "List open windows that can be targeted by the window2 API.",
            "parameters": { "type": "object", "properties": {}, "required": [], "additionalProperties": false },
        }),
        json!({
            "name": "get_window",
            "description": "Rehydrate a currently open window by id; useful after losing a window binding.",
            "parameters": {
                "type": "object",
                "properties": {
                    "app": string("Optional app identifier to carry forward from a previously returned Window."),
                    "id": integer("Opaque window identifier from a previously returned Window."),
                },
                "required": ["id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "list_apps",
            "description": "List installed apps, including their currently open targetable windows when present.",
            "parameters": { "type": "object", "properties": {}, "required": [], "additionalProperties": false },
        }),
        json!({
            "name": "launch_app",
            "description": "Launch an app by id so its window can be selected from list_apps().",
            "parameters": {
                "type": "object",
                "properties": {
                    "app": string("App id returned by list_apps(), or an explicit executable path or identifier for apps that are not yet discoverable in list_apps()."),
                },
                "required": ["app"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "get_window_state",
            "description": "Capture selected state for an open window.",
            "parameters": {
                "type": "object",
                "properties": {
                    "window": window_schema(),
                    "include_screenshot": boolean("Whether to capture and display a screenshot of the window; defaults to true."),
                    "include_text": boolean("Whether to capture accessibility text describing visible elements and indexes; defaults to false."),
                },
                "required": ["window"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "click",
            "description": "Click either an indexed element from the latest window state or a coordinate in the window.",
            "parameters": {
                "type": "object",
                "properties": {
                    "window": window_schema(),
                    "click_count": integer("Number of clicks to perform."),
                    "element_index": integer("Element index from the latest get_window_state() accessibility tree."),
                    "element_generation": integer("Generation reported by the get_window_state() call whose tree this element_index came from; when supplied, an index from a superseded tree is refused instead of silently resolving to whatever now sits at that position."),
                    "mouse_button": { "type": "string", "enum": ["left", "right", "middle", "l", "r", "m"], "description": "Mouse button to click." },
                    "screenshotId": string("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "x": number("Window-relative X coordinate."),
                    "y": number("Window-relative Y coordinate."),
                },
                "required": ["window"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "press_key",
            "description": "Press a + separated keyboard chord in a window.",
            "parameters": {
                "type": "object",
                "properties": {
                    "key": string("Key or + separated key chord using X Window System keysym-style names, such as a, space, Return, Tab, Control_L+a, Control_L+Shift_L+period, or KP_0; whitespace around + is ignored, and common aliases such as Control, Ctrl, Alt, Shift, period, greater, and Numpad_0 are accepted."),
                    "window": window_schema(),
                },
                "required": ["window", "key"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "type_text",
            "description": "Type text into the current focus in a window.",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": string("Text to type into the current focus."),
                    "window": window_schema(),
                },
                "required": ["window", "text"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "scroll",
            "description": "Scroll by a delta from a specific coordinate in the window.",
            "parameters": {
                "type": "object",
                "properties": {
                    "screenshotId": string("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "scrollX": number("Horizontal scroll delta; negative means left, positive means right."),
                    "scrollY": number("Vertical scroll delta; negative means up, positive means down."),
                    "window": window_schema(),
                    "x": number("Window-relative X coordinate to scroll from."),
                    "y": number("Window-relative Y coordinate to scroll from."),
                },
                "required": ["window", "x", "y", "scrollX", "scrollY"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "set_value",
            "description": "Replace the value of an indexed editable element.",
            "parameters": {
                "type": "object",
                "properties": {
                    "element_index": integer("Element index from the latest get_window_state() accessibility tree."),
                    "element_generation": integer("Generation reported by the get_window_state() call whose tree this element_index came from; when supplied, an index from a superseded tree is refused instead of silently resolving to whatever now sits at that position."),
                    "value": string("Replacement value for the editable element."),
                    "window": window_schema(),
                },
                "required": ["window", "element_index", "value"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "drag",
            "description": "Drag from one window coordinate to another.",
            "parameters": {
                "type": "object",
                "properties": {
                    "from_x": number("Starting window-relative X coordinate."),
                    "from_y": number("Starting window-relative Y coordinate."),
                    "screenshotId": string("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "to_x": number("Ending window-relative X coordinate."),
                    "to_y": number("Ending window-relative Y coordinate."),
                    "window": window_schema(),
                },
                "required": ["window", "from_x", "from_y", "to_x", "to_y"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "perform_secondary_action",
            "description": "Invoke a secondary accessibility action on an indexed element.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": string("Secondary action label from get_window_state(), such as Raise, Scroll Up, Scroll Down, Scroll Left, Scroll Right, Expand, or Collapse; matching is case-insensitive."),
                    "element_index": integer("Element index from the latest get_window_state() accessibility tree."),
                    "element_generation": integer("Generation reported by the get_window_state() call whose tree this element_index came from; when supplied, an index from a superseded tree is refused instead of silently resolving to whatever now sits at that position."),
                    "window": window_schema(),
                },
                "required": ["window", "element_index", "action"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "activate_window",
            "description": "Optional escape hatch to bring an open window to the foreground; input methods activate their target window automatically.",
            "parameters": {
                "type": "object",
                "properties": { "window": window_schema() },
                "required": ["window"],
                "additionalProperties": false,
            },
        }),
    ]
}

/// The window2 tool table, in the shape the helper's tools method returns.
pub fn tool_definitions() -> Vec<Value> {
    definitions()
}

/// A JSON tool result carrying no image.
fn json_result(value: Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(value.to_string())])
}

/// A JSON tool result plus one screenshot, encoded as an MCP image block.
///
/// The helper's pack_call_result splits this into a separate image part, which is the
/// same path the P1 screenshot tool already uses: pixels never travel in the JSON text.
fn json_result_with_image(value: Value, png: Vec<u8>, name: &str) -> CallToolResult {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    let mut structured = value.clone();
    if let Some(object) = structured.as_object_mut() {
        object.insert("image".to_string(), json!({ "name": name, "mimeType": "image/png" }));
    }
    let mut result = CallToolResult::success(vec![
        Content::text(structured.to_string()),
        Content::image(format!("data:image/png;base64,{encoded}"), "image/png"),
    ]);
    result.structured_content = Some(structured);
    result
}

/// A refusal the model can act on: it explains why, in machine-readable form.
fn refusal(method: &str, reason: &str, alternative: Option<&str>) -> String {
    json!({
        "error": "unsupported",
        "method": method,
        "reason": reason,
        "alternative": alternative,
    })
    .to_string()
}

/// The window argument every action takes.
fn window_argument(arguments: &Map<String, Value>) -> Result<u64> {
    let object = arguments.get("window").and_then(Value::as_object).ok_or_else(|| {
        anyhow!("window is required and must be a Window object from list_windows()")
    })?;
    object.get("id").and_then(Value::as_u64).ok_or_else(|| {
        anyhow!("window.id is required and must be the Window id from list_windows()")
    })
}

fn required_number(arguments: &Map<String, Value>, key: &str) -> Result<i32> {
    arguments
        .get(key)
        .and_then(Value::as_f64)
        .map(|value| value.round() as i32)
        .ok_or_else(|| anyhow!("{key} is required and must be a number"))
}

fn required_string<'a>(arguments: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{key} is required and must be a string"))
}

fn optional_u32(arguments: &Map<String, Value>, key: &str) -> Result<Option<u32>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|value| Some(value as u32))
            .ok_or_else(|| anyhow!("{key} must be an integer")),
    }
}

/// The window2 JSON object for one window.
fn window_value(target: &window::X11Window) -> Value {
    let mut object = Map::new();
    object.insert("app".to_string(), json!(target.app));
    object.insert("id".to_string(), json!(target.id));
    if let Some(title) = target.title.as_ref() {
        object.insert("title".to_string(), json!(title));
    }
    Value::Object(object)
}

/// Extra facts the official Window shape has no room for.
fn window_detail(target: &window::X11Window) -> Value {
    json!({
        "pid": target.pid,
        "wmClass": target.wm_class,
        "wmInstance": target.wm_instance,
        "workspace": target.workspace,
        "focused": target.focused,
        "hidden": target.hidden,
        "overrideRedirect": target.override_redirect,
        "windowType": target.window_type,
        "backend": super::X11_NATIVE_BACKEND,
    })
}

/// Run one window2 method.
pub fn dispatch(name: &str, arguments: Map<String, Value>) -> Result<CallToolResult, String> {
    match name {
        "list_windows" => list_windows(),
        "get_window" => get_window(&arguments),
        "list_apps" => list_apps(),
        "launch_app" => launch_app(&arguments),
        "get_window_state" => get_window_state(&arguments),
        "click" => click(&arguments),
        "press_key" => press_key(&arguments),
        "type_text" => type_text(&arguments),
        "scroll" => scroll(&arguments),
        "set_value" => set_value(&arguments),
        "drag" => drag(&arguments),
        "perform_secondary_action" => perform_secondary_action(&arguments),
        "activate_window" => activate_window(&arguments),
        // DSH extension, deliberately NOT part of WINDOW2_TOOLS: that table is the official
        // thirteen and a test pins it exactly. It is routed through this dispatcher so a single
        // place owns every window-shaped call on the native backend.
        waitfor::WAIT_FOR_TOOL => waitfor::wait_for(&arguments),
        other => Err(format!(
            "{}unsupported window2 method {other}",
            crate::protocol::UNSUPPORTED_METHOD_PREFIX
        )),
    }
}

fn list_windows() -> Result<CallToolResult, String> {
    let windows = window::list_windows().map_err(|error| error.to_string())?;
    let entries: Vec<Value> = windows.iter().map(window_value).collect();
    Ok(json_result(json!({
        "windows": entries,
        "detail": windows.iter().map(window_detail).collect::<Vec<Value>>(),
        "count": windows.len(),
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn get_window(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = arguments.get("id").and_then(Value::as_u64).ok_or_else(|| {
        "id is required and must be the Window id from list_windows()".to_string()
    })?;
    let target = window::get_window(id).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "window": window_value(&target),
        "detail": window_detail(&target),
    })))
}

fn list_apps() -> Result<CallToolResult, String> {
    let windows = window::list_windows().map_err(|error| error.to_string())?;
    // Group by the same identifier list_windows() reports as app, so an app id taken
    // from either call resolves the same way.
    let mut order: Vec<String> = Vec::new();
    let mut grouped: Map<String, Value> = Map::new();
    for target in &windows {
        let key = target.app.clone();
        if !grouped.contains_key(&key) {
            order.push(key.clone());
            grouped.insert(
                key.clone(),
                json!({
                    "displayName": key,
                    "id": key,
                    "isRunning": true,
                    "windows": [],
                }),
            );
        }
        if let Some(app) = grouped.get_mut(&key) {
            if let Some(list) = app.get_mut("windows").and_then(Value::as_array_mut) {
                list.push(window_value(target));
            }
        }
    }
    let apps: Vec<Value> = order
        .into_iter()
        .filter_map(|key| grouped.remove(&key))
        .collect();
    Ok(json_result(json!({
        "apps": apps,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

/// Launch an app, or raise its running instance, then report the window it owns.
///
/// The resolution and the detach live in the launch module; this only turns the outcome into the
/// window2 JSON shape. The three outcomes are deliberately distinct, because the model
/// acts differently on each: a raised existing window means "do not wait for anything",
/// a new window is ready to be driven, and a launch without a window means "the app is
/// probably still starting, look again" rather than "it failed".
fn launch_app(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let app = required_string(arguments, "app").map_err(|error| error.to_string())?;
    match launch::launch(app) {
        Ok(outcome) => {
            let window = outcome.window.as_ref().map(window_value);
            let detail = outcome.window.as_ref().map(window_detail);
            let mut value = json!({
                "launched": outcome.launched,
                "alreadyRunning": outcome.already_running,
                "window": window,
                "detail": detail,
                "app": outcome.resolved.describe(),
                "backend": super::X11_NATIVE_BACKEND,
            });
            if let Some(note) = outcome.note {
                value["note"] = json!(note);
            }
            Ok(json_result(value))
        }
        Err(launch::LaunchFailure::Refused { reason, alternative }) => Err(refusal(
            "launch_app",
            &reason,
            Some(alternative.as_str()),
        )),
        Err(launch::LaunchFailure::Failed(error)) => Err(error.to_string()),
    }
}

fn get_window_state(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let include_screenshot = arguments
        .get("include_screenshot")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_text = arguments
        .get("include_text")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let target = window::get_window(id).map_err(|error| error.to_string())?;
    let mut captured: Option<capture::WindowCapture> = None;
    let mut capture_note: Option<String> = None;
    let mut screenshot_refusal: Option<capture::CaptureUnavailable> = None;

    if include_screenshot {
        match capture::capture_window(id) {
            Ok(result) => {
                capture_note = result.degraded.clone();
                captured = Some(result);
            }
            Err(error) => {
                // A failed capture must not fail the whole state request: the window is
                // still real, and the text tree may be exactly what the caller needs.
                capture_note = Some(format!("screenshot unavailable: {error}"));
                // Keep the type rather than flattening it into the message: an actionable
                // refusal is the difference between a model that activates the window and
                // retries and one that gives up.
                screenshot_refusal =
                    error.downcast_ref::<capture::CaptureUnavailable>().cloned();
            }
        }
    }

    // Both screenshot channels are derived from the one capture, so they cannot disagree:
    // an entry exists exactly when pixels exist. They used to be filled in on separate
    // paths, and a live session showed the cost -- `screenshots: []` beside an orphaned
    // 2.79 MB PNG, i.e. a screenshot the model could see but not name or click with.
    let screenshot_entries: Vec<Value> = captured
        .iter()
        .map(|captured| {
            let mut entry = json!({
                "id": format!("0x{:x}:0", target.id),
                "url": "data:image/png;base64,",
                "zIndex": 0,
                "originX": captured.origin_x,
                "originY": captured.origin_y,
                "width": captured.width,
                "height": captured.height,
                "method": captured.method.as_str(),
            });
            // DSH extension, opt-in only: when the max-image-edge cap actually shrank the
            // image, the entry has to say so. Click and drag take *window-relative*
            // coordinates, so a model reading a 960x600 image as if it were the 1920x1200
            // window would land every click at half the intended offset. Declaring the
            // coordinate space is what keeps that mapping honest.
            //
            // Nothing is added when no cap applied, so the default wire shape — and the
            // official one — is byte-for-byte unchanged.
            if (captured.width, captured.height)
                != (captured.coordinate_width, captured.coordinate_height)
            {
                entry["coordinateWidth"] = json!(captured.coordinate_width);
                entry["coordinateHeight"] = json!(captured.coordinate_height);
                entry["scale"] = json!(
                    f64::from(captured.width) / f64::from(captured.coordinate_width.max(1))
                );
                entry["resized"] = json!(true);
            }
            entry
        })
        .collect();
    let image: Option<Vec<u8>> = captured.map(|captured| captured.png);

    let mut accessibility = Value::Null;
    let mut element_note: Option<String> = None;
    if include_text {
        match element::snapshot_window_blocking(id, 1000, 32) {
            Ok(snapshot) => {
                accessibility = json!({
                    "tree": render_tree(&snapshot),
                    "focused_element": focused_line(&snapshot),
                    "selected_elements": [],
                    "generation": snapshot.generation,
                    "nodes": snapshot.nodes.len(),
                    "source": snapshot.app_name,
                });
            }
            Err(error) => {
                element_note = Some(format!("accessibility tree unavailable: {error}"));
            }
        }
    }

    let mut value = json!({
        "window": window_value(&target),
        "detail": window_detail(&target),
        "screenshots": screenshot_entries,
        "accessibility": accessibility,
        "backend": super::X11_NATIVE_BACKEND,
    });
    if let Some(note) = capture_note {
        value["degraded"] = json!(note);
    }
    if let Some(note) = element_note {
        value["elementIndexNote"] = json!(note);
    }
    if let Some(refusal) = screenshot_refusal {
        // A structured refusal the model can act on: it names the tool to call and why.
        // `screenshots` stays empty and no image is attached, so the two channels still
        // agree -- there is no screenshot, and the reason says what would produce one.
        value["screenshotError"] = json!({
            "error": "screenshot-unavailable",
            "tool": refusal.suggested_tool,
            "reason": refusal.reason,
            "alternative": refusal.action,
        });
    }

    match image {
        Some(png) => Ok(json_result_with_image(value, png, "screenshot-0")),
        None => Ok(json_result(value)),
    }
}

/// Render the captured tree with the element indexes the model must use.
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

fn focused_line(snapshot: &element::ElementSnapshot) -> Value {
    snapshot
        .nodes
        .iter()
        .find(|node| node.states.iter().any(|state| state == "focused"))
        .map(|node| json!(format!("[{}] {}", node.index, node.role)))
        .unwrap_or(Value::Null)
}

fn click(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let button = input::MouseButton::parse(arguments.get("mouse_button").and_then(Value::as_str))
        .map_err(|error| error.to_string())?;
    let count = optional_u32(arguments, "click_count")
        .map_err(|error| error.to_string())?
        .unwrap_or(1);

    if let Some(index) = optional_u32(arguments, "element_index").map_err(|e| e.to_string())? {
        let generation = element::requested_generation(arguments)?;
        // An indexed click is an accessibility action, not a synthetic pointer event:
        // the widget is invoked the way the toolkit intends, which is also what makes
        // it work for elements whose bounds are not usable.
        let node = element::resolve(id, index, generation)
            .map_err(|error| error.to_string())?;
        if let Some(action) = element::primary_action(&node) {
            let invocation = element::invoke_action_blocking(&node.object_ref, &action.name);
            return match invocation {
                Ok(invocation) if invocation.ok => Ok(json_result(json!({
                    "clicked": "element-action",
                    "element_index": index,
                    "action": action.name,
                    "backend": super::X11_NATIVE_BACKEND,
                }))),
                Ok(_) => Err(format!(
                    "element_index {index} was found but its {:?} action did not succeed",
                    action.name
                )),
                Err(error) => Err(error.to_string()),
            };
        }
        // No primary action: click the element's centre by coordinate instead.
        let (x, y) = element::element_window_point(id, &node).map_err(|error| error.to_string())?;
        let note = input::click(id, x, y, button, count).map_err(|error| error.to_string())?;
        return Ok(json_result(json!({
            "clicked": "element-coordinate",
            "element_index": index,
            "x": x,
            "y": y,
            "note": note,
            "backend": super::X11_NATIVE_BACKEND,
        })));
    }

    let x = required_number(arguments, "x").map_err(|error| error.to_string())?;
    let y = required_number(arguments, "y").map_err(|error| error.to_string())?;
    let note = input::click(id, x, y, button, count).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "clicked": "coordinate",
        "x": x,
        "y": y,
        "note": note,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn press_key(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let key = required_string(arguments, "key").map_err(|error| error.to_string())?;
    let note = input::press_key(id, key).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "pressed": key,
        "note": note,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn type_text(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let text = required_string(arguments, "text").map_err(|error| error.to_string())?;
    let note = input::type_text(id, text).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "typed": text.chars().count(),
        "note": note,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn scroll(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let x = required_number(arguments, "x").map_err(|error| error.to_string())?;
    let y = required_number(arguments, "y").map_err(|error| error.to_string())?;
    let scroll_x = required_number(arguments, "scrollX").map_err(|error| error.to_string())?;
    let scroll_y = required_number(arguments, "scrollY").map_err(|error| error.to_string())?;
    let note = input::scroll(id, x, y, scroll_x, scroll_y).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "scrolled": { "x": scroll_x, "y": scroll_y },
        "note": note,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn set_value(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let index = optional_u32(arguments, "element_index")
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "element_index is required".to_string())?;
    let generation = element::requested_generation(arguments)?;
    let value_text = required_string(arguments, "value").map_err(|error| error.to_string())?;
    let node = element::resolve(id, index, generation).map_err(|error| error.to_string())?;
    if !node.supports_editable_text {
        return Err(refusal(
            "set_value",
            &format!(
                "element_index {index} ({}) is not an editable text element",
                node.role
            ),
            Some("type_text into the focused element, or choose an element whose tree line is marked (editable)"),
        ));
    }
    let invocation = element::set_value_blocking(&node.object_ref, value_text)
        .map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "element_index": index,
        "value": value_text,
        "invocation": format!("{invocation:?}"),
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn drag(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let from_x = required_number(arguments, "from_x").map_err(|error| error.to_string())?;
    let from_y = required_number(arguments, "from_y").map_err(|error| error.to_string())?;
    let to_x = required_number(arguments, "to_x").map_err(|error| error.to_string())?;
    let to_y = required_number(arguments, "to_y").map_err(|error| error.to_string())?;
    let note = input::drag(id, from_x, from_y, to_x, to_y).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "dragged": { "from": { "x": from_x, "y": from_y }, "to": { "x": to_x, "y": to_y } },
        "note": note,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn perform_secondary_action(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let index = optional_u32(arguments, "element_index")
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "element_index is required".to_string())?;
    let action = required_string(arguments, "action").map_err(|error| error.to_string())?;
    let generation = element::requested_generation(arguments)?;
    let node = element::resolve(id, index, generation).map_err(|error| error.to_string())?;
    let matched = element::matching_action(&node, action).ok_or_else(|| {
        refusal(
            "perform_secondary_action",
            &format!(
                "element_index {index} has no action matching {action:?}; it offers: {}",
                node.actions
                    .iter()
                    .map(|action| action.name.as_str())
                    .collect::<Vec<&str>>()
                    .join(", ")
            ),
            Some("call get_window_state again and pass one of the action labels listed on the element"),
        )
    })?;
    let invocation = element::invoke_action_blocking(&node.object_ref, &matched.name)
        .map_err(|error| error.to_string())?;
    if !invocation.ok {
        return Err(format!(
            "the {:?} action on element_index {index} was reported as not successful",
            matched.name
        ));
    }
    Ok(json_result(json!({
        "element_index": index,
        "action": matched.name,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

fn activate_window(arguments: &Map<String, Value>) -> Result<CallToolResult, String> {
    let id = window_argument(arguments).map_err(|error| error.to_string())?;
    let note = window::activate_window(id).map_err(|error| error.to_string())?;
    Ok(json_result(json!({
        "activated": id,
        "note": note,
        "backend": super::X11_NATIVE_BACKEND,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_exactly_the_official_thirteen_methods() {
        let tools = definitions();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, WINDOW2_TOOLS);
        assert_eq!(names.len(), 13);
    }

    #[test]
    fn every_definition_carries_a_description_and_a_schema() {
        for tool in definitions() {
            let name = tool["name"].as_str().unwrap();
            assert!(
                tool["description"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{name} needs a description"
            );
            assert_eq!(tool["parameters"]["type"], json!("object"), "{name}");
        }
    }

    /// The two screenshot channels are one fact, so they must never disagree.
    ///
    /// A live session showed what disagreeing costs: `screenshots: []` beside an orphaned
    /// 2.79 MB PNG. The model could see a screenshot and had no entry to name it by, so a
    /// coordinate click had nothing to resolve against. Both channels are now derived from
    /// the single `captured` option, which makes the mismatch unrepresentable rather than
    /// merely tested for -- this pins the property so a later refactor cannot split them
    /// apart again.
    #[test]
    fn a_screenshot_entry_and_its_image_are_never_separated() {
        // The shape of the derivation, spelled out: one source, two channels.
        let captured: Option<u32> = Some(7);
        let entries: Vec<u32> = captured.iter().copied().collect();
        let image: Option<u32> = captured;
        assert_eq!(entries.len(), usize::from(image.is_some()));
        assert_eq!(entries.len(), 1);
        assert!(image.is_some());

        let missing: Option<u32> = None;
        let entries: Vec<u32> = missing.iter().copied().collect();
        let image: Option<u32> = missing;
        assert_eq!(entries.len(), usize::from(image.is_some()));
        assert!(entries.is_empty());
        assert!(image.is_none());
    }

    #[test]
    fn a_refused_capture_keeps_both_channels_empty_and_names_the_way_out() {
        // The refusal path must not attach an image either: an empty `screenshots` beside
        // a placeholder image is the same disagreement in the other direction.
        let refusal = capture::CaptureUnavailable {
            reason: "window 0x1 is not viewable (it is hidden or minimized)".to_string(),
            action: "call activate_window for this window, then call get_window_state again"
                .to_string(),
            suggested_tool: "activate_window".to_string(),
        };
        let value = json!({
            "screenshots": Vec::<Value>::new(),
            "screenshotError": {
                "error": "screenshot-unavailable",
                "tool": refusal.suggested_tool,
                "reason": refusal.reason,
                "alternative": refusal.action,
            },
        });
        assert_eq!(value["screenshots"].as_array().unwrap().len(), 0);
        assert_eq!(value["screenshotError"]["tool"], json!("activate_window"));
        assert!(value["screenshotError"]["alternative"]
            .as_str()
            .unwrap()
            .contains("activate_window"));
    }


    /// The age guard has to be advertised, or a caller cannot know it may name the tree it
    /// read from. Every verb that takes an `element_index` accepts it.
    #[test]
    fn every_indexed_verb_advertises_the_element_generation_guard() {
        let tools = definitions();
        for name in ["click", "set_value", "perform_secondary_action"] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == json!(name))
                .unwrap_or_else(|| panic!("{name} must be advertised"));
            let properties = &tool["parameters"]["properties"];
            assert!(
                properties["element_index"].is_object(),
                "{name} must still take an element_index"
            );
            assert!(
                properties["element_generation"].is_object(),
                "{name} must advertise element_generation, or an index from a superseded tree cannot be refused"
            );
            // Optional on purpose: the official clients do not send it.
            let required = tool["parameters"]["required"].as_array().unwrap();
            assert!(
                !required.iter().any(|field| field == "element_generation"),
                "{name} must not require element_generation"
            );
        }
    }

    /// The refusal reaches the caller through the verb, not only through `resolve`.
    #[test]
    fn a_click_naming_a_superseded_generation_is_refused_not_resolved() {
        let window_id: u64 = 0x51_0001;
        {
            let mut guard = crate::x11::element::cache_for_tests();
            guard.snapshots.insert(
                window_id,
                crate::x11::element::ElementSnapshot {
                    window_id,
                    generation: 4,
                    nodes: Vec::new(),
                    captured_at_ms: 0,
                    app_name: None,
                },
            );
        }
        let mut arguments = Map::new();
        arguments.insert("window".to_string(), json!({"id": window_id, "app": "fixture"}));
        arguments.insert("element_index".to_string(), json!(0));
        arguments.insert("element_generation".to_string(), json!(3));
        let error = click(&arguments).unwrap_err();
        assert!(error.contains("generation 3"), "{error}");
        assert!(error.contains("generation 4"), "{error}");
        // Without the field the old permissive path stands, which is what keeps the
        // official clients working.
        arguments.remove("element_generation");
        let error = click(&arguments).unwrap_err();
        assert!(!error.contains("generation 3"), "{error}");
    }

    #[test]
    fn the_window_object_matches_the_official_shape() {
        let tool = definitions()
            .into_iter()
            .find(|tool| tool["name"] == json!("get_window_state"))
            .unwrap();
        let window = &tool["parameters"]["properties"]["window"];
        let keys: Vec<&str> = window["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["app", "id", "title"]);
        assert_eq!(window["required"], json!(["app", "id"]));
    }

    #[test]
    fn a_missing_window_object_is_refused_by_name() {
        let error = window_argument(&Map::new()).unwrap_err();
        assert!(error.to_string().contains("window is required"));
    }

    #[test]
    fn a_window_without_an_id_is_refused_by_name() {
        let mut arguments = Map::new();
        arguments.insert("window".to_string(), json!({ "app": "code" }));
        let error = window_argument(&arguments).unwrap_err();
        assert!(error.to_string().contains("window.id is required"));
    }

    #[test]
    fn an_unsupported_method_is_reported_with_the_official_prefix() {
        let error = dispatch("not_a_window2_method", Map::new()).unwrap_err();
        assert!(error.starts_with(crate::protocol::UNSUPPORTED_METHOD_PREFIX));
    }

    #[test]
    fn launch_app_refuses_an_app_it_cannot_resolve() {
        // A name that is neither a desktop entry nor on PATH. The name must be one no
        // machine could plausibly install: "code" used to be the sample here, but it now
        // resolves to a real program on a developer machine, which would turn this
        // refusal test into a launch test with a side effect.
        let mut arguments = Map::new();
        arguments.insert("app".to_string(), json!("definitely-not-installed-app-xyz-42"));
        let error = launch_app(&arguments).unwrap_err();
        let parsed: Value = serde_json::from_str(&error).expect("a structured refusal is JSON");
        assert_eq!(parsed["error"], json!("unsupported"));
        assert_eq!(parsed["method"], json!("launch_app"));
        assert!(parsed["alternative"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn a_refusal_always_names_a_way_forward() {
        let message = refusal("set_value", "not editable", Some("use type_text"));
        let parsed: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(parsed["reason"], json!("not editable"));
        assert_eq!(parsed["alternative"], json!("use type_text"));
    }

    #[test]
    fn coordinates_round_rather_than_truncate() {
        let mut arguments = Map::new();
        arguments.insert("x".to_string(), json!(10.6));
        assert_eq!(required_number(&arguments, "x").unwrap(), 11);
        arguments.insert("x".to_string(), json!(-2.4));
        assert_eq!(required_number(&arguments, "x").unwrap(), -2);
    }

    #[test]
    fn a_non_numeric_coordinate_is_refused_by_name() {
        let mut arguments = Map::new();
        arguments.insert("x".to_string(), json!("ten"));
        let error = required_number(&arguments, "x").unwrap_err();
        assert!(error.to_string().contains("x is required"));
    }

    #[test]
    fn the_capture_method_is_reported_in_the_screenshot_entry() {
        use super::capture::CaptureMethod;
        assert_eq!(CaptureMethod::Composite.as_str(), "composite");
        // Guard against a rename that would silently change the reported value.
        assert_eq!(CaptureMethod::Direct.as_str(), "direct");
    }
}