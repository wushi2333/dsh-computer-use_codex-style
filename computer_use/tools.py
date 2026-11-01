from __future__ import annotations

from typing import Any

WINDOW2_TOOLS = (
    "list_windows",
    "get_window",
    "list_apps",
    "launch_app",
    "get_window_state",
    "click",
    "press_key",
    "type_text",
    "scroll",
    "scroll_element",
    "set_value",
    "drag",
    "perform_secondary_action",
    "activate_window",
)


def _obj(properties: dict[str, Any], required: list[str] | None = None) -> dict[str, Any]:
    schema: dict[str, Any] = {
        "type": "object",
        "properties": properties,
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    return schema


def _str(description: str) -> dict[str, Any]:
    return {"type": "string", "description": description}


def _num(description: str) -> dict[str, Any]:
    return {"type": "number", "description": description}


def _int(description: str) -> dict[str, Any]:
    return {"type": "integer", "description": description}


def _bool(description: str) -> dict[str, Any]:
    return {"type": "boolean", "description": description}


def _window() -> dict[str, Any]:
    return {
        "type": "object",
        "description": "Window object from list_apps() or list_windows().",
        "properties": {
            "app": _str("App identifier for the app that owns this window; process-backed identifiers may include the full process path."),
            "id": _int("Opaque identifier for the open window."),
            "title": _str("User-visible window title when available; may contain PII."),
        },
        "required": ["app", "id"],
        "additionalProperties": False,
    }


def _tool(name: str, description: str, parameters: dict[str, Any]) -> dict[str, Any]:
    return {
        "type": "function",
        "function": {"name": name, "description": description, "parameters": parameters},
    }


def tool_definitions() -> list[dict[str, Any]]:
    window = _window()
    return [
        _tool("list_windows", "List open windows that can be targeted by the window2 API.", _obj({})),
        _tool(
            "get_window",
            "Rehydrate a currently open window by id; useful after losing a window binding.",
            _obj({"app": _str("Optional app identifier to carry forward from a previously returned Window."), "id": _int("Opaque window identifier from a previously returned Window.")}, ["id"]),
        ),
        _tool(
            "list_apps",
            "List installed apps, including their currently open targetable windows when present.",
            _obj({}),
        ),
        _tool(
            "launch_app",
            "Launch an app by id so its window can be selected from list_apps().",
            _obj({"app": _str("App id returned by list_apps(), or an explicit .exe process path/identifier.")}, ["app"]),
        ),
        _tool(
            "get_window_state",
            "Capture selected state for an open window. Default: screenshot on, accessibility null. Set include_text true for the indexed accessibility tree.",
            _obj(
                {
                    "window": window,
                    "include_screenshot": _bool("Whether to capture and display a screenshot of the window; defaults to true."),
                    "include_text": _bool("Whether to capture accessibility text describing visible elements and indexes; defaults to false."),
                },
                ["window"],
            ),
        ),
        _tool(
            "click",
            "Click either an indexed element from the latest window state or a coordinate in the window.",
            _obj(
                {
                    "window": window,
                    "element_index": _int("Element index from the latest get_window_state() accessibility tree."),
                    "x": _num("Window-relative X coordinate."),
                    "y": _num("Window-relative Y coordinate."),
                    "screenshotId": _str("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                    "mouse_button": {"type": "string", "enum": ["left", "right", "middle", "l", "r", "m"], "description": "Mouse button to click."},
                    "click_count": _int("Number of clicks to perform."),
                },
                ["window"],
            ),
        ),
        _tool(
            "press_key",
            "Press a + separated keyboard chord in a window using X Window System keysym-style names, such as a, space, Return, Tab, Control_L+a, Control_L+Shift_L+period, or KP_0.",
            _obj({"window": window, "key": _str("Key or + separated key chord.")}, ["window", "key"]),
        ),
        _tool(
            "type_text",
            "Type text into the current focus in a window.",
            _obj({"window": window, "text": _str("Text to type into the current focus.")}, ["window", "text"]),
        ),
        _tool(
            "scroll",
            "Scroll by a delta from a specific coordinate in the window. Negative scrollY is up; negative scrollX is left. Do not pass element_index.",
            _obj(
                {
                    "window": window,
                    "x": _num("Window-relative X coordinate to scroll from."),
                    "y": _num("Window-relative Y coordinate to scroll from."),
                    "scrollX": _num("Horizontal scroll delta; negative means left, positive means right."),
                    "scrollY": _num("Vertical scroll delta; negative means up, positive means down."),
                    "screenshotId": _str("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                },
                ["window", "x", "y", "scrollX", "scrollY"],
            ),
        ),
        _tool(
            "scroll_element",
            "Scroll an indexed element by direction and page count (helper scroll_element).",
            _obj(
                {
                    "window": window,
                    "element_index": _int("Element index from the latest get_window_state() accessibility tree."),
                    "direction": {"type": "string", "enum": ["up", "down", "left", "right"], "description": "Scroll direction."},
                    "pages": _num("Number of pages to scroll; must be a finite number > 0."),
                },
                ["window", "element_index", "direction"],
            ),
        ),
        _tool(
            "set_value",
            "Replace the value of an indexed editable element.",
            _obj(
                {
                    "window": window,
                    "element_index": _int("Element index from the latest get_window_state() accessibility tree."),
                    "value": _str("Replacement value for the editable element."),
                },
                ["window", "element_index", "value"],
            ),
        ),
        _tool(
            "drag",
            "Drag from one window coordinate to another.",
            _obj(
                {
                    "window": window,
                    "from_x": _num("Starting window-relative X coordinate."),
                    "from_y": _num("Starting window-relative Y coordinate."),
                    "to_x": _num("Ending window-relative X coordinate."),
                    "to_y": _num("Ending window-relative Y coordinate."),
                    "screenshotId": _str("Optional screenshot id from get_window_state(); when supplied, it must be cached for the target window."),
                },
                ["window", "from_x", "from_y", "to_x", "to_y"],
            ),
        ),
        _tool(
            "perform_secondary_action",
            "Invoke a secondary accessibility action on an indexed element, such as Raise, Scroll Up, Scroll Down, Expand, or Collapse.",
            _obj(
                {
                    "window": window,
                    "element_index": _int("Element index from the latest get_window_state() accessibility tree."),
                    "action": _str("Secondary action label from get_window_state(); matching is case-insensitive."),
                },
                ["window", "element_index", "action"],
            ),
        ),
        _tool(
            "activate_window",
            "Optional escape hatch to bring an open window to the foreground; input methods activate their target window automatically.",
            _obj({"window": window}, ["window"]),
        ),
    ]


def tool_names() -> list[str]:
    return [item["function"]["name"] for item in tool_definitions()]


def harness_tool_definitions() -> list[dict[str, Any]]:
    """DSH harness extensions that exist nowhere in the official catalogue.

    Kept out of `tool_definitions()` so the official contract tests keep passing:
    `tool_names()` must stay exactly `WINDOW2_TOOLS`. `scroll_element` is the
    precedent — an extension that the *native helper* surface publishes
    separately (see `harness_only_tools()` in helper-rs/src/tools.rs).
    """
    window = _window()
    return [
        _tool(
            "fill_form",
            "Atomically fill several form fields in one call, then return the per-step results for a single final observation. Use this instead of separate set_value/click round-trips whenever a form has 2+ fields: it *replaces* values (select-all then write) so a stale value can never become \"old + new\", and it stops at the first failing step unless continue_on_error is set. Field actions: \"set\" (write value), \"check\" (tick a checkbox), \"click\" (press a button), \"press\" (a key, default Return).",
            _obj(
                {
                    "window": window,
                    "continue_on_error": {"type": "boolean", "description": "Keep going after a failing step instead of stopping (default false)."},
                    "fields": {
                        "type": "array",
                        "items": _obj(
                            {
                                "element_index": _int("Element index from the latest get_window_state() accessibility tree."),
                                "action": _str("One of: set, check, click, press (defaults to set when value is present, otherwise click)."),
                                "value": _str("Value to write for action \"set\"."),
                                "key": _str("Key to press for action \"press\" (default Return)."),
                            },
                            ["element_index"],
                        ),
                        "description": "Ordered form steps, executed in one shot.",
                    },
                },
                ["window", "fields"],
            ),
        ),
    ]


def openai_tools() -> list[dict[str, Any]]:
    return tool_definitions()


def anthropic_tools() -> list[dict[str, Any]]:
    return [
        {
            "name": item["function"]["name"],
            "description": item["function"]["description"],
            "input_schema": item["function"]["parameters"],
        }
        for item in tool_definitions()
    ]
