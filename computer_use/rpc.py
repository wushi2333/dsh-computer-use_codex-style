"""JSON-RPC methods for the Computer Use sidecar (one process, exclusive pointer)."""

from __future__ import annotations

import json
import os
import queue
import sys
import threading
from typing import Any, Mapping

from computer_use.errors import ApprovalNeeded, DesktopUnavailable, TurnInterrupted
from computer_use.helper_protocol import APPROVED_APP_META_KEY
from computer_use.executor import ToolExecutor
from computer_use.harness import system_prompt
from computer_use.helper_locate import locate_helper
from computer_use.images import detach_images
from computer_use.runtime import make_executor
from computer_use.surfaces import tools_for_surface


def backend_health(requested: str) -> dict[str, Any]:
    helper = None
    try:
        helper = locate_helper()
    except OSError:
        helper = None
    resolved = requested
    if requested in ("live", "windows"):
        resolved = "windows"
    if requested == "helper":
        resolved = "helper" if helper is not None else "missing-helper"
    return {
        "requested": requested,
        "resolved": resolved,
        "overlay": "dsh" if resolved == "windows" else "codex-helper",
        "helperPath": str(helper) if helper is not None else None,
        "codexRequired": False,
        "python": sys.version.split()[0],
        "platform": sys.platform,
    }


def sanitize_schema(node: Any) -> Any:
    allowed = {
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "const",
        "description",
        "title",
        "default",
        "examples",
        "oneOf",
    }
    if isinstance(node, list):
        return [sanitize_schema(item) for item in node]
    if not isinstance(node, dict):
        return node
    out: dict[str, Any] = {}
    for key, value in node.items():
        if key not in allowed:
            continue
        if key == "additionalProperties":
            out[key] = True if isinstance(value, dict) else bool(value)
        elif key in ("properties",):
            if isinstance(value, dict):
                out[key] = {name: sanitize_schema(child) for name, child in value.items()}
        elif key == "items":
            out[key] = sanitize_schema(value) if isinstance(value, dict) else {}
        elif key == "oneOf":
            branches = [sanitize_schema(item) for item in value] if isinstance(value, list) else []
            if len(branches) >= 2:
                out[key] = branches
        else:
            out[key] = value
    return out


def _target_app(name: str, arguments: dict[str, Any]) -> str:
    if name == "launch_app":
        return str(arguments.get("app") or "")
    window = arguments.get("window")
    if isinstance(window, dict):
        return str(window.get("app") or "")
    return str(arguments.get("app") or "")


def dsh_tool_list(surface: str, disabled: list[str] | None = None) -> list[dict[str, Any]]:
    tools = []
    skip = set(disabled or [])
    for item in tools_for_surface(surface, disabled=list(skip)):
        if not isinstance(item, dict):
            continue
        # Accept both shapes: the OpenAI function envelope used by most of the
        # engine, and the flat MCP-shaped specs the browser catalog now emits.
        fn = item.get("function") if isinstance(item.get("function"), dict) else item
        if fn.get("name") in skip:
            continue
        parameters = fn.get("parameters") if isinstance(fn.get("parameters"), dict) else {"type": "object", "properties": {}}
        tools.append(
            {
                "name": fn.get("name"),
                "description": fn.get("description") or "",
                "parameters": sanitize_schema(parameters),
            }
        )
    return tools


class ComputerUseServer:
    """In-process RPC handler used by stdio serve and tests."""

    def __init__(
        self,
        executor: ToolExecutor,
        *,
        surface: str,
        backend: str,
        max_image_edge: int = 0,
    ) -> None:
        self.executor = executor
        self.executor.compact = False
        self.surface = surface
        self.backend = backend
        self.max_image_edge = max_image_edge
        self.closed = False
        self.approved_apps: set[str] = set()
        self._esc = None
        if backend in ("live", "windows", "helper"):
            from computer_use.interrupt import EscapeHook

            self._esc = EscapeHook(executor.driver.interrupt, overlay=executor.driver.overlay)
            self._esc.start()
            executor.driver.overlay.esc_hook = self._esc
            from computer_use.fake_backend import FakeDesktop

            if not isinstance(executor.driver.backend, FakeDesktop):
                executor.driver.interrupt.exit_on_trip = True

    def handle(self, request: Mapping[str, Any]) -> dict[str, Any]:
        method = str(request.get("method") or "")
        params = request.get("params") if isinstance(request.get("params"), dict) else {}
        req_id = request.get("id")
        meta = request.get("meta") if isinstance(request.get("meta"), dict) else {}
        official = request.get("jsonrpc") != "2.0" and method not in {
            "health",
            "tools",
            "call",
            "interrupt",
            "shutdown",
            "prompt",
            "end_turn",
            "close",
        }
        try:
            if official:
                result = self._call({"name": method, "arguments": params}, meta)
                return {"id": req_id, "ok": True, "result": result}
            if method == "close":
                method = "shutdown"
            result = self._dispatch(method, params)
        except ApprovalNeeded as exc:
            if official or request.get("jsonrpc") != "2.0":
                return {"id": req_id, "ok": False, "error": str(exc), "approvalRequest": exc.request}
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32001, "message": str(exc), "data": {"approvalRequest": exc.request}},
            }
        except KeyError as exc:
            if official:
                return {"id": req_id, "ok": False, "error": str(exc)}
            return {"jsonrpc": "2.0", "id": req_id, "error": {"code": -32601, "message": str(exc)}}
        except (DesktopUnavailable, TurnInterrupted, TypeError, ValueError, PermissionError) as exc:
            if official:
                return {"id": req_id, "ok": False, "error": str(exc)}
            return {"jsonrpc": "2.0", "id": req_id, "error": {"code": -32000, "message": str(exc)}}
        except Exception as exc:  # noqa: BLE001 — sidecar must never die on one call
            if official:
                return {"id": req_id, "ok": False, "error": f"{type(exc).__name__}: {exc}"}
            return {"jsonrpc": "2.0", "id": req_id, "error": {"code": -32000, "message": f"{type(exc).__name__}: {exc}"}}
        return {"jsonrpc": "2.0", "id": req_id, "result": result}

    def _dispatch(self, method: str, params: dict[str, Any]) -> Any:
        if method == "health":
            payload = backend_health(self.backend)
            payload["surface"] = self.surface
            payload["closed"] = self.closed
            payload["ttlMs"] = self.executor.driver.lease.ttl_ms
            payload["allowedApps"] = list(self.executor.driver.allowed_apps)
            payload["observation"] = self.executor.driver.lease.snapshot()
            try:
                from computer_use.win_dpi import dpi_snapshot

                payload["dpi"] = dpi_snapshot()
            except Exception:
                payload["dpi"] = {"aware": False, "scale": 1.0}
            try:
                from computer_use.wgc_winrt import wgc_framepool_available

                payload["capture"] = {"wgcFramePool": wgc_framepool_available(), "jpeg": True}
            except Exception:
                payload["capture"] = {"wgcInterop": False, "d3d11": False}
            payload["pipe"] = getattr(self, "pipe_name", None)
            return payload
        if method == "tools":
            surface = str(params.get("surface") or self.surface)
            disabled = list(getattr(self.executor.browser, "_disabled_ids", []) or [])
            return {"tools": dsh_tool_list(surface, disabled=disabled), "surface": surface, "disabledMemberIds": disabled}
        if method == "prompt":
            return {"prompt": system_prompt()}
        if method == "diagnostic_state":
            payload = {
                "backend": self.backend,
                "approvedApps": list(self.approved_apps),
                "overlay": self.executor.driver.overlay.enabled,
                "lease": self.executor.driver.lease.snapshot(),
            }
            try:
                from computer_use.notify_config import feature_status

                payload["feature_status"] = feature_status()
                payload["notify"] = True
            except Exception:
                payload["notify"] = True
            getter = getattr(self.executor.driver.backend, "diagnostic_state", None) or getattr(
                self.executor.driver.backend, "_cache_diagnostics", None
            )
            if callable(getter):
                try:
                    extra = getter()
                    if isinstance(extra, dict):
                        payload.update(extra)
                        payload["cacheDiagnostics"] = extra
                except Exception:
                    payload["cacheDiagnostics"] = {}
            return payload
        if method == "window":
            return self._call({"name": "get_window", "arguments": params})
        if method == "scroll_element":
            return self._call({"name": "scroll_element", "arguments": params})
        if method == "call":
            return self._call(params, request_meta=params.get("meta") if isinstance(params.get("meta"), dict) else {})
        if method == "interrupt":
            self.executor.driver.interrupt.trip()
            self.executor.driver.overlay.hide()
            return {"ok": True, "stopped": True}
        if method == "end_turn":
            return self.executor.execute("end_turn", params)
        if method == "shutdown":
            self.closed = True
            if self._esc is not None:
                self._esc.stop()
            self.executor.driver.overlay.hide()
            try:
                from computer_use.cursor_manager import shutdown_manager

                shutdown_manager()
            except Exception:
                pass
            closer = getattr(self.executor.driver.backend, "close", None)
            if callable(closer):
                closer()
            return {"ok": True}
        raise KeyError(f"unknown method {method}")

    def _call(self, params: dict[str, Any], meta: dict[str, Any] | None = None, request_meta: dict[str, Any] | None = None) -> dict[str, Any]:
        name = str(params.get("name") or "")
        arguments = params.get("arguments") if isinstance(params.get("arguments"), dict) else {}
        extra = dict(meta or {})
        extra.update(request_meta or {})
        env_meta = os.environ.get("NODE_REPL_REQUEST_META") or ""
        if env_meta.strip().startswith("{"):
            try:
                parsed = json.loads(env_meta)
                if isinstance(parsed, dict):
                    merged = dict(parsed)
                    merged.update(extra)
                    extra = merged
            except json.JSONDecodeError:
                pass
        approved = str(extra.get(APPROVED_APP_META_KEY) or "")
        if approved:
            self.approved_apps.add(approved.lower())
        conv = str(extra.get("conversationId") or extra.get("conversation_id") or "")
        turn = str(extra.get("turnId") or extra.get("turn_id") or extra.get("callId") or "")
        if conv and turn:
            from computer_use.interrupt import interrupt_path, raise_if_interrupted

            home = os.environ.get("DSH_HOME") or str(__import__("pathlib").Path.home() / ".dsh")
            flag = self.executor.driver.interrupt
            flag.path = interrupt_path(home, conv, turn)
            flag.session_id = conv
            flag.turn_id = turn
            raise_if_interrupted(flag.path)
        app = _target_app(name, arguments)
        if (
            self.backend != "fake"
            and name not in {"list_windows", "list_apps", "end_turn", "batch_actions", "session_note", "session_state"}
            and app
            and app.lower() not in self.approved_apps
        ):
            from computer_use.approval import AppApprovalRequest

            raise ApprovalNeeded(AppApprovalRequest(app=app, display_name=app).to_helper())
        flag = self.executor.driver.interrupt
        flag.stopped = False
        flag.ended = False
        payload = self.executor.execute(name, arguments)
        flag.check()
        observation = payload.get("observation") if isinstance(payload, dict) else None
        if observation:
            _value, images = detach_images(observation, max_edge=self.max_image_edge)
            return {"ok": True, "name": name, "value": None, "images": images}
        value, images = detach_images(payload.get("result") if isinstance(payload, dict) else payload, max_edge=self.max_image_edge)
        return {"ok": True, "name": name, "value": value, "images": images}


def _watch_parent(pid: int) -> None:
    def _run() -> None:
        try:
            import ctypes
            from ctypes import wintypes

            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            SYNCHRONIZE = 0x00100000
            handle = kernel32.OpenProcess(SYNCHRONIZE, False, int(pid))
            if not handle:
                return
            kernel32.WaitForSingleObject(handle, 0xFFFFFFFF)
            kernel32.CloseHandle(handle)
        except Exception:
            return
        os._exit(0)

    threading.Thread(target=_run, name="cu-parent", daemon=True).start()


def serve_stdio(
    *,
    backend: str,
    surface: str,
    steal_focus: bool,
    max_image_edge: int = 0,
    ttl_ms: int = 0,
    allowed_apps: list[str] | None = None,
    parent_pid: int = 0,
) -> int:
    if sys.platform == "win32":
        from computer_use.win_dpi import enable_dpi_awareness

        enable_dpi_awareness()
    if parent_pid:
        _watch_parent(parent_pid)
    server = ComputerUseServer(
        make_executor(backend, steal_focus=steal_focus, ttl_ms=ttl_ms, allowed_apps=allowed_apps),
        surface=surface,
        backend=backend,
        max_image_edge=max_image_edge,
    )
    try:
        from computer_use.pipe_server import start_pipe_thread

        def _pipe_dispatch(method: str, params: dict[str, Any]) -> Any:
            reply = server.handle({"jsonrpc": "2.0", "id": 0, "method": "call", "params": {"name": method, "arguments": params}})
            if "error" in reply:
                raise RuntimeError(reply["error"]["message"])
            return reply.get("result")

        server.pipe_name = start_pipe_thread(_pipe_dispatch)
    except Exception:
        server.pipe_name = None
    work: queue.Queue[dict[str, Any] | None] = queue.Queue()
    out_lock = threading.Lock()

    def emit(payload: dict[str, Any]) -> None:
        with out_lock:
            sys.stdout.write(json.dumps(payload, default=str) + "\n")
            sys.stdout.flush()

    def worker() -> None:
        while True:
            request = work.get()
            if request is None:
                break
            emit(server.handle(request))

    worker_thread = threading.Thread(target=worker, name="cu-rpc", daemon=True)
    worker_thread.start()
    try:
        while not server.closed:
            line = sys.stdin.readline()
            if line == "":
                break
            text = line.strip()
            if not text:
                continue
            try:
                request = json.loads(text)
            except json.JSONDecodeError as exc:
                emit({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(exc)}})
                continue
            if not isinstance(request, dict):
                emit({"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": "request must be an object"}})
                continue
            method = str(request.get("method") or "")
            if method == "interrupt":
                emit(server.handle(request))
                continue
            work.put(request)
            if method == "shutdown":
                break
    finally:
        work.put(None)
        worker_thread.join(timeout=2)
        if not server.closed:
            server.handle({"jsonrpc": "2.0", "id": None, "method": "shutdown"})
    return 0
