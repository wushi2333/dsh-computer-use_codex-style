from __future__ import annotations

from typing import Any, Mapping

from computer_use.browser_api import BROWSER_TOOLS, BrowserSurface
from computer_use.driver import ComputerUse
from computer_use.mac_api import MAC_TOOLS, MacSurface
from computer_use.session import CuaSession
from computer_use.tools import WINDOW2_TOOLS, tool_names

ALIASES = {
    "observe": "get_window_state",
    "type": "type_text",
    "keypress": "press_key",
    "click_element": "click",
    "window": "get_window",
}

HARNESS_TOOLS = ("batch_actions", "session_note", "session_state", "end_turn", "diagnostic_state", "fill_form")
VOID_COMPUTER = frozenset(
    {
        "click",
        "type_text",
        "press_key",
        "scroll",
        "scroll_element",
        "drag",
        "set_value",
        "perform_secondary_action",
        "activate_window",
        "launch_app",
    }
)


class ToolExecutor:
    """Dispatch computer / mac / browser / harness tool calls."""

    def __init__(
        self,
        driver: ComputerUse,
        *,
        compact: bool = True,
        steal_focus: bool = True,
        browser: BrowserSurface | None = None,
    ) -> None:
        self.driver = driver
        self.compact = compact
        self.mac = MacSurface(driver, steal_focus=steal_focus)
        self.browser = browser or BrowserSurface()
        self.session = CuaSession()
        self.tool_names = set(tool_names()) | set(MAC_TOOLS) | set(BROWSER_TOOLS) | set(HARNESS_TOOLS) | set(ALIASES)

    def execute(self, name: str, arguments: Mapping[str, Any] | None = None) -> dict[str, Any]:
        if name not in self.tool_names:
            raise KeyError(f"unknown tool {name}")
        # Underscore-prefixed keys are an internal channel (composite actions
        # marking their inner steps as lease-validated); callers are the model,
        # so they must never be able to inject them. Strip before dispatch.
        spec = {k: v for k, v in dict(arguments or {}).items() if not str(k).startswith("_")}
        return self._execute(name, spec, already_leased=False)

    def _execute(self, name: str, spec: dict[str, Any], *, already_leased: bool = False) -> dict[str, Any]:
        canonical = ALIASES.get(name, name)
        if canonical == "get_window_state" and name == "observe":
            spec.setdefault("include_screenshot", True)
            spec.setdefault("include_text", False)
        # A composite action (fill_form) validates the lease once up front; its
        # inner steps must not re-validate, otherwise an intervening observation
        # of a *different* window (the overlay, a popup, a refresh) fails every
        # remaining step with "last get_window_state was window id X". The flag
        # is injected here, one layer *inside* the public boundary, so it can
        # only originate from fill_form itself.
        if already_leased:
            spec = {**spec, "_lease_ok": True}
        result = self._dispatch(canonical, spec, lease_ok=already_leased)
        if canonical in VOID_COMPUTER:
            result = None
        return {"name": name, "canonical": canonical, "result": result, "observation": None}

    def execute_call(self, call: Mapping[str, Any]) -> dict[str, Any]:
        name = str(call.get("name") or call.get("tool") or "")
        arguments = call.get("arguments") or call.get("args") or {}
        if not isinstance(arguments, Mapping):
            raise TypeError("arguments must be an object")
        return self.execute(name, arguments)

    def _dispatch(self, name: str, spec: dict[str, object], *, lease_ok: bool = False) -> Any:
        if name in BROWSER_TOOLS:
            return self.browser.dispatch(name, spec)
        if name == "get_app_state":
            return self._observe(self.mac.get_app_state(spec), spec)
        if name == "paste":
            return self.mac.paste(spec)
        if name == "select_text":
            return self.mac.select_text(spec)
        if name == "batch_actions":
            return self._batch(spec)
        if name == "fill_form":
            return self._fill_form(spec, lease_ok=lease_ok)
        if name == "session_note":
            self.session.note(str(spec.get("text") or ""))
            return self.session.snapshot()
        if name == "session_state":
            return self.session.snapshot()
        if name == "end_turn":
            return self._end_turn(spec)
        if name == "diagnostic_state":
            payload = {
                "backend": type(self.driver.backend).__name__,
                "lease": self.driver.lease.snapshot(),
                "overlay": self.driver.overlay.enabled,
            }
            getter = getattr(self.driver.backend, "diagnostic_state", None)
            if callable(getter):
                try:
                    extra = getter()
                    if isinstance(extra, dict):
                        payload.update(extra)
                except Exception:
                    pass
            return payload
        return self._computer(name, spec)

    def _end_turn(self, spec: dict[str, object]) -> dict[str, Any]:
        from computer_use.turn import bind_interrupt, signal_turn_ended, turn_ended_payload

        session_id = str(spec.get("session_id") or "local")
        turn_id = str(spec.get("turn_id") or "turn")
        bind_interrupt(self.driver.interrupt, session_id, turn_id)
        self.driver.interrupt.end_turn()
        self.driver.overlay.hide()
        ended = getattr(self.driver.backend, "end_turn", None)
        if callable(ended):
            try:
                return ended(session_id, turn_id)
            except TypeError:
                return ended()
        closer = getattr(self.driver.backend, "close", None)
        if callable(closer):
            closer()
        signal_turn_ended(session_id)
        try:
            from computer_use.notify_config import write_notify_config

            write_notify_config(session_id, turn_id)
        except OSError:
            pass
        return turn_ended_payload(session_id, turn_id)

    def _computer(self, name: str, spec: dict[str, object]) -> Any:
        table = {
            "list_apps": lambda: self.driver.list_apps(),
            "list_windows": lambda: self.driver.list_windows(),
            "get_window": lambda: self.driver.get_window(spec),
            "launch_app": lambda: self.driver.launch_app(spec),
            "get_window_state": lambda: self._observe(self.driver.get_window_state(spec), spec),
            "click": lambda: self._maybe_mac_click(spec),
            "type_text": lambda: self.driver.type_text(spec),
            "press_key": lambda: self.driver.press_key(spec),
            "scroll": lambda: self.driver.scroll(spec),
            "scroll_element": lambda: self.driver.scroll_element(spec),
            "drag": lambda: self.driver.drag(spec),
            "set_value": lambda: self.driver.set_value(spec),
            "perform_secondary_action": lambda: self.driver.perform_secondary_action(spec),
            "activate_window": lambda: self.driver.activate_window(spec),
        }
        if name not in table:
            raise KeyError(f"unhandled tool {name}")
        return table[name]()

    def _maybe_mac_click(self, spec: dict[str, object]) -> Any:
        if spec.get("app") and not spec.get("window"):
            return self.mac.click(spec)
        return self.driver.click(spec)

    def _observe(self, raw: dict[str, Any], spec: dict[str, object]) -> dict[str, Any]:
        if not self.compact:
            self.session.remember_shot(raw)
            self.session.state = raw
            return raw
        emit = spec.get("emit_image") is True
        disable_diff = spec.get("disableDiff", spec.get("disableDiffing", True)) is not False
        return self.session.present(raw, emit_image=emit, disable_diff=disable_diff)

    def _fill_form(self, spec: dict[str, Any], *, lease_ok: bool = False) -> dict[str, Any]:
        """Atomically fill several form fields, then report per-step results.

        Mirrors the Rust helper's `fill_form` (helper-rs/src/main.rs): the
        default action is a *replace* (`set_value`), never a bare append, and a
        failing step stops the whole fill unless `continue_on_error` is set.
        """
        fields = spec.get("fields") or []
        if not isinstance(fields, list) or not fields:
            raise TypeError("fields array must not be empty")
        # One lease validation for the whole batch. Without it a caller that
        # never observed the window could drive input blind; the per-step
        # bypass below only stops *inner* steps from re-validating.
        #
        # The Python lease only records observations that came through *this*
        # executor. When `get_window_state` is served by the native helper
        # (the normal path) this lease stays empty, so requiring it here would
        # reject a perfectly valid fill with "no fresh get_window_state". The
        # helper enforces its own freshness; this side therefore observes once
        # itself when it has nothing on record, instead of failing closed.
        if not lease_ok:
            target = spec.get("window")
            # Audit P2-2 (2026-10-25): without a window there is nothing to
            # observe and nothing to validate — a fill would drive input blind
            # into whatever happens to be focused. Fail closed instead.
            if not isinstance(target, dict):
                raise ValueError(
                    "fill_form needs a \"window\" object (the same one get_window_state "
                    "returned) so the lease can be validated; omitting it would fill blind"
                )
            if self.driver.lease.observed_at is None:
                self.execute("get_window_state", {"window": target, "include_text": True})
            self.driver.lease.require(target, "fill_form")
        continue_on_error = spec.get("continue_on_error") is True
        results: list[dict[str, Any]] = []
        failed = False
        for i, field in enumerate(fields):
            if failed and not continue_on_error:
                break
            if not isinstance(field, Mapping):
                results.append({"ok": False, "index": i, "error": "field entry must be an object"})
                failed = True
                continue
            index = field.get("element_index")
            if index is None:
                results.append({"ok": False, "index": i, "error": "element_index is required"})
                failed = True
                continue
            value = field.get("value")
            action = str(field.get("action") or ("set" if value is not None else "click"))
            base: dict[str, Any] = {"window": field.get("window") or spec.get("window"), "element_index": index}
            if action in ("set", "value"):
                if value is None:
                    results.append({"ok": False, "index": i, "action": action, "element_index": index,
                                    "error": 'action "set" needs a value'})
                    failed = True
                    continue
                call = {"name": "set_value", "arguments": {**base, "value": str(value)}}
            elif action == "check":
                call = {"name": "perform_secondary_action", "arguments": {**base, "action": "Check"}}
            elif action == "click":
                call = {"name": "click", "arguments": base}
            elif action == "press":
                call = {"name": "press_key",
                        "arguments": {**base, "key": str(field.get("key") or "Return")}}
            else:
                results.append({"ok": False, "index": i, "action": action, "element_index": index,
                                "error": f"unsupported field action: {action}"})
                failed = True
                continue
            try:
                # Internal channel: already_leased marks the step as covered by
                # fill_form's one up-front lease check. Injecting "_lease_ok"
                # through execute_call would cross the public boundary, where
                # the same key from a model call must be (and is) stripped.
                outcome = self._execute(str(call["name"]), dict(call["arguments"]), already_leased=True)
            except Exception as exc:  # noqa: BLE001 - report, never abort the whole fill silently
                results.append({"ok": False, "index": i, "action": action, "element_index": index,
                                "error": str(exc)})
                failed = True
                continue
            results.append({"ok": True, "index": i, "action": action, "element_index": index,
                            "value": outcome})
        filled = sum(1 for r in results if r.get("ok"))
        return {
            "ok": not failed,
            "fields": len(results),
            "filled": filled,
            "stoppedEarly": failed and not continue_on_error,
            "results": results,
        }

    def _batch(self, spec: dict[str, object]) -> dict[str, Any]:
        actions = spec.get("actions") or []
        if not isinstance(actions, list):
            raise TypeError("actions must be an array")
        results: list[dict[str, Any]] = []
        for index, item in enumerate(actions):
            # Mirror the Rust helper's `batch_action_name` exactly: only a *string*
            # name counts, so `{"name": 7}` is rejected rather than stringified to
            # "7" and dispatched. Before both guards an entry with no usable name
            # either vanished (non-mapping) or reached dispatch as "", whose
            # diagnostic is the bare `unsupported method: ` prefix.
            name = item.get("name") if isinstance(item, Mapping) else None
            if not isinstance(name, str) or not name.strip():
                results.append(
                    {
                        "ok": False,
                        "index": index,
                        "error": 'each action needs a non-empty string "name" and an "arguments" object, '
                        'e.g. {"name":"press_key","arguments":{"window":{...},"key":"Return"}}; '
                        'an item written as {"action":...} names no method to run',
                    }
                )
                continue
            try:
                results.append(self.execute_call(item))
            except Exception as error:  # noqa: BLE001 - mirror the Rust per-item error
                # Rust turns a dispatch failure into a per-item entry; a Python
                # raise would otherwise abort the whole batch on one bad action.
                results.append(
                    {"ok": False, "index": index, "name": name, "error": str(error)}
                )
        then = str(spec.get("then") or "")
        refresh = None
        if then == "tab_ax_write":
            refresh = self.execute("tab_ax_write", {"tab_id": spec.get("tab_id"), "mode": spec.get("mode") or "state"})
        elif then == "get_window_state":
            args = spec.get("refresh_args") if isinstance(spec.get("refresh_args"), dict) else {}
            refresh = self.execute("get_window_state", args)
        return {"batched": len(results), "results": results, "refresh": refresh}


def official_tool_names() -> tuple[str, ...]:
    return WINDOW2_TOOLS
