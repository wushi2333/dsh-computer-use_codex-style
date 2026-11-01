from __future__ import annotations

from typing import Any

from computer_use.approval import ApprovalGate
from computer_use.backend import DesktopBackend
from computer_use.interrupt import InterruptFlag
from computer_use.keys import normalize_key_chord
from computer_use.models import ActionRecord, Observation, WindowRef
from computer_use.overlay import OverlaySession
from computer_use.freshness import DEFAULT_TTL_MS, ObservationLease
from computer_use.policy import deny_allowed_apps, deny_press_key, deny_target
from computer_use.validate import click_count, element_index, finite_round, require_state_flags, secondary_action


class ComputerUse:
    """sky window2 client over an injected desktop backend."""

    target = "windows"

    def __init__(
        self,
        backend: DesktopBackend,
        interrupt: InterruptFlag | None = None,
        approval: ApprovalGate | None = None,
        overlay: OverlaySession | None = None,
        ttl_ms: int = DEFAULT_TTL_MS,
        allowed_apps: list[str] | None = None,
    ) -> None:
        self.backend = backend
        self.last_observation: Observation | None = None
        self.interrupt = interrupt or InterruptFlag()
        self.approval = approval
        self.overlay = overlay or OverlaySession()
        self.lease = ObservationLease(ttl_ms)
        self.lease.input_monitor = self.interrupt.input_monitor
        self.allowed_apps = list(allowed_apps or [])

    def list_apps(self) -> list[dict[str, Any]]:
        return [app.to_dict() for app in self.backend.list_apps()]

    def list_windows(self) -> list[dict[str, Any]]:
        return [window.to_dict() for window in self.backend.list_windows()]

    def get_window(self, spec: dict[str, object]) -> dict[str, Any]:
        return self.backend.get_window(spec).to_dict()

    def launch_app(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec)
        app = str(spec.get("app") or "")
        deny_allowed_apps(app, self.allowed_apps)
        if self.approval and app:
            self.approval.ensure(app, app)
        return self._void(self.backend.launch_app(spec))

    def get_window_state(self, spec: dict[str, object] | None = None) -> dict[str, Any]:
        payload = dict(spec or {})
        self._before(payload)
        include_shot, include_text = require_state_flags(payload)
        payload["include_screenshot"] = include_shot
        payload["include_text"] = include_text
        observation = self.backend.observe(payload)
        self.last_observation = observation
        self.lease.record(observation)
        return observation.to_dict()

    def observe(self, spec: dict[str, object] | None = None) -> dict[str, Any]:
        payload = dict(spec or {})
        payload.setdefault("include_screenshot", True)
        payload.setdefault("include_text", False)
        return self.get_window_state(payload)

    def click(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="click")
        deny_target(self._window(spec), self.allowed_apps)
        payload = dict(spec)
        payload["click_count"] = click_count(payload)
        index = element_index(payload)
        if index is not None:
            payload["element_index"] = index
            payload["helper_method"] = "click_element"
        else:
            payload["helper_method"] = "click"
            if "x" in payload:
                payload["x"] = finite_round(payload["x"], "point.x")
            if "y" in payload:
                payload["y"] = finite_round(payload["y"], "point.y")
        record = self.backend.click(payload)
        px = record.payload.get("x")
        py = record.payload.get("y")
        self.interrupt.input_monitor.mark_synthetic(0.3)
        if px is not None and py is not None:
            self.overlay.show(float(px), float(py), press=True)
        else:
            self.overlay.show()
        return self._void(record)

    def type_text(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="type_text")
        deny_target(self._window(spec), self.allowed_apps)
        return self._void(self.backend.type_text(spec))

    def press_key(self, spec: dict[str, object]) -> dict[str, Any]:
        payload = dict(spec)
        self._before(payload, action="press_key")
        payload["key"] = normalize_key_chord(str(spec.get("key") or ""))
        deny_press_key(str(payload["key"]))
        deny_target(self._window(payload), self.allowed_apps)
        return self._void(self.backend.press_key(payload))

    def scroll(self, spec: dict[str, object]) -> dict[str, Any]:
        if spec.get("element_index") is not None or spec.get("direction"):
            return self.scroll_element(spec)
        self._before(spec, action="scroll")
        deny_target(self._window(spec), self.allowed_apps)
        payload = dict(spec)
        for key in ("x", "y", "scrollX", "scrollY"):
            if key in payload and payload[key] is not None:
                payload[key] = finite_round(payload[key], f"scroll.{key}")
        self.interrupt.input_monitor.mark_synthetic(0.3)
        return self._void(self.backend.scroll(payload))

    def scroll_element(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="scroll")
        deny_target(self._window(spec), self.allowed_apps)
        payload = dict(spec)
        from computer_use.validate import scroll_element_args

        direction, pages = scroll_element_args(payload)
        payload["direction"] = direction
        payload["pages"] = pages
        payload["element_index"] = element_index(payload)
        self.interrupt.input_monitor.mark_synthetic(0.3)
        scroll = getattr(self.backend, "scroll_element", None)
        if scroll is None:
            return self._void(self.backend.scroll(payload))
        return self._void(scroll(payload))

    def drag(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="drag")
        deny_target(self._window(spec), self.allowed_apps)
        payload = dict(spec)
        for key in ("from_x", "from_y", "to_x", "to_y"):
            if key in payload and payload[key] is not None:
                payload[key] = finite_round(payload[key], key)
        record = self.backend.drag(payload)
        fx, fy = record.payload.get("from_x"), record.payload.get("from_y")
        tx, ty = record.payload.get("to_x"), record.payload.get("to_y")
        if fx is not None and fy is not None:
            self.overlay.show(float(fx), float(fy))
        if tx is not None and ty is not None:
            self.overlay.move_cursor(float(tx), float(ty), press=True)
        return self._void(record)

    def set_value(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="set_value")
        deny_target(self._window(spec), self.allowed_apps)
        return self._void(self.backend.set_value(spec))

    def paste(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="paste")
        deny_target(self._window(spec), self.allowed_apps)
        paste = getattr(self.backend, "paste", None)
        if paste is None:
            return self.type_text(spec)
        return self._void(paste(spec))

    def select_text(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="select_text")
        deny_target(self._window(spec), self.allowed_apps)
        select = getattr(self.backend, "select_text", None)
        if select is None:
            return {"ok": True, "action": {"kind": "select_text", **spec}}
        return self._void(select(spec))

    def perform_secondary_action(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec, action="perform_secondary_action")
        deny_target(self._window(spec), self.allowed_apps)
        payload = dict(spec)
        payload["action"] = secondary_action(str(spec.get("action") or ""))
        payload["element_index"] = element_index(payload)
        return self._void(self.backend.perform_secondary_action(payload))

    def activate_window(self, spec: dict[str, object]) -> dict[str, Any]:
        self._before(spec)
        deny_target(self._window(spec), self.allowed_apps)
        return self._void(self.backend.activate_window(spec))

    def _before(self, spec: dict[str, object], action: str | None = None) -> None:
        self.interrupt.check()
        window = self._window(spec)
        app = window.app if window is not None else str(spec.get("app") or "")
        if app:
            deny_allowed_apps(app, self.allowed_apps)
        deny_target(window, self.allowed_apps)
        # `_lease_ok` is injected by the executor's internal channel for
        # composite actions (fill_form) that already validated the lease once
        # for the whole batch; re-validating per inner step would fail on any
        # intervening observation of another window. Consume it here so it can
        # never leak into a backend payload.
        if action and not spec.pop("_lease_ok", None):
            self.lease.require(spec, action)
            if self.approval and window is not None:
                self.approval.ensure(window.app, window.app)

    def _void(self, record: ActionRecord) -> dict[str, Any]:
        self.interrupt.check()
        return {"ok": True, "action": record.to_dict()}

    def _window(self, spec: dict[str, object]) -> WindowRef | None:
        raw = spec.get("window")
        if not isinstance(raw, dict):
            return None
        app = str(raw.get("app") or "")
        ident = raw.get("id", 0)
        title = str(raw.get("title") or "")
        if not app:
            return None
        return WindowRef(app=app, id=int(ident), title=title)
