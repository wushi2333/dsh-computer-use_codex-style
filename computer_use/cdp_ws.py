"""Minimal CDP JSON-RPC over WebSocket (Chrome DevTools Protocol).

The direct-CDP path (Chrome launched with `--remote-debugging-port`) speaks WebSocket,
which needs the third-party `websocket-client` package. The plugin's *primary* browser
channel on Linux is the Chrome extension bridge (ExtensionHub, 127.0.0.1:8765), which needs
no such package -- so a missing `websocket-client` must degrade this one path instead of
killing the whole engine at import time. It used to be a module-level `import websocket`,
and on a machine without the package `browser_api.default_browser()` -> `cdp_browser` ->
`cdp_ws` raised `ModuleNotFoundError` while the sidecar was starting, so the ExtensionHub
never bound and every browser tool was dead (observed 2026-09-17, python3.14 without pip).

The import is therefore lazy and the failure is a `CdpUnavailable` carrying the exact fix.
Nothing else in the engine (extension channel, tab catalog, non-CDP operations) is affected.
"""

from __future__ import annotations

import json
import time
from typing import Any

#: What the operator must install to use the direct-CDP path. Kept in one place so the
#: error text, the tests and the docs cannot drift apart.
WEBSOCKET_CLIENT_HINT = (
    "the direct-CDP (WebSocket) path needs the 'websocket-client' package: "
    "python3 -m pip install --user websocket-client, or apt install python3-websocket-client. "
    "The Chrome-extension channel (ExtensionHub on 127.0.0.1:8765) works without it."
)


class CdpUnavailable(RuntimeError):
    """The WebSocket transport cannot be used in this interpreter."""


def load_websocket():
    """Import `websocket-client` on demand, or explain exactly what is missing.

    Returns the module so callers can use `websocket.create_connection`. Raises
    `CdpUnavailable` -- never `ModuleNotFoundError` -- so an RPC handler reports a
    readable failure instead of a bare traceback.

    @returns {module}
    """
    try:
        import websocket  # noqa: PLC0415 - deliberate: the import IS the failure point
    except ImportError as exc:  # pragma: no cover - exercised through the guard test
        raise CdpUnavailable(
            f"cannot open a CDP WebSocket connection: {WEBSOCKET_CLIENT_HINT}"
        ) from exc
    return websocket


class CdpConn:
    def __init__(self, url: str, timeout: float = 20) -> None:
        self.url = url
        self.ws = load_websocket().create_connection(url, timeout=timeout)
        self._id = 0
        self._seq = 0
        self.events: list[dict[str, Any]] = []

    def _take(self) -> dict[str, Any]:
        raw = self.ws.recv()
        msg = json.loads(raw)
        if msg.get("method"):
            self._seq += 1
            msg["_sequence"] = self._seq
            self.events.append(msg)
        return msg

    def read_events(self, after_sequence: int = 0, methods: list[str] | None = None, timeout_ms: int = 0) -> list[dict[str, Any]]:
        if timeout_ms > 0:
            deadline = time.time() + timeout_ms / 1000
            while time.time() < deadline:
                try:
                    self.ws.settimeout(max(0.05, deadline - time.time()))
                    self._take()
                except Exception:
                    break
        allowed = set(methods) if methods else None
        out = []
        for msg in self.events:
            seq = int(msg.get("_sequence") or 0)
            if seq <= after_sequence:
                continue
            name = str(msg.get("method") or "")
            if allowed is not None and name not in allowed:
                continue
            out.append({"sequence": seq, "method": name, "params": msg.get("params") or {}})
        return out

    def last_event(self, method: str) -> dict[str, Any] | None:
        for msg in reversed(self.events):
            if msg.get("method") == method:
                params = msg.get("params")
                return params if isinstance(params, dict) else {}
        return None

    def pop_event(self, method: str) -> dict[str, Any] | None:
        for index in range(len(self.events) - 1, -1, -1):
            if self.events[index].get("method") == method:
                msg = self.events.pop(index)
                params = msg.get("params")
                return params if isinstance(params, dict) else {}
        return None

    def call(self, method: str, params: dict[str, Any] | None = None, timeout: float = 20) -> dict[str, Any]:
        self._id += 1
        nid = self._id
        self.ws.send(json.dumps({"id": nid, "method": method, "params": params or {}}))
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.ws.settimeout(max(0.2, deadline - time.time()))
            msg = self._take()
            if msg.get("id") != nid:
                continue
            if msg.get("error"):
                raise RuntimeError(f"{method}: {msg['error']}")
            result = msg.get("result")
            return result if isinstance(result, dict) else {}
        raise TimeoutError(method)

    def wait_event(self, name: str, timeout: float = 20) -> dict[str, Any]:
        cached = self.last_event(name)
        if cached is not None:
            return cached
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.ws.settimeout(max(0.2, deadline - time.time()))
            msg = self._take()
            if msg.get("method") == name:
                params = msg.get("params")
                return params if isinstance(params, dict) else {}
        raise TimeoutError(name)

    def close(self) -> None:
        try:
            self.ws.close()
        except OSError:
            pass
