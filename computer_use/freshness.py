"""Fresh observation lease: input actions require a recent get_window_state."""

from __future__ import annotations

import time
from typing import Any

from computer_use.models import Observation, WindowRef

# TC-21: official has no time-based expiry; 0 disables the lease by default.
DEFAULT_TTL_MS = 0

INPUT_ACTIONS = frozenset(
    {
        "click",
        "type_text",
        "press_key",
        "scroll",
        "drag",
        "set_value",
        "perform_secondary_action",
        "scroll_element",
        "fill_form",
        "paste",
        "select_text",
    }
)


def window_from_spec(spec: dict[str, object]) -> WindowRef | None:
    raw = spec.get("window")
    if isinstance(raw, dict):
        app = str(raw.get("app") or "")
        if not app:
            return None
        ident = raw.get("id", 0)
        try:
            window_id = int(ident)  # type: ignore[arg-type]
        except (TypeError, ValueError):
            window_id = 0
        return WindowRef(app=app, id=window_id, title=str(raw.get("title") or ""))
    app = str(spec.get("app") or "")
    if not app:
        return None
    ident = spec.get("id", 0)
    try:
        window_id = int(ident)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        window_id = 0
    return WindowRef(app=app, id=window_id, title=str(spec.get("title") or ""))


class ObservationLease:
    def __init__(self, ttl_ms: int = DEFAULT_TTL_MS) -> None:
        self.ttl_ms = max(0, int(ttl_ms))
        self.observed_at: float | None = None
        self.window: WindowRef | None = None
        self.screenshot_id: str | None = None
        self.input_monitor = None

    def record(self, observation: Observation) -> None:
        self.observed_at = time.monotonic()
        self.window = observation.window
        self.screenshot_id = observation.screenshot_id or None
        monitor = getattr(self, "input_monitor", None)
        if monitor is not None:
            monitor.clear()

    def snapshot(self) -> dict[str, Any]:
        age_ms = None
        if self.observed_at is not None:
            age_ms = int((time.monotonic() - self.observed_at) * 1000)
        return {
            "ttlMs": self.ttl_ms,
            "ageMs": age_ms,
            "window": None if self.window is None else self.window.to_dict(),
            "screenshotId": self.screenshot_id,
            "inputMonitor": None if self.input_monitor is None else self.input_monitor.snapshot(),
        }

    def require(self, spec: dict[str, object], action: str) -> None:
        monitor = getattr(self, "input_monitor", None)
        if monitor is not None and action in INPUT_ACTIONS:
            monitor.require_clean(action)
        # TC-21: ttl_ms == 0 disables only the *time* expiry. The observation
        # requirement and the identity checks below always run, matching the
        # official helper (helper-rs state.rs: require_fresh has the ttl early
        # return, but require_window_use still enforces window identity).
        if self.observed_at is None:
            raise PermissionError(
                f"{action} refused: no fresh get_window_state. "
                "Call get_window_state on the target window first."
            )
        if self.ttl_ms > 0:
            age_ms = int((time.monotonic() - self.observed_at) * 1000)
            if age_ms > self.ttl_ms:
                raise PermissionError(
                    f"{action} refused: observation expired ({age_ms}ms > ttl {self.ttl_ms}ms). "
                    "Call get_window_state again."
                )
        window = window_from_spec(spec)
        if window is not None and self.window is not None and window.id and self.window.id:
            if window.id != self.window.id:
                raise PermissionError(
                    f"{action} refused: last get_window_state was window id {self.window.id}, not {window.id}. "
                    "Observe that window first."
                )
        shot = spec.get("screenshotId")
        if isinstance(shot, str) and shot and self.screenshot_id and shot != self.screenshot_id:
            raise PermissionError(
                f"{action} refused: screenshotId {shot} is not the latest observation ({self.screenshot_id}). "
                "Call get_window_state again."
            )
