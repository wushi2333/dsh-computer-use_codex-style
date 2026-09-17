from __future__ import annotations

import json
import subprocess
import sys
import types
import unittest
from unittest.mock import patch

from computer_use.cdp_ws import CdpConn, CdpUnavailable, WEBSOCKET_CLIENT_HINT, load_websocket


class FakeWs:
    """A WebSocket connection double: enough of the API CdpConn frames against."""

    def __init__(self) -> None:
        self.sent: list[str] = []
        self.inbox: list[str] = []

    def send(self, data: str) -> None:
        self.sent.append(data)
        msg = json.loads(data)
        self.inbox.append(json.dumps({"id": msg["id"], "result": {"ok": True, "method": msg["method"]}}))

    def recv(self) -> str:
        if self.inbox:
            return self.inbox.pop(0)
        return json.dumps({"method": "Page.frameStartedLoading", "params": {}})

    def settimeout(self, _timeout: float) -> None:
        return None

    def close(self) -> None:
        return None


def fake_transport(fake: FakeWs) -> types.SimpleNamespace:
    """A stand-in for the `websocket-client` module.

    The framing tests patch the *transport module* rather than a real installation, so they
    exercise CdpConn's own logic and pass on a machine that does not have
    `websocket-client` at all -- which is exactly the machine this degradation exists for.
    """
    return types.SimpleNamespace(create_connection=lambda url, timeout=20: fake)


class CdpWsTests(unittest.TestCase):
    def test_call_matches_id_and_skips_events(self) -> None:
        fake = FakeWs()
        with patch("computer_use.cdp_ws.load_websocket", return_value=fake_transport(fake)):
            conn = CdpConn("ws://127.0.0.1:9334/devtools/page/x")
            result = conn.call("Page.enable")
        self.assertEqual(result["ok"], True)
        self.assertEqual(json.loads(fake.sent[0])["method"], "Page.enable")
        conn.close()

    def test_javascript_dialog_event_is_buffered(self) -> None:
        fake = FakeWs()

        def send(data: str) -> None:
            fake.sent.append(data)
            msg = json.loads(data)
            fake.inbox.append(
                json.dumps({"method": "Page.javascriptDialogOpening", "params": {"type": "alert", "message": "hi"}})
            )
            fake.inbox.append(json.dumps({"id": msg["id"], "result": {}}))

        fake.send = send  # type: ignore[method-assign]
        with patch("computer_use.cdp_ws.load_websocket", return_value=fake_transport(fake)):
            conn = CdpConn("ws://127.0.0.1:9334/devtools/page/x")
            conn.call("Page.enable")
            dialog = conn.last_event("Page.javascriptDialogOpening")
        self.assertEqual(dialog["message"], "hi")
        popped = conn.pop_event("Page.javascriptDialogOpening")
        self.assertEqual(popped["message"], "hi")
        self.assertIsNone(conn.last_event("Page.javascriptDialogOpening"))


class MissingWebsocketClientTests(unittest.TestCase):
    """A machine without `websocket-client` must lose the CDP path, not the engine.

    The historical failure: `cdp_ws` imported `websocket` at module level, so
    `browser_api.default_browser()` (reached from the sidecar's own startup with
    COMPUTER_USE_CDP=1) raised ModuleNotFoundError, the engine exited 1, and the
    ExtensionHub never bound on 8765 -- every browser tool was dead even though the
    extension channel needs no such package.
    """

    def test_importing_cdp_ws_does_not_require_websocket_client(self) -> None:
        # A fresh interpreter with the transport hidden: importing the module must succeed,
        # because importing a module is not using the CDP path.
        script = (
            "import sys; sys.modules['websocket'] = None;"
            "import computer_use.cdp_ws as m;"
            "print('imported', bool(m.CdpConn))"
        )
        proc = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("imported True", proc.stdout)

    def test_connect_without_websocket_client_raises_a_readable_error(self) -> None:
        # `sys.modules[name] = None` makes `import name` raise ImportError, which is what a
        # missing package does.
        with patch.dict(sys.modules, {"websocket": None}):
            with self.assertRaises(CdpUnavailable) as caught:
                CdpConn("ws://127.0.0.1:9334/devtools/page/x")
        message = str(caught.exception)
        # The error must name the fix, not just the symptom.
        self.assertIn("websocket-client", message)
        self.assertIn("pip install", message)
        # And it must stay a RuntimeError, which the RPC layer reports as a normal error
        # instead of killing the process.
        self.assertTrue(issubclass(CdpUnavailable, RuntimeError))

    def test_the_hint_names_both_install_routes_and_the_working_channel(self) -> None:
        self.assertIn("pip install --user websocket-client", WEBSOCKET_CLIENT_HINT)
        self.assertIn("apt install python3-websocket-client", WEBSOCKET_CLIENT_HINT)
        self.assertIn("8765", WEBSOCKET_CLIENT_HINT)

    def test_a_present_transport_is_used_unchanged(self) -> None:
        # With a transport available, behaviour is exactly what it was before the guard.
        fake = FakeWs()
        with patch.dict(sys.modules, {"websocket": fake_transport(fake)}):
            self.assertIsNotNone(load_websocket())
            conn = CdpConn("ws://127.0.0.1:9334/devtools/page/x")
            self.assertIsInstance(conn.ws, FakeWs)


if __name__ == "__main__":
    unittest.main()
